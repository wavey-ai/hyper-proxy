use crate::balancer::LoadBalancingMode;
use crate::state::ProxyState;
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::debug;
use web_service::{HandlerResponse, HandlerResult};

#[derive(Debug, Deserialize)]
struct AddBackendRequest {
    url: url::Url,
    max_connections: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SetBalancerRequest {
    mode: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct BackendsResponse {
    mode: String,
    http: Vec<crate::backend::BackendView>,
    ws: Vec<crate::backend::BackendView>,
}

#[derive(Debug, Serialize)]
struct BalancerResponse {
    mode: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub async fn handle_api(
    state: &ProxyState,
    req: Request<()>,
    body: Bytes,
) -> HandlerResult<HandlerResponse> {
    let path = req.uri().path();
    let method = req.method();

    match (method, path) {
        (&Method::GET, "/api/health") => Ok(json_response(
            &HealthResponse { status: "ok" },
            StatusCode::OK,
        )),
        (&Method::GET, "/api/backends") => {
            let (http, ws) = state.list_backends().await;
            let mode = state.mode().await;
            Ok(json_response(
                &BackendsResponse {
                    mode: mode.as_str().to_string(),
                    http,
                    ws,
                },
                StatusCode::OK,
            ))
        }
        (&Method::POST, "/api/backends") => {
            let request: AddBackendRequest = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(err) => {
                    return Ok(json_error(
                        StatusCode::BAD_REQUEST,
                        format!("invalid json: {err}"),
                    ));
                }
            };
            debug!(
                backend_url = %request.url,
                max_connections = ?request.max_connections,
                "api add backend"
            );
            match state
                .add_backend(request.url, request.max_connections)
                .await
            {
                Ok(backend) => Ok(json_response(&backend, StatusCode::CREATED)),
                Err(err) => Ok(json_error(StatusCode::BAD_REQUEST, err)),
            }
        }
        (&Method::DELETE, path) if path.starts_with("/api/backends/") => {
            let id = path.trim_start_matches("/api/backends/");
            if id.is_empty() {
                return Ok(json_error(
                    StatusCode::BAD_REQUEST,
                    "backend id is required".to_string(),
                ));
            }
            debug!(backend_id = %id, "api remove backend");
            match state.remove_backend(id).await {
                Some(backend) => Ok(json_response(&backend, StatusCode::OK)),
                None => Ok(json_error(
                    StatusCode::NOT_FOUND,
                    "backend not found".to_string(),
                )),
            }
        }
        (&Method::GET, "/api/balancer") => {
            let mode = state.mode().await;
            Ok(json_response(
                &BalancerResponse {
                    mode: mode.as_str().to_string(),
                },
                StatusCode::OK,
            ))
        }
        (&Method::PUT, "/api/balancer") => {
            let request: SetBalancerRequest = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(err) => {
                    return Ok(json_error(
                        StatusCode::BAD_REQUEST,
                        format!("invalid json: {err}"),
                    ));
                }
            };
            let mode = match request.mode.parse::<LoadBalancingMode>() {
                Ok(mode) => mode,
                Err(err) => {
                    return Ok(json_error(StatusCode::BAD_REQUEST, err));
                }
            };
            debug!(mode = %mode.as_str(), "api set load balancer mode");
            state.set_mode(mode).await;
            Ok(json_response(
                &BalancerResponse {
                    mode: mode.as_str().to_string(),
                },
                StatusCode::OK,
            ))
        }
        _ => Ok(json_error(
            StatusCode::NOT_FOUND,
            "unknown api endpoint".to_string(),
        )),
    }
}

fn json_response<T: Serialize>(value: &T, status: StatusCode) -> HandlerResponse {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    HandlerResponse {
        status,
        body: Some(Bytes::from(body)),
        content_type: Some("application/json".to_string()),
        headers: vec![],
        etag: None,
    }
}

fn json_error(status: StatusCode, message: String) -> HandlerResponse {
    json_response(
        &ErrorResponse {
            error: message,
        },
        status,
    )
}
