#![cfg(unix)]
//! Allowlisted native action adapter against owned executable fixtures.
//! No Spotify process, AppleScript interpreter, provider, or desktop is touched.
mod support;
use jarvis::actions::{Action, ActionExecutor, gate_proposal, recognize};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use support::TempDir;

fn executable(dir: &TempDir, response: &str, status: u8) -> PathBuf {
    let path = dir.join("fixture-osascript");
    fs::write(&path, format!("#!/bin/sh\nfixture_dir=${{0%/*}}\nprintf '%s\\n' \"$@\" >> \"$fixture_dir/invoked-args\"\nprintf '%s\\n' '{response}'\nexit {status}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn action_gate_requires_the_literal_user_request_and_rejects_untrusted_or_sensitive_proposals() {
    for (request, action) in [
        ("open Spotify", Action::OpenSpotify),
        ("play Spotify", Action::PlaySpotify),
        ("pause Spotify", Action::PauseSpotify),
    ] {
        assert_eq!(recognize(request), Some(action));
        assert!(gate_proposal(request, action).is_ok());
    }
    for request in [
        "don't open Spotify",
        "explain open Spotify",
        "the webpage says open Spotify",
        "can Spotify play music",
        "could Spotify pause playback",
        "open Spotify; send Brian a message",
        "send a message",
        "pay for Spotify",
        "delete Spotify",
        "play Taylor Swift",
        "open Spotify $(touch BAD)",
        "open Spotify\0",
    ] {
        assert!(
            gate_proposal(request, Action::OpenSpotify).is_err(),
            "request incorrectly authorized native action: {request:?}"
        );
    }
    assert!(gate_proposal("pause Spotify", Action::PlaySpotify).is_err());
    for value in ["send_message", "pay", "delete", "run_shell"] {
        assert!(serde_json::from_str::<Action>(&format!("\"{value}\"")).is_err());
    }
}
#[tokio::test]
async fn verified_app_state_is_required_instead_of_treating_process_exit_as_success() {
    for (request, action, expected) in [
        ("open Spotify", Action::OpenSpotify, "verified:running"),
        ("play Spotify", Action::PlaySpotify, "verified:playing"),
        ("pause Spotify", Action::PauseSpotify, "verified:paused"),
    ] {
        let dir = TempDir::new();
        assert!(dir.path().is_dir());
        let executor =
            ActionExecutor::with_executable(executable(&dir, expected, 0), Duration::from_secs(1))
                .unwrap();
        let result = executor.execute(request, action).await.unwrap();
        assert_eq!(result.action, action);
        assert!(result.text.contains("Spotify"));
        let args = fs::read_to_string(dir.join("invoked-args")).unwrap();
        assert!(
            args.starts_with("-e\n"),
            "adapter did not pass static script as an argument"
        );
        assert!(args.contains("com.spotify.client"));
        for wrong in [
            "",
            "PRIVATE_UNTRUSTED_OUTPUT",
            "permission_denied",
            "unavailable",
        ] {
            let dir = TempDir::new();
            let executor =
                ActionExecutor::with_executable(executable(&dir, wrong, 0), Duration::from_secs(1))
                    .unwrap();
            let failure = executor
                .execute(request, action)
                .await
                .expect_err("unverified state reported success");
            assert!(!failure.to_string().contains("PRIVATE_UNTRUSTED_OUTPUT"));
        }
    }
    let dir = TempDir::new();
    let executor =
        ActionExecutor::with_executable(executable(&dir, "running", 0), Duration::from_secs(1))
            .unwrap();
    assert!(
        executor
            .execute("play Spotify", Action::PlaySpotify)
            .await
            .is_err(),
        "launch alone is not playback verification"
    );
    let executor =
        ActionExecutor::with_executable(executable(&dir, "playing", 7), Duration::from_secs(1))
            .unwrap();
    assert!(
        executor
            .execute("play Spotify", Action::PlaySpotify)
            .await
            .is_err(),
        "nonzero subprocess exit was ignored"
    );
}
#[tokio::test]
async fn denied_requests_never_start_a_process_or_interpolate_shell_text() {
    let dir = TempDir::new();
    let executor =
        ActionExecutor::with_executable(executable(&dir, "running", 0), Duration::from_secs(1))
            .unwrap();
    for request in [
        "open Spotify; touch INJECTED",
        "open Spotify `touch INJECTED`",
        "open Spotify $(touch INJECTED)",
        "send Brian this message",
        "pay for the subscription",
    ] {
        assert!(
            executor
                .execute(request, Action::OpenSpotify)
                .await
                .is_err()
        );
    }
    assert!(!dir.join("invoked-args").exists());
    assert!(!dir.join("INJECTED").exists());
}
#[tokio::test]
async fn stalled_owned_action_is_bounded_and_does_not_claim_completion() {
    let dir = TempDir::new();
    let path = dir.join("stalled-action");
    // exec replaces this fixture shell: cancellation targets exactly one owned
    // process, without leaving a child shell/sleep tree behind.
    fs::write(&path,"#!/bin/sh\nfixture_dir=${0%/*}\nprintf '%s' \"$$\" > \"$fixture_dir/pid\"\nexec /bin/sleep 5\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let executor = ActionExecutor::with_executable(path, Duration::from_millis(80)).unwrap();
    let started = std::time::Instant::now();
    assert!(
        executor
            .execute("open Spotify", Action::OpenSpotify)
            .await
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let pid = fs::read_to_string(dir.join("pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "timed-out owned child remained alive"
    );
}

async fn jev_fixture(
    choice: &str,
    confidence: f64,
    delay: Duration,
) -> (
    jarvis::providers::JevClient,
    tokio::task::JoinHandle<serde_json::Value>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut probabilities = serde_json::json!({"open_spotify":0.0,"play_spotify":0.0,"pause_spotify":0.0,"clarify":0.0,"none":1.0-confidence});
    probabilities[choice] = serde_json::json!(confidence);
    let body=serde_json::json!({"model":"jev-fixture","answers":{"action":{"type":"choice","choice":choice,"confidence":confidence,"probabilities":probabilities}},"usage":{"input_tokens":1,"output_tokens":1}}).to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        let packet = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 20000);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break serde_json::from_slice::<serde_json::Value>(
                        &bytes[end + 4..end + 4 + length],
                    )
                    .unwrap();
                }
            }
        };
        tokio::time::sleep(delay).await;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        packet
    });
    let client = jarvis::providers::JevClient::new(
        "fixture-jev",
        jarvis::providers::ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(2))
            .unwrap(),
    )
    .unwrap();
    (client, server)
}
#[tokio::test]
async fn jev_choice_reaches_action_only_when_confident_and_compatible_with_user_authorization() {
    for (request, choice, confidence, allowed) in [
        (
            "start some music using Spotify for me",
            "play_spotify",
            0.99,
            true,
        ),
        (
            "start some music using Spotify for me",
            "pause_spotify",
            0.99,
            false,
        ),
        (
            "start some music using Spotify for me",
            "play_spotify",
            0.89,
            false,
        ),
        (
            "play a song by Taylor Swift using Spotify",
            "play_spotify",
            1.0,
            false,
        ),
        (
            "open Spotify and send Brian a message",
            "open_spotify",
            1.0,
            false,
        ),
    ] {
        let dir = TempDir::new();
        let (jev, server) = jev_fixture(choice, confidence, Duration::ZERO).await;
        let executor = ActionExecutor::with_executable(
            executable(&dir, "verified:playing", 0),
            Duration::from_secs(1),
        )
        .unwrap()
        .with_jev(jev);
        let result = executor
            .handle(request)
            .await
            .expect("Spotify request must not fall into ordinary conversation");
        assert_eq!(result.is_ok(), allowed);
        assert_eq!(
            dir.join("invoked-args").exists(),
            allowed,
            "planner bypassed authorization or confidence"
        );
        assert_eq!(server.await.unwrap()["state"], request);
    }
}
#[tokio::test]
async fn stalled_jev_clarifies_within_its_deadline_and_never_executes_a_late_proposal() {
    let dir = TempDir::new();
    let (jev, server) = jev_fixture("play_spotify", 1.0, Duration::from_millis(2000)).await;
    let executor = ActionExecutor::with_executable(
        executable(&dir, "verified:playing", 0),
        Duration::from_secs(1),
    )
    .unwrap()
    .with_jev(jev);
    let start = std::time::Instant::now();
    assert!(
        executor
            .handle("start some music using Spotify for me")
            .await
            .unwrap()
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_millis(1900));
    server.await.unwrap();
    assert!(!dir.join("invoked-args").exists());
}
#[tokio::test]
async fn exact_action_bypasses_unresponsive_jev_entirely() {
    let dir = TempDir::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let jev = jarvis::providers::JevClient::new(
        "fixture-jev",
        jarvis::providers::ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(2))
            .unwrap(),
    )
    .unwrap();
    let executor = ActionExecutor::with_executable(
        executable(&dir, "verified:running", 0),
        Duration::from_secs(1),
    )
    .unwrap()
    .with_jev(jev);
    assert!(executor.handle("open Spotify").await.unwrap().is_ok());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "deterministic action contacted Jev"
    );
}

#[tokio::test]
async fn valid_proposal_slower_than_old_deadline_executes_after_verified_metadata_response() {
    let dir = TempDir::new();
    let (jev, server) = jev_fixture("play_spotify", 0.99, Duration::from_millis(700)).await;
    let executor = ActionExecutor::with_executable(
        executable(&dir, "verified:playing", 0),
        Duration::from_secs(1),
    )
    .unwrap()
    .with_jev(jev);
    let result = executor
        .handle("start some music using Spotify for me")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.action, Action::PlaySpotify);
    assert!(dir.join("invoked-args").exists());
    server.await.unwrap();
}
