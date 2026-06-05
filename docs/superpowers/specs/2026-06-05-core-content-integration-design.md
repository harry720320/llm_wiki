# Core Content Integration — Design Spec

**Date**: 2026-06-05
**Status**: Design approved
**Approach**: A — Mirror xECM pattern

## Summary

Add OpenText Core Content as a second remote-source backend alongside the existing xECM integration. One LLM Wiki project connects to **either** xECM or Core Content, never both. The integration mirrors the xECM architecture: a Rust client, a settings section, and a poll-based file-sync watcher that emits `FileChangeTask` events into the existing ingest pipeline.

## Architecture

New components are added alongside (not inside) existing xECM code. No xECM code is refactored.

### New files

| File | Purpose |
|------|---------|
| `src-tauri/src/core_content_client.rs` | Rust client: auth, browse, download, snapshot |
| `src/components/settings/sections/core-content-section.tsx` | Settings UI |
| `src/commands/core-content.ts` | Tauri IPC command wrappers (frontend) |

### Modified files

| File | Change |
|------|--------|
| `src-tauri/src/lib.rs` | New `CoreContentState`, register 4 new commands |
| `src-tauri/src/commands/file_sync.rs` | Parameterize poll watcher with `RemoteSourceBackend` enum, add Core Content poll watcher |
| `src-tauri/src/commands/fs.rs` | Core Content content-read path in `readFile` |
| `src/stores/wiki-store.ts` | New `CoreContentConfig` type and setter |
| `src/components/settings/settings-types.ts` | New draft fields |
| `src/components/settings/settings-view.tsx` | Include `CoreContentSection` in settings tabs |
| `src/App.tsx` | Core Content config hydration on project open/close |
| `src/lib/project-store.ts` | Core Content config persistence key |

## Rust Backend

### `CoreContentConfig`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreContentConfig {
    pub enabled: bool,
    pub base_url: String,
    pub folder_node_id: String,     // UUID string from Core Content
    pub folder_name: String,
    pub username: String,
    pub password: String,           // encrypted at rest
    pub csrf_token: String,         // CCM-XSRF-TOKEN
    pub cookies_json: String,       // serialized cookie jar
    pub poll_interval_seconds: u64,
}
```

### `CoreContentClient`

```rust
pub struct CoreContentClient {
    http: reqwest::Client,
    config: CoreContentConfig,
    csrf_token: Mutex<String>,
    cache_dir: PathBuf,
}
```

**Public methods** (mirror `XecmClient` interface):

| Method | Description |
|--------|-------------|
| `new(config, cache_dir)` | Build client with cookie store from `cookies_json` |
| `list_root_folders() -> Vec<CoreContentNode>` | GET `/cm/v1/node/root` then children |
| `list_directory(node_id) -> Vec<CoreContentNode>` | GET `/cm/v1/node/{id}/nodes` |
| `get_content(node_id) -> Vec<u8>` | GET node → extract `cms_links["urn:eim:linkrel:download-media"]` → GET download URL; cached via MD5 key |
| `recursive_snapshot() -> HashMap<String, CoreContentSnapshotEntry>` | Stack-based walk from `folder_node_id` |
| `resolve_path(path) -> String` | Strip `raw/sources/` prefix, match filename in snapshot |
| `is_source_path(path) -> bool` | Same logic as `XecmClient::is_source_path` |

**Error type**: `CoreContentError` with variants `Auth`, `Network`, `NotFound`, `RateLimited`, `Other`.

**API request headers** (all requests):
- `X-CCM-XSRF-TOKEN: <csrf_token>`
- `X-Requested-With: XMLHttpRequest`
- `Authorization: dummy`
- `Referer: <base_url>`

**Auth retry**: On 401, retry once with credentials-based re-auth via webview. If MFA required, bubble error to frontend.

### State & mutual exclusion

```rust
// lib.rs
struct CoreContentState(Mutex<Option<CoreContentClient>>);

// set_core_content_config: if config.enabled, clear XecmState
// set_xecm_config: if config.enabled, clear CoreContentState
```

### Commands

| Command | Purpose |
|---------|---------|
| `set_core_content_config(config)` | Create or clear client in state |
| `core_content_connect_finish(csrf_token, cookies_json, username, password)` | Store session from webview, validate with test API call |
| `core_content_list_root(base_url, csrf_token, cookies_json)` | Return list of top-level folders for picker |
| `core_content_select_folder(folder_node_id, folder_name)` | Set target folder, persist config, start poll watcher |

### File sync watcher

`start_project_file_watcher` is changed to check both states:

```
if xecm_active      → start_xecm_poll_watcher(...)
if core_content_active → start_core_content_poll_watcher(...)
```

`start_core_content_poll_watcher` follows the same take/restore pattern as the xECM watcher:

1. Take `CoreContentClient` from `CoreContentState`
2. Call `recursive_snapshot()`
3. Diff against previous snapshot stored in `.llm-wiki/core-content-snapshot.json`
4. Emit `FileChangeTask` events for created/modified/deleted files
5. Put client back into state
6. Sleep for `poll_interval_seconds` (minimum 10s)

### Content read (fs.rs)

In the `readFile` command path, after the existing xECM check:

```rust
if let Some(client) = core_content_state.0.lock()... {
    if client.is_source_path(&path) {
        let node_id = client.resolve_path(&path)?;
        return Ok(client.get_content(node_id).await?);
    }
}
```

The extracted content flows through the same text extraction pipeline (pdfium, office_oxide) as local and xECM files.

## Webview Auth Flow

### Initial login

1. User clicks "Connect to Core Content" in settings
2. Frontend calls `core_content_connect_start` which opens a Tauri `WebviewWindow` at the Core Content login URL
3. User completes OTDS login form (username → password → MFA if prompted)
4. Frontend detects successful login: URL changes to `.../subscriptions/...` (non-OTDS page) after 15s idle period
5. Frontend injects JS to extract `document.cookie` and `window.location.href`
6. Frontend calls `core_content_connect_finish` with extracted data
7. Rust validates by calling `GET /cm/v1/node/root` — on 200, close webview and list root folders
8. Config persisted to `.llm-wiki/core-content-config.json`

### Auto-re-auth (session expiry)

User credentials (username + password) are persisted alongside config. On 401 from any API call:

1. Re-open webview at login URL
2. Inject JS to fill username field, click "next"
3. Inject JS to fill password field, click "Sign in"
4. If MFA page detected (body contains "verification code", "mfa", or "authenticator"), keep webview open and emit event to frontend to notify user
5. If no MFA, wait for successful navigation, extract new cookies, update config, continue

### Cookie limitation

`document.cookie` in Tauri webview only returns non-HttpOnly cookies. If the Core Content session cookie is HttpOnly and can't be extracted, fallback: after login, make a test API call (`GET /cm/v1/node/root`) from within the webview context using `fetch` to confirm session validity. Tag the auth as "webview-bound" and route subsequent API calls differently (pending investigation during implementation — may require keeping a hidden webview alive for API calls, or using a different extraction approach).

## Frontend

### `CoreContentConfig` (wiki-store.ts)

```typescript
interface CoreContentConfig {
  enabled: boolean
  baseUrl: string
  folderNodeId: string
  folderName: string
  username: string
  password: string
  csrfToken: string
  cookiesJson: string
  pollIntervalSeconds: number
}
```

### `CoreContentSection.tsx`

Three states, mirroring `XecmSection.tsx`:

1. **Disconnected**: Base URL input + "Connect to Core Content" button
2. **Folder selection**: List of top-level folders returned from `core_content_list_root`, user clicks one
3. **Connected**: Green status badge showing folder name and base URL, poll interval input, Disconnect button

**Mutual exclusion**: When `coreContentEnabled` is true, `XecmSection` is disabled (grayed out). When `xecmEnabled` is true, `CoreContentSection` is disabled.

### Settings integration

`CoreContentSection` is added to the Settings view under "Source Watch" tab, below the existing file-system source watch config and xECM section.

### Config hydration (App.tsx)

On project open:
1. Load `.llm-wiki/core-content-config.json`
2. If `enabled`, hydrate `CoreContentConfig` into wiki-store
3. Invoke `set_core_content_config` to create the Rust client

On project close/switch:
1. Invoke `set_core_content_config({ enabled: false })` to clear Rust state

### Config persistence (project-store.ts)

Add `coreContent` key to the project config persistence map. Save/load from `.llm-wiki/core-content-config.json`, same directory as `xecm-config.json`.

## Files to Create

1. `src-tauri/src/core_content_client.rs` — `CoreContentClient`, `CoreContentConfig`, `CoreContentNode`, `CoreContentError`
2. `src/components/settings/sections/core-content-section.tsx` — Settings UI
3. `src/commands/core-content.ts` — Frontend IPC wrappers

## Files to Modify

1. `src-tauri/src/lib.rs` — State + command registration + mutual exclusion
2. `src-tauri/src/commands/file_sync.rs` — Core Content poll watcher, `RemoteSourceBackend` enum
3. `src-tauri/src/commands/fs.rs` — Core Content content-read path
4. `src/stores/wiki-store.ts` — `CoreContentConfig` type + setter
5. `src/components/settings/settings-types.ts` — Draft fields
6. `src/components/settings/settings-view.tsx` — Include `CoreContentSection`
7. `src/components/settings/sections/xecm-section.tsx` — Mutual exclusion disable logic
8. `src/App.tsx` — Core Content config hydration
9. `src/lib/project-store.ts` — Core Content config persistence

## Open Questions (resolved during implementation)

1. **HttpOnly cookie extraction**: Confirm whether Core Content session cookie is HttpOnly. If yes, evaluate keeping a hidden webview alive for API calls vs. using Tauri's cookie store API.
2. **MFA detection reliability**: The Python script checks `body.includes("verification code")`. May need refinement for different MFA providers.
3. **Webview auto-fill timing**: Credential injection timing depends on page load speed and JS framework rendering. May need retry loops.

## Success Criteria

- [ ] User connects to Core Content via webview login in Settings
- [ ] User selects a folder; folder is visible in file tree under `raw/sources/`
- [ ] Files from selected folder are listed; clicking one downloads and displays content
- [ ] Poll watcher detects new, modified, and deleted files and emits change events
- [ ] Ingest pipeline processes Core Content files (same as local/xECM files)
- [ ] Session expiry triggers auto-re-auth; MFA prompts notification
- [ ] Connecting to Core Content when xECM is active disconnects xECM (and vice versa)
- [ ] xECM path works unchanged (no regressions)
