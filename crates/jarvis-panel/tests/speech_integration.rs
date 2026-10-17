#![cfg(unix)]
//! Real owned subprocess lifecycle through a silent fixture executable; never plays audio.
use jarvis_panel::speech::{SpeechError, SpeechWorker};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-speech-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        // The fixture records arguments/stdin and replaces itself with a silent
        // long-lived process, so cancellation must reap exactly this PID.
        let script = "#!/bin/sh\nfixture_dir=${0%/*}\nprintf '%s\\n' \"$@\" > \"$fixture_dir/args\"\ncat > \"$fixture_dir/input\"\nprintf '%s' \"$$\" > \"$fixture_dir/pid\"\nexec /bin/sleep 30\n";
        let executable = path.join("silent-speaker");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self { path }
    }
    fn executable(&self) -> PathBuf {
        self.path.join("silent-speaker")
    }
    fn pid(&self) -> Option<u32> {
        fs::read_to_string(self.path.join("pid")).ok()?.parse().ok()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < deadline, "speech lifecycle timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}
struct Unrelated(Child);
impl Drop for Unrelated {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn default_is_silent_and_invalid_or_oversized_text_never_starts_a_child() {
    let fixture = Fixture::new();
    let mut worker = SpeechWorker::with_executable(fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    thread::sleep(Duration::from_millis(100));
    assert!(!worker.control.busy());
    assert!(fixture.pid().is_none());
    for text in [
        String::new(),
        " \n\t".into(),
        "nul\0text".into(),
        "x".repeat(8193),
        "🦀".repeat(2049),
    ] {
        assert_eq!(worker.control.speak(text), Err(SpeechError::InvalidText));
    }
    assert!(fixture.pid().is_none());
    assert!(!fixture.path.join("input").exists());
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn speech_passes_literal_text_on_stdin_without_shell_argument_execution() {
    let fixture = Fixture::new();
    let mut worker = SpeechWorker::with_executable(fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    let marker = fixture.path.join("must-not-exist");
    let text = format!(
        "-v evil; $(touch {}) `touch {}` & <secret>\nsecond line",
        marker.display(),
        marker.display()
    );
    worker.control.speak(text.clone()).unwrap();
    wait_until(|| fixture.pid().is_some());
    assert_eq!(
        fs::read_to_string(fixture.path.join("input")).unwrap(),
        text
    );
    assert_eq!(
        fs::read_to_string(fixture.path.join("args")).unwrap(),
        "-f\n-\n"
    );
    assert!(!marker.exists());
    let pid = fixture.pid().unwrap();
    worker.control.stop_speaking();
    wait_until(|| !worker.control.busy());
    assert!(!alive(pid), "cancelled owned process was not reaped");
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn overlap_is_rejected_then_stop_allows_replacement_without_old_process_orphans() {
    let fixture = Fixture::new();
    let mut worker = SpeechWorker::with_executable(fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    worker.control.speak("first".into()).unwrap();
    wait_until(|| fixture.pid().is_some());
    let first = fixture.pid().unwrap();
    for _ in 0..100 {
        assert_eq!(
            worker.control.speak("must not queue".into()),
            Err(SpeechError::Busy)
        );
    }
    worker.control.stop_speaking();
    wait_until(|| !worker.control.busy());
    assert!(!alive(first));
    fs::remove_file(fixture.path.join("pid")).unwrap();
    worker.control.speak("second".into()).unwrap();
    wait_until(|| fixture.pid().is_some());
    let second = fixture.pid().unwrap();
    assert_ne!(first, second);
    assert_eq!(
        fs::read_to_string(fixture.path.join("input")).unwrap(),
        "second"
    );
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert!(!alive(second));
}

#[test]
fn shutdown_kills_only_owned_child_and_remains_terminal() {
    let fixture = Fixture::new();
    let mut unrelated = Unrelated(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let mut worker = SpeechWorker::with_executable(fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    worker.control.speak("owned child".into()).unwrap();
    wait_until(|| fixture.pid().is_some());
    let owned = fixture.pid().unwrap();
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
    assert!(!alive(owned));
    assert!(
        unrelated.0.try_wait().unwrap().is_none(),
        "shutdown killed an unrelated process"
    );
    assert_eq!(
        worker.control.speak("after shutdown".into()),
        Err(SpeechError::Unavailable)
    );
}
