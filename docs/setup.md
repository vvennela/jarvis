# Setup

The CLI and native panel support local file awareness, model conversation, local speech recognition, speech output, and verified local draft preparation. Offline indexing and ordinary tests need no provider credentials.

## API keys and model

The conversational provider configuration uses:

1. **`TYPESAFE_API_KEY`** from the [TypeSafe console](https://console.typesafe.ai/). Jev requests use bearer authentication at `https://api.typesafe.ai/v1/systemone`, model `jev-latest`. Jev returns structured routing choices; it is not a chat-completions substitute. [Official quickstart](https://docs.typesafe.ai/introduction/quickstart).
2. **`CRUSOE_API_KEY`** with serverless inference access. The client calls `https://api.inference.crusoecloud.com/v1/chat/completions`. The current rehearsal model is **Nemotron 3 Nano**, `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B`. This exact case-sensitive ID and its hosted tool protocol were verified live.

Optional configuration:

```dotenv
JARVIS_CONVERSATION_MODEL=nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B
JARVIS_WORKER_MODEL=nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B
JARVIS_OPENWAKEWORD_MODEL_DIR=/absolute/path/to/jarvis/models/openwakeword
JARVIS_VAD_MODEL_PATH=/absolute/path/to/jarvis/models/silero_vad.onnx
YOU_API_KEY=
ELEVENLABS_API_KEY=
ELEVENLABS_VOICE_ID=
```

The conversation override is used by `ask` and the panel. The worker override is used by the local draft task. One Crusoe key serves both roles. Jev routing has a bounded deadline; conversation can continue through Crusoe if routing is unavailable.

**You.com powers public research and named-artist lookup:** `search_web` sends only the original user request, returns up to five bounded public results, and marks snippets as untrusted evidence. It sends no indexed files, browser text, or model-written query and does not fetch result pages. For music, set `YOU_API_KEY` to enable `spotify_play_artist` in the native panel. Nemotron extracts the artist as a typed argument; the runner checks the original request, searches You, verifies candidate identities through Spotify oEmbed in parallel, resolves duplicate aliases through the structured identity in Spotify’s public embed data, then plays the resolved artist URI and reads back the playing artist and track identity. It confirms only a match. Embed JSON is parsed as data; no page script is executed. Distinct canonical artists remain ambiguous. The search key is sent only to You. Spotify developer credentials are not needed for this desktop path.

**ElevenLabs is optional:** supply `ELEVENLABS_API_KEY` for natural speech output and optionally select `ELEVENLABS_VOICE_ID`. Without that key, the panel uses native macOS speech. The native panel uses local openWakeWord detection and Apple SpeechAnalyzer transcription on macOS 26. Neither requires a provider key. Apple manages the transcription model assets; microphone capture uses macOS permission.

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

`ask` uses the shared broker: up to eight bounded, source-tagged excerpts, with foreground, file, and browser coverage. Current-work notes are prioritized for next-step questions. It overlaps Jev and retrieval, then calls Nemotron. The standalone command answers from context; the native panel additionally installs the typed tool runtime.

## Browser Native Messaging host packaging

The extension, authenticated native host, private socket, and fresh browser tools are implemented and tested. Live Arc installation remains pending while the browser's user-activity guard is active. Load `extension/` through `chrome://extensions` in Chrome or Arc using **Developer mode → Load unpacked**. Copy the 32-letter extension ID from its settings.

Register against the same database as the panel:

```sh
cargo build -p jarvis --bin jarvis
mkdir -p runtime
target/debug/jarvis install-browser-host --extension-id <installed-id> --db "$PWD/runtime/live.sqlite"
target/debug/jarvis browser-status
```

The installer creates an owner-only launcher and exact-origin manifest without overwriting an existing registration. Chrome and Arc use this native-host location on macOS:

```text
~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.jarvis.bridge.json
```

The launcher preserves the browser-supplied caller origin. The host validates it before opening the database. Its executable path must stay available across builds. Reload the extension, approve a website in its settings, and click **Reconnect and refresh approved tabs**. The settings show the exact extension ID and sanitized connection diagnostics.

`browser-status` reports registration and authenticated socket connectivity without querying tabs. An **ON** badge establishes the handshake; verify actual ingestion separately using an approved page. Private windows are excluded, and each site requires an explicit grant. Page text, URLs and titles remain untrusted evidence.

The host writes observations to SQLite and serves fresh reads over a same-user Unix socket under `~/Library/Application Support/Jarvis/BrowserBridge/`. `browser_search_tabs` discovers approved tab/document handles; `browser_read_tab` checks the same document and permission before and after extraction. The five-second request deadline, bounded frames and expiring read lease handle disconnects and stalled pages. There is no click, submit, send, pay, delete, or arbitrary page-script endpoint.

For the synthetic page-change rehearsal:

```sh
python3 examples/browser-demo/serve.py
```

Approve `http://localhost` without a port in extension settings, then open `http://localhost:8765`. The host grant covers localhost ports. Ask about its project priority, publish the edited priority and ask again. See [browser demo](../examples/browser-demo/README.md) and [native messaging documentation](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging).

## Native voice panel

The live panel uses **openWakeWord → Silero endpointing → Apple SpeechAnalyzer** on Apple Silicon with macOS 26 and the current Xcode SDK. Install its pinned local models and copy the two printed settings into `.env`:

```sh
bash scripts/setup-openwakeword.sh
mkdir -p target/release
xcrun swiftc -parse-as-library -O -target "$(uname -m)-apple-macos26.0" native/AppleSpeech.swift -o target/release/jarvis-apple-stt
target/release/jarvis-apple-stt --prepare
```

The setup script verifies the openWakeWord v0.5.1 ONNX model hashes and the existing Silero VAD hash. Files remain in ignored `models/`. openWakeWord shares Sherpa's statically linked ONNX Runtime 1.28.2 through the compatible v22 API; no separate runtime download or Python process is needed. Loading a second dynamic runtime caused a native teardown fault during validation, so the live path uses only the shared runtime. `--prepare` installs Apple's shared English speech assets once. The launcher packages the helper beside the panel, so `JARVIS_APPLE_STT_PATH` is only needed to override that executable location. No Whisper weights are required for the live panel.

Use **“Hey Jarvis”** with the supplied upstream `hey_jarvis` model. Standalone “Jarvis” is voice-dependent: in a ten-clip synthetic reference corpus it triggered Samantha but missed Daniel. Both Hey Jarvis clips triggered; six ordinary/near-word clips stayed below the unchanged 0.5 threshold. Rust output matched the official Python reference across all ten clips, and the shared-runtime process exited successfully. These are generated fixtures, not a real-microphone accuracy guarantee. See [openWakeWord](https://github.com/dscripka/openWakeWord) for model details and licensing, and [Apple SpeechAnalyzer](https://developer.apple.com/documentation/speech/speechanalyzer) for the native transcription API.

```sh
./scripts/launch-jarvis.command --root "$PWD/examples/demo-workspace" --db "$PWD/runtime/context.db"
```

Jarvis waits in the menu bar; a wake request or a click on its menu-bar item reveals the compact top-right glass panel. Dismissing the panel keeps wake listening and awareness running. Expand the panel for typed conversation, awareness status, pause, speech controls, and Quit. File awareness and conversation run on separate workers. The launcher builds a signed development app under Application Support and supplies the project directory so its `.env` loads through LaunchServices.

Configure the two model paths and prepare Apple speech assets before launching. Say “Hey Jarvis,” followed by the request; the panel automatically speaks its response. macOS may request microphone permission on first use. Capture uses the default macOS input device unless `JARVIS_MICROPHONE_NAME` specifies an exact available input name. Jarvis follows changes to the system input within a second and clears audio from the previous device. Select AirPods under Sound → Input when macOS lists them as an input; an output-only connection does not select their microphone. A missing explicitly selected input reports an error instead of substituting another microphone. AirPods capture still needs a live hardware rehearsal. The idle card identifies the selected microphone; hover over the menu-bar icon or microphone control to inspect signal levels, decode count, wake matches, and decode duration. These diagnostics contain no recording or transcript.

The wake path feeds 80 ms mono 16 kHz frames through openWakeWord's mel, embedding and classifier graphs. It does not continuously transcribe ambient speech. After a wake match, Silero tracks the command's speech activity and the bounded command recording goes to Apple SpeechAnalyzer through an owned helper process. The helper receives at most 30 seconds of PCM in memory, emits bounded JSON, and has a 20-second parent deadline with cancellation. It opens no microphone, writes no audio files, and prints no transcript diagnostics. Wake capture pauses during Jarvis's own response; voice barge-in remains a follow-up.

Actual Apple transcription of four generated compound-command clips took 0.19–0.64 seconds per helper process. Two difficult clips still misrecognized a proper name; this is an accuracy limitation, not evidence of successful human speech recognition. The older Whisper CLI remains available for offline experiments, but the native live path uses Apple SpeechAnalyzer.

The launcher reuses a local development certificate under `~/Library/Application Support/Jarvis/Signing`. Its certificate-pinned app requirement remains stable when executable contents change. The private key is non-exportable in a dedicated keychain; the keychain password and certificate are stored in the same owner-only directory for local build automation. Preserve that directory across rebuilds. The signing helper temporarily registers the keychain for identity lookup and restores the search list afterward; it changes neither certificate trust nor macOS privacy settings. This is local development signing, not Developer ID distribution. Switching from the older ad-hoc identity can require one new permission grant. Permission retention across subsequent builds still requires live OS verification.

The panel's optional `--voice-diagnostics` argument emits numeric capture/recognition counters to stderr every two seconds for at most ten minutes. It excludes device names, audio, transcripts, configuration, and provider content. Increasing sample counts show capture is alive. Keyword spotting runs without transcription; Apple transcription starts for an armed command. Separate xrun, input-route, and queue-overrun counters identify capture interruptions. Wake matches and accepted commands distinguish capture from downstream model failures.

Conversation failures show a specific, redacted reason in the card. Requests originating from voice also receive one spoken failure acknowledgment. Rate-limited conversations retain the question and retry at most twice, honoring the provider's `Retry-After` header or using increasing delays. Other transient inference errors receive at most one retry. All attempts share a 60-second deadline, and cancellation stops waiting and pending retries. A provider delay beyond the remaining deadline returns the rate-limit error without retrying early. Authentication, invalid requests, and malformed responses are not retried. Provider retries can repeat context reads. App and draft actions return an immediate observed result, with no later model call or automatic action retry.

The native panel supports opening Spotify and verified current-selection play/pause through fixed local controls. Exact requests run without inference. When `YOU_API_KEY` is configured, requests such as “play Metallica on Spotify” go through the conversational tool registry: named-artist lookup, verified Spotify identity, native playback and matching artist/track readback. Other artist names follow the same path; no names or artist IDs are built into the production adapter. Named requests never fall back to resuming the old selection. The result is a short acknowledgment such as “Playing Metallica.” Explicit songs use `spotify_play_track` with the supplied title and artist, verified Spotify track metadata and exact native playback readback. Different recordings remain ambiguous; album selection remains a separate capability. macOS may ask to let Jarvis control Spotify under Privacy & Security → Automation. Tools are not automatically replayed after failure. Model-generated prose never executes as a command.

Automated native smoke tests leave the microphone and native OS collection off. Actual Apple SpeechAnalyzer inference has been verified with generated WAV audio; synthetic wake tests verify command isolation and cancellation. The panel independently collects foreground app identity and permitted focused-window metadata. Missing Accessibility permission leaves app identity available and is shown in the panel. Known browser/password-manager titles are excluded from OS capture; approved browser pages use the extension. Fresh read-only browser tools are implemented. Browser mutations, personal speaker recognition, and a general autonomous coding worker remain separate milestones.

Git authentication remains the user's git transport. Do not invoke GitHub CLI or provision API credentials for repository work.

The native panel shares a typed tool registry across conversation and background work: `search_context`, `read_context`, `open_application`, `spotify_control`, optional `spotify_play_artist` and `search_web`, `browser_search_tabs`, `browser_read_tab`, `delegate_draft`, and `prepare_file_edit`. Independent indexed reads run in parallel. Each turn allows at most four model steps and eight proposed calls; a side effect must be the only call in its batch. Read results retain source IDs, versions, and truncation labels under a 48 KiB message budget. All provider requests remain capped at 64 KiB. Unknown tools, arbitrary file paths, extra arguments, and duplicate call IDs are rejected.

Approved applications live in `~/Library/Application Support/Jarvis/app-preferences.json`. Defaults are Spotify, Notes, Safari, Calendar, and Calculator. Clear launch commands run directly without inference; execution reloads the allowlist and verifies the target is running. App controls may require macOS Automation permission. A writing proposal queues the original user request, creates a separate verified artifact, and leaves it ready for review while conversation remains available. The file-edit worker prepares a separate revised copy of an explicitly named indexed file and preserves the original. Both workflows end at **Open result**, which revalidates the artifact before opening it. A durable journal restores review records after restart; restart and tamper handling are covered by SQLite/filesystem fixtures. Browser mutations and a general coding worker remain pending.
