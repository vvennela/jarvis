//! Explicit opt-in public fixture through the configured model and background worker.
use jarvis::{
    context::{ContextStore, Limits, Source},
    conversation::Conversation,
    tool_runtime::ToolRuntime,
};
use jarvis_panel::tasks::{State, TaskWorker};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_FILE_CONVERSATION=1 and configured providers; real inference, no viewer"]
async fn real_conversation_delegates_file_revision_and_recovers_verified_review() {
    assert_eq!(
        std::env::var("JARVIS_LIVE_FILE_CONVERSATION").as_deref(),
        Ok("1")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    // Cargo runs this one explicit ignored test in the panel package directory.
    // Use the application's production project-root .env loader, never copy keys.
    std::env::set_current_dir(&root).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = root.join("runtime").join(format!(
        "live-conversation-edit-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let source = directory.join("beacon.md");
    let original = "# Fictional Beacon\n\nStatus: planned\nPreserve this sentence.\n";
    fs::write(&source, original).unwrap();
    let database = directory.join("context.sqlite");
    let mut store = ContextStore::open(&database, Limits::default()).unwrap();
    store
        .observe(
            &format!("file:{}", source.display()),
            original,
            Source::File,
            1,
        )
        .unwrap();
    drop(store);
    let output = directory.join("reviews");
    let mut worker = TaskWorker::start(database.clone(), output.clone()).unwrap();
    let runtime = ToolRuntime::read_only(database.clone()).with_file_edits(worker.control.clone());
    let mut conversation = Conversation::from_env(database.clone()).unwrap();
    conversation.set_tool_runtime(Arc::new(runtime));
    let started = Instant::now();
    let reply=tokio::time::timeout(Duration::from_secs(60),conversation.ask("Update beacon.md: replace 'Status: planned' with 'Status: ready'. Preserve all other text and prepare the revised file for review.")).await.expect("conversation deadline exceeded").expect("live conversation failed");
    assert_eq!(
        reply.text,
        "I'm on it. I'll leave the revised file ready for your review."
    );
    tokio::time::timeout(Duration::from_secs(90), async {
        while worker.control.busy() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background edit deadline exceeded");
    let snapshot = worker.control.snapshot();
    assert_eq!(
        snapshot.state,
        State::ReadyForReview,
        "background state: {}",
        snapshot.text
    );
    let artifact = snapshot.path.unwrap();
    let content = fs::read_to_string(&artifact).unwrap();
    assert_eq!(
        content.trim(),
        original.replace("Status: planned", "Status: ready").trim()
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    worker.control.shutdown();
    worker.join().unwrap();
    let mut restored = TaskWorker::start(database, output).unwrap();
    assert_eq!(restored.control.snapshot().state, State::ReadyForReview);
    assert_eq!(restored.control.snapshot().path.as_ref(), Some(&artifact));
    restored.control.shutdown();
    restored.join().unwrap();
    println!(
        "conversation_file_tool_verified=true prepared_review_verified=true restart_recovery_verified=true source_unchanged=true elapsed_ms={} artifact_bytes={} viewer_opened=false",
        started.elapsed().as_millis(),
        content.len()
    );
}
