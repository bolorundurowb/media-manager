//! Append-only JSONL journal (§6.4, §6.7).
//!
//! Two writes per operation: `intent` before the syscall, `outcome` after.
//! Every line carries `run_id`, `root`, and `config_digest` so unmatched
//! intents can be attributed to a root and a plan. Tests must pass an
//! explicit directory — never the real user data dir.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mm_core::error::FatalReason;
use mm_core::plan::Plan;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Intent vs outcome phase of a journaled operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Intent,
    Outcome,
}

/// Kind of journaled filesystem operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JournalOp {
    Move,
    DirCreate,
    DirRename,
    DirRemove,
    Reclaim,
}

/// One JSONL record. Outcome lines reuse `seq` from the matching intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub ts: String,
    pub run_id: Uuid,
    pub root: PathBuf,
    pub config_digest: String,
    pub seq: u64,
    pub phase: JournalPhase,
    pub op: JournalOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Append-only journal file plus an in-memory index of records.
pub struct Journal {
    path: PathBuf,
    dir: PathBuf,
    file: File,
    entries: Vec<JournalEntry>,
    next_seq: u64,
    run_id: Uuid,
    root: PathBuf,
    config_digest: String,
}

impl Journal {
    /// Create the journal file (and parent dirs) if needed, then fsync.
    /// Failure is [`FatalReason::JournalUnwritable`].
    pub fn create(path: impl AsRef<Path>) -> Result<Self, FatalReason> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| unwritable(path, &e))?;
        }
        open_inner(path, true)
    }

    /// Open an existing journal. Missing/unreadable is Fatal.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FatalReason> {
        open_inner(path.as_ref(), false)
    }

    /// Bind subsequent writes to a run. Call once after [`create`] for execute.
    pub fn bind(&mut self, run_id: Uuid, root: PathBuf, config_digest: String) {
        self.run_id = run_id;
        self.root = root;
        self.config_digest = config_digest;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Persist `plan` next to the journal at `{dir}/plans/{run_id}.json`.
    pub fn persist_plan(&self, plan: &Plan) -> Result<PathBuf, FatalReason> {
        let plans_dir = self.dir.join("plans");
        std::fs::create_dir_all(&plans_dir).map_err(|e| unwritable(&plans_dir, &e))?;
        let dest = plans_dir.join(format!("{}.json", plan.run_id));
        let json = serde_json::to_vec_pretty(plan).map_err(|e| unwritable(&dest, &e))?;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&dest)
            .map_err(|e| unwritable(&dest, &e))?;
        f.write_all(&json).map_err(|e| unwritable(&dest, &e))?;
        f.sync_all().map_err(|e| unwritable(&dest, &e))?;
        Ok(dest)
    }

    /// Write an intent line and fsync. Returns the assigned `seq`.
    pub fn write_intent(
        &mut self,
        op: JournalOp,
        from: Option<&Path>,
        to: Option<&Path>,
    ) -> Result<u64, FatalReason> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let entry = JournalEntry {
            ts: rfc3339_now(),
            run_id: self.run_id,
            root: self.root.clone(),
            config_digest: self.config_digest.clone(),
            seq,
            phase: JournalPhase::Intent,
            op,
            from: from.map(Path::to_path_buf),
            to: to.map(Path::to_path_buf),
            status: None,
            bytes: None,
            message: None,
        };
        self.append(&entry)?;
        Ok(seq)
    }

    /// Write an outcome line for `seq` and fsync.
    #[allow(clippy::too_many_arguments)]
    pub fn write_outcome(
        &mut self,
        seq: u64,
        op: JournalOp,
        from: Option<&Path>,
        to: Option<&Path>,
        status: &str,
        bytes: Option<u64>,
        message: Option<&str>,
    ) -> Result<(), FatalReason> {
        let entry = JournalEntry {
            ts: rfc3339_now(),
            run_id: self.run_id,
            root: self.root.clone(),
            config_digest: self.config_digest.clone(),
            seq,
            phase: JournalPhase::Outcome,
            op,
            from: from.map(Path::to_path_buf),
            to: to.map(Path::to_path_buf),
            status: Some(status.to_string()),
            bytes,
            message: message.map(str::to_string),
        };
        self.append(&entry)
    }

    /// Intents with no matching outcome, optionally scoped to `root`.
    pub fn unmatched_intents(&self, root: Option<&Path>) -> Vec<JournalEntry> {
        let mut unmatched = Vec::new();
        for e in &self.entries {
            if e.phase != JournalPhase::Intent {
                continue;
            }
            if let Some(root) = root
                && e.root != root
            {
                continue;
            }
            let has_outcome = self.entries.iter().any(|o| {
                o.phase == JournalPhase::Outcome && o.run_id == e.run_id && o.seq == e.seq
            });
            if !has_outcome {
                unmatched.push(e.clone());
            }
        }
        unmatched
    }

    fn append(&mut self, entry: &JournalEntry) -> Result<(), FatalReason> {
        let mut line = serde_json::to_string(entry).map_err(|e| unwritable(&self.path, &e))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| unwritable(&self.path, &e))?;
        self.file
            .sync_all()
            .map_err(|e| unwritable(&self.path, &e))?;
        self.entries.push(entry.clone());
        Ok(())
    }
}

fn open_inner(path: &Path, create: bool) -> Result<Journal, FatalReason> {
    let entries = if path.exists() {
        load_entries(path)?
    } else if create {
        Vec::new()
    } else {
        return Err(unwritable(
            path,
            &io::Error::new(io::ErrorKind::NotFound, "journal not found"),
        ));
    };
    let next_seq = entries.iter().map(|e| e.seq).max().unwrap_or(0) + 1;

    let mut opts = OpenOptions::new();
    opts.append(true).read(true);
    if create {
        opts.create(true);
    }
    let file = opts.open(path).map_err(|e| unwritable(path, &e))?;
    file.sync_all().map_err(|e| unwritable(path, &e))?;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(Journal {
        path: path.to_path_buf(),
        dir,
        file,
        entries,
        next_seq,
        run_id: Uuid::nil(),
        root: PathBuf::new(),
        config_digest: String::new(),
    })
}

fn load_entries(path: &Path) -> Result<Vec<JournalEntry>, FatalReason> {
    let txt = std::fs::read_to_string(path).map_err(|e| unwritable(path, &e))?;
    let mut entries = Vec::new();
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(e) => entries.push(e),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping corrupt journal line");
            }
        }
    }
    Ok(entries)
}

fn unwritable(path: &Path, err: &impl ToString) -> FatalReason {
    FatalReason::JournalUnwritable {
        path: path.to_path_buf(),
        why: err.to_string(),
    }
}

/// RFC3339 UTC timestamp with millisecond precision. No extra crate.
pub fn rfc3339_now() -> String {
    rfc3339_from(SystemTime::now())
}

fn rfc3339_from(ts: SystemTime) -> String {
    let dur = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Unix timestamp → UTC civil date/time (Howard Hinnant's algorithm).
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d, hh, mm, ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::classify::MediaKind;
    use mm_core::volume::VolumeSemantics;
    use tempfile::TempDir;

    #[test]
    fn rfc3339_is_well_formed() {
        let s = rfc3339_from(UNIX_EPOCH);
        assert_eq!(s, "1970-01-01T00:00:00.000Z");
        let s = rfc3339_now();
        assert!(s.contains('T') && s.ends_with('Z') && s.len() >= 24);
    }

    #[test]
    fn intent_without_outcome_is_unmatched() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("journal.jsonl");
        let mut j = Journal::create(&path).unwrap();
        j.bind(Uuid::new_v4(), tmp.path().to_path_buf(), "deadbeef".into());
        let seq = j
            .write_intent(
                JournalOp::Move,
                Some(Path::new("a.mkv")),
                Some(Path::new("b.mkv")),
            )
            .unwrap();
        assert_eq!(j.unmatched_intents(None).len(), 1);
        j.write_outcome(seq, JournalOp::Move, None, None, "SUCCESS", Some(12), None)
            .unwrap();
        assert!(j.unmatched_intents(None).is_empty());
    }

    #[test]
    fn persist_plan_writes_json_next_to_journal() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("journal.jsonl");
        let j = Journal::create(&path).unwrap();
        let plan = Plan::new(
            Uuid::new_v4(),
            tmp.path().to_path_buf(),
            MediaKind::Movies,
            "abc".into(),
            VolumeSemantics::conservative(),
        );
        let dest = j.persist_plan(&plan).unwrap();
        assert!(dest.exists());
        let loaded: Plan = serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(loaded.run_id, plan.run_id);
    }

    #[test]
    fn unmatched_intents_scoped_by_root() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("journal.jsonl");
        let mut j = Journal::create(&path).unwrap();
        let root_a = tmp.path().join("a");
        let root_b = tmp.path().join("b");
        j.bind(Uuid::new_v4(), root_a.clone(), "x".into());
        j.write_intent(JournalOp::Move, Some(Path::new("a")), Some(Path::new("b")))
            .unwrap();
        j.bind(Uuid::new_v4(), root_b.clone(), "x".into());
        j.write_intent(JournalOp::Move, Some(Path::new("c")), Some(Path::new("d")))
            .unwrap();
        assert_eq!(j.unmatched_intents(Some(&root_a)).len(), 1);
        assert_eq!(j.unmatched_intents(None).len(), 2);
    }
}
