//! Framed Native Messaging integration with real SQLite persistence.
mod support;
use jarvis::{
    bridge::{
        BridgeError, MAX_FRAME_BYTES, SessionReport, run_session_with_clock,
        validate_extension_origin,
    },
    context::{Actor, ContextStore, Limits, Source},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{self, Cursor, Read, Write},
    rc::Rc,
};
use support::TempDir;

fn frame(value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap();
    let mut result = (bytes.len() as u32).to_le_bytes().to_vec();
    result.extend(bytes);
    result
}
fn hello() -> Value {
    json!({"type":"hello","protocol_version":1,"client_nonce":"test-client-nonce-123456"})
}
fn event() -> Value {
    json!({"kind":"document.changed","tab_id":42,"window_id":7,"url":"https://example.com/spec","title":"Specification","observed_at_ms":i64::MAX,"actor":"unknown","owner":"unknown","source":"browser_content_script","trust":"untrusted_page","document_id":"doc-current","content":"public mango specification"})
}
fn observation(sequence: u64, event: Value) -> Value {
    json!({"type":"observation","protocol_version":1,"session_id":"$SESSION","sequence":sequence,"event":event})
}
fn page_id() -> String {
    format!(
        "browser-page:42:{}",
        blake3::hash(b"https://example.com/spec").to_hex()
    )
}

#[derive(Clone)]
struct Output(Rc<RefCell<Vec<u8>>>);
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct InteractiveInput {
    current: Cursor<Vec<u8>>,
    pending: Option<Vec<Value>>,
    output: Rc<RefCell<Vec<u8>>>,
}
impl Read for InteractiveInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.current.position() == self.current.get_ref().len() as u64
            && let Some(mut pending) = self.pending.take()
        {
            let output = self.output.borrow();
            let ack: Value = serde_json::from_slice(&output[4..]).unwrap();
            let mut wire = Vec::new();
            for message in &mut pending {
                if message["session_id"] == "$SESSION" {
                    message["session_id"] = ack["session_id"].clone();
                }
                wire.extend(frame(message));
            }
            self.current = Cursor::new(wire);
        }
        // Fragment every header/body across reads, like a pipe may do.
        let limit = buffer.len().min(3);
        self.current.read(&mut buffer[..limit])
    }
}
fn run(
    store: &mut ContextStore,
    messages: Vec<Value>,
    times: Vec<i64>,
) -> (Result<SessionReport, BridgeError>, Vec<u8>) {
    let output = Rc::new(RefCell::new(Vec::new()));
    let input = InteractiveInput {
        current: Cursor::new(frame(&hello())),
        pending: Some(messages),
        output: output.clone(),
    };
    let mut times = times.into_iter();
    let result = run_session_with_clock(input, Output(output.clone()), store, || {
        times.next().unwrap_or(100)
    });
    let bytes = output.borrow().clone();
    (result, bytes)
}
fn store(dir: &TempDir, limits: Limits) -> ContextStore {
    assert!(dir.path().is_dir());
    ContextStore::open(&dir.join("context.db"), limits).unwrap()
}

#[test]
fn valid_framed_session_indexes_page_with_host_time_and_unknown_actor() {
    let dir = TempDir::new();
    let mut context = store(&dir, Limits::default());
    let (result, output) = run(&mut context, vec![observation(1, event())], vec![55]);
    assert_eq!(
        result.unwrap(),
        SessionReport {
            observations_accepted: 1,
            resources_changed: 2
        }
    );
    let ack: Value = serde_json::from_slice(&output[4..]).unwrap();
    assert_eq!(ack["type"], "hello_ack");
    assert_eq!(ack["client_nonce"], hello()["client_nonce"]);
    assert_eq!(ack["session_id"].as_str().unwrap().len(), 64);
    assert_eq!(
        u32::from_le_bytes(output[..4].try_into().unwrap()) as usize,
        output.len() - 4
    );
    let resource = context.get(&page_id()).unwrap().unwrap();
    assert_eq!(resource.observed_at_ms, 55);
    assert_eq!(resource.source, Source::Browser);
    assert_eq!(context.search("mango", 10).unwrap().len(), 1);
    assert!(
        context
            .recent(10)
            .unwrap()
            .iter()
            .all(|event| event.actor == Actor::Unknown)
    );
}

#[test]
fn native_origin_requires_exact_chrome_extension_identity() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    assert!(validate_extension_origin(id, &format!("chrome-extension://{id}/")).is_ok());
    for origin in [
        format!("chrome-extension://{id}"),
        format!("chrome-extension://{id}/extra"),
        format!("chrome-extension://{id}.evil/"),
        "https://example.com/".into(),
    ] {
        assert_eq!(
            validate_extension_origin(id, &origin),
            Err(BridgeError::InvalidOrigin)
        );
    }
    assert!(
        validate_extension_origin(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "chrome-extension://zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz/"
        )
        .is_err()
    );
}

#[test]
fn malformed_truncated_and_oversized_frames_cannot_create_context() {
    let dir = TempDir::new();
    let mut context = store(&dir, Limits::default());
    for (wire, expected) in [
        (vec![], BridgeError::InvalidHandshake),
        (vec![1, 2], BridgeError::InvalidFrame),
        (0u32.to_le_bytes().to_vec(), BridgeError::InvalidFrame),
        (
            ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec(),
            BridgeError::FrameTooLarge,
        ),
        (
            {
                let mut bytes = 10u32.to_le_bytes().to_vec();
                bytes.extend(b"{}");
                bytes
            },
            BridgeError::InvalidFrame,
        ),
        (
            frame(&json!({"type":"send_message","body":"SECRET"})),
            BridgeError::InvalidFrame,
        ),
        (
            frame(
                &json!({"type":"hello","protocol_version":999,"client_nonce":"test-client-nonce-123456"}),
            ),
            BridgeError::InvalidHandshake,
        ),
        (
            frame(&json!({"type":"hello","protocol_version":1,"client_nonce":"short"})),
            BridgeError::InvalidHandshake,
        ),
    ] {
        let mut output = Vec::new();
        let error =
            run_session_with_clock(Cursor::new(wire), &mut output, &mut context, || 1).unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("SECRET"));
        assert!(output.is_empty());
        assert!(context.recent(10).unwrap().is_empty());
    }
}

#[test]
fn wrong_session_duplicate_and_unsafe_sequences_cannot_mutate_prior_evidence() {
    for mut invalid in [
        observation(1, event()),
        observation(0, event()),
        observation(9_007_199_254_740_992, event()),
        {
            let mut wrong = observation(2, event());
            wrong["session_id"] = "attacker-session".into();
            wrong
        },
    ] {
        let dir = TempDir::new();
        let mut context = store(&dir, Limits::default());
        invalid["event"]["content"] = "forged replacement".into();
        let (result, _) = run(
            &mut context,
            vec![observation(1, event()), invalid],
            vec![10, 11],
        );
        assert!(matches!(
            result,
            Err(BridgeError::InvalidSequence | BridgeError::InvalidSession)
        ));
        assert_eq!(
            context.get(&page_id()).unwrap().unwrap().content,
            "public mango specification"
        );
        assert!(context.search("forged", 10).unwrap().is_empty());
    }
}

#[test]
fn invalid_provenance_ids_urls_and_content_fail_closed() {
    for (key, value) in [
        ("actor", json!("user")),
        ("owner", json!("agent")),
        ("trust", json!("trusted")),
        ("source", json!("user_instruction")),
        ("tab_id", json!(-1)),
        ("window_id", json!(i64::MAX)),
        ("url", json!("file:///etc/passwd")),
        ("url", json!("https://user:secret@example.com/")),
        ("title", json!("x".repeat(2049))),
        ("document_id", json!("x".repeat(129))),
        ("content", json!("🦀".repeat(16385))),
        ("content", json!("secret\0content")),
        ("navigation_id", json!("99:123")),
        ("observed_at_ms", json!(-1)),
    ] {
        let dir = TempDir::new();
        let mut context = store(&dir, Limits::default());
        let mut bad = event();
        bad[key] = value;
        let (result, _) = run(&mut context, vec![observation(1, bad)], vec![10]);
        assert_eq!(
            result.unwrap_err(),
            BridgeError::InvalidObservation,
            "accepted invalid {key}"
        );
        assert!(context.recent(10).unwrap().is_empty());
    }
}

#[test]
fn page_and_tab_metadata_roll_back_together_when_capacity_is_insufficient() {
    let dir = TempDir::new();
    let mut context = store(
        &dir,
        Limits {
            max_resources: 1,
            ..Limits::default()
        },
    );
    let (result, _) = run(&mut context, vec![observation(1, event())], vec![10]);
    assert_eq!(result.unwrap_err(), BridgeError::Store);
    assert!(context.get(&page_id()).unwrap().is_none());
    assert!(context.get("browser-tab:42").unwrap().is_none());
    assert!(context.search("mango", 10).unwrap().is_empty());
    assert!(context.recent(10).unwrap().is_empty());
}

#[test]
fn clock_regression_does_not_lose_newer_content_but_invalid_clock_is_rejected() {
    let dir = TempDir::new();
    let mut context = store(&dir, Limits::default());
    let mut newer = event();
    newer["content"] = "new banana".into();
    assert!(
        run(
            &mut context,
            vec![observation(1, event()), observation(2, newer)],
            vec![100, 90]
        )
        .0
        .is_ok()
    );
    assert_eq!(
        context.get(&page_id()).unwrap().unwrap().content,
        "new banana"
    );
    assert_eq!(
        context.get(&page_id()).unwrap().unwrap().observed_at_ms,
        100
    );
    assert_eq!(
        run(&mut context, vec![observation(1, event())], vec![-1])
            .0
            .unwrap_err(),
        BridgeError::Clock
    );
}

#[test]
fn tab_closure_removes_current_tab_without_claiming_historical_page_deletion() {
    let dir = TempDir::new();
    let mut context = store(&dir, Limits::default());
    let mut closed = event();
    closed["kind"] = "tab.closed".into();
    closed.as_object_mut().unwrap().remove("content");
    assert!(
        run(
            &mut context,
            vec![observation(1, event()), observation(2, closed)],
            vec![1, 2]
        )
        .0
        .is_ok()
    );
    assert!(context.get("browser-tab:42").unwrap().is_none());
    assert!(context.get(&page_id()).unwrap().is_some());
}

#[test]
fn actual_native_host_binary_handshakes_and_persists_observations_without_stdout_noise() {
    use std::process::{Command, Stdio};
    let dir = TempDir::new();
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let mut child = Command::new(env!("CARGO_BIN_EXE_jarvis"))
        .args(["native-host", "--db"])
        .arg(dir.join("context.db"))
        .args(["--extension-id", id, &format!("chrome-extension://{id}/")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin.write_all(&frame(&hello())).unwrap();
    stdin.flush().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut prefix = [0; 4];
        stdout.read_exact(&mut prefix).unwrap();
        let length = u32::from_le_bytes(prefix) as usize;
        assert!(length <= MAX_FRAME_BYTES);
        let mut bytes = vec![0; length];
        stdout.read_exact(&mut bytes).unwrap();
        sender
            .send(serde_json::from_slice::<Value>(&bytes).unwrap())
            .unwrap();
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        rest
    });
    let ack = match receiver.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(value) => value,
        Err(error) => {
            child.kill().unwrap();
            panic!("native host handshake stalled: {error}");
        }
    };
    let mut message = observation(1, event());
    message["session_id"] = ack["session_id"].clone();
    stdin.write_all(&frame(&message)).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        reader.join().unwrap().is_empty(),
        "stdout must contain only framed protocol messages"
    );
    assert!(output.stderr.is_empty());
    let reopened = store(&dir, Limits::default());
    assert_eq!(reopened.search("mango", 10).unwrap().len(), 1);
}

#[test]
fn native_host_rejects_wrong_origin_before_creating_database() {
    let dir = TempDir::new();
    let path = dir.join("must-not-exist.db");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jarvis"))
        .args(["native-host", "--db"])
        .arg(&path)
        .args([
            "--extension-id",
            "abcdefghijklmnopabcdefghijklmnop",
            "https://attacker.example/",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!path.exists());
}
