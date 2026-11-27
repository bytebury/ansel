//! MP4 detection and validation.
//!
//! Videos are stored and served byte for byte; ansel never decodes or
//! re-encodes them. An upload is accepted as a video only when its box
//! structure parses and it has a video track, whose dimensions are reported.

use super::error::ApiError;

/// `ftyp` brands of MP4 video files.
const MP4_BRANDS: &[&[u8; 4]] = &[
    b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"iso7", b"iso8", b"iso9", b"mp41",
    b"mp42", b"avc1", b"dash", b"M4V ", b"M4VH", b"M4VP", b"mmp4", b"MSNV", b"3gp4", b"3gp5",
    b"3gp6", b"3g2a",
];

/// Major brands of still-image formats built on the same box structure
/// (HEIF, AVIF), which also list `isom`-style compatible brands.
const IMAGE_BRANDS: &[&[u8; 4]] = &[
    b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1", b"avif", b"avis",
];

/// True when the bytes start with an MP4 `ftyp` box. Like images, videos are
/// detected from their bytes only; filenames and `Content-Type` are ignored.
pub fn is_mp4(bytes: &[u8]) -> bool {
    let Some(ftyp) = boxes(bytes).next() else {
        return false;
    };
    let Ok((b"ftyp", body)) = ftyp else {
        return false;
    };
    // Major brand (4), minor version (4), then compatible brands.
    let Some(major) = body.get(..4) else {
        return false;
    };
    if IMAGE_BRANDS.iter().any(|b| b.as_slice() == major) {
        return false;
    }
    let compatible = body.get(8..).unwrap_or_default().chunks_exact(4);
    std::iter::once(major)
        .chain(compatible)
        .any(|brand| MP4_BRANDS.iter().any(|b| b.as_slice() == brand))
}

/// Validates an MP4's top-level structure and returns the display size of
/// its first video track.
pub fn probe(bytes: &[u8]) -> Result<(u32, u32), ApiError> {
    let invalid = || ApiError::unprocessable("video could not be read: malformed MP4");
    let mut moov = None;
    for item in boxes(bytes) {
        let (kind, body) = item.map_err(|()| invalid())?;
        if kind == b"moov" {
            moov = Some(body);
        }
    }
    let moov = moov.ok_or_else(invalid)?;

    for item in boxes(moov) {
        let (kind, trak) = item.map_err(|()| invalid())?;
        if kind != b"trak" {
            continue;
        }
        let mut tkhd = None;
        let mut is_video = false;
        for item in boxes(trak) {
            let (kind, body) = item.map_err(|()| invalid())?;
            match kind {
                b"tkhd" => tkhd = Some(body),
                b"mdia" => is_video = handler(body).map_err(|()| invalid())? == Some(*b"vide"),
                _ => {}
            }
        }
        if is_video {
            let (w, h) = track_size(tkhd.ok_or_else(invalid)?).ok_or_else(invalid)?;
            if w == 0 || h == 0 {
                return Err(invalid());
            }
            return Ok((w, h));
        }
    }
    Err(ApiError::unprocessable("MP4 has no video track"))
}

/// The handler type (`vide`, `soun`, …) from an `mdia` box's `hdlr`.
fn handler(mdia: &[u8]) -> Result<Option<[u8; 4]>, ()> {
    for item in boxes(mdia) {
        let (kind, body) = item?;
        if kind == b"hdlr" {
            // Version and flags (4), pre-defined (4), then the handler type.
            return Ok(body.get(8..12).map(|t| t.try_into().expect("4 bytes")));
        }
    }
    Ok(None)
}

/// Width and height from a `tkhd` box, with 90° and 270° rotations applied.
fn track_size(tkhd: &[u8]) -> Option<(u32, u32)> {
    // After version and flags: times, track ID, and duration (20 bytes in
    // version 0, 32 in version 1), 16 reserved bytes, a 36-byte matrix, then
    // width and height as 16.16 fixed point.
    let matrix_at = match tkhd.first()? {
        0 => 4 + 20 + 16,
        1 => 4 + 32 + 16,
        _ => return None,
    };
    let u32_at = |at: usize| {
        tkhd.get(at..at + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
    };
    let (a, b) = (u32_at(matrix_at)?, u32_at(matrix_at + 4)?);
    let w = u32_at(matrix_at + 36)? >> 16;
    let h = u32_at(matrix_at + 40)? >> 16;
    // A rotation by 90° or 270° has a = 0 and b = ±1.
    Some(if a == 0 && b != 0 { (h, w) } else { (w, h) })
}

/// Iterates over the boxes in `data`, yielding each box's type and body.
/// A box that runs past the end of `data` is an error.
fn boxes(mut data: &[u8]) -> impl Iterator<Item = Result<(&[u8; 4], &[u8]), ()>> {
    std::iter::from_fn(move || {
        if data.is_empty() {
            return None;
        }
        let item = next_box(data);
        data = match item {
            Ok((_, _, rest)) => rest,
            Err(()) => &[],
        };
        Some(item.map(|(kind, body, _)| (kind, body)))
    })
}

/// A box's type, its body, and the data after it.
type Split<'a> = (&'a [u8; 4], &'a [u8], &'a [u8]);

fn next_box(data: &[u8]) -> Result<Split<'_>, ()> {
    let header = data.get(..8).ok_or(())?;
    let kind: &[u8; 4] = header[4..8].try_into().expect("4 bytes");
    let (size, header_len) = match u32::from_be_bytes(header[..4].try_into().expect("4 bytes")) {
        // Size 0: the box extends to the end of the data.
        0 => (data.len() as u64, 8),
        // Size 1: a 64-bit size follows the type.
        1 => {
            let large = data.get(8..16).ok_or(())?;
            (u64::from_be_bytes(large.try_into().expect("8 bytes")), 16)
        }
        n => (u64::from(n), 8),
    };
    if size < header_len as u64 || size > data.len() as u64 {
        return Err(());
    }
    let size = size as usize;
    Ok((kind, &data[header_len..size], &data[size..]))
}

/// Builds a minimal MP4: `ftyp`, then a `moov` with one track.
#[cfg(test)]
pub fn test_mp4(handler: &[u8; 4], width: u32, height: u32) -> Vec<u8> {
    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }
    let mut tkhd = vec![0u8; 4 + 20 + 16];
    for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        tkhd.extend_from_slice(&m.to_be_bytes());
    }
    tkhd.extend_from_slice(&(width << 16).to_be_bytes());
    tkhd.extend_from_slice(&(height << 16).to_be_bytes());
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(handler);
    hdlr.extend_from_slice(&[0u8; 13]);
    let mdia = mp4_box(b"mdia", &mp4_box(b"hdlr", &hdlr));
    let trak = mp4_box(b"trak", &[mp4_box(b"tkhd", &tkhd), mdia].concat());
    let mut out = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");
    out.extend(mp4_box(b"moov", &trak));
    out.extend(mp4_box(b"mdat", &[0u8; 64]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_probes_mp4() {
        let mp4 = test_mp4(b"vide", 1920, 1080);
        assert!(is_mp4(&mp4));
        assert_eq!(probe(&mp4).unwrap(), (1920, 1080));
    }

    #[test]
    fn applies_rotation() {
        let mut mp4 = test_mp4(b"vide", 1920, 1080);
        // Rewrite the matrix to a 90° rotation: a = 0, b = 1, c = -1, d = 0.
        let tkhd = mp4.windows(4).position(|w| w == b"tkhd").unwrap() + 4;
        let matrix = tkhd + 40;
        mp4[matrix..matrix + 16].copy_from_slice(
            &[0u32, 0x0001_0000, 0, 0xFFFF_0000]
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect::<Vec<_>>(),
        );
        assert_eq!(probe(&mp4).unwrap(), (1080, 1920));
    }

    #[test]
    fn rejects_audio_only_and_truncated_files() {
        assert_eq!(probe(&test_mp4(b"soun", 0, 0)).unwrap_err().status, 422);
        let mp4 = test_mp4(b"vide", 640, 480);
        assert_eq!(probe(&mp4[..mp4.len() - 10]).unwrap_err().status, 422);
    }

    #[test]
    fn ignores_images_and_other_files() {
        let mut heic = test_mp4(b"vide", 1, 1);
        heic[8..12].copy_from_slice(b"heic");
        heic[16..32].copy_from_slice(b"mif1heicmiafMiHE");
        assert!(!is_mp4(&heic));
        assert!(!is_mp4(b"\x89PNG\r\n\x1a\n"));
        assert!(!is_mp4(b""));
        let mut qt = test_mp4(b"vide", 1, 1);
        qt[8..12].copy_from_slice(b"qt  ");
        qt[16..32].copy_from_slice(b"qt  qt  qt  qt  ");
        assert!(!is_mp4(&qt));
    }
}
