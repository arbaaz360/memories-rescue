//! Robustness of the probe on hostile input: random bytes, mutated real
//! MP4/JPEG/PNG files, every box size field set to extreme values, and the
//! field parsers fed garbage. Each call runs under a watchdog: a panic or a
//! call that does not finish fails the test (an endless loop would otherwise
//! hang the suite). Debug builds also turn any arithmetic overflow into a
//! panic, which these tests then report.

use std::io::Cursor;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use proptest::prelude::*;

use super::*;

const WATCHDOG: Duration = Duration::from_secs(30);

/// Run `f` on its own thread; fail on a panic or when it outlives the watchdog.
fn total<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(r);
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => panic!("{what} panicked"),
        Err(_) => panic!("{what} did not finish within {WATCHDOG:?}"),
    }
}

/// Every entry point on the same bytes: the seekable probe, the stream probe
/// with no limit, a tight limit, and a wrong declared length.
fn probe_all(bytes: &[u8]) {
    let len = bytes.len() as u64;
    let b = bytes.to_vec();
    total("probe_seekable", move || {
        let _ = probe_seekable(&mut Cursor::new(b), len);
    });
    for (limit, declared) in [
        (u64::MAX, len),
        (64, len),
        (u64::MAX, 0),
        (u64::MAX, len * 2 + 1),
    ] {
        let b = bytes.to_vec();
        total("probe_reader", move || {
            let _ = probe_reader(Cursor::new(b), declared, limit);
        });
    }
}

fn sample(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mr-meta/tests/data")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const SAMPLES: [&str; 4] = [
    "tiny-moov-first.mp4",
    "tiny-moov-last.mp4",
    "tiny.jpg",
    "tiny.png",
];

/// Offsets of every box header in a well-formed MP4, containers included.
fn box_offsets(buf: &[u8], base: usize, out: &mut Vec<usize>) {
    // Children are contiguous: each box starts where the previous one ends.
    let mut pos = 0usize;
    for c in children(buf) {
        out.push(base + pos);
        if matches!(
            &c.typ,
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"edts" | b"dinf"
        ) {
            box_offsets(&buf[c.payload.clone()], base + c.payload.start, out);
        }
        pos = c.payload.end;
    }
}

// ---------------------------------------------------------------------------
// Regressions: the exact shapes of the reported bug
// ---------------------------------------------------------------------------

fn ftyp() -> Vec<u8> {
    let mut v = 16u32.to_be_bytes().to_vec();
    v.extend_from_slice(b"ftypisom\0\0\0\x01");
    v
}

/// A top-level box with a 64-bit size that makes `pos + size` wrap to 0:
/// the old walker accepted it and jumped back to the start for ever.
#[test]
fn top_level_size_that_wraps_to_zero_is_rejected() {
    for size64 in [u64::MAX - 15, u64::MAX, u64::MAX - 7, 1 << 63] {
        let mut b = ftyp();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"free");
        b.extend_from_slice(&size64.to_be_bytes());
        b.extend_from_slice(&[0u8; 64]);
        let len = b.len() as u64;
        let c = b.clone();
        let r = total("probe_seekable", move || {
            probe_seekable(&mut Cursor::new(c), len)
        });
        assert!(r.is_err(), "size {size64:#x}: {r:?}");
        let r = total("probe_reader", move || {
            probe_reader(Cursor::new(b), len, u64::MAX)
        });
        assert!(r.is_err(), "size {size64:#x}: {r:?}");
    }
}

/// A child of `moov` with a 64-bit size after an 8-byte sibling, so that
/// `pos + size` wraps: the old `children()` accepted it, built a reversed
/// range and the slice panicked (in release builds too).
#[test]
fn child_box_with_huge_size_is_skipped_not_sliced() {
    for size64 in [u64::MAX, u64::MAX - 7, u64::MAX - 3, 1 << 63, 17] {
        let mut child = 8u32.to_be_bytes().to_vec();
        child.extend_from_slice(b"free");
        child.extend_from_slice(&1u32.to_be_bytes());
        child.extend_from_slice(b"trak");
        child.extend_from_slice(&size64.to_be_bytes());
        child.extend_from_slice(&[0u8; 32]);
        let mut b = ftyp();
        b.extend_from_slice(&((8 + child.len()) as u32).to_be_bytes());
        b.extend_from_slice(b"moov");
        b.extend_from_slice(&child);
        let len = b.len() as u64;
        let c = b.clone();
        let r = total("probe_seekable", move || {
            probe_seekable(&mut Cursor::new(c), len)
        });
        let info = r.expect("moov is there; its broken child is ignored");
        assert!(info.video.is_none() && info.audio.is_none());
        let r = total("probe_reader", move || {
            probe_reader(Cursor::new(b), len, u64::MAX)
        });
        assert!(r.unwrap().video.is_none());
    }
}

/// A moov that claims 200 MiB on a 1 KiB stream: refused without
/// allocating the claimed size first.
#[test]
fn moov_claiming_more_than_the_stream_holds_is_an_error() {
    let mut b = ftyp();
    b.extend_from_slice(&(200u32 << 20).to_be_bytes());
    b.extend_from_slice(b"moov");
    b.extend_from_slice(&[0u8; 1000]);
    let declared = 300 << 20;
    let r = total("probe_reader", move || {
        probe_reader(Cursor::new(b), declared, u64::MAX)
    });
    assert!(r.is_err(), "{r:?}");
}

/// Millions of 8-byte boxes: bounded work and memory.
#[test]
fn a_file_of_tiny_boxes_is_bounded() {
    let mut b = ftyp();
    for _ in 0..200_000 {
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(b"free");
    }
    probe_all(&b);
    let len = b.len() as u64;
    let r = total("probe_seekable", move || {
        probe_seekable(&mut Cursor::new(b), len)
    });
    assert!(r.is_err());
}

/// Every box header of the real samples set to each extreme size, in both
/// the 32-bit and the 64-bit form.
#[test]
fn every_box_size_field_at_extremes() {
    for name in ["tiny-moov-first.mp4", "tiny-moov-last.mp4"] {
        let orig = sample(name);
        let mut offsets = Vec::new();
        box_offsets(&orig, 0, &mut offsets);
        assert!(offsets.len() > 10, "{name}: {} boxes", offsets.len());
        let len = orig.len() as u64;
        for &at in &offsets {
            for s32 in [0u32, 7, 8, 9, 15, 0x7FFF_FFFF, 0xFFFF_FFFF, len as u32 + 1] {
                let mut b = orig.clone();
                b[at..at + 4].copy_from_slice(&s32.to_be_bytes());
                probe_all(&b);
            }
            for s64 in [
                0u64,
                15,
                16,
                len + 1,
                1 << 32,
                1 << 63,
                u64::MAX - 15,
                u64::MAX,
            ] {
                let mut b = orig.clone();
                if at + 16 > b.len() {
                    continue;
                }
                b[at..at + 4].copy_from_slice(&1u32.to_be_bytes());
                b[at + 8..at + 16].copy_from_slice(&s64.to_be_bytes());
                probe_all(&b);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

fn box_type() -> impl Strategy<Value = [u8; 4]> {
    prop::sample::select(vec![
        *b"ftyp", *b"moov", *b"mdat", *b"free", *b"trak", *b"mvhd", *b"stsd",
    ])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn random_bytes_never_panic_or_hang(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        probe_all(&bytes);
    }

    /// Random bytes behind a plausible box header, so the MP4 paths run.
    #[test]
    fn random_boxes_never_panic_or_hang(
        boxes in prop::collection::vec(
            (any::<u32>(), box_type(), any::<bool>(), any::<u64>(), prop::collection::vec(any::<u8>(), 0..512)),
            1..6,
        )
    ) {
        let mut b = Vec::new();
        for (size32, typ, large, size64, payload) in boxes {
            b.extend_from_slice(&if large { 1u32 } else { size32 }.to_be_bytes());
            b.extend_from_slice(&typ);
            if large {
                b.extend_from_slice(&size64.to_be_bytes());
            }
            b.extend_from_slice(&payload);
        }
        probe_all(&b);
    }

    /// Real files with bytes overwritten and the tail cut off.
    #[test]
    fn mutated_samples_never_panic_or_hang(
        which in 0usize..4,
        edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..32),
        cut in prop::option::of(any::<usize>()),
    ) {
        let mut b = sample(SAMPLES[which]);
        for (at, v) in edits {
            let n = b.len();
            b[at % n] = v;
        }
        if let Some(c) = cut {
            let n = b.len();
            b.truncate(c % (n + 1));
        }
        probe_all(&b);
    }

    /// The field parsers on garbage: total, and their answers stay sane.
    #[test]
    fn field_parsers_are_total(
        p in prop::collection::vec(any::<u8>(), 0..512),
        ts in any::<u32>(),
        ts2 in any::<u32>(),
    ) {
        let (_, rate, _) = parse_stts(&p, ts);
        if let Some(r) = rate {
            prop_assert!(r.is_finite() && r >= 0.0);
        }
        let _ = parse_stsz(&p);
        let _ = parse_elst(&p, ts, ts2);
        let _ = parse_mvhd(&p);
        if let Some(r) = parse_tkhd_rotation(&p) {
            prop_assert!(matches!(r, 0 | 90 | 180 | 270));
        }
        let _ = parse_avcc(&p);
        let _ = parse_hvcc(&p);
        let _ = esds_object_type(&p);
        let _ = sps_bit_depth(&p);
        if let Some(o) = tiff_orientation(&p) {
            prop_assert!((1..=8).contains(&o));
        }
        let q = p.clone();
        total("parse_trak", move || { let _ = parse_trak(&q, ts); });
        let q = p.clone();
        total("info_from_moov", move || { let _ = info_from_moov(&q, Container::Mp4, None, 1 << 20); });
        let q = p.clone();
        total("children", move || { let _ = children(&q); });
    }
}

/// The file-path entry point on a few damaged files, end to end.
#[test]
fn probe_on_damaged_files_returns() {
    let dir = std::env::temp_dir().join(format!("mr-composite-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (k, name) in SAMPLES.iter().enumerate() {
        let mut b = sample(name);
        let n = b.len();
        for i in (0..n).step_by(7) {
            b[i] ^= 0x5A;
        }
        let path = dir.join(format!("damaged{k}"));
        std::fs::write(&path, &b).unwrap();
        total("probe", move || {
            let _ = probe(&path);
        });
    }
    let _ = std::fs::remove_dir_all(&dir);
}
