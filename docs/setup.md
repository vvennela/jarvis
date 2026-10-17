# Setup

The working CLI supports local awareness and read-only model conversation. Native voice, OS focus capture and autonomous workers are not yet implemented. Offline indexing and tests need no provider credentials.

## API keys and model

Only two keys are required for the implemented live-provider path:

1. **`TYPESAFE_API_KEY`** from the [TypeSafe console](https://console.typesafe.ai/). Jev requests use bearer authentication at `https://api.typesafe.ai/v1/systemone`, model `jev-latest`. Jev returns structured routing choices; it is not a chat-completions substitute. [Official quickstart](https://docs.typesafe.ai/introduction/quickstart).
2. **`CRUSOE_API_KEY`** with serverless inference access. The client calls `https://api.inference.crusoecloud.com/v1/chat/completions`. The default selected model is **Nemotron 3.5 Lightning**, `nvidia/nemotron-3.5-lightning-30b-a3b`, published in the [Crusoe catalog](https://docs.crusoecloud.com/serverless-inference/index.html). Account access still needs a live verification.

Optional configuration:

```dotenv
JARVIS_CONVERSATION_MODEL=nvidia/nemotron-3.5-lightning-30b-a3b
JARVIS_WORKER_MODEL=nvidia/nemotron-3.5-lightning-30b-a3b
```

The conversation override is used by `ask`. The worker value is reserved; no worker loop currently consumes it. One Crusoe key suffices for both planned roles if the account allows the models.

No OpenAI, Anthropic, ElevenLabs, Wispr, Gmail, Spotify, GitHub API token, or wake-word key is required. Local Whisper would need a model file and microphone permission, not another API key, but audio is not implemented yet.

## Local configuration

Run from the repository directory. Copy `.env.example` to `.env` and edit the local file without sharing its contents. `doctor`, `route` and `ask` automatically read `.env` in the current working directory. The loader recognizes the two key variables and two model variables above; it accepts plain values and matching single/double quotes. It is a simple parser, not a shell: no variable expansion, command substitution, or `export` syntax. The file must be UTF-8 and at most 16 KiB. Environment variables take precedence; an empty environment variable removes a file-specified value.

```sh
cargo run -- doctor
```

“Configured (not yet verified)” only means a value was supplied. A minimal `route` request verifies Jev connectivity; `ask` verifies the conversational path. Use non-private fixture notes for the first run. No actual production keys have been tested as part of the fixture test suite. Errors and diagnostics omit keys and provider response bodies.

`config.example.toml` still documents proposed settings; the CLI does not parse it. Bounds currently live in typed library defaults and constants. See [memory limits](memory.md).

## Local awareness commands

Choose a dedicated root for the first demo rather than your entire home directory:

```sh
mkdir -p runtime/demo-notes
cargo run -- index --db runtime/context.db --root runtime/demo-notes
cargo run -- watch --db runtime/context.db --root runtime/demo-notes --seconds 60
cargo run -- search --db runtime/context.db 'authentication'
cargo run -- recent --db runtime/context.db
cargo run -- ask --db runtime/context.db --question 'Where was I?'
```

`watch` accepts 1–3600 seconds and returns a summary when it exits. Native notifications trigger bounded scans; a final reconciliation handles pending events. Only text/Markdown files within the explicit root are eligible. Filesystem permission errors are reported; no OS accessibility or microphone permission is needed for these commands.

`ask` uses up to four resource excerpts, 4 KiB each, with IDs/versions/source/timestamps. If lexical search yields nothing, it selects recent surviving resources. This is a small evidence handoff, not an automatic reconstruction of every decision. It overlaps Jev and retrieval, then makes a non-streaming Nemotron call. There is no tool execution in this command.

## Browser Native Messaging host packaging

The host protocol, CLI and macOS installer exist; installation has not yet been performed. Load `extension/` unpacked in Chrome and record its 32-character extension ID. Build the binary, create the chosen database parent directory, and register it explicitly:

```sh
cargo build --release
mkdir -p runtime
target/release/jarvis install-browser-host --extension-id <installed-id> --db /Users/vishnuv/Documents/projects/jarvis/runtime/context.db
```

Replace `<installed-id>` with the ID shown by Chrome. The installer creates a new owner-only launcher and manifest without overwriting existing registrations. It records the executable used to run the installer. The launcher invokes:

```text
jarvis native-host --db <absolute-db-path> --extension-id <installed-id> <chrome-origin>
```

Register `com.jarvis.bridge` as a `stdio` host. Its manifest must point to the wrapper's absolute path and contain only the exact `chrome-extension://<installed-id>/` allowed origin. Default user-level Chrome location on macOS:

```text
~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.jarvis.bridge.json
```

Chrome supplies the caller origin; the wrapper must preserve it, not substitute a trusted value. Keep stdout exclusively for framed protocol messages. See [Chrome Native Messaging documentation](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging). The host needs no provider keys.

The launcher's executable path must remain stable: moving the repository or removing the release binary can break the registration. Browser collection requires explicit origin grants in extension settings. Private windows are excluded. A badge showing ON confirms a handshake, not successful ingestion of every event. Verify ingestion by opening an approved fixture page, then querying the shared SQLite database. Existing registrations intentionally cause an installation error rather than being replaced silently.

The current host writes directly to SQLite; the planned separate service/socket pairing architecture is not implemented. No host message can click, submit, send, pay, delete or run page scripts.

## Next native pieces

A top-right native panel is being integrated in `crates/jarvis-panel`. Until its live path is verified, the CLI and extension badge are the reliable implemented status surfaces. Microphone capture, push-to-talk, local transcription, speech output, accessibility focus capture and background worker execution remain planned. They need their own native permission and end-to-end validation, not additional mandatory hosted API keys.

Git authentication remains the user's git transport. Do not invoke GitHub CLI or provision API credentials for repository work.
