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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl<'a> EnvGuard<'a> {
        fn new(keys: &[&'static str]) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let saved = keys.iter().map(|key| (*key, env::var(key).ok())).collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard<'_> {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn from_env_reads_config() {
        let _guard = EnvGuard::new(&[
            "TLS_CERT_BASE64",
            "TLS_KEY_BASE64",
            "PROXY_PORT",
            "ENABLE_H2",
            "ENABLE_H3",
            "ENABLE_WEBSOCKET",
            "LB_MODE",
        ]);

        env::set_var("TLS_CERT_BASE64", "cert");
        env::set_var("TLS_KEY_BASE64", "key");
        env::set_var("PROXY_PORT", "8443");
        env::set_var("ENABLE_H2", "false");
        env::set_var("ENABLE_H3", "0");
        env::set_var("ENABLE_WEBSOCKET", "no");
        env::set_var("LB_MODE", "queue");

        let config = ProxyConfig::from_env().unwrap();
        assert_eq!(config.cert_pem_base64, "cert");
        assert_eq!(config.key_pem_base64, "key");
        assert_eq!(config.port, 8443);
        assert!(!config.enable_h2);
        assert!(!config.enable_h3);
        assert!(!config.enable_websocket);
        assert_eq!(config.initial_mode, LoadBalancingMode::Queue);
    }
}
