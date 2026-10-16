//! Input limits and an EXIF/TIFF structure check, applied before any
//! untrusted bytes reach `little_exif`.
//!
//! `little_exif` trusts the counts it reads: an entry's byte count (up to
//! 4 GiB) becomes an allocation before the data is read, a sub-IFD offset
//! that points back at its parent recurses until the stack overflows, and a
//! next-IFD link to itself loops while memory grows. None of those is a
//! catchable panic. [`check_tiff`] walks the same structure first, with
//! bounds, a cycle check and a budget, and refuses anything little_exif
//! could not read safely.

use std::collections::HashSet;
use std::io;

/// Largest photo read into memory whole (JPEG, PNG). Phone photos are a few
/// MB; anything near this is not a photo from an export.
pub const MAX_PHOTO_BYTES: u64 = 200 << 20;
/// Largest `moov` box read into memory (it holds only the sample tables:
/// a few MB even for an hour of video). The same cap as mr-composite's probe.
pub const MAX_MOOV_BYTES: u64 = 256 << 20;
/// Largest decompressed EXIF raw profile accepted from a PNG text chunk.
pub const MAX_EXIF_BYTES: usize = 4 << 20;

const MAX_IFDS: usize = 64;
const MAX_DEPTH: usize = 4;

const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_THUMB_OFFSET: u16 = 0x0201;
const TAG_THUMB_LENGTH: u16 = 0x0202;

pub(crate) fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Read a whole file of at most `max` bytes.
pub(crate) fn read_capped(path: &std::path::Path, max: u64, what: &str) -> io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len > max {
        return Err(invalid(format!(
            "{what} of {} MB is larger than the {} MB this app reads",
            len >> 20,
            max >> 20
        )));
    }
    std::fs::read(path)
}

/// Run a third-party decoder, turning a panic into an error.
pub(crate) fn no_panic<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(invalid("the metadata reader stopped on malformed data")))
}

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

struct Tiff<'a> {
    b: &'a [u8],
    order: Order,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let s = self.b.get(at..at.checked_add(2)?)?;
        Some(match self.order {
            Order::Little => u16::from_le_bytes([s[0], s[1]]),
            Order::Big => u16::from_be_bytes([s[0], s[1]]),
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(match self.order {
            Order::Little => u32::from_le_bytes(s),
            Order::Big => u32::from_be_bytes(s),
        })
    }
}

fn type_size(t: u16) -> Option<u32> {
    Some(match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// Check that a TIFF block (the EXIF payload after `Exif\0\0`) can be read
/// without trusting any count or offset in it.
pub fn check_tiff(b: &[u8]) -> Result<(), String> {
    let order = match b.get(..4) {
        Some(b"II*\0") => Order::Little,
        Some(b"MM\0*") => Order::Big,
        _ => return Err("not a TIFF header".into()),
    };
    let t = Tiff { b, order };
    let ifd0 = t.u32(4).ok_or("truncated TIFF header")? as usize;
    let mut visited = HashSet::new();
    let mut total_data = 0u64;
    walk(&t, ifd0, 0, &mut visited, &mut total_data)
}

fn walk(
    t: &Tiff<'_>,
    mut offset: usize,
    depth: usize,
    visited: &mut HashSet<usize>,
    total_data: &mut u64,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("sub-IFDs nested too deep".into());
    }
    // The chain IFD0 -> IFD1 -> ... at this depth.
    loop {
        if !visited.insert(offset) {
            return Err("an IFD offset points back at an earlier IFD".into());
        }
        if visited.len() > MAX_IFDS {
            return Err("too many IFDs".into());
        }
        let n = t.u16(offset).ok_or("IFD outside the data")? as usize;
        let entries_end = offset
            .checked_add(2 + 12 * n)
            .filter(|&e| e <= t.b.len())
            .ok_or("IFD entries run past the data")?;
        let mut strips: (Option<Vec<u64>>, Option<Vec<u64>>) = (None, None);
        let mut thumb: (Option<u64>, Option<u64>) = (None, None);
        for i in 0..n {
            let e = offset + 2 + 12 * i;
            let tag = t.u16(e).ok_or("entry outside the data")?;
            let typ = t.u16(e + 2).ok_or("entry outside the data")?;
            let count = t.u32(e + 4).ok_or("entry outside the data")?;
            let size = type_size(typ).ok_or_else(|| format!("tag 0x{tag:04x} has type {typ}"))?;
            let bytes = size
                .checked_mul(count)
                .ok_or_else(|| format!("tag 0x{tag:04x} claims more than 4 GiB"))?
                as usize;
            let value_at = if bytes > 4 {
                let off = t.u32(e + 8).ok_or("entry outside the data")? as usize;
                off.checked_add(bytes)
                    .filter(|&end| end <= t.b.len())
                    .ok_or_else(|| format!("tag 0x{tag:04x} data runs past the EXIF block"))?;
                off
            } else {
                e + 8
            };
            *total_data += bytes as u64;
            if *total_data > (t.b.len() as u64) * 4 + 4096 {
                return Err("EXIF entries claim far more data than the block holds".into());
            }
            match tag {
                TAG_EXIF_IFD | TAG_GPS_IFD | TAG_INTEROP_IFD => {
                    let sub = t.u32(value_at).ok_or("sub-IFD offset outside the data")? as usize;
                    walk(t, sub, depth + 1, visited, total_data)?;
                }
                TAG_STRIP_OFFSETS | TAG_STRIP_BYTE_COUNTS | TAG_THUMB_OFFSET | TAG_THUMB_LENGTH => {
                    let values = (0..count as usize)
                        .map(|k| match size {
                            2 => t.u16(value_at + 2 * k).map(u64::from),
                            4 => t.u32(value_at + 4 * k).map(u64::from),
                            _ => None,
                        })
                        .collect::<Option<Vec<u64>>>()
                        .ok_or_else(|| format!("tag 0x{tag:04x} holds no offsets"))?;
                    match tag {
                        TAG_STRIP_OFFSETS => strips.0 = Some(values),
                        TAG_STRIP_BYTE_COUNTS => strips.1 = Some(values),
                        TAG_THUMB_OFFSET => thumb.0 = values.first().copied(),
                        _ => thumb.1 = values.first().copied(),
                    }
                }
                _ => {}
            }
        }
        let len = t.b.len() as u64;
        if let (Some(offs), Some(counts)) = &strips {
            let mut sum = 0u64;
            for (o, c) in offs.iter().zip(counts) {
                if o.checked_add(*c).is_none_or(|end| end > len) {
                    return Err("a strip runs past the EXIF block".into());
                }
                sum += c;
            }
            if sum > len {
                return Err("strips claim more data than the block holds".into());
            }
        }
        if let (Some(o), Some(l)) = thumb
            && o.checked_add(l).is_none_or(|end| end > len)
        {
            return Err("the thumbnail runs past the EXIF block".into());
        }
        // Sub-IFDs often have no next link at all; little_exif tolerates that.
        match t.u32(entries_end) {
            Some(0) | None => return Ok(()),
            Some(next) if depth > 0 => {
                // A link after a sub-IFD is ignored by little_exif's walk.
                let _ = next;
                return Ok(());
            }
            Some(next) => offset = next as usize,
        }
    }
}

/// The payload of the first APP1 segment of a JPEG, found exactly the way
/// little_exif finds it: a byte scan where every byte after 0xFF other than
/// EOI starts a segment with a length, and the entropy-coded data after SOS
/// is skipped up to the next 0xFF that is not a stuffed byte or a restart
/// marker. `Ok(None)` when there is none.
pub fn first_app1(b: &[u8]) -> Result<Option<&[u8]>, String> {
    if b.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err("not a JPEG".into());
    }
    let mut i = 2usize;
    let mut prev_ff = false;
    while i < b.len() {
        let byte = b[i];
        i += 1;
        if !prev_ff {
            prev_ff = byte == 0xFF;
            continue;
        }
        prev_ff = false;
        if byte == 0xD9 {
            return Ok(None);
        }
        let len = b
            .get(i..i + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as usize)
            .ok_or("JPEG segment length past the end")?;
        if len < 2 {
            return Err("JPEG segment shorter than its length field".into());
        }
        let start = i + 2;
        let end = i + len;
        if byte == 0xE1 {
            return b
                .get(start..end)
                .map(Some)
                .ok_or_else(|| "JPEG APP1 runs past the end".into());
        }
        i = end;
        if byte == 0xDA {
            // Skip the entropy-coded data; stop on the 0xFF of a real marker.
            let mut ff = false;
            while i < b.len() {
                let c = b[i];
                i += 1;
                if ff {
                    if matches!(c, 0x00 | 0xD0..=0xD7) {
                        ff = false;
                        continue;
                    }
                    i -= 2;
                    break;
                }
                ff = c == 0xFF;
            }
        }
    }
    Ok(None)
}

/// Check a JPEG's EXIF before little_exif reads it.
pub fn check_jpeg(b: &[u8]) -> Result<(), String> {
    match first_app1(b)? {
        Some(app1) => match app1.strip_prefix(b"Exif\0\0") {
            Some(tiff) => check_tiff(tiff),
            // An APP1 that is not EXIF (XMP): little_exif reports no EXIF.
            None => Ok(()),
        },
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiff(entries: &[(u16, u16, u32, u32)], next: u32) -> Vec<u8> {
        let mut v = b"MM\0*".to_vec();
        v.extend_from_slice(&8u32.to_be_bytes());
        v.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (tag, typ, count, value) in entries {
            v.extend_from_slice(&tag.to_be_bytes());
            v.extend_from_slice(&typ.to_be_bytes());
            v.extend_from_slice(&count.to_be_bytes());
            v.extend_from_slice(&value.to_be_bytes());
        }
        v.extend_from_slice(&next.to_be_bytes());
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    #[test]
    fn sane_blocks_pass_and_crafted_ones_do_not() {
        assert!(check_tiff(&tiff(&[(0x010F, 2, 4, 0x4142_4300)], 0)).is_ok());
        assert!(
            check_tiff(&tiff(&[(0x010F, 2, u32::MAX, 26)], 0)).is_err(),
            "huge count"
        );
        assert!(
            check_tiff(&tiff(&[(0x8769, 4, 1, 8)], 0)).is_err(),
            "sub-IFD cycle"
        );
        assert!(
            check_tiff(&tiff(&[(0x010F, 2, 2, 0)], 8)).is_err(),
            "chain cycle"
        );
        assert!(
            check_tiff(&tiff(&[(0x0111, 4, 1, 8), (0x0117, 4, 1, 0xFFFF_FF00)], 0)).is_err(),
            "strip"
        );
        assert!(
            check_tiff(&tiff(&[(0x0201, 4, 1, 8), (0x0202, 4, 1, 0x7FFF_FFFF)], 0)).is_err(),
            "thumbnail"
        );
        assert!(
            check_tiff(&tiff(&[(0x010F, 99, 1, 0)], 0)).is_err(),
            "unknown type"
        );
        assert!(
            check_tiff(b"MM\0*\xff\xff\xff\xff").is_err(),
            "IFD0 outside"
        );
    }
}
