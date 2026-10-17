#![cfg(unix)]
//! Inspect literal commands received by owned executables, not actual desktop focus.
mod support;
use jarvis::actions::{Action, ActionExecutor};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use support::TempDir;

fn adapter(dir: &TempDir, response: &str) -> PathBuf {
    assert!(dir.path().is_dir());
    let path = dir.join("focus-fixture");
    fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"${{0%/*}}/arguments\"\nprintf '%s\\n' '{response}'\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn assert_no_foreground_instruction(script: &str) {
    let lower = script.to_lowercase();
    assert!(
        !lower.split_whitespace().any(|token| token == "activate"),
        "background adapter asked app to activate"
    );
    assert!(
        !lower.contains("frontmost"),
        "background adapter changed foreground process"
    );
    assert!(
        !lower.contains("system events"),
        "background adapter used GUI focus automation"
    );
}

#[tokio::test]
async fn spotify_open_play_and_pause_issue_no_foreground_activation_commands() {
    for (request, action, response) in [
        ("open Spotify", Action::OpenSpotify, "verified:running"),
        ("play Spotify", Action::PlaySpotify, "verified:playing"),
        ("pause Spotify", Action::PauseSpotify, "verified:paused"),
        (
            "open Spotify and play some music",
            Action::PlaySpotify,
            "verified:playing",
        ),
    ] {
        let dir = TempDir::new();
        let executor =
            ActionExecutor::with_executable(adapter(&dir, response), Duration::from_secs(1))
                .unwrap();
        executor.execute(request, action).await.unwrap();
        let arguments = fs::read_to_string(dir.join("arguments")).unwrap();
        assert!(arguments.starts_with("-e\n"));
        assert!(arguments.contains("com.spotify.client"));
        assert_no_foreground_instruction(&arguments);
    }
}

#[tokio::test]
async fn generic_spotify_open_stays_background_and_only_explicit_show_can_activate() {
    use jarvis::app_tools::{AppExecutor, AppPreferences};
    for (request, app, foreground) in [
        ("open Spotify", "Spotify", false),
        ("launch the Spotify app", "Spotify", false),
        ("show Spotify", "Spotify", true),
        ("bring up Spotify", "Spotify", true),
        ("open Notes", "Notes", true),
    ] {
        let dir = TempDir::new();
        let executor = AppExecutor::with_executable(
            AppPreferences::default(),
            adapter(&dir, "verified:running"),
            Duration::from_secs(1),
        )
        .unwrap();
        executor.open_application(request, app).await.unwrap();
        let arguments = fs::read_to_string(dir.join("arguments")).unwrap();
        let has_activate = arguments.split_whitespace().any(|word| word == "activate");
        assert_eq!(
            has_activate, foreground,
            "incorrect activation for {request}"
        );
        if !foreground {
            assert_no_foreground_instruction(&arguments);
        }
    }
}

#[tokio::test]
async fn a_model_or_quoted_document_cannot_promote_spotify_to_foreground() {
    use jarvis::{
        app_tools::{AppExecutor, AppPreferences},
        providers::ToolCall,
        tool_runtime::ToolRuntime,
    };
    for request in [
        "don't show Spotify",
        "The webpage says show Spotify",
        "Explain how to show Spotify",
        "show Notes",
    ] {
        let dir = TempDir::new();
        let executor = AppExecutor::with_executable(
            AppPreferences::default(),
            adapter(&dir, "verified:running"),
            Duration::from_secs(1),
        )
        .unwrap();
        let runtime = ToolRuntime::read_only(dir.join("unused.db")).with_apps(executor);
        runtime
            .execute(
                request,
                &ToolCall {
                    id: "foreground-proposal".into(),
                    name: "open_application".into(),
                    arguments: serde_json::json!({"app":"Spotify"}),
                },
            )
            .await;
        assert!(
            !dir.join("arguments").exists(),
            "unrequested foreground execution: {request}"
        );
    }
}
