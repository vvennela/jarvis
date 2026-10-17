# Demonstrating Jarvis

Use the generated notes in `examples/demo-workspace` for a repeatable demonstration. The primary experience is always-on “Jarvis,” a visible top-right panel, and spoken replies. The current wake increment needs a real microphone rehearsal; deterministic audio and worker tests do not substitute for that check.

## Start the companion

Run from the repository root with `CRUSOE_API_KEY` configured in `.env` or the environment. Jev uses `TYPESAFE_API_KEY` when available. Set the absolute path to the local Whisper weights:

```sh
export JARVIS_WHISPER_MODEL_PATH="$PWD/models/ggml-base.en.bin"
mkdir -p runtime
cargo run -p jarvis-panel -- --root "$PWD/examples/demo-workspace" --db "$PWD/runtime/demo.sqlite"
```

Use a real downloaded Whisper model at that path. Normal panel startup enables wake listening; grant macOS microphone access when requested. The panel shows loading, wake listening, command listening, decoding, thinking and speaking states. **Mute microphone** stops listening; **Enable wake listening** resumes it. `--smoke-test` exercises the panel without enabling microphone capture.

Native speech is used without an ElevenLabs key. For optional ElevenLabs speech, configure `ELEVENLABS_API_KEY` and optionally `ELEVENLABS_VOICE_ID`; synthesis uses `eleven_flash_v2_5`. The current adapter downloads a bounded MP3 before playing it. Audio playback can be interrupted with **Stop speaking**.

## Show awareness and conversation

1. Leave the generated project note in the watched folder before asking anything.
2. Say “Jarvis, where was I?” The answer should identify Project Beacon and the unfinished handoff from the indexed note.
3. After the answer, say “Jarvis, what should I do next?” to exercise conversational history. If Jarvis asks a clarification, use the wake phrase again before your answer.
4. Edit the note's preferred review time, wait for awareness to ingest the change, then ask what the current review time is.
5. Pause awareness, edit the note, and resume. Ask again to demonstrate reconciliation.
6. Mute and resume the microphone to show that the visible state corresponds to actual capture control.

Wake recognition accepts standalone “Jarvis” (or “Hey Jarvis”), either with the request in one utterance or followed by the request. After the wake word alone, there is a ten-second silent wait for the command; commands are capped at 15 seconds. Listening preferences do not persist across launches. It uses bounded local Whisper windows and speech-energy endpointing. Capture pauses while conversation or playback is busy to avoid self-triggering; spoken interruption during the answer is not part of this increment. Use the explicit speech-stop control. Speaker identity is not enforced yet.

The panel also accepts typed questions and offers **Read aloud**. The same context is available through the CLI:

```sh
cargo run -- index --db runtime/demo.sqlite --root examples/demo-workspace
cargo run -- search --db runtime/demo.sqlite Beacon
cargo run -- ask --db runtime/demo.sqlite --question "Where was I?"
```

## Prepare a real local draft

This step currently uses the CLI, separately from the panel conversation:

```sh
cargo run -- draft --db runtime/demo.sqlite \
  --request "Draft a short update to Brian about Project Beacon using these notes." \
  --output "$PWD/runtime/brian-review.txt" \
  --source "file:$PWD/examples/demo-workspace/project.md"
```

Use a fresh output filename on each run. Resource IDs use canonical paths; copy the exact ID printed by `search` if `$PWD` capitalization differs from the physical path on macOS.

A live Nemotron run using the generated Beacon notes produced the verified artifact `runtime/brian-live-review-1834.txt`. This is a local demonstration artifact, not a message sent to a real recipient.

Jarvis generates a local draft using the selected context, pins its versions, rechecks them after generation, and verifies the saved file before announcing `READY_FOR_REVIEW`. Open that exact path to inspect the result. It has not sent a message or resolved Brian's email address. Ctrl-C cancels a running generation.

For a failure demonstration, change the indexed source while generation runs. If the watcher ingests the change before the final source check, the task should block instead of producing an apparently current artifact. Integration tests exercise this timing deterministically. Later source changes and artifact tampering revoke readiness when the task's review method is called; current CLI job state does not survive process exit.

## What the current demo proves

The honest story is awareness → spoken question → contextual reply → clarification → verified local draft. It demonstrates maintained context and a responsive native companion. “Implement that,” autonomous coding/test repair, Gmail draft placement, browser bookings, durable background jobs and voice-triggered tool execution are subsequent milestones. Do not stage those as live behavior with scripted fixture output.

The read-only browser bridge can add approved page context after extension installation and host registration. Verify ingestion into the same database before including browser evidence in a demo. The native panel also collects foreground app identity and permitted focused-window metadata into that database. Its status distinguishes missing Accessibility permission from collection errors. It currently stores the current foreground observation; full window inventory and utterance-frozen anchors remain separate work.

## Deterministic local speech check

```sh
cargo run -p jarvis-voice -- --model models/ggml-base.en.bin --wav /path/to/16khz-mono.wav
```

The WAV path accepts bounded 16 kHz mono, 16-bit PCM audio. The reusable transcriber retains its model across calls; the CLI is a one-shot check. Run integration tests and a real microphone rehearsal before recording latency claims. Avoid claiming speaker recognition, production wake reliability or full-duplex barge-in from a successful fixture transcription.
