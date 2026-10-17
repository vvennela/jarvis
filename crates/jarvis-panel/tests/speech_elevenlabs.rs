#![cfg(unix)]
//! Fixture synthesis and a silent fixture player; no ElevenLabs calls or audio playback.
use jarvis::elevenlabs::ElevenLabsClient;
use jarvis_panel::speech::{SpeechWorker, State};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
const AUDIO: &[u8] = b"ID3\x04\0\0\0\0\0\0synthetic-fixture";
struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "jarvis-eleven-player-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let script = "#!/bin/sh\nfixture_dir=${0%/*}\nprintf '%s\\n' \"$@\" > \"$fixture_dir/args\"\ncp \"$1\" \"$fixture_dir/audio\"\nprintf '%s' \"$$\" > \"$fixture_dir/pid\"\nexec /bin/sleep 30\n";
        let executable = path.join("player");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self { path }
    }
    fn executable(&self) -> PathBuf {
        self.path.join("player")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
struct Peer {
    url: String,
    received: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Peer {
    fn new(status: &str, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(AtomicBool::new(false));
        let seen = received.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let status = status.to_owned();
        let handle = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = match socket.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                seen.store(true, Ordering::Release);
                thread::sleep(delay);
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    AUDIO.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(AUDIO);
            }
        });
        Self {
            url,
            received,
            stop,
            handle: Some(handle),
        }
    }
    fn client(&self) -> ElevenLabsClient {
        ElevenLabsClient::loopback_for_tests(
            "fixture-key".into(),
            &self.url,
            Duration::from_secs(2),
        )
        .unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "Eleven speech lifecycle timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn stop(worker: &mut SpeechWorker) {
    worker.control.shutdown();
    wait_until(|| worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn synthesized_audio_uses_private_owned_tempfile_and_is_removed_after_cancel() {
    let fixture = Fixture::new();
    let peer = Peer::new("200 OK", Duration::ZERO);
    let mut worker = SpeechWorker::with_elevenlabs(peer.client(), fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    worker
        .control
        .speak("never pass this text as executable arguments".into())
        .unwrap();
    wait_until(|| fixture.path.join("pid").exists());
    let args = fs::read_to_string(fixture.path.join("args")).unwrap();
    assert_eq!(args.lines().count(), 1);
    let audio = PathBuf::from(args.trim());
    assert!(audio.is_absolute());
    assert_eq!(fs::read(&audio).unwrap(), AUDIO);
    assert_eq!(fs::read(fixture.path.join("audio")).unwrap(), AUDIO);
    assert_eq!(
        fs::metadata(&audio).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let parent = audio.parent().unwrap().to_path_buf();
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    worker.control.stop_speaking();
    wait_until(|| !worker.control.busy());
    wait_until(|| !audio.exists() && !parent.exists());
    stop(&mut worker);
}

#[test]
fn cancellation_during_synthesis_never_starts_playback() {
    let fixture = Fixture::new();
    let peer = Peer::new("200 OK", Duration::from_millis(350));
    let mut worker = SpeechWorker::with_elevenlabs(peer.client(), fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    worker.control.speak("cancel before audio".into()).unwrap();
    wait_until(|| peer.received.load(Ordering::Acquire));
    let started = Instant::now();
    worker.control.stop_speaking();
    wait_until(|| !worker.control.busy());
    assert!(started.elapsed() < Duration::from_millis(250));
    drop(peer);
    assert!(!fixture.path.join("args").exists());
    stop(&mut worker);
}

#[test]
fn synthesis_authentication_failure_is_reported_without_playback() {
    let fixture = Fixture::new();
    let peer = Peer::new("401 Unauthorized", Duration::ZERO);
    let mut worker = SpeechWorker::with_elevenlabs(peer.client(), fixture.executable()).unwrap();
    wait_until(|| worker.control.available());
    worker.control.speak("private request".into()).unwrap();
    wait_until(|| !worker.control.busy());
    assert_eq!(worker.control.snapshot().state, State::Failed);
    assert!(!fixture.path.join("args").exists());
    stop(&mut worker);
}
