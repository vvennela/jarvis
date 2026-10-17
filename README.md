# Jarvis

Your work, in context. A native Rust companion for Mac that keeps a searchable memory of files and browser activity, ready when you ask.

- **Continuous awareness:** watches selected folders and approved Chrome sites.
- **Fast context:** SQLite search, incremental hashes, and parallel retrieval and routing.
- **Native presence:** a floating panel with live awareness status and pause/resume.
- **Conversation:** Jev routing and Nemotron 3.5 Lightning through Crusoe.
- **Ready for review:** prepares and verifies draft artifacts for your review.

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

Model: `nvidia/nemotron-3.5-lightning-30b-a3b`.

## Build and test

```sh
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
node --test extension/tests/*.test.js
```

[Architecture](docs/architecture.md) · [Build plan](docs/build-plan.md) · [Setup](docs/setup.md) · [Memory discipline](docs/memory.md)
