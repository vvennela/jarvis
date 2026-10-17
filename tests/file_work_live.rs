//! Opt-in real inference against fictional local files; no viewer or send action.
use jarvis::{
    context::{ContextStore, Limits, Source},
    draft::{BlockReason, DraftOutcome, DraftTask},
    file_work::FileEditTask,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const ORIGINAL: &str =
    "# Fictional Beacon\n\nStatus: planned\nRelease date: November 10.\nPreserve this sentence.\n";

fn fixture() -> (PathBuf, PathBuf, String) {
    assert_eq!(std::env::var("JARVIS_LIVE_FILE_WORK").as_deref(), Ok("1"));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
    fs::create_dir_all(&root).unwrap();
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).unwrap();
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let directory = root.join(format!("live-file-fixture-{suffix}"));
    fs::create_dir(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let source = directory.join("beacon.md");
    fs::write(&source, ORIGINAL).unwrap();
    let resource_id = format!("file:{}", source.display());
    let mut store =
        ContextStore::open(&directory.join("context.sqlite"), Limits::default()).unwrap();
    store
        .observe(&resource_id, ORIGINAL, Source::File, 1)
        .unwrap();
    (directory, source, resource_id)
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_FILE_WORK=1 and configured provider; one inference request"]
async fn real_file_edit_preserves_source_and_invalidates_tampered_review() {
    let (directory, source, id) = fixture();
    let output = directory.join("beacon.prepared.md");
    let mut task = FileEditTask::from_env(
        directory.join("context.sqlite"),
        "live-edit",
        "live-request",
    )
    .unwrap();
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(60), task.generate(
        "In beacon.md, replace 'Status: planned' with 'Status: ready'. Preserve every other character and return the complete revised Markdown file.",
        &id,
        &output,
        Arc::new(AtomicBool::new(false)),
    )).await.expect("live edit exceeded deadline").expect("live file inference failed");
    assert!(matches!(outcome, DraftOutcome::ReadyForReview { .. }));
    assert_eq!(fs::read_to_string(&source).unwrap(), ORIGINAL);
    let prepared = fs::read_to_string(&output).unwrap();
    assert_eq!(
        prepared.trim(),
        ORIGINAL.replace("Status: planned", "Status: ready").trim()
    );
    assert!(matches!(
        task.review().unwrap(),
        DraftOutcome::ReadyForReview { .. }
    ));
    fs::write(&output, "Changed by the live test after generation.\n").unwrap();
    assert!(matches!(
        task.review().unwrap(),
        DraftOutcome::Blocked {
            reason: BlockReason::ArtifactChanged,
            ..
        }
    ));
    println!(
        "file_edit_verified=true source_unchanged=true tamper_blocked=true elapsed_ms={} prepared_bytes={} requests=1 viewer_opened=false",
        started.elapsed().as_millis(),
        prepared.len()
    );
}

#[tokio::test]
#[ignore = "requires JARVIS_LIVE_FILE_WORK=1 and configured provider; one inference request"]
async fn real_local_draft_uses_fictional_evidence_and_stops_for_review() {
    let (directory, source, id) = fixture();
    let output = directory.join("handoff.review.txt");
    let mut task = DraftTask::from_env(
        directory.join("context.sqlite"),
        "live-draft",
        "live-request",
    )
    .unwrap();
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(60), task.generate(
        "Prepare a two-sentence local handoff draft for the fictional Beacon project. Mention its planned status and release date November 10. Do not send anything.",
        &[id],
        &output,
        Arc::new(AtomicBool::new(false)),
    )).await.expect("live draft exceeded deadline").expect("live draft inference failed");
    assert!(matches!(outcome, DraftOutcome::ReadyForReview { .. }));
    let prepared = fs::read_to_string(&output).unwrap();
    assert!(prepared.contains("Beacon") && prepared.contains("November 10"));
    assert_eq!(fs::read_to_string(&source).unwrap(), ORIGINAL);
    assert!(matches!(
        task.review().unwrap(),
        DraftOutcome::ReadyForReview { .. }
    ));
    println!(
        "local_draft_verified=true ready_for_review=true elapsed_ms={} prepared_bytes={} requests=1 sent=false viewer_opened=false",
        started.elapsed().as_millis(),
        prepared.len()
    );
}
