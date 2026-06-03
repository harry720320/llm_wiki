# xECM Raw File Layer — Design Spec

**Date**: 2026-06-03
**Status**: Draft
**Goal**: Use OpenText xECM (24.1 REST API) as the raw source file layer, replacing the local filesystem `raw/sources/` with a virtual view of an xECM workspace. The entire LLM Wiki app sees files normally; the virtualization is transparent.

---

## Architecture

Virtualization boundary at `src-tauri/src/commands/fs.rs` — the single Rust module all file I/O already passes through.

```
TS components (unchanged)
  → src/commands/fs.ts (unchanged — thin invoke() wrappers)
  → Tauri IPC
  → src-tauri/commands/fs.rs  ← conditional dispatch
       ├── Local path: existing std::fs logic
       ├── xECM path: new xecm_client module (reqwest)
       └── Cache: .llm-wiki/xecm-cache/{node_id}
```

xECM is **read-only** from LLM Wiki's perspective. Wiki output (`wiki/`, `.llm-wiki/`, vector store) remains local filesystem. Only `raw/sources/` is virtualized.

---

## xECM API Contract (OpenText Content Server 24.1)

Base URL: `/otcs/cs.exe/api/v1`

| Operation | HTTP | Endpoint | Input | Output |
|-----------|------|----------|-------|--------|
| Authenticate | POST | `/auth` | username + password (form-encoded) | `{ ticket: "..." }` |
| List children | GET | `/nodes/{id}/nodes?limit=N&page=M` | `OTCSTicket` header | `{ data: [{id, name, type, container, size, modify_date, mime_type, parent_id}], page_total: N }` |
| Get node | GET | `/nodes/{id}` | `OTCSTicket` header | `{ data: { id, name, type, container, size, modify_date, mime_type, parent_id } }` |
| Get content | GET | `/nodes/{id}/content` | `OTCSTicket` header | raw bytes |
| List volumes | GET | `/volumes` | `OTCSTicket` header | `{ data: [{ id, name, type }] }` |

Auth uses the `OTCSTicket` header on every request. The ticket is obtained once at connection time and regenerated on 401. Node types: `0` = Folder, `141` = Enterprise Workspace, `144` = Document, `848` = Business Workspace, etc.

---

## Rust Module: `src-tauri/src/xecm_client.rs`

### Types

```rust
struct XecmNode {
    id: u64, name: String, type_: u64, container: bool,
    size: u64, modify_date: Option<String>, mime_type: Option<String>,
    parent_id: i64,
}

struct XecmConfig {
    enabled: bool,
    base_url: String,         // "http://host/otcs/cs.exe/api/v1"
    workspace_node_id: u64,   // resolved from workspace name at setup
    workspace_name: String,
    username: String,
    ticket: String,           // OTCSTicket, regenerated on 401 or app restart
    poll_interval_secs: u64,  // default 30
}
```

### Public functions

- `xecm_authenticate(base_url, username, password) -> Result<String>` — POST `/auth`, return ticket
- `xecm_list_directory(config, node_id, page) -> Result<(Vec<XecmNode>, u32)>` — returns nodes + total pages
- `xecm_get_content(config, node_id) -> Result<Vec<u8>>` — checks local cache first, fetches on miss
- `xecm_get_node(config, node_id) -> Result<XecmNode>` — single node metadata lookup
- `xecm_resolve_path(config, path) -> Result<u64>` — walk `raw/sources/folder/doc.txt` segments from workspace root, resolve to node ID. Cache path→node_id in session HashMap
- `xecm_resolve_workspace(config) -> Result<XecmNode>` — find workspace by name from volumes list
- `xecm_list_workspaces(base_url, ticket) -> Result<Vec<XecmNode>>` — list all container-type top-level nodes for the workspace picker UI

### Cache

Content cache at `{project}/.llm-wiki/xecm-cache/{node_id}`. Cache validity keyed by `(node_id, modify_date)`. On hit with matching date → return cached bytes. On miss → fetch, write cache, return. Stale cache entries for deleted/changed nodes are overwritten on next fetch, never cleaned automatically (size is bounded by user's document corpus).

Path resolution cache: in-memory `HashMap<String, u64>` mapping normalized paths to node IDs. Cleared on project switch. Optional: persist to `.llm-wiki/xecm-paths.json` for fast cold start.

---

## Routing in `commands/fs.rs`

Each affected command gets a top-of-function guard:

```
fn read_file(path) {
    if xecm_enabled && path under raw/sources/ {
        node_id = xecm_resolve_path(config, path)?
        bytes = xecm_get_content(config, node_id)?
        return String::from_utf8_lossy(bytes)
    }
    // existing local filesystem code
}
```

**Affected commands** (all read-path): `read_file`, `read_file_as_base64`, `list_directory`, `file_exists`, `get_file_modified_time`, `get_file_size`, `get_file_md5`, `delete_file`, `find_related_wiki_pages`, `preprocess_file`.

**Not affected** (write-path, stays local): `write_file`, `write_file_atomic`, `copy_file`, `copy_directory`, `create_directory`.

### Path mapping

xECM paths are relative to the workspace root and discovered via the API. The path string `raw/sources/` maps to the workspace root node. Subfolders map to nested container nodes. Example:

```
raw/sources/Reports/Q1.pdf
    → workspace root (node 2000)
    → child "Reports" (container, node X)
    → child "Q1.pdf" (document, node Y)
    → content at GET /nodes/Y/content
```

`xecm_resolve_path` walks this tree by `GET /nodes/{current}/nodes` at each level, matching by `name`. Session cache avoids re-walking on every `read_file`.

### get_file_md5 for xECM

xECM doesn't expose content MD5. Compute it from `modify_date + size` string hash when possible (cheap, no download). When the ingest pipeline needs actual content hash for the SHA256 cache, download once (populates content cache) and hash the bytes. The SHA256 cached result is stored in `.llm-wiki/ingest-cache.json` as usual.

### delete_file for xECM

xECM is read-only — `delete_file` on an xECM path returns an error. Source file lifecycle (delete source → cascade wiki cleanup) is disabled for xECM-backed projects. The xECM branch `delete_file` returns: `Err("Cannot delete xECM files from LLM Wiki. Manage files in xECM directly.")`.

---

## File Watcher Replacement

When xECM is active, the Rust `notify` watcher is replaced by a poll loop.

### Poll loop (`xecm_watcher.rs` or inline in `file_sync.rs`)

1. Every `poll_interval_secs`, fetch full flat node tree under workspace root (paginated recursive listing)
2. Compare each node's `modify_date` against persisted snapshot at `.llm-wiki/xecm-snapshot.json`
3. Emit change tasks through the existing `FileSyncState` channel:
   - **New node** (in API, not in snapshot) → `created` task
   - **Changed `modify_date`** → `modified` task
   - **Missing node** (in snapshot, not in API) → `deleted` task
4. Update snapshot with current state
5. TS-side `project-file-sync.ts` receives events through the same `file-sync://changed` Tauri event channel, feeds them into `processFileChangeBatch` unchanged

### Snapshot format

```json
{
  "workspaceNodeId": 2000,
  "lastPoll": "2026-06-03T12:00:00Z",
  "nodes": {
    "14802": { "name": "doc.txt", "modifyDate": "2026-06-03T06:25:17", "size": 159339 },
    "...": { }
  }
}
```

### Config

`pollIntervalSeconds` added to `SourceWatchConfig`. Default 30 seconds. Minimum 10 seconds (rate limit protection). Disable watcher entirely when `autoIngest` is off (same as local watcher behavior).

---

## Config & State

### `{project}/.llm-wiki/xecm-config.json`

```json
{
  "enabled": true,
  "baseUrl": "http://192.168.0.29/otcs/cs.exe/api/v1",
  "workspaceName": "Enterprise",
  "workspaceNodeId": 2000,
  "username": "admin",
  "ticket": null,
  "pollIntervalSeconds": 30
}
```

- `ticket` is ephemeral, regenerated on app startup or 401
- `username` is stored for silent re-auth
- Password is never persisted — on 401, UI prompts user to re-enter

### Zustand wiki-store additions

```ts
interface XecmConfig {
  enabled: boolean
  baseUrl: string
  workspaceName: string
  workspaceNodeId: number
  username: string
  ticket: string | null
  pollIntervalSeconds: number
}
```

Added to `WikiState` with `setXecmConfig`. Hydrated from disk on project open, alongside existing config hydration in `App.tsx`.

### Rust-side state

`XecmConfig` held in `tauri::State` via `app.manage()` in `lib.rs`. Loaded from `xecm-config.json` during project open (frontend sends config via a new `set_xecm_config` Tauri command or reads it on first file operation).

Actually: pass config from frontend on each project open via `invoke("set_xecm_config", { config })`. Rust stores it in `Mutex<Option<XecmConfig>>` managed state. This avoids Rust needing to read the frontend's config file.

---

## UI Changes

### Settings → xECM Connection (new section: `xecm-section.tsx`)

1. **Disconnected state**: Fields for Base URL, Username, Password. "Connect" button.
   - On connect: `invoke("xecm_connect", { baseUrl, username, password })` → Rust authenticates, lists workspaces, returns workspace list
2. **Workspace picker**: Dropdown showing workspace names. User selects one.
   - On select: `invoke("xecm_set_workspace", { workspaceName })` → Rust resolves name to node ID, writes config, returns config
3. **Connected state**: Shows "Connected to {workspaceName} at {baseUrl}". "Disconnect" button. Poll interval slider (10s–300s).
4. **Error state**: "Session expired" with "Re-enter password" prompt. "Connection failed" with retry.

### Project creation (`create-project-dialog.tsx`)

Add a third option alongside existing templates: "xECM Workspace". Selecting it shows the xECM connection form inline (same fields as settings). After successful connect + workspace pick, project is created with xECM as its source layer. Template files (`purpose.md`, `schema.md`) still get scaffolded locally.

### Error banner

A dismissible banner in `app-layout.tsx` (similar to `UpdateBanner`) showing xECM connectivity status:
- "xECM: Connected to {workspaceName}" (green, auto-dismiss after 5s on successful connect)
- "xECM: Connection lost — retrying..." (yellow, on first failure)
- "xECM: Session expired — re-enter credentials in Settings" (red, on 401)

---

## Error Handling

| Error | Detection | Behavior |
|-------|-----------|----------|
| Auth failure | `POST /auth` returns non-200 | Show "Invalid credentials" in settings |
| Ticket expired | Any API call returns 401 | Return distinct error type; UI shows "Session expired" prompt |
| Network down | reqwest timeout/connection refused | Return error; ingest queue pauses; banner shows "Connection lost" |
| Node not found | API returns 404 for node ID | `read_file` returns "file not found" (same as local missing file) |
| Workspace renamed | Resolved node ID returns 404 | UI prompts to re-pick workspace in settings |
| Rate limit | API returns 429 | Exponential backoff in poll watcher; immediate requests show "xECM busy" |

All xECM errors propagate through the existing `run_guarded` pattern in `commands/fs.rs`. The TS side sees them as regular Tauri command errors with an `"xECM: "` prefix for user-visible messages.

---

## Files Changed

| File | Change |
|------|--------|
| `src-tauri/src/xecm_client.rs` | **New** — xECM HTTP client module (~250 lines) |
| `src-tauri/src/commands/fs.rs` | Add conditional dispatch in 10 read-path commands (~80 lines) |
| `src-tauri/src/commands/file_sync.rs` | Add poll-based xECM watcher (~120 lines) |
| `src-tauri/src/lib.rs` | Register `XecmConfig` state, add `set_xecm_config` command, `xecm_connect` command, `xecm_set_workspace` command (~40 lines) |
| `src-tauri/Cargo.toml` | No new dependencies (reqwest already present) |
| `src/stores/wiki-store.ts` | Add `XecmConfig` interface + setter (~15 lines) |
| `src/lib/project-store.ts` | Add xECM config load/save functions (~20 lines) |
| `src/components/settings/sections/xecm-section.tsx` | **New** — xECM settings UI (~150 lines) |
| `src/components/settings/settings-view.tsx` | Wire in xecm-section (~5 lines) |
| `src/components/project/create-project-dialog.tsx` | Add xECM option (~40 lines) |
| `src/App.tsx` | Hydrate xECM config on project open, set config on Rust side (~15 lines) |
| `src/components/layout/app-layout.tsx` | Add xECM status banner (~30 lines) |
| `src/i18n/en.json` | xECM-related i18n keys (~15 keys) |
| `src/i18n/zh.json` | xECM-related i18n keys (~15 keys) |

**Total**: ~800 lines new/changed across 14 files. No new dependencies.

---

## What Stays Unchanged

- Entire ingest pipeline (`ingest.ts`, `ingest-queue.ts`, `ingest-cache.ts`, `ingest-sanitize.ts`)
- Search pipeline (`search-pipeline.ts`, `graph-search.ts`, `embedding.ts`)
- Knowledge graph (`graph-relevance.ts`, `graph-insights.ts`)
- Deep research, lint, dedup, review, chat, wiki editor, preview
- Chrome extension and clip server
- Vector store (LanceDB — references wiki pages, not source paths)
- Scheduled import (`scheduled-import.ts`)
- All UI except Settings section + create project dialog + app layout banner
- All TS `commands/fs.ts` wrappers

---

## Testing Strategy

### Unit tests (Vitest, mock mode)

- `xecm_config_serialization.test.ts` — config JSON round-trip
- `xecm_path_mapping.test.ts` — path → node ID resolution with mocked API responses

### Integration tests (Rust `#[cfg(test)]`)

- `xecm_client::tests::resolve_path_flat` — flat workspace, single level
- `xecm_client::tests::resolve_path_nested` — nested folder structure
- `xecm_client::tests::cache_hit_skips_fetch` — content cache hit test
- `xecm_client::tests::cache_miss_fetches_and_stores` — content cache miss test
- `xecm_client::tests::auth_401_rejects` — expired ticket handling

### Integration tests (real-LLM, with xECM instance)

- `xecm_ingest.real-llm.test.ts` — ingest a small document from xECM, verify wiki page created with correct `sources[]` frontmatter
- Uses `.env.test.local` for xECM credentials
- Tagged with `real-llm` so only runs with `npm run test:llm`

### Manual QA

1. Create project with xECM workspace → verify file tree shows workspace contents
2. Click a file → verify preview renders content
3. Trigger ingest → verify wiki pages generated with `sources[]` pointing to xECM paths
4. Modify file in xECM (via Content Server UI) → wait poll interval → verify file watcher detects change
5. Add new file in xECM → verify auto-ingest
6. Disconnect network → verify error banner and ingest pause
7. Reconnect → verify resume
