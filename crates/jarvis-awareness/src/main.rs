use jarvis::context::{ContextStore, Limits, ObservationResult};
use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn run() -> Result<(), &'static str> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!("jarvis-awareness --db <path> --seconds <1..3600>");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--db" || args[2] != "--seconds" {
        return Err("usage: jarvis-awareness --db <path> --seconds <1..3600>");
    }
    let seconds = args[3]
        .to_str()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| (1..=3600).contains(n))
        .ok_or("seconds must be 1..3600")?;
    let mut store = ContextStore::open(&PathBuf::from(&args[1]), Limits::default())
        .map_err(|_| "context database unavailable")?;
    let until = Instant::now() + Duration::from_secs(seconds);
    let mut changed = 0;
    let mut polls = 0;
    let mut permission_missing = false;
    while Instant::now() < until {
        let observation = jarvis_awareness::capture()
            .map_err(|_| "native foreground awareness requires macOS")?;
        permission_missing |= observation.status == jarvis_awareness::Status::PermissionMissing;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock unavailable")?
            .as_millis()
            .min(i64::MAX as u128) as i64;
        if matches!(
            jarvis_awareness::publish(&mut store, observation, timestamp)
                .map_err(|_| "foreground context update failed")?,
            ObservationResult::Changed { .. }
        ) {
            changed += 1;
        }
        polls += 1;
        std::thread::sleep(
            jarvis_awareness::POLL_INTERVAL.min(until.saturating_duration_since(Instant::now())),
        );
    }
    println!(
        "foreground awareness: polls={polls}, changes={changed}, accessibility_permission_missing={permission_missing}"
    );
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}
