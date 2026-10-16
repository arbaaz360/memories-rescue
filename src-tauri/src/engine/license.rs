//! Licence (SRS §9, FR-44 to FR-47).
//!
//! * Activation is one online check with Gumroad's licence verify API; it is
//!   the only call that increments Gumroad's `uses` count.
//! * The key lives in the OS keychain ([`SecretStore`]); a small status cache
//!   in the state directory is integrity-protected with an HMAC whose key is
//!   a per-install secret, also in the keychain (§9.3 rule 5). Only the key's
//!   status is stored, never the purchaser's details (rule 6).
//! * A verified licence stays valid offline. A background re-check runs at
//!   most every 30 days with `increment_uses_count=false`; transport errors,
//!   timeouts, 429, 5xx and non-Gumroad bodies change nothing (rule 3). Only an
//!   explicit revocation (rule 4) starts a 14-day grace period.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ipc::{ErrorCode, IpcError, IpcResult, LicenseStatus, LicenseTier};

const CACHE_FILE: &str = "license.json";
const KEY_ENTRY: &str = "license-key";
const SECRET_ENTRY: &str = "install-secret";
const RECHECK_DAYS: i64 = 30;
const GRACE_DAYS: i64 = 14;
pub const DEFAULT_VERIFY_URL: &str = "https://api.gumroad.com/v2/licenses/verify";

/// The OS keychain, or a stand-in for tests.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str);
}

/// macOS Keychain / Windows Credential Manager through the `keyring` crate.
pub struct Keychain {
    service: String,
}

impl Keychain {
    pub fn new(service: &str) -> Keychain {
        Keychain {
            service: service.to_string(),
        }
    }

    fn entry(&self, name: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(&self.service, name).ok()
    }
}

impl SecretStore for Keychain {
    fn get(&self, name: &str) -> Option<String> {
        self.entry(name)?.get_password().ok()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.entry(name)
            .ok_or("keychain unavailable")?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) {
        if let Some(e) = self.entry(name) {
            let _ = e.delete_credential();
        }
    }
}

/// In-memory secrets, for tests.
#[derive(Default)]
pub struct MemorySecrets(Mutex<std::collections::HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, name: &str) -> Option<String> {
        self.0.lock().expect("secrets").get(name).cloned()
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .expect("secrets")
            .insert(name.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, name: &str) {
        self.0.lock().expect("secrets").remove(name);
    }
}

/// Build configuration for the Gumroad product (§9.1: identifiers live in build configuration).
#[derive(Debug, Clone)]
pub struct GumroadConfig {
    pub product_id: Option<String>,
    pub verify_url: String,
    pub product_url: String,
    pub price_display: Option<String>,
}

impl GumroadConfig {
    /// `MR_GUMROAD_PRODUCT_ID`, `MR_GUMROAD_PRODUCT_URL` and `MR_PRICE_DISPLAY`
    /// at build time. Debug builds also read them at run time, plus
    /// `MR_GUMROAD_VERIFY_URL` to point at a fake Gumroad on loopback.
    pub fn from_build() -> GumroadConfig {
        let runtime = |k: &str| {
            if cfg!(debug_assertions) {
                std::env::var(k).ok().filter(|v| !v.is_empty())
            } else {
                None
            }
        };
        GumroadConfig {
            product_id: runtime("MR_GUMROAD_PRODUCT_ID")
                .or_else(|| option_env!("MR_GUMROAD_PRODUCT_ID").map(str::to_string)),
            verify_url: runtime("MR_GUMROAD_VERIFY_URL")
                .unwrap_or_else(|| DEFAULT_VERIFY_URL.to_string()),
            product_url: runtime("MR_GUMROAD_PRODUCT_URL")
                .or_else(|| option_env!("MR_GUMROAD_PRODUCT_URL").map(str::to_string))
                .unwrap_or_else(|| {
                    "https://novabyteworks.gumroad.com/l/memories-rescue".to_string()
                }),
            price_display: runtime("MR_PRICE_DISPLAY")
                .or_else(|| option_env!("MR_PRICE_DISPLAY").map(str::to_string)),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Cache {
    tier: String,
    key_masked: Option<String>,
    verified_at: Option<String>,
    last_check_at: Option<String>,
    grace_until: Option<String>,
    #[serde(default)]
    hmac: String,
}

/// What one verify call said.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Valid,
    /// A well-formed Gumroad reply: `success: false`, or refunded / chargebacked.
    Rejected,
    /// Anything else: transport error, timeout, 429, 5xx, a body that is not Gumroad's JSON.
    Unreachable(String),
}

pub struct Licenser {
    state_dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    pub config: GumroadConfig,
    cache: Mutex<Cache>,
}

impl Licenser {
    pub fn new(
        state_dir: PathBuf,
        secrets: Arc<dyn SecretStore>,
        config: GumroadConfig,
    ) -> Licenser {
        let l = Licenser {
            state_dir,
            secrets,
            config,
            cache: Mutex::new(Cache::default()),
        };
        let loaded = l.load();
        *l.cache.lock().expect("licence") = loaded;
        l
    }

    fn install_secret(&self) -> String {
        if let Some(s) = self.secrets.get(SECRET_ENTRY) {
            return s;
        }
        let s =
            uuid::Uuid::new_v4().simple().to_string() + &uuid::Uuid::new_v4().simple().to_string();
        let _ = self.secrets.set(SECRET_ENTRY, &s);
        s
    }

    fn mac(&self, c: &Cache) -> String {
        let body = format!(
            "{}|{}|{}|{}|{}",
            c.tier,
            c.key_masked.as_deref().unwrap_or(""),
            c.verified_at.as_deref().unwrap_or(""),
            c.last_check_at.as_deref().unwrap_or(""),
            c.grace_until.as_deref().unwrap_or("")
        );
        hex(&hmac_sha256(
            self.install_secret().as_bytes(),
            body.as_bytes(),
        ))
    }

    /// The stored status; a tampered or unreadable cache, or a missing key, is the free tier.
    fn load(&self) -> Cache {
        let path = self.state_dir.join(CACHE_FILE);
        let Some(c) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Cache>(&s).ok())
        else {
            return free();
        };
        if c.hmac != self.mac(&c) || self.secrets.get(KEY_ENTRY).is_none() {
            return free();
        }
        c
    }

    fn store(&self, mut c: Cache) {
        c.hmac = self.mac(&c);
        let _ = std::fs::create_dir_all(&self.state_dir);
        let path = self.state_dir.join(CACHE_FILE);
        if let Ok(text) = serde_json::to_string_pretty(&c) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        *self.cache.lock().expect("licence") = c;
    }

    pub fn status(&self) -> LicenseStatus {
        let mut c = self.cache.lock().expect("licence").clone();
        // Grace over: back to the free tier.
        if c.tier == "grace"
            && c.grace_until
                .as_deref()
                .and_then(parse)
                .is_some_and(|t| t < Utc::now())
        {
            self.secrets.delete(KEY_ENTRY);
            c = free();
            self.store(c.clone());
        }
        LicenseStatus {
            tier: match c.tier.as_str() {
                "licensed" => LicenseTier::Licensed,
                "grace" => LicenseTier::Grace,
                _ => LicenseTier::Free,
            },
            key_masked: c.key_masked.clone(),
            verified_at: c.verified_at.clone(),
            last_check_at: c.last_check_at.clone(),
            grace_until: c.grace_until.clone(),
            product_url: self.config.product_url.clone(),
            price_display: self.config.price_display.clone(),
            free_limit: mr_job::FREE_TIER_ITEMS as u64,
        }
    }

    /// Licensed, or revoked but still inside the grace period.
    pub fn unlocked(&self) -> bool {
        matches!(
            self.status().tier,
            LicenseTier::Licensed | LicenseTier::Grace
        )
    }

    pub fn activate(&self, key: &str) -> IpcResult<LicenseStatus> {
        let key = key.trim();
        if key.is_empty() {
            return Err(IpcError::new(
                ErrorCode::LicenseInvalid,
                "That key wasn't recognised.",
            ));
        }
        let Some(product) = self.config.product_id.clone() else {
            return Err(IpcError::new(
                ErrorCode::LicenseOffline,
                "This build can't activate licences: no Gumroad product is configured.",
            )
            .with_detail("MR_GUMROAD_PRODUCT_ID not set at build time"));
        };
        match verify(&self.config.verify_url, &product, key, true) {
            Verdict::Valid => {
                self.secrets.set(KEY_ENTRY, key).map_err(|e| {
                    IpcError::unknown("The licence key could not be saved in the system keychain.")
                        .with_detail(e)
                })?;
                let now = super::now_utc();
                self.store(Cache {
                    tier: "licensed".into(),
                    key_masked: Some(mask(key)),
                    verified_at: Some(now.clone()),
                    last_check_at: Some(now),
                    grace_until: None,
                    hmac: String::new(),
                });
                Ok(self.status())
            }
            Verdict::Rejected => Err(IpcError::new(
                ErrorCode::LicenseInvalid,
                "That key wasn't recognised.",
            )),
            Verdict::Unreachable(why) => Err(IpcError::new(
                ErrorCode::LicenseOffline,
                "Connect to the internet once to activate.",
            )
            .with_detail(why)),
        }
    }

    pub fn deactivate(&self) -> LicenseStatus {
        self.secrets.delete(KEY_ENTRY);
        self.store(free());
        self.status()
    }

    /// Background re-check (§9.3 rules 3 and 4). Returns true when the
    /// licence was revoked by this check, so the caller can warn once.
    pub fn recheck_if_due(&self) -> bool {
        let c = self.cache.lock().expect("licence").clone();
        if c.tier != "licensed" {
            return false;
        }
        let due = c
            .last_check_at
            .as_deref()
            .and_then(parse)
            .is_none_or(|t| Utc::now() - t >= chrono::Duration::days(RECHECK_DAYS));
        if !due {
            return false;
        }
        let (Some(product), Some(key)) =
            (self.config.product_id.clone(), self.secrets.get(KEY_ENTRY))
        else {
            return false;
        };
        match verify(&self.config.verify_url, &product, &key, false) {
            Verdict::Valid => {
                self.store(Cache {
                    last_check_at: Some(super::now_utc()),
                    ..c
                });
                false
            }
            Verdict::Rejected => {
                let until = (Utc::now() + chrono::Duration::days(GRACE_DAYS))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                self.store(Cache {
                    tier: "grace".into(),
                    grace_until: Some(until),
                    last_check_at: Some(super::now_utc()),
                    ..c
                });
                true
            }
            Verdict::Unreachable(_) => false,
        }
    }
}

fn free() -> Cache {
    Cache {
        tier: "free".into(),
        ..Cache::default()
    }
}

fn parse(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `****-****-****-A1B2`: only the last four letters or digits stay visible.
pub fn mask(key: &str) -> String {
    let tail: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("****-****-****-{tail}")
}

/// One POST to Gumroad's licence verify endpoint, classified per §9.3.
pub fn verify(url: &str, product_id: &str, key: &str, increment: bool) -> Verdict {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Verdict::Unreachable(e.to_string()),
    };
    let form = [
        ("product_id", product_id),
        ("license_key", key),
        (
            "increment_uses_count",
            if increment { "true" } else { "false" },
        ),
    ];
    let resp = match client.post(url).form(&form).send() {
        Ok(r) => r,
        Err(e) => return Verdict::Unreachable(e.to_string()),
    };
    let status = resp.status();
    if status.as_u16() == 429 || status.is_server_error() {
        return Verdict::Unreachable(format!("HTTP {status}"));
    }
    let Ok(body) = resp.json::<serde_json::Value>() else {
        return Verdict::Unreachable(format!("HTTP {status}: not Gumroad's JSON"));
    };
    classify(&body)
        .unwrap_or_else(|| Verdict::Unreachable(format!("HTTP {status}: not Gumroad's JSON")))
}

/// Rule 4: `success: false`, or `success: true` with the purchase refunded or
/// chargebacked, is a rejection; `success: true` otherwise is valid; a body
/// without a boolean `success` is not an answer.
pub fn classify(body: &serde_json::Value) -> Option<Verdict> {
    let success = body.get("success")?.as_bool()?;
    if !success {
        return Some(Verdict::Rejected);
    }
    let purchase = body.get("purchase");
    let flag = |k: &str| {
        purchase
            .and_then(|p| p.get(k))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    if flag("refunded") || flag("chargebacked") || flag("disputed") {
        return Some(Verdict::Rejected);
    }
    Some(Verdict::Valid)
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
    use serde_json::json;

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn classification_follows_rule_4() {
        assert_eq!(
            classify(&json!({"success": true, "purchase": {"refunded": false}})),
            Some(Verdict::Valid)
        );
        assert_eq!(
            classify(&json!({"success": false, "message": "That license does not exist"})),
            Some(Verdict::Rejected)
        );
        assert_eq!(
            classify(&json!({"success": true, "purchase": {"refunded": true}})),
            Some(Verdict::Rejected)
        );
        assert_eq!(
            classify(&json!({"success": true, "purchase": {"chargebacked": true}})),
            Some(Verdict::Rejected)
        );
        assert_eq!(classify(&json!({"error": "oops"})), None);
        assert_eq!(classify(&json!("<html>")), None);
    }

    #[test]
    fn cache_is_integrity_protected_and_needs_the_key() {
        let dir =
            std::env::temp_dir().join(format!("mr-licence-{}", uuid::Uuid::new_v4().simple()));
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::default());
        let config = GumroadConfig {
            product_id: None,
            verify_url: String::new(),
            product_url: "u".into(),
            price_display: None,
        };
        let l = Licenser::new(dir.clone(), secrets.clone(), config.clone());
        assert_eq!(l.status().tier, LicenseTier::Free);
        secrets.set(KEY_ENTRY, "ABCD-1234").unwrap();
        l.store(Cache {
            tier: "licensed".into(),
            key_masked: Some(mask("ABCD-1234")),
            verified_at: Some(crate::engine::now_utc()),
            last_check_at: Some(crate::engine::now_utc()),
            ..Cache::default()
        });
        assert!(Licenser::new(dir.clone(), secrets.clone(), config.clone()).unlocked());
        // Editing the file by hand breaks the HMAC: free tier.
        let path = dir.join(CACHE_FILE);
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("2026", "2099");
        std::fs::write(&path, text).unwrap();
        assert!(!Licenser::new(dir.clone(), secrets.clone(), config.clone()).unlocked());
        // No key in the keychain: free tier, whatever the cache says.
        l.store(Cache {
            tier: "licensed".into(),
            ..Cache::default()
        });
        secrets.delete(KEY_ENTRY);
        assert!(!Licenser::new(dir.clone(), secrets, config).unlocked());
        assert_eq!(
            mask("A1B2C3D4-E5F6A7B8-C9D0E1F2-A3B4C5D6"),
            "****-****-****-C5D6"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn activation_without_a_configured_product_is_offline_not_invalid() {
        let dir =
            std::env::temp_dir().join(format!("mr-licence-{}", uuid::Uuid::new_v4().simple()));
        let l = Licenser::new(
            dir.clone(),
            Arc::new(MemorySecrets::default()),
            GumroadConfig {
                product_id: None,
                verify_url: String::new(),
                product_url: "u".into(),
                price_display: None,
            },
        );
        assert_eq!(
            l.activate("KEY").unwrap_err().code,
            ErrorCode::LicenseOffline
        );
        assert_eq!(
            l.activate("   ").unwrap_err().code,
            ErrorCode::LicenseInvalid
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
