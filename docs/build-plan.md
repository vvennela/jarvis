# Jarvis build plan

Build a companion that stays present while you work: continuous file and browser awareness, local “Jarvis” recognition, a responsive conversation, and delegated work that ends with an exact artifact ready for review. Always-on wake listening is the primary voice interaction. Personal speaker identification comes later.

## Current milestone

| Component | Current implementation | Next verification or extension |
|---|---|---|
| File awareness | Explicit-root text/Markdown collection, bounded notifications and reconciliation, hashes, SQLite history and FTS search | Exercise changes while the panel stays open |
| Browser awareness | Read-only Chrome extension and authenticated Native Messaging session, approved-origin page/tab evidence | Install and verify a real approved page |
| OS awareness | Native foreground app and permitted focused-window metadata; independent panel collector; shared SQLite evidence | Live Accessibility-title rehearsal and utterance-frozen anchors; app identity captured on this Mac |
| Conversation | Jev routing and context retrieval overlap; bounded conversational history; Nemotron replies and clarification follow-ups | Rehearse a real multi-turn session using the configured provider |
| Native panel | Menu-bar idle presence, 360×140 wake card, native Liquid Glass with older-macOS blur fallback, expandable conversation and speech controls | Rehearse the complete microphone-to-reply path on the user's Mac |
| Local voice | Resident Whisper model, bounded microphone capture and transcription; continuous wake worker and automatic spoken-reply integration are the current increment | Validate wake recognition, endpointing, mute/resume and echo suppression together |
| Speech output | Native speech; optional ElevenLabs Flash synthesis with cancellable owned playback and private temporary audio | Native playback is the default without an ElevenLabs key; live ElevenLabs quality requires a configured key |
| Review preparation | Context-backed model-generated local message draft, source-version checks, artifact verification, cancellation and readiness invalidation | Add durable jobs and a review surface in the panel |

The local draft command is a real generation path. A live Nemotron request against the generated Beacon fixture produced a verified `READY_FOR_REVIEW` artifact at `runtime/brian-live-review-1834.txt`; no message was sent. The older `demo` command is explicitly a scripted policy/review fixture. Neither is an autonomous coding worker or a Gmail integration. Conversation currently returns text; it does not execute model-generated tools. State-machine tests establish contracts, not live desktop execution.

## What runs continuously and on demand

Continuously while enabled: filesystem collection and reconciliation; permitted browser observations; local microphone capture with bounded audio windows and speech-energy checks. The wake worker uses the resident local Whisper model when speech needs recognition. It makes no hosted LM call for idle awareness or wake detection. This is local speech recognition over windows, not a specialized hardware wake-word detector.

On a recognized “Jarvis” request: reveal the compact card, finish local transcription, overlap Jev with indexed-context retrieval, then call the conversational model. Keep the UI responsive on its own thread. A spoken request receives a spoken answer automatically. Wake-opened cards hide after eight idle seconds; menu-opened or expanded cards stay open until dismissed. Hiding preserves listening and awareness. Typed input remains available for debugging and recovery, not as the primary voice interaction.

For an explicitly requested local draft: pin selected indexed resources, generate with the model, recheck source versions, save the caller-selected artifact, and verify it. A changed or missing source blocks readiness. Sending, purchasing and deleting remain final human actions; preparing a draft does not grant a commit capability.

## Next implementation order

| Order | Work | Acceptance gate |
|---|---|---|
| 1 | Complete the always-on wake increment | “Jarvis, where was I?” reaches conversation and speaks back; an ordinary ambient utterance is not submitted; mute closes capture; silence does not trigger model requests |
| 2 | Rehearse context continuity | A note indexed before the question is cited accurately; changing its review time changes the next answer; clarification history survives follow-ups |
| 3 | Rehearse the verified local draft | Real model output reflects the selected note; source changes during generation block output; artifact tampering or later observed changes revoke readiness on review |
| 4 | Integrate draft jobs with conversation and panel | A typed proposal goes through the gate; the runner owns job state; the UI opens the exact verified artifact; no send capability appears |
| 5 | Add one bounded tool-backed worker | Isolated project workspace, explicit tool arguments, execution-time preconditions, a real test command and bounded repair after failures; conflicting user edits pause the job |
| 6 | Measure and rehearse the complete delegation story | Only then demonstrate “implement that,” worker correction and test repair as live capabilities |

Use independent adversarial integration/e2e testers after each meaningful change, in parallel with implementation. Give each agent distinct files. Commit and push tested increments using git. Prefer one working vertical path over expanding a general framework during the demo build.

## Ready-for-review contract

Every sensitive task ends at preparation: “The draft is ready for your review. It hasn't been sent.” Readiness identifies the job, revision and exact verified output path. The model cannot choose an arbitrary output path or turn generated prose into tool calls. The draft object rechecks indexed-source versions and artifact identity/content before returning review readiness again.

Current review state lives in the process. Changes not yet ingested by collectors are not detected by an index-version check; the future application adapter must also validate its target at execution/review time. Durable job recovery, live dependency notifications, browser draft preparation and final UI review integration are subsequent work. Do not describe a local text file as an email already drafted inside Gmail.

## Voice behavior and remaining work

The current wake design recognizes the standalone word “Jarvis” in local transcription (an optional preceding “Hey” is also accepted), then captures a command. It supports a wake phrase and command together or a wake phrase followed by the request. The current defaults use a four-second rolling wake window, speech-triggered decoding at 2.5-second intervals, a 700 ms silence endpoint, a 15-second command cap and a ten-second silent wait after a standalone wake word. These are prototype thresholds to measure, not latency guarantees. Listening is enabled on normal startup; the mute preference is not persisted. The panel exposes microphone mute/resume and actual listening/decoding/thinking/speaking state. To avoid hearing its own reply, this increment suppresses capture during conversation and playback; spoken barge-in is therefore not implemented. The explicit Stop speaking control remains available.

Tune false activations and endpointing using real microphone trials before claiming wake reliability or a latency figure. Personal speaker identification, robust noise handling, a dedicated low-power wake engine, full-duplex barge-in, OS-wide focus anchors and speculative transcript retrieval remain future improvements. Do not defer the always-on interaction itself in favor of push-to-talk.

## Measurements

Record wake recognition, command endpoint, evidence readiness, route readiness, first model response, speech start and review readiness. Include local speech-window and endpoint delays in the result. Jev has a bounded advisory deadline; falling back to conversation is preferable to waiting on routing indefinitely. ElevenLabs currently buffers bounded MP3 audio before playback, so do not call that streaming playback.

Use a fixed set of prompts and report sample counts before p50/p95. Compare maintained context with on-demand retrieval on the same tasks. Record stale-context rejection and answer correctness alongside speed. A quick acknowledgment is not task completion, and local fixture tests do not establish real microphone or browser reliability.
