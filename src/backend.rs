use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendScheme {
    Http,
    Https,
    Ws,
    Wss,
}

impl BackendScheme {
    pub fn from_url(url: &Url) -> Option<Self> {
        match url.scheme() {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "ws" => Some(Self::Ws),
            "wss" => Some(Self::Wss),
            _ => None,
        }
    }

    pub fn is_http(self) -> bool {
        matches!(self, Self::Http | Self::Https)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Backend {
    pub id: String,
    pub url: Url,
    pub scheme: BackendScheme,
    pub max_connections: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendView {
    pub id: String,
    pub url: Url,
    pub scheme: BackendScheme,
    pub max_connections: Option<usize>,
    pub active_connections: usize,
}
