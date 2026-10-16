//! Fuzz-style property tests: random bytes and mutated real files through
//! every reader and writer in mr-meta. Each call must return (Ok or Err)
//! without panicking, within a time bound, and without asking the allocator
//! for more than a bounded block (a crafted count must not turn into a
//! multi-gigabyte allocation; on a smaller machine that is an abort).
//!
//! The inputs are at most a few hundred kilobytes, so any single allocation
//! above `ALLOC_LIMIT` is a bug. Tests in this binary run one at a time
//! (they share the allocation counter).

use std::alloc::{GlobalAlloc, Layout, System};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::TimeZone;
use mr_meta::fields::Capture;
use mr_meta::{native_jpeg, native_mp4, native_png};
use proptest::prelude::*;

const ALLOC_LIMIT: usize = 64 << 20;
static LARGEST: AtomicUsize = AtomicUsize::new(0);

struct Watch;

unsafe impl GlobalAlloc for Watch {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LARGEST.fetch_max(new_size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Watch = Watch;

static SERIAL: Mutex<()> = Mutex::new(());

fn cap() -> Capture {
    let utc = chrono::Utc
        .with_ymd_and_hms(2021, 12, 25, 18, 42, 7)
        .unwrap();
    Capture::new(utc, 3600, Some((51.50735, -0.12776)))
}

fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("mr-meta-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

/// Run `f` on another thread: no panic, done within 10 s, no allocation
/// above the limit.
fn bounded(what: &str, f: impl FnOnce() + Send + 'static) {
    LARGEST.store(0, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(AssertUnwindSafe(f));
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(true) => {}
        Ok(false) => panic!("{what} panicked"),
        Err(_) => panic!("{what} did not finish within 10 s"),
    }
    let largest = LARGEST.load(Ordering::Relaxed);
    assert!(
        largest <= ALLOC_LIMIT,
        "{what} asked for a {} MB block",
        largest >> 20
    );
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

/// Real seeds: the test files as they are, and after our writers (so the
/// EXIF, eXIf, keys and udta paths are exercised too).
fn seeds() -> &'static [(&'static str, Vec<u8>)] {
    static SEEDS: OnceLock<Vec<(&'static str, Vec<u8>)>> = OnceLock::new();
    SEEDS.get_or_init(|| {
        let dir = scratch().join("seeds");
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = vec![];
        for (kind, name) in [
            ("jpg", "tiny.jpg"),
            ("png", "tiny.png"),
            ("mp4", "tiny-moov-first.mp4"),
            ("mp4", "tiny-moov-last.mp4"),
        ] {
            let raw = data(name);
            let src = dir.join(name);
            std::fs::write(&src, &raw).unwrap();
            let dst = dir.join(format!("tagged-{name}"));
            let ok = match kind {
                "jpg" => native_jpeg::write_jpeg(&src, &dst, &cap()).is_ok(),
                "png" => native_png::write_png(&src, &dst, &cap()).is_ok(),
                _ => native_mp4::write_mp4(&src, &dst, &cap()).is_ok(),
            };
            assert!(ok, "seed {name} writes");
            out.push((kind, raw));
            out.push((kind, std::fs::read(&dst).unwrap()));
        }
        out
    })
}

#[derive(Debug, Clone)]
enum Mutation {
    Flip(usize, u8),
    SetU32(usize, u32),
    SetU64(usize, u64),
    Truncate(usize),
    Delete(usize, usize),
    Insert(usize, Vec<u8>),
    Duplicate(usize, usize),
}

fn interesting_u32() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0u32),
        Just(1),
        Just(4),
        Just(7),
        Just(8),
        Just(16),
        Just(0x7FFF_FFFF),
        Just(0x8000_0000),
        Just(0xFFFF_FFF0),
        Just(u32::MAX),
        any::<u32>(),
    ]
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        (any::<usize>(), 1u8..=255).prop_map(|(p, m)| Mutation::Flip(p, m)),
        (any::<usize>(), interesting_u32()).prop_map(|(p, v)| Mutation::SetU32(p, v)),
        (
            any::<usize>(),
            prop_oneof![Just(u64::MAX), Just(1u64 << 63), Just(0u64), any::<u64>()]
        )
            .prop_map(|(p, v)| Mutation::SetU64(p, v)),
        any::<usize>().prop_map(Mutation::Truncate),
        (any::<usize>(), 1usize..64).prop_map(|(p, n)| Mutation::Delete(p, n)),
        (any::<usize>(), prop::collection::vec(any::<u8>(), 1..32))
            .prop_map(|(p, b)| Mutation::Insert(p, b)),
        (any::<usize>(), 1usize..256).prop_map(|(p, n)| Mutation::Duplicate(p, n)),
    ]
}

/// Field positions favour box and segment headers: half of the mutations
/// land on a 4-byte boundary.
fn apply(mut v: Vec<u8>, muts: &[Mutation]) -> Vec<u8> {
    for m in muts {
        if v.is_empty() {
            break;
        }
        let at = |p: usize, v: &Vec<u8>| {
            if p.is_multiple_of(2) {
                ((p / 2) % v.len()) & !3
            } else {
                p % v.len()
            }
        };
        match m {
            Mutation::Flip(p, mask) => {
                let i = at(*p, &v);
                v[i] ^= mask;
            }
            Mutation::SetU32(p, x) => {
                let i = at(*p, &v);
                let end = (i + 4).min(v.len());
                v[i..end].copy_from_slice(&x.to_be_bytes()[..end - i]);
            }
            Mutation::SetU64(p, x) => {
                let i = at(*p, &v);
                let end = (i + 8).min(v.len());
                v[i..end].copy_from_slice(&x.to_be_bytes()[..end - i]);
            }
            Mutation::Truncate(p) => {
                let n = p % v.len();
                v.truncate(n);
            }
            Mutation::Delete(p, n) => {
                let i = p % v.len();
                let end = (i + n).min(v.len());
                v.drain(i..end);
            }
            Mutation::Insert(p, b) => {
                let i = p % (v.len() + 1);
                v.splice(i..i, b.iter().copied());
            }
            Mutation::Duplicate(p, n) => {
                let i = p % v.len();
                let end = (i + n).min(v.len());
                let copy = v[i..end].to_vec();
                v.splice(end..end, copy);
            }
        }
    }
    v
}

/// Every reader and writer that could see `bytes` as an input of `kind`.
fn exercise(kind: &str, bytes: Vec<u8>, case: u64) {
    let dir = scratch().join(format!("case-{case}"));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("in.{kind}"));
    let dst = dir.join(format!("out.{kind}"));
    std::fs::write(&src, &bytes).unwrap();
    let kind = kind.to_string();
    let (s, d) = (src.clone(), dst.clone());
    bounded(&format!("{kind} writer"), move || {
        let _ = match kind.as_str() {
            "jpg" => native_jpeg::write_jpeg(&s, &d, &cap())
                .map(|_| ())
                .map_err(|e| e.to_string()),
            "png" => native_png::write_png(&s, &d, &cap())
                .map(|_| ())
                .map_err(|e| e.to_string()),
            _ => native_mp4::write_mp4(&s, &d, &cap())
                .map(|_| ())
                .map_err(|e| e.to_string()),
        };
        // The readers run on the input and on whatever the writer made.
        for p in [&s, &d] {
            if !p.exists() {
                continue;
            }
            match kind.as_str() {
                "jpg" => {
                    let _ = native_jpeg::read_jpeg(p);
                }
                "png" => {
                    let _ = native_png::read_png(p);
                }
                _ => {
                    let _ = native_mp4::summarize(p);
                }
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

fn case_id() -> u64 {
    static N: AtomicUsize = AtomicUsize::new(0);
    N.fetch_add(1, Ordering::Relaxed) as u64
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn mutated_real_files_never_panic_hang_or_balloon(
        seed in 0usize..8,
        muts in prop::collection::vec(mutation(), 1..6),
    ) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (kind, base) = &seeds()[seed % seeds().len()];
        exercise(kind, apply(base.clone(), &muts), case_id());
    }

    #[test]
    fn random_bytes_behind_a_real_signature_never_panic(
        kind in prop_oneof![Just("jpg"), Just("png"), Just("mp4")],
        body in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut bytes = match kind {
            "jpg" => vec![0xFF, 0xD8, 0xFF, 0xE1],
            "png" => b"\x89PNG\r\n\x1a\n".to_vec(),
            _ => vec![0, 0, 0, 0x18, b'f', b't', b'y', b'p'],
        };
        bytes.extend(body);
        exercise(kind, bytes, case_id());
    }
}

// ---------------------------------------------------------------------------
// Crafted cases the random search is unlikely to hit
// ---------------------------------------------------------------------------

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// A JPEG whose APP1 holds `tiff`.
fn jpeg_with_tiff(tiff: &[u8]) -> Vec<u8> {
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(tiff);
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
    v.extend_from_slice(&be16((app1.len() + 2) as u16));
    v.extend_from_slice(&app1);
    // A real image after it.
    v.extend_from_slice(&data("tiny.jpg")[2..]);
    v
}

/// Big-endian TIFF with one IFD at offset 8 holding `entries`
/// (tag, type, count, value) and the given next-IFD link.
fn tiff(entries: &[(u16, u16, u32, u32)], next: u32) -> Vec<u8> {
    let mut v = b"MM\0*".to_vec();
    v.extend_from_slice(&be32(8));
    v.extend_from_slice(&be16(entries.len() as u16));
    for (tag, typ, count, value) in entries {
        v.extend_from_slice(&be16(*tag));
        v.extend_from_slice(&be16(*typ));
        v.extend_from_slice(&be32(*count));
        v.extend_from_slice(&be32(*value));
    }
    v.extend_from_slice(&be32(next));
    v.extend_from_slice(&[0u8; 64]);
    v
}

#[test]
fn crafted_exif_counts_cycles_and_strips_are_refused_quietly() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        // A 4 GiB ASCII value: the count alone must not become an allocation.
        ("huge count", tiff(&[(0x010F, 2, u32::MAX, 26)], 0)),
        ("huge LONG count", tiff(&[(0x010F, 4, 0x3FFF_FFFF, 26)], 0)),
        // The Exif sub-IFD points back at IFD0: a cycle, not a stack overflow.
        ("sub-IFD cycle", tiff(&[(0x8769, 4, 1, 8)], 0)),
        // The next-IFD link points at itself.
        ("IFD chain cycle", tiff(&[(0x010F, 2, 2, 0x4100_0000)], 8)),
        // Strips that claim gigabytes.
        (
            "strip byte counts",
            tiff(&[(0x0111, 4, 1, 8), (0x0117, 4, 1, 0xFFFF_FF00)], 0),
        ),
        // A thumbnail longer than the file.
        (
            "thumbnail length",
            tiff(&[(0x0201, 4, 1, 8), (0x0202, 4, 1, 0x7FFF_FFFF)], 0),
        ),
    ];
    for (what, t) in cases {
        let bytes = jpeg_with_tiff(&t);
        let dir = scratch().join(format!("crafted-{}", what.replace(' ', "-")));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.jpg");
        std::fs::write(&src, &bytes).unwrap();
        let (s, d) = (src.clone(), dir.join("out.jpg"));
        bounded(what, move || {
            let _ = native_jpeg::write_jpeg(&s, &d, &cap());
            let _ = native_jpeg::read_jpeg(&s);
        });
    }
}

#[test]
fn crafted_mp4_sizes_and_short_boxes_are_refused_quietly() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let ftyp = {
        let mut v = be32(16).to_vec();
        v.extend_from_slice(b"ftypisom");
        v.extend_from_slice(&be32(0x200));
        v
    };
    let boxed = |typ: &[u8; 4], payload: &[u8]| {
        let mut v = be32((8 + payload.len()) as u32).to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    };
    let mut huge64 = be32(1).to_vec();
    huge64.extend_from_slice(b"moov");
    huge64.extend_from_slice(&u64::MAX.to_be_bytes());
    let mut short_keys = vec![0u8; 4]; // keys full box
    short_keys.extend_from_slice(&be32(1));
    short_keys.extend_from_slice(&be32(3)); // entry size 3 < 8
    short_keys.extend_from_slice(b"mdta");
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]);
    let meta = boxed(
        b"meta",
        &[boxed(b"hdlr", &hdlr), boxed(b"keys", &short_keys)].concat(),
    );
    let short_stco = boxed(b"stco", &[0, 0, 0]);
    let stbl = boxed(b"stbl", &short_stco);
    let trak = boxed(b"trak", &boxed(b"mdia", &boxed(b"minf", &stbl)));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("64-bit size past the end", [ftyp.clone(), huge64].concat()),
        (
            "keys entry shorter than its header",
            [ftyp.clone(), boxed(b"moov", &meta)].concat(),
        ),
        (
            "keys in udta",
            [ftyp.clone(), boxed(b"moov", &boxed(b"udta", &meta))].concat(),
        ),
        (
            "stco body under 8 bytes",
            [
                ftyp.clone(),
                boxed(b"moov", &trak),
                boxed(b"mdat", &[0u8; 16]),
            ]
            .concat(),
        ),
        (
            "moov claims 3 GiB",
            [
                ftyp.clone(),
                be32(0xC000_0000).to_vec(),
                b"moov".to_vec(),
                vec![0u8; 64],
            ]
            .concat(),
        ),
    ];
    for (what, bytes) in cases {
        exercise("mp4", bytes, case_id());
        let _ = what;
    }
}
