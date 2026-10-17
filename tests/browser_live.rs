//! Explicit opt-in installed-browser acceptance. Reads only the localhost demo fixture.
#![cfg(unix)]
use jarvis::{
    browser_tools::{BrowserClient, BrowserPage},
    context::{ContextStore, Source},
};
use std::{path::PathBuf, time::Instant};

const QUERY: &str = "Jarvis demo project priorities";
const URL: &str = "http://localhost:8765/";
fn expected() -> String {
    assert_eq!(std::env::var("JARVIS_LIVE_BROWSER").as_deref(), Ok("1"));
    let expected = std::env::var("JARVIS_LIVE_BROWSER_EXPECTED").unwrap_or_else(|_| "Atlas".into());
    assert!(
        matches!(expected.as_str(), "Atlas" | "Beacon"),
        "Only public fixture phases are supported"
    );
    expected
}
fn verify_page(page: &BrowserPage, expected: &str) {
    assert!(
        page.tab.url == URL,
        "The read must remain on the approved fixture"
    );
    assert_eq!(page.trust, "untrusted_page");
    assert!(
        !page.content.contains("Private-field fixture"),
        "Private fixture escaped extraction"
    );
    assert!(
        !page.content.contains("this text must never appear"),
        "Private fixture escaped extraction"
    );
    if expected == "Atlas" {
        assert!(
            page.content
                .contains("The Atlas prototype is the highest priority."),
            "Fresh Atlas text absent"
        );
        assert!(
            page.content.contains("Revision 1"),
            "Wrong fixture revision"
        );
        assert!(
            !page.content.contains("Beacon is now the highest priority."),
            "Unpublished form value escaped extraction"
        );
    } else {
        assert!(
            page.content.contains("Beacon is now the highest priority."),
            "Fresh Beacon text absent"
        );
        assert!(
            page.content.contains("Revision 2"),
            "Wrong fixture revision"
        );
        assert!(
            !page
                .content
                .contains("The Atlas prototype is the highest priority."),
            "Old priority survived fresh read"
        );
    }
}
async fn read_fixture(client: &BrowserClient) -> BrowserPage {
    let tabs = client
        .search_tabs(QUERY)
        .await
        .expect("Installed browser search failed");
    assert_eq!(
        tabs.len(),
        1,
        "Expected one uniquely named demo tab; no tab metadata is printed"
    );
    let tab = &tabs[0];
    assert!(tab.url == URL, "Refusing to read a non-fixture tab");
    assert!(tab.title == QUERY, "Refusing to read a non-fixture tab");
    client
        .read_tab(&tab.resource_id, &tab.document_id)
        .await
        .expect("Installed browser fresh read failed")
}
#[tokio::test]
#[ignore = "requires JARVIS_LIVE_BROWSER=1 and installed connected extension with localhost demo approved"]
async fn installed_browser_fresh_page_and_live_index_match_fixture_phase() {
    let expected = expected();
    let started = Instant::now();
    let client = BrowserClient::native().expect("Native browser socket unavailable");
    let page = read_fixture(&client).await;
    verify_page(&page, &expected);
    let database = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/live.sqlite");
    let store = ContextStore::open_readonly(&database).expect("Live awareness index unavailable");
    let resource_id = format!(
        "browser-page:{}:{}",
        page.tab.tab_id,
        blake3::hash(URL.as_bytes()).to_hex()
    );
    let stored = store
        .get(&resource_id)
        .expect("Live index read failed")
        .expect("Fresh browser page was not persisted");
    assert_eq!(stored.source, Source::Browser);
    assert!(
        stored.content == page.content,
        "Live index disagrees with the verified fresh page"
    );
    assert!(stored.version > 0);
    eprintln!(
        "live_browser_phase={} read_and_index_ms={} indexed_version={}",
        expected,
        started.elapsed().as_millis(),
        stored.version
    );
}

/// A fixture-only socket relay keeps the live model away from all other tabs and
/// the real awareness database, while preserving the actual native transport.
#[tokio::test]
#[ignore = "requires JARVIS_LIVE_BROWSER_MODEL=1, provider configuration and approved installed localhost fixture"]
async fn configured_model_reads_installed_fixture_through_real_browser_tools() {
    use jarvis::{context::Limits, conversation::Conversation, tool_runtime::ToolRuntime};
    use serde_json::{Value, json};
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    assert_eq!(
        std::env::var("JARVIS_LIVE_BROWSER_MODEL").as_deref(),
        Ok("1")
    );
    let expected = expected();
    let native = BrowserClient::native().unwrap();
    let initial = read_fixture(&native).await;
    verify_page(&initial, &expected);
    let approved = initial.tab;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let directory = PathBuf::from("/tmp").join(format!("jarvis-browser-live-{suffix}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let socket_path = directory.join("fixture.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).unwrap();
    let searches = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let (search_count, read_count) = (searches.clone(), reads.clone());
    let phase = expected.clone();
    let relay = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::time::timeout(Duration::from_secs(8), async {
                let len = socket.read_u32_le().await.unwrap() as usize;
                assert!((1..=4096).contains(&len), "Fixture request exceeded cap");
                let mut bytes = vec![0; len]; socket.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let mut response = json!({"request_id": request["request_id"], "status":"permission_required", "tabs":[], "page":null});
                if request["operation"] == "search_tabs" && request["arguments"]["query"] == QUERY {
                    let tabs = native.search_tabs(QUERY).await.unwrap();
                    assert_eq!(tabs.len(), 1, "Fixture search must remain unique");
                    assert!(tabs[0].url == URL && tabs[0].resource_id == approved.resource_id && tabs[0].document_id == approved.document_id, "Fixture changed identity");
                    response["status"] = json!("ok"); response["tabs"] = json!(tabs);
                    search_count.fetch_add(1, Ordering::SeqCst);
                } else if request["operation"] == "read_tab" && request["arguments"]["resource_id"] == approved.resource_id && request["arguments"]["expected_document_id"] == approved.document_id {
                    let page = native.read_tab(&approved.resource_id, &approved.document_id).await.unwrap();
                    verify_page(&page, &phase);
                    response["status"] = json!("ok"); response["page"] = json!(page);
                    read_count.fetch_add(1, Ordering::SeqCst);
                }
                let body = serde_json::to_vec(&response).unwrap();
                assert!(body.len() <= jarvis::browser_tools::MAX_FRAME);
                socket.write_u32_le(body.len() as u32).await.unwrap(); socket.write_all(&body).await.unwrap();
            }).await.expect("Fixture relay deadline");
        }
    });
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = Abort(relay.abort_handle());
    let database = directory.join("empty.sqlite");
    drop(ContextStore::open(&database, Limits::default()).unwrap());
    let mut conversation = Conversation::from_env(database.clone()).unwrap();
    conversation.set_tool_runtime(Arc::new(
        ToolRuntime::read_only(database).with_browser(BrowserClient::new(socket_path).unwrap()),
    ));
    let started = Instant::now();
    let reply = tokio::time::timeout(Duration::from_secs(60), conversation.ask("Use browser_search_tabs with query exactly Jarvis demo project priorities, then browser_read_tab on that tab. From the fresh page text, what is the current highest-priority project? Answer only its project name.")).await.expect("Browser model deadline").expect("Browser model request failed");
    assert!(
        searches.load(Ordering::SeqCst) > 0,
        "Model did not search the real browser"
    );
    assert!(
        reads.load(Ordering::SeqCst) > 0,
        "Model did not read the real browser"
    );
    assert!(
        reply.text.to_lowercase().contains(&expected.to_lowercase()),
        "Reply missed the current fixture priority"
    );
    let old = if expected == "Atlas" {
        "beacon"
    } else {
        "atlas"
    };
    assert!(
        !reply.text.to_lowercase().contains(old),
        "Reply used an unpublished or stale priority"
    );
    eprintln!(
        "live_browser_model_phase={} elapsed_ms={} real_searches={} real_reads={} reply_bytes={} private_context_sent=false",
        expected,
        started.elapsed().as_millis(),
        searches.load(Ordering::SeqCst),
        reads.load(Ordering::SeqCst),
        reply.text.len()
    );
}
