//! `add_inputs` / `remove_input` (FR-01 to FR-03): the window's list of
//! export files, classified with `mr_core::discover` over the whole set so
//! part numbers and duplicates are decided from content, not names alone.

use std::path::{Path, PathBuf};

use crate::ipc::{
    ErrorCode, InputFile, InputKind, InputRole, InputStatus, IpcError, IpcResult, Issue,
};

use super::{Engine, file_name};

#[derive(Debug, Default)]
pub struct Registry {
    next: u64,
    pub(crate) list: Vec<InputFile>,
}

impl Registry {
    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        self.list
            .iter()
            .find(|i| i.id == id)
            .map(|i| PathBuf::from(&i.path))
    }

    pub fn get(&self, id: &str) -> Option<&InputFile> {
        self.list.iter().find(|i| i.id == id)
    }

    fn add(&mut self, path: &Path) -> String {
        let p = path.to_string_lossy().into_owned();
        if let Some(existing) = self.list.iter().find(|i| i.path == p) {
            return existing.id.clone();
        }
        self.next += 1;
        let id = format!("in-{}", self.next);
        self.list.push(InputFile {
            id: id.clone(),
            path: p,
            name: file_name(path),
            size_bytes: 0,
            kind: kind_of(path),
            role: InputRole::Unknown,
            part_number: None,
            status: InputStatus::Pending,
            issue: None,
        });
        id
    }
}

fn kind_of(path: &Path) -> InputKind {
    if path.is_dir() {
        InputKind::Folder
    } else if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        InputKind::Zip
    } else {
        InputKind::Other
    }
}

/// Size on disk; folders are summed (bounded so a huge tree cannot stall the list).
fn size_of(path: &Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > 200_000 {
                return total;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

fn issue_of(i: &mr_core::Issue) -> Issue {
    Issue {
        code: ErrorCode::from_engine(i.code.as_str()),
        message: i.detail.clone(),
        count: None,
        part_number: None,
    }
}

/// Re-classifies every input from one discovery over the whole set.
pub fn classify(list: &mut [InputFile]) {
    let paths: Vec<PathBuf> = list.iter().map(|i| PathBuf::from(&i.path)).collect();
    let d = mr_core::discover(&paths);
    for input in list.iter_mut() {
        let path = PathBuf::from(&input.path);
        input.kind = kind_of(&path);
        input.size_bytes = size_of(&path);
        input.issue = None;
        if d.ignored.iter().any(|g| g.path == path) {
            input.role = InputRole::Ignored;
            input.part_number = None;
            input.status = InputStatus::NotExport;
            input.issue = Some(Issue {
                code: ErrorCode::InputNotExport,
                message: "This doesn't look like a Snapchat export.".into(),
                count: None,
                part_number: None,
            });
            continue;
        }
        // The part this input is, or the parts inside it (a folder of ZIPs).
        let parts: Vec<&mr_core::Part> = d
            .parts
            .iter()
            .filter(|p| p.path == path || p.path.starts_with(&path))
            .collect();
        if parts.is_empty() {
            input.role = InputRole::Ignored;
            input.part_number = None;
            input.status = InputStatus::NotExport;
            input.issue = Some(Issue {
                code: ErrorCode::InputNotExport,
                message: "This doesn't look like a Snapchat export.".into(),
                count: None,
                part_number: None,
            });
            continue;
        }
        let primary = parts
            .iter()
            .any(|p| p.has_json || d.exports.iter().any(|e| e.primary == Some(p.id)));
        input.role = if primary {
            InputRole::Primary
        } else if parts.iter().any(|p| p.number().is_some()) {
            InputRole::Part
        } else {
            InputRole::Unknown
        };
        input.part_number = parts.iter().filter_map(|p| p.number()).min();
        input.status = if parts.iter().all(|p| p.duplicate_of.is_some()) {
            InputStatus::Duplicate
        } else if parts.iter().any(|p| p.is_truncated()) {
            InputStatus::Truncated
        } else {
            InputStatus::Ok
        };
        input.issue = parts
            .iter()
            .flat_map(|p| p.issues.iter())
            .next()
            .map(issue_of);
        if input.status == InputStatus::Duplicate {
            input.issue = Some(Issue {
                code: ErrorCode::InputNotExport,
                message: "Same file as another part; it is read once.".into(),
                count: None,
                part_number: input.part_number,
            });
        }
    }
}

impl Engine {
    pub fn add_inputs(&self, paths: &[String]) -> IpcResult<Vec<InputFile>> {
        let mut reg = self.inputs.lock().expect("inputs");
        for p in paths {
            let path = PathBuf::from(p);
            if !path.exists() {
                continue;
            }
            reg.add(&path);
        }
        classify(&mut reg.list);
        Ok(reg.list.clone())
    }

    pub fn remove_input(&self, id: &str) {
        let mut reg = self.inputs.lock().expect("inputs");
        reg.list.retain(|i| i.id != id);
        classify(&mut reg.list);
    }

    /// Registers one more input (add_part) and returns it classified.
    pub(crate) fn register_input(&self, path: &Path) -> InputFile {
        let mut reg = self.inputs.lock().expect("inputs");
        let id = reg.add(path);
        classify(&mut reg.list);
        reg.get(&id).cloned().expect("just added")
    }

    /// Paths of usable inputs, in the order given.
    pub(crate) fn input_paths(&self, ids: &[String]) -> IpcResult<Vec<PathBuf>> {
        let reg = self.inputs.lock().expect("inputs");
        let mut out = Vec::new();
        for id in ids {
            match reg.get(id) {
                Some(i)
                    if matches!(
                        i.status,
                        InputStatus::Ok | InputStatus::Truncated | InputStatus::Duplicate
                    ) =>
                {
                    out.push(PathBuf::from(&i.path))
                }
                Some(_) => {}
                None => {
                    return Err(IpcError::new(
                        ErrorCode::InputNotExport,
                        "One of the files is no longer in the list.",
                    )
                    .with_detail(id.clone()));
                }
            }
        }
        if out.is_empty() {
            return Err(IpcError::new(
                ErrorCode::InputNotExport,
                "None of these files is a Snapchat export.",
            ));
        }
        Ok(out)
    }

    pub(crate) fn input_by_path(&self, path: &Path) -> Option<InputFile> {
        let p = path.to_string_lossy();
        self.inputs
            .lock()
            .expect("inputs")
            .list
            .iter()
            .find(|i| i.path == p)
            .cloned()
    }
}
