#![cfg(unix)]
//! Actual native-host executable and Unix socket, with synthetic extension frames.
use jarvis::{
    browser_tools::BrowserClient,
    context::{ContextStore, Source},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
async fn send<W: AsyncWrite + Unpin>(output: &mut W, value: &Value) {
    let body = value.to_string().into_bytes();
    output.write_u32_le(body.len() as u32).await.unwrap();
    output.write_all(&body).await.unwrap();
    output.flush().await.unwrap();
}
async fn receive<R: AsyncRead + Unpin>(input: &mut R) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        let len = input.read_u32_le().await.unwrap();
        assert!(len > 0 && len <= 512 * 1024);
        let mut bytes = vec![0; len as usize];
        input.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    })
    .await
    .unwrap()
}
struct FixtureHome(PathBuf);
impl Drop for FixtureHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn actual_native_host_process_serves_fresh_browser_reads_without_private_environment() {
    let dir = PathBuf::from(format!(
        "/tmp/jp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let home = FixtureHome(dir);
    let database = home.0.join("context.db");
    let extension_id = "abcdefghijklmnopabcdefghijklmnop";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_jarvis"))
        .env_clear()
        .env("HOME", &home.0)
        .args(["native-host", "--db"])
        .arg(&database)
        .args([
            "--extension-id",
            extension_id,
            &format!("chrome-extension://{extension_id}/"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    send(&mut stdin,&json!({"type":"hello","protocol_version":1,"client_nonce":"process-fixture-nonce-1234","capabilities":["fresh_reads"]})).await;
    let ack = receive(&mut stdout).await;
    assert_eq!(ack["type"], "hello_ack");
    let session = ack["session_id"].as_str().unwrap();
    let socket = home
        .0
        .join("Library/Application Support/Jarvis/BrowserBridge/bridge.sock");
    let client = BrowserClient::new(socket.clone()).unwrap();
    let tab = json!({"resource_id":format!("browser-live:{session}:11"),"tab_id":11,"window_id":4,"document_id":"process-document","url":"https://example.com/fixture","title":"Process fixture"});
    let extension = async {
        let request = receive(&mut stdout).await;
        assert_eq!(request["operation"], "search_tabs");
        assert_eq!(request["session_id"], session);
        send(&mut stdin,&json!({"type":"browser.result","protocol_version":1,"session_id":session,"request_id":request["request_id"],"status":"ok","tabs":[tab],"page":null})).await;
    };
    let (found, ()) = tokio::join!(client.search_tabs("fixture"), extension);
    let found = found.unwrap();
    assert_eq!(found.len(), 1);
    let extension = async {
        let request = receive(&mut stdout).await;
        assert_eq!(request["operation"], "read_tab");
        assert_eq!(
            request["arguments"]["expected_document_id"],
            "process-document"
        );
        send(&mut stdin,&json!({"type":"browser.result","protocol_version":1,"session_id":session,"request_id":request["request_id"],"status":"ok","tabs":[],"page":{"tab":tab,"content":"SYNTHETIC_PRIVATE_SENTINEL browser process fixture","truncated":false,"trust":"untrusted_page"}})).await;
    };
    let (read, ()) = tokio::join!(
        client.read_tab(&found[0].resource_id, &found[0].document_id),
        extension
    );
    assert!(read.unwrap().content.contains("browser process fixture"));
    drop(stdin);
    drop(stdout);
    let result = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "native host failed: {:?}",
        result.status
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains("SYNTHETIC_PRIVATE_SENTINEL"));
    assert!(
        !socket.exists(),
        "normal host exit left a discoverable stale socket"
    );
    let store = ContextStore::open_readonly(&database).unwrap();
    let resources = store.search("browser process fixture", 4).unwrap();
    assert!(
        resources.iter().any(
            |r| r.source == Source::Browser && r.content.contains("SYNTHETIC_PRIVATE_SENTINEL")
        )
    );
}

#[tokio::test]
async fn browser_status_distinguishes_missing_valid_and_spoofed_registration_without_tab_reads() {
    use std::os::unix::fs::symlink;
    let directory = PathBuf::from(format!(
        "/tmp/js-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    let home = FixtureHome(directory);
    let manifest = home.0.join(
        "Library/Application Support/Google/Chrome/NativeMessagingHosts/com.jarvis.bridge.json",
    );
    let launcher = home.0.join("launcher");
    fs::write(&launcher, "fixture launcher, never executed").unwrap();
    let valid = json!({"name":"com.jarvis.bridge","type":"stdio","path":launcher,"allowed_origins":["chrome-extension://abcdefghijklmnopabcdefghijklmnop/"]});
    for case in [
        "missing",
        "valid",
        "wrong-origin",
        "manifest-symlink",
        "launcher-symlink",
    ] {
        if let Ok(metadata) = fs::symlink_metadata(&manifest) {
            assert!(metadata.is_file() || metadata.file_type().is_symlink());
            fs::remove_file(&manifest).unwrap();
        }
        if case != "missing" {
            fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            let mut value = valid.clone();
            if case == "wrong-origin" {
                value["allowed_origins"] = json!(["https://evil.invalid/"]);
            }
            if case == "launcher-symlink" {
                let link = home.0.join("launcher-link");
                symlink(&launcher, &link).unwrap();
                value["path"] = json!(link);
            }
            if case == "manifest-symlink" {
                let target = home.0.join("manifest-target");
                fs::write(&target, value.to_string()).unwrap();
                symlink(&target, &manifest).unwrap();
            } else {
                fs::write(&manifest, value.to_string()).unwrap();
            }
        }
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_jarvis"))
            .env_clear()
            .env("HOME", &home.0)
            .arg("browser-status")
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            status["native_host_registered"],
            case == "valid",
            "case {case}"
        );
        assert_eq!(status["bridge_connected"], false);
        assert!(
            !home
                .0
                .join("Library/Application Support/Jarvis/BrowserBridge")
                .exists(),
            "status wrote runtime state"
        );
    }
}
