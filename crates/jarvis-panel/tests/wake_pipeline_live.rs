#![cfg(target_os = "macos")]
//! Opt-in end-to-end wake check on this Mac: macOS voices speak, the real
//! openWakeWord model listens, the production timing cut drops the wake
//! phrase, Apple SpeechAnalyzer transcribes, and the production request
//! parsers decide the action. Generated speech, not a human microphone.
//!
//! JARVIS_TEST_OWW_MODEL=<models/openwakeword> \
//! JARVIS_TEST_APPLE_STT=<target/release/jarvis-apple-stt> \
//! cargo test -p jarvis-panel --test wake_pipeline_live -- --ignored --nocapture
#[path = "../../../tests/support/mod.rs"]
mod support;
use jarvis::request_intent::{
    canonical_request, command_clauses, generic_music_request, resolved_action,
};
use jarvis_voice::{apple::AppleTranscriber, openwakeword::LocalWakeSpotter};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use support::TempDir;

const VOICES: &[&str] = &["Samantha", "Daniel", "Karen", "Moira", "Tessa", "Rishi"];
const RATES: &[u32] = &[175, 230];
const COMMANDS: &[&str] = &[
    "Hey Jarvis, open the Wikipedia page for Napoleon",
    "Hey Jarvis play Taylor Swift",
    "Hey Jarvis, play Metallica on Spotify",
    "Hey Jarvis open the Wikipedia page for Napoleon and play Taylor Swift on Spotify",
    "Hey Jarvis, pull up Napoleon's Wikipedia page",
    "Hey Jarvis, play some music",
    "Hey Jarvis, look up cartoonnetwork.com",
    "Hey Jarvis, switch to the docs tab",
];
/// Ordinary speech near a desk. None of it may wake Jarvis.
const NEGATIVES: &[&str] = &[
    "Let's play Taylor Swift on Spotify later tonight",
    "I was talking to Jarvis yesterday about the launch",
    "The harvest was great this year",
    "Hey, can you pass me the charger",
    "Open the Wikipedia page for Napoleon when you get a chance",
    "Marvis and Davis went home early",
    "Hey there, do you have a minute to talk",
    "Jars of jam are on the top shelf",
];

fn synthesize(dir: &Path, voice: &str, rate: u32, text: &str, index: usize) -> Option<Vec<f32>> {
    let aiff = dir.join(format!("{index}.aiff"));
    let wav = dir.join(format!("{index}.wav"));
    let spoke = Command::new("/usr/bin/say")
        .args(["-v", voice, "-r", &rate.to_string(), "-o"])
        .arg(&aiff)
        .arg(text)
        .status()
        .ok()?
        .success();
    let converted = spoke
        && Command::new("/usr/bin/afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&aiff)
            .arg(&wav)
            .status()
            .ok()?
            .success();
    if !converted {
        return None;
    }
    let mut samples = vec![0.0; 16_000];
    samples.extend(jarvis_voice::load_wav(&wav).ok()?);
    samples.extend(vec![0.0; 24_000]);
    Some(samples)
}

/// Feed 20 ms chunks like the capture loop. On detection, keep the same
/// lookback the worker keeps and transcribe with the production cut.
fn run(spotter: &mut LocalWakeSpotter, stt: &AppleTranscriber, samples: &[f32]) -> Option<String> {
    spotter.reset();
    let mut heard = Vec::new();
    for (index, chunk) in samples.chunks(320).enumerate() {
        heard.extend_from_slice(chunk);
        if let Some(detection) = spotter.accept(chunk).unwrap() {
            let begin = heard
                .len()
                .saturating_sub(detection.lookback_samples.min(64_000));
            let armed = (heard.len() - begin).min(64_000);
            let mut command = heard[begin..].to_vec();
            command.extend_from_slice(&samples[(index + 1) * 320..]);
            return Some(
                stt.transcribe_after(
                    &command,
                    Some(armed.saturating_sub(4_800)),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap_or_default(),
            );
        }
    }
    None
}

fn understood(text: &str) -> bool {
    let canonical = canonical_request(text);
    let text = canonical.as_deref().unwrap_or(text);
    generic_music_request(text)
        || command_clauses(text).is_ok_and(|clauses| clauses.iter().all(|c| resolved_action(c)))
}

#[test]
#[ignore = "needs local openWakeWord models, the Apple speech helper and macOS voices"]
fn generated_voices_wake_and_are_understood_without_false_wakes() {
    let model = PathBuf::from(std::env::var_os("JARVIS_TEST_OWW_MODEL").expect("model dir"));
    let helper = PathBuf::from(std::env::var_os("JARVIS_TEST_APPLE_STT").expect("stt helper"));
    let mut spotter = LocalWakeSpotter::load(&model).unwrap();
    let stt = AppleTranscriber::new(helper, Duration::from_secs(20)).unwrap();
    let dir = TempDir::new();
    let (mut total, mut woke, mut ok) = (0, 0, 0);
    let mut misses = Vec::new();
    let mut index = 0;
    for voice in VOICES {
        for rate in RATES {
            for command in COMMANDS {
                index += 1;
                let Some(samples) = synthesize(dir.path(), voice, *rate, command, index) else {
                    continue;
                };
                total += 1;
                match run(&mut spotter, &stt, &samples) {
                    Some(text) if understood(&text) => {
                        woke += 1;
                        ok += 1;
                    }
                    Some(text) => {
                        woke += 1;
                        misses.push(format!("{voice}@{rate} {command:?} -> heard {text:?}"));
                    }
                    None => misses.push(format!("{voice}@{rate} {command:?} -> no wake")),
                }
            }
        }
    }
    let mut false_wakes = Vec::new();
    for voice in VOICES {
        for sentence in NEGATIVES {
            index += 1;
            let Some(samples) = synthesize(dir.path(), voice, 185, sentence, index) else {
                continue;
            };
            if let Some(text) = run(&mut spotter, &stt, &samples) {
                false_wakes.push(format!("{voice} {sentence:?} -> {text:?}"));
            }
        }
    }
    println!("wake: {woke}/{total}  understood: {ok}/{woke}");
    for miss in &misses {
        println!("  miss: {miss}");
    }
    for wake in &false_wakes {
        println!("  FALSE WAKE: {wake}");
    }
    assert!(total >= 60, "too few clips synthesized: {total}");
    assert!(woke * 100 >= total * 85, "wake rate {woke}/{total}");
    assert!(ok * 100 >= woke * 95, "understood {ok}/{woke}");
    assert!(false_wakes.is_empty(), "{} false wakes", false_wakes.len());
}
