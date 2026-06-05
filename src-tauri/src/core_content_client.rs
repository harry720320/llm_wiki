use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A single node returned by the Core Content REST API.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreContentNode {
    pub id: String,
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
    pub base_url: String,
    pub folder_node_id: String,
    pub folder_name: String,
    pub username: String,
    pub password: String,
    pub csrf_token: String,
    pub cookies_json: String,
    pub poll_interval_seconds: u64,
}

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
        let cookie_jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
        if !config.cookies_json.is_empty() {
            if let Ok(cookies) = serde_json::from_str::<HashMap<String, String>>(&config.cookies_json) {
                let domain_url = &config.base_url;
                for (name, value) in &cookies {
                    cookie_jar.add_cookie_str(
                        &format!("{name}={value}"),
                        &domain_url.parse().unwrap(),
                    );
                }
            }
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .cookie_provider(cookie_jar)
            .build()
            .unwrap_or_default();

        Self {
            http,
            csrf_token: Mutex::new(config.csrf_token.clone()),
            config,
            cache_dir,
        }
    }

    fn cc_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        let csrf = self.csrf_token.lock().ok().map(|s| s.clone()).unwrap_or_default();
        headers.insert(
            "X-CCM-XSRF-TOKEN",
            reqwest::header::HeaderValue::from_str(&csrf).unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
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
            reqwest::header::HeaderValue::from_str(&self.config.base_url).unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
        headers
    }

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

    pub async fn resolve_path(&self, path: &str) -> Result<String, CoreContentError> {
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
