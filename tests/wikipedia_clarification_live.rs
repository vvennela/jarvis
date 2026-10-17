//! Opt-in public browser acceptance; no private context or automatic CI actions.
#![cfg(target_os = "macos")]
use jarvis::{
    context::{ContextStore, Limits},
    conversation::Conversation,
    tool_runtime::ToolRuntime,
    web_open::WebOpener,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_WIKIPEDIA_CLARIFICATION=1; real model and two public browser opens"]
async fn generic_page_request_and_named_page_clarification_open_exact_article() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_WIKIPEDIA_CLARIFICATION").as_deref(),
        Ok("1")
    );
    for (scenario, original, requires_followup) in [
        ("generic_named_page", "open the page for Napoleon", false),
        ("missing_topic", "open the Wikipedia page", true),
    ] {
        if std::env::var("JARVIS_WIKIPEDIA_SCENARIO")
            .ok()
            .is_some_and(|only| only != scenario)
        {
            continue;
        }
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runtime")
            .join(format!("live-wikipedia-{scenario}-{suffix}"));
        fs::create_dir_all(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let database = directory.join("context.sqlite");
        drop(ContextStore::open(&database, Limits::default()).unwrap());
        let marker = directory.join("opened-public-urls");
        let escaped_marker = marker.to_str().unwrap().replace('\'', "'\\''");
        let wrapper = directory.join("native-open");
        fs::write(&wrapper, format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{escaped_marker}'\nexec /usr/bin/open \"$@\"\n")).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = ToolRuntime::read_only(database.clone())
            .with_web_opener(WebOpener::with_executable(wrapper).unwrap());
        let (mut conversation, observer) =
            if std::env::var("JARVIS_TRACE_PUBLIC_TOOLS").as_deref() == Ok("1") {
                let (conversation, observer) = observed_conversation(database).await;
                (conversation, Some(observer))
            } else {
                (Conversation::from_env(database).unwrap(), None)
            };
        conversation.set_tool_runtime(Arc::new(runtime));
        let started = Instant::now();
        let mut calls = 1;
        let mut reply = tokio::time::timeout(Duration::from_secs(30), conversation.ask(original))
            .await
            .expect("initial request deadline")
            .expect("initial model request failed");
        if requires_followup {
            assert!(
                reply.needs_followup,
                "missing topic must ask typed clarification; public reply: {}",
                reply.text
            );
            assert!(
                !marker.exists(),
                "a missing topic must not open an arbitrary article"
            );
        }
        if reply.needs_followup {
            assert!(
                !marker.exists(),
                "clarification must precede browser action"
            );
            calls += 1;
            reply = tokio::time::timeout(Duration::from_secs(30), conversation.ask("Napoleon."))
                .await
                .expect("followup deadline")
                .expect("followup model request failed");
        }
        if let Some(observer) = observer {
            observer.abort();
            let _ = observer.await;
        }
        let opened = fs::read_to_string(&marker).unwrap_or_default();
        println!(
            "scenario={scenario} conversation_calls={calls} browser_opens={} elapsed_ms={} public_reply={:?}",
            opened.lines().count(),
            started.elapsed().as_millis(),
            reply.text
        );
        assert_eq!(
            opened.lines().collect::<Vec<_>>(),
            vec!["https://en.wikipedia.org/wiki/Napoleon"],
            "exactly one canonical article launch required per scenario"
        );
        assert_eq!(reply.text, "Opened Napoleon on Wikipedia.");
        assert!(!reply.needs_followup);
    }
}

/// Test-only observer for these public empty-database requests. Real credentials
/// go only to the fixed official HTTPS service; loopback receives a placeholder.
async fn observed_conversation(database: PathBuf) -> (Conversation, tokio::task::JoinHandle<()>) {
    use jarvis::{
        conversation::load_provider_settings,
        providers::{CRUSOE_ENDPOINT, CrusoeClient, JevClient, NEMOTRON_MODEL, ProviderConfig},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let settings = load_provider_settings().unwrap();
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!(
        "Bearer {}",
        settings
            .get("CRUSOE_API_KEY")
            .expect("provider not configured")
    ))
    .unwrap();
    authorization.set_sensitive(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let observer = tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        for _ in 0..6 {
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_secs(30), listener.accept()).await
            else {
                break;
            };
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let body = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() <= 70 * 1024);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(length <= 64 * 1024);
                    if bytes.len() >= end + 4 + length {
                        break bytes[end + 4..end + 4 + length].to_vec();
                    }
                }
            };
            let mut response = client
                .post(CRUSOE_ENDPOINT)
                .header(reqwest::header::AUTHORIZATION, authorization.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .expect("public observer provider transport");
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.unwrap() {
                assert!(body.len() + chunk.len() <= 1024 * 1024);
                body.extend_from_slice(&chunk);
            }
            if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) {
                if let Some(calls) = parsed.pointer("/choices/0/message/tool_calls") {
                    println!("public_fixture_tool_calls={calls}");
                } else {
                    println!("public_fixture_provider_status={status} tool_calls=none");
                }
            }
            let header = format!(
                "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let jev = settings
        .get("TYPESAFE_API_KEY")
        .map(|key| JevClient::production(key.clone()).unwrap());
    let lm = CrusoeClient::new(
        "observer-placeholder",
        settings
            .get("JARVIS_CONVERSATION_MODEL")
            .map(String::as_str)
            .unwrap_or(NEMOTRON_MODEL),
        ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(25)).unwrap(),
    )
    .unwrap();
    (Conversation::new(database, jev, lm), observer)
}
