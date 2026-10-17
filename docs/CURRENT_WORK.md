# Jarvis — current work

As of October 17, 2026, 12:53 PM America/Los_Angeles.
Maintained by the development assistant from Vishnu's explicit requests and verified repository results. This is a project work note, not a record of all desktop activity.

## Objective and deadline

Prepare the Jarvis prototype for hackathon judging in three hours, targeting 3:41 PM today. The demonstration should show continuous awareness, hands-free interaction, useful work context, and background preparation of a verified artifact for human review.

## Current priorities

1. Make “Jarvis” and “Hey Jarvis” work on the first attempt. The microphone delivers audio, but human first-attempt recognition still needs verification. A dedicated streaming keyword detector now passes generated-audio checks, including a 1.10-second synthetic probe with 35 ms CPU processing. Complete live capture integration and measure genuine first-attempt success. Do not treat synthetic speech tests as a human reliability measurement.
2. Rehearse “What should I work on next?” against this note, then edit a priority and ask again. Source-balanced retrieval now ranks current-work and task notes above setup guides; eight broker and nine conversation integration tests verify selection and provenance. The live edit-to-answer demonstration remains to be rehearsed.
3. Rehearse spoken background delegation while conversation remains available. The configured production TaskWorker has now generated a current-priority handoff from the real project index; the independent worker, typed/voice dispatch, explicit Open draft button, cancellation and stale-source checks are implemented and tested. Generated drafts must remain local and unsent.
4. Rehearse the complete demo, measure wake and response latency, and polish the compact status/review UI after the core path works.

## Verified working

- Native menu-bar app, wake-triggered card, explicit microphone mute, local Whisper transcription, and spoken responses.
- Scoped text/Markdown indexing, file-change notifications, content hashes, SQLite context history, and keyword retrieval.
- Foreground app identity collection; focused-window details depend on macOS Accessibility permission.
- Jev routing overlaps context retrieval, then Nemotron 3.5 Lightning through Crusoe answers.
- The direct context/model request “What should I work on next?” succeeded in 2.44 seconds during a live check. This does not establish complete voice-to-speech latency.
- Stable local code-signing identity across changed builds; one bounded retry and specific error reporting for model failures.
- Local draft CLI generates an unsent artifact and verifies source versions and artifact identity before reporting ready for review.
- Panel-connected TaskWorker prepares one bounded background draft, retains conversation responsiveness, and opens a draft only on explicit review after revalidation. Five independent HTTP/SQLite/opener tests cover generation, cancellation, source/artifact changes and missing evidence.
- A production TaskWorker check against the configured Crusoe model and live project index reached ready for review in 8.064 seconds, producing a 4,521-byte unsent artifact that mentioned the current wake-reliability priority. Deliberately modifying that test artifact and requesting review correctly revoked readiness before opening any viewer. This was an engine check, not a live spoken-delegation rehearsal. Earlier attempts hit provider rate limiting; transient generation failures now receive at most one cancellable retry within a single 90-second budget.
- An earlier full Rust suite checkpoint had 221 passing tests. Subsequent context, background-task and native UI increments have their own passing targeted tests; this count is not a new full-suite measurement.

## Open constraints

- The selected system input is currently the MacBook Pro microphone. AirPods are exposed as output, not an available input, in the latest device inventory.
- Awareness currently indexes this project's docs, not every file in Documents or Downloads.
- Browser bridge code and fixture tests exist; a real browser-page collection demo still needs installation and verification.
- Conversation answers questions while the independent background worker prepares local handoff drafts. There is no verified general coding worker or arbitrary file-editing agent yet.
- Sensitive operations must end ready for human review. Sending, paying, deleting, and final booking submission are not part of the prototype's automatic actions.

## Immediate next action

Complete and measure first-attempt wake recognition while the parallel context and background-draft work proceeds. Then rehearse a live question, change a project priority, ask again, and create a handoff draft from the updated context.
