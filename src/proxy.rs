use crate::context;
use crate::state::ProxyState;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::header::HeaderName;
use http::{Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, field, debug_span, Instrument};
use web_service::{BodyStream, HandlerResponse, HandlerResult, ServerError};

pub async fn proxy_http(
    state: &ProxyState,
    req: Request<()>,
    body: Option<BodyStream>,
) -> HandlerResult<HandlerResponse> {
    debug!(
        method = %req.method(),
        uri = %req.uri(),
        "proxy http request"
    );
    let lease = match state.acquire_http().await {
        Some(lease) => lease,
        None => {
            debug!("proxy http: no backends available");
            return Ok(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "no http backends available",
            ))
        }
    };

    debug!(
        backend_url = %lease.backend().url,
        "proxy http selected backend"
    );
    let backend_url = match compose_backend_url(&lease.backend().url, req.uri()) {
        Ok(url) => url,
        Err(err) => {
            return Ok(text_response(
                StatusCode::BAD_GATEWAY,
                &format!("invalid backend url: {err}"),
            ))
        }
    };

    let backend_uri: Uri = match backend_url.as_str().parse() {
        Ok(uri) => uri,
        Err(err) => {
            return Ok(text_response(
                StatusCode::BAD_GATEWAY,
                &format!("invalid backend uri: {err}"),
            ))
        }
    };

    let mut builder = http::Request::builder()
        .method(req.method().clone())
        .uri(backend_uri.clone());

    for (name, value) in req.headers().iter() {
        if should_skip_request_header(name) {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }

    if let Some(request_id) = context::request_id(&req) {
        if !req.headers().contains_key(context::REQUEST_ID_HEADER) {
            builder = builder.header(context::REQUEST_ID_HEADER, request_id);
        }
    }

    if let Some(host) = req.headers().get(http::header::HOST) {
        builder = builder.header("x-forwarded-host", host.clone());
    }
    if !req.headers().contains_key("x-forwarded-proto") {
        builder = builder.header("x-forwarded-proto", "https");
    }
    if let Some(authority) = backend_uri.authority() {
        builder = builder.header(http::header::HOST, authority.as_str());
    }

    let body = match body {
        Some(body) => collect_body(body).await?,
        None => Bytes::new(),
    };
    let upstream_request = builder
        .body(Full::new(body))
        .map_err(|err| ServerError::Handler(Box::new(err)))?;

    let upstream_span = debug_span!(
        "upstream_request",
        backend = %backend_url,
        status = field::Empty
    );

    let response = match async { state.client().request(upstream_request).await }
        .instrument(upstream_span.clone())
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            return Ok(text_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {err}"),
            ))
        }
    };

    upstream_span.record("status", &field::display(response.status()));
    debug!(
        backend_status = %response.status(),
        "proxy http upstream response"
    );
    map_upstream_response(response).await
}

pub async fn proxy_websocket(
    state: &ProxyState,
    req: Request<()>,
    stream: WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>,
) -> HandlerResult<()> {
    debug!(
        method = %req.method(),
        uri = %req.uri(),
        "proxy websocket request"
    );
    let lease = match state.acquire_ws().await {
        Some(lease) => lease,
        None => {
            debug!("proxy websocket: no backends available");
            return Err(ServerError::Handler(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no websocket backends available",
            ))))
        }
    };

    debug!(
        backend_url = %lease.backend().url,
        "proxy websocket selected backend"
    );
    let backend_url = compose_backend_url(&lease.backend().url, req.uri())
        .map_err(|err| ServerError::Handler(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            err,
        ))))?;

    let mut ws_request = http::Request::builder()
        .method(http::Method::GET)
        .uri(backend_url.as_str());
    if let Some(request_id) = context::request_id(&req) {
        ws_request = ws_request.header(context::REQUEST_ID_HEADER, request_id);
    }
    let ws_request = ws_request
        .body(())
        .map_err(|err| ServerError::Handler(Box::new(err)))?;

    let connect_span = debug_span!("upstream_websocket_connect", backend = %backend_url);
    let (backend_ws, _) = tokio_tungstenite::connect_async(ws_request)
        .instrument(connect_span)
        .await
        .map_err(|err| ServerError::Handler(Box::new(err)))?;

    let (mut client_sink, mut client_stream) = stream.split();
    let (mut backend_sink, mut backend_stream) = backend_ws.split();

    let client_to_backend = async {
        while let Some(msg) = client_stream.next().await {
            let msg = msg.map_err(|err| ServerError::Handler(Box::new(err)))?;
            backend_sink
                .send(msg)
                .await
                .map_err(|err| ServerError::Handler(Box::new(err)))?;
        }
        Ok::<(), ServerError>(())
    };

    let backend_to_client = async {
        while let Some(msg) = backend_stream.next().await {
            let msg = msg.map_err(|err| ServerError::Handler(Box::new(err)))?;
            client_sink
                .send(msg)
                .await
                .map_err(|err| ServerError::Handler(Box::new(err)))?;
        }
        Ok::<(), ServerError>(())
    };

    tokio::try_join!(client_to_backend, backend_to_client).map(|_| ())
}

async fn map_upstream_response(
    response: http::Response<Incoming>,
) -> HandlerResult<HandlerResponse> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|err| ServerError::Handler(Box::new(err)))?;
    let body = body.to_bytes();

    let mut out_headers = Vec::new();
    let mut content_type = None;
    for (name, value) in headers.iter() {
        if should_skip_response_header(name) {
            continue;
        }
        if name == http::header::CONTENT_TYPE {
            if let Ok(value) = value.to_str() {
                content_type = Some(value.to_string());
            }
            continue;
        }
        if let Ok(value) = value.to_str() {
            out_headers.push((name.to_string(), value.to_string()));
        }
    }

    Ok(HandlerResponse {
        status,
        body: Some(body),
        content_type,
        headers: out_headers,
        etag: None,
    })
}

fn compose_backend_url(base: &url::Url, uri: &Uri) -> Result<url::Url, String> {
    let req_path = uri.path();
    let req_query = uri.query();

    let base_path = base.path().trim_end_matches('/');
    let req_path = req_path.trim_start_matches('/');

    let combined = if base_path.is_empty() {
        if req_path.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", req_path)
        }
    } else if req_path.is_empty() {
        base_path.to_string()
    } else {
        format!("{}/{}", base_path, req_path)
    };

    let mut url = base.clone();
    url.set_path(&combined);
    url.set_query(req_query);
    Ok(url)
}

fn should_skip_request_header(name: &HeaderName) -> bool {
    if is_hop_by_hop_header(name) {
        return true;
    }
    name == http::header::HOST
}

fn should_skip_response_header(name: &HeaderName) -> bool {
    is_hop_by_hop_header(name)
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
    )
}

fn text_response(status: StatusCode, message: &str) -> HandlerResponse {
    HandlerResponse {
        status,
        body: Some(Bytes::from(message.to_string())),
        content_type: Some("text/plain".to_string()),
        headers: vec![],
        etag: None,
    }
}

async fn collect_body(mut body: BodyStream) -> Result<Bytes, ServerError> {
    let mut data = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        data.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(data))
}
