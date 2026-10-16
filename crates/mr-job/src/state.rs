//! Job state in SQLite (SRS section 3.8, FR-33), WAL mode, one file per job
//! in the per-user state directory.
//!
//! The section 3.8 columns are kept as named; the full plan of an item lives
//! beside them in `plan_json` so the schema does not have to grow a column
//! for every planning detail. Only the coordinator thread touches the
//! connection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};

use crate::control::Stage;
use crate::error::{JobError, Result};
use crate::fingerprint::PartFingerprint;
use crate::options::Options;
use crate::plan::{ExtraRow, PlanPart, PlannedItem, Warning};

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS job (
    id TEXT PRIMARY KEY, created_utc TEXT, app_version TEXT, options_json TEXT,
    input_fingerprint TEXT, output_path TEXT, format_profile TEXT, status TEXT,
    inputs_json TEXT, root_key TEXT, missing_parts_json TEXT, warnings_json TEXT,
    tz_data_version TEXT, home_zone TEXT);
CREATE TABLE IF NOT EXISTS part (
    path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER, sha256_head TEXT,
    role TEXT, status TEXT, label TEXT, number INTEGER, truncated INTEGER, zip INTEGER);
CREATE TABLE IF NOT EXISTS item (
    id TEXT PRIMARY KEY, date_utc TEXT, media_type TEXT, lat REAL, lon REAL,
    match_conf TEXT, location_decision TEXT,
    main_part TEXT, main_entry TEXT, overlay_part TEXT, overlay_entry TEXT,
    stage TEXT, attempts INTEGER, last_error_code TEXT, output_path TEXT, output_sha256 TEXT,
    chrono_rank INTEGER, last_error_detail TEXT, overlay_output TEXT,
    originals_main TEXT, originals_overlay TEXT, composite_pending INTEGER,
    previous_outputs TEXT, done_utc TEXT, seconds REAL, plan_json TEXT);
CREATE INDEX IF NOT EXISTS item_stage ON item(stage, chrono_rank);
CREATE TABLE IF NOT EXISTS extra (
    rowid INTEGER PRIMARY KEY, kind TEXT, json_index INTEGER, source_json TEXT,
    media_type TEXT, date_utc TEXT, detail TEXT);
CREATE TABLE IF NOT EXISTS clip_group (group_id TEXT, item_id TEXT, seq INTEGER);
CREATE TABLE IF NOT EXISTS journal (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, item_id TEXT, event TEXT, at_utc TEXT);
"#;

/// The job row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRow {
    pub id: String,
    pub created_utc: String,
    pub app_version: String,
    pub options: Options,
    pub input_fingerprint: String,
    pub output_path: String,
    pub format_profile: String,
    pub status: String,
    pub inputs: Vec<PathBuf>,
    pub root_key: String,
    pub missing_parts: Vec<u32>,
    pub warnings: Vec<Warning>,
    pub tz_data_version: String,
    pub home_zone: Option<String>,
}

/// One part as recorded at creation (or at the last add/re-index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRow {
    pub fingerprint: PartFingerprint,
    pub label: String,
    pub number: Option<u32>,
    pub primary: bool,
    pub truncated: bool,
    pub zip: bool,
}

impl PartRow {
    pub fn info(&self) -> PlanPart {
        PlanPart {
            path: self.fingerprint.path.clone(),
            label: self.label.clone(),
            number: self.number,
            primary: self.primary,
            truncated: self.truncated,
            zip: self.zip,
        }
    }
}

/// One item: its plan and where it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRow {
    pub plan: PlannedItem,
    pub stage: Stage,
    pub attempts: u32,
    pub last_error_code: Option<String>,
    pub last_error_detail: Option<String>,
    /// The main output actually written (relative), which can differ from
    /// the planned name after a collision at write time.
    pub output_path: Option<String>,
    pub output_sha256: Option<String>,
    pub overlay_output: Option<String>,
    pub originals_main: Option<String>,
    pub originals_overlay: Option<String>,
    pub composite_pending: bool,
    /// Outputs of an earlier write to remove once the item is written again.
    pub previous_outputs: Vec<String>,
    pub done_utc: Option<String>,
    pub seconds: Option<f64>,
}

impl ItemRow {
    pub fn new(plan: PlannedItem, stage: Stage) -> ItemRow {
        ItemRow {
            plan,
            stage,
            attempts: 0,
            last_error_code: None,
            last_error_detail: None,
            output_path: None,
            output_sha256: None,
            overlay_output: None,
            originals_main: None,
            originals_overlay: None,
            composite_pending: false,
            previous_outputs: Vec::new(),
            done_utc: None,
            seconds: None,
        }
    }

    pub fn key(&self) -> &str {
        &self.plan.key
    }

    /// Every output file this item has on record (relative paths).
    pub fn recorded_outputs(&self) -> Vec<String> {
        [
            &self.output_path,
            &self.overlay_output,
            &self.originals_main,
            &self.originals_overlay,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect()
    }
}

/// What a finished item wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finished {
    pub stage: Option<Stage>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub output_path: Option<String>,
    pub output_sha256: Option<String>,
    pub overlay_output: Option<String>,
    pub originals_main: Option<String>,
    pub originals_overlay: Option<String>,
    pub composite_pending: bool,
    pub seconds: f64,
}

/// A journal event (`start`, `done`, `failed`, `requeue`, `adopted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub seq: i64,
    pub item: String,
    pub event: String,
    pub at_utc: String,
}

/// Why a state file could not be used.
#[derive(Debug)]
pub enum OpenError {
    /// `JOB_STATE_CORRUPT`: not a database, or `integrity_check` failed.
    Corrupt(String),
    Other(JobError),
}

pub struct Db {
    conn: Connection,
    pub path: PathBuf,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("path", &self.path).finish()
    }
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serialisable")
}

fn from_json<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| JobError::State(format!("bad JSON in state: {e}")))
}

impl Db {
    fn configure(conn: &Connection) -> rusqlite::Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // WAL + NORMAL survives a killed process (FR-33 AC1); a power cut can
        // lose the last checkpoints, which only re-queues those items, and
        // their outputs are found intact by hash on the next run.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// A new, empty state file. Fails if one exists.
    pub fn create(path: &Path) -> Result<Db> {
        if path.exists() {
            return Err(JobError::State(format!(
                "{} already exists",
                path.display()
            )));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| JobError::io(dir, e))?;
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(Db {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Open an existing state file and check its integrity.
    pub fn open(path: &Path) -> std::result::Result<Db, OpenError> {
        if !path.is_file() {
            return Err(OpenError::Other(JobError::State(format!(
                "{} is missing",
                path.display()
            ))));
        }
        let conn = Connection::open(path).map_err(|e| OpenError::Corrupt(e.to_string()))?;
        let check: rusqlite::Result<String> =
            conn.query_row("PRAGMA integrity_check", [], |r| r.get(0));
        match check {
            Ok(s) if s == "ok" => {}
            Ok(s) => return Err(OpenError::Corrupt(s)),
            Err(e) => return Err(OpenError::Corrupt(e.to_string())),
        }
        let has_job: rusqlite::Result<i64> =
            conn.query_row("SELECT count(*) FROM job", [], |r| r.get(0));
        match has_job {
            Ok(1) => {}
            Ok(n) => return Err(OpenError::Corrupt(format!("{n} job rows"))),
            Err(e) => return Err(OpenError::Corrupt(e.to_string())),
        }
        Self::configure(&conn).map_err(|e| OpenError::Corrupt(e.to_string()))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| OpenError::Corrupt(e.to_string()))?;
        Ok(Db {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn put_job(&self, j: &JobRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO job(id, created_utc, app_version, options_json, input_fingerprint,
                output_path, format_profile, status, inputs_json, root_key, missing_parts_json,
                warnings_json, tz_data_version, home_zone)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                j.id,
                j.created_utc,
                j.app_version,
                json(&j.options),
                j.input_fingerprint,
                j.output_path,
                j.format_profile,
                j.status,
                json(&j.inputs),
                j.root_key,
                json(&j.missing_parts),
                json(&j.warnings),
                j.tz_data_version,
                j.home_zone,
            ],
        )?;
        Ok(())
    }

    pub fn job(&self) -> Result<JobRow> {
        let row = self.conn.query_row(
            "SELECT id, created_utc, app_version, options_json, input_fingerprint, output_path,
                    format_profile, status, inputs_json, root_key, missing_parts_json,
                    warnings_json, tz_data_version, home_zone FROM job",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, String>(12)?,
                    r.get::<_, Option<String>>(13)?,
                ))
            },
        )?;
        Ok(JobRow {
            id: row.0,
            created_utc: row.1,
            app_version: row.2,
            options: from_json(&row.3)?,
            input_fingerprint: row.4,
            output_path: row.5,
            format_profile: row.6,
            status: row.7,
            inputs: from_json(&row.8)?,
            root_key: row.9,
            missing_parts: from_json(&row.10)?,
            warnings: from_json(&row.11)?,
            tz_data_version: row.12,
            home_zone: row.13,
        })
    }

    pub fn set_status(&self, status: &str) -> Result<()> {
        self.conn
            .execute("UPDATE job SET status = ?1", params![status])?;
        Ok(())
    }

    pub fn replace_parts(&mut self, parts: &[PartRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM part", [])?;
        {
            let mut st = tx.prepare(
                "INSERT INTO part(path, size, mtime, sha256_head, role, status, label, number, truncated, zip)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6, ?7, ?8, ?9)",
            )?;
            for p in parts {
                st.execute(params![
                    p.fingerprint.path,
                    p.fingerprint.size as i64,
                    p.fingerprint.mtime,
                    p.fingerprint.sha256_head,
                    if p.primary { "primary" } else { "extra" },
                    p.label,
                    p.number,
                    p.truncated,
                    p.zip,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn parts(&self) -> Result<Vec<PartRow>> {
        let mut st = self.conn.prepare(
            "SELECT path, size, mtime, sha256_head, role, label, number, truncated, zip FROM part ORDER BY number, path",
        )?;
        let rows = st.query_map([], |r| {
            Ok(PartRow {
                fingerprint: PartFingerprint {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    mtime: r.get(2)?,
                    sha256_head: r.get(3)?,
                },
                primary: r.get::<_, String>(4)? == "primary",
                label: r.get(5)?,
                number: r.get(6)?,
                truncated: r.get(7)?,
                zip: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_items(&mut self, items: &[ItemRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO item(id, date_utc, media_type, lat, lon, match_conf,
                    location_decision, main_part, main_entry, overlay_part, overlay_entry, stage,
                    attempts, last_error_code, output_path, output_sha256, chrono_rank,
                    last_error_detail, overlay_output, originals_main, originals_overlay,
                    composite_pending, previous_outputs, done_utc, seconds, plan_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                    ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            )?;
            for it in items {
                let p = &it.plan;
                st.execute(params![
                    p.key,
                    p.capture_utc.to_rfc3339(),
                    match p.kind {
                        crate::compositor::MediaKind::Photo => "photo",
                        crate::compositor::MediaKind::Video => "video",
                    },
                    p.gps.map(|g| g.0),
                    p.gps.map(|g| g.1),
                    p.confidence,
                    p.location_decision,
                    p.main.part_path,
                    p.main.entry_name,
                    p.overlay.as_ref().map(|o| o.part_path.clone()),
                    p.overlay.as_ref().map(|o| o.entry_name.clone()),
                    it.stage.as_str(),
                    it.attempts,
                    it.last_error_code,
                    it.output_path,
                    it.output_sha256,
                    p.chrono_rank as i64,
                    it.last_error_detail,
                    it.overlay_output,
                    it.originals_main,
                    it.originals_overlay,
                    it.composite_pending,
                    json(&it.previous_outputs),
                    it.done_utc,
                    it.seconds,
                    json(p),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn item_from(r: &Row<'_>) -> rusqlite::Result<(String, ItemRowRaw)> {
        Ok((
            r.get(0)?,
            ItemRowRaw {
                stage: r.get(1)?,
                attempts: r.get(2)?,
                last_error_code: r.get(3)?,
                last_error_detail: r.get(4)?,
                output_path: r.get(5)?,
                output_sha256: r.get(6)?,
                overlay_output: r.get(7)?,
                originals_main: r.get(8)?,
                originals_overlay: r.get(9)?,
                composite_pending: r.get(10)?,
                previous_outputs: r.get(11)?,
                done_utc: r.get(12)?,
                seconds: r.get(13)?,
                plan_json: r.get(14)?,
            },
        ))
    }

    const ITEM_COLUMNS: &'static str =
        "id, stage, attempts, last_error_code, last_error_detail, output_path,
        output_sha256, overlay_output, originals_main, originals_overlay, composite_pending,
        previous_outputs, done_utc, seconds, plan_json";

    fn query_items(&self, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<ItemRow>> {
        let sql = format!(
            "SELECT {} FROM item {filter} ORDER BY chrono_rank, id",
            Self::ITEM_COLUMNS
        );
        let mut st = self.conn.prepare(&sql)?;
        let raws = st
            .query_map(args, Self::item_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter().map(|(_, raw)| raw.into_row()).collect()
    }

    pub fn items(&self) -> Result<Vec<ItemRow>> {
        self.query_items("", &[])
    }

    pub fn items_in(&self, stage: Stage) -> Result<Vec<ItemRow>> {
        self.query_items("WHERE stage = ?1", &[&stage.as_str()])
    }

    pub fn item(&self, key: &str) -> Result<Option<ItemRow>> {
        Ok(self.query_items("WHERE id = ?1", &[&key])?.pop())
    }

    pub fn delete_items(&mut self, keys: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for k in keys {
            tx.execute("DELETE FROM item WHERE id = ?1", params![k])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_stage(&mut self, keys: &[String], stage: Stage) -> Result<()> {
        let tx = self.conn.transaction()?;
        for k in keys {
            tx.execute(
                "UPDATE item SET stage = ?2 WHERE id = ?1",
                params![k, stage.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Counts per stage.
    pub fn counts(&self) -> Result<BTreeMap<String, usize>> {
        let mut st = self
            .conn
            .prepare("SELECT stage, count(*) FROM item GROUP BY stage")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Checkpoint the start of an attempt (attempts + 1, journal `start`).
    pub fn mark_started(&mut self, key: &str) -> Result<u32> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE item SET attempts = attempts + 1 WHERE id = ?1",
            params![key],
        )?;
        tx.execute(
            "INSERT INTO journal(item_id, event, at_utc) VALUES (?1, 'start', ?2)",
            params![key, now()],
        )?;
        let attempts: u32 = tx.query_row(
            "SELECT attempts FROM item WHERE id = ?1",
            params![key],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(attempts)
    }

    /// Checkpoint a finished attempt (FR-33).
    pub fn mark_finished(&mut self, key: &str, f: &Finished) -> Result<()> {
        let stage = f.stage.unwrap_or(Stage::Done);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE item SET stage = ?2, last_error_code = ?3, last_error_detail = ?4,
                output_path = COALESCE(?5, output_path), output_sha256 = COALESCE(?6, output_sha256),
                overlay_output = ?7, originals_main = ?8, originals_overlay = ?9,
                composite_pending = ?10, done_utc = ?11, seconds = ?12,
                previous_outputs = CASE WHEN ?5 IS NULL THEN previous_outputs ELSE '[]' END
             WHERE id = ?1",
            params![
                key,
                stage.as_str(),
                f.error_code,
                f.error_detail,
                f.output_path,
                f.output_sha256,
                f.overlay_output,
                f.originals_main,
                f.originals_overlay,
                f.composite_pending,
                now(),
                f.seconds,
            ],
        )?;
        tx.execute(
            "INSERT INTO journal(item_id, event, at_utc) VALUES (?1, ?2, ?3)",
            params![
                key,
                if stage == Stage::Done {
                    "done"
                } else {
                    "failed"
                },
                now()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Return an item to the queue without counting it as failed.
    pub fn mark_requeued(&mut self, key: &str, reason: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE item SET stage = 'pending', attempts = MAX(attempts - 1, 0) WHERE id = ?1",
            params![key],
        )?;
        tx.execute(
            "INSERT INTO journal(item_id, event, at_utc) VALUES (?1, ?2, ?3)",
            params![key, format!("requeue:{reason}"), now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn journal_event(&self, key: &str, event: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal(item_id, event, at_utc) VALUES (?1, ?2, ?3)",
            params![key, event, now()],
        )?;
        Ok(())
    }

    pub fn journal(&self) -> Result<Vec<JournalEvent>> {
        let mut st = self
            .conn
            .prepare("SELECT seq, item_id, event, at_utc FROM journal ORDER BY seq")?;
        let rows = st.query_map([], |r| {
            Ok(JournalEvent {
                seq: r.get(0)?,
                item: r.get(1)?,
                event: r.get(2)?,
                at_utc: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn last_journal_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT max(seq) FROM journal", [], |r| {
                r.get::<_, Option<i64>>(0)
            })?
            .unwrap_or(0))
    }

    pub fn replace_extras(&mut self, extras: &[ExtraRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM extra", [])?;
        {
            let mut st = tx.prepare(
                "INSERT INTO extra(kind, json_index, source_json, media_type, date_utc, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for e in extras {
                st.execute(params![
                    e.kind.as_str(),
                    e.json_index.map(|i| i as i64),
                    e.source.as_ref().map(json),
                    e.media_type,
                    e.date_utc,
                    e.detail,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn extras(&self) -> Result<Vec<ExtraRow>> {
        let mut st = self.conn.prepare(
            "SELECT kind, json_index, source_json, media_type, date_utc, detail FROM extra ORDER BY rowid",
        )?;
        let raws = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter()
            .map(|(kind, idx, src, mt, date, detail)| {
                Ok(ExtraRow {
                    kind: crate::plan::ExtraKind::parse(&kind)
                        .ok_or_else(|| JobError::State(format!("unknown extra kind {kind}")))?,
                    json_index: idx.map(|i| i as usize),
                    source: src.as_deref().map(from_json).transpose()?,
                    media_type: mt,
                    date_utc: date,
                    detail,
                })
            })
            .collect()
    }

    /// Checkpoint the WAL into the main file (on clean shutdown).
    pub fn checkpoint(&self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

struct ItemRowRaw {
    stage: String,
    attempts: u32,
    last_error_code: Option<String>,
    last_error_detail: Option<String>,
    output_path: Option<String>,
    output_sha256: Option<String>,
    overlay_output: Option<String>,
    originals_main: Option<String>,
    originals_overlay: Option<String>,
    composite_pending: Option<bool>,
    previous_outputs: Option<String>,
    done_utc: Option<String>,
    seconds: Option<f64>,
    plan_json: String,
}

impl ItemRowRaw {
    fn into_row(self) -> Result<ItemRow> {
        Ok(ItemRow {
            plan: from_json(&self.plan_json)?,
            stage: Stage::parse(&self.stage)
                .ok_or_else(|| JobError::State(format!("unknown stage {}", self.stage)))?,
            attempts: self.attempts,
            last_error_code: self.last_error_code,
            last_error_detail: self.last_error_detail,
            output_path: self.output_path,
            output_sha256: self.output_sha256,
            overlay_output: self.overlay_output,
            originals_main: self.originals_main,
            originals_overlay: self.originals_overlay,
            composite_pending: self.composite_pending.unwrap_or(false),
            previous_outputs: self
                .previous_outputs
                .as_deref()
                .map(from_json)
                .transpose()?
                .unwrap_or_default(),
            done_utc: self.done_utc,
            seconds: self.seconds,
        })
    }
}

/// Read a job's row without keeping the database open (for `list_jobs`).
pub fn peek(path: &Path) -> Option<(JobRow, BTreeMap<String, usize>)> {
    let db = Db::open(path).ok()?;
    let job = db.job().ok()?;
    let counts = db.counts().ok()?;
    Some((job, counts))
}

/// `true` when the file opens and passes `integrity_check`.
pub fn is_healthy(path: &Path) -> bool {
    Db::open(path).is_ok()
}
