//! Native UI and Objective-C interop are isolated from the unsafe-free core.
#![deny(unsafe_op_in_unsafe_fn)]

use std::path::PathBuf;

#[derive(Debug)]
struct Options {
    root: PathBuf,
    database: PathBuf,
    smoke_test: bool,
}

fn options() -> Result<Options, &'static str> {
    let mut args = std::env::args_os().skip(1);
    let mut root = None;
    let mut database = None;
    let mut smoke_test = false;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--root") if root.is_none() => {
                root = Some(PathBuf::from(
                    args.next().ok_or("--root requires a directory")?,
                ))
            }
            Some("--db") if database.is_none() => {
                database = Some(PathBuf::from(
                    args.next().ok_or("--db requires a database path")?,
                ))
            }
            Some("--smoke-test") if !smoke_test => smoke_test = true,
            _ => {
                return Err(
                    "usage: jarvis-panel --root <directory> --db <database> [--smoke-test]",
                );
            }
        }
    }
    Ok(Options {
        root: root.ok_or("--root is required; no folders are collected by default")?,
        database: database.ok_or("--db is required")?,
        smoke_test,
    })
}

#[cfg(target_os = "macos")]
mod macos;

fn main() {
    let parsed = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(message) = macos::run(parsed) {
        eprintln!("{message}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (parsed.root, parsed.database, parsed.smoke_test);
        eprintln!("jarvis-panel requires macOS; use the jarvis CLI on this platform");
        std::process::exit(1);
    }
}
