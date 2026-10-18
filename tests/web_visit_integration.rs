#![cfg(unix)]
//! Website requests: only the domain the user said is opened, over https,
//! in a named browser if installed, else the default browser.
mod support;
use jarvis::{
    app_tools::authorize_open,
    providers::ToolCall,
    tool_runtime::ToolRuntime,
    web_visit::{WebVisitor, website_intent},
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use support::TempDir;

#[test]
fn spoken_and_typed_website_requests_resolve_to_the_named_domain() {
    for (request, url, browser) in [
        (
            "Can you open up Chrome and look up cartoonnetwork.com?",
            "https://cartoonnetwork.com/",
            Some("chrome"),
        ),
        ("go to nytimes.com", "https://nytimes.com/", None),
        (
            "pull up cartoon network dot com",
            "https://cartoonnetwork.com/",
            None,
        ),
        (
            "look up Cartoon Network.com in Safari",
            "https://cartoonnetwork.com/",
            Some("safari"),
        ),
        (
            "visit https://www.example.org/news/today",
            "https://example.org/news/today",
            None,
        ),
        (
            "take me to the website wsj.com please",
            "https://wsj.com/",
            None,
        ),
        ("open github.com in arc", "https://github.com/", Some("arc")),
    ] {
        let visit = website_intent(request).unwrap_or_else(|| panic!("{request}"));
        assert_eq!(visit.url, url, "{request}");
        assert_eq!(visit.browser, browser, "{request}");
    }
}

#[test]
fn non_requests_credentials_and_ambiguity_never_open_anything() {
    for request in [
        "what is cartoonnetwork.com",
        "don't open cartoonnetwork.com",
        "open cartoonnetwork.com if it's not late",
        "email brian@example.com",
        "open https://user:pass@example.com",
        "open cnn.com and nytimes.com",
        "open the Wikipedia page for Napoleon",
        "open Spotify",
        "open javascript:alert(1)",
        "go to file.txt",
        "open www.",
    ] {
        assert!(website_intent(request).is_none(), "{request}");
    }
}

/// A fake `open` that records its arguments; Applications holds only Arc.
fn visitor(dir: &TempDir) -> WebVisitor {
    let open = dir.join("open");
    fs::write(
        &open,
        "#!/bin/sh\nprintf '%s ' \"$@\" >> \"${0%/*}/opened\"\n",
    )
    .unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir_all(dir.join("Applications/Arc.app")).unwrap();
    WebVisitor::with_executable(open, vec![dir.join("Applications")])
}
fn opened(dir: &TempDir) -> String {
    fs::read_to_string(dir.join("opened")).unwrap_or_default()
}

#[tokio::test]
async fn named_installed_browser_is_used_and_a_missing_one_falls_back_to_default() {
    let dir = TempDir::new();
    let visitor = visitor(&dir);
    let text = visitor
        .visit("open github.com in Arc", "https://github.com/")
        .await
        .unwrap();
    assert_eq!(text, "Opened github.com in Arc.");
    assert_eq!(
        opened(&dir),
        "-b company.thebrowser.Browser https://github.com/ "
    );
    fs::remove_file(dir.join("opened")).unwrap();
    let text = visitor
        .visit(
            "Can you open up Chrome and look up cartoonnetwork.com?",
            "https://cartoonnetwork.com/",
        )
        .await
        .unwrap();
    assert_eq!(
        text,
        "Opened cartoonnetwork.com. Google Chrome isn't installed, so I used your default browser."
    );
    assert_eq!(opened(&dir), "https://cartoonnetwork.com/ ");
}

#[tokio::test]
async fn a_model_cannot_open_a_site_the_user_did_not_say() {
    let dir = TempDir::new();
    let runtime = ToolRuntime::read_only(dir.join("context.db")).with_web_visitor(visitor(&dir));
    let call = |url: &str| ToolCall {
        id: "w".into(),
        name: "open_website".into(),
        arguments: json!({ "url": url }),
    };
    for (request, url) in [
        ("look up cartoonnetwork.com", "https://evil.example.com/"),
        ("look up cartoonnetwork.com", "http://cartoonnetwork.com/"),
        ("what's new today", "https://cartoonnetwork.com/"),
    ] {
        runtime.execute(request, &call(url)).await;
    }
    assert!(opened(&dir).is_empty(), "{}", opened(&dir));
}

#[tokio::test]
async fn website_requests_run_directly_and_inside_multi_step_requests() {
    let dir = TempDir::new();
    let runtime = ToolRuntime::read_only(dir.join("context.db")).with_web_visitor(visitor(&dir));
    let result = runtime
        .handle_resolved_intent("Can you open up Chrome and look up cartoonnetwork.com?")
        .await
        .unwrap();
    assert!(
        result
            .acknowledgment
            .unwrap()
            .starts_with("Opened cartoonnetwork.com.")
    );
    assert!(jarvis::request_intent::resolved_action("go to nytimes.com"));
    assert_eq!(
        jarvis::request_intent::command_clauses("go to nytimes.com and play Metallica").unwrap(),
        vec!["go to nytimes.com and play Metallica"],
        "\"go\" alone is too ambiguous to split on"
    );
    assert_eq!(
        jarvis::request_intent::command_clauses("play Metallica and visit nytimes.com").unwrap(),
        vec!["play Metallica", "visit nytimes.com"]
    );
    let _ = PathBuf::new();
}

#[test]
fn app_requests_accept_natural_opening_phrases() {
    for request in [
        "open Notes",
        "open up Notes",
        "pull up Notes",
        "bring up the Notes app",
        "fire up Notes",
        "can you open up Notes",
        "go ahead and launch Notes",
        "start up Notes please",
        "show me Notes",
    ] {
        assert!(authorize_open(request, "Notes").is_ok(), "{request}");
    }
    for request in [
        "don't open Notes",
        "open Notes's file",
        "what is Notes",
        "open Calendar",
    ] {
        assert!(authorize_open(request, "Notes").is_err(), "{request}");
    }
}
