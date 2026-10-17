# Jarvis — current work

Maintained development note, October 17, 2026. This records the project priorities and verified progress; it is not a complete desktop activity log.

## Next action

Finish the installed-app rehearsal: reliable voice request, fresh shared context, an actual tool action, and a background result opened for review. Current product fixes target microphone recovery and carrying a clarified request through to execution.

## Current priorities

1. Rehearse the installed voice-to-action path. The full live model → artist tool → You lookup → verified Spotify playback passed in 4.531 seconds, with the natural acknowledgment “Playing Metallica.”
2. Rehearse the installed browser context through voice. Arc is connected with localhost explicitly approved; the real page changed from Atlas to Beacon, the shared index advanced from version 1 to 2, and Nano answered Beacon after actual fresh search/read tool calls.
3. Rehearse human voice on the current AirPods route. Generated audio passes real local wake detection, VAD, Whisper, handoff and speech synthesis; the model response in that test is a fixture. Test natural pauses and first-attempt wake separately.
4. Demonstrate a file edit or handoff while conversation remains available. Open the verified result explicitly. Rehearse the three-minute story in demo.md.

## Verified now

- The public page-opening flow now resolves trusted intent into the installed typed tool, with the same authorization and canonical-article verification. Live “open the page for Napoleon” passed in 3.013 seconds. “Open the Wikipedia page” → clarification → “Napoleon.” passed in 2.156 seconds, with exactly one actual browser open. These checks use typed input; human voice acceptance remains separate.
- Microphone recovery has bounded reconnect attempts, physical-device tracking across Bluetooth format changes, and a spoken/visible notice when switching inputs. Current live capture recovered and recorded a wake plus two commands; first-attempt human reliability remains under rehearsal. Integration fixtures cover retry limits, stale audio cancellation, and exact fallback notices.
- Nemotron Nano through Crusoe completes live native function-call/result continuation. Lightning's earlier HTTP 429 was a model quota response, not a missing API key. The configured conversation and worker model is `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B`.
- Live conversation → file-edit tool → background worker → verified review artifact → restart recovery passed in 2.098 seconds using a fictional Markdown fixture. Original content stayed unchanged. Malformed reasoning delimiters block publication.
- Production You search → canonical Spotify identity → native playback passed in 3.276 seconds. The later full conversational tool path passed in 4.531 seconds; both checks exclude human voice.
- Public You web research returned five results in 3.45 seconds. Full configured model → actual You request → answer citing a returned URL passed in 6.958 seconds. Only the original user request becomes the query; private context cannot be appended.
- Installed Arc → authenticated native bridge → fresh page → live context index passed for Atlas and Beacon. Beacon read/index took **44 ms**; the full configured Nano → actual browser search/read → Beacon answer took **3.316 seconds**. Only the fictional localhost page was exposed to that model check; form values and the marked private section stayed excluded.
- Full live conversation → isolated Codex job → independent offline Rust tests → explicit verified code review passed in **22.351 seconds** (delegation: 1.815 seconds). Coding uses the configured clean Git project and leaves its original checkout intact. Current job state is available to conversation.
- Separate live fictional file-edit and draft checks passed. Background work preserves source versions and verifies exact artifacts before review; changed sources or modified artifacts revoke readiness.
- Menu-bar companion, glass card and spinner; local keyword recognition and transcription; spoken replies; incremental file indexing; foreground application metadata; parallel context retrieval and Jev routing; typed, policy-checked app/context/music/draft/file/browser tools.
- Durable writing-job metadata restores verified review results without replaying model calls or actions. Interrupted jobs become blocked and need an explicit new request. Coding has verified live review, with no restart recovery claim.

## Remaining scope

Current awareness watches the approved project docs and explicitly granted browser sites. Website form interaction, mailbox access, resuming an existing Codex conversation, continuous task reconstruction across every app, utterance-frozen focus, full-duplex voice interruption and speaker identification remain later work.

Independent compound actions and scoped clarification have passing integration fixtures. The full live compound now passed in **5.747 seconds**: Nano planned two parallel clause-specific tool loops, opened the canonical Napoleon Wikipedia page, and verified a Taylor Swift track playing. A separate paused-Metallica → Taylor Swift switch passed in **6.033 seconds**. Requests using “then” stay intact rather than being dispatched as independent parallel actions. These live checks exclude human voice.

Working scope estimate: roughly **80% of the judging prototype** and **60% of the broader vision**, as a planning assessment rather than measured test coverage. The remaining prototype work is the current-build human voice rehearsal and product fixes.

Sensitive tasks produce local review artifacts. Sending, payment, deletion and final booking submission are not automatic capabilities.
