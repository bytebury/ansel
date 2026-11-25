//! Client SDK for [ansel](https://github.com/bytebury/ansel), an image upload,
//! editing, and conversion microservice.
//!
//! ```no_run
//! use ansel::{Fit, Format, Transform};
//!
//! # async fn run() -> ansel::Result<()> {
//! ansel::setup("https://ansel.example.com", "super-secret-token")?;
//!
//! let image = ansel::upload(std::fs::read("photo.jpg").unwrap()).await?;
//! let src = ansel::url(
//!     &image.id,
//!     Transform::new().width(400).height(400).fit(Fit::Cover).format(Format::Auto),
//! )?;
//! # Ok(()) }
//! ```
//!
//! Every function also has a macro shorthand, such as [`upload!`] and
//! [`convert!`].

#![forbid(unsafe_code)]

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
mod macros;
pub mod signing;
mod transform;

#[cfg(feature = "client")]
pub use client::{
    Client, DEFAULT_URL_TTL, Error, Image, Result, client, convert, delete, download, get, install,
    setup, upload, url,
};
pub use transform::{
    Crop, Fit, Flip, Format, Gravity, ImageInfo, Rotation, Transform, is_valid_id,
    is_valid_preset_name,
};
