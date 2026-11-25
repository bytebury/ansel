//! Configuration from environment variables, validated at startup.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use ansel::{Format, Transform, signing};
use axum::http::HeaderValue;
use image::ImageFormat;
use reqwest::Url;

/// Shortest API token the server accepts.
pub const MIN_TOKEN_LEN: usize = 32;

pub struct Config {
    /// SHA-256 of the API token, or `None` when `ANSEL_REQUIRE_API_TOKEN=false`.
    /// Only the hash is kept in memory, and comparing hashes keeps the check
    /// constant-time for any input length.
    pub api_token_hash: Option<[u8; 32]>,
    pub signing_key: Vec<u8>,
    pub storage: StorageConfig,
    pub key_prefix: String,
    pub max_upload_bytes: usize,
    pub max_pixels: u64,
    pub max_output_dimension: u32,
    pub allowed_formats: Vec<ImageFormat>,
    pub default_quality: u8,
    pub store_format: StoreFormat,
    pub strip_metadata: bool,
    pub public_reads: bool,
    pub presets: HashMap<String, Transform>,
    pub cache_variants: bool,
    pub max_concurrency: usize,
    pub request_timeout: Duration,
    pub cors_origins: Vec<HeaderValue>,
    pub port: u16,
}

pub enum StorageConfig {
    S3 {
        endpoint: Url,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        path_style: bool,
    },
    Fs {
        root: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFormat {
    Original,
    WebP,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let (api_token_hash, signing_key) = if parse_bool("ANSEL_REQUIRE_API_TOKEN", true)? {
            let (hash, key) = token_auth()?;
            (Some(hash), key)
        } else {
            no_auth()?
        };

        let max_output_dimension = parse("ANSEL_MAX_OUTPUT_DIMENSION", 4096u32)?;
        if !(1..=16_383).contains(&max_output_dimension) {
            return Err("ANSEL_MAX_OUTPUT_DIMENSION must be between 1 and 16383".into());
        }

        let store_format = match var("ANSEL_STORE_FORMAT").as_deref().unwrap_or("original") {
            "original" => StoreFormat::Original,
            "webp" => StoreFormat::WebP,
            // AVIF can be served, but ansel can't decode it to transform it
            // later, so originals are never stored as AVIF.
            other => {
                return Err(format!(
                    "ANSEL_STORE_FORMAT must be `original` or `webp`, got `{other}`"
                ));
            }
        };

        let default_quality = parse("ANSEL_DEFAULT_QUALITY", 82u8)?;
        if !(1..=100).contains(&default_quality) {
            return Err("ANSEL_DEFAULT_QUALITY must be between 1 and 100".into());
        }

        let mut cors_origins = Vec::new();
        for origin in list("ANSEL_CORS_ORIGINS") {
            if origin == "*" {
                return Err("ANSEL_CORS_ORIGINS can't be `*`; list the allowed origins".into());
            }
            cors_origins.push(
                HeaderValue::from_str(&origin)
                    .map_err(|_| format!("invalid origin `{origin}` in ANSEL_CORS_ORIGINS"))?,
            );
        }

        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());

        Ok(Self {
            api_token_hash,
            signing_key,
            storage: storage_config()?,
            key_prefix: key_prefix()?,
            max_upload_bytes: parse("ANSEL_MAX_UPLOAD_BYTES", 20 * 1024 * 1024)?,
            max_pixels: parse("ANSEL_MAX_PIXELS", 50_000_000)?,
            max_output_dimension,
            allowed_formats: allowed_formats()?,
            default_quality,
            store_format,
            strip_metadata: parse_bool("ANSEL_STRIP_METADATA", true)?,
            public_reads: parse_bool("ANSEL_PUBLIC_READS", false)?,
            presets: presets(max_output_dimension)?,
            cache_variants: parse_bool("ANSEL_CACHE_VARIANTS", true)?,
            max_concurrency: parse("ANSEL_MAX_CONCURRENCY", cpus)?.max(1),
            request_timeout: Duration::from_secs(parse("ANSEL_REQUEST_TIMEOUT", 30)?.max(1)),
            cors_origins,
            port: parse("PORT", 8080)?,
        })
    }
}

/// The API token's hash and the URL signing key, when a token is required.
fn token_auth() -> Result<([u8; 32], Vec<u8>), String> {
    let token = var("ANSEL_API_TOKEN").ok_or(
        "ANSEL_API_TOKEN is not set. Generate one with `openssl rand -hex 32` and add it to the \
         service's variables. To run without a token on a private network, set \
         ANSEL_REQUIRE_API_TOKEN=false instead.",
    )?;
    if token.contains("$(") || token.contains("${") || token.contains('`') {
        return Err(
            "ANSEL_API_TOKEN contains a shell command instead of a token. Variables aren't \
             run through a shell, so run `openssl rand -hex 32` in a terminal and paste its \
             output as the value."
                .into(),
        );
    }
    if token.len() < MIN_TOKEN_LEN {
        return Err(format!(
            "ANSEL_API_TOKEN must be at least {MIN_TOKEN_LEN} characters (it is {}). Generate \
             one with `openssl rand -hex 32`.",
            token.len()
        ));
    }
    let signing_key = signing::signing_key(&token, var("ANSEL_SIGNING_KEY").as_deref());
    if signing_key.len() < MIN_TOKEN_LEN {
        return Err(format!(
            "ANSEL_SIGNING_KEY must be at least {MIN_TOKEN_LEN} characters"
        ));
    }
    Ok((signing::sha256(token.as_bytes()), signing_key))
}

/// Authentication turned off with `ANSEL_REQUIRE_API_TOKEN=false`. Refused
/// when Railway reports a public domain for the service.
fn no_auth() -> Result<(Option<[u8; 32]>, Vec<u8>), String> {
    if let Some(domain) = var("RAILWAY_PUBLIC_DOMAIN") {
        return Err(format!(
            "ANSEL_REQUIRE_API_TOKEN=false, but this service has a public domain ({domain}), so \
             anyone on the internet could upload and delete images. Remove the public domain and \
             reach ansel over the private network, or set ANSEL_API_TOKEN and remove \
             ANSEL_REQUIRE_API_TOKEN."
        ));
    }
    if var("ANSEL_API_TOKEN").is_some() {
        tracing::warn!("ANSEL_API_TOKEN is set but ignored, because ANSEL_REQUIRE_API_TOKEN=false");
    }
    // Every request is allowed, so signatures are never checked. A random key
    // keeps the type simple without a usable default.
    let mut key = vec![0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("couldn't generate a key: {e}"))?;
    Ok((None, key))
}

fn storage_config() -> Result<StorageConfig, String> {
    match var("ANSEL_STORAGE").as_deref().unwrap_or("s3") {
        "fs" => Ok(StorageConfig::Fs {
            root: PathBuf::from(var("ANSEL_FS_ROOT").unwrap_or_else(|| "./data".into())),
        }),
        "s3" => {
            let bucket = first(&["BUCKET", "AWS_S3_BUCKET_NAME", "AWS_BUCKET"]).ok_or(
                "No bucket is configured. Connect a Railway bucket to this service, set BUCKET \
                 (and ENDPOINT, ACCESS_KEY_ID, SECRET_ACCESS_KEY) for another S3-compatible \
                 store, or set ANSEL_STORAGE=fs for local development.",
            )?;
            let endpoint_var = first(&["ENDPOINT", "AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"]);
            let region =
                first(&["REGION", "AWS_REGION", "AWS_DEFAULT_REGION"]).unwrap_or_else(|| {
                    if endpoint_var.is_some() {
                        "auto"
                    } else {
                        "us-east-1"
                    }
                    .to_string()
                });
            let endpoint =
                endpoint_var.unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
            let endpoint =
                Url::parse(&endpoint).map_err(|e| format!("invalid ENDPOINT `{endpoint}`: {e}"))?;
            let access_key = first(&["ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"])
                .ok_or("ACCESS_KEY_ID (or AWS_ACCESS_KEY_ID) is not set")?;
            let secret_key = first(&["SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY"])
                .ok_or("SECRET_ACCESS_KEY (or AWS_SECRET_ACCESS_KEY) is not set")?;
            Ok(StorageConfig::S3 {
                endpoint,
                bucket,
                region,
                access_key,
                secret_key,
                path_style: parse_bool("ANSEL_S3_PATH_STYLE", false)?,
            })
        }
        other => Err(format!("ANSEL_STORAGE must be `s3` or `fs`, got `{other}`")),
    }
}

fn key_prefix() -> Result<String, String> {
    let prefix = var("ANSEL_KEY_PREFIX").unwrap_or_else(|| "ansel/".into());
    let ok = prefix.is_empty()
        || (prefix.ends_with('/')
            && !prefix.starts_with('/')
            && prefix.split('/').all(|seg| seg != "." && seg != "..")
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)));
    if ok {
        Ok(prefix)
    } else {
        Err(format!(
            "ANSEL_KEY_PREFIX must be empty or end with `/`, and use only letters, digits, \
             `-`, `_`, `.`, and `/`; got `{prefix}`"
        ))
    }
}

fn allowed_formats() -> Result<Vec<ImageFormat>, String> {
    let names = list("ANSEL_ALLOWED_FORMATS");
    let names = if names.is_empty() {
        vec!["jpeg".into(), "png".into(), "webp".into(), "gif".into()]
    } else {
        names
    };
    names
        .iter()
        .map(|name| match name.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Ok(ImageFormat::Jpeg),
            "png" => Ok(ImageFormat::Png),
            "webp" => Ok(ImageFormat::WebP),
            "gif" => Ok(ImageFormat::Gif),
            "tiff" | "tif" => Ok(ImageFormat::Tiff),
            "bmp" => Ok(ImageFormat::Bmp),
            "svg" => Err("SVG can't be allowed: SVG files can contain scripts".to_string()),
            "avif" => Err(
                "AVIF uploads aren't supported (AVIF is available as an output format)".to_string(),
            ),
            other => Err(format!("unknown format `{other}` in ANSEL_ALLOWED_FORMATS")),
        })
        .collect()
}

/// Parses `thumb=w:200,h:200,fit:cover;hero=w:1600,fmt:auto`.
fn presets(max_dimension: u32) -> Result<HashMap<String, Transform>, String> {
    let mut presets = HashMap::new();
    let Some(raw) = var("ANSEL_PRESETS") else {
        return Ok(presets);
    };
    for entry in raw.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (name, params) = entry
            .split_once('=')
            .ok_or_else(|| format!("preset `{entry}` must look like `name=w:200,h:200`"))?;
        let name = name.trim();
        if !ansel::is_valid_preset_name(name) {
            return Err(format!(
                "invalid preset name `{name}` (use a-z, 0-9, `-`, `_`)"
            ));
        }
        let query = params
            .split(',')
            .map(|p| p.trim().replacen(':', "=", 1))
            .collect::<Vec<_>>()
            .join("&");
        let (t, exp, sig) =
            Transform::parse_query(&query).map_err(|e| format!("preset `{name}`: {e}"))?;
        if t.preset.is_some() || exp.is_some() || sig.is_some() {
            return Err(format!(
                "preset `{name}` can only contain transform parameters"
            ));
        }
        if t.width.unwrap_or(0) > max_dimension || t.height.unwrap_or(0) > max_dimension {
            return Err(format!(
                "preset `{name}` is larger than ANSEL_MAX_OUTPUT_DIMENSION"
            ));
        }
        if t.format == Some(Format::Avif) && !cfg!(feature = "avif") {
            return Err(format!(
                "preset `{name}` uses AVIF, but this build has no `avif` feature"
            ));
        }
        if presets.insert(name.to_string(), t).is_some() {
            return Err(format!("preset `{name}` is defined twice"));
        }
    }
    Ok(presets)
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn first(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| var(n))
}

fn list(name: &str) -> Vec<String> {
    var(name)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse<T: FromStr>(name: &str, default: T) -> Result<T, String> {
    match var(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name} has an invalid value `{v}`")),
    }
}

fn parse_bool(name: &str, default: bool) -> Result<bool, String> {
    match var(name).map(|v| v.to_ascii_lowercase()).as_deref() {
        None => Ok(default),
        Some("true" | "1" | "yes" | "on") => Ok(true),
        Some("false" | "0" | "no" | "off") => Ok(false),
        Some(v) => Err(format!("{name} must be true or false, got `{v}`")),
    }
}
