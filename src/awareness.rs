//! Explicit-root, bounded text-file awareness. No model calls or screenshots.
//!
//! Filesystem events are dirty hints. Scans use directory capabilities and
//! NOFOLLOW at every component, metadata fingerprints avoid rereading unchanged
//! files, and the context store hashes actual content before creating versions.
use crate::context::{ContextError, ContextStore, MAX_CONTENT_BYTES, ObservationResult, Source};
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt,
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, TrySendError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_DEPTH: usize = 8;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_FILES: usize = 1024;
pub const MAX_DIRTY_PATHS: usize = 128;
const DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub enum AwarenessError {
    InvalidRoot,
    InvalidDuration,
    RootChanged,
    WatchUnavailable,
    Context(ContextError),
}
impl fmt::Display for AwarenessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot => f.write_str("awareness root must be an accessible real directory"),
            Self::InvalidDuration => {
                f.write_str("watch duration must be greater than zero and at most one hour")
            }
            Self::RootChanged => f.write_str("awareness root identity changed; collection stopped"),
            Self::WatchUnavailable => f.write_str("filesystem observation unavailable"),
            Self::Context(_) => f.write_str("awareness context update failed"),
        }
    }
}
impl std::error::Error for AwarenessError {}
impl From<ContextError> for AwarenessError {
    fn from(value: ContextError) -> Self {
        Self::Context(value)
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ScanReport {
    pub resources_changed: usize,
    pub resources_unchanged: usize,
    pub resources_removed: usize,
    pub skipped: usize,
    pub reconciliations: usize,
    pub overflow_reconciliations: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}
#[cfg(unix)]
impl Fingerprint {
    fn from(meta: &Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            modified_seconds: meta.mtime(),
            modified_nanos: meta.mtime_nsec(),
            changed_seconds: meta.ctime(),
            changed_nanos: meta.ctime_nsec(),
        }
    }
    fn same_identity(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

struct Clock {
    last: i64,
}
impl Clock {
    fn next(&mut self) -> i64 {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let now = i64::try_from(epoch).unwrap_or(i64::MAX - 1);
        self.last = now.max(self.last.saturating_add(1));
        self.last
    }
}

#[cfg(unix)]
struct Collector {
    root: PathBuf,
    directory: File,
    root_identity: Fingerprint,
    known: HashMap<PathBuf, Fingerprint>,
    directory_paths: BTreeSet<PathBuf>,
    clock: Clock,
}

#[cfg(unix)]
impl Collector {
    fn new(root: &Path) -> Result<Self, AwarenessError> {
        use rustix::fs::{Mode, OFlags, open};
        let original = fs::symlink_metadata(root).map_err(|_| AwarenessError::InvalidRoot)?;
        if !original.is_dir() || original.file_type().is_symlink() {
            return Err(AwarenessError::InvalidRoot);
        }
        let root = fs::canonicalize(root).map_err(|_| AwarenessError::InvalidRoot)?;
        if root.to_str().is_none() {
            return Err(AwarenessError::InvalidRoot);
        }
        let directory = File::from(
            open(
                &root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| AwarenessError::InvalidRoot)?,
        );
        let root_identity = Fingerprint::from(
            &directory
                .metadata()
                .map_err(|_| AwarenessError::InvalidRoot)?,
        );
        if !root_identity.same_identity(&Fingerprint::from(&original)) {
            return Err(AwarenessError::RootChanged);
        }
        Ok(Self {
            root,
            directory,
            root_identity,
            known: HashMap::new(),
            directory_paths: BTreeSet::new(),
            clock: Clock { last: 0 },
        })
    }

    fn check_root(&self) -> Result<(), AwarenessError> {
        let metadata = fs::symlink_metadata(&self.root).map_err(|_| AwarenessError::RootChanged)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !self
                .root_identity
                .same_identity(&Fingerprint::from(&metadata))
        {
            return Err(AwarenessError::RootChanged);
        }
        Ok(())
    }

    fn resource_id(&self, relative: &Path) -> Option<String> {
        let path = self.root.join(relative);
        let text = path.to_str()?;
        let id = format!("file:{text}");
        (id.len() <= 2048).then_some(id)
    }

    fn scan(
        &mut self,
        store: &mut ContextStore,
        report: &mut ScanReport,
    ) -> Result<(), AwarenessError> {
        self.check_root()?;
        let existing = store.file_ids_under(&self.root)?;
        let directory = self
            .directory
            .try_clone()
            .map_err(|_| AwarenessError::InvalidRoot)?;
        let mut pass = ScanPass {
            entries: 0,
            files: 0,
            seen: HashSet::new(),
            directories: BTreeSet::new(),
            complete: true,
        };
        self.walk(store, &directory, Path::new(""), 0, &mut pass, report)?;
        self.check_root()?;
        if pass.complete {
            let seen_ids: HashSet<_> = pass
                .seen
                .iter()
                .filter_map(|path| self.resource_id(path))
                .collect();
            for id in existing {
                if !seen_ids.contains(&id)
                    && matches!(
                        store.forget(&id, self.clock.next())?,
                        Some(ObservationResult::Changed { .. })
                    )
                {
                    report.resources_removed += 1;
                }
            }
            self.known.retain(|path, _| pass.seen.contains(path));
        }
        self.directory_paths = pass.directories;
        report.reconciliations += 1;
        Ok(())
    }

    fn walk(
        &mut self,
        store: &mut ContextStore,
        directory: &File,
        relative: &Path,
        depth: usize,
        pass: &mut ScanPass,
        report: &mut ScanReport,
    ) -> Result<(), AwarenessError> {
        use rustix::fs::{Dir, FileType, Mode, OFlags, openat};
        use std::os::unix::ffi::OsStrExt;
        pass.directories.insert(self.root.join(relative));
        let Ok(entries) = Dir::read_from(directory) else {
            pass.complete = false;
            report.skipped += 1;
            return Ok(());
        };
        for entry in entries {
            if pass.entries >= MAX_ENTRIES || pass.files >= MAX_FILES {
                report.truncated = true;
                pass.complete = false;
                break;
            }
            let Ok(entry) = entry else {
                pass.complete = false;
                report.skipped += 1;
                continue;
            };
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            pass.entries += 1;
            // Hidden directories and files are excluded; do not crawl .git/.env.
            if name.starts_with(b".") {
                report.skipped += 1;
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(name);
            let child = relative.join(name);
            match entry.file_type() {
                FileType::Directory => {
                    if depth >= MAX_DEPTH {
                        report.skipped += 1;
                        report.truncated = true;
                        pass.complete = false;
                        continue;
                    }
                    match openat(
                        directory,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    ) {
                        Ok(fd) => {
                            self.walk(store, &File::from(fd), &child, depth + 1, pass, report)?
                        }
                        Err(_) => {
                            pass.complete = false;
                            report.skipped += 1;
                        }
                    }
                }
                FileType::RegularFile => {
                    let supported = child
                        .extension()
                        .and_then(|v| v.to_str())
                        .is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("md")
                        });
                    if !supported {
                        report.skipped += 1;
                        continue;
                    }
                    pass.files += 1;
                    self.read_file(store, directory, name, &child, pass, report)?;
                }
                // Symlinks and devices are never followed/opened.
                _ => {
                    report.skipped += 1;
                }
            }
        }
        Ok(())
    }

    fn read_file(
        &mut self,
        store: &mut ContextStore,
        directory: &File,
        name: &std::ffi::OsStr,
        relative: &Path,
        pass: &mut ScanPass,
        report: &mut ScanReport,
    ) -> Result<(), AwarenessError> {
        use rustix::fs::{Mode, OFlags, openat};
        let Some(id) = self.resource_id(relative) else {
            report.skipped += 1;
            return Ok(());
        };
        let Ok(fd) = openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            report.skipped += 1;
            return Ok(());
        };
        let mut file = File::from(fd);
        let Ok(before) = file.metadata() else {
            report.skipped += 1;
            return Ok(());
        };
        if !before.is_file() || before.len() > MAX_CONTENT_BYTES as u64 {
            report.skipped += 1;
            return Ok(());
        }
        let fingerprint = Fingerprint::from(&before);
        if self.known.get(relative) == Some(&fingerprint) {
            if !self
                .reopen(relative)
                .and_then(|f| f.metadata().ok())
                .is_some_and(|meta| Fingerprint::from(&meta) == fingerprint)
            {
                report.skipped += 1;
                return Ok(());
            }
            pass.seen.insert(relative.to_owned());
            report.resources_unchanged += 1;
            return Ok(());
        }
        let mut bytes = Vec::with_capacity((before.len() as usize).min(MAX_CONTENT_BYTES));
        if file
            .by_ref()
            .take((MAX_CONTENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > MAX_CONTENT_BYTES
        {
            report.skipped += 1;
            return Ok(());
        }
        let Ok(after) = file.metadata() else {
            report.skipped += 1;
            return Ok(());
        };
        if fingerprint != Fingerprint::from(&after) {
            report.skipped += 1;
            return Ok(());
        }
        // Reopen through the root capability to detect rename/replacement of any
        // intermediate component while reading. Every component remains NOFOLLOW.
        let Some(current) = self.reopen(relative) else {
            report.skipped += 1;
            return Ok(());
        };
        let Ok(current_metadata) = current.metadata() else {
            report.skipped += 1;
            return Ok(());
        };
        if fingerprint != Fingerprint::from(&current_metadata) {
            report.skipped += 1;
            return Ok(());
        }
        let Ok(content) = String::from_utf8(bytes) else {
            report.skipped += 1;
            return Ok(());
        };
        match store.observe(&id, &content, Source::File, self.clock.next())? {
            ObservationResult::Changed { .. } => report.resources_changed += 1,
            ObservationResult::Unchanged { .. } | ObservationResult::IgnoredOlder => {
                report.resources_unchanged += 1
            }
        }
        pass.seen.insert(relative.to_owned());
        if self.known.len() >= MAX_FILES
            && !self.known.contains_key(relative)
            && let Some(oldest) = self.known.keys().next().cloned()
        {
            self.known.remove(&oldest);
        }
        self.known.insert(relative.to_owned(), fingerprint);
        Ok(())
    }

    fn reopen(&self, relative: &Path) -> Option<File> {
        use rustix::fs::{Mode, OFlags, openat};
        let mut directory = self.directory.try_clone().ok()?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            if components.peek().is_some() {
                flags |= OFlags::DIRECTORY;
            }
            directory = File::from(openat(&directory, name, flags, Mode::empty()).ok()?);
        }
        Some(directory)
    }
}

struct ScanPass {
    entries: usize,
    files: usize,
    seen: HashSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
    complete: bool,
}

/// Index only an explicitly supplied root. No default home-directory discovery.
#[cfg(unix)]
pub fn index_root(store: &mut ContextStore, root: &Path) -> Result<ScanReport, AwarenessError> {
    let mut collector = Collector::new(root)?;
    let mut report = ScanReport::default();
    collector.scan(store, &mut report)?;
    Ok(report)
}

/// Observe a root for a bounded duration. Overflow forces bounded reconciliation.
/// Initial scan occurs after watcher registration so startup writes are queued.
#[cfg(unix)]
pub fn watch_root(
    store: &mut ContextStore,
    root: &Path,
    duration: Duration,
) -> Result<ScanReport, AwarenessError> {
    watch_root_with_ready(store, root, duration, || {})
}

/// The callback runs only after subscriptions and initial reconciliation succeed.
#[cfg(unix)]
pub fn watch_root_with_ready(
    store: &mut ContextStore,
    root: &Path,
    duration: Duration,
    on_ready: impl FnOnce(),
) -> Result<ScanReport, AwarenessError> {
    watch_root_controlled(store, root, duration, on_ready, || false)
}

/// Preserve the metadata cache while observing; cancellation is checked between
/// scans and at least every debounce interval while idle. A scan is bounded by
/// file/entry limits, but an in-progress filesystem call cannot be preempted.
#[cfg(unix)]
pub fn watch_root_controlled(
    store: &mut ContextStore,
    root: &Path,
    duration: Duration,
    on_ready: impl FnOnce(),
    should_stop: impl Fn() -> bool,
) -> Result<ScanReport, AwarenessError> {
    if duration.is_zero() || duration > Duration::from_secs(3600) {
        return Err(AwarenessError::InvalidDuration);
    }
    let mut collector = Collector::new(root)?;
    let (sender, receiver) = mpsc::sync_channel(MAX_DIRTY_PATHS);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = Arc::clone(&overflow);
    let mut watcher = notify::RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            let result = result.map(|mut event| {
                if event.paths.len() > MAX_DIRTY_PATHS {
                    event.paths.truncate(MAX_DIRTY_PATHS);
                    callback_overflow.store(true, Ordering::Release);
                }
                event
            });
            if let Err(TrySendError::Full(_)) = sender.try_send(result) {
                callback_overflow.store(true, Ordering::Release);
            }
        },
        notify::Config::default().with_follow_symlinks(false),
    )
    .map_err(|_| AwarenessError::WatchUnavailable)?;
    watcher
        .watch(&collector.root, RecursiveMode::NonRecursive)
        .map_err(|_| AwarenessError::WatchUnavailable)?;
    let deadline = Instant::now() + duration;
    let mut registered = BTreeSet::from([collector.root.clone()]);
    let mut report = ScanReport::default();
    collector.scan(store, &mut report)?;
    sync_watches(&mut watcher, &collector, &mut registered)?;
    // Reconcile writes between initial discovery and registering child watches.
    collector.scan(store, &mut report)?;
    sync_watches(&mut watcher, &collector, &mut registered)?;
    on_ready();
    let mut dirty = BTreeSet::new();
    let mut next_scan = None;
    loop {
        if should_stop() {
            return Ok(report);
        }
        let now = Instant::now();
        let lost = overflow.swap(false, Ordering::AcqRel);
        if lost {
            report.overflow_reconciliations += 1;
            next_scan = Some(now);
        }
        if next_scan.is_some_and(|when| now >= when) || (now >= deadline && !dirty.is_empty()) {
            collector.scan(store, &mut report)?;
            sync_watches(&mut watcher, &collector, &mut registered)?;
            dirty.clear();
            next_scan = None;
        }
        if now >= deadline {
            break;
        }
        let wait = next_scan
            .unwrap_or(deadline)
            .min(deadline)
            .saturating_duration_since(now)
            .min(DEBOUNCE);
        match receiver.recv_timeout(wait) {
            Ok(Ok(event)) => {
                // Reading a file may generate Access events on some backends.
                // Those must not produce a self-triggering scan loop.
                if matches!(event.kind, notify::EventKind::Access(_)) {
                    continue;
                }
                if event.need_rescan() {
                    overflow.store(true, Ordering::Release);
                }
                for path in event.paths {
                    if path.starts_with(&collector.root) {
                        if dirty.len() >= MAX_DIRTY_PATHS {
                            overflow.store(true, Ordering::Release);
                            break;
                        }
                        dirty.insert(path);
                    }
                }
                if !dirty.is_empty() && next_scan.is_none() {
                    next_scan = Some(Instant::now() + DEBOUNCE);
                }
            }
            Ok(Err(_)) => {
                overflow.store(true, Ordering::Release);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(AwarenessError::WatchUnavailable),
        }
    }
    // Flush queued platform events at the time boundary with one bounded scan.
    collector.scan(store, &mut report)?;
    Ok(report)
}

#[cfg(unix)]
fn sync_watches(
    watcher: &mut impl Watcher,
    collector: &Collector,
    registered: &mut BTreeSet<PathBuf>,
) -> Result<(), AwarenessError> {
    // Never ask notify to recursively crawl an unbounded tree. Only directories
    // already discovered by the bounded, nofollow scan receive subscriptions.
    for path in registered.difference(&collector.directory_paths) {
        let _ = watcher.unwatch(path); // Removed directories may already be unwatched.
    }
    for path in collector.directory_paths.difference(registered) {
        watcher
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|_| AwarenessError::WatchUnavailable)?;
    }
    registered.clone_from(&collector.directory_paths);
    Ok(())
}
