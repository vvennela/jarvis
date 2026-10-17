//! One bounded recovery record. Restoring a review never reruns a model or action.
use crate::{
    MAX_REVIEW_BYTES,
    context::{ContextStore, MAX_CONTENT_BYTES, Source},
    draft::DraftOutcome,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub const MAX_JOURNAL_BYTES: usize = 32 * 1024;
const NAME: &str = "task-review.json";
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskKind {
    Draft,
    FileEdit,
}
#[derive(Clone, Copy, Debug)]
pub struct JournalError;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    hash: String,
    device: u64,
    inode: u64,
    bytes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    id: String,
    version: i64,
    source: Source,
    hash: String,
    disk: Option<Fingerprint>,
}
#[derive(Clone)]
pub struct EvidenceSnapshot {
    kind: TaskKind,
    dependencies: Vec<Dependency>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: u8,
    database: PathBuf,
    kind: TaskKind,
    ready: Option<ReadyRecord>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyRecord {
    job_id: String,
    revision: u64,
    artifact: String,
    fingerprint: Fingerprint,
    dependencies: Vec<Dependency>,
}
pub enum Restore {
    Empty,
    Blocked,
    Ready(RecoveredReview),
}
pub struct RecoveredReview {
    database: PathBuf,
    directory: PathBuf,
    kind: TaskKind,
    record: ReadyRecord,
}
pub struct TaskJournal {
    database: PathBuf,
    directory: PathBuf,
}
impl TaskJournal {
    pub fn new(database: &Path, directory: &Path) -> Result<Self, JournalError> {
        let directory = fs::canonicalize(directory).map_err(|_| JournalError)?;
        let database = if database.is_absolute() {
            database.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|_| JournalError)?
                .join(database)
        };
        Ok(Self {
            database,
            directory,
        })
    }
    pub fn mark_running(&self, kind: TaskKind) -> Result<(), JournalError> {
        self.write(&Record {
            format: 1,
            database: self.database.clone(),
            kind,
            ready: None,
        })
    }
    pub fn capture(
        &self,
        kind: TaskKind,
        ids: &[String],
    ) -> Result<EvidenceSnapshot, JournalError> {
        if ids.is_empty() || ids.len() > 4 {
            return Err(JournalError);
        }
        let store = ContextStore::open_readonly(&self.database).map_err(|_| JournalError)?;
        let mut dependencies = Vec::new();
        for id in ids {
            if id.len() > 2048 {
                return Err(JournalError);
            }
            let item = store
                .get(id)
                .map_err(|_| JournalError)?
                .ok_or(JournalError)?;
            let disk = if item.source == Source::File {
                match id
                    .strip_prefix("file:")
                    .filter(|p| Path::new(p).is_absolute())
                {
                    Some(path) => {
                        let (bytes, fingerprint) =
                            read_fingerprint(Path::new(path), MAX_CONTENT_BYTES)?;
                        if bytes != item.content.as_bytes() {
                            return Err(JournalError);
                        }
                        Some(fingerprint)
                    }
                    None if kind == TaskKind::FileEdit => return Err(JournalError),
                    None => None,
                }
            } else {
                None
            };
            dependencies.push(Dependency {
                id: id.clone(),
                version: item.version,
                source: item.source,
                hash: hash(item.content.as_bytes()),
                disk,
            });
        }
        Ok(EvidenceSnapshot { kind, dependencies })
    }
    pub fn save_ready(
        &self,
        evidence: EvidenceSnapshot,
        outcome: &DraftOutcome,
    ) -> Result<RecoveredReview, JournalError> {
        let DraftOutcome::ReadyForReview {
            job_id,
            revision,
            path,
        } = outcome
        else {
            return Err(JournalError);
        };
        if path.parent() != Some(self.directory.as_path()) || job_id.len() > 256 {
            return Err(JournalError);
        }
        let artifact = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(JournalError)?
            .to_owned();
        if artifact == NAME {
            return Err(JournalError);
        }
        let (_, fingerprint) = read_fingerprint(path, MAX_REVIEW_BYTES)?;
        let review = RecoveredReview {
            database: self.database.clone(),
            directory: self.directory.clone(),
            kind: evidence.kind,
            record: ReadyRecord {
                job_id: job_id.clone(),
                revision: *revision,
                artifact,
                fingerprint,
                dependencies: evidence.dependencies,
            },
        };
        review.review()?;
        self.write(&Record {
            format: 1,
            database: self.database.clone(),
            kind: review.kind,
            ready: Some(review.record.clone()),
        })?;
        Ok(review)
    }
    pub fn restore(&self) -> Restore {
        if let Err(error) = fs::symlink_metadata(self.directory.join(NAME)) {
            return if error.kind() == std::io::ErrorKind::NotFound {
                Restore::Empty
            } else {
                Restore::Blocked
            };
        }
        let load = || -> Result<RecoveredReview, JournalError> {
            let (bytes, _) = read_fingerprint(&self.directory.join(NAME), MAX_JOURNAL_BYTES)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(self.directory.join(NAME))
                    .map_err(|_| JournalError)?
                    .permissions()
                    .mode()
                    & 0o077
                    != 0
                {
                    return Err(JournalError);
                }
            }
            let record: Record = serde_json::from_slice(&bytes).map_err(|_| JournalError)?;
            if record.format != 1 || record.database != self.database {
                return Err(JournalError);
            }
            let ready = record.ready.ok_or(JournalError)?;
            if ready.job_id.len() > 256
                || ready.dependencies.is_empty()
                || ready.dependencies.len() > 4
                || ready.artifact.len() > 255
                || Path::new(&ready.artifact).components().count() != 1
                || !matches!(
                    Path::new(&ready.artifact).components().next(),
                    Some(Component::Normal(_))
                )
                || ready.artifact == NAME
            {
                return Err(JournalError);
            }
            let review = RecoveredReview {
                database: self.database.clone(),
                directory: self.directory.clone(),
                kind: record.kind,
                record: ready,
            };
            review.review()?;
            Ok(review)
        };
        match load() {
            Ok(review) => Restore::Ready(review),
            Err(_) => Restore::Blocked,
        }
    }
    fn write(&self, record: &Record) -> Result<(), JournalError> {
        let bytes = serde_json::to_vec(record).map_err(|_| JournalError)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError);
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| JournalError)?;
        let name: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let temporary = self.directory.join(format!(".review-{name}.tmp"));
        let operation = || -> Result<(), JournalError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|_| JournalError)?;
            file.write_all(&bytes).map_err(|_| JournalError)?;
            file.sync_all().map_err(|_| JournalError)?;
            fs::rename(&temporary, self.directory.join(NAME)).map_err(|_| JournalError)?;
            File::open(&self.directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| JournalError)?;
            Ok(())
        };
        let result = operation();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
impl RecoveredReview {
    pub fn kind(&self) -> TaskKind {
        self.kind
    }
    pub fn review(&self) -> Result<DraftOutcome, JournalError> {
        let store = ContextStore::open_readonly(&self.database).map_err(|_| JournalError)?;
        for dependency in &self.record.dependencies {
            if dependency.id.len() > 2048 {
                return Err(JournalError);
            }
            let current = store
                .get(&dependency.id)
                .map_err(|_| JournalError)?
                .ok_or(JournalError)?;
            if current.version != dependency.version
                || current.source != dependency.source
                || hash(current.content.as_bytes()) != dependency.hash
            {
                return Err(JournalError);
            }
            if let Some(expected) = &dependency.disk {
                let path = dependency.id.strip_prefix("file:").ok_or(JournalError)?;
                let (_, fingerprint) = read_fingerprint(Path::new(path), MAX_CONTENT_BYTES)?;
                if &fingerprint != expected {
                    return Err(JournalError);
                }
            } else if self.kind == TaskKind::FileEdit {
                return Err(JournalError);
            }
        }
        let path = self.directory.join(&self.record.artifact);
        let (_, fingerprint) = read_fingerprint(&path, MAX_REVIEW_BYTES)?;
        if fingerprint != self.record.fingerprint {
            return Err(JournalError);
        }
        Ok(DraftOutcome::ReadyForReview {
            job_id: self.record.job_id.clone(),
            revision: self.record.revision,
            path,
        })
    }
}
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
#[cfg(unix)]
fn read_fingerprint(path: &Path, cap: usize) -> Result<(Vec<u8>, Fingerprint), JournalError> {
    use rustix::fs::{Mode, OFlags, openat};
    if !path.is_absolute() {
        return Err(JournalError);
    }
    let mut file = File::open("/").map_err(|_| JournalError)?;
    let mut components = path
        .components()
        .filter(|c| !matches!(c, Component::RootDir))
        .peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(JournalError);
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        file = File::from(openat(&file, name, flags, Mode::empty()).map_err(|_| JournalError)?);
    }
    read_open_file(file, cap)
}
#[cfg(not(unix))]
fn read_fingerprint(_path: &Path, _cap: usize) -> Result<(Vec<u8>, Fingerprint), JournalError> {
    Err(JournalError)
}
fn read_open_file(mut file: File, cap: usize) -> Result<(Vec<u8>, Fingerprint), JournalError> {
    let before = file.metadata().map_err(|_| JournalError)?;
    if !before.is_file() || before.len() > cap as u64 {
        return Err(JournalError);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| JournalError)?;
    let after = file.metadata().map_err(|_| JournalError)?;
    if bytes.len() > cap
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || identity(&before) != identity(&after)
    {
        return Err(JournalError);
    }
    let (device, inode) = identity(&before);
    let fingerprint = Fingerprint {
        hash: hash(&bytes),
        device,
        inode,
        bytes: bytes.len() as u64,
    };
    Ok((bytes, fingerprint))
}
#[cfg(unix)]
fn identity(metadata: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}
#[cfg(not(unix))]
fn identity(_metadata: &Metadata) -> (u64, u64) {
    (0, 0)
}
