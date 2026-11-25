//! The HTTP client SDK.

use std::borrow::Borrow;
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::{ImageInfo, Transform, is_valid_id, signing};

/// How long signed URLs last when neither `ANSEL_URL_TTL` nor
/// [`Transform::expires_in`] says otherwise.
pub const DEFAULT_URL_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Errors returned by the SDK.
#[derive(Debug)]
pub enum Error {
    /// Missing or invalid configuration, such as an unset `ANSEL_URL`.
    Config(String),
    /// `setup` was called after the global client was already configured.
    AlreadyConfigured,
    /// The image ID isn't a valid ansel ID.
    InvalidId(String),
    /// The request couldn't be sent, or the response couldn't be read.
    Http(reqwest::Error),
    /// The server answered with an error status.
    Api { status: u16, message: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "ansel is not configured correctly: {msg}"),
            Self::AlreadyConfigured => f.write_str("ansel::setup was already called"),
            Self::InvalidId(id) => write!(f, "`{id}` is not a valid ansel image ID"),
            Self::Http(e) => write!(f, "request to ansel failed: {e}"),
            Self::Api { status, message } => write!(f, "ansel returned {status}: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

/// Result type used by the SDK.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A client for one ansel server.
///
/// Most code can use the free functions ([`upload`], [`url`], …), which share
/// one global client. Build a `Client` yourself to talk to several servers or
/// to supply your own [`reqwest::Client`].
#[derive(Clone)]
pub struct Client {
    base_url: String,
    token: Option<String>,
    signing_key: Option<Vec<u8>>,
    url_ttl: Duration,
    http: reqwest::Client,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the token or the signing key.
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("url_ttl", &self.url_ttl)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client for the server at `url`, authenticating with `token`.
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.is_empty() {
            return Err(Error::Config("API token is empty".into()));
        }
        Self::build(url.into(), Some(token))
    }

    /// Creates a client that sends no token, for a server running with
    /// `ANSEL_REQUIRE_API_TOKEN=false` (for example, one only reachable over
    /// a private network).
    ///
    /// [`Client::url`] then returns plain, unsigned URLs.
    pub fn unauthenticated(url: impl Into<String>) -> Result<Self> {
        Self::build(url.into(), None)
    }

    fn build(url: String, token: Option<String>) -> Result<Self> {
        let base_url = url.trim().trim_end_matches('/').to_string();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(Error::Config(format!(
                "server URL must start with https:// or http://, got `{base_url}`"
            )));
        }
        Ok(Self {
            base_url,
            signing_key: token.as_deref().map(|t| signing::signing_key(t, None)),
            token,
            url_ttl: DEFAULT_URL_TTL,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }

    /// Creates a client from `ANSEL_URL` and, when set, `ANSEL_API_TOKEN`.
    /// Without a token the client is [unauthenticated](Client::unauthenticated).
    /// Also reads `ANSEL_SIGNING_KEY` and `ANSEL_URL_TTL` (seconds) when set.
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let url = var("ANSEL_URL").ok_or_else(|| Error::Config("ANSEL_URL is not set".into()))?;
        let mut client = match var("ANSEL_API_TOKEN") {
            Some(token) => Self::new(url, token)?,
            None => Self::unauthenticated(url)?,
        };
        if let Some(key) = var("ANSEL_SIGNING_KEY") {
            client = client.with_signing_key(key);
        }
        if let Some(ttl) = var("ANSEL_URL_TTL") {
            let secs = ttl.trim().parse().map_err(|_| {
                Error::Config(format!("ANSEL_URL_TTL must be seconds, got `{ttl}`"))
            })?;
            client = client.with_url_ttl(Duration::from_secs(secs));
        }
        Ok(client)
    }

    /// Uses a separate URL signing key. Only needed when the server sets
    /// `ANSEL_SIGNING_KEY`.
    pub fn with_signing_key(mut self, key: impl AsRef<[u8]>) -> Self {
        self.signing_key = Some(key.as_ref().to_vec());
        self
    }

    /// Adds the bearer token, when there is one.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    /// Changes how long signed URLs last by default.
    pub fn with_url_ttl(mut self, ttl: Duration) -> Self {
        self.url_ttl = ttl;
        self
    }

    /// Uses your own `reqwest::Client`, for custom timeouts or proxies.
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Uploads an image and returns its metadata.
    pub async fn upload(&self, bytes: impl Into<reqwest::Body>) -> Result<ImageInfo> {
        let res = self
            .auth(self.http.post(format!("{}/images", self.base_url)))
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?;
        Ok(check(res).await?.json().await?)
    }

    /// Fetches an image's metadata.
    pub async fn get(&self, id: impl AsRef<str>) -> Result<ImageInfo> {
        let id = checked_id(id.as_ref())?;
        let res = self
            .auth(self.http.get(format!("{}/images/{id}/info", self.base_url)))
            .send()
            .await?;
        Ok(check(res).await?.json().await?)
    }

    /// Downloads an image, with a transform applied.
    ///
    /// The result carries the content type, so your app can serve it to a
    /// browser as is. See [`Image`].
    pub async fn download(
        &self,
        id: impl AsRef<str>,
        transform: impl Borrow<Transform>,
    ) -> Result<Image> {
        let id = checked_id(id.as_ref())?;
        let query = transform.borrow().to_query();
        let mut url = format!("{}/images/{id}", self.base_url);
        if !query.is_empty() {
            url = format!("{url}?{query}");
        }
        let res = check(self.auth(self.http.get(url)).send().await?).await?;
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        Ok(Image {
            bytes: res.bytes().await?.to_vec(),
            content_type,
        })
    }

    /// Deletes an image and all of its cached variants.
    pub async fn delete(&self, id: impl AsRef<str>) -> Result<()> {
        let id = checked_id(id.as_ref())?;
        let res = self
            .auth(self.http.delete(format!("{}/images/{id}", self.base_url)))
            .send()
            .await?;
        check(res).await?;
        Ok(())
    }

    /// Transforms an image and returns the result. Nothing is stored.
    pub async fn convert(
        &self,
        bytes: impl Into<reqwest::Body>,
        transform: impl Borrow<Transform>,
    ) -> Result<Vec<u8>> {
        let query = transform.borrow().to_query();
        let res = self
            .auth(self.http.post(format!("{}/convert?{query}", self.base_url)))
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?;
        Ok(check(res).await?.bytes().await?.to_vec())
    }

    /// Builds a signed URL for an image. No network call is made.
    ///
    /// An [unauthenticated](Client::unauthenticated) client has no signing
    /// key, so it returns a plain URL instead.
    pub fn url(&self, id: impl AsRef<str>, transform: impl Borrow<Transform>) -> Result<String> {
        let id = checked_id(id.as_ref())?;
        let transform = transform.borrow();
        let Some(signing_key) = &self.signing_key else {
            let query = transform.to_query();
            let sep = if query.is_empty() { "" } else { "?" };
            return Ok(format!("{}/images/{id}{sep}{query}", self.base_url));
        };
        let ttl = transform.expiry().unwrap_or(self.url_ttl);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let exp = (now + ttl).as_secs();
        let query = transform.to_query();
        let sig = signing::sign(signing_key, id, &query, exp);
        let sep = if query.is_empty() { "" } else { "&" };
        Ok(format!(
            "{}/images/{id}?{query}{sep}exp={exp}&sig={sig}",
            self.base_url
        ))
    }
}

/// A downloaded image, ready to send to a browser.
///
/// Images never change once uploaded, so a response built from one can be
/// cached for as long as browsers allow. Send [`Image::CACHE_CONTROL`] with it:
///
/// ```no_run
/// # async fn run(id: String) -> ansel::Result<()> {
/// let image = ansel::download!(id, width = 800)?;
/// // In your web framework's handler:
/// let headers = [
///     ("content-type", image.content_type.as_str()),
///     ("cache-control", ansel::Image::CACHE_CONTROL),
/// ];
/// # let _ = (headers, image.bytes);
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The encoded image.
    pub bytes: Vec<u8>,
    /// Its MIME type, such as `image/webp`.
    pub content_type: String,
}

impl Image {
    /// `Cache-Control` for serving an image: browsers and CDNs may keep it
    /// for a year without checking back, because an image ID and transform
    /// always produce the same bytes.
    pub const CACHE_CONTROL: &'static str = "public, max-age=31536000, immutable";
}

fn checked_id(id: &str) -> Result<&str> {
    if is_valid_id(id) {
        Ok(id)
    } else {
        Err(Error::InvalidId(id.to_string()))
    }
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let text = res.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ErrorBody>(&text)
        .map(|b| b.error)
        .unwrap_or(text);
    Err(Error::Api {
        status: status.as_u16(),
        message,
    })
}

static GLOBAL: OnceLock<Client> = OnceLock::new();

/// Configures the global client used by the free functions and macros.
///
/// You can skip this: the first call that needs a client reads `ANSEL_URL`
/// and `ANSEL_API_TOKEN` from the environment instead.
pub fn setup(url: impl Into<String>, token: impl Into<String>) -> Result<()> {
    install(Client::new(url, token)?)
}

/// Configures the global client for a server that doesn't require a token
/// (`ANSEL_REQUIRE_API_TOKEN=false`). See [`Client::unauthenticated`].
pub fn setup_unauthenticated(url: impl Into<String>) -> Result<()> {
    install(Client::unauthenticated(url)?)
}

/// Installs a fully configured [`Client`] as the global client.
pub fn install(client: Client) -> Result<()> {
    GLOBAL.set(client).map_err(|_| Error::AlreadyConfigured)
}

/// The global client, built from the environment on first use if
/// [`setup`] wasn't called.
pub fn client() -> Result<&'static Client> {
    if let Some(c) = GLOBAL.get() {
        return Ok(c);
    }
    let c = Client::from_env()?;
    // Another thread may have won the race; either client is equivalent.
    let _ = GLOBAL.set(c);
    Ok(GLOBAL.get().expect("global client was just set"))
}

/// Uploads an image with the global client. See [`Client::upload`].
pub async fn upload(bytes: impl Into<reqwest::Body>) -> Result<ImageInfo> {
    client()?.upload(bytes).await
}

/// Fetches an image's metadata with the global client. See [`Client::get`].
pub async fn get(id: impl AsRef<str>) -> Result<ImageInfo> {
    client()?.get(id).await
}

/// Downloads an image with the global client. See [`Client::download`].
pub async fn download(id: impl AsRef<str>, transform: impl Borrow<Transform>) -> Result<Image> {
    client()?.download(id, transform).await
}

/// Deletes an image with the global client. See [`Client::delete`].
pub async fn delete(id: impl AsRef<str>) -> Result<()> {
    client()?.delete(id).await
}

/// Transforms an image without storing it. See [`Client::convert`].
pub async fn convert(
    bytes: impl Into<reqwest::Body>,
    transform: impl Borrow<Transform>,
) -> Result<Vec<u8>> {
    client()?.convert(bytes, transform).await
}

/// Builds a signed URL with the global client. See [`Client::url`].
pub fn url(id: impl AsRef<str>, transform: impl Borrow<Transform>) -> Result<String> {
    client()?.url(id, transform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Format;

    #[test]
    fn rejects_bad_config() {
        assert!(matches!(
            Client::new("ansel.example.com", "t"),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            Client::new("https://ansel.example.com", ""),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn builds_signed_urls() {
        let c = Client::new("https://ansel.example.com/", "token").unwrap();
        let id = "01JD8Z6Q4M3V9K2T7XW5R8N0PB";
        let url = c
            .url(id, Transform::new().width(400).format(Format::WebP))
            .unwrap();
        let (base, query) = url.split_once('?').unwrap();
        assert_eq!(base, format!("https://ansel.example.com/images/{id}"));
        let (t, exp, sig) = Transform::parse_query(query).unwrap();
        assert_eq!(t.to_query(), "w=400&fmt=webp");
        let key = signing::signing_key("token", None);
        assert!(signing::verify(
            &key,
            id,
            &t.to_query(),
            exp.unwrap(),
            &sig.unwrap()
        ));

        let plain = c.url(id, Transform::new()).unwrap();
        assert!(plain.contains(&format!("{id}?exp=")));
    }

    #[test]
    fn unauthenticated_client_builds_plain_urls() {
        let c = Client::unauthenticated("http://ansel.railway.internal:8080").unwrap();
        let id = "01JD8Z6Q4M3V9K2T7XW5R8N0PB";
        assert_eq!(
            c.url(id, Transform::new().width(400)).unwrap(),
            format!("http://ansel.railway.internal:8080/images/{id}?w=400")
        );
        assert_eq!(
            c.url(id, Transform::new()).unwrap(),
            format!("http://ansel.railway.internal:8080/images/{id}")
        );
    }

    #[test]
    fn url_rejects_bad_ids() {
        let c = Client::new("https://ansel.example.com", "token").unwrap();
        assert!(matches!(
            c.url("../admin", Transform::new()),
            Err(Error::InvalidId(_))
        ));
    }

    #[test]
    fn debug_hides_secrets() {
        let c = Client::new("https://ansel.example.com", "super-secret").unwrap();
        assert!(!format!("{c:?}").contains("super-secret"));
    }
}
