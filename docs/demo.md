# Demonstrating Jarvis

Run commands from the project root. The included `examples/demo-workspace` contains generated project notes for a reproducible session.

## Awareness and conversation

```sh
mkdir -p runtime
cargo run -p jarvis-panel -- --root "$PWD/examples/demo-workspace" --db "$PWD/runtime/demo.sqlite"
```

Ask “Where was I?” in the panel. Follow with “What should I do next?” to exercise conversation history. Edit the project note, then ask about the updated review time. Pause awareness, edit again, and resume to demonstrate reconciliation. **Read aloud** speaks the reply; **Stop speaking** interrupts playback.

The same context is available from the command line:

```sh
cargo run -- index --db runtime/demo.sqlite --root examples/demo-workspace
cargo run -- search --db runtime/demo.sqlite Beacon
cargo run -- ask --db runtime/demo.sqlite --question "Where was I?"
```

## Prepare a real local draft

```sh
cargo run -- draft --db runtime/demo.sqlite \
  --request "Draft a short update to Brian about Project Beacon using these notes." \
  --output "$PWD/runtime/brian-review.txt" \
  --source "file:$PWD/examples/demo-workspace/project.md"
```

The model generates a local draft. Jarvis pins the selected context versions, rechecks them after generation, and verifies the saved artifact before announcing `READY_FOR_REVIEW`. Use a fresh output filename on each run. Ctrl-C cancels a running generation.

File resource IDs use canonical paths. On case-insensitive macOS, use the exact ID printed by `search` if `$PWD` differs in capitalization from the physical path.

This worker prepares text rather than a Gmail draft; review state currently lives in the process. Indexed-source changes invalidate readiness when checked, while changes not yet observed by collectors remain a freshness limitation. Durable job recovery, browser draft adapters, and live dependency notifications remain the next worker milestones.

## Local transcription

```sh
cargo run -p jarvis-voice -- --model models/ggml-base.en.bin --wav /path/to/16khz-mono.wav
```

The transcription crate keeps its model resident for reuse. The CLI accepts bounded 16 kHz mono, 16-bit PCM WAV input. The panel's microphone adapter uses the same transcription engine.
