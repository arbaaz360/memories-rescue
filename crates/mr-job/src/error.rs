//! Errors of the engine API. Per-item failures are not errors: they are
//! recorded on the item and the job carries on (NFR-REL-03).

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// `INPUT_NOT_EXPORT`: nothing among the inputs looks like an export.
    #[error("no Snapchat export found among the inputs ({0})")]
    NoExport(String),
    #[error("the inputs hold {0} different exports; start one job per export")]
    MultipleExports(usize),
    #[error("unknown home time zone {0:?}")]
    TimeZone(String),
    #[error("no job {0} in this state directory")]
    NoSuchJob(String),
    /// The output folder already belongs to a job: resume it instead.
    #[error("the output folder is already used by job {job_id}; resume it instead")]
    OutputInUse { job_id: String },
    /// Another process or thread is running this job right now.
    #[error("job {job_id} is already running")]
    Busy { job_id: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("job state: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("job state: {0}")]
    State(String),
}

impl JobError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> JobError {
        JobError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, JobError>;
