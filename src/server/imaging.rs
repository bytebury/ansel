//! Decoding, transforming, and encoding images.
//!
//! Everything here is CPU-bound and runs on blocking threads.

use std::io::Cursor;

use ansel::{Crop, Fit, Flip, Format, Rotation, Transform};
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    DynamicImage, GenericImageView, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits,
};

use super::error::ApiError;

/// Formats a stored original can be in. Used when decoding stored images,
/// which were already validated on upload.
pub const STORED_FORMATS: &[ImageFormat] = &[
    ImageFormat::Jpeg,
    ImageFormat::Png,
    ImageFormat::WebP,
    ImageFormat::Gif,
];

/// Detects the format from magic bytes. Filenames and `Content-Type` are never
/// consulted.
pub fn sniff(bytes: &[u8], allowed: &[ImageFormat]) -> Result<ImageFormat, ApiError> {
    let format = image::guess_format(bytes)
        .map_err(|_| ApiError::unsupported("not a supported image format"))?;
    if allowed.contains(&format) {
        Ok(format)
    } else {
        Err(ApiError::unsupported(format!(
            "{} images aren't allowed on this server",
            format
                .extensions_str()
                .first()
                .copied()
                .unwrap_or("this kind of")
        )))
    }
}

/// Decodes an image within the configured limits and applies its EXIF
/// orientation, so the pixels are upright.
pub fn decode(
    bytes: &[u8],
    allowed: &[ImageFormat],
    max_pixels: u64,
) -> Result<(DynamicImage, ImageFormat), ApiError> {
    let format = sniff(bytes, allowed)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    // Room for the largest allowed image at 16 bytes per pixel (RGBA, 32-bit
    // float), with a 64 MiB floor for decoder buffers.
    limits.max_alloc = Some(max_pixels.saturating_mul(16).max(64 * 1024 * 1024));
    reader.limits(limits);

    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    // Check dimensions from the header before decoding any pixel data, so a
    // tiny file that claims to be 100,000 × 100,000 is rejected immediately.
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(ApiError::too_large(format!(
            "image is {w}×{h}, larger than the {max_pixels}-pixel limit"
        )));
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    image.apply_orientation(orientation);
    Ok((image, format))
}

fn decode_error(e: ImageError) -> ApiError {
    match e {
        ImageError::Limits(_) => ApiError::too_large("image exceeds the decoding limits"),
        ImageError::Unsupported(_) => ApiError::unsupported("this image variant isn't supported"),
        _ => ApiError::unprocessable("image could not be decoded"),
    }
}

/// Maps a decoded input format to the format it's stored and served as.
pub fn output_format(format: ImageFormat) -> Format {
    match format {
        ImageFormat::Jpeg => Format::Jpeg,
        ImageFormat::WebP => Format::WebP,
        ImageFormat::Gif => Format::Gif,
        // PNG, TIFF, BMP: lossless and widely supported.
        _ => Format::Png,
    }
}

/// Applies a transform: crop → resize → rotate/flip → blur → grayscale.
pub fn apply(
    mut image: DynamicImage,
    t: &Transform,
    max_dimension: u32,
) -> Result<DynamicImage, ApiError> {
    if let Some(Crop {
        x,
        y,
        width,
        height,
    }) = t.crop
    {
        let (iw, ih) = image.dimensions();
        if u64::from(x) + u64::from(width) > u64::from(iw)
            || u64::from(y) + u64::from(height) > u64::from(ih)
        {
            return Err(ApiError::bad_request(format!(
                "crop is outside the {iw}×{ih} image"
            )));
        }
        image = image.crop_imm(x, y, width, height);
    }

    if t.width.is_some() || t.height.is_some() {
        image = resize(image, t, max_dimension);
    }

    image = match t.rotate {
        Some(Rotation::Deg90) => image.rotate90(),
        Some(Rotation::Deg180) => image.rotate180(),
        Some(Rotation::Deg270) => image.rotate270(),
        None => image,
    };
    image = match t.flip {
        Some(Flip::Horizontal) => image.fliph(),
        Some(Flip::Vertical) => image.flipv(),
        Some(Flip::Both) => image.fliph().flipv(),
        None => image,
    };
    if let Some(sigma) = t.blur {
        image = image.fast_blur(sigma);
    }
    if t.grayscale {
        image = image.grayscale();
    }
    Ok(image)
}

fn resize(image: DynamicImage, t: &Transform, max_dimension: u32) -> DynamicImage {
    let (sw, sh) = image.dimensions();
    let fit = t.fit.unwrap_or_default();

    if let (Fit::Cover, Some(w), Some(h)) = (fit, t.width, t.height) {
        // Crop the source to the target aspect ratio first, then resize. This
        // never builds an intermediate image larger than the output.
        let target = f64::from(w) / f64::from(h);
        let (cw, ch) = if f64::from(sw) / f64::from(sh) > target {
            (((f64::from(sh) * target).round() as u32).clamp(1, sw), sh)
        } else {
            (sw, ((f64::from(sw) / target).round() as u32).clamp(1, sh))
        };
        let (gx, gy) = t.gravity.unwrap_or_default().anchor();
        let x = (f64::from(sw - cw) * gx).round() as u32;
        let y = (f64::from(sh - ch) * gy).round() as u32;
        return image
            .crop_imm(x, y, cw, ch)
            .resize_exact(w, h, FilterType::Lanczos3);
    }

    let (tw, th) = match (fit, t.width, t.height) {
        (Fit::Fill, Some(w), Some(h)) => (w, h),
        _ => {
            let sx = t
                .width
                .map_or(f64::INFINITY, |w| f64::from(w) / f64::from(sw));
            let sy = t
                .height
                .map_or(f64::INFINITY, |h| f64::from(h) / f64::from(sh));
            let mut scale = sx.min(sy);
            if fit == Fit::Inside {
                scale = scale.min(1.0);
            }
            scaled(sw, sh, scale)
        }
    };
    // A one-sided resize of a very narrow image can derive a huge other side.
    let (tw, th) = if tw > max_dimension || th > max_dimension {
        let scale = (f64::from(max_dimension) / f64::from(tw))
            .min(f64::from(max_dimension) / f64::from(th));
        scaled(tw, th, scale)
    } else {
        (tw, th)
    };

    if (tw, th) == (sw, sh) {
        image
    } else {
        image.resize_exact(tw, th, FilterType::Lanczos3)
    }
}

fn scaled(w: u32, h: u32, scale: f64) -> (u32, u32) {
    let s = |v: u32| ((f64::from(v) * scale).round().max(1.0)).min(f64::from(u32::MAX)) as u32;
    (s(w), s(h))
}

/// Encodes an image. Encoders never write EXIF, XMP, or other metadata, so
/// the output carries pixels only.
pub fn encode(image: &DynamicImage, format: Format, quality: u8) -> Result<Vec<u8>, ApiError> {
    let mut out = Vec::new();
    let result = match format {
        Format::Jpeg => {
            let rgb = flatten(image);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb)
        }
        Format::Png => to_8bit(image).write_to(&mut Cursor::new(&mut out), ImageFormat::Png),
        Format::Gif => DynamicImage::ImageRgba8(image.to_rgba8())
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Gif),
        Format::WebP => return encode_webp(image, quality),
        Format::Avif => return encode_avif(image, quality),
        Format::Auto => return Err(ApiError::internal("`auto` reached the encoder unresolved")),
    };
    result.map_err(|e| ApiError::unprocessable(format!("couldn't encode as {format}: {e}")))?;
    Ok(out)
}

fn encode_webp(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, ApiError> {
    let (w, h) = image.dimensions();
    if w > 16_383 || h > 16_383 {
        return Err(ApiError::bad_request(
            "WebP images can't be larger than 16383×16383",
        ));
    }
    let memory = if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        webp::Encoder::from_rgba(&rgba, w, h).encode_simple(false, f32::from(quality))
    } else {
        let rgb = image.to_rgb8();
        webp::Encoder::from_rgb(&rgb, w, h).encode_simple(false, f32::from(quality))
    };
    memory
        .map(|m| m.to_vec())
        .map_err(|e| ApiError::unprocessable(format!("couldn't encode as webp: {e:?}")))
}

#[cfg(feature = "avif")]
fn encode_avif(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, ApiError> {
    let mut out = Vec::new();
    // Speed 8 of 10: a good balance of size and latency for on-the-fly use.
    let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut out, 8, quality);
    let rgba = DynamicImage::ImageRgba8(image.to_rgba8());
    rgba.write_with_encoder(encoder)
        .map_err(|e| ApiError::unprocessable(format!("couldn't encode as avif: {e}")))?;
    Ok(out)
}

#[cfg(not(feature = "avif"))]
fn encode_avif(_: &DynamicImage, _: u8) -> Result<Vec<u8>, ApiError> {
    Err(ApiError::bad_request(
        "AVIF output isn't enabled on this server",
    ))
}

/// RGB with any transparency composited onto white (JPEG has no alpha).
fn flatten(image: &DynamicImage) -> image::RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let a = u16::from(a);
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}

/// Converts float and 16-bit images to 8-bit, keeping alpha when present.
fn to_8bit(image: &DynamicImage) -> DynamicImage {
    match image {
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageRgb8(_)
        | DynamicImage::ImageRgba8(_) => image.clone(),
        _ if image.color().has_alpha() => DynamicImage::ImageRgba8(image.to_rgba8()),
        _ => DynamicImage::ImageRgb8(image.to_rgb8()),
    }
}

/// Rewrites a PNG's IHDR dimensions and fixes its checksum, producing a
/// valid header that claims a huge image.
#[cfg(test)]
pub fn png_with_dimensions(mut png: Vec<u8>, w: u32, h: u32) -> Vec<u8> {
    png[16..20].copy_from_slice(&w.to_be_bytes());
    png[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = png[12..29].iter().fold(0xFFFF_FFFFu32, |mut crc, &byte| {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
        crc
    }) ^ 0xFFFF_FFFF;
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::new(w, h))
    }

    fn dims(t: Transform, w: u32, h: u32) -> (u32, u32) {
        apply(img(w, h), &t, 4096).unwrap().dimensions()
    }

    #[test]
    fn fits() {
        assert_eq!(dims(Transform::new().width(100), 400, 200), (100, 50));
        assert_eq!(dims(Transform::new().height(100), 400, 200), (200, 100));
        // Inside never upscales; contain does.
        assert_eq!(dims(Transform::new().width(800), 400, 200), (400, 200));
        assert_eq!(
            dims(Transform::new().width(800).fit(Fit::Contain), 400, 200),
            (800, 400)
        );
        assert_eq!(dims(Transform::new().size(100, 100), 400, 200), (100, 50));
        assert_eq!(
            dims(Transform::new().size(100, 100).fit(Fit::Cover), 400, 200),
            (100, 100)
        );
        assert_eq!(
            dims(Transform::new().size(100, 30).fit(Fit::Fill), 400, 200),
            (100, 30)
        );
        assert_eq!(
            dims(Transform::new().crop(10, 10, 50, 20), 400, 200),
            (50, 20)
        );
        assert_eq!(
            dims(Transform::new().rotate(Rotation::Deg90), 400, 200),
            (200, 400)
        );
    }

    #[test]
    fn derived_side_is_capped() {
        let (w, h) = dims(Transform::new().width(4000).fit(Fit::Contain), 10, 1000);
        assert!(w <= 4096 && h <= 4096, "{w}×{h}");
    }

    #[test]
    fn crop_outside_is_rejected() {
        assert!(apply(img(10, 10), &Transform::new().crop(5, 5, 10, 10), 4096).is_err());
    }

    #[test]
    fn rejects_svg_and_garbage() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        assert_eq!(sniff(svg, STORED_FORMATS).unwrap_err().status, 415);
        assert_eq!(sniff(b"hello", STORED_FORMATS).unwrap_err().status, 415);
    }

    #[test]
    fn rejects_huge_dimensions_before_decoding() {
        // A valid PNG header claiming 50000×50000 pixels, with no pixel data.
        let mut png = Vec::new();
        img(1, 1)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let png = png_with_dimensions(png, 50_000, 50_000);
        let err = decode(&png, &[ImageFormat::Png], 50_000_000).unwrap_err();
        assert_eq!(err.status, 413);
    }

    #[test]
    fn encodes_every_format() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8));
        for format in [Format::Jpeg, Format::Png, Format::WebP, Format::Gif] {
            let bytes = encode(&image, format, 80).unwrap();
            let (decoded, _) = decode(&bytes, STORED_FORMATS, 1_000).unwrap();
            assert_eq!(decoded.dimensions(), (8, 8), "{format}");
        }
    }
}
