//! Reproducible local awareness measurement, using only generated fixture files.
use jarvis::{awareness::watch_root_controlled, context::ContextStore};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("jarvis-benchmark-{}", std::process::id()));
    // create_dir refuses to reuse a directory from an interrupted run.
    fs::create_dir(&root)?;
    let files = root.join("files");
    fs::create_dir(&files)?;
    for index in 0..100 {
        fs::write(
            files.join(format!("note-{index}.md")),
            format!(
                "Fixture project {index}\n{}",
                "Generated benchmark text.\n".repeat(40)
            ),
        )?;
    }
    let db = root.join("context.sqlite");
    let canonical_files = fs::canonicalize(&files)?;
    let target = files.join("note-0.md");
    let id = format!("file:{}", canonical_files.join("note-0.md").display());
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_db = db.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let startup = Instant::now();
    let worker = thread::spawn(move || -> Result<(), String> {
        let mut store =
            ContextStore::open(&worker_db, Default::default()).map_err(|e| e.to_string())?;
        watch_root_controlled(
            &mut store,
            &files,
            Duration::from_secs(60),
            || {
                let _ = ready_tx.send(());
            },
            || worker_stop.load(Ordering::Acquire),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    });
    ready_rx.recv_timeout(Duration::from_secs(10))?;
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.0;
    let store = ContextStore::open(&db, Default::default())?;
    let mut samples = Vec::new();
    for sequence in 1..=20 {
        let expected = format!("Changed fixture revision {sequence}");
        let begin = Instant::now();
        fs::write(&target, &expected)?;
        loop {
            if store.get(&id)?.is_some_and(|r| r.content == expected) {
                break;
            }
            if begin.elapsed() > Duration::from_secs(3) {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                return Err("fixture update was not observed within three seconds".into());
            }
            thread::sleep(Duration::from_millis(2));
        }
        samples.push(begin.elapsed().as_secs_f64() * 1000.0);
    }
    stop.store(true, Ordering::Release);
    worker.join().map_err(|_| "observer thread failed")??;
    samples.sort_by(f64::total_cmp);
    let result = serde_json::json!({
        "fixture_files":100, "samples":samples.len(), "startup_to_ready_ms":startup_ms,
        "write_to_index_p50_ms":samples[9], "write_to_index_p95_ms":samples[18],
        "write_to_index_max_ms":samples[19], "poll_resolution_ms":2,
        "database_bytes_before_close":fs::metadata(&db)?.len(),
        "scope":"generated text files; no model, browser, speech, or end-to-end assistant latency"
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    drop(store);
    fs::remove_dir_all(&root)?;
    Ok(())
}
