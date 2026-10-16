//! What `open_path` may open: only paths the engine itself handed out.
//!
//! The window names a path and the system shell opens it, and the shell runs
//! an executable or a script as readily as it opens a folder. So a window
//! that was compromised (a script injected into the page) must not be able
//! to choose freely. Allowed, after resolving links and `..`:
//!
//! * a known job's output root (Open folder), and inside it only that job's
//!   report files and the joined videos it recorded;
//! * a preview report or diagnostics ZIP written in this session;
//! * the bundled licence and third-party notices (About);
//! * the app's own log folder.
//!
//! Everything else is refused, and the refusal is logged.

use std::path::{Path, PathBuf};

use crate::ipc::{ErrorCode, IpcError, IpcResult};

use super::{Engine, plain_path, redact_home};

/// Report files a job's `_report` folder may hold.
const REPORT_FILES: [&str; 3] = ["report.html", "report.csv", "preview-report.html"];

impl Engine {
    /// Remember a file this session wrote for the user to open.
    pub(crate) fn issue_path(&self, path: &Path) {
        self.issued.lock().expect("issued").push(path.to_path_buf());
    }

    /// The path to hand to the shell, if `requested` is one the engine gave out.
    pub fn openable(&self, requested: &str) -> IpcResult<PathBuf> {
        let plain = PathBuf::from(plain_path(requested));
        let target = std::fs::canonicalize(&plain).map_err(|_| {
            IpcError::new(
                ErrorCode::PartMissing,
                "That file or folder doesn't exist (any more).",
            )
            .with_detail(redact_home(requested))
        })?;
        if self.allowed(&target) {
            return Ok(PathBuf::from(plain_path(&target.to_string_lossy())));
        }
        tracing::warn!(path = %redact_home(requested), "refused to open a path the engine did not hand out");
        Err(
            IpcError::unknown("Memories Rescue only opens its own folders and files.")
                .with_detail(redact_home(requested)),
        )
    }

    fn allowed(&self, target: &Path) -> bool {
        let same = |p: &Path| std::fs::canonicalize(p).is_ok_and(|c| c == target);
        let info = self.app_info();
        if same(Path::new(&info.licenses_path))
            || same(Path::new(&info.third_party_notices_path))
            || same(&self.paths.log_dir)
        {
            return true;
        }
        if self.issued.lock().expect("issued").iter().any(|p| same(p)) {
            return true;
        }
        for j in mr_job::list_jobs(self.state_dir()) {
            let root = PathBuf::from(&j.output_root);
            let Ok(canonical_root) = std::fs::canonicalize(&root) else {
                continue;
            };
            if target == canonical_root {
                return true;
            }
            if !target.starts_with(&canonical_root) {
                continue;
            }
            let reports = root.join(mr_job::layout::REPORT_DIR);
            if REPORT_FILES.iter().any(|n| same(&reports.join(n))) {
                return true;
            }
            let joined = mr_job::Job::open(self.state_dir(), &j.id)
                .and_then(|job| job.clip_groups())
                .unwrap_or_default();
            if joined
                .iter()
                .filter_map(|c| c.output_path.as_deref())
                .any(|rel| same(&mr_job::layout::to_fs(&root, rel)))
            {
                return true;
            }
        }
        false
    }
}
