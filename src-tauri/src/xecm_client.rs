use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
pub struct XecmNode {
    pub id: u64,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: u64,
    #[serde(default)]
    pub container: bool,
    #[serde(default)]
    pub size: u64,
    pub modify_date: Option<String>,
    pub mime_type: Option<String>,
    pub parent_id: i64,
}

#[derive(Debug, Deserialize)]
struct XecmNodeListResponse {
    data: Vec<XecmNode>,
    page_total: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct XecmNodeResponse {
    data: XecmNode,
}

#[derive(Debug, Deserialize)]
struct XecmVolumesResponse {
    data: Vec<XecmNode>,
}

#[derive(Debug, Deserialize)]
struct XecmAuthResponse {
    ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XecmConfig {
    pub enabled: bool,
    pub base_url: String,
    pub workspace_node_id: u64,
    pub workspace_name: String,
    pub username: String,
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub password: String,
    pub poll_interval_seconds: u64,
}

#[derive(Debug)]
pub enum XecmError {
    Auth(String),
    Network(String),
    NotFound(String),
    RateLimited,
    Other(String),
}

impl std::fmt::Display for XecmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XecmError::Auth(m) => write!(f, "xECM auth error: {m}"),
            XecmError::Network(m) => write!(f, "xECM network error: {m}"),
            XecmError::NotFound(m) => write!(f, "xECM not found: {m}"),
            XecmError::RateLimited => write!(f, "xECM rate limited"),
            XecmError::Other(m) => write!(f, "xECM error: {m}"),
        }
    }
}

impl From<reqwest::Error> for XecmError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            XecmError::Network("request timed out".to_string())
        } else if e.is_connect() {
            XecmError::Network(format!("connection failed: {e}"))
        } else {
            XecmError::Other(format!("HTTP request failed: {e}"))
        }
    }
}

pub struct XecmClient {
    http: reqwest::Client,
    config: XecmConfig,
    ticket: Mutex<Option<String>>,
    path_cache: HashMap<String, u64>,
    cache_dir: PathBuf,
}

impl XecmClient {
    pub fn new(config: XecmConfig, cache_dir: PathBuf) -> Self {
        let ticket = config.ticket.clone();
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            config,
            ticket: Mutex::new(ticket),
            path_cache: HashMap::new(),
            cache_dir,
        }
    }

    fn ticket(&self) -> Result<String, XecmError> {
        self.ticket.lock().ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| XecmError::Auth("no ticket configured".to_string()))
    }

    async fn re_authenticate(&self) -> Result<(), XecmError> {
        let password = self.config.password.clone();
        if password.is_empty() {
            return Err(XecmError::Auth("no password stored, cannot re-authenticate".to_string()));
        }
        let new_ticket = Self::authenticate(
            &self.config.base_url,
            &self.config.username,
            &password,
        ).await?;
        *self.ticket.lock().map_err(|_| XecmError::Auth("lock poisoned".to_string()))? = Some(new_ticket);
        eprintln!("[xecm] auto-re-authenticated: new ticket obtained");
        Ok(())
    }

    pub async fn authenticate(
        base_url: &str,
        username: &str,
        password: &str,
    ) -> Result<String, XecmError> {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base_url}/auth"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "username={}&password={}",
                urlencoding(username),
                urlencoding(password)
            ))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(XecmError::Auth(format!(
                "authentication failed with status {}",
                resp.status()
            )));
        }

        let auth: XecmAuthResponse = resp.json().await?;
        Ok(auth.ticket)
    }

    pub async fn list_workspaces(
        base_url: &str,
        ticket: &str,
    ) -> Result<Vec<XecmNode>, XecmError> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base_url}/volumes"))
            .header("OTCSTicket", ticket)
            .send()
            .await?;
        Self::check_status(&resp)?;
        let volumes: XecmVolumesResponse = resp.json().await?;
        Ok(volumes.data)
    }

    pub async fn resolve_workspace(&self) -> Result<XecmNode, XecmError> {
        let ticket = self.ticket()?;
        let workspaces = Self::list_workspaces(&self.config.base_url, &ticket).await?;
        workspaces
            .into_iter()
            .find(|w| w.name == self.config.workspace_name)
            .ok_or_else(|| {
                XecmError::NotFound(format!(
                    "workspace '{}' not found",
                    self.config.workspace_name
                ))
            })
    }

    pub async fn get_node(&self, node_id: u64) -> Result<XecmNode, XecmError> {
        let mut retried = false;
        loop {
            let ticket = self.ticket()?;
            let resp = self
                .http
                .get(format!("{}/nodes/{node_id}", self.config.base_url))
                .header("OTCSTicket", &ticket)
                .send()
                .await?;
            if resp.status().as_u16() == 401 && !retried {
                self.re_authenticate().await?;
                retried = true;
                continue;
            }
            Self::check_status(&resp)?;
            let node_resp: XecmNodeResponse = resp.json().await?;
            return Ok(node_resp.data);
        }
    }

    pub async fn list_directory(
        &self,
        node_id: u64,
        page: u32,
    ) -> Result<(Vec<XecmNode>, u32), XecmError> {
        let mut retried = false;
        loop {
            let ticket = self.ticket()?;
            let resp = self
                .http
                .get(format!(
                    "{}/nodes/{node_id}/nodes?limit=100&page={page}",
                    self.config.base_url
                ))
                .header("OTCSTicket", &ticket)
                .send()
                .await?;
            if resp.status().as_u16() == 401 && !retried {
                self.re_authenticate().await?;
                retried = true;
                continue;
            }
            Self::check_status(&resp)?;
            let list: XecmNodeListResponse = resp.json().await?;
            return Ok((list.data, list.page_total.unwrap_or(1)));
        }
    }

    pub async fn list_all_children(&self, node_id: u64) -> Result<Vec<XecmNode>, XecmError> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let (mut nodes, total) = self.list_directory(node_id, page).await?;
            all.append(&mut nodes);
            if page >= total {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    pub async fn get_content(&self, node_id: u64) -> Result<Vec<u8>, XecmError> {
        let node = self.get_node(node_id).await?;
        let cache_key = cache_key_for(&node);
        let cache_path = self.cache_dir.join(&cache_key);

        if cache_path.exists() {
            if let Ok(cached) = std::fs::read(&cache_path) {
                return Ok(cached);
            }
        }

        let mut retried = false;
        loop {
            let ticket = self.ticket()?;
            let resp = self
                .http
                .get(format!("{base_url}/nodes/{node_id}/content", base_url = self.config.base_url))
                .header("OTCSTicket", &ticket)
                .send()
                .await?;
            if resp.status().as_u16() == 401 && !retried {
                self.re_authenticate().await?;
                retried = true;
                continue;
            }
            Self::check_status(&resp)?;
            let bytes = resp.bytes().await?.to_vec();

            let _ = std::fs::create_dir_all(&self.cache_dir);
            let _ = std::fs::write(&cache_path, &bytes);

            return Ok(bytes);
        }
    }

    pub async fn resolve_path(&mut self, path: &str) -> Result<u64, XecmError> {
        if let Some(&id) = self.path_cache.get(path) {
            return Ok(id);
        }

        let normalized = path.replace('\\', "/");
        let prefix = "raw/sources";
        let relative = match extract_relative(&normalized, prefix) {
            Some(r) => r,
            None => {
                return Err(XecmError::Other(format!(
                    "path '{}' is not under raw/sources/",
                    path
                )));
            }
        };

        let mut current_id = self.config.workspace_node_id;

        if relative.is_empty() {
            self.path_cache.insert(path.to_string(), current_id);
            return Ok(current_id);
        }

        let segments: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
        for segment in segments {
            let children = self.list_all_children(current_id).await?;
            let child = children
                .iter()
                .find(|n| n.name == segment)
                .ok_or_else(|| XecmError::NotFound(format!(
                    "'{}' not found in parent node {} (path: {})",
                    segment, current_id, path
                )))?;
            current_id = child.id;
        }

        self.path_cache.insert(path.to_string(), current_id);
        Ok(current_id)
    }

    pub async fn recursive_snapshot(&self) -> Result<HashMap<u64, XecmNode>, XecmError> {
        let mut snapshot = HashMap::new();

        // Use an explicit stack instead of recursion to avoid async recursion issues
        let mut stack: Vec<u64> = vec![self.config.workspace_node_id];
        while let Some(node_id) = stack.pop() {
            let children = self.list_all_children(node_id).await?;
            for child in children {
                let id = child.id;
                if child.container {
                    stack.push(id);
                }
                snapshot.insert(id, child);
            }
        }
        Ok(snapshot)
    }

    // -- Helpers --

    fn check_status(resp: &reqwest::Response) -> Result<(), XecmError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(match status.as_u16() {
            401 => XecmError::Auth("session expired".to_string()),
            404 => XecmError::NotFound("node not found".to_string()),
            429 => XecmError::RateLimited,
            other => XecmError::Other(format!("HTTP {other}")),
        })
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

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

fn cache_key_for(node: &XecmNode) -> String {
    use md5::{Digest, Md5};
    let date = node.modify_date.as_deref().unwrap_or("unknown");
    let hash_input = format!("{}-{}", node.id, date);
    let digest = Md5::digest(hash_input.as_bytes());
    format!("{:x}", digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BASE_URL: &str = "http://192.168.0.29/otcs/cs.exe/api/v1";
    const TEST_USERNAME: &str = "admin";
    const TEST_PASSWORD: &str = "OpenText1";
    const TEST_WORKSPACE: &str = "Enterprise";

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    fn test_config() -> XecmConfig {
        let ticket = rt()
            .block_on(XecmClient::authenticate(
                TEST_BASE_URL,
                TEST_USERNAME,
                TEST_PASSWORD,
            ))
            .expect("authenticate");

        XecmConfig {
            enabled: true,
            base_url: TEST_BASE_URL.to_string(),
            workspace_node_id: 2000,
            workspace_name: TEST_WORKSPACE.to_string(),
            username: TEST_USERNAME.to_string(),
            password: TEST_PASSWORD.to_string(),
            ticket: Some(ticket),
            poll_interval_seconds: 30,
        }
    }

    fn test_client() -> XecmClient {
        let config = test_config();
        XecmClient::new(config, std::env::temp_dir().join("xecm-test-cache"))
    }

    #[test]
    fn authenticate_returns_ticket() {
        let ticket = rt()
            .block_on(XecmClient::authenticate(TEST_BASE_URL, TEST_USERNAME, TEST_PASSWORD));
        assert!(ticket.is_ok(), "auth failed: {:?}", ticket.err());
        assert!(!ticket.unwrap().is_empty());
    }

    #[test]
    fn list_workspaces_finds_enterprise() {
        let config = test_config();
        let workspaces = rt()
            .block_on(XecmClient::list_workspaces(TEST_BASE_URL, config.ticket.as_deref().unwrap()))
            .expect("list workspaces");
        assert!(!workspaces.is_empty(), "no workspaces returned");
        let enterprise = workspaces.iter().find(|w| w.name == TEST_WORKSPACE);
        assert!(enterprise.is_some(), "Enterprise workspace not found");
        assert_eq!(enterprise.unwrap().id, 2000);
    }

    #[test]
    fn get_node_returns_workspace_root() {
        let client = test_client();
        let node = rt().block_on(client.get_node(2000)).expect("get_node");
        assert_eq!(node.id, 2000);
        assert_eq!(node.name, "Enterprise");
        assert!(node.container, "workspace root should be a container");
    }

    #[test]
    fn list_directory_returns_items() {
        let client = test_client();
        let (items, total) = rt()
            .block_on(client.list_directory(2000, 1))
            .expect("list_directory");
        assert!(!items.is_empty(), "workspace should have items");
        assert!(total >= 1);
        for item in &items {
            assert!(!item.name.is_empty(), "every item should have a name");
            assert!(item.id > 0, "every item should have a valid id");
        }
    }

    #[test]
    fn list_all_children_gets_all_pages() {
        let client = test_client();
        let children = rt()
            .block_on(client.list_all_children(2000))
            .expect("list_all_children");
        assert!(!children.is_empty());
    }

    #[test]
    fn get_content_downloads_file_bytes() {
        let client = test_client();
        let children = rt()
            .block_on(client.list_all_children(2000))
            .expect("list_all_children");
        let file = children
            .iter()
            .find(|n| !n.container && n.size > 0)
            .expect("need at least one non-empty file in workspace");
        let bytes = rt()
            .block_on(client.get_content(file.id))
            .expect("get_content");
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() as u64, file.size);
    }

    #[test]
    fn resolve_path_finds_root() {
        let mut client = test_client();
        let id = rt()
            .block_on(client.resolve_path("raw/sources"))
            .expect("resolve_path");
        assert_eq!(id, 2000);
    }

    #[test]
    fn resolve_path_caches_repeated_lookups() {
        let mut client = test_client();
        let id1 = rt()
            .block_on(client.resolve_path("raw/sources"))
            .expect("resolve_path");
        let id2 = rt()
            .block_on(client.resolve_path("raw/sources"))
            .expect("resolve_path");
        assert_eq!(id1, id2);
        assert!(client.path_cache.contains_key("raw/sources"));
    }

    #[test]
    fn recursive_snapshot_collects_all_nodes() {
        let client = test_client();
        let snapshot = rt()
            .block_on(client.recursive_snapshot())
            .expect("recursive_snapshot");
        eprintln!(
            "recursive_snapshot collected {} nodes under Enterprise workspace",
            snapshot.len()
        );
        for (id, node) in &snapshot {
            assert_eq!(*id, node.id);
            assert!(!node.name.is_empty());
        }
    }

    #[test]
    fn urlencoding_handles_special_characters() {
        assert_eq!(urlencoding("admin"), "admin");
        assert_eq!(urlencoding("user@name"), "user%40name");
        assert_eq!(urlencoding("a b"), "a%20b");
        assert_eq!(urlencoding("a+b"), "a%2Bb");
    }

    #[test]
    fn cache_key_is_deterministic() {
        let node = XecmNode {
            id: 123,
            name: "test.pdf".into(),
            type_: 144,
            container: false,
            size: 100,
            modify_date: Some("2026-01-01T00:00:00".into()),
            mime_type: None,
            parent_id: 0,
        };
        let k1 = cache_key_for(&node);
        let k2 = cache_key_for(&node);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
    }

    #[test]
    fn cache_key_differs_on_modify_date_change() {
        let mut node = XecmNode {
            id: 123,
            name: "test.pdf".into(),
            type_: 144,
            container: false,
            size: 100,
            modify_date: Some("2026-01-01T00:00:00".into()),
            mime_type: None,
            parent_id: 0,
        };
        let k1 = cache_key_for(&node);
        node.modify_date = Some("2026-06-01T00:00:00".into());
        let k2 = cache_key_for(&node);
        assert_ne!(k1, k2);
    }

    #[test]
    fn is_source_path_recognizes_valid_paths() {
        let config = test_config();
        let client = XecmClient::new(config, std::env::temp_dir().join("xecm-test-cache"));
        assert!(client.is_source_path("raw/sources"));
        assert!(client.is_source_path("raw/sources/doc.pdf"));
        assert!(client.is_source_path("raw/sources/folder/file.txt"));
        assert!(client.is_source_path("raw\\sources\\doc.pdf"));
        // Absolute paths (what the frontend actually sends)
        assert!(client.is_source_path("C:/Users/test/project/raw/sources"));
        assert!(client.is_source_path("C:/Users/test/project/raw/sources/doc.pdf"));
        assert!(client.is_source_path("C:/Users/test/project/raw/sources/folder/file.txt"));
        assert!(!client.is_source_path("wiki/index.md"));
        assert!(!client.is_source_path("purpose.md"));
        assert!(!client.is_source_path(""));
    }

    #[test]
    fn xecm_error_display_messages() {
        assert!(format!("{}", XecmError::Auth("bad".into())).contains("auth"));
        assert!(format!("{}", XecmError::Network("timeout".into())).contains("network"));
        assert!(format!("{}", XecmError::NotFound("missing".into())).contains("not found"));
        assert!(format!("{}", XecmError::RateLimited).contains("rate limited"));
        assert!(format!("{}", XecmError::Other("oops".into())).contains("oops"));
    }

    #[test]
    fn xecm_config_serde_roundtrips() {
        let config = XecmConfig {
            enabled: true,
            base_url: "http://example.com/api/v1".into(),
            workspace_node_id: 42,
            workspace_name: "TestWS".into(),
            username: "user1".into(),
            password: "".into(),
            ticket: Some("ticket123".into()),
            poll_interval_seconds: 60,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: XecmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.enabled, config.enabled);
        assert_eq!(parsed.base_url, config.base_url);
        assert_eq!(parsed.workspace_node_id, config.workspace_node_id);
        assert_eq!(parsed.workspace_name, config.workspace_name);
        assert_eq!(parsed.ticket, config.ticket);
        assert_eq!(parsed.poll_interval_seconds, config.poll_interval_seconds);

    }

    #[test]
    fn xecm_config_camelcase_json() {
        let json = r#"{
            "enabled": true,
            "baseUrl": "http://example.com/api/v1",
            "workspaceNodeId": 42,
            "workspaceName": "TestWS",
            "username": "user1",
            "ticket": "ticket123",
            "pollIntervalSecs": 60
        }"#;
        let config: XecmConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.base_url, "http://example.com/api/v1");
        assert_eq!(config.workspace_node_id, 42);
        assert_eq!(config.workspace_name, "TestWS");
        assert_eq!(config.poll_interval_seconds, 60);
    }

}
