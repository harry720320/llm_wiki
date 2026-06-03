use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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
    pub ticket: String,
    pub poll_interval_secs: u64,
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
    path_cache: HashMap<String, u64>,
    cache_dir: PathBuf,
}

impl XecmClient {
    pub fn new(config: XecmConfig, cache_dir: PathBuf) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            config,
            path_cache: HashMap::new(),
            cache_dir,
        }
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
        let workspaces = Self::list_workspaces(&self.config.base_url, &self.config.ticket).await?;
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
        let resp = self
            .http
            .get(format!("{}/nodes/{node_id}", self.config.base_url))
            .header("OTCSTicket", &self.config.ticket)
            .send()
            .await?;
        Self::check_status(&resp)?;
        let node_resp: XecmNodeResponse = resp.json().await?;
        Ok(node_resp.data)
    }

    pub async fn list_directory(
        &self,
        node_id: u64,
        page: u32,
    ) -> Result<(Vec<XecmNode>, u32), XecmError> {
        let resp = self
            .http
            .get(format!(
                "{}/nodes/{node_id}/nodes?limit=100&page={page}",
                self.config.base_url
            ))
            .header("OTCSTicket", &self.config.ticket)
            .send()
            .await?;
        Self::check_status(&resp)?;
        let list: XecmNodeListResponse = resp.json().await?;
        Ok((list.data, list.page_total.unwrap_or(1)))
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

        let resp = self
            .http
            .get(format!("{base_url}/nodes/{node_id}/content", base_url = self.config.base_url))
            .header("OTCSTicket", &self.config.ticket)
            .send()
            .await?;
        Self::check_status(&resp)?;
        let bytes = resp.bytes().await?.to_vec();

        let _ = std::fs::create_dir_all(&self.cache_dir);
        let _ = std::fs::write(&cache_path, &bytes);

        Ok(bytes)
    }

    pub async fn resolve_path(&mut self, path: &str) -> Result<u64, XecmError> {
        if let Some(&id) = self.path_cache.get(path) {
            return Ok(id);
        }

        let normalized = path.replace('\\', "/");
        let prefix = "raw/sources";
        let relative = if normalized == prefix {
            ""
        } else if let Some(rest) = normalized.strip_prefix(&format!("{prefix}/")) {
            rest
        } else {
            return Err(XecmError::Other(format!(
                "path '{}' is not under raw/sources/",
                path
            )));
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
        let pp = "raw/sources";
        normalized == pp || normalized.starts_with(&format!("{pp}/"))
    }
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
    let date = node.modify_date.as_deref().unwrap_or("unknown");
    let hash_input = format!("{}-{}", node.id, date);
    let digest = md5::compute(hash_input.as_bytes());
    format!("{:x}", digest)
}
