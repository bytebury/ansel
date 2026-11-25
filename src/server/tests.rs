//! End-to-end tests: a real server on a random port, local-disk storage, and
//! the client SDK.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use ansel::{Client, Fit, Format, Transform};
use image::{DynamicImage, GenericImageView, ImageFormat};

use super::config::{Config, StorageConfig, StoreFormat};
use super::{AppState, Storage, router};

const TOKEN: &str = "test-token-0123456789abcdef0123456789";

struct TestServer {
    url: String,
    root: PathBuf,
    client: Client,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn config(root: PathBuf) -> Config {
    Config {
        api_token_hash: ansel::signing::sha256(TOKEN.as_bytes()),
        signing_key: ansel::signing::signing_key(TOKEN, None),
        storage: StorageConfig::Fs { root },
        key_prefix: "ansel/".into(),
        max_upload_bytes: 2 * 1024 * 1024,
        max_pixels: 4_000_000,
        max_output_dimension: 1024,
        allowed_formats: vec![
            ImageFormat::Jpeg,
            ImageFormat::Png,
            ImageFormat::WebP,
            ImageFormat::Gif,
        ],
        default_quality: 82,
        store_format: StoreFormat::Original,
        strip_metadata: true,
        public_reads: false,
        presets: HashMap::new(),
        cache_variants: true,
        max_concurrency: 2,
        request_timeout: Duration::from_secs(30),
        cors_origins: Vec::new(),
        port: 0,
    }
}

async fn start(customize: impl FnOnce(&mut Config)) -> TestServer {
    let root = std::env::temp_dir().join(format!("ansel-test-{}", super::ids::new_id()));
    let mut config = config(root.clone());
    customize(&mut config);
    let storage = Storage::new(&config.storage).unwrap();
    let app = router(AppState::new(config, storage));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(&url, TOKEN).unwrap();
    TestServer { url, root, client }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
    }));
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

fn decode(bytes: &[u8]) -> (DynamicImage, ImageFormat) {
    let format = image::guess_format(bytes).unwrap();
    (image::load_from_memory(bytes).unwrap(), format)
}

/// A JPEG whose EXIF says "rotate 90° clockwise" and which carries a GPS tag.
fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::ImageRgb8(image::RgbImage::new(w, h));
    let mut jpeg = Vec::new();
    img.write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
        .unwrap();

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\0\x2A\0\0\0\x08"); // big-endian header, IFD0 at 8
    tiff.extend_from_slice(&2u16.to_be_bytes()); // IFD0: 2 entries
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]); // Orientation = 6
    tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 38]); // GPS IFD at 38
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff.extend_from_slice(&1u16.to_be_bytes()); // GPS IFD: 1 entry
    tiff.extend_from_slice(&[0x00, 0x01, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]); // GPSLatitudeRef = "N"
    tiff.extend_from_slice(&0u32.to_be_bytes());

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn find_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(find_files(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test]
async fn upload_info_signed_url_and_delete() {
    let s = start(|_| {}).await;
    let info = s.client.upload(png(300, 200)).await.unwrap();
    assert_eq!(
        (info.width, info.height, info.format),
        (300, 200, Format::Png)
    );
    assert!(ansel::is_valid_id(&info.id));
    assert_eq!(s.client.get(&info.id).await.unwrap(), info);

    // A signed URL works without a token.
    let url = s
        .client
        .url(
            &info.id,
            Transform::new()
                .size(100, 100)
                .fit(Fit::Cover)
                .format(Format::WebP),
        )
        .unwrap();
    let res = reqwest::get(&url).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "image/webp");
    let cache = res.headers()["cache-control"].to_str().unwrap().to_string();
    assert!(cache.starts_with("public, max-age="), "{cache}");
    let (img, format) = decode(&res.bytes().await.unwrap());
    assert_eq!((format, img.dimensions()), (ImageFormat::WebP, (100, 100)));

    // The variant is cached in storage (written in the background).
    let mut cached = false;
    for _ in 0..50 {
        if find_files(&s.root)
            .iter()
            .any(|p| p.to_string_lossy().contains("/variants/"))
        {
            cached = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(cached, "variant was not cached");
    // The cached copy is served on the next request.
    let again = reqwest::get(&url).await.unwrap();
    assert_eq!(again.status(), 200);
    assert_eq!(
        decode(&again.bytes().await.unwrap()).0.dimensions(),
        (100, 100)
    );

    s.client.delete(&info.id).await.unwrap();
    match s.client.get(&info.id).await {
        Err(ansel::Error::Api { status: 404, .. }) => {}
        other => panic!("expected 404, got {other:?}"),
    }
    assert!(find_files(&s.root).is_empty(), "files left after delete");
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 404);
}

#[tokio::test]
async fn signed_urls_are_checked() {
    let s = start(|_| {}).await;
    let info = s.client.upload(png(50, 50)).await.unwrap();
    let base = format!("{}/images/{}", s.url, info.id);

    // No credentials at all.
    assert_eq!(reqwest::get(&base).await.unwrap().status(), 401);
    assert_eq!(
        reqwest::get(format!("{base}?w=10")).await.unwrap().status(),
        401
    );

    let url = s.client.url(&info.id, Transform::new().width(10)).unwrap();
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 200);

    // Changing the transform invalidates the signature.
    let tampered = url.replace("w=10", "w=20");
    assert_eq!(reqwest::get(&tampered).await.unwrap().status(), 401);
    // Adding a parameter invalidates it too, wherever it goes.
    assert_eq!(
        reqwest::get(format!("{url}&blur=50"))
            .await
            .unwrap()
            .status(),
        401
    );
    let extra = url.replacen("w=10", "w=10&blur=50", 1);
    assert_eq!(reqwest::get(&extra).await.unwrap().status(), 401);

    // Expired URLs are rejected.
    let expired = s
        .client
        .url(
            &info.id,
            Transform::new().width(10).expires_in(Duration::ZERO),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(reqwest::get(&expired).await.unwrap().status(), 401);

    // A client with a different token can't sign valid URLs.
    let other = Client::new(&s.url, "some-other-token-0123456789abcdefghij").unwrap();
    let forged = other.url(&info.id, Transform::new().width(10)).unwrap();
    assert_eq!(reqwest::get(&forged).await.unwrap().status(), 401);
    // And it can't use the API.
    assert!(matches!(
        other.get(&info.id).await,
        Err(ansel::Error::Api { status: 401, .. })
    ));
}

#[tokio::test]
async fn uploads_are_validated() {
    let s = start(|_| {}).await;
    let status = |r: ansel::Result<ansel::ImageInfo>| match r {
        Err(ansel::Error::Api { status, .. }) => status,
        other => panic!("expected an API error, got {other:?}"),
    };

    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    assert_eq!(status(s.client.upload(svg.to_vec()).await), 415);
    assert_eq!(status(s.client.upload(b"not an image".to_vec()).await), 415);
    assert_eq!(status(s.client.upload(Vec::new()).await), 400);

    // A PNG polyglot: a valid header followed by garbage pixel data.
    let mut broken = png(10, 10);
    broken.truncate(40);
    assert_eq!(status(s.client.upload(broken).await), 422);

    // A header claiming more pixels than allowed is rejected before decoding.
    let bomb = super::imaging::png_with_dimensions(png(1, 1), 40_000, 40_000);
    assert_eq!(status(s.client.upload(bomb).await), 413);

    // Bodies over the upload limit.
    assert_eq!(
        status(s.client.upload(vec![0u8; 3 * 1024 * 1024]).await),
        413
    );

    // TIFF isn't allowed by default.
    let mut tiff = Vec::new();
    DynamicImage::ImageRgb8(image::RgbImage::new(4, 4))
        .write_to(&mut Cursor::new(&mut tiff), ImageFormat::Tiff)
        .unwrap();
    assert_eq!(status(s.client.upload(tiff).await), 415);
}

#[tokio::test]
async fn metadata_is_stripped_and_orientation_applied() {
    let s = start(|_| {}).await;
    let original = jpeg_with_exif(40, 20);
    let info = s.client.upload(original).await.unwrap();
    // Orientation 6 rotates the 40×20 image to 20×40.
    assert_eq!((info.width, info.height), (20, 40));

    let stored = s.client.download(&info.id, Transform::new()).await.unwrap();
    assert_eq!(stored.content_type, "image/jpeg");
    let stored = stored.bytes;
    assert!(!stored.windows(4).any(|w| w == b"Exif"), "EXIF survived");
    assert_eq!(decode(&stored).0.dimensions(), (20, 40));
}

#[tokio::test]
async fn multipart_uploads_work() {
    let s = start(|_| {}).await;
    let boundary = "ansel-test-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.png\"\r\n\
         Content-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&png(12, 8));
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let res = reqwest::Client::new()
        .post(format!("{}/images", s.url))
        .bearer_auth(TOKEN)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let info: ansel::ImageInfo = res.json().await.unwrap();
    assert_eq!((info.width, info.height), (12, 8));
}

#[tokio::test]
async fn convert_and_negotiation() {
    let s = start(|_| {}).await;
    let jpeg = s
        .client
        .convert(
            png(64, 32),
            Transform::new().width(32).format(Format::Jpeg).quality(70),
        )
        .await
        .unwrap();
    let (img, format) = decode(&jpeg);
    assert_eq!((format, img.dimensions()), (ImageFormat::Jpeg, (32, 16)));

    // fmt=auto picks WebP when the client accepts it.
    let res = reqwest::Client::new()
        .post(format!("{}/convert?fmt=auto", s.url))
        .bearer_auth(TOKEN)
        .header("accept", "image/webp,image/*")
        .body(png(8, 8))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "image/webp");
    assert_eq!(res.headers()["vary"], "Accept");
    assert_eq!(res.headers()["cache-control"], "no-store");

    // Limits apply to convert too.
    let too_big = s
        .client
        .convert(png(8, 8), Transform::new().width(5000))
        .await;
    assert!(matches!(
        too_big,
        Err(ansel::Error::Api { status: 400, .. })
    ));
    // Nothing was stored.
    assert!(find_files(&s.root).is_empty());
}

#[tokio::test]
async fn public_reads_and_presets() {
    let s = start(|c| {
        c.public_reads = true;
        c.presets.insert(
            "thumb".into(),
            Transform::new().size(16, 16).fit(Fit::Cover),
        );
    })
    .await;
    let info = s.client.upload(png(64, 48)).await.unwrap();
    let base = format!("{}/images/{}", s.url, info.id);

    let original = reqwest::get(&base).await.unwrap();
    assert_eq!(original.status(), 200);
    assert!(
        original.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    let thumb = reqwest::get(format!("{base}?preset=thumb")).await.unwrap();
    assert_eq!(thumb.status(), 200);
    assert_eq!(
        decode(&thumb.bytes().await.unwrap()).0.dimensions(),
        (16, 16)
    );

    // Arbitrary transforms still need a signature.
    assert_eq!(
        reqwest::get(format!("{base}?w=10")).await.unwrap().status(),
        401
    );
    assert_eq!(
        reqwest::get(format!("{base}?preset=nope"))
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        reqwest::get(format!("{base}?preset=thumb&w=3"))
            .await
            .unwrap()
            .status(),
        401
    );
    // Writes still need the token.
    let res = reqwest::Client::new().delete(&base).send().await.unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn security_headers_and_misc_routes() {
    let s = start(|_| {}).await;
    let res = reqwest::get(format!("{}/health", s.url)).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert!(
        res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert_eq!(res.text().await.unwrap(), "ok");

    let res = reqwest::get(format!("{}/images/../../etc/passwd", s.url))
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    let res = reqwest::Client::new()
        .get(format!("{}/images/not-an-id/info", s.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = reqwest::get(format!("{}/images", s.url)).await.unwrap();
    assert_eq!(res.status(), 405);
    let res = reqwest::Client::new()
        .post(format!("{}/images", s.url))
        .body(png(2, 2))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.headers()["www-authenticate"], "Bearer");

    let res = reqwest::get(format!("{}/swagger-ui/", s.url))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(
        res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("cdn.jsdelivr.net")
    );
    let res = reqwest::get(format!("{}/api-docs/openapi.json", s.url))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn store_as_webp() {
    let s = start(|c| c.store_format = StoreFormat::WebP).await;
    let info = s.client.upload(png(20, 10)).await.unwrap();
    assert_eq!(info.format, Format::WebP);
    let image = s.client.download(&info.id, Transform::new()).await.unwrap();
    assert_eq!(image.content_type, "image/webp");
    let bytes = image.bytes;
    assert_eq!(decode(&bytes).1, ImageFormat::WebP);
}

/// The only test that touches the global client, since it's process-wide.
#[tokio::test]
async fn macros_use_the_global_client() {
    let s = start(|_| {}).await;
    ansel::install(s.client.clone()).unwrap();
    assert!(matches!(
        ansel::setup(&s.url, TOKEN),
        Err(ansel::Error::AlreadyConfigured)
    ));

    let image = ansel::upload!(png(40, 40)).unwrap();
    assert_eq!(ansel::get!(image.id).unwrap(), image);

    let small = ansel::download!(image.id, width = 10, format = Format::Jpeg).unwrap();
    assert_eq!(small.content_type, "image/jpeg");
    assert_eq!(decode(&small.bytes).0.dimensions(), (10, 10));

    let webp = ansel::convert!(png(8, 4), format = Format::WebP, quality = 50).unwrap();
    assert_eq!(decode(&webp).1, ImageFormat::WebP);

    let url = ansel::url!(image.id, width = 20, height = 20, fit = Fit::Cover).unwrap();
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 200);

    ansel::delete!(image.id).unwrap();
    assert!(ansel::get!(image.id).is_err());
}

#[cfg(feature = "avif")]
#[tokio::test]
async fn avif_output() {
    let s = start(|_| {}).await;
    let avif = s
        .client
        .convert(png(16, 16), Transform::new().format(Format::Avif))
        .await
        .unwrap();
    assert_eq!(&avif[4..12], b"ftypavif");

    // `fmt=auto` prefers AVIF when the client accepts it.
    let res = reqwest::Client::new()
        .post(format!("{}/convert?fmt=auto", s.url))
        .bearer_auth(TOKEN)
        .header("accept", "image/avif,image/webp,*/*")
        .body(png(8, 8))
        .send()
        .await
        .unwrap();
    assert_eq!(res.headers()["content-type"], "image/avif");
}
