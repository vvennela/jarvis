//! Explicitly opted-in production-provider check. Never launches a viewer.
//! Run from the repository root with JARVIS_RUN_LIVE_DRAFT_TEST=1 and an
//! absolute JARVIS_LIVE_CONTEXT_DB; .env is read through production loaders.
use jarvis_panel::tasks::{State, TaskWorker};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
#[ignore = "contacts configured inference provider using explicitly approved project context"]
fn configured_worker_prepares_unsent_project_handoff_and_revokes_tampered_review() {
    assert_eq!(
        std::env::var("JARVIS_RUN_LIVE_DRAFT_TEST").ok().as_deref(),
        Some("1"),
        "explicit live-test opt-in required"
    );
    let database = PathBuf::from(
        std::env::var_os("JARVIS_LIVE_CONTEXT_DB").expect("explicit context database required"),
    );
    assert!(
        database.is_absolute() && database.is_file(),
        "existing absolute context database required"
    );
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    std::env::set_current_dir(project).expect("production configuration directory");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = database
        .parent()
        .unwrap()
        .join(format!("live-draft-check-{}-{unique}", std::process::id()));
    let mut worker =
        TaskWorker::start(database, output.clone()).expect("production worker should start");
    let started = Instant::now();
    worker.control.submit("Draft a concise handoff for Brian about Jarvis's current priorities, unfinished work, and the hackathon deadline. Use the current work note as reported evidence. State the most urgent unfinished task and distinguish verified progress from unverified human wake reliability. Leave this as an unsent local draft; do not invent an email address.".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    while worker.control.busy() {
        if Instant::now() >= deadline {
            worker.control.cancel();
            panic!("live draft deadline exceeded");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let ready = worker.control.snapshot();
    assert!(
        ready.state == State::ReadyForReview,
        "live draft did not reach ready: state={:?}; category={}",
        ready.state,
        ready.text
    );
    let elapsed_ms = started.elapsed().as_millis();
    let path = ready.path.expect("ready artifact path");
    let canonical_output = output.canonicalize().expect("created output directory");
    assert_eq!(path.parent(), Some(canonical_output.as_path()));
    let bytes = fs::read(&path).expect("local artifact readable");
    let text = std::str::from_utf8(&bytes).expect("UTF-8 artifact");
    assert!(
        text.starts_with("LOCAL DRAFT FOR REVIEW — NOT SENT"),
        "local unsent header required"
    );
    let lower = text.to_ascii_lowercase();
    let mentions_project = lower.contains("jarvis");
    let mentions_wake_priority =
        lower.contains("wake") && (lower.contains("first") || lower.contains("reliab"));
    assert!(
        mentions_project && mentions_wake_priority,
        "draft must use current project priority"
    );
    println!(
        "live_draft state=ReadyForReview latency_ms={elapsed_ms} bytes={} project_priority_present=true sent=false viewer_opened=false",
        bytes.len()
    );

    // The only explicit review request is made after tampering, so the worker
    // must revoke readiness before its production /usr/bin/open can run.
    let mut altered = bytes;
    altered.extend_from_slice(
        b"\nTEST VALIDATION: this artifact was deliberately modified; prior readiness revoked.\n",
    );
    fs::write(&path, altered).unwrap();
    worker.control.request_review().unwrap();
    let review_deadline = Instant::now() + Duration::from_secs(5);
    while worker.control.busy() {
        assert!(
            Instant::now() < review_deadline,
            "review validation deadline exceeded"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let reviewed = worker.control.snapshot();
    assert_eq!(reviewed.state, State::Blocked);
    assert!(reviewed.path.is_none());
    println!("live_review state=Blocked tampered_artifact_rejected=true viewer_opened=false");
    worker.control.shutdown();
    worker.join().unwrap();
}
