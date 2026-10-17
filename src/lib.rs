//! Deterministic Jarvis policy and job lifecycle foundation.
//! Adapters must derive evidence and operation classifications from trusted code,
//! never from model-supplied safety labels. Desktop adapters are not built yet.
use std::{
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    CloseTab,
    DraftMessage,
    PreparePurchase,
    PrepareDeletion,
    SendMessage,
    Pay,
    Delete,
}
impl Operation {
    pub fn is_commit(self) -> bool {
        matches!(self, Self::SendMessage | Self::Pay | Self::Delete)
    }
    pub fn needs_review(self) -> bool {
        matches!(
            self,
            Self::DraftMessage | Self::PreparePurchase | Self::PrepareDeletion
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStatus {
    Ready,
    Missing,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    AwaitEvidence,
    TimedOut,
    DenyFinalCommit,
}
/// Time arguments use the same monotonic clock. A deadline is exclusive.
pub fn gate(
    operation: Operation,
    evidence: EvidenceStatus,
    now_ms: u64,
    deadline_ms: u64,
) -> GateDecision {
    if operation.is_commit() {
        GateDecision::DenyFinalCommit
    } else if now_ms >= deadline_ms {
        GateDecision::TimedOut
    } else if evidence != EvidenceStatus::Ready {
        GateDecision::AwaitEvidence
    } else {
        GateDecision::Allow
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    AwaitingClarification,
    Blocked,
    ReadyForReview,
    Completed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub job_id: String,
    pub revision: u64,
    pub text: String,
}
#[derive(Debug)]
pub enum JobError {
    FinalCommitForbidden,
    InvalidState,
    WrongQuestion,
    StaleRevision,
    EmptyAnswer,
    NotReviewable,
    ArtifactChanged,
    Io(io::Error),
}
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "artifact I/O failed: {err}"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for JobError {}
impl From<io::Error> for JobError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
#[derive(Debug)]
struct ReviewArtifact {
    path: PathBuf,
    revision: u64,
    expected: Vec<u8>,
    identity: fs::Metadata,
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    // Fail closed until this platform has a stable file-identity adapter.
    false
}

fn verify_artifact(path: &Path, identity: &fs::Metadata, expected: &[u8]) -> Result<(), JobError> {
    let entry = fs::symlink_metadata(path)?;
    if !entry.is_file() || !same_file(&entry, identity) {
        return Err(JobError::ArtifactChanged);
    }
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || !same_file(&opened, identity) || opened.len() != expected.len() as u64 {
        return Err(JobError::ArtifactChanged);
    }
    let mut actual = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut actual)?;
    if actual != expected {
        return Err(JobError::ArtifactChanged);
    }
    Ok(())
}
#[derive(Debug)]
pub struct Job {
    id: String,
    request_id: String,
    operation: Operation,
    revision: u64,
    state: JobState,
    question: Option<Question>,
    artifact: Option<ReviewArtifact>,
    answers: Vec<String>,
}
impl Job {
    pub fn new(
        id: impl Into<String>,
        request_id: impl Into<String>,
        operation: Operation,
    ) -> Result<Self, JobError> {
        if operation.is_commit() {
            return Err(JobError::FinalCommitForbidden);
        }
        Ok(Self {
            id: id.into(),
            request_id: request_id.into(),
            operation,
            revision: 1,
            state: JobState::Running,
            question: None,
            artifact: None,
            answers: Vec::new(),
        })
    }
    pub fn state(&self) -> JobState {
        self.state
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn answers(&self) -> &[String] {
        &self.answers
    }
    pub fn ask(
        &mut self,
        id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Question, JobError> {
        if self.state != JobState::Running {
            return Err(JobError::InvalidState);
        }
        let question = Question {
            id: id.into(),
            job_id: self.id.clone(),
            revision: self.revision,
            text: text.into(),
        };
        self.question = Some(question.clone());
        self.state = JobState::AwaitingClarification;
        Ok(question)
    }
    pub fn answer(&mut self, question: &Question, text: &str) -> Result<(), JobError> {
        if self.state != JobState::AwaitingClarification {
            return Err(JobError::InvalidState);
        }
        if question.revision != self.revision {
            return Err(JobError::StaleRevision);
        }
        if self.question.as_ref() != Some(question) {
            return Err(JobError::WrongQuestion);
        }
        if text.trim().is_empty() {
            return Err(JobError::EmptyAnswer);
        }
        self.answers.push(text.to_owned());
        self.revision += 1;
        self.question = None;
        self.state = JobState::Running;
        Ok(())
    }
    /// External relevant changes invalidate results and outstanding questions.
    /// The collector/ledger must resolve expected self-writes first.
    pub fn dependency_changed(&mut self) -> Result<(), JobError> {
        if matches!(self.state, JobState::Completed | JobState::Cancelled) {
            return Err(JobError::InvalidState);
        }
        self.revision += 1;
        self.artifact = None;
        self.question = None;
        self.state = JobState::Blocked;
        Ok(())
    }
    pub fn resume_after_reconciliation(&mut self, revision: u64) -> Result<(), JobError> {
        if revision != self.revision {
            return Err(JobError::StaleRevision);
        }
        if self.state != JobState::Blocked {
            return Err(JobError::InvalidState);
        }
        self.state = JobState::Running;
        Ok(())
    }
    pub fn cancel(&mut self) -> Result<(), JobError> {
        if self.state == JobState::Completed {
            return Err(JobError::InvalidState);
        }
        self.state = JobState::Cancelled;
        self.question = None;
        self.artifact = None;
        Ok(())
    }
    /// Local review artifact only. create_new prevents overwriting existing files
    /// or following a file symlink. Production adapters must also enforce roots.
    pub fn prepare_review(
        &mut self,
        revision: u64,
        path: &Path,
        content: &[u8],
    ) -> Result<(), JobError> {
        if revision != self.revision {
            return Err(JobError::StaleRevision);
        }
        if self.state != JobState::Running {
            return Err(JobError::InvalidState);
        }
        if !self.operation.needs_review() {
            return Err(JobError::NotReviewable);
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(content)?;
        file.sync_all()?;
        let identity = file.metadata()?;
        verify_artifact(path, &identity, content)?;
        let canonical = fs::canonicalize(path)?;
        verify_artifact(&canonical, &identity, content)?;
        self.artifact = Some(ReviewArtifact {
            path: canonical,
            revision,
            expected: content.to_vec(),
            identity,
        });
        self.state = JobState::ReadyForReview;
        Ok(())
    }
    /// Recheck immediately before exposing the artifact. A failed verification
    /// removes the READY_FOR_REVIEW claim until reconciliation.
    pub fn review_path(&mut self) -> Result<PathBuf, JobError> {
        if self.state != JobState::ReadyForReview {
            return Err(JobError::InvalidState);
        }
        let artifact = self.artifact.as_ref().ok_or(JobError::NotReviewable)?;
        let result = if artifact.revision != self.revision {
            Err(JobError::StaleRevision)
        } else {
            verify_artifact(&artifact.path, &artifact.identity, &artifact.expected)
                .map(|()| artifact.path.clone())
        };
        if result.is_err() {
            self.state = JobState::Blocked;
            self.artifact = None;
        }
        result
    }
    /// Call only after a real tool-specific postcondition, not model assertion.
    pub fn complete_verified_action(&mut self) -> Result<(), JobError> {
        if self.state != JobState::Running {
            return Err(JobError::InvalidState);
        }
        if self.operation.needs_review() {
            return Err(JobError::NotReviewable);
        }
        self.state = JobState::Completed;
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    TransientRead,
    StaleTarget,
    ConflictingEdit,
    UncertainExternalOutcome,
    Permanent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Retry,
    RefreshAndRevalidate,
    Pause,
    ReconcileOutcome,
    Stop,
}
/// Retry counters are runner-owned. Uncertain external writes are never retried.
pub fn recovery(failure: Failure, retries_used: u32, retry_limit: u32) -> Recovery {
    match failure {
        Failure::UncertainExternalOutcome => Recovery::ReconcileOutcome,
        Failure::ConflictingEdit => Recovery::Pause,
        Failure::Permanent => Recovery::Stop,
        _ if retries_used >= retry_limit => Recovery::Stop,
        Failure::TransientRead => Recovery::Retry,
        Failure::StaleTarget => Recovery::RefreshAndRevalidate,
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeculationKey {
    pub request_id: String,
    pub transcript_revision: u64,
}
/// Conservatively discard speculative results on any transcript revision change.
pub fn speculation_is_current(prepared: &SpeculationKey, current: &SpeculationKey) -> bool {
    prepared == current
}
