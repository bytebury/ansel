//! The ansel HTTP server.

mod config;
mod docs;
mod error;
mod ids;
mod imaging;
mod routes;
mod storage;
mod video;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::routing::{get, post};
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;

pub use config::Config;
pub use storage::Storage;

/// Room for multipart framing on top of `ANSEL_MAX_UPLOAD_BYTES`. The exact
/// file size is checked after reading.
const MULTIPART_OVERHEAD: usize = 64 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub storage: Arc<Storage>,
    pub permits: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: Config, storage: Storage) -> Self {
        let permits = Arc::new(Semaphore::new(config.max_concurrency));
        Self {
            config: Arc::new(config),
            storage: Arc::new(storage),
            permits,
        }
    }
}

pub fn router(state: AppState) -> Router {
    let config = state.config.clone();

    let mut app = Router::new()
        .route("/health", get(routes::health))
        .route("/images", post(routes::upload))
        .route("/images/{id}", get(routes::fetch).delete(routes::delete))
        .route("/images/{id}/info", get(routes::info))
        .route("/convert", post(routes::convert))
        .route("/api-docs/openapi.json", get(docs::openapi))
        .route("/swagger-ui", get(docs::redirect))
        .route("/swagger-ui/", get(docs::page))
        .route("/swagger-ui/init.js", get(docs::init_js))
        .fallback(|| async { error::ApiError::new(StatusCode::NOT_FOUND, "not found") })
        .with_state(state)
        .layer(DefaultBodyLimit::max(
            config.max_upload_bytes + MULTIPART_OVERHEAD,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            config.request_timeout,
        ));

    if !config.cors_origins.is_empty() {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(config.cors_origins.clone())
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::RANGE])
                .expose_headers([
                    header::ACCEPT_RANGES,
                    header::CONTENT_RANGE,
                    header::CONTENT_LENGTH,
                ])
                .max_age(std::time::Duration::from_secs(3600)),
        );
    }

    app.layer(SetResponseHeaderLayer::overriding(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
    ))
    .layer(SetResponseHeaderLayer::overriding(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    ))
    .layer(SetResponseHeaderLayer::overriding(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    ))
}

#[cfg(all(test, feature = "client"))]
mod tests;
