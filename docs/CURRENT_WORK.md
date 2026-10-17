# Jarvis — current work

Verified checkpoint: October 17, 2026, commit `a240ff6`. Maintained from Vishnu's requests and repository/test results; this note is not a complete desktop activity log.

## Objective and deadline

Prepare a useful, always-on Mac companion for judging by approximately **3:41 PM Pacific today (22:41 UTC)**. Show maintained context, hands-free requests, responsive conversation, approved app actions, and background work ending with a verified artifact for human review.

## Current priorities

1. **Rehearse actual native app actions by voice.** The current installed build integrates approved app opening and Spotify controls. Exact app requests bypass the hosted model, including during provider rate limits or missing conversational credentials. Adversarial process fixtures verify authorization, execution and outcome handling; actual app opening/playback still needs a live user trial. Try an approved app command, confirm the real result, then test a conversational follow-up.
2. **Verify the complete Nemotron tool-call protocol against Crusoe.** Typed tools and their bounded runner are implemented and fixture-tested. Recent full-protocol probes received HTTP 429, identified as `max_requests_limit (orchestrator)`; a minimal 16-token text request returned HTTP 200. This is not evidence of missing API keys. Respect provider cooldown; do not describe full live tool calling as verified yet.
3. **Check the current voice build with ordinary human speech.** A previous human wake worked on the first attempt, but the current quiet-audio/preroll increment still needs acceptance. The current app reports loaded wake listening with healthy numeric capture counters and zero reported capture errors; that does not prove a human command succeeded. AirPods input and output were previously verified at 24 kHz mono, but the current device name has not been confirmed.
4. **Rehearse shared context and background review.** Ask what to do next, save a truthful priority change, ask again, and delegate a handoff. Confirm the new draft reflects the changed note and opens only after explicit verified review. Then time the three-minute walkthrough in `docs/demo.md`.

## Verified implementation

- Pushed checkpoint `a240ff6`: **312 workspace tests passed, 14 ignored**; formatting, clippy and native panel smoke passed. Ignored live/hardware tests are not included as successes.
- Native menu-bar companion with compact glass card, real busy spinner, local keyword detection and Whisper transcription, microphone controls and spoken responses. Awareness and draft work run independently.
- Explicit-root file notifications, hashes, SQLite history/FTS, foreground app metadata, source-balanced retrieval and next-work ranking. The current demo scope is this project's docs, not all Documents/Downloads.
- Jev routing overlaps context retrieval. Jev can propose typed Spotify actions; the original caller request still passes the policy gate. Ordinary exact app actions need no model.
- Nemotron function-call adapter and bounded runner support indexed search/read, approved application opening, Spotify play/pause, and local draft delegation. Tool results retain untrusted provenance. Model prose cannot execute tools; actions recheck original caller intent and return observed outcomes without a further model call.
- Independent TaskWorker creates local drafts, rechecks evidence versions and artifact identity/content, and leaves results ready for explicit review. One earlier production check completed in **8.064 seconds**, producing **4,521 bytes** from the real project index. Tampering correctly blocked review. That measurement is an engine check, not spoken end-to-end latency.

## Still pending

Live installed browser-page collection and browser actions, a general coding worker, durable job restart recovery, utterance-frozen focus, full-duplex voice interruption and speaker identification remain future milestones. The browser bridge currently provides read-only collection code and fixtures. Recheck the current microphone route during rehearsal rather than assuming it from an output connection.

Sending messages, paying, deleting and final booking submission have no automatic capability. Drafts remain local and unsent; preparing a handoff for Brian does not resolve an email address or send it.
