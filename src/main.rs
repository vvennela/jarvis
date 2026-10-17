use jarvis::{EvidenceStatus, GateDecision, Job, Operation, gate};
use std::{env, fs, path::PathBuf, process::ExitCode};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [command, flag, directory] if command == "demo" && flag == "--output" => {
            let output = PathBuf::from(directory);
            fs::create_dir_all(&output)?;
            let mut job = Job::new("demo-job", "demo-request", Operation::DraftMessage)?;
            let question = job.ask("recipient", "Who should receive the draft?")?;
            job.answer(&question, "Brian")?;
            if gate(Operation::DraftMessage, EvidenceStatus::Ready, 0, 1000) != GateDecision::Allow
            {
                return Err("preparation denied".into());
            }
            let draft = b"LOCAL REVIEW DRAFT - NOT SENT\n\nTo: Brian (recipient address unresolved)\n\nThis is a scripted Jarvis integration fixture, not a live agent result.\nReview and resolve the recipient before sending anything yourself.\n";
            job.prepare_review(job.revision(), &output.join("message-review.txt"), draft)?;
            let path = job.review_path()?;
            println!(
                "SCRIPTED FIXTURE\nREADY_FOR_REVIEW\nartifact={}\nI've prepared a local draft for your review. Nothing has been sent.",
                path.display()
            );
            Ok(())
        }
        [command, operation] if command == "policy" => {
            let operation = match operation.as_str() {
                "send" => Operation::SendMessage,
                "pay" => Operation::Pay,
                "delete" => Operation::Delete,
                _ => return Err("expected send, pay, or delete".into()),
            };
            Err(format!(
                "{:?}: final sensitive actions must be performed by the user",
                gate(operation, EvidenceStatus::Ready, 0, 1000)
            )
            .into())
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!(
                "Jarvis foundation (no live collectors or models yet)\n\njarvis demo --output <directory>\njarvis policy <send|pay|delete>"
            );
            Ok(())
        }
        _ => {
            Err("usage: jarvis demo --output <directory> | jarvis policy <send|pay|delete>".into())
        }
    }
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
    }
}
