# Jarvis

A Rust-first Mac companion that maintains work context before you ask for help. Structured file and browser observations feed a local evidence store; Jev routes requests while context is retrieved in parallel, and Nemotron answers from selected evidence.

**Current prototype:** real bounded file indexing/watching, SQLite + FTS5 search and version history, Jev and Crusoe clients, a read-only Chrome extension with a Rust Native Messaging host, and a scripted ready-for-review fixture. The top-right native panel is being integrated separately. Provider credentials have not been exercised live, and the Chrome extension is not yet installed in the user's browser.

There is no voice capture, native OS focus/selection capture, autonomous task worker, or general browser-action execution yet. `ask` is a single read-only conversation turn; the clarification/cancellation runner primitives are not yet a connected conversational job loop. The [architecture](docs/architecture.md) describes the target system, not a list of shipped features.

## Try the working local layer

Run from the repository at `/Users/vishnuv/Documents/projects/jarvis`:

```sh
cargo build
mkdir -p runtime/demo-notes
printf 'Implement the authentication settings page. Pending: choose session duration.\n' > runtime/demo-notes/spec.md
cargo run -- index --db runtime/context.db --root runtime/demo-notes
cargo run -- search --db runtime/context.db authentication
cargo run -- recent --db runtime/context.db
cargo run -- watch --db runtime/context.db --root runtime/demo-notes --seconds 60
```

Edit the note while `watch` runs, then search again. File events trigger bounded reconciliation; metadata fingerprints avoid rereading unchanged files and content hashes avoid duplicate versions. Only `.txt` and `.md` files under the explicitly supplied root are collected. Hidden paths, symlinks, oversized files and invalid UTF-8 are excluded. Truncated scans are reported rather than presented as complete coverage. No model or API key is needed for this flow.

## Add Jev and Nemotron

Copy `.env.example` to the ignored `.env` and fill in exactly these keys:

| Variable | Purpose | Obtain from |
|---|---|---|
| `TYPESAFE_API_KEY` | Jev routing | [TypeSafe console](https://console.typesafe.ai/) |
| `CRUSOE_API_KEY` | Nemotron conversation | [Crusoe](https://crusoe.ai/) inference account |

The selected model defaults to **Nemotron 3.5 Lightning**, `nvidia/nemotron-3.5-lightning-30b-a3b`, listed in the [Crusoe catalog](https://docs.crusoecloud.com/serverless-inference/index.html). `JARVIS_CONVERSATION_MODEL` can override it. `JARVIS_WORKER_MODEL` is reserved for the planned worker integration.

```sh
cargo run -- doctor
cargo run -- route 'Help me resume the authentication work'
cargo run -- ask --db runtime/context.db --question 'Where was I?'
```

`doctor` checks configuration presence, not provider authentication. `route` performs Jev inference. `ask` overlaps Jev routing and database retrieval, then calls Crusoe with bounded excerpts. If routing fails, it falls back to a conversational route; it never executes a tool. These clients currently return complete responses rather than streaming tokens. Selected evidence is sent to the configured cloud provider; collection itself remains local.

`.env` loads automatically from the current working directory. Environment variables override it; an explicitly empty environment variable clears the corresponding file value. Do not commit or print secrets. See [setup](docs/setup.md) for exact configuration and browser installation requirements.

## Sensitive actions stop at review

```sh
cargo run -- demo --output runtime/review-demo
cargo run -- policy send
cargo run -- policy pay
cargo run -- policy delete
```

The demo creates a real local `message-review.txt` in a fresh output directory. It is explicitly marked **SCRIPTED FIXTURE**, resolves no real recipient address, creates no Gmail draft, and sends nothing. The runner verifies the exact file content, identity, and job revision before claiming `READY_FOR_REVIEW`. The policy commands intentionally exit with code 2.

The product boundary is preparation for human review. There are no final send, pay, booking submission or deletion tools. “Ready for review” does not mean a sensitive operation completed. Runner primitives also support clarification, revision invalidation, cancellation and bounded recovery decisions; durable job persistence and live task execution remain future work.

## Browser awareness

The [Chrome extension](extension/README.md) collects approved HTTP(S) tab metadata and rendered text, with no screenshots or action API. The Rust host validates the handshake, session, sequence and payload bounds, then atomically stores separate page and tab records. Closing a tab removes its live metadata while retaining observed page evidence.

Transport and simulated-browser tests pass, but loading the extension and registering its Native Messaging host are separate installation steps. The macOS `install-browser-host --extension-id <id> --db <absolute-path>` command creates a new registration; it has not been run against the user's browser. The badge indicates connection state, not microphone activity or a guarantee of durable delivery. See [setup](docs/setup.md#browser-native-host-packaging).

## Development and validation

```sh
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

The default workspace member is the headless core. Native panel validation is tracked in its own crate while integration proceeds. Tests prioritize real temporary SQLite databases, filesystem events, subprocess CLI runs, and local HTTP/native-message peers. Those tests do not prove live provider authentication or installed Chrome behavior.

Use **git only** for repository operations. Pull before starting, delegate adversarial integration/e2e tests for meaningful changes, then commit and push verified increments to `https://github.com/vvennela/jarvis.git`.

- [Today: build order and demo scope](docs/build-plan.md)
- [Architecture and invariants](docs/architecture.md)
- [Integration/e2e acceptance matrix](docs/testing.md)
- [Setup and credentials](docs/setup.md)
- [Implemented memory and resource limits](docs/memory.md)
