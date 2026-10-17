use jarvis::conversation::{
    Conversation, load_provider_settings as provider_settings, required_key,
};
use jarvis::{EvidenceStatus, GateDecision, Job, Operation, gate};
use std::{env, fs, path::PathBuf, process::ExitCode};
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [
            command,
            db_flag,
            db,
            request_flag,
            instruction,
            output_flag,
            output,
            sources @ ..,
        ] if command == "draft"
            && db_flag == "--db"
            && request_flag == "--request"
            && output_flag == "--output" =>
        {
            use std::sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            };
            if sources.len() % 2 != 0
                || sources.len() > 8
                || sources.chunks(2).any(|pair| pair[0] != "--source")
            {
                return Err("provide up to four --source <resource-id> pairs".into());
            }
            let resources: Vec<_> = sources.chunks(2).map(|pair| pair[1].clone()).collect();
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| "job ID generation failed")?;
            let id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut task = jarvis::draft::DraftTask::from_env(
                PathBuf::from(db),
                format!("draft-{id}"),
                format!("request-{id}"),
            )?;
            let cancel = Arc::new(AtomicBool::new(false));
            let generation = task.generate(
                instruction,
                &resources,
                std::path::Path::new(output),
                Arc::clone(&cancel),
            );
            tokio::pin!(generation);
            let outcome = tokio::select! {
                result = &mut generation => result?,
                _ = tokio::signal::ctrl_c() => {
                    cancel.store(true, Ordering::Release);
                    generation.await?
                }
            };
            match outcome {
                jarvis::draft::DraftOutcome::ReadyForReview { path, .. } => {
                    println!(
                        "READY_FOR_REVIEW\nartifact={}\nI've prepared a local draft for your review.",
                        path.display()
                    );
                    Ok(())
                }
                jarvis::draft::DraftOutcome::Blocked { reason, .. } => {
                    Err(format!("Draft paused: {reason:?}").into())
                }
                jarvis::draft::DraftOutcome::Cancelled { .. } => {
                    println!("Draft cancelled.");
                    Ok(())
                }
            }
        }
        [command, id_flag, id, db_flag, db]
            if command == "install-browser-host"
                && id_flag == "--extension-id"
                && db_flag == "--db" =>
        {
            if !cfg!(target_os = "macos") {
                return Err(
                    "automatic browser host installation currently supports macOS only".into(),
                );
            }
            let home = PathBuf::from(env::var("HOME").map_err(|_| "HOME is unavailable")?);
            let support = home.join("Library/Application Support");
            let result = jarvis::install::install_host(
                &env::current_exe()?,
                std::path::Path::new(db),
                id,
                &support.join("Jarvis"),
                &support.join("Google/Chrome/NativeMessagingHosts"),
            )?;
            println!(
                "Registered observation-only host: {}\nReload the extension to connect.",
                result.manifest.display()
            );
            Ok(())
        }
        [command, db_flag, db, root_flag, root]
            if command == "index" && db_flag == "--db" && root_flag == "--root" =>
        {
            let mut store =
                jarvis::context::ContextStore::open(std::path::Path::new(db), Default::default())?;
            let report = jarvis::awareness::index_root(&mut store, std::path::Path::new(root))?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        [command, db_flag, db, root_flag, root, time_flag, seconds]
            if command == "watch"
                && db_flag == "--db"
                && root_flag == "--root"
                && time_flag == "--seconds" =>
        {
            let seconds: u64 = seconds.parse().map_err(|_| "invalid watch duration")?;
            if !(1..=3600).contains(&seconds) {
                return Err("watch duration must be 1..3600 seconds".into());
            }
            let mut store =
                jarvis::context::ContextStore::open(std::path::Path::new(db), Default::default())?;
            let report = jarvis::awareness::watch_root(
                &mut store,
                std::path::Path::new(root),
                std::time::Duration::from_secs(seconds),
            )?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        [command, db_flag, db, query] if command == "search" && db_flag == "--db" => {
            let store =
                jarvis::context::ContextStore::open(std::path::Path::new(db), Default::default())?;
            println!("{}", serde_json::to_string(&store.search(query, 8)?)?);
            Ok(())
        }
        [command, db_flag, db] if command == "recent" && db_flag == "--db" => {
            let store =
                jarvis::context::ContextStore::open(std::path::Path::new(db), Default::default())?;
            println!("{}", serde_json::to_string(&store.recent(16)?)?);
            Ok(())
        }
        [command, db_flag, db, id_flag, id, origin]
            if command == "native-host" && db_flag == "--db" && id_flag == "--extension-id" =>
        {
            jarvis::bridge::validate_extension_origin(id, origin)?;
            let mut store =
                jarvis::context::ContextStore::open(std::path::Path::new(db), Default::default())?;
            jarvis::bridge::run_live_session(
                std::io::stdin(),
                std::io::stdout().lock(),
                &mut store,
                &jarvis::browser_tools::socket_path()?,
            )?;
            Ok(())
        }
        [command] if command == "browser-status" => {
            println!(
                "{}",
                serde_json::to_string(&jarvis::browser_tools::setup_status().await)?
            );
            Ok(())
        }
        [command] if command == "doctor" => {
            let config = provider_settings()?;
            let ready = ["TYPESAFE_API_KEY", "CRUSOE_API_KEY"]
                .into_iter()
                .all(|key| config.contains_key(key));
            for key in [
                "TYPESAFE_API_KEY",
                "CRUSOE_API_KEY",
                "YOU_API_KEY",
                "ELEVENLABS_API_KEY",
            ] {
                println!(
                    "{key}: {}",
                    if config.contains_key(key) {
                        "configured (not yet verified)"
                    } else {
                        "missing"
                    }
                );
            }
            let model = config
                .get("JARVIS_CONVERSATION_MODEL")
                .map(String::as_str)
                .unwrap_or(jarvis::providers::NEMOTRON_MODEL);
            if model.len() <= 128
                && model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
            {
                println!("model: {model}");
            } else {
                println!("model: invalid configuration");
            }
            if ready {
                Ok(())
            } else {
                Err("live provider keys are missing; offline tests still work".into())
            }
        }
        [command, question] if command == "route" => {
            let config = provider_settings()?;
            let client = jarvis::providers::JevClient::production(required_key(
                &config,
                "TYPESAFE_API_KEY",
            )?)?;
            let decision = client.route(question).await?;
            println!(
                "route={:?} confidence={}",
                decision.route, decision.confidence
            );
            Ok(())
        }
        [command, db_flag, db, question_flag, question]
            if command == "ask" && db_flag == "--db" && question_flag == "--question" =>
        {
            let mut conversation = Conversation::from_env(PathBuf::from(db))?;
            let reply = conversation.ask(question).await?;
            println!("{}", reply.text);
            Ok(())
        }
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
                "Jarvis context and review CLI\n\njarvis index --db <path> --root <directory>\njarvis watch --db <path> --root <directory> --seconds <1..3600>\njarvis search --db <path> <query>\njarvis recent --db <path>\njarvis doctor\njarvis browser-status\njarvis route <question>\njarvis ask --db <path> --question <text>\njarvis draft --db <path> --request <text> --output <file> [--source <resource-id>]\njarvis install-browser-host --extension-id <id> --db <absolute-path>\njarvis demo --output <directory> (scripted review fixture)\njarvis policy <send|pay|delete>"
            );
            Ok(())
        }
        _ => Err("Run jarvis --help for commands and usage.".into()),
    }
}
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
    }
}
