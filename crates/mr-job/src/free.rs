//! The free tier, per install (FR-44).
//!
//! Without a licence an item may be processed when its key is already in
//! this install's free set (re-processing the same memories, into another
//! folder or after a retry, stays free), or when the set holds fewer than
//! [`FREE_TIER_ITEMS`] keys; the item then joins the set as it finishes,
//! done or failed (FR-44 counts both). New items are let in in
//! chronological order. Choosing another destination therefore gives no
//! new allowance.
//!
//! The set holds keyed hashes, never item keys: the first 8 bytes of
//! HMAC-SHA256(per-install secret, item key). Its file, [`FREE_FILE`] in the
//! state directory, carries an HMAC over its contents with the same secret,
//! the protection the licence cache has (§9.3 rule 5): it stops casual
//! tampering; strong DRM is a non-goal.
//!
//! **What deleting the store does.** Beside the file, the app keeps the
//! set's size in the OS keychain (a [`Seal`]). It is written after the file
//! each time the set grows, so it is never ahead of a finished item.
//!
//! | File | Keychain count | Used | Free to redo |
//! |---|---|---|---|
//! | valid | valid | the larger of the two | the file's items |
//! | valid | missing or edited | the file's size | the file's items |
//! | missing, unreadable or edited | valid | the count | none |
//! | missing, unreadable or edited | missing or edited | 0 | none |
//!
//! So deleting or editing the file never gives new free items; it only
//! loses the free re-processing of the earlier ones. A fresh allowance needs
//! the keychain entries removed too (the per-install secret, which also
//! drops a licence, or both the file and the count): a deliberate step in
//! Credential Manager or Keychain Access, not a casual one. The rejected
//! alternatives: the file alone (deleting one file in app data gives
//! another 100); treating a missing file as a used-up allowance (no way to
//! tell it from a first run without the keychain anyway, and it punishes
//! someone whose app data was lost with nothing at all); the whole set in
//! the keychain (Windows caps an entry at 2,560 bytes, a keychain write per
//! item, and it buys nothing over the count).
//!
//! Items let in but not finished are *reserved* by the job running them,
//! so two jobs running at once cannot both spend the last places. A crash
//! loses only the reservations: the next run counts the items that were in
//! flight as new again.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::options::FREE_TIER_ITEMS;

/// The free set's file in the state directory.
pub const FREE_FILE: &str = "free-set.json";

/// Key for state directories the app did not register ([`install`]):
/// `mr-cli` and tests. Its own secret is what makes the app's file casual-
/// tamper resistant; this one only keeps the code path the same.
const DEFAULT_SECRET: &[u8] = b"mr-job: free set of an unregistered state directory";

const VERSION: u32 = 1;
const HASH_BYTES: usize = 8;

/// A small value kept apart from the state directory that survives its
/// deletion (the app: the OS keychain). See the module documentation.
pub trait Seal: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, value: &str) -> Result<(), String>;
}

#[derive(Debug, Serialize, Deserialize)]
struct FreeFile {
    version: u32,
    items: Vec<String>,
    hmac: String,
}

#[derive(Debug, Default)]
struct State {
    /// Hashes of the items in the set.
    known: BTreeSet<String>,
    /// Items counted (by the keychain) whose hashes were lost with the file.
    lost: usize,
    /// Hashes let in and not finished yet, with the jobs holding them.
    reserved: HashMap<String, HashSet<String>>,
}

impl State {
    fn used(&self) -> usize {
        self.known.len() + self.lost
    }

    /// Places taken by reservations of jobs other than `job`.
    fn reserved_by_others(&self, job: Option<&str>) -> usize {
        self.reserved
            .values()
            .filter(|holders| job.is_none_or(|j| holders.iter().any(|h| h != j)))
            .count()
    }
}

/// This install's free set. Shared by every job of one state directory
/// ([`for_state`]); all methods take `&self`.
pub struct FreeSet {
    path: Option<PathBuf>,
    secret: Vec<u8>,
    seal: Option<Box<dyn Seal>>,
    state: Mutex<State>,
}

impl std::fmt::Debug for FreeSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("FreeSet")
            .field("path", &self.path)
            .field("used", &s.used())
            .field("reserved", &s.reserved.len())
            .finish()
    }
}

impl FreeSet {
    /// A set that lives in memory only: a fresh allowance (dry runs, tests).
    pub fn memory() -> FreeSet {
        FreeSet {
            path: None,
            secret: DEFAULT_SECRET.to_vec(),
            seal: None,
            state: Mutex::new(State::default()),
        }
    }

    /// The set stored at `path`, protected with `secret`, with its size
    /// also kept in `seal`. Never fails: what cannot be verified is read
    /// as the module documentation's table says.
    pub fn open(path: impl Into<PathBuf>, secret: &[u8], seal: Option<Box<dyn Seal>>) -> FreeSet {
        let set = FreeSet {
            path: Some(path.into()),
            secret: secret.to_vec(),
            seal,
            state: Mutex::new(State::default()),
        };
        let known = set.read_file();
        let sealed = set.read_seal();
        if known.is_none() && sealed.is_some_and(|n| n > 0) {
            tracing::warn!(
                counted = sealed.unwrap_or(0),
                "free set file missing or not valid; the keychain count is kept"
            );
        }
        let known = known.unwrap_or_default();
        let lost = sealed.unwrap_or(0).saturating_sub(known.len());
        {
            let mut s = set.lock();
            s.known = known;
            s.lost = lost;
        }
        set
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn mac(&self, domain: &str, body: &str) -> String {
        let mut msg = Vec::with_capacity(domain.len() + 1 + body.len());
        msg.extend_from_slice(domain.as_bytes());
        msg.push(0);
        msg.extend_from_slice(body.as_bytes());
        hex(&hmac_sha256(&self.secret, &msg))
    }

    /// The keyed hash the set stores for an item key.
    fn hash(&self, item_key: &str) -> String {
        self.mac("free-item", item_key)[..HASH_BYTES * 2].to_string()
    }

    fn file_mac(&self, items: &[String]) -> String {
        self.mac(&format!("free-set v{VERSION}"), &items.join("\n"))
    }

    fn seal_value(&self, count: usize) -> String {
        format!(
            "{count}:{}",
            &self.mac("free-count", &count.to_string())[..32]
        )
    }

    /// The file's hashes, or `None` when it is missing, unreadable or does
    /// not verify.
    fn read_file(&self) -> Option<BTreeSet<String>> {
        let path = self.path.as_ref()?;
        let text = fs::read_to_string(path).ok()?;
        let Ok(f) = serde_json::from_str::<FreeFile>(&text) else {
            tracing::warn!("free set file unreadable; ignored");
            return None;
        };
        let well_formed = f.version == VERSION
            && f.items.len() <= 4 * FREE_TIER_ITEMS
            && f.items
                .iter()
                .all(|h| h.len() == HASH_BYTES * 2 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        if !well_formed || f.hmac != self.file_mac(&f.items) {
            tracing::warn!("free set file does not verify; ignored");
            return None;
        }
        Some(f.items.into_iter().collect())
    }

    fn read_seal(&self) -> Option<usize> {
        let v = self.seal.as_ref()?.get()?;
        let (n, _) = v.split_once(':')?;
        let n: usize = n.parse().ok()?;
        if v != self.seal_value(n) {
            tracing::warn!("free set count in the keychain does not verify; ignored");
            return None;
        }
        Some(n)
    }

    /// Write the file (synced, then renamed into place), then the count.
    fn persist(&self, s: &State) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let items: Vec<String> = s.known.iter().cloned().collect();
        let f = FreeFile {
            version: VERSION,
            hmac: self.file_mac(&items),
            items,
        };
        let text = serde_json::to_string_pretty(&f).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut out = fs::File::create(&tmp)?;
            out.write_all(text.as_bytes())?;
            out.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(seal) = &self.seal
            && let Err(e) = seal.set(&self.seal_value(s.used()))
        {
            tracing::warn!("free set count not saved in the keychain: {e}");
        }
        Ok(())
    }

    /// Items processed on the free tier so far, on this install.
    pub fn used(&self) -> usize {
        self.lock().used().min(FREE_TIER_ITEMS)
    }

    /// Places left for items not in the set.
    pub fn remaining(&self) -> usize {
        let s = self.lock();
        FREE_TIER_ITEMS.saturating_sub(s.used() + s.reserved_by_others(None))
    }

    /// Whether `item_key` is in the set already (free to process again).
    pub fn contains(&self, item_key: &str) -> bool {
        let h = self.hash(item_key);
        self.lock().known.contains(&h)
    }

    /// Of `items` (key and chronological rank), the keys the free tier lets
    /// `job` process now: every item already in the set, and as many new
    /// ones, in chronological order, as there are places left after other
    /// jobs' reservations. Changes nothing.
    pub fn select<'a>(
        &self,
        job: Option<&str>,
        items: impl IntoIterator<Item = (&'a str, usize)>,
    ) -> HashSet<String> {
        let s = self.lock();
        let mut places = FREE_TIER_ITEMS.saturating_sub(s.used() + s.reserved_by_others(job));
        let mut out = HashSet::new();
        let mut new: Vec<(usize, &str, String)> = Vec::new();
        for (key, rank) in items {
            let h = self.hash(key);
            if s.known.contains(&h) {
                out.insert(key.to_string());
            } else {
                new.push((rank, key, h));
            }
        }
        new.sort();
        let mut taken: HashSet<String> = HashSet::new();
        for (_, key, h) in new {
            // The same item twice (two exports holding it) takes one place.
            if taken.contains(&h) {
                out.insert(key.to_string());
            } else if places > 0 {
                places -= 1;
                taken.insert(h);
                out.insert(key.to_string());
            }
        }
        out
    }

    /// Let `job` start `item_key`: true when it is in the set, already
    /// reserved, or a place is left (which it then reserves until
    /// [`FreeSet::record`] or [`FreeSet::release`]).
    pub fn admit(&self, job: &str, item_key: &str) -> bool {
        let h = self.hash(item_key);
        let mut s = self.lock();
        if s.known.contains(&h) {
            return true;
        }
        if let Some(holders) = s.reserved.get_mut(&h) {
            holders.insert(job.to_string());
            return true;
        }
        if s.used() + s.reserved.len() >= FREE_TIER_ITEMS {
            return false;
        }
        s.reserved.entry(h).or_default().insert(job.to_string());
        true
    }

    /// `item_key` finished on the free tier: it joins the set, which is
    /// saved before this returns. An item already in the set changes
    /// nothing. Returns whether it joined.
    pub fn record(&self, item_key: &str) -> std::io::Result<bool> {
        let h = self.hash(item_key);
        let mut s = self.lock();
        let was_reserved = s.reserved.remove(&h).is_some();
        if s.known.contains(&h) {
            return Ok(false);
        }
        // Only an item let in by `admit` should get here; never past the limit.
        if !was_reserved && s.used() + s.reserved.len() >= FREE_TIER_ITEMS {
            return Ok(false);
        }
        s.known.insert(h);
        self.persist(&s)?;
        Ok(true)
    }

    /// Drop the reservations `job` holds (its run ended).
    pub fn release(&self, job: &str) {
        let mut s = self.lock();
        s.reserved.retain(|_, holders| {
            holders.remove(job);
            !holders.is_empty()
        });
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<FreeSet>>> {
    static SETS: OnceLock<Mutex<HashMap<PathBuf, Arc<FreeSet>>>> = OnceLock::new();
    SETS.get_or_init(Default::default)
}

/// Use `set` for every job of `state_dir` in this process (the app does
/// this at start, with its per-install secret and the keychain count).
pub fn install(state_dir: &Path, set: FreeSet) -> Arc<FreeSet> {
    let set = Arc::new(set);
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(crate::layout::absolute(state_dir), set.clone());
    set
}

/// The free set of `state_dir`: the installed one, else its [`FREE_FILE`]
/// under a fixed key, opened once per process.
pub fn for_state(state_dir: &Path) -> Arc<FreeSet> {
    let dir = crate::layout::absolute(state_dir);
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(dir.clone())
        .or_insert_with(|| Arc::new(FreeSet::open(dir.join(FREE_FILE), DEFAULT_SECRET, None)))
        .clone()
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemSeal(Mutex<Option<String>>);
    impl Seal for Arc<MemSeal> {
        fn get(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn set(&self, v: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(v.to_string());
            Ok(())
        }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mr-free-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.join(FREE_FILE)
    }

    fn keys(n: usize, prefix: &str) -> Vec<String> {
        (0..n)
            .map(|i| format!("2024-01-01_{prefix}{i:04}"))
            .collect()
    }

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn the_set_fills_to_the_limit_and_keeps_its_members_free() {
        let set = FreeSet::memory();
        let a = keys(150, "A");
        let ranked: Vec<(&str, usize)> =
            a.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        let first = set.select(None, ranked.clone());
        assert_eq!(first.len(), FREE_TIER_ITEMS);
        assert!(first.contains(&a[0]) && !first.contains(&a[100]));
        for k in &a[..100] {
            assert!(set.admit("j1", k));
            set.record(k).unwrap();
        }
        assert_eq!(set.used(), 100);
        assert!(!set.admit("j1", &a[100]), "full");
        assert!(set.admit("j2", &a[5]), "a member stays free");
        assert_eq!(set.select(None, ranked), first, "the same 100 again");
    }

    #[test]
    fn reservations_hold_places_until_recorded_or_released() {
        let set = FreeSet::memory();
        let a = keys(100, "R");
        for k in &a[..99] {
            set.record(k).unwrap();
        }
        assert!(set.admit("j1", &a[99]));
        assert!(
            !set.admit("j2", "2024-01-01_OTHER"),
            "the last place is j1's"
        );
        assert!(set.admit("j2", &a[99]), "the same item shares the place");
        assert_eq!(set.select(Some("j1"), [("2024-01-01_X", 0)]).len(), 0);
        set.release("j1");
        set.release("j2");
        assert_eq!(set.remaining(), 1);
        assert!(set.admit("j3", "2024-01-01_OTHER"));
    }

    #[test]
    fn the_file_and_the_count_survive_reopening() {
        let path = temp("reopen");
        let seal = Arc::new(MemSeal::default());
        let open = || FreeSet::open(&path, b"secret", Some(Box::new(seal.clone())));
        let set = open();
        for k in keys(30, "F") {
            assert!(set.record(&k).unwrap());
        }
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("2024-01-01"),
            "no item keys are stored: {text}"
        );
        let again = open();
        assert_eq!(again.used(), 30);
        assert!(again.contains("2024-01-01_F0007"));
        // Another secret (another install) cannot read it: nothing is free to redo.
        let other = FreeSet::open(&path, b"another", None);
        assert_eq!(other.used(), 0);
        assert!(!other.contains("2024-01-01_F0007"));
    }

    #[test]
    fn deleting_or_editing_the_file_keeps_the_count_but_not_the_members() {
        let path = temp("tamper");
        let seal = Arc::new(MemSeal::default());
        let open = || FreeSet::open(&path, b"secret", Some(Box::new(seal.clone())));
        let set = open();
        for k in keys(60, "T") {
            set.record(&k).unwrap();
        }
        // Edited: one member removed by hand.
        let mut f: FreeFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        f.items.pop();
        fs::write(&path, serde_json::to_string(&f).unwrap()).unwrap();
        let edited = open();
        assert_eq!(edited.used(), 60, "the keychain count stands");
        assert!(!edited.contains("2024-01-01_T0000"));
        // Deleted.
        fs::remove_file(&path).unwrap();
        let deleted = open();
        assert_eq!(deleted.used(), 60);
        assert_eq!(deleted.remaining(), 40);
        // A new item after that counts on top of the lost ones.
        deleted.record("2024-02-02_NEW").unwrap();
        assert_eq!(open().used(), 61);
        // An edited count is ignored; the file then decides.
        *seal.0.lock().unwrap() = Some("0:00".into());
        assert_eq!(open().used(), 1);
        // Both gone: a fresh allowance (the step this design leaves open).
        *seal.0.lock().unwrap() = None;
        fs::remove_file(&path).unwrap();
        assert_eq!(open().used(), 0);
    }

    #[test]
    fn an_older_copy_of_the_file_gives_nothing_back() {
        let path = temp("rollback");
        let seal = Arc::new(MemSeal::default());
        let open = || FreeSet::open(&path, b"secret", Some(Box::new(seal.clone())));
        let set = open();
        for k in keys(10, "O") {
            set.record(&k).unwrap();
        }
        let copy = fs::read(&path).unwrap();
        for k in keys(50, "P") {
            set.record(&k).unwrap();
        }
        fs::write(&path, copy).unwrap();
        let restored = open();
        assert_eq!(restored.used(), 60);
        assert!(restored.contains("2024-01-01_O0003"));
    }
}
