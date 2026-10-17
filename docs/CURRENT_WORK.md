# Jarvis — current work

Maintained development note, October 17, 2026. This records the project priorities and verified progress; it is not a complete desktop activity log.

## Next action

Finish the installed-app rehearsal: reliable voice request, fresh shared context, an actual tool action, and a background result opened for review. The next checkpoint is **3:23 PM Pacific today (22:23 UTC)**, followed by two hours of product fixes requested by Vishnu.

## Current priorities

1. Rehearse the installed voice-to-action path. The full live model → artist tool → You lookup → verified Spotify playback now passes in 4.531 seconds, with the natural acknowledgment “Playing Metallica.”
2. Connect the browser extension in Arc and demonstrate a fresh page change reaching the shared index. Extension/native-host/fresh-read integration tests pass; installation in the current browser session is pending.
3. Rehearse human voice on the current AirPods route. Generated audio passes real local wake detection, VAD, Whisper, handoff and speech synthesis; the model response in that test is a fixture. Test natural pauses and first-attempt wake separately.
4. Demonstrate a file edit or handoff while conversation remains available. Open the verified result explicitly. Rehearse the three-minute story in demo.md.

## Verified now

- Nemotron Nano through Crusoe completes live native function-call/result continuation. Lightning's earlier HTTP 429 was a model quota response, not a missing API key. The configured conversation and worker model is `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B`.
- Live conversation → file-edit tool → background worker → verified review artifact → restart recovery passed in 2.098 seconds using a fictional Markdown fixture. Original content stayed unchanged. Malformed reasoning delimiters now block publication.
- Production You search → canonical Spotify identity → native playback passed in 3.276 seconds. The later full conversational tool path passed in 4.531 seconds; both checks exclude human voice.
- Public You web research returned five results in 3.45 seconds. Only the original user request can become the search query; model-added file or page contents cannot be appended.
- Separate live fictional file-edit and draft checks passed. Background work preserves source versions and verifies exact artifacts before review; changed sources or modified artifacts revoke readiness.
- Menu-bar companion, glass card and spinner; local keyword recognition and transcription; spoken replies; incremental file indexing; foreground application metadata; parallel context retrieval and Jev routing; typed, policy-checked app/context/music/draft/file/browser tools.
- Durable job metadata restores verified review results without replaying model calls or actions. Interrupted jobs become blocked and need an explicit new request.

## Remaining scope

Current awareness watches the approved project docs. Browser actions, arbitrary repository coding/testing, continuous task reconstruction across every app, utterance-frozen focus, full-duplex voice interruption and speaker identification remain later work. The browser bridge provides approved-site collection and fresh read tools; actual browser-session installation still needs acceptance.

Sensitive tasks produce local review artifacts. Sending, payment, deletion and final booking submission are not automatic capabilities.
