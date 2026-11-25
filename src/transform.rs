//! Transform parameters shared by the client SDK and the server.
//!
//! A [`Transform`] serializes to a *canonical* query string: parameters always
//! appear in the same order and with the same spelling. URL signatures are
//! computed over that canonical form, so the client and the server agree on
//! what was signed no matter how the query was written on the wire.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// An image format.
///
/// [`Format::Auto`] is only meaningful as a transform output: the server picks
/// AVIF or WebP from the request's `Accept` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Jpeg,
    Png,
    #[serde(rename = "webp")]
    WebP,
    Gif,
    Avif,
    Auto,
}

impl Format {
    /// The lowercase name used in query strings and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Avif => "avif",
            Self::Auto => "auto",
        }
    }

    /// The MIME type of this format. `Auto` has none and returns
    /// `application/octet-stream`.
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Avif => "image/avif",
            Self::Auto => "application/octet-stream",
        }
    }

    /// Whether the format takes a quality setting.
    pub const fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::WebP | Self::Avif)
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "jpeg" | "jpg" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::WebP,
            "gif" => Self::Gif,
            "avif" => Self::Avif,
            "auto" => Self::Auto,
            _ => return Err(format!("unknown format `{s}`")),
        })
    }
}

/// How an image is fitted into the `w` × `h` box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Fit {
    /// Fit inside the box, keeping the aspect ratio. Never upscales.
    #[default]
    Inside,
    /// Fit inside the box, keeping the aspect ratio. May upscale.
    Contain,
    /// Fill the box, keeping the aspect ratio, and crop the overflow.
    Cover,
    /// Stretch to exactly the box, ignoring the aspect ratio.
    Fill,
}

/// Which part of the image to keep when [`Fit::Cover`] crops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Gravity {
    #[default]
    Center,
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

impl Gravity {
    /// Horizontal and vertical position, each 0.0 (start), 0.5, or 1.0 (end).
    pub const fn anchor(self) -> (f64, f64) {
        match self {
            Self::Center => (0.5, 0.5),
            Self::North => (0.5, 0.0),
            Self::South => (0.5, 1.0),
            Self::East => (1.0, 0.5),
            Self::West => (0.0, 0.5),
            Self::NorthEast => (1.0, 0.0),
            Self::NorthWest => (0.0, 0.0),
            Self::SouthEast => (1.0, 1.0),
            Self::SouthWest => (0.0, 1.0),
        }
    }
}

/// A clockwise rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rotation {
    Deg90,
    Deg180,
    Deg270,
}

/// A mirror operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flip {
    Horizontal,
    Vertical,
    Both,
}

/// A crop rectangle, in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

macro_rules! str_enum {
    ($ty:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            /// The name used in query strings.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $ty {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    _ => Err(format!(concat!("invalid ", stringify!($ty), " `{}`"), s)),
                }
            }
        }
    };
}

str_enum!(Fit { Inside => "inside", Contain => "contain", Cover => "cover", Fill => "fill" });
str_enum!(Gravity {
    Center => "center", North => "n", South => "s", East => "e", West => "w",
    NorthEast => "ne", NorthWest => "nw", SouthEast => "se", SouthWest => "sw",
});
str_enum!(Rotation { Deg90 => "90", Deg180 => "180", Deg270 => "270" });
str_enum!(Flip { Horizontal => "h", Vertical => "v", Both => "hv" });

/// A set of edits to apply to an image.
///
/// Build one with the chained setters, then pass it to
/// [`url`](crate::url) or [`convert`](crate::convert):
///
/// ```
/// use ansel::{Fit, Format, Transform};
///
/// let t = Transform::new().width(400).height(400).fit(Fit::Cover).format(Format::WebP);
/// assert_eq!(t.to_query(), "w=400&h=400&fit=cover&fmt=webp");
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transform {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Option<Fit>,
    pub gravity: Option<Gravity>,
    pub crop: Option<Crop>,
    pub rotate: Option<Rotation>,
    pub flip: Option<Flip>,
    pub blur: Option<f32>,
    pub grayscale: bool,
    pub format: Option<Format>,
    pub quality: Option<u8>,
    pub preset: Option<String>,
    expires_in: Option<Duration>,
}

impl Transform {
    /// An empty transform: the image is returned as stored.
    pub fn new() -> Self {
        Self::default()
    }

    /// Target width in pixels.
    pub fn width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }

    /// Target height in pixels.
    pub fn height(mut self, height: u32) -> Self {
        self.height = Some(height);
        self
    }

    /// Target width and height in pixels.
    pub fn size(self, width: u32, height: u32) -> Self {
        self.width(width).height(height)
    }

    /// How the image fits the `width` × `height` box. Defaults to [`Fit::Inside`].
    pub fn fit(mut self, fit: Fit) -> Self {
        self.fit = Some(fit);
        self
    }

    /// Which part of the image [`Fit::Cover`] keeps. Defaults to [`Gravity::Center`].
    pub fn gravity(mut self, gravity: Gravity) -> Self {
        self.gravity = Some(gravity);
        self
    }

    /// Crop to a rectangle (in source pixels) before resizing.
    pub fn crop(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.crop = Some(Crop {
            x,
            y,
            width,
            height,
        });
        self
    }

    /// Rotate clockwise, after EXIF auto-orientation.
    pub fn rotate(mut self, rotation: Rotation) -> Self {
        self.rotate = Some(rotation);
        self
    }

    /// Mirror the image.
    pub fn flip(mut self, flip: Flip) -> Self {
        self.flip = Some(flip);
        self
    }

    /// Gaussian blur with the given sigma (0.1–100).
    pub fn blur(mut self, sigma: f32) -> Self {
        self.blur = Some(sigma);
        self
    }

    /// Convert to grayscale.
    pub fn grayscale(mut self, grayscale: bool) -> Self {
        self.grayscale = grayscale;
        self
    }

    /// Output format. Defaults to the stored format.
    pub fn format(mut self, format: Format) -> Self {
        self.format = Some(format);
        self
    }

    /// Quality for lossy formats (1–100).
    pub fn quality(mut self, quality: u8) -> Self {
        self.quality = Some(quality);
        self
    }

    /// Use a named preset from the server's `ANSEL_PRESETS`. A preset can't be
    /// combined with other parameters.
    pub fn preset(mut self, name: impl Into<String>) -> Self {
        self.preset = Some(name.into());
        self
    }

    /// How long a signed URL built from this transform stays valid. Only used
    /// by [`url`](crate::url); it isn't part of the transform itself.
    pub fn expires_in(mut self, duration: Duration) -> Self {
        self.expires_in = Some(duration);
        self
    }

    /// The lifetime set with [`Transform::expires_in`], if any.
    pub fn expiry(&self) -> Option<Duration> {
        self.expires_in
    }

    /// True when no parameter is set, so the stored image is returned as is.
    pub fn is_empty(&self) -> bool {
        self.to_query().is_empty()
    }

    /// The canonical query string, without a leading `?`.
    pub fn to_query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut push = |k: &str, v: String| parts.push(format!("{k}={v}"));
        if let Some(w) = self.width {
            push("w", w.to_string());
        }
        if let Some(h) = self.height {
            push("h", h.to_string());
        }
        if let Some(fit) = self.fit {
            push("fit", fit.to_string());
        }
        if let Some(g) = self.gravity {
            push("gravity", g.to_string());
        }
        if let Some(c) = self.crop {
            push("crop", format!("{},{},{},{}", c.x, c.y, c.width, c.height));
        }
        if let Some(r) = self.rotate {
            push("rot", r.to_string());
        }
        if let Some(f) = self.flip {
            push("flip", f.to_string());
        }
        if let Some(b) = self.blur {
            push("blur", b.to_string());
        }
        if self.grayscale {
            push("gray", "true".into());
        }
        if let Some(f) = self.format {
            push("fmt", f.to_string());
        }
        if let Some(q) = self.quality {
            push("q", q.to_string());
        }
        if let Some(p) = &self.preset {
            push("preset", p.clone());
        }
        parts.join("&")
    }

    /// Parses and validates a transform query string, also returning the
    /// signature parameters (`exp`, `sig`) when present.
    ///
    /// Unknown or repeated parameters are rejected rather than ignored, so a
    /// signed URL can't be stretched with extra parameters.
    #[doc(hidden)]
    pub fn parse_query(query: &str) -> Result<(Self, Option<u64>, Option<String>), String> {
        let mut t = Self::new();
        let mut exp = None;
        let mut sig = None;
        let mut seen: Vec<String> = Vec::new();

        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(raw_key)?;
            let value = percent_decode(raw_value)?;
            if seen.contains(&key) {
                return Err(format!("parameter `{key}` is repeated"));
            }
            seen.push(key.clone());

            let v = value.as_str();
            match key.as_str() {
                "w" => t.width = Some(parse_dimension("w", v)?),
                "h" => t.height = Some(parse_dimension("h", v)?),
                "fit" => t.fit = Some(v.parse()?),
                "gravity" => t.gravity = Some(v.parse()?),
                "crop" => t.crop = Some(parse_crop(v)?),
                "rot" => t.rotate = Some(v.parse()?),
                "flip" => t.flip = Some(v.parse()?),
                "blur" => {
                    let sigma: f32 = v.parse().map_err(|_| format!("invalid blur `{v}`"))?;
                    if !(0.1..=100.0).contains(&sigma) {
                        return Err("blur must be between 0.1 and 100".into());
                    }
                    t.blur = Some(sigma);
                }
                "gray" => match v {
                    "true" => t.grayscale = true,
                    _ => return Err("gray must be `true` (or left out)".into()),
                },
                "fmt" => t.format = Some(v.parse()?),
                "q" => {
                    let q: u8 = v.parse().map_err(|_| format!("invalid quality `{v}`"))?;
                    if !(1..=100).contains(&q) {
                        return Err("q must be between 1 and 100".into());
                    }
                    t.quality = Some(q);
                }
                "preset" => {
                    if !is_valid_preset_name(v) {
                        return Err(format!("invalid preset name `{v}`"));
                    }
                    t.preset = Some(value);
                }
                "exp" => exp = Some(v.parse().map_err(|_| format!("invalid exp `{v}`"))?),
                "sig" => sig = Some(value),
                other => return Err(format!("unknown parameter `{other}`")),
            }
        }
        Ok((t, exp, sig))
    }

    /// True when the transform names a preset and nothing else.
    #[doc(hidden)]
    pub fn is_preset_only(&self) -> bool {
        self.preset.is_some() && {
            let mut rest = self.clone();
            rest.preset = None;
            rest.is_empty()
        }
    }
}

/// Preset names are 1–32 characters of `a-z`, `0-9`, `-`, and `_`.
#[doc(hidden)]
pub fn is_valid_preset_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn parse_dimension(name: &str, v: &str) -> Result<u32, String> {
    match v.parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("{name} must be a positive integer")),
    }
}

fn parse_crop(v: &str) -> Result<Crop, String> {
    let nums: Vec<u32> = v
        .split(',')
        .map(|n| n.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .map_err(|_| "crop must be `x,y,w,h`".to_string())?;
    match nums[..] {
        [x, y, width, height] if width > 0 && height > 0 => Ok(Crop {
            x,
            y,
            width,
            height,
        }),
        _ => Err("crop must be `x,y,w,h` with a positive width and height".into()),
    }
}

fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or("malformed percent-encoding in query")?;
                out.push(hex);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| "query is not valid UTF-8".into())
}

/// Metadata about a stored image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInfo {
    /// The image's ID: a 26-character ULID.
    pub id: String,
    /// The format the image is stored in.
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// Size of the stored image in bytes.
    pub bytes: u64,
    /// Upload time, RFC 3339 in UTC.
    pub created_at: String,
}

/// True when `id` is a well-formed image ID (a 26-character Crockford base32
/// ULID). IDs are checked before they're used in a URL or a storage key.
pub fn is_valid_id(id: &str) -> bool {
    id.len() == 26
        && id.as_bytes()[0] <= b'7'
        && id
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_round_trips_canonically() {
        let t = Transform::new()
            .format(Format::WebP)
            .width(400)
            .height(300)
            .fit(Fit::Cover)
            .gravity(Gravity::NorthEast)
            .crop(1, 2, 30, 40)
            .rotate(Rotation::Deg90)
            .flip(Flip::Both)
            .blur(2.5)
            .grayscale(true)
            .quality(80);
        let q = t.to_query();
        assert_eq!(
            q,
            "w=400&h=300&fit=cover&gravity=ne&crop=1,2,30,40&rot=90&flip=hv&blur=2.5&gray=true&fmt=webp&q=80"
        );
        let (parsed, exp, sig) = Transform::parse_query(&q).unwrap();
        assert_eq!(parsed, t);
        assert_eq!((exp, sig), (None, None));
    }

    #[test]
    fn parse_normalizes_and_decodes() {
        let (t, exp, sig) =
            Transform::parse_query("fmt=jpg&crop=0%2C0%2C10%2C10&w=5&exp=99&sig=ab").unwrap();
        assert_eq!(t.to_query(), "w=5&crop=0,0,10,10&fmt=jpeg");
        assert_eq!(exp, Some(99));
        assert_eq!(sig.as_deref(), Some("ab"));
    }

    #[test]
    fn parse_rejects_bad_input() {
        for q in [
            "w=0",
            "w=-1",
            "w=abc",
            "q=0",
            "q=101",
            "blur=0",
            "blur=NaN",
            "blur=101",
            "fmt=svg",
            "fit=stretch",
            "crop=1,2,3",
            "crop=0,0,0,5",
            "gray=1",
            "w=1&w=2",
            "foo=bar",
            "preset=../x",
            "preset=",
            "w=%zz",
        ] {
            assert!(Transform::parse_query(q).is_err(), "{q} should be rejected");
        }
    }

    #[test]
    fn empty_and_preset_only() {
        assert!(Transform::new().is_empty());
        assert!(Transform::new().preset("thumb").is_preset_only());
        assert!(!Transform::new().preset("thumb").width(3).is_preset_only());
        // An expiry alone doesn't make a transform non-empty.
        assert!(
            Transform::new()
                .expires_in(Duration::from_secs(5))
                .is_empty()
        );
    }

    #[test]
    fn validates_ids() {
        assert!(is_valid_id("01JD8Z6Q4M3V9K2T7XW5R8N0PB"));
        assert!(!is_valid_id("01JD8Z6Q4M3V9K2T7XW5R8N0P"));
        assert!(!is_valid_id("01jd8z6q4m3v9k2t7xw5r8n0pb"));
        assert!(!is_valid_id("../../../../etc/passwd...."));
        assert!(!is_valid_id("81JD8Z6Q4M3V9K2T7XW5R8N0PB"));
        assert!(!is_valid_id("01JD8Z6Q4M3V9K2T7XW5R8N0PI"));
    }
}
