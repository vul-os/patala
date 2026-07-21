//! The real `patala-sidecar` process. See `patala-sidecar/README.md` for the
//! full threat model; this file only wires environment -> [`patala_sidecar::app`].

use patala_sidecar::{app, auth::SidecarToken, registry::default_registry};

const HOST: std::net::Ipv4Addr = std::net::Ipv4Addr::LOCALHOST;
const DEFAULT_PORT: u16 = 8420;
const PORT_ENV_VAR: &str = "PATALA_SIDECAR_PORT";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "patala_sidecar=info".into()),
        )
        .init();

    // Fail-closed: no token in the environment, no server. See
    // `auth::SidecarToken::from_env`.
    let token = match SidecarToken::from_env() {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("patala-sidecar: refusing to start: {msg}");
            std::process::exit(1);
        }
    };

    let port: u16 = std::env::var(PORT_ENV_VAR)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // Hardcoded to loopback, on purpose — see README.md "Threat model". This
    // is not an env-configurable knob: a sidecar that hands out signing
    // authority over a network interface would defeat the entire reason it
    // exists.
    let addr = std::net::SocketAddr::from((HOST, port));

    let router = app(default_registry(), token);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("patala-sidecar: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!("patala-sidecar listening on {addr} (loopback only)");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("patala-sidecar: server error");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("patala-sidecar: shutting down");
}
