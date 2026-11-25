<p align="center">
  <img src="assets/ansel.svg" alt="ansel: a crab in a sun hat holding a camera" width="200">
</p>

<h1 align="center">ansel</h1>

<p align="center">
  Upload, resize, and convert images through one small, secure Rust service.
</p>

An image upload, editing, and conversion microservice written in Rust, named
after the photographer Ansel Adams. It is built on
[axum](https://github.com/tokio-rs/axum) and [image](https://github.com/image-rs/image).

Ansel comes in two parts:

- an **HTTP microservice** that holds your storage credentials, checks every
  upload, and resizes and converts images on request, and
- a **Rust client crate** (SDK) that your other services use to upload images
  and build URLs for them.

You deploy one ansel server, then run `cargo add ansel` in every Rust service
that handles images. Only the server knows the bucket credentials. Clients
just make HTTP calls.

```text
 your app ──(ansel crate)──▶ ansel ──(S3 API)──▶ Railway Bucket / any S3
    ▲                          │
    └──── signed image URL ◀───┘   resize · crop · convert · strip metadata
```

## Contents

- [Quick start (Railway)](#quick-start-railway)
- [Client usage (SDK)](#client-usage-sdk)
- [API](#api)
- [Transforms](#transforms)
- [Security](#security)
- [Configuration](#configuration)
- [Cargo features](#cargo-features)
- [Development](#development)
- [Deploying elsewhere](#deploying-elsewhere)
- [Publishing the SDK](#publishing-the-sdk)

## Quick start (Railway)

Ansel works with [Railway Storage Buckets](https://docs.railway.com/storage-buckets)
without any storage configuration.

1. **Create a bucket** in your Railway project.
2. **Deploy ansel** from this repo as a new service. The included
   `railway.json` builds the Dockerfile and sets `/health` as the health check.
3. **Connect the bucket** to the ansel service. Railway adds the bucket's
   variables (`BUCKET`, `ACCESS_KEY_ID`, `SECRET_ACCESS_KEY`, `REGION`,
   `ENDPOINT`) to the service, and ansel reads them as they are.
4. **Add one secret** to the service's **Variables** tab:

   ```sh
   ANSEL_API_TOKEN=$(openssl rand -hex 32)
   ```

That's all the setup. Ansel refuses to start if `ANSEL_API_TOKEN` is missing
or shorter than 32 characters, so a misconfigured deploy never runs without
authentication.

Railway buckets are private, and ansel keeps it that way. Images are served
through ansel with signed URLs (see [Security](#security)). The bucket never
needs public access.

## Client usage (SDK)

```toml
[dependencies]
ansel = "0.1"  # client by default; no need to disable anything
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Set your server's address and token once, then upload from anywhere:

```rust
ansel::setup("https://ansel.example.com", "super-secret-token")?;

let bytes = std::fs::read("avatar.jpg")?;
let image = ansel::upload(bytes).await?;

println!("{}", image.id);      // "01JD8Z6Q4M3V9K2T7XW5R8N0PB"
println!("{}x{}", image.width, image.height);
```

You can also skip `setup()`. On first use, the SDK reads `ANSEL_URL` and
`ANSEL_API_TOKEN` from the environment:

```rust
let image = ansel::upload(bytes).await?; // reads ANSEL_URL / ANSEL_API_TOKEN
```

### Building image URLs

`ansel::url` builds a **signed** URL for an image with the transforms you ask
for. You can put it straight into an `<img>` tag, and it doesn't need a
network call:

```rust
use ansel::{Fit, Format, Transform};

let src = ansel::url(
    &image.id,
    Transform::new()
        .width(400)
        .height(400)
        .fit(Fit::Cover)
        .format(Format::WebP)
        .quality(80),
)?;
// https://ansel.example.com/images/01JD8Z...?w=400&h=400&fit=cover&fmt=webp&q=80&exp=…&sig=…
```

Signed URLs expire after 7 days, or after `ANSEL_URL_TTL` seconds if you set
it in the client's environment. Use `.expires_in(Duration)` to change it for a
single URL.

### Converting without storing

Use `ansel::convert` to transform bytes and get the result back. Nothing is
written to the bucket:

```rust
let webp: Vec<u8> = ansel::convert(png_bytes, Transform::new().format(Format::WebP)).await?;
```

### Other operations

```rust
let info = ansel::get(&image.id).await?;                              // dimensions, format, size, created_at
let thumb = ansel::download(&image.id, Transform::new().width(200)).await?; // the image bytes
ansel::delete(&image.id).await?;                                      // removes the original and all cached variants
```

If you talk to more than one server, or need control over the `reqwest::Client`,
use `ansel::Client::new(url, token)` directly.

### Macros

Every operation has a macro shorthand that awaits for you. Transform
parameters are written as `name = value`, where each name is a `Transform`
setter:

```rust
use ansel::{Fit, Format};

let image = ansel::upload!(bytes)?;
let info = ansel::get!(image.id)?;
let webp = ansel::convert!(png_bytes, format = Format::WebP, quality = 80)?;
let thumb = ansel::download!(image.id, width = 200, height = 200, fit = Fit::Cover)?;
let src = ansel::url!(image.id, width = 400, format = Format::Auto)?; // no await: no network call
ansel::delete!(image.id)?;
```

The macros use the global client, so call `ansel::setup` first or set
`ANSEL_URL` and `ANSEL_API_TOKEN`. Like the functions, they return a `Result`.

## API

All endpoints except `GET /health` require authentication. Send the token from
`ANSEL_API_TOKEN` as `Authorization: Bearer <token>`. `GET /images/{id}` also
accepts a signed URL in place of the token.

### `POST /images`: upload

Send the image in a `multipart/form-data` field named `file`, or as the raw
request body:

```sh
curl -X POST https://ansel.example.com/images \
  -H "Authorization: Bearer $ANSEL_API_TOKEN" \
  -F "file=@photo.jpg"
```

Returns `201`:

```json
{
  "id": "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
  "format": "jpeg",
  "width": 4032,
  "height": 3024,
  "bytes": 2481930,
  "created_at": "2026-11-25T18:04:11Z"
}
```

Ansel reads the file's real format from its contents, and ignores the
filename and `Content-Type`. It decodes the image within the size limits,
rotates it upright from its EXIF orientation, and re-encodes it before storing
it. Re-encoding removes all metadata, including GPS location (see
[Security](#security)).

### `GET /images/{id}`: fetch, optionally transformed

```sh
curl -H "Authorization: Bearer $ANSEL_API_TOKEN" \
  "https://ansel.example.com/images/01JD8Z6Q4M3V9K2T7XW5R8N0PB?w=800&fmt=webp" -o out.webp
```

Without query parameters, this returns the stored original. With
[transform parameters](#transforms), the first request renders the variant and
caches it in the bucket. Later requests are served from that cache.

Images never change once uploaded, so responses are cacheable. Signed-URL
responses are `public` until the URL expires, so a CDN in front of ansel
caches them but never serves them past their expiry. Responses to
token-authenticated requests are `private`. With `fmt=auto`, responses also
send `Vary: Accept`.

### `GET /images/{id}/info`: metadata

Returns the same JSON as the upload response.

### `DELETE /images/{id}`

Deletes the original and every cached variant. Returns `204`.

### `POST /convert`: stateless transform

This endpoint takes the same body as `POST /images` and the same query
parameters as `GET /images/{id}`. It returns the transformed image and stores
nothing. Use it for one-off conversions, such as PNG to WebP in a build step.

```sh
curl -X POST "https://ansel.example.com/convert?fmt=avif&q=60" \
  -H "Authorization: Bearer $ANSEL_API_TOKEN" \
  --data-binary @logo.png -o logo.avif
```

### `GET /health`

Liveness check that needs no authentication. Returns `ok`.

### Errors

Errors return `{ "error": "..." }` with one of these status codes:

| Status | Meaning                                                                  |
| ------ | ------------------------------------------------------------------------ |
| `400`  | Invalid transform parameters.                                            |
| `401`  | Missing or wrong token, or a bad or expired signature.                   |
| `404`  | No image with that ID.                                                   |
| `413`  | Upload is larger than `ANSEL_MAX_UPLOAD_BYTES` or `ANSEL_MAX_PIXELS`.    |
| `415`  | Not an image, or a format that isn't allowed.                            |
| `422`  | The file claims to be an image but can't be decoded.                     |
| `502`  | The storage backend failed.                                              |
| `503`  | The request took longer than `ANSEL_REQUEST_TIMEOUT`.                    |

## API documentation

While the server is running, Swagger documentation is available at
<http://localhost:8080/swagger-ui/>, and the OpenAPI document is at
<http://localhost:8080/api-docs/openapi.json>. Click **Authorize** and enter
your `ANSEL_API_TOKEN` to try requests.

## Transforms

Transforms are query parameters on `GET /images/{id}` and `POST /convert`.
They run in this order: **crop → resize → rotate/flip → adjust → encode**.

| Param    | Values                                     | Default         | Notes                                                                     |
| -------- | ------------------------------------------ | --------------- | ------------------------------------------------------------------------- |
| `w`, `h` | `1`–`ANSEL_MAX_OUTPUT_DIMENSION`           | —               | Target size. Give one to keep the aspect ratio.                           |
| `fit`    | `inside`, `contain`, `cover`, `fill`       | `inside`        | `inside` fits the box without upscaling; `contain` may upscale; `cover` fills the box and crops; `fill` stretches. |
| `gravity`| `center`, `n`, `s`, `e`, `w`, `ne`, …      | `center`        | Which part to keep when `fit=cover` crops.                                |
| `crop`   | `x,y,w,h`                                  | —               | Crop in source pixels before resizing.                                    |
| `rot`    | `90`, `180`, `270`                         | —               | Rotation, applied after EXIF auto-orientation.                            |
| `flip`   | `h`, `v`, `hv`                             | —               | Mirror horizontally or vertically.                                        |
| `blur`   | `0.1`–`100`                                | —               | Gaussian blur sigma.                                                      |
| `gray`   | `true`                                     | —               | Convert to grayscale.                                                     |
| `fmt`    | `jpeg`, `png`, `webp`, `avif`, `gif`, `auto` | source format | `auto` chooses AVIF or WebP from the request's `Accept` header.           |
| `q`      | `1`–`100`                                  | `82`            | Quality for lossy formats (JPEG, WebP, AVIF). PNG and GIF ignore it.      |
| `preset` | a name from `ANSEL_PRESETS`                | —               | A named set of the parameters above. Can't be combined with them.         |

**Input formats:** JPEG, PNG, WebP, and GIF by default; TIFF and BMP can be
enabled with `ANSEL_ALLOWED_FORMATS`. Animated GIF and WebP uploads keep only
their first frame. AVIF is an output format only (decoding it would need a
native library). Ansel never accepts SVG, because SVG files can carry scripts
(see [Security](#security)).

**Output formats:** JPEG, PNG, WebP, GIF, and AVIF. AVIF needs the `avif` Cargo
feature, which the Docker image enables.

## Security

Every default below can be loosened with an environment variable. You
shouldn't need to change any of them for a typical deployment.

**Authentication**

- **The token is required, and must be strong.** The server won't start
  without an `ANSEL_API_TOKEN` of at least 32 characters. It compares tokens
  in constant time.
- **Image URLs are signed.** A `GET /images/{id}` request without a token must
  include `exp` and `sig`. `sig` is an HMAC-SHA256 over the image ID, the
  transform parameters, and the expiry time. Without it, anyone could request
  `?w=4999&h=4999&blur=100` thousands of times to burn your CPU and fill your
  bucket with variants. By default, the signing key is derived from
  `ANSEL_API_TOKEN`, so you only manage one secret. Set `ANSEL_SIGNING_KEY` to
  rotate the signing key separately.
- **Public reads are opt-in.** `ANSEL_PUBLIC_READS=true` serves originals,
  and the named transforms in `ANSEL_PRESETS`, without a signature. Any other
  transform still needs one, so the public can't make the server render
  arbitrary variants.
- **Signed URLs can't be stretched.** Unknown or repeated query parameters
  are rejected, and any parameter added to a signed URL invalidates it.

**Upload checks**

- **Ansel checks file contents.** It detects the format from the file's magic
  bytes, never from the filename or `Content-Type`. It rejects anything outside
  `ANSEL_ALLOWED_FORMATS`.
- **No SVG, ever.** SVG can contain JavaScript, and an SVG served from your
  domain is a stored-XSS risk.
- **Decompression bombs are rejected.** Ansel reads the image dimensions from
  the header and rejects the upload before decoding if it exceeds
  `ANSEL_MAX_PIXELS`. It also limits upload size and decoder memory.
- **Metadata is removed.** Every stored image is re-encoded, which drops EXIF,
  XMP, IPTC, and GPS data. Users' photos don't leak their home location.
  Ansel applies the orientation tag before removing it, so images still
  display upright.
- **Re-encoding removes hidden content.** Polyglot files, data appended after
  the image, and malformed chunks don't survive decoding and re-encoding.

**Storage and responses**

- **The server generates object keys.** Stored images are named with random
  ULIDs, and the client never supplies a path. Users can't overwrite each
  other's files, traverse directories, or guess IDs.
- **Ansel never downloads remote URLs.** There is no "fetch from URL"
  endpoint, so there is no risk of server-side request forgery.
- **Response headers are hardened.** Every response sets
  `X-Content-Type-Options: nosniff`,
  `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'; sandbox`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, and an exact
  `Content-Type`. CORS is off unless you list origins.
- **Errors don't leak internals.** Storage failures are logged in full but
  reported to clients as a generic `502`.
- **Resources are limited.** Request timeouts, a concurrency cap on image
  processing (`ANSEL_MAX_CONCURRENCY`), and a maximum output size keep one
  client from starving the others.
- **The container is minimal.** The Docker image runs as a non-root user on
  a distroless base, with no shell. With bucket storage it never writes to
  disk, so it can run with a read-only filesystem.

## Configuration

Everything is configured through environment variables. For local
development, put them in a `.env` file. `dev.sh` creates a template on first
run, and `.env` is gitignored.

### Required

| Variable          | Purpose                                                                           |
| ----------------- | --------------------------------------------------------------------------------- |
| `ANSEL_API_TOKEN` | Shared secret for the API, at least 32 characters. Also the default signing key.  |

### Storage

Ansel works with any S3-compatible store. It reads Railway's bucket variable
names first, then the standard AWS names. Most deployments don't need to set
any of these by hand.

| Variable            | Fallback                                    | Default                    | Purpose                                                             |
| ------------------- | ------------------------------------------- | -------------------------- | ------------------------------------------------------------------- |
| `BUCKET`            | `AWS_S3_BUCKET_NAME`, `AWS_BUCKET`          | —                          | Bucket name. Required unless `ANSEL_STORAGE=fs`.                    |
| `ENDPOINT`          | `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL`   | AWS S3                     | S3 endpoint URL.                                                    |
| `REGION`            | `AWS_REGION`, `AWS_DEFAULT_REGION`          | `auto` (`us-east-1` for AWS) | Region used for request signing.                                  |
| `ACCESS_KEY_ID`     | `AWS_ACCESS_KEY_ID`                         | —                          | Access key.                                                         |
| `SECRET_ACCESS_KEY` | `AWS_SECRET_ACCESS_KEY`                     | —                          | Secret key.                                                         |
| `ANSEL_S3_PATH_STYLE` | —                                         | `false`                    | Use path-style URLs. MinIO usually needs this; Railway doesn't.     |
| `ANSEL_STORAGE`     | —                                           | `s3`                       | `s3`, or `fs` to store files on local disk for development.         |
| `ANSEL_FS_ROOT`     | —                                           | `./data`                   | Directory used when `ANSEL_STORAGE=fs`.                             |
| `ANSEL_KEY_PREFIX`  | —                                           | `ansel/`                   | Prefix for every object key, so ansel can share a bucket.           |

### Limits and behavior

| Variable                     | Default                         | Purpose                                                                      |
| ---------------------------- | ------------------------------- | ---------------------------------------------------------------------------- |
| `ANSEL_MAX_UPLOAD_BYTES`     | `20971520` (20 MiB)             | Largest request body accepted.                                               |
| `ANSEL_MAX_PIXELS`           | `50000000` (50 MP)              | Largest decoded image (width × height). Protects against decompression bombs. |
| `ANSEL_MAX_OUTPUT_DIMENSION` | `4096`                          | Largest `w` or `h` a transform can produce (at most `16383`).                |
| `ANSEL_ALLOWED_FORMATS`      | `jpeg,png,webp,gif`             | Input formats accepted on upload. `tiff` and `bmp` can be added.             |
| `ANSEL_DEFAULT_QUALITY`      | `82`                            | Quality used when a transform doesn't set `q`.                               |
| `ANSEL_STORE_FORMAT`         | `original`                      | Format originals are stored in: `original` or `webp`. TIFF and BMP are stored as PNG. |
| `ANSEL_STRIP_METADATA`       | `true`                          | Re-encode uploads, removing EXIF, XMP, IPTC, and GPS data. When `false`, files are stored byte for byte (they're still fully decoded to validate them). |
| `ANSEL_SIGNING_KEY`          | derived from `ANSEL_API_TOKEN`  | Separate HMAC key for signed URLs, at least 32 characters.                   |
| `ANSEL_PUBLIC_READS`         | `false`                         | Serve originals and presets without a signature.                             |
| `ANSEL_PRESETS`              | —                               | Named transforms, e.g. `thumb=w:200,h:200,fit:cover;hero=w:1600,fmt:auto`. Request them with `?preset=thumb`. |
| `ANSEL_CACHE_VARIANTS`       | `true`                          | Store rendered variants in the bucket so they are only rendered once.        |
| `ANSEL_MAX_CONCURRENCY`      | number of CPUs                  | Maximum images processed at the same time. Extra requests wait in a queue.   |
| `ANSEL_REQUEST_TIMEOUT`      | `30`                            | Seconds before a request is aborted with `503`.                              |
| `ANSEL_CORS_ORIGINS`         | —                               | Comma-separated origins allowed for browser uploads. CORS is off when unset. |
| `PORT`                       | `8080`                          | Port the HTTP server listens on. Railway sets this automatically.            |
| `RUST_LOG`                   | `ansel=info`                    | Log filter.                                                                  |

### Client SDK

| Variable          | Purpose                                                                                 |
| ----------------- | --------------------------------------------------------------------------------------- |
| `ANSEL_URL`       | Base URL of your ansel server.                                                          |
| `ANSEL_API_TOKEN` | The same token the server uses. The SDK also derives the URL signing key from it.       |
| `ANSEL_SIGNING_KEY` | Set this only if the server sets it.                                                  |
| `ANSEL_URL_TTL`   | Lifetime of signed URLs in seconds. Defaults to `604800` (7 days).                      |

## Cargo features

Ansel keeps its dependency list short. The client is `reqwest`, `serde`,
`serde_json`, and `hmac`/`sha2` for signing URLs. The server uses
[`rusty-s3`](https://github.com/paolobarbolini/rusty-s3), which only signs
requests and sends them with the same `reqwest` client, instead of the full
AWS SDK. Image IDs, timestamps, and hex encoding are written by hand rather
than pulled in as crates.

| Feature  | Default | Purpose                                                                                     |
| -------- | ------- | ------------------------------------------------------------------------------------------- |
| `client` | yes     | The HTTP client SDK.                                                                        |
| `server` | no      | The ansel server binary. Adds `axum`, `tokio`, `tower-http`, `tracing`, `dotenvy`, `image`, `rusty-s3`, `getrandom`, and `webp`. |
| `avif`   | no      | AVIF output through `ravif`. It's a separate feature because it adds build time. The Docker image enables it. |

The `webp` crate builds libwebp from source for quality-controlled (lossy)
WebP, so building the server needs a C compiler. The `image` crate alone can
only write lossless WebP.

Run the server locally with:

```sh
cargo run --no-default-features --features server
```

The Dockerfile and `dev.sh` already pass these flags.

## Development

```sh
./dev.sh
```

This creates a `.env` file with a fresh `ANSEL_API_TOKEN` if one doesn't
exist, sets `ANSEL_STORAGE=fs` so you don't need a bucket, and runs the server
under [`cargo watch`](https://crates.io/crates/cargo-watch), which rebuilds on
every change.

To test against a real S3 API locally, start MinIO:

```sh
docker run -p 9000:9000 minio/minio server /data
# BUCKET=ansel ENDPOINT=http://localhost:9000 ANSEL_S3_PATH_STYLE=true ...
```

Run the tests. The server tests start a real server on a random port and
drive it through the SDK:

```sh
cargo test --features server          # quick
cargo test --all-features             # also AVIF; slower to build
```

## Deploying elsewhere

Ansel ships as a Docker image. Set the variables above in your platform's
secrets configuration:

```sh
docker build -t ansel .
docker run -p 8080:8080 --env-file .env --read-only ansel
```

It works with AWS S3, Cloudflare R2, Backblaze B2, MinIO, Tigris, and any
other S3-compatible store.

## Publishing the SDK

The client SDK is published to [crates.io](https://crates.io/crates/ansel), so
other services can `cargo add ansel`. Push a `v*` tag and the
[`release` workflow](.github/workflows/release.yml) runs `cargo publish`. The
workflow needs a `CARGO_REGISTRY_TOKEN` repository secret. See
[PUBLISHING.md](PUBLISHING.md) for the full checklist.

## License

[MIT](LICENSE)
