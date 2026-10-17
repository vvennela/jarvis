# Setup

The CLI and native panel support local file awareness, model conversation, local speech recognition, speech output, and verified local draft preparation. Offline indexing and ordinary tests need no provider credentials.

## API keys and model

Only two keys are required for the implemented live-provider path:

1. **`TYPESAFE_API_KEY`** from the [TypeSafe console](https://console.typesafe.ai/). Jev requests use bearer authentication at `https://api.typesafe.ai/v1/systemone`, model `jev-latest`. Jev returns structured routing choices; it is not a chat-completions substitute. [Official quickstart](https://docs.typesafe.ai/introduction/quickstart).
2. **`CRUSOE_API_KEY`** with serverless inference access. The client calls `https://api.inference.crusoecloud.com/v1/chat/completions`. The selected model is **Nemotron 3.5 Lightning**, `nvidia/Nemotron-3.5-Lightning-30B-A3B`. This exact case-sensitive ID was verified through the authenticated model catalog and a live response.

Optional configuration:

```dotenv
JARVIS_CONVERSATION_MODEL=nvidia/Nemotron-3.5-Lightning-30B-A3B
JARVIS_WORKER_MODEL=nvidia/Nemotron-3.5-Lightning-30B-A3B
JARVIS_WHISPER_MODEL_PATH=/absolute/path/to/ggml-base.en.bin
ELEVENLABS_API_KEY=
ELEVENLABS_VOICE_ID=
```

The conversation override is used by `ask` and the panel. The worker override is used by the local draft task. One Crusoe key serves both roles. Jev routing has a bounded deadline; conversation can continue through Crusoe if routing is unavailable.

**ElevenLabs is optional:** supply `ELEVENLABS_API_KEY` for natural speech output and optionally select `ELEVENLABS_VOICE_ID`. Without that key, the panel uses native macOS speech. Local Whisper needs the model file and macOS microphone permission, not a Wispr or transcription API key. No additional wake-word key is needed.

## Local configuration

Run from the repository directory. Copy `.env.example` to `.env` and edit the local file without sharing its contents. Provider commands and the panel automatically read `.env` in the current working directory. The loader recognizes the settings above; it accepts plain values and matching single/double quotes. It is a simple parser, not a shell: no variable expansion, command substitution, or `export` syntax. The file must be UTF-8 and at most 16 KiB. Environment variables take precedence; an empty environment variable removes a file-specified value.

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

## Native voice panel

Install the local streaming wake model with `bash scripts/setup-wake-model.sh` and add the printed `JARVIS_KWS_MODEL_DIR` and `JARVIS_VAD_MODEL_PATH` values to `.env`. The pinned sherpa-onnx English keyword model listens for Jarvis continuously; “Hey Jarvis” includes the same keyword. Local Silero speech detection separates speech from background noise and closes a turn after a 1.1-second speech-free grace period. Whisper transcribes commands after acoustic wake detection. The keyword archive and speech activity model are checksum-verified and stays in the ignored `models/` directory. No additional provider key is needed.

```sh
./scripts/launch-jarvis.command --root "$PWD/examples/demo-workspace" --db "$PWD/runtime/context.db"
```

Jarvis waits in the menu bar; a wake request or a click on its menu-bar item reveals the compact top-right glass panel. Dismissing the panel keeps wake listening and awareness running. Expand the panel for typed conversation, awareness status, pause, speech controls, and Quit. File awareness and conversation run on separate workers. The launcher builds a signed development app under Application Support and supplies the project directory so its `.env` loads through LaunchServices.

Configure the absolute Whisper model path before launching. The voice interaction uses “Jarvis” or “Hey Jarvis” and automatically speaks the response. macOS may request microphone permission on first use. Capture uses the default macOS input device unless `JARVIS_MICROPHONE_NAME` specifies an exact available input name. Jarvis follows changes to the system input within a second and clears audio from the previous device. Select AirPods under Sound → Input when macOS lists them as an input; an output-only connection does not select their microphone. A missing explicitly selected input reports an error instead of substituting another microphone. AirPods capture still needs a live hardware rehearsal. The idle card identifies the selected microphone; hover over the menu-bar icon or microphone control to inspect signal levels, decode count, wake matches, and decode duration. These diagnostics contain no recording or transcript.

The native wake path checks a dedicated streaming acoustic keyword model first, then uses a local Whisper backup on Silero-detected speech when needed. Backup transcripts are inspected transiently for the exact invocation and discarded when no wake word is present; they are never stored, logged, or sent to a provider. A legacy injected backend remains for portable tests. Synthetic speech measurements do not establish real-user first-attempt accuracy. Local fallback retains one second before speech activation so delayed speech detection preserves quiet wake-word beginnings. Command completion waits for 1.1 seconds without detected speech and checks audio arriving during inference. Repeated invocation words keep the command window open. Streaming acoustic keyword detection runs locally on every microphone chunk; false wakes and power usage still need measurement on real microphone input. Wake capture pauses during Jarvis's own response; voice barge-in remains a follow-up.

The launcher reuses a local development certificate under `~/Library/Application Support/Jarvis/Signing`. Its certificate-pinned app requirement remains stable when executable contents change. The private key is non-exportable in a dedicated keychain; the keychain password and certificate are stored in the same owner-only directory for local build automation. Preserve that directory across rebuilds. The signing helper temporarily registers the keychain for identity lookup and restores the search list afterward; it changes neither certificate trust nor macOS privacy settings. This is local development signing, not Developer ID distribution. Switching from the older ad-hoc identity can require one new permission grant. Permission retention across subsequent builds still requires live OS verification.

The panel's optional `--voice-diagnostics` argument emits numeric capture/recognition counters to stderr every two seconds for at most ten minutes. It excludes device names, audio, transcripts, configuration, and provider content. Increasing sample counts show capture is alive. Keyword spotting runs without transcription; a speech-gated local Whisper backup checks missed invocations. Separate xrun, input-route, and queue-overrun counters identify capture interruptions. Wake matches and accepted commands distinguish capture from downstream model failures.

Conversation failures show a specific, redacted reason in the card. Requests originating from voice also receive one spoken failure acknowledgment. Rate-limited conversations retain the question and retry at most twice, honoring the provider's `Retry-After` header or using increasing delays. Other transient inference errors receive at most one retry. All attempts share a 60-second deadline, and cancellation stops waiting and pending retries. A provider delay beyond the remaining deadline returns the rate-limit error without retrying early. Authentication, invalid requests, and malformed responses are not retried. Provider retries can repeat context reads. App and draft actions return an immediate observed result, with no later model call or automatic action retry.

The native panel supports “open Spotify,” “play music on Spotify,” and “pause Spotify.” These requests use fixed local app controls and verify the resulting running or playback state before replying. Exact commands do not require inference or a Spotify API key. Rephrased Spotify requests can use a typed Jev proposal with a 1.5 second deadline and 0.9 confidence threshold; every proposal still passes the allowlist and intent checks before execution. macOS may ask to let Jarvis control Spotify under Privacy & Security → Automation. Playback resumes Spotify's current selection. Named tracks and playlists require a separate selection flow; the current adapter asks for clarification. Tool failures are not automatically retried, and verified results are retained for conversational follow-ups. Nemotron can propose typed calls from the installed registry. Model-generated prose never executes as a command.

Automated native smoke tests leave the microphone and native OS collection off. Actual Whisper inference has been verified with generated WAV audio; synthetic wake tests verify command isolation and cancellation. The panel independently collects foreground app identity and permitted focused-window metadata. Missing Accessibility permission leaves app identity available and is shown in the panel. Known browser/password-manager titles are excluded from OS capture; approved browser pages use the extension. Browser action execution, persistent autonomous jobs, and personal speaker recognition remain separate milestones.

Git authentication remains the user's git transport. Do not invoke GitHub CLI or provision API credentials for repository work.

The native panel shares a typed tool registry across conversation and background work: `search_context`, `read_context`, `open_application`, `spotify_control`, and `delegate_draft`. Independent indexed reads run in parallel. Each turn allows at most four model steps and eight proposed calls; a side effect must be the only call in its batch. Read results retain source IDs, versions, and truncation labels under a 48 KiB message budget. All provider requests remain capped at 64 KiB. Unknown tools, arbitrary file paths, extra arguments, and duplicate call IDs are rejected.

Approved applications live in `~/Library/Application Support/Jarvis/app-preferences.json`. Defaults are Spotify, Notes, Safari, Calendar, and Calculator. Clear launch commands run directly without inference; execution reloads the allowlist and verifies the target is running. App controls may require macOS Automation permission. A writing proposal queues the original user request, creates a separate verified artifact, and leaves it ready for review while conversation remains available. Existing files are not edited by this worker. Browser actions and a general coding worker remain pending.
