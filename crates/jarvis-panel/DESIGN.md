# Native adapter boundaries

`src/macos.rs` owns main-thread AppKit objects. The core and worker library forbid unsafe code. Objective-C interop is limited to the NSObject subclass declaration/init, typed target/action selectors, window release ownership, and Apple's immutable run-loop global. Each unsafe operation carries its lifetime/type rationale.

The panel displays the current worker states, including explicit wake-listening and muted status. Showing the panel does not intentionally activate the app or take keyboard focus. Clicking the editable question field allows keyboard entry. The main display's visible frame determines launch position.

The file worker owns SQLite and long `watch_root_controlled` sessions. Actual observer readiness triggers Active; pause acknowledgment follows collector cancellation. Filesystem I/O and an in-progress bounded scan cannot be forcibly preempted. The panel owns one latest-state snapshot per worker and no raw awareness event queue.

Conversation uses one current-thread Tokio runtime on a separate OS thread. A capacity-one request channel and atomic busy flag permit one pending or active request; additional submissions return Busy. Input is capped at 8 KiB. Cancel increments a watched request generation and drops the in-flight ask future; the engine commits history only after a successful response. Cancellation cannot undo a request already completed at the provider. The core engine bounds conversation history and evidence; display text is capped at 32 KiB. Provider/credential errors yield generic actionable UI text, never secret contents.

A missing model configuration leaves the file worker usable. Restart reads updated project `.env` settings. Conversation currently returns text and sourced evidence; execution of task proposals and sensitive prepared artifacts remains the core runner's separate boundary. Browser status display and signed packaging are separate adapters.

The real native smoke run creates an NSPanel, drives the same pause/resume/quit handlers, and checks acknowledgment states. Loopback HTTP tests exercise the real conversation worker without live provider calls. Neither substitutes for human visual/focus review.

## Native speech

Voice-request replies speak automatically; typed replies offer Read aloud. A dedicated worker launches `/usr/bin/say` directly with fixed arguments `-f -` and sends at most 8 KiB of text over stdin. No shell is invoked and reply text never becomes arguments. The UI reads a bounded excerpt of longer replies. The worker permits one request and one owned child, rejects overlap, caps playback at two minutes, and kills/reaps only its own process on stop or shutdown. Cancelling a reply and quitting both stop owned playback. Speech and inference have independent lifecycles. An injectable executable lets tests verify these properties without producing audio; native UI smoke remains silent.

## Continuous local wake recognition

The main panel loads one explicitly configured Whisper model on a dedicated worker and enables continuous microphone capture at ordinary launch. macOS microphone permission still applies. Smoke mode leaves the microphone disabled. The mute button stops the owned stream and clears pending audio and commands. A decoding call can finish after mute; its result is discarded by generation checks. Native Whisper inference currently has pre/post cancellation, not mid-decode abort.

ContinuousCapture keeps at most 15 seconds of normalized mono 16 kHz samples, downmixing and resampling f32/i16/u16 device input into a preallocated buffer. The consumer drains bounded chunks. Overflow invalidates the current utterance; no ambient audio or transcript is persisted. The continuous callback uses try_lock; a short consumer-lock collision skips that callback. The bounded ring implementation shifts a one-second block when full; it is not a lock-free audio pipeline.

WakeDetector uses a fixed RMS energy threshold, a four-second rolling wake window, local recognition every 2.5 seconds of active audio, and a 700 ms silence endpoint. An exact Jarvis word token transition into a command window capped at 15 seconds. Same-utterance commands and a separate utterance after wake are supported. Only an accepted command enters the conversational model; ambient decoded text is transient. A ten-second silent command wait resets to standby. This is a Whisper phrase detector, not a dedicated low-power wake model; real-room accuracy and CPU/latency need live measurements.

The panel sends accepted voice commands automatically and plays their replies through the independent speech controller. It suppresses wake processing and closes the owned mic while a conversation or owned speech is active, then resumes. This avoids self-echo for the prototype; voice barge-in during playback is not supported. Stop speaking remains available on the panel. There is no speaker identity recognition. PTT remains an independently tested debug worker API, separate from the main wake interaction.

Synthetic detector/backend tests exercise wake boundaries, endpointing, mute/cancel and echo suppression without touching microphone hardware. Native panel smoke is silent with microphone disabled; human launch verifies microphone permission and acoustic behavior.
