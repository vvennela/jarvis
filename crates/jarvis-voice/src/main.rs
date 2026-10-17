#![forbid(unsafe_code)]
use std::{env, path::Path, process::ExitCode};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("jarvis-voice --model <local-ggml-model> --wav <mono-16k-pcm-wav>");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "--model" || args[2] != "--wav" {
        return Err("usage: jarvis-voice --model <path> --wav <path>".into());
    }
    // Reject an invalid recording before spending time/memory loading weights.
    let samples = jarvis_voice::load_wav(Path::new(&args[3]))?;
    let mut transcriber = jarvis_voice::Transcriber::load(Path::new(&args[1]))?;
    println!("{}", transcriber.transcribe(&samples)?);
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
