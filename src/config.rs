use crate::balancer::LoadBalancingMode;
use anyhow::Context;
use std::env;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub cert_pem_base64: String,
    pub key_pem_base64: String,
    pub port: u16,
    pub enable_h2: bool,
    pub enable_h3: bool,
    pub enable_websocket: bool,
    pub initial_mode: LoadBalancingMode,
}

impl ProxyConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let cert_pem_base64 = env::var("TLS_CERT_BASE64")
            .context("TLS_CERT_BASE64 must contain the base64-encoded PEM certificate")?;
        let key_pem_base64 = env::var("TLS_KEY_BASE64")
            .context("TLS_KEY_BASE64 must contain the base64-encoded PEM private key")?;

        let port = env::var("PROXY_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(443);

        let enable_h2 = env_bool("ENABLE_H2", true);
        let enable_h3 = env_bool("ENABLE_H3", true);
        let enable_websocket = env_bool("ENABLE_WEBSOCKET", true);

        let initial_mode = match env::var("LB_MODE") {
            Ok(value) => value
                .parse::<LoadBalancingMode>()
                .map_err(|err| anyhow::anyhow!(err))?,
            Err(_) => LoadBalancingMode::LeastConn,
        };

        Ok(Self {
            cert_pem_base64,
            key_pem_base64,
            port,
            enable_h2,
            enable_h3,
            enable_websocket,
            initial_mode,
        })
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => default,
    }
}
