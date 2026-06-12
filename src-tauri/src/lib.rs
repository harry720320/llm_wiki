mod api_server;
mod cc_cookies;
mod clip_server;
mod commands;
mod panic_guard;
mod proxy;
mod tray;
mod types;
mod core_content_client;
mod xecm_client;

use panic_guard::run_guarded;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::Manager;
use tokio::sync::oneshot;

use crate::core_content_client::{CoreContentClient, CoreContentConfig};
use crate::xecm_client::{XecmClient, XecmConfig};

struct CloseBehaviorState(Mutex<String>);
struct TrayAvailabilityState(Mutex<bool>);

struct XecmState(Mutex<Option<XecmClient>>);

struct CoreContentState(Mutex<Option<CoreContentClient>>);

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoreContentLoginResult {
    pub(crate) csrf_token: String,
    pub(crate) cookies_json: String,
    /// JSON array of container nodes from the Core Content API, fetched
    /// directly by the webview JS (which has access to HttpOnly session
    /// cookies that document.cookie cannot return).
    pub(crate) root_folders_json: String,
}

pub(crate) struct CCLoginChannels(pub(crate) Mutex<HashMap<String, oneshot::Sender<CoreContentLoginResult>>>);


#[tauri::command]
fn clip_server_status() -> String {
    run_guarded("clip_server_status", || {
        Ok(clip_server::get_daemon_status().to_string())
    })
    .unwrap_or_else(|e| format!("error: {e}"))
}

#[tauri::command]
fn api_server_status() -> String {
    run_guarded("api_server_status", || {
        Ok(api_server::get_api_status().to_string())
    })
    .unwrap_or_else(|e| format!("error: {e}"))
}

#[tauri::command]
fn api_server_reload_config() -> String {
    run_guarded("api_server_reload_config", || {
        api_server::invalidate_config_cache();
        Ok("ok".to_string())
    })
    .unwrap_or_else(|e| format!("error: {e}"))
}

#[tauri::command]
fn mcp_server_entry_path(app: tauri::AppHandle) -> Result<String, String> {
    run_guarded("mcp_server_entry_path", || {
        let relative = std::path::Path::new("mcp-server")
            .join("dist")
            .join("src")
            .join("index.js");
        let mut candidates = Vec::new();

        let mut push_repo_candidates = |base: std::path::PathBuf| {
            candidates.push(base.join(&relative));
            candidates.push(base.join("..").join(&relative));
            candidates.push(base.join("..").join("..").join(&relative));
        };

        push_repo_candidates(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        if let Ok(cwd) = std::env::current_dir() {
            push_repo_candidates(cwd);
        }
        if let Ok(resource_dir) = app.path().resource_dir() {
            candidates.push(resource_dir.join(&relative));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                candidates.push(exe_dir.join(&relative));
                candidates.push(exe_dir.join("..").join("Resources").join(&relative));
            }
        }

        for candidate in &candidates {
            if candidate.is_file() {
                return Ok(candidate
                    .canonicalize()
                    .unwrap_or_else(|_| candidate.clone())
                    .to_string_lossy()
                    .into_owned());
            }
        }

        Err("MCP server entry was not found. Run `npm run mcp:build` from the LLM Wiki repository, then reopen Settings.".to_string())
    })
}

/// Apply a proxy configuration to the process env immediately, so the
/// next outbound HTTP request picks it up without needing the user to
/// restart the app. tauri-plugin-http builds a fresh
/// `reqwest::ClientBuilder` per fetch and reqwest's `auto_sys_proxy`
/// re-reads HTTP_PROXY / HTTPS_PROXY / NO_PROXY each time, so updating
/// these env vars is sufficient to flip the proxy on/off live.
///
/// Returns the same human-readable summary `apply_proxy_env` produces
/// for logging.
#[tauri::command]
fn set_proxy_env(config: proxy::ProxyConfig) -> String {
    let summary = proxy::apply_proxy_env(&config);
    eprintln!("[proxy] live update: {summary}");
    summary
}

#[tauri::command]
fn set_close_behavior(
    value: String,
    state: tauri::State<'_, CloseBehaviorState>,
) -> Result<String, String> {
    let normalized = match value.as_str() {
        "ask" | "minimize" | "exit" => value,
        other => return Err(format!("Invalid close behavior: {other}")),
    };
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "Close behavior state is unavailable".to_string())?;
    *guard = normalized.clone();
    Ok(normalized)
}

/// Set/reset the active xECM client.
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
            eprintln!("[xecm] set_xecm_config: enabled=true, url={}, ticket={}",
                cfg.base_url,
                cfg.ticket.as_deref().map(|_| "present").unwrap_or("MISSING"));
            // Mutual exclusion: clear Core Content
            if let Ok(mut g) = cc_state.0.lock() { *g = None; }
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

/// Browse root folders for Core Content webview login flow.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
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
    eprintln!("[cc-connect] core_content_connect_finish: base_url={base_url} csrf_token_len={} cookies_json_len={}", csrf_token.len(), cookies_json.len());
    if let Ok(cookies) = serde_json::from_str::<HashMap<String, String>>(&cookies_json) {
        let names: Vec<&str> = cookies.keys().map(|s| s.as_str()).collect();
        eprintln!("[cc-connect] cookie names: {names:?}");
    }
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

/// Select a folder after webview login.
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

/// Open a webview for Core Content login.  The injected JS detects the
/// CSRF token in document.cookie after authentication and signals completion
/// by writing `window.location.hash = "__cc_data__" + <cookie JSON>`.
/// Rust polls `webview.url()` for the hash signal, then extracts all cookies
/// (including HttpOnly) from the WebView2 cookie store, merges them with the
/// hash cookies, and returns the result.
///
/// We poll the URL hash instead of using an HTTP callback or Tauri IPC
/// because:  (1) navigation from HTTPS → HTTP (callback URL) is silently
/// blocked by WebView2, and (2) `window.__TAURI__` is unavailable in
/// external-URL webviews.
#[tauri::command]
async fn core_content_start_login(
    app: tauri::AppHandle,
    base_url: String,
    state: tauri::State<'_, CCLoginChannels>,
) -> Result<CoreContentLoginResult, String> {
    let login_id = uuid::Uuid::new_v4().to_string();
    eprintln!("[cc-login] core_content_start_login: starting login_id={login_id} base_url={base_url}");

    let js = include_str!("cc_login_init.js")
        .replace("__CC_BASE_URL__", &base_url)
        .replace("__CC_LOGIN_ID__", &login_id);

    let _webview = tauri::WebviewWindowBuilder::new(
        &app,
        &format!("core-content-login-{login_id}"),
        tauri::WebviewUrl::External(
            base_url
                .parse()
                .map_err(|e| format!("invalid base_url: {e}"))?,
        ),
    )
    .title("Core Content Login")
    .inner_size(800.0, 600.0)
    .initialization_script(&js)
    .build()
    .map_err(|e| format!("failed to create login window: {e}"))?;

    // Poll webview.url() for the __cc_data__ hash signal.  The JS sets
    // window.location.hash = "__cc_data__" + <cookie JSON> when it detects
    // the CSRF token.  Hash changes are safe — no navigation, no blocking.
    let webview_label = format!("core-content-login-{login_id}");
    eprintln!("[cc-login] polling webview URL for hash signal...");

    let result: CoreContentLoginResult = loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let wv = match app.get_webview_window(&webview_label) {
            Some(w) => w,
            None => {
                eprintln!("[cc-login] webview window closed by user");
                break CoreContentLoginResult {
                    csrf_token: String::new(),
                    cookies_json: String::new(),
                    root_folders_json: String::new(),
                };
            }
        };

        let current_url = wv.url().map(|u| u.to_string()).unwrap_or_default();

        if let Some(hash_pos) = current_url.find("__cc_data__") {
            eprintln!("[cc-login] hash signal detected!");

            // 1. Parse document.cookie from the hash
            let encoded = &current_url[hash_pos + "__cc_data__".len()..];
            let decoded = percent_decode_url(encoded);
            let mut all_cookies: std::collections::HashMap<String, String> =
                match serde_json::from_str(&decoded) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[cc-login] JSON parse error from hash: {e}");
                        break CoreContentLoginResult {
                            csrf_token: String::new(),
                            cookies_json: String::new(),
                            root_folders_json: String::new(),
                        };
                    }
                };
            eprintln!("[cc-login] {} cookies from document.cookie (hash)", all_cookies.len());

            // 2. Enrich with ALL cookies from WebView2 (includes HttpOnly)
            //    Uses the FIXED extract_all_cookies — no main-thread deadlock.
            let wv2 = wv.clone();
            let url = base_url.clone();
            match tokio::task::spawn_blocking(move || {
                crate::cc_cookies::extract_all_cookies(&wv2, &url)
            }).await {
                Ok(Ok(webview_cookies)) => {
                    eprintln!("[cc-login] WebView2 gave {} additional cookies", webview_cookies.len());
                    for (k, v) in webview_cookies {
                        all_cookies.entry(k).or_insert(v);
                    }
                }
                Ok(Err(e)) => eprintln!("[cc-login] WebView2 extract failed (using hash cookies only): {e}"),
                Err(join_err) => eprintln!("[cc-login] spawn_blocking join error: {join_err}"),
            }

            let csrf_token = all_cookies
                .get("CCM-XSRF-TOKEN")
                .cloned()
                .unwrap_or_default();
            let cookies_json = serde_json::to_string(&all_cookies).unwrap_or_default();
            eprintln!(
                "[cc-login] merged {} total cookies, csrf_token_len={}",
                all_cookies.len(),
                csrf_token.len()
            );

            // 3. Close the webview
            if let Err(e) = wv.close() {
                eprintln!("[cc-login] failed to close webview: {e}");
            }

            let channel_result = CoreContentLoginResult {
                csrf_token,
                cookies_json,
                root_folders_json: String::new(),
            };

            // 4. If we got a CSRF token, fetch root folders via reqwest
            if !channel_result.csrf_token.is_empty() {
                eprintln!("[cc-login] fetching root folders via reqwest...");
                let temp_config = CoreContentConfig {
                    enabled: true,
                    base_url: base_url.clone(),
                    folder_node_id: String::new(),
                    folder_name: String::new(),
                    username: String::new(),
                    password: String::new(),
                    csrf_token: channel_result.csrf_token.clone(),
                    cookies_json: channel_result.cookies_json.clone(),
                    poll_interval_seconds: 30,
                };
                let client = CoreContentClient::new(
                    temp_config,
                    std::path::PathBuf::from(".llm-wiki/core-content-cache"),
                );
                match client.list_root_folders().await {
                    Ok(folders) => {
                        let json = serde_json::to_string(
                            &folders
                                .iter()
                                .map(|f| serde_json::json!({"id": f.id, "name": f.name}))
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default();
                        eprintln!("[cc-login] reqwest listed {} root folders", folders.len());
                        break CoreContentLoginResult {
                            root_folders_json: json,
                            ..channel_result
                        };
                    }
                    Err(e) => {
                        eprintln!("[cc-login] reqwest list_root_folders failed: {e}");
                        let error_json =
                            format!("{{\"error\":\"{}\"}}", e.to_string().replace('"', "\\\""));
                        break CoreContentLoginResult {
                            root_folders_json: error_json,
                            ..channel_result
                        };
                    }
                }
            }
            break channel_result;
        }
    };

    // Clean up any stale channel entry (from old HTTP-callback flow)
    if let Ok(mut guard) = state.0.lock() {
        guard.remove(&login_id);
    }

    Ok(result)
}

/// Called by JS in the login webview via Tauri IPC when cookies are detected.
/// Uses the WebView2 cookie manager to extract ALL cookies (including HttpOnly),
/// signals the oneshot channel so core_content_start_login can return, and
/// closes the webview window.
#[tauri::command]
fn core_content_login_complete(
    app: tauri::AppHandle,
    login_id: String,
    base_url: String,
    cookies: String,
    state: tauri::State<'_, CCLoginChannels>,
) -> Result<(), String> {
    eprintln!("[cc-login] core_content_login_complete invoked via IPC: login_id={login_id} cookies_len={} base_url={base_url}",
        cookies.len());

    let tx = {
        let mut guard = state
            .0
            .lock()
            .map_err(|e| format!("login channels unavailable: {e}"))?;
        guard.remove(&login_id)
    };

    let label = format!("core-content-login-{login_id}");

    // Parse cookies from document.cookie (non-HttpOnly only).
    let mut csrf_token = String::new();
    let mut cookies_map: HashMap<String, String> = HashMap::new();
    for part in cookies.split(';') {
        let trimmed = part.trim();
        if let Some(eq) = trimmed.find('=') {
            let name = trimmed[..eq].trim().to_string();
            let value = trimmed[eq + 1..].trim().to_string();
            if name == "CCM-XSRF-TOKEN" {
                csrf_token = value.clone();
            }
            cookies_map.insert(name, value);
        }
    }

    eprintln!("[cc-login] parsed {} cookies from document.cookie (non-HttpOnly)", cookies_map.len());

    // Enrich with ALL cookies from WebView2 (includes HttpOnly session cookies
    // that document.cookie cannot return).
    let cookie_uri = if base_url.is_empty() {
        app.get_webview_window(&label)
            .and_then(|w| w.url().ok())
            .map(|u| u.to_string())
            .unwrap_or_default()
    } else {
        base_url.clone()
    };
    eprintln!("[cc-login] extracting WebView2 cookies for uri={cookie_uri}");

    if let Some(webview) = app.get_webview_window(&label) {
        match crate::cc_cookies::extract_all_cookies(&webview, &cookie_uri) {
            Ok(all_cookies) => {
                eprintln!("[cc-login] WebView2 returned {} total cookies (including HttpOnly)", all_cookies.len());
                for (name, value) in all_cookies {
                    cookies_map.insert(name, value);
                }
            }
            Err(e) => {
                eprintln!("[cc-login] WARNING: could not extract WebView2 cookies: {e}");
            }
        }

        // If csrf_token wasn't found in document.cookie (HttpOnly), pull
        // it from the WebView2-enriched cookies_map.
        if csrf_token.is_empty() {
            if let Some(token) = cookies_map.get("CCM-XSRF-TOKEN") {
                csrf_token = token.clone();
                eprintln!("[cc-login] found CSRF token in WebView2 cookies (was HttpOnly)");
            }
        }

        // Close the webview window — the login is complete.
        eprintln!("[cc-login] closing login webview '{label}'");
        if let Err(e) = webview.close() {
            eprintln!("[cc-login] failed to close webview: {e}");
        }
    } else {
        eprintln!("[cc-login] WARNING: webview window '{label}' not found");
    }

    let cookies_json = serde_json::to_string(&cookies_map).unwrap_or_default();
    eprintln!("[cc-login] final cookie count: {} csrf_token_len={}", cookies_map.len(), csrf_token.len());

    if let Some(tx) = tx {
        let _ = tx.send(CoreContentLoginResult {
            csrf_token,
            cookies_json,
            root_folders_json: String::new(),
        });
        eprintln!("[cc-login] channel result sent to core_content_start_login");
    } else {
        eprintln!("[cc-login] no channel found for login_id={login_id} (already consumed or expired)");
    }

    Ok(())
}

#[derive(serde::Serialize)]
struct XecmConnectResult {
    ticket: String,
    workspaces: Vec<serde_json::Value>,
}

/// Authenticate and list available workspaces.
#[tauri::command]
async fn xecm_connect(
    base_url: String,
    username: String,
    password: String,
) -> Result<XecmConnectResult, String> {
    let ticket = XecmClient::authenticate(&base_url, &username, &password)
        .await
        .map_err(|e| e.to_string())?;

    let workspaces = XecmClient::list_workspaces(&base_url, &ticket)
        .await
        .map_err(|e| e.to_string())?;

    eprintln!("[xecm] xecm_connect: authenticated, ticket_len={}", ticket.len());

    Ok(XecmConnectResult {
        ticket,
        workspaces: workspaces
            .into_iter()
            .filter(|w| w.container)
            .map(|w| serde_json::json!({
                "name": w.name,
                "id": w.id,
                "type": w.type_,
            }))
            .collect(),
    })
}

fn percent_decode_url(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn close_behavior<R: tauri::Runtime>(window: &tauri::Window<R>) -> String {
    window
        .state::<CloseBehaviorState>()
        .0
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "minimize".to_string())
}

fn tray_available<R: tauri::Runtime>(window: &tauri::Window<R>) -> bool {
    window
        .state::<TrayAvailabilityState>()
        .0
        .lock()
        .map(|value| *value)
        .unwrap_or(false)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    clip_server::start_clip_server();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        // Rust-backed fetch so third-party LLM APIs that reject
        // browser-origin headers via CORS preflight (MiniMax, Volcengine
        // Ark's api/coding/v3, etc.) still work. Requests leave the app
        // from Rust, never the webview.
        .plugin(tauri_plugin_http::init())
        .setup(|app| {
            // Let the PDF extractor find the bundled pdfium dynamic
            // library via Tauri's platform-correct resource path.
            if let Ok(dir) = app.path().resource_dir() {
                commands::fs::set_resource_dir_hint(dir);
            }
            // Apply user-configured global HTTP proxy by setting
            // HTTP_PROXY / HTTPS_PROXY / NO_PROXY env vars BEFORE
            // any HTTP request is made. tauri-plugin-http's reqwest
            // client reads these on first construction. Lives next
            // to the resource-dir hint so the proxy applies to
            // everything: LLM, embedding, update check, deep
            // research, captioning. See src-tauri/src/proxy.rs.
            if let Ok(dir) = app.path().app_data_dir() {
                let store_path = dir.join("app-state.json");
                eprintln!("[proxy] reading from {}", store_path.display());
                if let Some(cfg) = proxy::read_proxy_config_from_store(&store_path) {
                    let summary = proxy::apply_proxy_env(&cfg);
                    eprintln!("[proxy] {summary}");
                } else {
                    eprintln!("[proxy] no proxyConfig in store, requests go direct");
                }
            } else {
                eprintln!("[proxy] could not resolve app_data_dir");
            }
            // Registry of running `claude` subprocesses, keyed by the
            // frontend-generated stream id. Populated by claude_cli_spawn,
            // drained on process exit or by claude_cli_kill.
            app.manage(commands::claude_cli::ClaudeCliState::default());
            app.manage(commands::codex_cli::CodexCliState::default());
            app.manage(commands::file_sync::FileSyncState::default());
            app.manage(CloseBehaviorState(Mutex::new("minimize".to_string())));
            app.manage(XecmState(Mutex::new(None)));
            app.manage(CoreContentState(Mutex::new(None)));
            app.manage(CCLoginChannels(Mutex::new(HashMap::new())));
            let tray_available = match tray::create_tray(app.handle()) {
                Ok(()) => true,
                Err(err) => {
                    eprintln!("[tray] system tray unavailable, continuing without it: {err}");
                    false
                }
            };
            app.manage(TrayAvailabilityState(Mutex::new(tray_available)));
            api_server::start_api_server(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::fs::read_file,
            commands::fs::write_file,
            commands::fs::write_file_atomic,
            commands::fs::list_directory,
            commands::fs::copy_file,
            commands::fs::copy_directory,
            commands::fs::preprocess_file,
            commands::fs::delete_file,
            commands::fs::find_related_wiki_pages,
            commands::fs::create_directory,
            commands::fs::file_exists,
            commands::fs::get_file_modified_time,
            commands::fs::get_file_size,
            commands::fs::get_file_md5,
            commands::fs::read_file_as_base64,
            commands::project::create_project,
            commands::project::open_project,
            commands::project::open_project_folder,
            commands::search::search_project,
            clip_server_status,
            api_server_status,
            api_server_reload_config,
            mcp_server_entry_path,
            commands::vectorstore::vector_upsert,
            commands::vectorstore::vector_search,
            commands::vectorstore::vector_delete,
            commands::vectorstore::vector_count,
            commands::vectorstore::vector_upsert_chunks,
            commands::vectorstore::vector_search_chunks,
            commands::vectorstore::vector_delete_page,
            commands::vectorstore::vector_count_chunks,
            commands::vectorstore::vector_legacy_row_count,
            commands::vectorstore::vector_drop_legacy,
            commands::claude_cli::claude_cli_detect,
            commands::claude_cli::claude_cli_spawn,
            commands::claude_cli::claude_cli_kill,
            commands::codex_cli::codex_cli_detect,
            commands::codex_cli::codex_cli_spawn,
            commands::codex_cli::codex_cli_kill,
            commands::extract_images::extract_pdf_images_cmd,
            commands::extract_images::extract_office_images_cmd,
            commands::extract_images::extract_and_save_pdf_images_cmd,
            commands::extract_images::extract_and_save_office_images_cmd,
            commands::file_sync::start_project_file_watcher,
            commands::file_sync::stop_project_file_watcher,
            commands::file_sync::rescan_project_files,
            commands::file_sync::get_file_change_queue,
            commands::file_sync::retry_file_change_task,
            commands::file_sync::ignore_file_change_task,
            set_proxy_env,
            set_close_behavior,
            set_xecm_config,
            xecm_connect,
            set_core_content_config,
            core_content_connect_finish,
            core_content_select_folder,
            core_content_start_login,
            core_content_login_complete,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Login webview windows should close without the behavior dialog.
                let label = window.label().to_string();
                if label.starts_with("core-content-login-") {
                    return;
                }
                api.prevent_close();
                let behavior = close_behavior(window);
                let win = window.clone();
                let app = window.app_handle().clone();
                match behavior.as_str() {
                    "exit" => {
                        tauri::async_runtime::spawn(async move {
                            let _ = win.destroy();
                            app.exit(0);
                        });
                    }
                    "minimize" => {
                        if tray_available(window) {
                            let _ = window.hide();
                        } else {
                            let _ = window.minimize();
                        }
                    }
                    _ => {
                        tauri::async_runtime::spawn(async move {
                            use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
                            let confirmed = app
                                .dialog()
                                .message(
                                    "Quit LLM Wiki? Choose Quit to exit. Choose Hide Window to keep background features running.",
                                )
                                .title("LLM Wiki")
                                .buttons(MessageDialogButtons::OkCancelCustom(
                                    "Quit".to_string(),
                                    "Hide Window".to_string(),
                                ))
                                .kind(tauri_plugin_dialog::MessageDialogKind::Warning)
                                .blocking_show();

                            if confirmed {
                                let _ = win.destroy();
                                app.exit(0);
                            } else {
                                let _ = win.hide();
                            }
                        });
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows,
                ..
            } = event
            {
                if !has_visible_windows {
                    use tauri::Manager;
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
            let _ = (app, event); // suppress unused warnings on non-macOS
        });
}
