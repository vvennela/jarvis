# Native adapter boundaries

`src/macos.rs` owns main-thread AppKit objects. The core and worker library forbid unsafe code. Objective-C interop is limited to the NSObject subclass declaration/init, typed target/action selectors, window release ownership, and Apple's immutable run-loop global. Each unsafe operation carries its lifetime/type rationale.

The panel displays the current worker states, including explicit microphone-off status. Showing the panel does not intentionally activate the app or take keyboard focus. Clicking the editable question field allows keyboard entry. The main display's visible frame determines launch position.

The file worker owns SQLite and long `watch_root_controlled` sessions. Actual observer readiness triggers Active; pause acknowledgment follows collector cancellation. Filesystem I/O and an in-progress bounded scan cannot be forcibly preempted. The panel owns one latest-state snapshot per worker and no raw awareness event queue.

Conversation uses one current-thread Tokio runtime on a separate OS thread. A capacity-one request channel and atomic busy flag permit one pending or active request; additional submissions return Busy. Input is capped at 8 KiB. Cancel increments a watched request generation and drops the in-flight ask future; the engine commits history only after a successful response. Cancellation cannot undo a request already completed at the provider. The core engine bounds conversation history and evidence; display text is capped at 32 KiB. Provider/credential errors yield generic actionable UI text, never secret contents.

A missing model configuration leaves the file worker usable. Restart reads updated project `.env` settings. Conversation currently returns text and sourced evidence; execution of task proposals and sensitive prepared artifacts remains the core runner's separate boundary. Browser status display and signed packaging are separate adapters.

The real native smoke run creates an NSPanel, drives the same pause/resume/quit handlers, and checks acknowledgment states. Loopback HTTP tests exercise the real conversation worker without live provider calls. Neither substitutes for human visual/focus review.

## Native speech

Speech is opt-in for each reply. A dedicated worker launches `/usr/bin/say` directly with fixed arguments `-f -` and sends at most 8 KiB of text over stdin. No shell is invoked and reply text never becomes arguments. The UI reads a bounded excerpt of longer replies. The worker permits one request and one owned child, rejects overlap, caps playback at two minutes, and kills/reaps only its own process on stop or shutdown. Cancelling a reply and quitting both stop owned playback. Speech and inference have independent lifecycles. An injectable executable lets tests verify these properties without producing audio; native UI smoke remains silent.

## Local push-to-talk

The voice worker loads the explicitly configured local Whisper model once and retains it. Only the Record button starts the default input device. CaptureSession reports success after CPAL stream playback starts; only then does the panel show Recording. Finish stops and drops the owned stream before running local transcription. Capture is limited to15seconds/240,000 mono16k samples, with streaming downmix and deterministic linear resampling for f32/i16/u16 device formats. Callback processing uses a preallocated output buffer and no growing input queue.

Record stops owned speech playback. Cancel drops capture; during native Whisper inference it sets cancellation and discards the result after inference returns. Current Whisper binding supports pre/post cancellation checks; native decoding is not forcefully interrupted. The main UI, file observation, and conversation workers remain independent. Quit keeps the event loop responsive while owned workers stop. Transcripts fill the editable question field and require Ask before any model-provider request.

Synthetic backend tests cover recording/finish/cancel/permission-failure lifecycle without microphone access. Native panel smoke never invokes Record. Live microphone verification requires an explicit user click.
