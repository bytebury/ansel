//! HTTP handlers.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ansel::{Format, ImageInfo, Transform, is_valid_id, signing};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Multipart, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::AppState;
use super::config::{Config, StoreFormat};
use super::error::ApiError;
use super::ids;
use super::imaging;

/// Quality used when re-encoding uploaded originals.
const ORIGINAL_QUALITY: u8 = 92;
/// One year, the longest `max-age` worth sending.
const ONE_YEAR: u64 = 365 * 24 * 60 * 60;

/// Proof that the request carried the API token.
pub struct Authenticated;

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match bearer_status(&parts.headers, &state.config) {
            Bearer::Valid => Ok(Self),
            Bearer::Missing => Err(ApiError::unauthorized("missing bearer token")),
            Bearer::Invalid => Err(ApiError::unauthorized("invalid bearer token")),
        }
    }
}

enum Bearer {
    Missing,
    Invalid,
    Valid,
}

fn bearer_status(headers: &HeaderMap, config: &Config) -> Bearer {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Bearer::Missing;
    };
    let token = value
        .to_str()
        .ok()
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    match token {
        Some(t)
            if signing::constant_time_eq(
                &signing::sha256(t.as_bytes()),
                &config.api_token_hash,
            ) =>
        {
            Bearer::Valid
        }
        _ => Bearer::Invalid,
    }
}

pub async fn health() -> &'static str {
    "ok"
}

/// `POST /images`
pub async fn upload(
    State(state): State<AppState>,
    _: Authenticated,
    req: Request,
) -> Result<Response, ApiError> {
    let bytes = read_image_body(&state.config, req).await?;
    let config = state.config.clone();
    let (stored, format, width, height) = state
        .process(move || prepare_original(&bytes, &config))
        .await?;

    let id = ids::new_id();
    let info = ImageInfo {
        id: id.clone(),
        format,
        width,
        height,
        bytes: stored.len() as u64,
        created_at: ids::rfc3339_now(),
    };
    let meta = serde_json::to_vec(&info).map_err(ApiError::internal)?;
    // The original goes first: an image exists once its metadata does.
    state
        .storage
        .put(&state.original_key(&id), stored, format.mime_type())
        .await
        .map_err(ApiError::storage)?;
    state
        .storage
        .put(&state.meta_key(&id), meta, "application/json")
        .await
        .map_err(ApiError::storage)?;

    tracing::info!(%id, %format, width, height, bytes = info.bytes, "stored image");
    Ok((StatusCode::CREATED, no_store(), Json(info)).into_response())
}

/// Validates an upload and produces the bytes to store.
fn prepare_original(
    bytes: &[u8],
    config: &Config,
) -> Result<(Vec<u8>, Format, u32, u32), ApiError> {
    let (image, source) = imaging::decode(bytes, &config.allowed_formats, config.max_pixels)?;
    let (width, height) = (image.width(), image.height());
    let format = match config.store_format {
        StoreFormat::Original => imaging::output_format(source),
        StoreFormat::WebP => Format::WebP,
    };
    // With metadata stripping turned off, formats ansel can serve are stored
    // byte for byte. TIFF and BMP are always converted to PNG.
    let keep_as_is = !config.strip_metadata
        && config.store_format == StoreFormat::Original
        && imaging::STORED_FORMATS.contains(&source);
    if keep_as_is {
        return Ok((bytes.to_vec(), format, width, height));
    }
    let stored = imaging::encode(&image, format, ORIGINAL_QUALITY.max(config.default_quality))?;
    Ok((stored, format, width, height))
}

/// `GET /images/{id}/info`
pub async fn info(
    State(state): State<AppState>,
    _: Authenticated,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let info = state.load_info(&id).await?;
    Ok((no_store(), Json(info)).into_response())
}

/// `DELETE /images/{id}`
pub async fn delete(
    State(state): State<AppState>,
    _: Authenticated,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.load_info(&id).await?;
    state
        .storage
        .delete_prefix(&state.image_prefix(&id))
        .await
        .map_err(ApiError::storage)?;
    tracing::info!(%id, "deleted image");
    Ok(StatusCode::NO_CONTENT)
}

/// How a `GET /images/{id}` request was authorized; decides caching.
enum Access {
    Token,
    Signed { exp: u64 },
    Public,
}

/// `GET /images/{id}`
pub async fn fetch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    req: Request,
) -> Result<Response, ApiError> {
    if !is_valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let query = req.uri().query().unwrap_or("");
    let (requested, exp, sig) = Transform::parse_query(query).map_err(ApiError::bad_request)?;
    let config = &state.config;

    let access = match bearer_status(&headers, config) {
        Bearer::Valid => Access::Token,
        Bearer::Invalid => return Err(ApiError::unauthorized("invalid bearer token")),
        Bearer::Missing => match (exp, sig) {
            (Some(exp), Some(sig)) => {
                if exp < now() {
                    return Err(ApiError::unauthorized("signed URL has expired"));
                }
                if !signing::verify(&config.signing_key, &id, &requested.to_query(), exp, &sig) {
                    return Err(ApiError::unauthorized("invalid signature"));
                }
                Access::Signed { exp }
            }
            (None, None)
                if config.public_reads && (requested.is_empty() || requested.is_preset_only()) =>
            {
                Access::Public
            }
            (None, None) => {
                return Err(ApiError::unauthorized(
                    "a bearer token or a signed URL is required",
                ));
            }
            _ => {
                return Err(ApiError::unauthorized(
                    "signed URLs need both `exp` and `sig`",
                ));
            }
        },
    };

    let transform = resolve_preset(config, requested)?;
    validate(config, &transform)?;
    let info = state.load_info(&id).await?;

    let cache_control = match access {
        Access::Token => format!("private, max-age={ONE_YEAR}, immutable"),
        Access::Public => format!("public, max-age={ONE_YEAR}, immutable"),
        // Never let a cache keep serving a signed URL after it expires.
        Access::Signed { exp } => format!(
            "public, max-age={}",
            exp.saturating_sub(now()).min(ONE_YEAR)
        ),
    };

    if transform.is_empty() {
        let bytes = state
            .storage
            .get(&state.original_key(&id))
            .await
            .map_err(ApiError::storage)?
            .ok_or_else(ApiError::not_found)?;
        return Ok(image_response(bytes, info.format, &cache_control, false));
    }

    let negotiated = transform.format == Some(Format::Auto);
    let transform = finalize(config, transform, info.format, &headers);
    let format = transform.format.expect("finalize sets the format");
    let variant_key = state.variant_key(&id, &transform);

    if config.cache_variants
        && let Some(bytes) = state
            .storage
            .get(&variant_key)
            .await
            .map_err(ApiError::storage)?
    {
        return Ok(image_response(bytes, format, &cache_control, negotiated));
    }

    let original = state
        .storage
        .get(&state.original_key(&id))
        .await
        .map_err(ApiError::storage)?
        .ok_or_else(ApiError::not_found)?;
    let max_pixels = config.max_pixels;
    let max_dimension = config.max_output_dimension;
    let quality = transform.quality.unwrap_or(config.default_quality);
    let render = transform.clone();
    let bytes = state
        .process(move || {
            let (image, _) = imaging::decode(&original, imaging::STORED_FORMATS, max_pixels)?;
            let image = imaging::apply(image, &render, max_dimension)?;
            imaging::encode(&image, format, quality)
        })
        .await?;

    if config.cache_variants {
        let storage = state.storage.clone();
        let body = bytes.clone();
        // Store in the background; the response doesn't wait for the bucket.
        tokio::spawn(async move {
            if let Err(e) = storage.put(&variant_key, body, format.mime_type()).await {
                tracing::warn!("couldn't cache variant {variant_key}: {e}");
            }
        });
    }
    Ok(image_response(bytes, format, &cache_control, negotiated))
}

/// `POST /convert`
pub async fn convert(
    State(state): State<AppState>,
    _: Authenticated,
    req: Request,
) -> Result<Response, ApiError> {
    let headers = req.headers().clone();
    let query = req.uri().query().unwrap_or("").to_string();
    let (requested, exp, sig) = Transform::parse_query(&query).map_err(ApiError::bad_request)?;
    if exp.is_some() || sig.is_some() {
        return Err(ApiError::bad_request(
            "`exp` and `sig` only apply to GET /images/{id}",
        ));
    }
    let transform = resolve_preset(&state.config, requested)?;
    validate(&state.config, &transform)?;
    let bytes = read_image_body(&state.config, req).await?;

    let config = state.config.clone();
    let negotiated = transform.format == Some(Format::Auto);
    let (bytes, format) = state
        .process(move || {
            let (image, source) =
                imaging::decode(&bytes, &config.allowed_formats, config.max_pixels)?;
            let transform = finalize(&config, transform, imaging::output_format(source), &headers);
            let format = transform.format.expect("finalize sets the format");
            let image = imaging::apply(image, &transform, config.max_output_dimension)?;
            let quality = transform.quality.unwrap_or(config.default_quality);
            Ok((imaging::encode(&image, format, quality)?, format))
        })
        .await?;
    Ok(image_response(bytes, format, "no-store", negotiated))
}

/// Replaces `?preset=name` with the preset's parameters.
fn resolve_preset(config: &Config, t: Transform) -> Result<Transform, ApiError> {
    let Some(name) = &t.preset else {
        return Ok(t);
    };
    if !t.is_preset_only() {
        return Err(ApiError::bad_request(
            "`preset` can't be combined with other parameters",
        ));
    }
    config
        .presets
        .get(name)
        .cloned()
        .ok_or_else(|| ApiError::bad_request(format!("unknown preset `{name}`")))
}

/// Checks a transform against this server's limits.
fn validate(config: &Config, t: &Transform) -> Result<(), ApiError> {
    let max = config.max_output_dimension;
    if t.width.unwrap_or(0) > max || t.height.unwrap_or(0) > max {
        return Err(ApiError::bad_request(format!(
            "w and h can't be larger than {max}"
        )));
    }
    if t.format == Some(Format::Avif) && !cfg!(feature = "avif") {
        return Err(ApiError::bad_request(
            "AVIF output isn't enabled on this server",
        ));
    }
    Ok(())
}

/// Resolves the output format (including `auto`) and quality, so the
/// transform describes exactly what will be rendered. Its canonical query is
/// then a safe cache key.
fn finalize(config: &Config, mut t: Transform, source: Format, headers: &HeaderMap) -> Transform {
    let format = match t.format {
        None => source,
        Some(Format::Auto) => negotiate(headers, source),
        Some(f) => f,
    };
    t.format = Some(format);
    t.quality = format
        .is_lossy()
        .then(|| t.quality.unwrap_or(config.default_quality));
    t
}

/// Picks the best format the client accepts for `fmt=auto`.
fn negotiate(headers: &HeaderMap, source: Format) -> Format {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if cfg!(feature = "avif") && accept.contains("image/avif") {
        Format::Avif
    } else if accept.contains("image/webp") {
        Format::WebP
    } else if source == Format::WebP {
        // The client doesn't accept WebP, so fall back to a universal format.
        Format::Jpeg
    } else {
        source
    }
}

/// Reads an image from a `multipart/form-data` field named `file`, or from
/// the raw request body.
async fn read_image_body(config: &Config, req: Request) -> Result<Bytes, ApiError> {
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"));

    let bytes = if is_multipart {
        let mut multipart = Multipart::from_request(req, &())
            .await
            .map_err(|e| ApiError::new(e.status(), e.body_text()))?;
        let mut file = None;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::new(e.status(), e.body_text()))?
        {
            if field.name() == Some("file") {
                file = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::new(e.status(), e.body_text()))?,
                );
                break;
            }
        }
        file.ok_or_else(|| ApiError::bad_request("multipart uploads need a `file` field"))?
    } else {
        Bytes::from_request(req, &())
            .await
            .map_err(|e| ApiError::new(e.status(), e.body_text()))?
    };

    if bytes.len() > config.max_upload_bytes {
        return Err(ApiError::too_large(format!(
            "upload is larger than {} bytes",
            config.max_upload_bytes
        )));
    }
    if bytes.is_empty() {
        return Err(ApiError::bad_request("request body is empty"));
    }
    Ok(bytes)
}

fn image_response(
    bytes: Vec<u8>,
    format: Format,
    cache_control: &str,
    vary_accept: bool,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.mime_type()),
    );
    if let Ok(v) = HeaderValue::from_str(cache_control) {
        headers.insert(header::CACHE_CONTROL, v);
    }
    if vary_accept {
        headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    }
    (headers, bytes).into_response()
}

fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl AppState {
    pub fn image_prefix(&self, id: &str) -> String {
        format!("{}{id}/", self.config.key_prefix)
    }

    pub fn original_key(&self, id: &str) -> String {
        format!("{}original", self.image_prefix(id))
    }

    pub fn meta_key(&self, id: &str) -> String {
        format!("{}meta.json", self.image_prefix(id))
    }

    pub fn variant_key(&self, id: &str, t: &Transform) -> String {
        let hash = signing::hex(&signing::sha256(t.to_query().as_bytes()));
        format!("{}variants/{}", self.image_prefix(id), &hash[..32])
    }

    /// Loads an image's metadata; 404 for invalid or unknown IDs.
    pub async fn load_info(&self, id: &str) -> Result<ImageInfo, ApiError> {
        if !is_valid_id(id) {
            return Err(ApiError::not_found());
        }
        let bytes = self
            .storage
            .get(&self.meta_key(id))
            .await
            .map_err(ApiError::storage)?
            .ok_or_else(ApiError::not_found)?;
        serde_json::from_slice(&bytes).map_err(ApiError::internal)
    }

    /// Runs CPU-heavy work on a blocking thread, at most
    /// `ANSEL_MAX_CONCURRENCY` at a time. Extra requests wait their turn.
    pub async fn process<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(ApiError::internal)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(ApiError::internal)?
    }
}
