//! Fuzz-style property tests for mr-core's readers of untrusted input: the
//! ZIP index (central directory and local-header recovery), CRC checks,
//! discovery, the merged index, the streaming JSON reader and the matcher,
//! over mutated real export parts and random bytes. Every call must return
//! without panicking, within a time bound, and without any single
//! allocation above `ALLOC_LIMIT` (inputs are tens of kilobytes).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use common::{fixture_dir, zips_in};
use mr_core::discover::discover;
use mr_core::index::{build_index, load_json, summarize};
use mr_core::matcher::{MatchOptions, match_export};
use mr_core::stream::SavedMediaReader;
use mr_core::zipindex::ZipIndex;
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

fn bounded(what: &str, f: impl FnOnce() + Send + 'static) {
    LARGEST.store(0, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(AssertUnwindSafe(f));
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(true) => {}
        Ok(false) => panic!("{what} panicked"),
        Err(_) => panic!("{what} did not finish within 20 s"),
    }
    let largest = LARGEST.load(Ordering::Relaxed);
    assert!(
        largest <= ALLOC_LIMIT,
        "{what} asked for a {} MB block",
        largest >> 20
    );
}

fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("mr-core-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

/// Small real parts: zip-slip entries, the weird JSON, the legacy links,
/// and a ZIP64 archive with stored and deflated entries.
fn zip_seeds() -> &'static [Vec<u8>] {
    static SEEDS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    SEEDS.get_or_init(|| {
        let mut out: Vec<Vec<u8>> = ["zipslip", "weird-json", "small-legacy"]
            .iter()
            .map(|f| std::fs::read(&zips_in(&fixture_dir(f))[0]).unwrap())
            .collect();
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut z = zip::ZipWriter::new(&mut buf);
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .large_file(true);
            let deflated = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            z.start_file("memories/2021-12-25_A81B4C2D-0000-4000-8000-00000000A81B-main.jpg", stored)
                .unwrap();
            z.write_all(&[0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
            z.start_file("json/memories_history.json", deflated).unwrap();
            z.write_all(br#"{"Saved Media":[{"Date":"2021-12-25 18:42:07 UTC","Media Type":"Image","Location":"Latitude, Longitude: 51.5, -0.1"}]}"#)
                .unwrap();
            z.finish().unwrap();
        }
        out.push(buf.into_inner());
        out
    })
}

fn json_seed() -> &'static [u8] {
    static SEED: OnceLock<Vec<u8>> = OnceLock::new();
    SEED.get_or_init(|| {
        let path = &zips_in(&fixture_dir("weird-json"))[0];
        let mut a = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut f = a.by_name("json/memories_history.json").unwrap();
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        v
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
}

fn mutation() -> impl Strategy<Value = Mutation> {
    let interesting = prop_oneof![
        Just(0u32),
        Just(1),
        Just(0xFFFF),
        Just(0x7FFF_FFFF),
        Just(u32::MAX),
        any::<u32>()
    ];
    prop_oneof![
        (any::<usize>(), 1u8..=255).prop_map(|(p, m)| Mutation::Flip(p, m)),
        (any::<usize>(), interesting).prop_map(|(p, v)| Mutation::SetU32(p, v)),
        (
            any::<usize>(),
            prop_oneof![Just(u64::MAX), Just(1u64 << 63), any::<u64>()]
        )
            .prop_map(|(p, v)| Mutation::SetU64(p, v)),
        any::<usize>().prop_map(Mutation::Truncate),
        (any::<usize>(), 1usize..64).prop_map(|(p, n)| Mutation::Delete(p, n)),
        (any::<usize>(), prop::collection::vec(any::<u8>(), 1..32))
            .prop_map(|(p, b)| Mutation::Insert(p, b)),
    ]
}

/// ZIP headers are little-endian; write fields that way, and favour
/// positions near the end, where the central directory lives.
fn apply(mut v: Vec<u8>, muts: &[Mutation]) -> Vec<u8> {
    for m in muts {
        if v.is_empty() {
            break;
        }
        let at = |p: usize, v: &Vec<u8>| {
            if p.is_multiple_of(3) {
                v.len().saturating_sub(1 + (p / 3) % 256.min(v.len()))
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
                v[i..end].copy_from_slice(&x.to_le_bytes()[..end - i]);
            }
            Mutation::SetU64(p, x) => {
                let i = at(*p, &v);
                let end = (i + 8).min(v.len());
                v[i..end].copy_from_slice(&x.to_le_bytes()[..end - i]);
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
        }
    }
    v
}

fn case_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = scratch().join(format!("case-{}", N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The whole read path over one part: open, every entry's CRC, discovery,
/// merged index, JSON, summary, matching.
fn exercise_part(bytes: Vec<u8>) {
    let dir = case_dir();
    let part = dir.join("mydata~1760000000000.zip");
    std::fs::write(&part, &bytes).unwrap();
    let p = part.clone();
    bounded("ZIP part", move || {
        if let Ok(index) = ZipIndex::open(&p) {
            for i in 0..index.entries.len().min(200) {
                let _ = index.verify_crc(i);
            }
        }
        let d = discover(std::slice::from_ref(&p));
        for export in &d.exports {
            let index = build_index(&d, export);
            let json = load_json(&d, &index);
            let _ = summarize(&d, export, &index, json.as_ref());
            let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
            let _ = match_export(entries, &index, &MatchOptions::default());
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

fn exercise_json(bytes: Vec<u8>) {
    bounded("JSON reader", move || {
        if let Ok(r) = SavedMediaReader::new(&bytes[..]) {
            for (n, item) in r.enumerate() {
                if n > 100_000 || item.as_ref().is_err_and(|e| !e.is_recoverable()) {
                    break;
                }
            }
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn mutated_parts_never_panic_hang_or_balloon(
        seed in 0usize..4,
        muts in prop::collection::vec(mutation(), 1..6),
    ) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let base = zip_seeds()[seed % zip_seeds().len()].clone();
        exercise_part(apply(base, &muts));
    }

    #[test]
    fn random_bytes_as_a_part_never_panic(
        body in prop::collection::vec(any::<u8>(), 0..4096),
        with_eocd in any::<bool>(),
    ) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend(body);
        if with_eocd {
            // An end-of-central-directory record pointing anywhere.
            bytes.extend_from_slice(b"PK\x05\x06\0\0\0\0\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\0\0");
        }
        exercise_part(bytes);
    }

    #[test]
    fn mutated_json_never_panics_hangs_or_balloons(
        muts in prop::collection::vec(mutation(), 1..8),
    ) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        exercise_json(apply(json_seed().to_vec(), &muts));
    }

    #[test]
    fn random_json_text_never_panics(text in "\\PC{0,512}") {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut bytes = br#"{"Saved Media":["#.to_vec();
        bytes.extend(text.into_bytes());
        exercise_json(bytes);
    }
}

#[test]
fn deeply_nested_json_is_refused_without_a_stack_overflow() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for open in ["[", "{\"a\":"] {
        let mut bytes = br#"{"Saved Media":[{"Date":"#.to_vec();
        bytes.extend(open.repeat(200_000).into_bytes());
        exercise_json(bytes.clone());
        let mut top = Vec::new();
        top.extend(open.repeat(200_000).into_bytes());
        exercise_json(top);
    }
}
