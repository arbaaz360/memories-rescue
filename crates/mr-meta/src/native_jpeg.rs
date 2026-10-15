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

/// Read `src`, apply the §4.3 photo field set (keeping other EXIF), write `dst`.
/// Returns `(bytes_in, bytes_out)`.
pub fn write_jpeg(src: &Path, dst: &Path, cap: &Capture) -> io::Result<(u64, u64)> {
    let data = std::fs::read(src)?;
    // Snapchat exports carry no EXIF at all, which little_exif reports as an error.
    let mut md = match Metadata::new_from_vec(&data, FileExtension::JPEG) {
        Ok(md) => md,
        Err(e) if e.to_string().contains("No EXIF data found") => Metadata::new(),
        Err(e) => return Err(e),
    };
    apply(&mut md, cap);
    let mut out = data;
    md.write_to_vec(&mut out, FileExtension::JPEG)?;
    std::fs::write(dst, &out)?;
    Ok((std::fs::metadata(src)?.len(), out.len() as u64))
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
