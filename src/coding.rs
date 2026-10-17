//! A bounded coding job in an owned Git worktree. Never applies, commits or pushes.
use serde_json::Value;
use std::{
    fmt,
    fs::{self, Metadata, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::Instant,
};

pub const MAX_INSTRUCTION_BYTES: usize = 8192;
pub const MAX_PROCESS_BYTES: usize = 1024 * 1024;
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
#[derive(Clone)]
pub struct CodingConfig {
    project: PathBuf,
    output_root: PathBuf,
    codex: PathBuf,
    git: PathBuf,
    timeout: Duration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodingError {
    InvalidConfiguration,
    InvalidRequest,
    InvalidState,
    Process,
    OutputLimit,
    Protocol,
    Timeout,
    Cancelled,
    Artifact,
}
impl fmt::Display for CodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => {
                "Coding needs an approved project and installed local Codex executable."
            }
            Self::InvalidRequest => {
                "Tell me what to build in the approved project, using a shorter request."
            }
            Self::InvalidState => "Start a new coding task for that request.",
            Self::Process => {
                "The coding process couldn't complete. Its isolated worktree was preserved."
            }
            Self::OutputLimit => {
                "The coding output exceeded its limit. Its isolated worktree was preserved."
            }
            Self::Protocol => {
                "The coding process didn't report a completed turn. Its worktree was preserved."
            }
            Self::Timeout => {
                "The coding task reached its time limit. Its isolated worktree was preserved."
            }
            Self::Cancelled => "Coding stopped. Its isolated worktree was preserved.",
            Self::Artifact => "The coding result couldn't be verified for review.",
        })
    }
}
impl std::error::Error for CodingError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingOutcome {
    ReadyForReview {
        summary: PathBuf,
        worktree: PathBuf,
    },
    Blocked {
        reason: &'static str,
        worktree: Option<PathBuf>,
    },
    Cancelled {
        worktree: Option<PathBuf>,
    },
}
impl CodingConfig {
    pub fn new(
        project: PathBuf,
        output_root: PathBuf,
        codex_executable: PathBuf,
    ) -> Result<Self, CodingError> {
        for path in [&project, &output_root, &codex_executable] {
            validate_path(path)?;
        }
        let project = fs::canonicalize(project).map_err(|_| CodingError::InvalidConfiguration)?;
        let output_root = resolve_future_directory(&output_root)?;
        if !project.is_dir() || !codex_executable.is_file() || output_root.starts_with(&project) {
            return Err(CodingError::InvalidConfiguration);
        }
        Ok(Self {
            project,
            output_root,
            codex: codex_executable,
            git: PathBuf::from("/usr/bin/git"),
            timeout: DEFAULT_TIMEOUT,
        })
    }
    pub fn with_git_executable(mut self, path: PathBuf) -> Result<Self, CodingError> {
        validate_path(&path)?;
        if !path.is_file() {
            return Err(CodingError::InvalidConfiguration);
        }
        self.git = path;
        Ok(self)
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, CodingError> {
        if timeout.is_zero() || timeout > DEFAULT_TIMEOUT {
            return Err(CodingError::InvalidConfiguration);
        }
        self.timeout = timeout;
        Ok(self)
    }
}
#[derive(Clone, PartialEq, Eq)]
struct Fingerprint {
    hash: blake3::Hash,
    device: u64,
    inode: u64,
}
struct Ready {
    source_head: String,
    source_status: Vec<u8>,
    work_status: Vec<u8>,
    diff: Fingerprint,
    report: Fingerprint,
    summary: Fingerprint,
    diff_path: PathBuf,
    report_path: PathBuf,
    summary_path: PathBuf,
}
pub struct CodingTask {
    config: CodingConfig,
    started: bool,
    worktree: Option<PathBuf>,
    ready: Option<Ready>,
    blocked: bool,
}
impl CodingTask {
    pub fn new(config: CodingConfig) -> Result<Self, CodingError> {
        Ok(Self {
            config,
            started: false,
            worktree: None,
            ready: None,
            blocked: false,
        })
    }
    pub async fn run(
        &mut self,
        instruction: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<CodingOutcome, CodingError> {
        if self.started {
            return Err(CodingError::InvalidState);
        }
        if instruction.trim().is_empty()
            || instruction.len() > MAX_INSTRUCTION_BYTES
            || instruction.contains('\0')
        {
            return Err(CodingError::InvalidRequest);
        }
        self.started = true;
        let result = self
            .run_inner(instruction, &cancel, Instant::now() + self.config.timeout)
            .await;
        match result {
            Err(CodingError::Cancelled) => Ok(CodingOutcome::Cancelled {
                worktree: self.worktree.clone(),
            }),
            other => other,
        }
    }
    async fn run_inner(
        &mut self,
        instruction: &str,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<CodingOutcome, CodingError> {
        check_cancel(cancel)?;
        let top = self
            .git(
                &self.config.project,
                &["rev-parse", "--show-toplevel"],
                cancel,
                deadline,
            )
            .await?;
        let actual =
            fs::canonicalize(text(&top)?).map_err(|_| CodingError::InvalidConfiguration)?;
        if actual != self.config.project {
            return Err(CodingError::InvalidConfiguration);
        }
        let head = text(
            &self
                .git(
                    &self.config.project,
                    &["rev-parse", "HEAD"],
                    cancel,
                    deadline,
                )
                .await?,
        )?
        .to_owned();
        if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CodingError::Protocol);
        }
        let source_status = self.status(&self.config.project, cancel, deadline).await?;
        if !source_status.is_empty() {
            return Ok(self.block("Save or commit your current project changes first, then ask me to code in a clean checkout."));
        }
        let output_root = private_directory(&self.config.output_root)?;
        if output_root.starts_with(&self.config.project) {
            return Err(CodingError::InvalidConfiguration);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| CodingError::Artifact)?;
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let job = output_root.join(format!("coding-{name}"));
        private_directory(&job)?;
        let worktree = job.join("worktree");
        self.worktree = Some(worktree.clone());
        self.git(
            &self.config.project,
            &["worktree", "add", "--detach", path_text(&worktree)?, &head],
            cancel,
            deadline,
        )
        .await?;
        // Project-defined servers, plugins or hooks are not part of this
        // approved coding capability. Do not load any project agent config.
        if [".codex", ".mcp.json", ".codex.toml"]
            .iter()
            .any(|name| fs::symlink_metadata(worktree.join(name)).is_ok())
        {
            return Ok(self.block("This project has custom agent configuration. Coding is paused until that configuration can be reviewed."));
        }
        let report_path = job.join("agent-report.txt");
        let prompt = format!(
            "Work only inside this isolated Git worktree. Fulfill the user's coding request, run relevant tests, and explain changed files and actual test results. Use git only for repository operations; never use gh or GitHub API tools. Do not commit, push, send messages, install global tools, access credentials, modify another checkout, or apply changes outside this worktree. Network access for shell commands is disabled. If blocked, describe the blocker accurately. The result is a local proposal for human review; never claim it was applied to the original project.\n\nOriginal user request:\n{instruction}\n"
        );
        let args = vec![
            "exec".into(),
            "--ignore-user-config".into(),
            "--ephemeral".into(),
            "--disable".into(),
            "apps".into(),
            "--disable".into(),
            "plugins".into(),
            "--disable".into(),
            "hooks".into(),
            "--disable".into(),
            "remote_plugin".into(),
            "-c".into(),
            "web_search=\"disabled\"".into(),
            "--sandbox".into(),
            "workspace-write".into(),
            "-c".into(),
            "approval_policy=\"never\"".into(),
            "-c".into(),
            "sandbox_workspace_write.network_access=false".into(),
            "--json".into(),
            "--color".into(),
            "never".into(),
            "--output-last-message".into(),
            path_text(&report_path)?.into(),
            "-C".into(),
            path_text(&worktree)?.into(),
            "-".into(),
        ];
        let output = run_process(
            &self.config.codex,
            &args,
            &worktree,
            Some(prompt.as_bytes()),
            cancel,
            deadline,
        )
        .await?;
        if !output.status.success() {
            return Err(CodingError::Process);
        }
        let command_events = completed_protocol(&output.stdout)?;
        let (report_bytes, report) = fingerprint(&report_path, MAX_REPORT_BYTES)?;
        let report_text = String::from_utf8(report_bytes).map_err(|_| CodingError::Artifact)?;
        if report_text.trim().is_empty() {
            return Err(CodingError::Artifact);
        }
        if text(
            &self
                .git(&worktree, &["rev-parse", "HEAD"], cancel, deadline)
                .await?,
        )? != head
        {
            return Ok(self.block("The coding worker changed its commit history. Review the isolated worktree manually."));
        }
        // Intent-to-add is confined to the owned worktree. It lets the actual
        // diff include newly created files without committing any content.
        self.git(
            &worktree,
            &["add", "--intent-to-add", "--all"],
            cancel,
            deadline,
        )
        .await?;
        let diff_bytes = self
            .git(
                &worktree,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--binary",
                    "HEAD",
                    "--",
                ],
                cancel,
                deadline,
            )
            .await?;
        let work_status = self.status(&worktree, cancel, deadline).await?;
        if text(
            &self
                .git(
                    &self.config.project,
                    &["rev-parse", "HEAD"],
                    cancel,
                    deadline,
                )
                .await?,
        )? != head
            || self.status(&self.config.project, cancel, deadline).await? != source_status
        {
            return Ok(self.block("Your original project changed while coding. The isolated result is preserved; start a fresh task before review."));
        }
        check_cancel(cancel)?;
        let diff_path = job.join("changes.diff");
        write_private(&diff_path, &diff_bytes)?;
        let summary_path = job.join("review.md");
        let summary_text = format!(
            "# Coding result — ready for review\n\nOriginal project: {}\nBase commit: {head}\nIsolated worktree: {}\nActual Git diff: {}\n\nThe original checkout was not changed. Nothing was committed, pushed or applied.\nRunner verified successful process exit and a completed Codex turn. Observed command-completion events: {command_events}. Test claims below are the agent's report, not an independent test certification.\n\n## Agent report (untrusted text)\n\n{report_text}\n",
            self.config.project.display(),
            worktree.display(),
            diff_path.display()
        );
        write_private(&summary_path, summary_text.as_bytes())?;
        let (_, diff) = fingerprint(&diff_path, MAX_PROCESS_BYTES)?;
        let (_, summary) = fingerprint(&summary_path, MAX_REPORT_BYTES + 8192)?;
        self.ready = Some(Ready {
            source_head: head,
            source_status,
            work_status,
            diff,
            report,
            summary,
            diff_path,
            report_path,
            summary_path,
        });
        self.review_inner(cancel, deadline).await
    }
    pub async fn review(&mut self) -> Result<CodingOutcome, CodingError> {
        self.review_inner(
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(15),
        )
        .await
    }
    async fn review_inner(
        &mut self,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<CodingOutcome, CodingError> {
        if self.blocked {
            return Ok(self.block(
                "The coding result is no longer verified. Its isolated worktree is preserved.",
            ));
        }
        let Some(ready) = self.ready.as_ref() else {
            return Err(CodingError::InvalidState);
        };
        let worktree = self
            .worktree
            .as_ref()
            .ok_or(CodingError::InvalidState)?
            .clone();
        let valid = async {
            if text(
                &self
                    .git(
                        &self.config.project,
                        &["rev-parse", "HEAD"],
                        cancel,
                        deadline,
                    )
                    .await?,
            )? != ready.source_head
                || self.status(&self.config.project, cancel, deadline).await? != ready.source_status
                || text(
                    &self
                        .git(&worktree, &["rev-parse", "HEAD"], cancel, deadline)
                        .await?,
                )? != ready.source_head
                || self.status(&worktree, cancel, deadline).await? != ready.work_status
            {
                return Ok(false);
            }
            let diff = self
                .git(
                    &worktree,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--binary",
                        "HEAD",
                        "--",
                    ],
                    cancel,
                    deadline,
                )
                .await?;
            if blake3::hash(&diff) != ready.diff.hash {
                return Ok(false);
            }
            for (path, expected, cap) in [
                (&ready.diff_path, &ready.diff, MAX_PROCESS_BYTES),
                (&ready.report_path, &ready.report, MAX_REPORT_BYTES),
                (&ready.summary_path, &ready.summary, MAX_REPORT_BYTES + 8192),
            ] {
                if fingerprint(path, cap)?.1 != *expected {
                    return Ok(false);
                }
            }
            Ok::<bool, CodingError>(true)
        }
        .await;
        match valid {Ok(true)=>Ok(CodingOutcome::ReadyForReview{summary:ready.summary_path.clone(),worktree}),Err(CodingError::Cancelled)=>Err(CodingError::Cancelled),_=>Ok(self.block("The project, isolated changes or review files changed. Review access was revoked; the worktree is preserved."))}
    }
    fn block(&mut self, reason: &'static str) -> CodingOutcome {
        self.blocked = true;
        CodingOutcome::Blocked {
            reason,
            worktree: self.worktree.clone(),
        }
    }
    async fn status(
        &self,
        path: &Path,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<Vec<u8>, CodingError> {
        self.git(
            path,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            cancel,
            deadline,
        )
        .await
    }
    async fn git(
        &self,
        path: &Path,
        args: &[&str],
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<Vec<u8>, CodingError> {
        let mut arguments = vec![
            "--no-pager".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-C".into(),
            path_text(path)?.into(),
        ];
        arguments.extend(args.iter().map(|s| (*s).to_owned()));
        let output =
            run_process(&self.config.git, &arguments, path, None, cancel, deadline).await?;
        if !output.status.success() {
            return Err(CodingError::Process);
        }
        Ok(output.stdout)
    }
}
fn completed_protocol(stdout: &[u8]) -> Result<usize, CodingError> {
    let mut completed = false;
    let mut commands = 0;
    for line in stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        if line.len() > MAX_LINE_BYTES {
            return Err(CodingError::OutputLimit);
        }
        let event: Value = serde_json::from_slice(line).map_err(|_| CodingError::Protocol)?;
        match event.get("type").and_then(Value::as_str) {
            Some("turn.completed") => completed = true,
            Some("turn.failed" | "error") => return Err(CodingError::Protocol),
            Some("item.completed")
                if event.pointer("/item/type").and_then(Value::as_str)
                    == Some("command_execution") =>
            {
                commands += 1
            }
            Some(_) => {}
            None => return Err(CodingError::Protocol),
        }
    }
    if !completed {
        return Err(CodingError::Protocol);
    }
    Ok(commands)
}
struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
}
#[cfg(unix)]
struct Group(i32);
#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
async fn run_process(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    input: Option<&[u8]>,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Output, CodingError> {
    check_cancel(cancel)?;
    if Instant::now() >= deadline {
        return Err(CodingError::Timeout);
    }
    let mut command = Command::new(executable);
    // Preserve only local login/runtime essentials. The configured Codex home
    // retains existing OAuth auth, but no ambient API keys or config overrides
    // reach either Codex or its shell commands.
    command.env_clear();
    for name in [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "CODEX_HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|_| CodingError::Process)?;
    #[cfg(unix)]
    let group = Group(
        i32::try_from(child.id().ok_or(CodingError::Process)?).map_err(|_| CodingError::Process)?,
    );
    let mut stdin = child.stdin.take().ok_or(CodingError::Process)?;
    let stdout = child.stdout.take().ok_or(CodingError::Process)?;
    let stderr = child.stderr.take().ok_or(CodingError::Process)?;
    let operation = async {
        let (stdout, _, (), status) = tokio::try_join!(
            read_bounded(stdout),
            read_bounded(stderr),
            async {
                if let Some(input) = input {
                    stdin
                        .write_all(input)
                        .await
                        .map_err(|_| CodingError::Process)?;
                }
                drop(stdin);
                Ok::<(), CodingError>(())
            },
            async { child.wait().await.map_err(|_| CodingError::Process) }
        )?;
        Ok(Output { status, stdout })
    };
    let result = tokio::select! {biased;_ = cancelled(cancel)=>Err(CodingError::Cancelled), result=tokio::time::timeout_at(deadline,operation)=>result.unwrap_or(Err(CodingError::Timeout))};
    #[cfg(unix)]
    drop(group);
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}
async fn read_bounded(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, CodingError> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|_| CodingError::Process)?;
        if n == 0 {
            return Ok(output);
        }
        if output.len() + n > MAX_PROCESS_BYTES {
            return Err(CodingError::OutputLimit);
        }
        output.extend_from_slice(&buffer[..n]);
    }
}
async fn cancelled(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), CodingError> {
    if cancel.load(Ordering::Acquire) {
        Err(CodingError::Cancelled)
    } else {
        Ok(())
    }
}
fn text(bytes: &[u8]) -> Result<&str, CodingError> {
    std::str::from_utf8(bytes)
        .map(str::trim)
        .map_err(|_| CodingError::Protocol)
}
fn path_text(path: &Path) -> Result<&str, CodingError> {
    path.to_str().ok_or(CodingError::InvalidConfiguration)
}
fn resolve_future_directory(path: &Path) -> Result<PathBuf, CodingError> {
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        missing.push(
            existing
                .file_name()
                .ok_or(CodingError::InvalidConfiguration)?
                .to_owned(),
        );
        existing = existing.parent().ok_or(CodingError::InvalidConfiguration)?;
    }
    let mut resolved = fs::canonicalize(existing).map_err(|_| CodingError::InvalidConfiguration)?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}
fn validate_path(path: &Path) -> Result<(), CodingError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        || path
            .to_str()
            .is_none_or(|s| s.len() > 4096 || s.contains(['\0', '\n', '\r']))
    {
        Err(CodingError::InvalidConfiguration)
    } else {
        Ok(())
    }
}
fn private_directory(path: &Path) -> Result<PathBuf, CodingError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|_| CodingError::Artifact)?;
    fs::canonicalize(path).map_err(|_| CodingError::Artifact)
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CodingError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| CodingError::Artifact)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| CodingError::Artifact)
}
fn fingerprint(path: &Path, cap: usize) -> Result<(Vec<u8>, Fingerprint), CodingError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|_| CodingError::Artifact)?;
    let before = file.metadata().map_err(|_| CodingError::Artifact)?;
    if !before.is_file() || before.len() > cap as u64 {
        return Err(CodingError::Artifact);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CodingError::Artifact)?;
    let after = file.metadata().map_err(|_| CodingError::Artifact)?;
    if bytes.len() > cap
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || identity(&before) != identity(&after)
    {
        return Err(CodingError::Artifact);
    }
    let (device, inode) = identity(&before);
    let hash = blake3::hash(&bytes);
    Ok((
        bytes,
        Fingerprint {
            hash,
            device,
            inode,
        },
    ))
}
#[cfg(unix)]
fn identity(m: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}
#[cfg(not(unix))]
fn identity(_m: &Metadata) -> (u64, u64) {
    (0, 0)
}
