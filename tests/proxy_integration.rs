use base64::{engine::general_purpose, Engine as _};
use bytes::Bytes;
use futures_util::StreamExt;
use hyper_proxy::{LoadBalancingMode, ProxyRouter, ProxyState};
use http::{Method, Request};
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing_subscriber::{fmt, EnvFilter};
use tracing_subscriber::prelude::*;
use web_service::{
    BodyStream, HandlerResponse, HandlerResult, H2H3Server, Router, Server, ServerBuilder,
    ServerError, StreamWriter, WebSocketHandler, WebTransportHandler,
};

struct TestCerts {
    cert_der: Vec<u8>,
    cert_pem: String,
    cert_base64: String,
    key_base64: String,
}

struct RunningServer {
    addr: SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<()>,
    finished_rx: tokio::sync::oneshot::Receiver<()>,
}

impl RunningServer {
    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.finished_rx.await;
    }
}

type TestClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

struct TestBackend {
    id: String,
    default_delay: Duration,
    notify: Option<Arc<Notify>>,
}

impl TestBackend {
    fn notify_arrival(&self) {
        if let Some(notify) = &self.notify {
            notify.notify_waiters();
        }
    }
    fn delay_for_path(&self, path: &str) -> Duration {
        let mut parts = path.split('/').filter(|part| !part.is_empty());
        while let Some(part) = parts.next() {
            if part == "sleep" {
                if let Some(value) = parts.next() {
                    if let Ok(ms) = value.parse::<u64>() {
                        return Duration::from_millis(ms);
                    }
                }
            }
        }
        self.default_delay
    }

    fn response_body(&self, suffix: &str) -> HandlerResponse {
        HandlerResponse {
            status: http::StatusCode::OK,
            body: Some(Bytes::from(format!("{}{}", self.id, suffix))),
            content_type: Some("text/plain".to_string()),
            headers: vec![],
            etag: None,
        }
    }
}

#[async_trait::async_trait]
impl Router for TestBackend {
    async fn route(&self, req: http::Request<()>) -> HandlerResult<HandlerResponse> {
        self.notify_arrival();
        let delay = self.delay_for_path(req.uri().path());
        if delay.as_millis() > 0 {
            tokio::time::sleep(delay).await;
        }
        Ok(self.response_body(""))
    }

    async fn route_body(
        &self,
        req: http::Request<()>,
        body: BodyStream,
    ) -> HandlerResult<HandlerResponse> {
        self.notify_arrival();
        let bytes = collect_body(body).await?;
        let delay = self.delay_for_path(req.uri().path());
        if delay.as_millis() > 0 {
            tokio::time::sleep(delay).await;
        }
        Ok(self.response_body(&format!(":{}", bytes.len())))
    }

    fn has_body_handler(&self, path: &str) -> bool {
        path.starts_with("/echo")
    }

    fn is_streaming(&self, _path: &str) -> bool {
        false
    }

    async fn route_stream(
        &self,
        _req: http::Request<()>,
        _stream_writer: Box<dyn StreamWriter>,
    ) -> HandlerResult<()> {
        Err(ServerError::Config("streaming not supported".into()))
    }

    fn webtransport_handler(&self) -> Option<&dyn WebTransportHandler> {
        None
    }

    fn websocket_handler(&self, _path: &str) -> Option<&dyn WebSocketHandler> {
        None
    }
}

fn init_crypto() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _ = tracing_log::LogTracer::init();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::from_default_env())
            .with(
                fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE),
            );
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn generate_test_certs() -> anyhow::Result<TestCerts> {
    let rcgen::CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = cert.der().as_ref().to_vec();
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    let cert_base64 = general_purpose::STANDARD.encode(cert_pem.as_bytes());
    let key_base64 = general_purpose::STANDARD.encode(key_pem.as_bytes());

    Ok(TestCerts {
        cert_der,
        cert_pem,
        cert_base64,
        key_base64,
    })
}

fn pick_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("allocate test port")
}

fn build_test_client(certs: &TestCerts) -> anyhow::Result<TestClient> {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(certs.cert_der.clone()))?;
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

fn build_request(
    method: Method,
    url: &str,
    body: Bytes,
) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(Request::builder()
        .method(method)
        .uri(url)
        .body(Full::new(body))?)
}

async fn send_request(
    client: &TestClient,
    request: Request<Full<Bytes>>,
) -> anyhow::Result<(http::StatusCode, Bytes)> {
    let response = client.request(request).await?;
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();
    Ok((status, body))
}

async fn request_text(
    client: &TestClient,
    request: Request<Full<Bytes>>,
) -> anyhow::Result<String> {
    let (status, body) = send_request(client, request).await?;
    if !status.is_success() {
        let body_text = String::from_utf8_lossy(&body);
        anyhow::bail!("unexpected status {status}: {body_text}");
    }
    Ok(String::from_utf8(body.to_vec())?)
}

async fn start_backend(
    id: &str,
    delay_ms: u64,
    certs: &TestCerts,
    notify: Option<Arc<Notify>>,
) -> anyhow::Result<RunningServer> {
    let port = pick_port();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let router = TestBackend {
        id: id.to_string(),
        default_delay: Duration::from_millis(delay_ms),
        notify,
    };

    let server = H2H3Server::builder()
        .with_tls(certs.cert_base64.clone(), certs.key_base64.clone())
        .with_port(port)
        .enable_h2(true)
        .enable_h3(false)
        .enable_websocket(false)
        .with_router(Box::new(router))
        .build()?;

    let handle = server.start().await?;
    let web_service::ServerHandle {
        shutdown_tx,
        ready_rx,
        finished_rx,
    } = handle;
    let _ = ready_rx.await;

    Ok(RunningServer {
        addr,
        shutdown_tx,
        finished_rx,
    })
}

async fn start_proxy(
    mode: LoadBalancingMode,
    certs: &TestCerts,
    client: TestClient,
) -> anyhow::Result<RunningServer> {
    let port = pick_port();
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let state = std::sync::Arc::new(ProxyState::new_with_client(mode, client));
    let router = ProxyRouter::new(state);

    let server = H2H3Server::builder()
        .with_tls(certs.cert_base64.clone(), certs.key_base64.clone())
        .with_port(port)
        .enable_h2(true)
        .enable_h3(false)
        .enable_websocket(false)
        .with_router(Box::new(router))
        .build()?;

    let handle = server.start().await?;
    let web_service::ServerHandle {
        shutdown_tx,
        ready_rx,
        finished_rx,
    } = handle;
    let _ = ready_rx.await;

    Ok(RunningServer {
        addr,
        shutdown_tx,
        finished_rx,
    })
}

async fn add_backend(
    client: &TestClient,
    proxy_addr: SocketAddr,
    backend_url: &str,
    max_connections: Option<usize>,
) -> anyhow::Result<hyper_proxy::BackendView> {
    let url = format!("https://localhost:{}/api/backends", proxy_addr.port());
    let payload = serde_json::json!({
        "url": backend_url,
        "max_connections": max_connections,
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri(url)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(serde_json::to_vec(&payload)?)))?;
    let (status, body) = send_request(client, request).await?;
    if !status.is_success() {
        let body_text = String::from_utf8_lossy(&body);
        anyhow::bail!("unexpected status {status}: {body_text}");
    }
    let backend = serde_json::from_slice::<hyper_proxy::BackendView>(&body)?;
    Ok(backend)
}

async fn get_text(
    client: &TestClient,
    proxy_addr: SocketAddr,
    path: &str,
) -> anyhow::Result<String> {
    let url = format!("https://localhost:{}{}", proxy_addr.port(), path);
    let request = build_request(Method::GET, &url, Bytes::new())?;
    request_text(client, request).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_backends_iteratively_and_route_queue() -> anyhow::Result<()> {
    init_crypto();
    let certs = generate_test_certs()?;

    let backend_a = start_backend("backend-a", 0, &certs, None).await?;
    let backend_b = start_backend("backend-b", 0, &certs, None).await?;

    let proxy_client = build_test_client(&certs)?;
    let proxy = start_proxy(LoadBalancingMode::Queue, &certs, proxy_client).await?;

    let api_client = build_test_client(&certs)?;

    let backend_a_url = format!("https://localhost:{}", backend_a.addr.port());
    add_backend(&api_client, proxy.addr, &backend_a_url, None).await?;

    let first = get_text(&api_client, proxy.addr, "/").await?;
    assert_eq!(first, "backend-a");

    let backend_b_url = format!("https://localhost:{}", backend_b.addr.port());
    add_backend(&api_client, proxy.addr, &backend_b_url, None).await?;

    let second = get_text(&api_client, proxy.addr, "/").await?;
    let third = get_text(&api_client, proxy.addr, "/").await?;
    assert_eq!(second, "backend-a");
    assert_eq!(third, "backend-b");

    proxy.shutdown().await;
    backend_a.shutdown().await;
    backend_b.shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leastconn_prefers_idle_backend() -> anyhow::Result<()> {
    init_crypto();
    let certs = generate_test_certs()?;

    let slow_notify = Arc::new(Notify::new());
    let backend_slow = start_backend("backend-slow", 300, &certs, Some(Arc::clone(&slow_notify))).await?;
    let backend_fast = start_backend("backend-fast", 0, &certs, None).await?;

    let proxy_client = build_test_client(&certs)?;
    let proxy = start_proxy(LoadBalancingMode::LeastConn, &certs, proxy_client).await?;

    let api_client = build_test_client(&certs)?;

    let slow_url = format!("https://localhost:{}", backend_slow.addr.port());
    let fast_url = format!("https://localhost:{}", backend_fast.addr.port());

    add_backend(&api_client, proxy.addr, &slow_url, None).await?;
    add_backend(&api_client, proxy.addr, &fast_url, None).await?;

    let client = build_test_client(&certs)?;

    let url = format!("https://localhost:{}/", proxy.addr.port());

    let slow_request = {
        let client = client.clone();
        let url = url.clone();
        tokio::spawn(async move {
            let request = build_request(Method::GET, &url, Bytes::new())?;
            let response = request_text(&client, request).await?;
            Ok::<_, anyhow::Error>(response)
        })
    };

    slow_notify.notified().await;

    let fast_text = request_text(&client, build_request(Method::GET, &url, Bytes::new())?).await?;
    let third_text = request_text(&client, build_request(Method::GET, &url, Bytes::new())?).await?;
    let slow_text = slow_request.await??;

    assert_eq!(fast_text, "backend-fast");
    assert_eq!(third_text, "backend-fast");
    assert_eq!(slow_text, "backend-slow");

    proxy.shutdown().await;
    backend_slow.shutdown().await;
    backend_fast.shutdown().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_waits_for_maxconn_and_preserves_body() -> anyhow::Result<()> {
    init_crypto();
    let certs = generate_test_certs()?;

    let backend = start_backend("backend-queue", 200, &certs, None).await?;

    let proxy_client = build_test_client(&certs)?;
    let proxy = start_proxy(LoadBalancingMode::Queue, &certs, proxy_client).await?;

    let api_client = build_test_client(&certs)?;

    let backend_url = format!("https://localhost:{}", backend.addr.port());
    add_backend(&api_client, proxy.addr, &backend_url, Some(1)).await?;

    let client = build_test_client(&certs)?;

    let url = format!("https://localhost:{}/echo/sleep/200", proxy.addr.port());

    let body_one = Bytes::from(vec![b'a'; 1024]);
    let body_two = Bytes::from(vec![b'b'; 2048]);

    let first = async {
        let start = Instant::now();
        let request = build_request(Method::POST, &url, body_one.clone())?;
        let text = request_text(&client, request).await?;
        Ok::<_, anyhow::Error>((start.elapsed(), text))
    };

    let second = async {
        let start = Instant::now();
        let request = build_request(Method::POST, &url, body_two.clone())?;
        let text = request_text(&client, request).await?;
        Ok::<_, anyhow::Error>((start.elapsed(), text))
    };

    let (first, second) = tokio::try_join!(first, second)?;

    let mut durations = vec![first.0, second.0];
    durations.sort();
    assert!(durations[1] >= durations[0] + Duration::from_millis(150));

    let mut lengths = vec![first.1, second.1];
    lengths.sort();
    assert_eq!(lengths, vec!["backend-queue:1024", "backend-queue:2048"]);

    proxy.shutdown().await;
    backend.shutdown().await;

    Ok(())
}

async fn collect_body(mut body: BodyStream) -> Result<Bytes, ServerError> {
    let mut data = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        data.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(data))
}
