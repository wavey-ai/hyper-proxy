mod api;
mod backend;
mod balancer;
mod config;
mod context;
mod pool;
mod proxy;
mod router;
mod state;

use crate::config::ProxyConfig;
use crate::router::ProxyRouter;
use crate::state::ProxyState;
use std::sync::Arc;
use tracing_subscriber::{fmt, EnvFilter};
use tracing_subscriber::prelude::*;
use web_service::{H2H3Server, Server, ServerBuilder};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let _ = dotenvy::dotenv();

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

    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set global default subscriber");

    let config = ProxyConfig::from_env()?;
    let state = Arc::new(ProxyState::new(config.initial_mode)?);
    let router = ProxyRouter::new(state);

    let server = H2H3Server::builder()
        .with_tls(config.cert_pem_base64, config.key_pem_base64)
        .with_port(config.port)
        .enable_h2(config.enable_h2)
        .enable_h3(config.enable_h3)
        .enable_websocket(config.enable_websocket)
        .with_router(Box::new(router))
        .build()?;

    let handle = server.start().await?;
    let _ = handle.ready_rx.await;
    tracing::info!("hyper-proxy ready on port {}", config.port);

    tokio::signal::ctrl_c().await?;
    let _ = handle.shutdown_tx.send(());
    let _ = handle.finished_rx.await;

    Ok(())
}
