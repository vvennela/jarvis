//! Opt-in configured inference plus the real platform file watcher; fictional data only.
#![cfg(unix)]
use jarvis::{
    awareness::{ScanReport, watch_root_controlled},
    context::{ContextStore, Limits},
    conversation::Conversation,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
struct WatchGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<ScanReport>>,
}
impl WatchGuard {
    fn finish(&mut self) -> ScanReport {
        self.stop.store(true, Ordering::Release);
        self.handle.take().unwrap().join().expect("watcher failed")
    }
}
impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
#[tokio::test]
#[ignore = "requires JARVIS_LIVE_CONTEXT=1 and configured provider; two public-fixture inference calls"]
async fn live_file_change_updates_the_same_conversation_without_stale_priority() {
    assert_eq!(std::env::var("JARVIS_LIVE_CONTEXT").as_deref(), Ok("1"));
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join(format!("live-context-{suffix}"));
    fs::create_dir_all(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let approved = directory.join("approved");
    fs::create_dir(&approved).unwrap();
    let file = approved.join("CURRENT_WORK.md");
    let first = "# Fictional work priorities\n\nCurrent priority: Project Atlas.\nNext step: finish the Atlas prototype before starting other projects.\n";
    let second = "# Fictional work priorities\n\nCurrent priority: Project Beacon.\nNext step: finish the Beacon prototype before starting other projects.\n";
    fs::write(&file, first).unwrap();
    let id = format!("file:{}", file.display());
    let database = directory.join("context.sqlite");
    let mut store = ContextStore::open(&database, Limits::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        watch_root_controlled(
            &mut store,
            &approved,
            Duration::from_secs(120),
            move || {
                let _ = ready_tx.send(());
            },
            move || stopped.load(Ordering::Acquire),
        )
        .expect("approved fixture watcher failed")
    });
    let mut watcher = WatchGuard {
        stop,
        handle: Some(handle),
    };
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("watcher startup deadline");
    let observer = ContextStore::open_readonly(&database).unwrap();
    let initial = observer
        .get(&id)
        .unwrap()
        .expect("initial scan did not publish");
    assert_eq!(initial.content, first);
    let mut conversation = Conversation::from_env(database).unwrap();
    let question = "According to CURRENT_WORK.md, which project is the current priority? Answer only its project name.";
    let start = Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(45), conversation.ask(question))
        .await
        .expect("first answer timeout")
        .expect("first inference failed");
    assert!(
        response.text.to_lowercase().contains("atlas"),
        "first public-fixture answer: {}",
        response.text
    );
    assert!(!response.text.to_lowercase().contains("beacon"));
    let first_ms = start.elapsed().as_millis();
    let changed_at = Instant::now();
    fs::write(&file, second).unwrap();
    let version = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(item) = observer.get(&id).unwrap()
                && item.version > initial.version
                && item.content == second
            {
                break item.version;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("platform watcher did not publish changed evidence");
    let watch_ms = changed_at.elapsed().as_millis();
    let second_at = Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(45), conversation.ask(question))
        .await
        .expect("second answer timeout")
        .expect("second inference failed");
    assert!(
        response.text.to_lowercase().contains("beacon"),
        "updated public-fixture answer: {}",
        response.text
    );
    assert!(
        !response.text.to_lowercase().contains("atlas"),
        "old priority survived new indexed evidence: {}",
        response.text
    );
    let second_ms = second_at.elapsed().as_millis();
    let report = watcher.finish();
    assert!(report.resources_changed >= 2);
    println!(
        "live_context_refresh_verified=true same_conversation=true initial_version={} updated_version={} first_answer_ms={} watch_update_ms={} second_answer_ms={} conversation_calls=2 ambient_capture=false",
        initial.version, version, first_ms, watch_ms, second_ms
    );
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_WEB_CONVERSATION=1; one observed public You query and configured conversation"]
async fn real_conversation_uses_public_you_search_and_cites_returned_evidence() {
    use jarvis::{
        conversation::load_provider_settings,
        tool_runtime::ToolRuntime,
        web_search::{MAX_RESPONSE_BYTES, SEARCH_ENDPOINT, WebSearch, WebSearchConfig},
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    assert_eq!(
        std::env::var("JARVIS_LIVE_WEB_CONVERSATION").as_deref(),
        Ok("1")
    );
    const QUESTION: &str = "Research NVIDIA Nemotron 3 Nano using the web and briefly explain what it is. Cite one public source URL.";
    let settings = load_provider_settings().unwrap();
    let mut key = reqwest::header::HeaderValue::from_str(
        settings
            .get("YOU_API_KEY")
            .expect("public search credentials missing"),
    )
    .unwrap();
    key.set_sensitive(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    // Only a placeholder credential crosses loopback. The observer verifies the
    // public query, then forwards it to the fixed official HTTPS endpoint.
    let observer = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(45),async move {
            let (mut socket,_)=listener.accept().await.unwrap();
            let mut bytes=Vec::new();let mut buffer=[0u8;2048];
            let request:Value=loop {
                let count=socket.read(&mut buffer).await.unwrap();assert!(count>0);
                bytes.extend_from_slice(&buffer[..count]);assert!(bytes.len()<=8192);
                if let Some(end)=bytes.windows(4).position(|w|w==b"\r\n\r\n") {
                    let header=String::from_utf8_lossy(&bytes[..end]);
                    let length=header.lines().find_map(|line| {let (name,value)=line.split_once(':')?;name.eq_ignore_ascii_case("content-length").then(||value.trim().parse::<usize>().unwrap())}).unwrap();
                    assert!(length<=4096);
                    if bytes.len()>=end+4+length {break serde_json::from_slice(&bytes[end+4..end+4+length]).unwrap();}
                }
            };
            assert_eq!(request,json!({"query":QUESTION,"count":5}));
            let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().connect_timeout(Duration::from_secs(3)).timeout(Duration::from_secs(9)).build().unwrap();
            let mut response=client.post(SEARCH_ENDPOINT).header("X-API-Key",key).json(&request).send().await.expect("official public search transport failed");
            assert_eq!(response.status().as_u16(),200,"official public search status");
            let mut body=Vec::new();
            while let Some(chunk)=response.chunk().await.expect("public response body failed") {assert!(body.len()+chunk.len()<=MAX_RESPONSE_BYTES);body.extend_from_slice(&chunk);}
            let parsed:Value=serde_json::from_slice(&body).unwrap();
            let urls:Vec<String>=parsed.pointer("/results/web").and_then(Value::as_array).expect("public search web results missing").iter().take(5).filter_map(|item|item.get("url").and_then(Value::as_str).map(str::to_owned)).collect();
            assert!(!urls.is_empty());
            let header=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(header.as_bytes()).await.unwrap();socket.write_all(&body).await.unwrap();
            urls
        }).await.expect("public-search observer deadline exceeded")
    });
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runtime")
        .join(format!("live-web-conversation-{suffix}"));
    fs::create_dir_all(&directory).unwrap();
    let database = directory.join("context.sqlite");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    let search = WebSearch::new(
        "observer-placeholder",
        WebSearchConfig::loopback_for_tests(&endpoint, Duration::from_secs(10)).unwrap(),
    )
    .unwrap();
    let mut conversation = Conversation::from_env(database.clone()).unwrap();
    conversation.set_tool_runtime(Arc::new(
        ToolRuntime::read_only(database).with_web_search(search),
    ));
    let start = Instant::now();
    let answer = tokio::time::timeout(Duration::from_secs(60), conversation.ask(QUESTION))
        .await
        .expect("web conversation deadline exceeded")
        .expect("web conversation failed");
    assert!(
        observer.is_finished(),
        "the model did not complete a real public-search request; public reply: {}",
        answer.text
    );
    let urls = observer.await.expect("public search observer failed");
    assert!(answer.text.to_lowercase().contains("nemotron"));
    assert!(
        urls.iter().any(|url| answer.text.contains(url)),
        "public answer did not cite a returned search URL"
    );
    println!(
        "conversation_web_tool_verified=true official_you_request_verified=true returned_source_cited=true elapsed_ms={} answer_bytes={} public_queries=1 private_context_sent=false observer_proxy=true",
        start.elapsed().as_millis(),
        answer.text.len()
    );
}
