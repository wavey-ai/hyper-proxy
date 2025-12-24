use crate::backend::{Backend, BackendScheme, BackendView};
use crate::balancer::LoadBalancingMode;
use crate::pool::{BackendLease, BackendPool};
use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;
use url::Url;

pub type ProxyHttpClient =
    Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

pub struct ProxyState {
    http_pool: Arc<BackendPool>,
    ws_pool: Arc<BackendPool>,
    mode: RwLock<LoadBalancingMode>,
    client: ProxyHttpClient,
}

impl ProxyState {
    pub fn new(mode: LoadBalancingMode) -> anyhow::Result<Self> {
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Ok(Self::new_with_client(mode, client))
    }

    pub fn new_with_client(mode: LoadBalancingMode, client: ProxyHttpClient) -> Self {
        Self {
            http_pool: Arc::new(BackendPool::new()),
            ws_pool: Arc::new(BackendPool::new()),
            mode: RwLock::new(mode),
            client,
        }
    }

    pub fn client(&self) -> &ProxyHttpClient {
        &self.client
    }

    pub async fn mode(&self) -> LoadBalancingMode {
        *self.mode.read().await
    }

    pub async fn set_mode(&self, mode: LoadBalancingMode) {
        *self.mode.write().await = mode;
        debug!(mode = %mode.as_str(), "load balancer mode updated");
    }

    pub async fn add_backend(
        &self,
        url: Url,
        max_connections: Option<usize>,
    ) -> Result<BackendView, String> {
        let scheme = BackendScheme::from_url(&url)
            .ok_or_else(|| format!("unsupported backend scheme: {}", url.scheme()))?;
        if let Some(0) = max_connections {
            return Err("max_connections must be > 0".to_string());
        }
        let backend = Backend {
            id: uuid::Uuid::new_v4().to_string(),
            url,
            scheme,
            max_connections,
        };

        let view = if scheme.is_http() {
            self.http_pool.add_backend(backend).await
        } else {
            self.ws_pool.add_backend(backend).await
        };
        Ok(view)
    }

    pub async fn remove_backend(&self, id: &str) -> Option<BackendView> {
        if let Some(view) = self.http_pool.remove_backend(id).await {
            return Some(view);
        }
        self.ws_pool.remove_backend(id).await
    }

    pub async fn list_backends(&self) -> (Vec<BackendView>, Vec<BackendView>) {
        let http = self.http_pool.list_backends().await;
        let ws = self.ws_pool.list_backends().await;
        (http, ws)
    }

    pub async fn acquire_http(&self) -> Option<BackendLease> {
        let mode = self.mode().await;
        self.http_pool.acquire(mode).await
    }

    pub async fn acquire_ws(&self) -> Option<BackendLease> {
        let mode = self.mode().await;
        self.ws_pool.acquire(mode).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::LoadBalancingMode;
    use std::sync::Once;
    use url::Url;

    fn init_crypto() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn add_backend_rejects_invalid_scheme() {
        init_crypto();
        let state = ProxyState::new(LoadBalancingMode::LeastConn).unwrap();
        let url = Url::parse("ftp://example.com").unwrap();
        let err = state.add_backend(url, None).await.unwrap_err();
        assert!(err.contains("unsupported backend scheme"));
    }

    #[tokio::test]
    async fn add_backend_rejects_zero_max_connections() {
        init_crypto();
        let state = ProxyState::new(LoadBalancingMode::LeastConn).unwrap();
        let url = Url::parse("https://example.com").unwrap();
        let err = state.add_backend(url, Some(0)).await.unwrap_err();
        assert!(err.contains("max_connections must be > 0"));
    }

    #[tokio::test]
    async fn list_backends_separates_http_and_ws() {
        init_crypto();
        let state = ProxyState::new(LoadBalancingMode::LeastConn).unwrap();
        let http_url = Url::parse("https://example.com").unwrap();
        let ws_url = Url::parse("wss://example.com").unwrap();

        state.add_backend(http_url, None).await.unwrap();
        state.add_backend(ws_url, None).await.unwrap();

        let (http, ws) = state.list_backends().await;
        assert_eq!(http.len(), 1);
        assert_eq!(ws.len(), 1);
        assert_eq!(http[0].scheme, BackendScheme::Https);
        assert_eq!(ws[0].scheme, BackendScheme::Wss);
    }
}
