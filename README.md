# Jarvis

Your work, in context. A native Rust companion for Mac that keeps a searchable memory of files and browser activity, ready when you ask.

- **Voice:** say “Jarvis” or “Hey Jarvis” to ask a question and hear the reply, with local wake recognition and a microphone mute control.
- **Continuous awareness:** watches selected folders, approved Chrome sites, and foreground app/window metadata.
- **Fast context:** source-balanced SQLite retrieval, incremental hashes, and parallel context gathering and routing.
- **Native presence:** a menu-bar companion with a compact glass panel when you call Jarvis.
- **Conversation:** Jev routing and NVIDIA Nemotron through Crusoe.
- **Tools:** open approved apps, select Spotify artists, research with You, read shared context, and delegate writing through verified tools.
- **Coding:** delegate a task to Codex in an isolated Git worktree, follow its progress, and open the verified code review.
- **Ready for review:** prepares drafts and revised file copies, verifies their sources, and restores review state after restart.

## Run

```sh
mkdir -p runtime
cargo run -p jarvis-panel -- --root /path/to/your/folder --db "$PWD/runtime/context.sqlite"
```

Jarvis indexes `.md` and `.txt` files. Add the [Chrome extension](extension/README.md) for browser context.

```sh
cargo run -- ask --db "$PWD/runtime/context.sqlite" --question "Where was I?"
```

## Configure

Copy `.env.example` to `.env` and set:

- `TYPESAFE_API_KEY` — [Jev](https://console.typesafe.ai/)
- `CRUSOE_API_KEY` — [Crusoe inference](https://crusoe.ai/)
- `JARVIS_WHISPER_MODEL_PATH` — absolute path to a local Whisper GGML model

For ElevenLabs speech, add `ELEVENLABS_API_KEY` and `ELEVENLABS_VOICE_ID`. Jarvis also supports your Mac’s built-in voice.

Set `JARVIS_CONVERSATION_MODEL` and `JARVIS_WORKER_MODEL` to `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B`. Add `YOU_API_KEY` for web research and Spotify artist discovery.

## Build and test

```sh
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
node --test extension/tests/*.test.js
```

[Coding](docs/coding.md) · [Architecture](docs/architecture.md) · [Build plan](docs/build-plan.md) · [Setup](docs/setup.md) · [Memory discipline](docs/memory.md)
