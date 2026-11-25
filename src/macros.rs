//! Shorthand macros for the global client.
//!
//! Each macro awaits the matching free function, so it must be used inside an
//! `async` block or function. It returns the same `Result`, so add `?` to it.
//!
//! Transform parameters are written as `name = value` pairs. Each name is a
//! [`Transform`](crate::Transform) setter: `width = 400` calls `.width(400)`.

/// Uploads an image with the global client.
///
/// ```no_run
/// # async fn run() -> ansel::Result<()> {
/// let image = ansel::upload!(std::fs::read("photo.jpg").unwrap())?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! upload {
    ($bytes:expr $(,)?) => {
        $crate::upload($bytes).await
    };
}

/// Fetches an image's metadata with the global client.
///
/// ```no_run
/// # async fn run(id: String) -> ansel::Result<()> {
/// let info = ansel::get!(id)?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! get {
    ($id:expr $(,)?) => {
        $crate::get(&$id).await
    };
}

/// Deletes an image and all of its cached variants with the global client.
///
/// ```no_run
/// # async fn run(id: String) -> ansel::Result<()> {
/// ansel::delete!(id)?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! delete {
    ($id:expr $(,)?) => {
        $crate::delete(&$id).await
    };
}

/// Transforms an image without storing it.
///
/// ```no_run
/// use ansel::Format;
/// # async fn run(png: Vec<u8>) -> ansel::Result<()> {
/// let webp = ansel::convert!(png, format = Format::WebP, quality = 80)?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! convert {
    ($bytes:expr $(, $param:ident = $value:expr)* $(,)?) => {
        $crate::convert($bytes, $crate::Transform::new()$(.$param($value))*).await
    };
}

/// Downloads an image, optionally transformed.
///
/// ```no_run
/// # async fn run(id: String) -> ansel::Result<()> {
/// let thumb = ansel::download!(id, width = 200, height = 200)?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! download {
    ($id:expr $(, $param:ident = $value:expr)* $(,)?) => {
        $crate::download(&$id, $crate::Transform::new()$(.$param($value))*).await
    };
}

/// Builds a signed image URL. Unlike the other macros this doesn't await,
/// because no network call is made.
///
/// ```no_run
/// use ansel::{Fit, Format};
/// # fn run(id: String) -> ansel::Result<()> {
/// let src = ansel::url!(id, width = 400, height = 400, fit = Fit::Cover, format = Format::Auto)?;
/// # Ok(()) }
/// ```
#[macro_export]
macro_rules! url {
    ($id:expr $(, $param:ident = $value:expr)* $(,)?) => {
        $crate::url(&$id, $crate::Transform::new()$(.$param($value))*)
    };
}
