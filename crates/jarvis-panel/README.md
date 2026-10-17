# Jarvis panel

A native Rust panel that stays beside your work. Ask questions about your approved files, continue the conversation, and pause or resume continuous file awareness.

- Top-right AppKit panel with live awareness status.
- Typed questions, contextual replies, and follow-up conversation.
- **Record → Finish → Ask** with local Whisper transcription.
- Independent conversation cancellation and file-awareness controls.
- On-demand native speech with **Read aloud** and **Stop speaking**.
- Event-driven indexing backed by SQLite.

## Run

Add `CRUSOE_API_KEY` to the project's `.env`. Set `TYPESAFE_API_KEY` to enable Jev routing.

```sh
cargo run -p jarvis-panel -- --root /absolute/path/to/approved-folder --db /absolute/path/to/context.sqlite
```

Set `JARVIS_WHISPER_MODEL_PATH` to your local GGML model. Click **Record**, speak, then **Finish recording**. Review the transcript and press **Ask**. You can also type directly into the question field. **Cancel** stops the current question or recording. **Pause** controls file awareness; **Quit** stops all workers. Selected context goes to your configured model provider.

Click **Read aloud** to hear a reply using your Mac’s selected system voice. **Stop speaking** ends playback. Native speech uses your Mac’s built-in synthesizer and needs no API key.

## Verify

```sh
cargo test -p jarvis-panel
cargo run -p jarvis-panel -- --root /absolute/path/to/demo-fixtures --db /absolute/path/to/demo.sqlite --smoke-test
```

The native smoke check exercises observer readiness, pause, resume, and graceful shutdown on your Mac.
