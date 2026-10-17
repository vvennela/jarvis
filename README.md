# Jarvis

A persistent Mac companion that keeps track of your work, accepts natural follow-ups, and carries tasks forward in the background. Rust owns the local service, state, policy, and native UI. Structured browser and filesystem events provide awareness without screenshots or continuous model calls.

**Status:** foundation prototype, not a working desktop assistant yet. Policy/runner primitives and a headless review-flow fixture are implemented. Native UI, microphone, live collectors, SQLite persistence, browser control, and hosted providers are planned integrations. See the [today build plan](docs/build-plan.md) for scope and go/no-go gates; do not interpret the architecture diagram as shipped functionality.

## Product boundary

- Maintain current evidence and an attributed activity history locally.
- Resolve “this” against focus captured when the utterance begins.
- Route simple supported actions through Jev while retrieving evidence in parallel.
- Keep the conversational model available while workers run.
- Ask when information is missing; retry only when safe; pause conflicting writes.
- Prepare sensitive actions for human review. Jarvis never sends messages, purchases, submits bookings, or executes deletions in this version.

“Ready for review” is a verified intermediate result, not a claim that a purchase or send completed. Every worker proposal passes through the same policy gate.

## Development

Install Rust stable with Cargo, then run:

```sh
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Headless foundation tests require no credentials. Run the implemented scripted fixture with a fresh output directory:

```sh
cargo run -- demo --output /tmp/jarvis-review-demo
```

It creates a real local `message-review.txt`, explicitly marked as a scripted fixture and ready for review. It does not call a model, create a Gmail draft, resolve a real recipient address, or send a message. Choose a different fresh output directory for another run.

The policy probe demonstrates rejection of final sensitive actions:

```sh
cargo run -- policy send
cargo run -- policy pay
cargo run -- policy delete
```

Each intentionally exits with code 2. These rejection probes are not live adapters. The core also models clarification, revision invalidation, cancellation, bounded recovery decisions, and speculative transcript revision keys. Review verification checks content and Unix file identity; there is no durable SQLite job store yet.

Use **git only** for repository operations. The upstream is `https://github.com/vvennela/jarvis.git`. Pull before starting, commit each meaningful tested increment, and push each meaningful commit. Delegate adversarial integration/e2e tests to a separate agent while implementation proceeds in parallel.

## Required credentials for the live demo

Only two API keys are required by the planned hosted-model stack:

| Variable | Purpose | Obtain from |
|---|---|---|
| `TYPESAFE_API_KEY` | Jev intent/routing decisions | [TypeSafe console](https://console.typesafe.ai/) |
| `CRUSOE_API_KEY` | Conversational and worker inference | [Crusoe](https://crusoe.ai/) inference account |

Also configure `JARVIS_CONVERSATION_MODEL` and `JARVIS_WORKER_MODEL` with exact IDs available to that Crusoe account. These are configuration values, not keys. Select a supported Nemotron model from the authenticated model catalog; do not guess model identifiers.

Local `whisper-rs` transcription needs a compatible model file and microphone permission, **not an API key**. Native macOS speech synthesis also needs no key. Accessibility permission and browser-extension installation are required for their respective adapters. Existing website sessions remain in the browser. No OpenAI, Anthropic, ElevenLabs, Gmail, or Spotify API key is mandatory for this plan.

Copy `.env.example` to a local ignored `.env` when live adapters are implemented; never commit secrets or paste them into traces. Automatic `.env` loading is not yet promised. The example TOML documents planned defaults, not an implemented configuration parser.

## Design and validation

- [Architecture and invariants](docs/architecture.md)
- [Today: implementation order and scope](docs/build-plan.md)
- [Adversarial integration and e2e acceptance matrix](docs/testing.md)
- [Provider and local setup](docs/setup.md)

The differentiation to demonstrate is evidence gathered **before** a request and corrections propagated **during** a job. No claim of being faster or more capable than another product is valid without measurement.
