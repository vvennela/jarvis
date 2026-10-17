# Jarvis panel

A native Rust panel that stays beside your work. Ask questions about your approved files, continue the conversation, and pause or resume continuous file awareness.

- Top-right AppKit panel with live awareness status.
- Typed questions, contextual replies, and follow-up conversation.
- **Jarvis → question → spoken reply** with local Whisper wake recognition.
- Independent conversation cancellation and file-awareness controls.
- Automatic spoken voice replies, plus **Read aloud** and **Stop speaking**.
- Event-driven indexing backed by SQLite.

## Run

Add `CRUSOE_API_KEY` to the project's `.env`. Set `TYPESAFE_API_KEY` to enable Jev routing.

```sh
cargo run -p jarvis-panel -- --root /absolute/path/to/approved-folder --db /absolute/path/to/context.sqlite
```

Set `JARVIS_WHISPER_MODEL_PATH` to your local GGML model. Launch Jarvis, allow microphone access, then say **“Jarvis, where was I?”**. Jarvis recognizes the wake phrase locally, hears your question, and speaks the reply. **Mute microphone** controls wake listening; **Pause** controls file awareness. You can also type a question and click **Ask**. Selected context goes to your configured model provider.

Set `ELEVENLABS_API_KEY` and `ELEVENLABS_VOICE_ID` for ElevenLabs speech. The Mac’s native voice is available with no speech API key. **Stop speaking** ends playback; **Quit** stops all workers.

## Verify

```sh
cargo test -p jarvis-panel
cargo run -p jarvis-panel -- --root /absolute/path/to/demo-fixtures --db /absolute/path/to/demo.sqlite --smoke-test
```

The native smoke check exercises observer readiness, pause, resume, and graceful shutdown on your Mac.
