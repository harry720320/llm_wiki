# Core Content Integration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OpenText Core Content as a second remote-source backend alongside xECM, with webview-based auth, folder selection, poll-based file sync, and mutual exclusion with xECM.

**Architecture:** Mirror the existing xECM pattern — a Rust `CoreContentClient` (alongside `XecmClient`), a `CoreContentSection` settings component (alongside `XecmSection`), shared poll-watcher plumbing parameterized by backend type. One project = one remote backend.

**Tech Stack:** Rust (reqwest, serde), TypeScript/React (Zustand), Tauri v2 (WebviewWindow, IPC commands)

---

### Task 1: Rust `CoreContentConfig` and `CoreContentClient`

**Files:**
- Create: `src-tauri/src/core_content_client.rs`

- [ ] **Step 1: Write the complete `core_content_client.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A single node returned by the Core Content REST API.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreContentNode {
    pub id: String,                // Core Content uses strings/UUIDs for node IDs
    pub name: String,
    #[serde(default)]
    pub container: bool,
    #[serde(default)]
    pub size: u64,
    pub modify_date: Option<String>,
    pub mime_type: Option<String>,
    #[serde(default)]
    pub content_size: Option<u64>,
    #[serde(default)]
    pub cms_links: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreContentConfig {
    pub enabled: bool,
    pub base_url: String,          // "https://corecontent.dev.ca.opentext.com/subscriptions/avstcc"
    pub folder_node_id: String,    // selected folder UUID
    pub folder_name: String,
    pub username: String,
    pub password: String,
    pub csrf_token: String,        // CCM-XSRF-TOKEN value
    pub cookies_json: String,      // serialized cookie jar JSON
    pub poll_interval_seconds: u64,
}

/// Lightweight snapshot entry for change detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreContentSnapshotEntry {
    pub name: String,
    pub modify_date: Option<String>,
    pub size: u64,
}

#[derive(Debug)]
pub enum CoreContentError {
    Auth(String),
    Network(String),
    NotFound(String),
    RateLimited,
    Other(String),
}

impl std::fmt::Display for CoreContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreContentError::Auth(m) => write!(f, "Core Content auth error: {m}"),
            CoreContentError::Network(m) => write!(f, "Core Content network error: {m}"),
            CoreContentError::NotFound(m) => write!(f, "Core Content not found: {m}"),
            CoreContentError::RateLimited => write!(f, "Core Content rate limited"),
            CoreContentError::Other(m) => write!(f, "Core Content error: {m}"),
        }
    }
}

impl From<reqwest::Error> for CoreContentError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            CoreContentError::Network("request timed out".to_string())
        } else if e.is_connect() {
            CoreContentError::Network(format!("connection failed: {e}"))
        } else {
            CoreContentError::Other(format!("HTTP request failed: {e}"))
        }
    }
}

pub struct CoreContentClient {
    http: reqwest::Client,
    config: CoreContentConfig,
    csrf_token: Mutex<String>,
    cache_dir: PathBuf,
}

impl CoreContentClient {
    pub fn new(config: CoreContentConfig, cache_dir: PathBuf) -> Self {
        // Build a cookie store from the serialized cookies JSON.
        // cookies_json is a JSON object mapping cookie name → value.
        let mut cookie_jar = reqwest::cookie::Jar::default();
        if !config.cookies_json.is_empty() {
            if let Ok(cookies) = serde_json::from_str::<HashMap<String, String>>(&config.cookies_json) {
                // Add each cookie with a wildcard domain covering the CC host.
                // The cookie jar requires a URL to set domain scope.
                let domain_url = &config.base_url;
                for (name, value) in &cookies {
                    cookie_jar.add_cookie_str(
                        &format!("{name}={value}"),
                        &domain_url.parse().unwrap(),
                    );
                }
            }
        }

        let jar = std::sync::Arc::new(cookie_jar);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .cookie_provider(jar)
            .build()
            .unwrap_or_default();

        Self {
            http,
            csrf_token: Mutex::new(config.csrf_token.clone()),
            config,
            cache_dir,
        }
    }

    /// Build the standard Core Content request headers.
    fn cc_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        let csrf = self.csrf_token.lock().ok().cloned().unwrap_or_default();
        headers.insert(
            "X-CCM-XSRF-TOKEN",
            reqwest::header::HeaderValue::from_str(&csrf).unwrap_or_default(),
        );
        headers.insert(
            "X-Requested-With",
            reqwest::header::HeaderValue::from_static("XMLHttpRequest"),
        );
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_static("dummy"),
        );
        headers.insert(
            "Referer",
            reqwest::header::HeaderValue::from_str(&self.config.base_url).unwrap_or_default(),
        );
        headers
    }

    /// GET a Core Content API path and return (status, parsed body).
    async fn api_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, CoreContentError> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.config.base_url, path)
        };
        let resp = self.http.get(&url).headers(self.cc_headers()).send().await?;
        Self::check_status(&resp)?;
        let body = resp.json().await?;
        Ok(body)
    }

    /// GET raw bytes from a URL (for file download).
    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CoreContentError> {
        let resp = self.http.get(url).headers(self.cc_headers()).send().await?;
        Self::check_status(&resp)?;
        Ok(resp.bytes().await?.to_vec())
    }

    fn check_status(resp: &reqwest::Response) -> Result<(), CoreContentError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(match status.as_u16() {
            401 => CoreContentError::Auth("session expired".to_string()),
            404 => CoreContentError::NotFound("node not found".to_string()),
            429 => CoreContentError::RateLimited,
            other => CoreContentError::Other(format!("HTTP {other}")),
        })
    }

    // ── Public API (mirrors XecmClient interface) ──

    /// List root-level folders. Returns only container nodes.
    pub async fn list_root_folders(&self) -> Result<Vec<CoreContentNode>, CoreContentError> {
        let root: serde_json::Value = self.api_get("/cm/v1/node/root").await?;
        let root_id = root["id"].as_str().ok_or_else(|| CoreContentError::NotFound("root node has no id".to_string()))?;

        let resp: serde_json::Value = self.api_get(&format!("/cm/v1/node/{root_id}/nodes")).await?;
        let items = resp["_embedded"]["collection"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let nodes: Vec<CoreContentNode> = items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .filter(|n: &CoreContentNode| n.container)
            .collect();

        Ok(nodes)
    }

    /// List direct children of a folder node.
    pub async fn list_directory(&self, node_id: &str) -> Result<Vec<CoreContentNode>, CoreContentError> {
        let resp: serde_json::Value = self.api_get(&format!("/cm/v1/node/{node_id}/nodes")).await?;
        let items = resp["_embedded"]["collection"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let nodes: Vec<CoreContentNode> = items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect();

        Ok(nodes)
    }

    /// Download a file's content by node ID.
    /// 1. GET node details to find the download-media link
    /// 2. GET the download URL
    /// 3. Cache by MD5(node_id + modify_date)
    pub async fn get_content(&self, node_id: &str) -> Result<Vec<u8>, CoreContentError> {
        let node: CoreContentNode = self.api_get(&format!("/cm/v1/node/{node_id}")).await?;

        let cache_key = cache_key_for(&node);
        let cache_path = self.cache_dir.join(&cache_key);

        if cache_path.exists() {
            if let Ok(cached) = std::fs::read(&cache_path) {
                return Ok(cached);
            }
        }

        let cms_links = node.cms_links.as_ref().ok_or_else(|| {
            CoreContentError::NotFound("node has no cms_links".to_string())
        })?;
        let dl_path = cms_links.get("urn:eim:linkrel:download-media").ok_or_else(|| {
            CoreContentError::NotFound("no download-media link".to_string())
        })?;
        let dl_url = format!("https://corecontent.dev.ca.opentext.com{dl_path}");

        let bytes = self.download_bytes(&dl_url).await?;

        let _ = std::fs::create_dir_all(&self.cache_dir);
        let _ = std::fs::write(&cache_path, &bytes);

        Ok(bytes)
    }

    /// Walk the entire tree from the configured folder and collect
    /// all file nodes (non-container) with their metadata.
    pub async fn recursive_snapshot(&self) -> Result<HashMap<String, CoreContentSnapshotEntry>, CoreContentError> {
        let mut snapshot = HashMap::new();
        let mut stack: Vec<String> = vec![self.config.folder_node_id.clone()];

        while let Some(node_id) = stack.pop() {
            let children = self.list_directory(&node_id).await?;
            for child in children {
                if child.container {
                    stack.push(child.id);
                } else {
                    snapshot.insert(child.id.clone(), CoreContentSnapshotEntry {
                        name: child.name,
                        modify_date: child.modify_date,
                        size: child.size.max(child.content_size.unwrap_or(0)),
                    });
                }
            }
        }

        Ok(snapshot)
    }

    /// Resolve a virtual `raw/sources/<filename>` path to a Core Content node ID.
    /// Looks up the filename in the current snapshot.
    pub async fn resolve_path(&self, path: &str) -> Result<String, CoreContentError>
    where
        Self: Sized,
    {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let snapshot = self.recursive_snapshot().await?;
        for (node_id, entry) in &snapshot {
            if entry.name == name {
                return Ok(node_id.clone());
            }
        }
        Err(CoreContentError::NotFound(format!(
            "file '{}' not found in Core Content folder '{}'",
            name,
            self.config.folder_name
        )))
    }

    pub fn is_source_path(&self, path: &str) -> bool {
        let normalized = path.replace('\\', "/");
        extract_relative(&normalized, "raw/sources").is_some()
    }
}

fn extract_relative<'a>(normalized: &'a str, prefix: &str) -> Option<&'a str> {
    if normalized == prefix {
        return Some("");
    }
    if let Some(rest) = normalized.strip_prefix(&format!("{prefix}/")) {
        return Some(rest);
    }
    if let Some(idx) = normalized.find(&format!("/{prefix}/")) {
        return Some(&normalized[idx + prefix.len() + 2..]);
    }
    if normalized.ends_with(&format!("/{prefix}")) {
        return Some("");
    }
    None
}

fn cache_key_for(node: &CoreContentNode) -> String {
    use md5::{Digest, Md5};
    let date = node.modify_date.as_deref().unwrap_or("unknown");
    let hash_input = format!("{}-{}", node.id, date);
    let digest = Md5::digest(hash_input.as_bytes());
    format!("{:x}", digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_source_path_recognizes_valid_paths() {
        let config = CoreContentConfig {
            enabled: true,
            base_url: "https://example.com/subscriptions/test".into(),
            folder_node_id: "abc123".into(),
            folder_name: "test".into(),
            username: "".into(),
            password: "".into(),
            csrf_token: "".into(),
            cookies_json: "".into(),
            poll_interval_seconds: 30,
        };
        let client = CoreContentClient::new(config, std::env::temp_dir().join("cc-test-cache"));

        assert!(client.is_source_path("raw/sources"));
        assert!(client.is_source_path("raw/sources/doc.pdf"));
        assert!(client.is_source_path("raw/sources/folder/file.txt"));
        assert!(client.is_source_path("raw\\sources\\doc.pdf"));
        assert!(client.is_source_path("C:/Users/test/project/raw/sources/doc.pdf"));
        assert!(!client.is_source_path("wiki/index.md"));
        assert!(!client.is_source_path("purpose.md"));
        assert!(!client.is_source_path(""));
    }

    #[test]
    fn cache_key_is_deterministic() {
        let node = CoreContentNode {
            id: "abc-123".into(),
            name: "test.pdf".into(),
            container: false,
            size: 100,
            modify_date: Some("2026-01-01T00:00:00".into()),
            mime_type: None,
            content_size: None,
            cms_links: None,
        };
        let k1 = cache_key_for(&node);
        let k2 = cache_key_for(&node);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
    }

    #[test]
    fn cache_key_differs_on_modify_date_change() {
        let mut node = CoreContentNode {
            id: "abc-123".into(),
            name: "test.pdf".into(),
            container: false,
            size: 100,
            modify_date: Some("2026-01-01T00:00:00".into()),
            mime_type: None,
            content_size: None,
            cms_links: None,
        };
        let k1 = cache_key_for(&node);
        node.modify_date = Some("2026-06-01T00:00:00".into());
        let k2 = cache_key_for(&node);
        assert_ne!(k1, k2);
    }

    #[test]
    fn error_display_messages() {
        assert!(format!("{}", CoreContentError::Auth("bad".into())).contains("auth"));
        assert!(format!("{}", CoreContentError::Network("timeout".into())).contains("network"));
        assert!(format!("{}", CoreContentError::NotFound("missing".into())).contains("not found"));
        assert!(format!("{}", CoreContentError::RateLimited).contains("rate limited"));
        assert!(format!("{}", CoreContentError::Other("oops".into())).contains("oops"));
    }
}
```

- [ ] **Step 2: Add `mod core_content_client;` to `src-tauri/src/main.rs`**

`src-tauri/src/main.rs` already has `mod xecm_client;`. Add after it.

- [ ] **Step 3: Run tests for the new module**

```bash
cargo test -p llm-wiki -- core_content_client
```

Expected: 4 tests pass (is_source_path, cache_key_deterministic, cache_key_differs, error_display).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/core_content_client.rs src-tauri/src/main.rs
git commit -m "feat: add CoreContentClient with browse, download, and snapshot

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 2: Backend State + Commands in `lib.rs`

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add module import and state struct at the top of `lib.rs`**

After line 14 (`use crate::xecm_client::{XecmClient, XecmConfig};`), add:

```rust
use crate::core_content_client::{CoreContentClient, CoreContentConfig};
```

After the `XecmState` struct (line 19), add:

```rust
struct CoreContentState(Mutex<Option<CoreContentClient>>);
```

- [ ] **Step 2: Add `set_core_content_config` command after `set_xecm_config`**

After the `set_xecm_config` function (ends around line 149), add:

```rust
/// Set/reset the active Core Content client. Clears xECM if enabled.
#[tauri::command]
fn set_core_content_config(
    config: Option<CoreContentConfig>,
    state: tauri::State<'_, CoreContentState>,
    xecm_state: tauri::State<'_, XecmState>,
) -> Result<String, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "Core Content state is unavailable".to_string())?;
    match config {
        Some(ref cfg) if cfg.enabled => {
            eprintln!("[core_content] set_core_content_config: enabled=true, url={}, folder={}",
                cfg.base_url,
                cfg.folder_name);
            // Mutual exclusion: clear xECM
            if let Ok(mut g) = xecm_state.0.lock() { *g = None; }
            *guard = Some(CoreContentClient::new(
                cfg.clone(),
                std::path::PathBuf::from(".llm-wiki/core-content-cache"),
            ));
            Ok("Core Content client configured".to_string())
        }
        _ => {
            eprintln!("[core_content] set_core_content_config: clearing client");
            *guard = None;
            Ok("Core Content client cleared".to_string())
        }
    }
}
```

- [ ] **Step 3: Add `core_content_connect_finish` command**

After `set_core_content_config`, add:

```rust
#[derive(serde::Serialize)]
struct CoreContentConnectResult {
    root_folders: Vec<serde_json::Value>,
}

/// Store session from webview login and list root folders.
#[tauri::command]
async fn core_content_connect_finish(
    base_url: String,
    csrf_token: String,
    cookies_json: String,
) -> Result<CoreContentConnectResult, String> {
    // Build a temporary client to test the session and list root folders
    let temp_config = CoreContentConfig {
        enabled: true,
        base_url: base_url.clone(),
        folder_node_id: String::new(),
        folder_name: String::new(),
        username: String::new(),
        password: String::new(),
        csrf_token: csrf_token.clone(),
        cookies_json: cookies_json.clone(),
        poll_interval_seconds: 30,
    };
    let client = CoreContentClient::new(temp_config, std::env::temp_dir().join("cc-temp"));
    let folders = client.list_root_folders().await.map_err(|e| e.to_string())?;

    Ok(CoreContentConnectResult {
        root_folders: folders
            .into_iter()
            .map(|f| serde_json::json!({
                "id": f.id,
                "name": f.name,
            }))
            .collect(),
    })
}
```

- [ ] **Step 4: Add `core_content_select_folder` command**

```rust
#[tauri::command]
fn core_content_select_folder(
    folder_node_id: String,
    folder_name: String,
    state: tauri::State<'_, CoreContentState>,
) -> Result<String, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "Core Content state is unavailable".to_string())?;
    match guard.as_mut() {
        Some(client) => {
            client.config.folder_node_id = folder_node_id;
            client.config.folder_name = folder_name;
            Ok("folder selected".to_string())
        }
        None => Err("No Core Content client configured".to_string()),
    }
}
```

- [ ] **Step 5: Add mutual exclusion to `set_xecm_config`**

In the existing `set_xecm_config` function (line 124), change to clear `CoreContentState` when enabling xECM. The function signature needs a `CoreContentState` parameter:

```rust
#[tauri::command]
fn set_xecm_config(
    config: Option<XecmConfig>,
    state: tauri::State<'_, XecmState>,
    cc_state: tauri::State<'_, CoreContentState>,
) -> Result<String, String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "xECM state is unavailable".to_string())?;
    match config {
        Some(ref cfg) if cfg.enabled => {
            // Mutual exclusion: clear Core Content
            if let Ok(mut g) = cc_state.0.lock() { *g = None; }
            eprintln!("[xecm] set_xecm_config: enabled=true, url={}, ticket={}",
                cfg.base_url,
                cfg.ticket.as_deref().map(|_| "present").unwrap_or("MISSING"));
            *guard = Some(XecmClient::new(
                cfg.clone(),
                std::path::PathBuf::from(".llm-wiki/xecm-cache"),
            ));
            Ok("xECM client configured".to_string())
        }
        _ => {
            eprintln!("[xecm] set_xecm_config: clearing client");
            *guard = None;
            Ok("xECM client cleared".to_string())
        }
    }
}
```

- [ ] **Step 6: Register new state and commands**

In the `setup` closure (around line 255), after `app.manage(XecmState(Mutex::new(None)));` add:

```rust
app.manage(CoreContentState(Mutex::new(None)));
```

In `invoke_handler`, add the three new commands:

```rust
set_xecm_config,               // modified — keep existing
xecm_connect,                  // keep existing
set_core_content_config,
core_content_connect_finish,
core_content_select_folder,
```

- [ ] **Step 7: Build check**

```bash
cargo build -p llm-wiki
```

Expected: clean compilation, no warnings.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: add CoreContentState, commands, and mutual exclusion with xECM

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 3: Core Content Content-Read Path in `fs.rs`

**Files:**
- Modify: `src-tauri/src/commands/fs.rs`

- [ ] **Step 1: Add imports at top of `fs.rs`**

After line 14 (`use crate::xecm_client::{XecmClient, XecmError};`), add:

```rust
use crate::core_content_client::{CoreContentClient, CoreContentError};
use crate::CoreContentState;
```

- [ ] **Step 2: Add Core Content helper functions after `is_xecm_source`**

After `is_xecm_source` (around line 33), add:

```rust
fn is_core_content_source(state: &tauri::State<'_, CoreContentState>, path: &str) -> bool {
    state.0.lock().ok().map(|g| {
        g.as_ref().map(|c| c.is_source_path(path)).unwrap_or(false)
    }).unwrap_or(false)
}

fn cc_err(e: CoreContentError) -> String {
    format!("Core Content: {e}")
}
```

- [ ] **Step 3: Add Core Content content-read block in `read_file`**

After the xECM block (around line 80, after `return Ok(text);` closing brace), add:

```rust
    if is_core_content_source(&state, &path) {
        let mut client = {
            let mut guard = cc_state.0.lock().map_err(|e| format!("Core Content: {e}"))?;
            guard.take().ok_or("Core Content: client not configured")?
        };
        let node_id = match client.resolve_path(&path).await {
            Ok(id) => id,
            Err(e) => {
                if let Ok(mut g) = cc_state.0.lock() { let _ = g.insert(client); }
                return Err(cc_err(e));
            }
        };
        let bytes = match client.get_content(&node_id).await {
            Ok(b) => b,
            Err(e) => {
                if let Ok(mut g) = cc_state.0.lock() { let _ = g.insert(client); }
                return Err(cc_err(e));
            }
        };
        // Put client back
        if let Ok(mut guard) = cc_state.0.lock() { let _ = guard.insert(client); }
        let p = std::path::Path::new(&path);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if ext == "pdf" || OFFICE_EXTS.contains(&ext.as_str()) {
            let cache_key = format!("{:x}", Md5::digest(path.as_bytes()));
            let cache_path = std::env::temp_dir().join(format!("cc-{cache_key}.{ext}"));
            std::fs::write(&cache_path, &bytes).map_err(|e| format!("Core Content: {e}"))?;
            let result = if ext == "pdf" {
                extract_pdf_text(&cache_path.to_string_lossy(), true)
                    .map_err(|e| format!("Core Content: {e}"))?
            } else {
                extract_office_text(&cache_path.to_string_lossy(), &ext)
                    .map_err(|e| format!("Core Content: {e}"))?
            };
            let _ = std::fs::remove_file(&cache_path);
            return Ok(result);
        }
        let text = String::from_utf8_lossy(&bytes).to_string();
        return Ok(text);
    }
```

- [ ] **Step 4: Update `read_file` signature to include `cc_state` parameter**

Change the function signature from:

```rust
pub async fn read_file(path: String, extract_images: Option<bool>, state: tauri::State<'_, XecmState>) -> Result<String, String> {
```

To:

```rust
pub async fn read_file(path: String, extract_images: Option<bool>, state: tauri::State<'_, XecmState>, cc_state: tauri::State<'_, CoreContentState>) -> Result<String, String> {
```

- [ ] **Step 5: Build check**

```bash
cargo build -p llm-wiki
```

Expected: clean compilation.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/fs.rs
git commit -m "feat: add Core Content content-read path to read_file

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 4: Core Content Poll Watcher in `file_sync.rs`

**Files:**
- Modify: `src-tauri/src/commands/file_sync.rs`

- [ ] **Step 1: Add Core Content imports**

After line 18 (`use crate::xecm_client::XecmClient;`), add:

```rust
use crate::core_content_client::{CoreContentClient, CoreContentSnapshotEntry};
```

After line 19 (`use crate::XecmState;`), add:

```rust
use crate::CoreContentState;
```

- [ ] **Step 2: Add Core Content snapshot constants**

After the xECM snapshot constants (around lines 347-362), add:

```rust
const CC_POLL_MIN_INTERVAL_SECS: u64 = 10;
const CC_SNAPSHOT_FILE: &str = ".llm-wiki/core-content-snapshot.json";

#[derive(Debug, Serialize, Deserialize)]
struct CoreContentSnapshot {
    folder_node_id: String,
    last_poll: String,
    nodes: std::collections::HashMap<String, CoreContentSnapshotEntry>,
}
```

- [ ] **Step 3: Add `start_core_content_poll_watcher` function**

After `start_xecm_poll_watcher` (ends around line 536), add:

```rust
fn start_core_content_poll_watcher(
    app: AppHandle,
    project_id: String,
    project_path: String,
    poll_interval_secs: u64,
    _auto_ingest: bool,
) {
    let interval = poll_interval_secs.max(CC_POLL_MIN_INTERVAL_SECS);
    let snapshot_path = format!("{}/{}", project_path, CC_SNAPSHOT_FILE);

    std::thread::spawn(move || {
        eprintln!("[cc-watcher] poll watcher thread spawned");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("Core Content poll watcher tokio runtime");

        rt.block_on(async move {
            let mut last_snapshot: Option<std::collections::HashMap<String, CoreContentSnapshotEntry>> =
                std::fs::read_to_string(&snapshot_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<CoreContentSnapshot>(&s).ok())
                    .map(|s| s.nodes);

            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval));
                eprintln!("[cc-watcher] poll cycle: checking for changes");

                let state = app.state::<CoreContentState>();

                let client_opt: Option<CoreContentClient> = {
                    let mut guard = match state.0.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            eprintln!("[cc-watcher] state poisoned, stopping");
                            break;
                        }
                    };
                    guard.take()
                };

                let current_snapshot = match client_opt {
                    Some(client) => match client.recursive_snapshot().await {
                        Ok(snap) => {
                            if let Ok(mut guard) = state.0.lock() {
                                *guard = Some(client);
                            }
                            Some(snap)
                        }
                        Err(e) => {
                            eprintln!("[cc-watcher] snapshot failed: {e}");
                            if let Ok(mut guard) = state.0.lock() {
                                *guard = Some(client);
                            }
                            None
                        }
                    },
                    None => {
                        eprintln!("[cc-watcher] client cleared, stopping");
                        break;
                    }
                };

                if let Some(snapshot) = current_snapshot {
                    if let Some(ref prev) = last_snapshot {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let mut changed_tasks: Vec<FileChangeTask> = Vec::new();

                        for (id, entry) in &snapshot {
                            match prev.get(id) {
                                None => {
                                    changed_tasks.push(FileChangeTask {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        project_id: project_id.clone(),
                                        path: format!("{}/raw/sources/{}", project_path, entry.name),
                                        kind: FileChangeKind::Created,
                                        status: FileChangeStatus::Pending,
                                        hash_before: None,
                                        hash_after: None,
                                        size: Some(entry.size),
                                        mtime_ms: None,
                                        created_at: now_ms,
                                        updated_at: now_ms,
                                        retry_count: 0,
                                        error: None,
                                        needs_rerun: false,
                                    });
                                }
                                Some(prev_entry) if prev_entry.modify_date != entry.modify_date => {
                                    changed_tasks.push(FileChangeTask {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        project_id: project_id.clone(),
                                        path: format!("{}/raw/sources/{}", project_path, entry.name),
                                        kind: FileChangeKind::Modified,
                                        status: FileChangeStatus::Pending,
                                        hash_before: None,
                                        hash_after: None,
                                        size: Some(entry.size),
                                        mtime_ms: None,
                                        created_at: now_ms,
                                        updated_at: now_ms,
                                        retry_count: 0,
                                        error: None,
                                        needs_rerun: false,
                                    });
                                }
                                _ => {}
                            }
                        }

                        for (id, entry) in prev {
                            if !snapshot.contains_key(id) {
                                changed_tasks.push(FileChangeTask {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    project_id: project_id.clone(),
                                    path: format!("{}/raw/sources/{}", project_path, entry.name),
                                    kind: FileChangeKind::Deleted,
                                    status: FileChangeStatus::Pending,
                                    hash_before: None,
                                    hash_after: None,
                                    size: Some(entry.size),
                                    mtime_ms: None,
                                    created_at: now_ms,
                                    updated_at: now_ms,
                                    retry_count: 0,
                                    error: None,
                                    needs_rerun: false,
                                });
                            }
                        }

                        if !changed_tasks.is_empty() {
                            let _ = app.emit(
                                EVENT_CHANGED,
                                FileSyncPayload {
                                    project_id: project_id.clone(),
                                    tasks: changed_tasks,
                                },
                            );
                        }
                    }

                    let snap = CoreContentSnapshot {
                        folder_node_id: String::new(),
                        last_poll: chrono::Utc::now().to_rfc3339(),
                        nodes: snapshot.clone(),
                    };
                    if let Ok(json) = serde_json::to_string_pretty(&snap) {
                        let _ = std::fs::write(&snapshot_path, json);
                    }
                    last_snapshot = Some(snapshot);
                }
            }
        });
    });
}
```

- [ ] **Step 4: Modify `start_project_file_watcher` to check Core Content state**

In the `start_project_file_watcher` function (around line 198), after the existing xECM check block, add:

```rust
        let cc_active = app.state::<CoreContentState>().0.lock().ok().map(|g| g.is_some()).unwrap_or(false);
        eprintln!("[cc-watcher] Core Content active: {cc_active}");
        if cc_active {
            eprintln!("[cc-watcher] starting poll watcher (interval={poll_interval}s)");
            start_core_content_poll_watcher(
                app.clone(),
                project_id.clone(),
                project_path,
                poll_interval,
                auto_ingest,
            );
            return Ok(FileChangeRescanResult {
                queue: FileChangeQueue { version: 1, tasks: vec![] },
                changed_tasks: vec![],
            });
        }
```

- [ ] **Step 5: Build check**

```bash
cargo build -p llm-wiki
```

Expected: clean compilation.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/file_sync.rs
git commit -m "feat: add Core Content poll watcher to file sync

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 5: Frontend IPC Command Wrappers

**Files:**
- Create: `src/commands/core-content.ts`

- [ ] **Step 1: Write `src/commands/core-content.ts`**

```typescript
import { invoke } from "@tauri-apps/api/core"

export interface CoreContentNode {
  id: string
  name: string
}

export interface CoreContentConnectResult {
  rootFolders: CoreContentNode[]
}

export function setCoreContentConfig(config: unknown): Promise<string> {
  return invoke<string>("set_core_content_config", { config })
}

export function coreContentConnectFinish(
  baseUrl: string,
  csrfToken: string,
  cookiesJson: string,
): Promise<CoreContentConnectResult> {
  return invoke<CoreContentConnectResult>("core_content_connect_finish", {
    baseUrl,
    csrfToken,
    cookiesJson,
  })
}

export function coreContentSelectFolder(
  folderNodeId: string,
  folderName: string,
): Promise<string> {
  return invoke<string>("core_content_select_folder", {
    folderNodeId,
    folderName,
  })
}
```

- [ ] **Step 2: Commit**

```bash
git add src/commands/core-content.ts
git commit -m "feat: add Core Content frontend IPC command wrappers

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 6: Wiki Store — `CoreContentConfig` Type and Setter

**Files:**
- Modify: `src/stores/wiki-store.ts`

- [ ] **Step 1: Add `CoreContentConfig` interface**

After the `XecmConfig` interface (around line 208), add:

```typescript
export interface CoreContentConfig {
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

- [ ] **Step 2: Add `coreContentConfig` to state interface**

After `xecmConfig: XecmConfig` in the `WikiState` interface, add:

```typescript
  coreContentConfig: CoreContentConfig
  setCoreContentConfig: (config: CoreContentConfig) => void
```

- [ ] **Step 3: Add default and setter to store creation**

After the `setXecmConfig` block (around line 489), add:

```typescript
  coreContentConfig: {
    enabled: false,
    baseUrl: "",
    folderNodeId: "",
    folderName: "",
    username: "",
    password: "",
    csrfToken: "",
    cookiesJson: "",
    pollIntervalSeconds: 30,
  },
  setCoreContentConfig: (coreContentConfig) => set({ coreContentConfig }),
```

- [ ] **Step 4: Export `CoreContentConfig` in the type exports**

At the bottom of the file (around line 493), update the export line:

```typescript
export type { WikiState, LlmConfig, SearchApiConfig, EmbeddingConfig, MultimodalConfig, OutputLanguage, ProxyConfig, ScheduledImportConfig, SourceWatchConfig, ApiConfig, XecmConfig, CoreContentConfig }
```

- [ ] **Step 5: Type check**

```bash
npm run typecheck
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/stores/wiki-store.ts
git commit -m "feat: add CoreContentConfig type and setter to wiki store

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 7: Settings Types — Core Content Draft Fields

**Files:**
- Modify: `src/components/settings/settings-types.ts`

- [ ] **Step 1: Add Core Content fields to `SettingsDraft`**

After the xECM fields (around line 91), add:

```typescript
  // Core Content
  coreContentEnabled: boolean
  coreContentBaseUrl: string
  coreContentFolderNodeId: string
  coreContentFolderName: string
  coreContentUsername: string
  coreContentPassword: string
  coreContentCsrfToken: string
  coreContentCookiesJson: string
  coreContentPollIntervalSeconds: number
```

- [ ] **Step 2: Type check**

```bash
npm run typecheck
```

Expected: errors in `settings-view.tsx` where the draft object is constructed — expected, will be fixed in Task 9.

- [ ] **Step 3: Commit**

```bash
git add src/components/settings/settings-types.ts
git commit -m "feat: add Core Content draft fields to settings types

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 8: `CoreContentSection.tsx` — Settings UI

**Files:**
- Create: `src/components/settings/sections/core-content-section.tsx`

- [ ] **Step 1: Write the complete `core-content-section.tsx`**

```tsx
import { useState } from "react"
import { useTranslation } from "react-i18next"
import { Cloud, CloudOff, Loader2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import type { SettingsDraft, DraftSetter } from "../settings-types"
import { coreContentConnectFinish, coreContentSelectFolder, type CoreContentNode } from "@/commands/core-content"

interface Props {
  draft: SettingsDraft
  setDraft: DraftSetter
}

export function CoreContentSection({ draft, setDraft }: Props) {
  const { t } = useTranslation()
  const [connecting, setConnecting] = useState(false)
  const [connectError, setConnectError] = useState<string | null>(null)
  const [folders, setFolders] = useState<CoreContentNode[]>([])
  const [showFolders, setShowFolders] = useState(false)
  const [connected, setConnected] = useState(false)
  const [disconnecting, setDisconnecting] = useState(false)

  // Rehydrate from persisted config
  const [didRehydrate, setDidRehydrate] = useState(false)
  if (!didRehydrate && draft.coreContentEnabled && draft.coreContentBaseUrl && draft.coreContentFolderName) {
    setDidRehydrate(true)
    setConnected(true)
  }

  async function handleConnect() {
    setConnecting(true)
    setConnectError(null)
    try {
      // First pass: manual cookie entry via prompt.
      // Full Tauri WebviewWindow auth + cookie extraction will be
      // added in a follow-up task (OS-dependent cookie APIs).
      // User logs in to Core Content in their browser, opens DevTools
      // (F12 → Application → Cookies), and pastes the CSRF token.
      const csrf = window.prompt(
        "Log into Core Content in your browser, then paste the CCM-XSRF-TOKEN cookie value here:"
      )
      if (!csrf) throw new Error("CSRF token not provided")

      // Get all cookies as JSON
      const cookiesStr = window.prompt(
        "Paste additional cookies as JSON {\"name\":\"value\",...} or leave empty:"
      )
      const cookiesJson = cookiesStr?.trim() || "{}"

      // Validate with backend
      const result = await coreContentConnectFinish(
        draft.coreContentBaseUrl,
        csrf,
        cookiesJson,
      )
      setFolders(result.rootFolders)
      setDraft("coreContentCsrfToken", csrf)
      setDraft("coreContentCookiesJson", cookiesJson)
      setShowFolders(true)
      setConnectError(null)
    } catch (err) {
      setConnectError(String(err))
    } finally {
      setConnecting(false)
    }
  }

  function handleSelectFolder(folder: CoreContentNode) {
    setDraft("coreContentFolderName", folder.name)
    setDraft("coreContentFolderNodeId", folder.id)
    setDraft("coreContentEnabled", true)
    coreContentSelectFolder(folder.id, folder.name).catch((err) =>
      console.error("Failed to select folder:", err)
    )
    setConnected(true)
    setShowFolders(false)
  }

  function handleDisconnect() {
    setDisconnecting(true)
    setDraft("coreContentEnabled", false)
    setDraft("coreContentBaseUrl", "")
    setDraft("coreContentFolderNodeId", "")
    setDraft("coreContentFolderName", "")
    setDraft("coreContentUsername", "")
    setDraft("coreContentPassword", "")
    setDraft("coreContentCsrfToken", "")
    setDraft("coreContentCookiesJson", "")
    setConnected(false)
    setFolders([])
    setShowFolders(false)
    setConnectError(null)
    setDisconnecting(false)
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {draft.coreContentEnabled && connected ? (
          <Cloud className="h-5 w-5 text-green-500" />
        ) : (
          <CloudOff className="h-5 w-5 text-muted-foreground" />
        )}
        <h2 className="text-lg font-semibold">{t("settings.coreContent.title", "Core Content Connection")}</h2>
      </div>

      {/* Disabled state when xECM is active */}
      {draft.xecmEnabled && (
        <div className="rounded-md border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950">
          <p className="text-sm text-amber-700 dark:text-amber-300">
            {t("settings.coreContent.disabledByXecm", "Core Content is unavailable while xECM is connected. Disconnect xECM first.")}
          </p>
        </div>
      )}

      {draft.coreContentEnabled && connected ? (
        <div className="space-y-4">
          <div className="rounded-md border border-green-200 bg-green-50 p-4 dark:border-green-800 dark:bg-green-950">
            <p className="text-sm font-medium text-green-700 dark:text-green-300">
              Connected to <strong>{draft.coreContentFolderName}</strong> at {draft.coreContentBaseUrl}
            </p>
          </div>

          <div className="space-y-2">
            <Label>Poll interval (seconds)</Label>
            <Input
              type="number"
              min={10}
              max={300}
              value={draft.coreContentPollIntervalSeconds}
              onChange={(e) =>
                setDraft("coreContentPollIntervalSeconds", parseInt(e.target.value) || 30)
              }
            />
          </div>

          <Button variant="outline" onClick={handleDisconnect} disabled={disconnecting}>
            {disconnecting ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
            Disconnect
          </Button>
        </div>
      ) : showFolders ? (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Select a folder to use as your source layer:
          </p>
          <div className="space-y-2">
            {folders.map((f) => (
              <button
                key={f.id}
                type="button"
                onClick={() => handleSelectFolder(f)}
                className="w-full rounded-md border px-4 py-3 text-left transition-colors hover:bg-accent hover:text-accent-foreground"
              >
                <div className="font-medium">{f.name}</div>
              </button>
            ))}
          </div>
          <Button variant="ghost" size="sm" onClick={() => setShowFolders(false)}>
            Back
          </Button>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="cc-url">Base URL</Label>
            <Input
              id="cc-url"
              placeholder="https://corecontent.dev.ca.opentext.com/subscriptions/avstcc"
              value={draft.coreContentBaseUrl}
              onChange={(e) => setDraft("coreContentBaseUrl", e.target.value)}
              disabled={draft.xecmEnabled}
            />
          </div>

          {connectError && (
            <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {connectError}
            </div>
          )}

          <Button
            onClick={handleConnect}
            disabled={connecting || !draft.coreContentBaseUrl || draft.xecmEnabled}
          >
            {connecting ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
            Connect to Core Content
          </Button>
        </div>
      )}
    </div>
  )
}
```

- [ ] **Step 2: Commit**

```bash
git add src/components/settings/sections/core-content-section.tsx
git commit -m "feat: add CoreContentSection settings UI

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 9: Mutual Exclusion in `XecmSection` and Settings View Integration

**Files:**
- Modify: `src/components/settings/sections/xecm-section.tsx`
- Modify: `src/components/settings/settings-view.tsx`

- [ ] **Step 1: Add disable logic to `XecmSection`**

In `xecm-section.tsx`, wrap the existing content sections in a conditional opacity div when Core Content is active. Find the JSX pattern:

```tsx
      {draft.xecmEnabled && connected ? (
        <div className="space-y-4">
```

Before this block, add the warning banner that shows when Core Content is connected:

```tsx
      {draft.coreContentEnabled && (
        <div className="rounded-md border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950">
          <p className="text-sm text-amber-700 dark:text-amber-300">
            xECM is unavailable while Core Content is connected. Disconnect Core Content first.
          </p>
        </div>
      )}
```

Then wrap the three state branches (connected, workspace list, disconnected form) with an opacity wrapper:

```tsx
      <div className={draft.coreContentEnabled ? "opacity-50 pointer-events-none" : ""}>
        {/* existing three-state content: connected, workspace selection, login form */}
      </div>
```

- [ ] **Step 2: Update `initialDraft` function signature to accept `CoreContentConfig`**

Add a parameter to `initialDraft` (line 91). After the `xecmConfig` parameter (line 101), add:

```typescript
  coreContentConfig: ReturnType<typeof useWikiStore.getState>["coreContentConfig"],
```

Before the `return` statement (line 118), add fields to the returned object. After `xecmTicket` (line 172), add:

```typescript
    coreContentEnabled: coreContentConfig.enabled,
    coreContentBaseUrl: coreContentConfig.baseUrl,
    coreContentFolderNodeId: coreContentConfig.folderNodeId,
    coreContentFolderName: coreContentConfig.folderName,
    coreContentUsername: coreContentConfig.username,
    coreContentPassword: coreContentConfig.password ?? "",
    coreContentCsrfToken: coreContentConfig.csrfToken ?? "",
    coreContentCookiesJson: coreContentConfig.cookiesJson ?? "",
    coreContentPollIntervalSeconds: coreContentConfig.pollIntervalSeconds,
```

- [ ] **Step 3: Update all call sites of `initialDraft`**

Search for `initialDraft(` in settings-view.tsx (found at ~line 214 and ~line 269). At each call site, add the `coreContentConfig` parameter:

```typescript
    coreContentConfig: useWikiStore.getState().coreContentConfig,
```

within the argument list after `xecmConfig`.

- [ ] **Step 4: Add `CoreContentSection` import and render**

Add import (after line 45 `import { XecmSection } from "./sections/xecm-section"`):

```typescript
import { CoreContentSection } from "./sections/core-content-section"
```

In the source-watch category tab content (where `XecmSection` and `SourceWatchSection` are rendered), add after the `XecmSection` line:

```tsx
            <CoreContentSection draft={draft} setDraft={setDraft} />
```

- [ ] **Step 5: Type check and fix remaining errors**

```bash
npm run typecheck
```

Expected: clean. If any call sites of `initialDraft` are missed, fix them.

- [ ] **Step 6: Commit**

```bash
git add src/components/settings/sections/xecm-section.tsx src/components/settings/settings-view.tsx
git commit -m "feat: add CoreContentSection to settings and mutual exclusion UX

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 10: Config Persistence in `project-store.ts`

**Files:**
- Modify: `src/lib/project-store.ts`

- [ ] **Step 1: Add `saveCoreContentConfig` and `loadCoreContentConfig`**

After the xECM persistence block (around line 386), add:

```typescript
// ── Core Content config persistence ──────────────────────────────────────────
// Written directly to the project directory's .llm-wiki/core-content-config.json
// using Tauri commands, so the Rust backend can also read it without going
// through the store layer.

export async function saveCoreContentConfig(
  config: import("@/stores/wiki-store").CoreContentConfig,
  projectPath: string,
): Promise<void> {
  const pp = normalizePath(projectPath)
  const configPath = `${pp}/.llm-wiki/core-content-config.json`
  // Never persist cookies or CSRF token to disk — they're ephemeral
  await invoke("write_file_atomic", {
    path: configPath,
    contents: JSON.stringify(
      { ...config, csrfToken: "", cookiesJson: "" },
      null,
      2,
    ),
  })
}

export async function loadCoreContentConfig(
  projectPath: string,
): Promise<import("@/stores/wiki-store").CoreContentConfig | null> {
  const pp = normalizePath(projectPath)
  const configPath = `${pp}/.llm-wiki/core-content-config.json`
  try {
    const exists = await invoke<boolean>("file_exists", { path: configPath })
    if (!exists) return null
    const content = await invoke<string>("read_file", {
      path: configPath,
      extractImages: false,
    })
    return JSON.parse(content) as import("@/stores/wiki-store").CoreContentConfig
  } catch {
    return null
  }
}
```

- [ ] **Step 2: Build check**

```bash
npm run typecheck
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/lib/project-store.ts
git commit -m "feat: add Core Content config persistence to project-store

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 11: Config Hydration in `App.tsx`

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Add Core Content hydration after xECM hydration block**

After the xECM hydration block (around line 446), add:

```typescript
    // Hydrate Core Content config
    try {
      const { loadCoreContentConfig } = await import("@/lib/project-store")
      const savedCC = await loadCoreContentConfig(proj.path)
      if (savedCC?.enabled) {
        useWikiStore.getState().setCoreContentConfig(savedCC)
        await invoke("set_core_content_config", { config: savedCC })
        console.log("[core_content] hydrated config for folder", savedCC.folderName)
      } else {
        useWikiStore.getState().setCoreContentConfig({
          enabled: false,
          baseUrl: "",
          folderNodeId: "",
          folderName: "",
          username: "",
          password: "",
          csrfToken: "",
          cookiesJson: "",
          pollIntervalSeconds: 30,
        })
        await invoke("set_core_content_config", { config: { enabled: false } })
      }
    } catch (err) {
      console.error("[core_content] failed to hydrate config:", err)
    }
```

- [ ] **Step 2: Add Core Content cleanup on project switch**

Near the existing xECM clear block (around line 488), add after:

```typescript
    // Clear Core Content state on project switch
    try {
      await invoke("set_core_content_config", { config: { enabled: false } })
    } catch {}
```

- [ ] **Step 3: Type check**

```bash
npm run typecheck
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx
git commit -m "feat: add Core Content config hydration and cleanup to App.tsx

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

### Task 12: End-to-End Integration Test

**Files:**
- No new files — manual verification

- [ ] **Step 1: Full build and verify no regressions**

```bash
npm run tauri build
```

Expected: successful build with MSI/NSIS outputs. xECM path still functional.

- [ ] **Step 2: Push all commits**

```bash
git push origin main
```

---

## File Change Summary

### Created files
1. `src-tauri/src/core_content_client.rs` — `CoreContentClient`, `CoreContentConfig`, `CoreContentError`
2. `src/components/settings/sections/core-content-section.tsx` — Settings UI
3. `src/commands/core-content.ts` — Frontend IPC wrappers

### Modified files
1. `src-tauri/src/lib.rs` — State + commands + mutual exclusion
2. `src-tauri/src/main.rs` — Module declaration
3. `src-tauri/src/commands/file_sync.rs` — Core Content poll watcher
4. `src-tauri/src/commands/fs.rs` — Core Content content-read path
5. `src/stores/wiki-store.ts` — `CoreContentConfig` type + setter
6. `src/components/settings/settings-types.ts` — Draft fields
7. `src/components/settings/settings-view.tsx` — Include `CoreContentSection`
8. `src/components/settings/sections/xecm-section.tsx` — Mutual exclusion disable
9. `src/App.tsx` — Config hydration + cleanup
10. `src/lib/project-store.ts` — Config persistence
