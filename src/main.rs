//! The ansel server. Build with `--no-default-features --features server`.

#![forbid(unsafe_code)]

mod server;

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    // A missing .env file is normal in production.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("ansel=info")),
        )
        .init();

    let config = match server::Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let storage = match server::Storage::new(&config.storage) {
        Ok(storage) => storage,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let port = config.port;
    tracing::info!("storing images in {}", storage.describe());
    let app = server::router(server::AppState::new(config, storage));

    // `::` accepts IPv4 and IPv6, which Railway's private network needs.
    let listener = match tokio::net::TcpListener::bind(("::", port)).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("couldn't listen on port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("ansel listening on port {port}");
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
    {
        tracing::error!("server error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
