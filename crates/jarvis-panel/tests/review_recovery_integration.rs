#![cfg(unix)]
//! Prepared edits composed through the actual conversational and background workers.
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::{
    context::{ContextStore, Limits, Source},
    file_work::FileEditTask,
    providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig},
};
use jarvis_panel::tasks::{State as TaskState, TaskWorker};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Peer {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
    hold: Arc<AtomicBool>,
}
impl Peer {
    async fn new(plans: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut plans = VecDeque::from(plans);
        let hold = Arc::new(AtomicBool::new(false));
        let held = hold.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    assert!(bytes.len() < 150000);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + size {
                            captured.lock().unwrap().push(if size == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap()
                            });
                            break;
                        }
                    }
                }
                while held.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                let body = plans
                    .pop_front()
                    .unwrap_or_else(|| answer("unexpected extra provider call"))
                    .to_string();
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            server,
            hold,
        }
    }
    fn packets(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn client(url: &str) -> CrusoeClient {
    CrusoeClient::new(
        "fixture-only-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(url, Duration::from_secs(3)).unwrap(),
    )
    .unwrap()
}
fn answer(text: &str) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":text}}]})
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn seed(dir: &TempDir) -> (String, PathBuf) {
    assert!(dir.path().is_dir());
    let source = dir.join("work.txt");
    fs::write(&source, "Original work note.\n").unwrap();
    let source = fs::canonicalize(source).unwrap();
    let id = format!("file:{}", source.display());
    let mut db = ContextStore::open(&dir.join("context.db"), Limits::default()).unwrap();
    db.observe(&id, "Original work note.\n", Source::File, 1)
        .unwrap();
    (id, source)
}
fn worker(dir: &TempDir, peer: &Peer) -> TaskWorker {
    let draft_endpoint = peer.url.clone();
    let edit_endpoint = peer.url.clone();
    TaskWorker::with_factories(
        dir.join("context.db"),
        dir.join("reviews"),
        move |db, job, request| {
            jarvis::draft::DraftTask::new(db, client(&draft_endpoint), job, request)
        },
        move |db, job, request| FileEditTask::new(db, client(&edit_endpoint), job, request),
        script(
            dir,
            "review-opener",
            "printf '%s\\n' \"$1\" > \"${0%/*}/opened\"",
        ),
    )
    .unwrap()
}
fn restored(dir: &TempDir) -> TaskWorker {
    TaskWorker::with_factories(
        dir.join("context.db"),
        dir.join("reviews"),
        |_, _, _| panic!("restart invoked draft inference"),
        |_, _, _| panic!("restart invoked file inference"),
        script(
            dir,
            "review-opener",
            "printf '%s\\n' \"$1\" > \"${0%/*}/opened\"",
        ),
    )
    .unwrap()
}
async fn stop(worker: &mut TaskWorker) {
    worker.control.shutdown();
    wait(|| worker.is_finished()).await;
    worker.join().unwrap();
}

#[tokio::test]
async fn ready_draft_and_file_edit_restore_without_provider_calls_or_automatic_opening() {
    for editing in [false, true] {
        let dir = TempDir::new();
        let (id, source) = seed(&dir);
        let peer = Peer::new(vec![answer("Prepared fixture response")]).await;
        let mut first = worker(&dir, &peer);
        if editing {
            first
                .control
                .submit_file_edit("Update work.txt".into(), id)
                .unwrap();
        } else {
            first.control.submit("Draft a work note".into()).unwrap();
        }
        wait(|| !first.control.busy()).await;
        let ready = first.control.snapshot();
        assert_eq!(ready.state, TaskState::ReadyForReview, "{}", ready.text);
        let artifact = ready.path.unwrap();
        let expected = fs::read(&artifact).unwrap();
        let journal = dir.join("reviews/task-review.json");
        assert!(journal.exists());
        assert_eq!(
            fs::metadata(&journal).unwrap().permissions().mode() & 0o077,
            0
        );
        stop(&mut first).await;
        let mut second = restored(&dir);
        wait(|| second.control.snapshot().state == TaskState::ReadyForReview).await;
        assert_eq!(second.control.snapshot().path, Some(artifact.clone()));
        assert_eq!(peer.packets().len(), 1);
        assert!(!dir.join("opened").exists());
        second.control.request_review().unwrap();
        wait(|| !second.control.busy()).await;
        assert_eq!(
            fs::read_to_string(dir.join("opened")).unwrap().trim(),
            artifact.to_str().unwrap()
        );
        assert_eq!(fs::read(artifact).unwrap(), expected);
        assert_eq!(fs::read_to_string(source).unwrap(), "Original work note.\n");
        stop(&mut second).await;
    }
}

#[tokio::test]
async fn restart_blocks_changed_source_tampered_artifact_or_same_bytes_replacement() {
    for change in ["source", "artifact", "same-bytes-inode"] {
        let dir = TempDir::new();
        let (id, source) = seed(&dir);
        let peer = Peer::new(vec![answer("Prepared fixture response")]).await;
        let mut first = worker(&dir, &peer);
        first
            .control
            .submit_file_edit("Update work.txt".into(), id)
            .unwrap();
        wait(|| !first.control.busy()).await;
        let artifact = first.control.snapshot().path.unwrap();
        stop(&mut first).await;
        match change {
            "source" => {
                fs::write(&source, "User edited the source while Jarvis was stopped").unwrap()
            }
            "artifact" => fs::write(&artifact, "Changed artifact").unwrap(),
            "same-bytes-inode" => {
                let replacement = dir.join("replacement");
                fs::write(&replacement, fs::read(&artifact).unwrap()).unwrap();
                fs::rename(replacement, &artifact).unwrap();
            }
            _ => unreachable!(),
        }
        let mut second = restored(&dir);
        wait(|| second.control.snapshot().state == TaskState::Blocked).await;
        assert!(second.control.snapshot().path.is_none());
        assert!(second.control.request_review().is_err());
        assert!(!dir.join("opened").exists());
        assert_eq!(peer.packets().len(), 1);
        stop(&mut second).await;
    }
}

#[tokio::test]
async fn interrupted_running_journal_is_a_blocker_not_an_instruction_to_repeat_work() {
    let dir = TempDir::new();
    let (id, source) = seed(&dir);
    let peer = Peer::new(vec![answer("Never publish late")]).await;
    peer.hold.store(true, Ordering::Release);
    let mut first = worker(&dir, &peer);
    first
        .control
        .submit_file_edit("Update work.txt".into(), id)
        .unwrap();
    wait(|| peer.packets().len() == 1).await;
    let journal = dir.join("reviews/task-review.json");
    let interrupted = fs::read(&journal).unwrap();
    stop(&mut first).await;
    peer.hold.store(false, Ordering::Release);
    // Preserve the exact in-flight record as a crash would, without a real process kill.
    fs::write(&journal, interrupted).unwrap();
    let mut second = restored(&dir);
    wait(|| second.control.snapshot().state == TaskState::Blocked).await;
    assert!(!dir.join("opened").exists());
    assert!(second.control.snapshot().path.is_none());
    assert_eq!(peer.packets().len(), 1);
    assert_eq!(fs::read_to_string(source).unwrap(), "Original work note.\n");
    stop(&mut second).await;
}

#[tokio::test]
async fn corrupt_or_symlinked_review_journal_never_recovers_a_ready_artifact() {
    use std::os::unix::fs::symlink;
    for symlinked in [false, true] {
        let dir = TempDir::new();
        seed(&dir);
        let reviews = dir.join("reviews");
        fs::create_dir(&reviews).unwrap();
        let journal = reviews.join("task-review.json");
        let bytes = "PRIVATE_SENTINEL not a valid journal";
        if symlinked {
            let target = dir.join("external-journal");
            fs::write(&target, bytes).unwrap();
            symlink(target, &journal).unwrap();
        } else {
            fs::write(&journal, bytes).unwrap();
        }
        let mut worker = restored(&dir);
        wait(|| worker.control.snapshot().state == TaskState::Blocked).await;
        assert!(!worker.control.snapshot().text.contains("PRIVATE_SENTINEL"));
        assert!(worker.control.snapshot().path.is_none());
        assert!(!dir.join("opened").exists());
        stop(&mut worker).await;
    }
}
