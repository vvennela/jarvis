# Jarvis — current work

As of October 17, 2026, 12:41 PM America/Los_Angeles.
Maintained by the development assistant from Vishnu's explicit requests and verified repository results. This is a project work note, not a record of all desktop activity.

## Objective and deadline

Prepare the Jarvis prototype for hackathon judging in three hours, targeting 3:41 PM today. The demonstration should show continuous awareness, hands-free interaction, useful work context, and background preparation of a verified artifact for human review.

## Current priorities

1. Make “Jarvis” and “Hey Jarvis” work on the first attempt. The live microphone delivers audio and local Whisper inference takes roughly 200–300 ms in the latest diagnostic session, but repeated invocations are still needed. Investigate a dedicated streaming wake-word detector and test genuine first-attempt success. Do not treat synthetic speech tests as a human reliability measurement.
2. Make “What should I work on next?” identify this deadline and the highest-priority unfinished work. Prioritize current task notes over generic setup instructions. Validate that editing this note changes the next answer.
3. Connect a spoken delegation to an independent background worker that writes a context-backed local handoff draft. Keep conversation available during generation and open the exact verified artifact for review. Source changes and cancellation must prevent stale success.
4. Rehearse the complete demo, measure wake and response latency, and polish the compact status/review UI after the core path works.

## Verified working

- Native menu-bar app, wake-triggered card, explicit microphone mute, local Whisper transcription, and spoken responses.
- Scoped text/Markdown indexing, file-change notifications, content hashes, SQLite context history, and keyword retrieval.
- Foreground app identity collection; focused-window details depend on macOS Accessibility permission.
- Jev routing overlaps context retrieval, then Nemotron 3.5 Lightning through Crusoe answers.
- The direct context/model request “What should I work on next?” succeeded in 2.44 seconds during a live check. This does not establish complete voice-to-speech latency.
- Stable local code-signing identity across changed builds; one bounded retry and specific error reporting for model failures.
- Local draft CLI generates an unsent artifact and verifies source versions and artifact identity before reporting ready for review.
- Latest full Rust suite: 221 passing tests. Additional native app smoke and real Whisper tests have passed separately.

## Open constraints

- The selected system input is currently the MacBook Pro microphone. AirPods are exposed as output, not an available input, in the latest device inventory.
- Awareness currently indexes this project's docs, not every file in Documents or Downloads.
- Browser bridge code and fixture tests exist; a real browser-page collection demo still needs installation and verification.
- Conversation currently answers questions; panel-connected background draft execution is being implemented. There is no verified general coding worker or arbitrary file-editing agent yet.
- Sensitive operations must end ready for human review. Sending, paying, deleting, and final booking submission are not part of the prototype's automatic actions.

## Immediate next action

Complete and measure first-attempt wake recognition while the parallel context and background-draft work proceeds. Then rehearse a live question, change a project priority, ask again, and create a handoff draft from the updated context.
