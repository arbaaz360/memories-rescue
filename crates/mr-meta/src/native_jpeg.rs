//! Native JPEG EXIF writer on `little_exif`, the one maintained pure-Rust
//! crate that both reads and writes EXIF (kamadak-exif and nom-exif are
//! read-only; img-parts moves APP segments but does not encode TIFF/EXIF).

use std::io;
use std::path::Path;

use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;

use crate::fields::Capture;
use crate::guard;

/// Read `src`, apply the §4.3 photo field set (keeping other EXIF), write `dst`.
/// Returns `(bytes_in, bytes_out)`.
pub fn write_jpeg(src: &Path, dst: &Path, cap: &Capture) -> io::Result<(u64, u64)> {
    let data = guard::read_capped(src, guard::MAX_PHOTO_BYTES, "photo")?;
    let mut md = existing_exif(&data)?.unwrap_or_else(Metadata::new);
    apply(&mut md, cap);
    let bytes_in = data.len() as u64;
    let mut out = data;
    guard::no_panic(|| md.write_to_vec(&mut out, FileExtension::JPEG))?;
    std::fs::write(dst, &out)?;
    Ok((bytes_in, out.len() as u64))
}

/// The EXIF a JPEG already has; empty when there is none (Snapchat exports
/// carry none). The EXIF block is checked by [`guard::check_tiff`] and
/// little_exif reads it from a minimal JPEG (SOI, that APP1, EOI), so it
/// never parses the rest of an untrusted file.
fn existing_exif(data: &[u8]) -> io::Result<Option<Metadata>> {
    let bad = |e: String| guard::invalid(format!("unreadable EXIF: {e}"));
    let Some(app1) = guard::first_app1(data).map_err(bad)? else {
        return Ok(None);
    };
    let Some(tiff) = app1.strip_prefix(b"Exif\0\0") else {
        // An APP1 that is not EXIF (XMP): no EXIF to keep.
        return Ok(None);
    };
    guard::check_tiff(tiff).map_err(bad)?;
    let mut mini = vec![0xFF, 0xD8, 0xFF, 0xE1];
    mini.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    mini.extend_from_slice(app1);
    mini.extend_from_slice(&[0xFF, 0xD9]);
    guard::no_panic(|| Metadata::new_from_vec(&mini, FileExtension::JPEG)).map(Some)
}

/// Read the EXIF of a JPEG (the read-back of NFR-REL-05).
pub fn read_jpeg(path: &Path) -> io::Result<Metadata> {
    let data = guard::read_capped(path, guard::MAX_PHOTO_BYTES, "photo")?;
    existing_exif(&data)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No EXIF data found"))
}

pub fn apply(md: &mut Metadata, cap: &Capture) {
    let local = cap.exif_local();
    let off = cap.exif_offset();
    md.set_tag(ExifTag::DateTimeOriginal(local.clone()));
    md.set_tag(ExifTag::CreateDate(local.clone()));
    md.set_tag(ExifTag::ModifyDate(local));
    md.set_tag(ExifTag::OffsetTimeOriginal(off.clone()));
    md.set_tag(ExifTag::OffsetTimeDigitized(off.clone()));
    md.set_tag(ExifTag::OffsetTime(off));
    if let Some((lat, lat_ref, lon, lon_ref)) = cap.gps_refs() {
        md.set_tag(ExifTag::GPSVersionID(vec![2, 3, 0, 0]));
        md.set_tag(ExifTag::GPSLatitudeRef(lat_ref.to_string()));
        md.set_tag(ExifTag::GPSLatitude(dms(lat)));
        md.set_tag(ExifTag::GPSLongitudeRef(lon_ref.to_string()));
        md.set_tag(ExifTag::GPSLongitude(dms(lon)));
        let t = cap.utc;
        use chrono::Timelike;
        md.set_tag(ExifTag::GPSTimeStamp(vec![
            uR64 {
                nominator: t.hour(),
                denominator: 1,
            },
            uR64 {
                nominator: t.minute(),
                denominator: 1,
            },
            uR64 {
                nominator: t.second(),
                denominator: 1,
            },
        ]));
        md.set_tag(ExifTag::GPSDateStamp(cap.gps_date()));
    }
}

/// Degrees, minutes and seconds as rationals; seconds carry four decimals,
/// which is about 3 mm, the precision exiftool uses.
fn dms(deg_abs: f64) -> Vec<uR64> {
    let d = deg_abs.floor();
    let m_full = (deg_abs - d) * 60.0;
    let m = m_full.floor();
    let s = (m_full - m) * 60.0;
    vec![
        uR64 {
            nominator: d as u32,
            denominator: 1,
        },
        uR64 {
            nominator: m as u32,
            denominator: 1,
        },
        uR64 {
            nominator: (s * 10_000.0).round() as u32,
            denominator: 10_000,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dms_round_trips_to_four_decimals() {
        let v = dms(51.5074);
        assert_eq!((v[0].nominator, v[1].nominator), (51, 30));
        let secs = v[2].nominator as f64 / v[2].denominator as f64;
        let back = 51.0 + 30.0 / 60.0 + secs / 3600.0;
        assert!((back - 51.5074).abs() < 1e-6, "{back}");
    }
}
