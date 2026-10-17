# Jarvis build plan

Build a companion that stays present while you work: maintained context, local “Jarvis” recognition, responsive conversation, and delegated work that ends with an exact artifact ready for review. Always-on voice is the primary interaction; typed input is recovery. The current judging path is [the three-minute live demonstration](demo.md).

## Implemented milestones

Current combined checkpoint: **409 workspace tests passed, 26 ignored**, plus 16 extension tests, formatting and all-target clippy. The configured Nano model completes real native tool-call continuation; You web search and native Spotify artist playback are verified live. Full conversational artist execution subsequently passed in 4.531 seconds with verified native playback. A live fictional file request passed model planning, background editing, verified publication and restart recovery in **2.098 seconds**. The current checkpoint is **22:23 UTC / 3:23 PM Pacific on October 17**, followed by two hours of product fixes.

| Component | Implemented and checked | Remaining live acceptance |
|---|---|---|
| File awareness | Explicit-root text/Markdown collection, bounded notifications/reconciliation, hashes, SQLite history and FTS search | Rehearse a saved priority change reaching the next answer |
| Shared context | Coherent read snapshot; up to eight source-tagged excerpts; foreground pinned; file/browser balance; current-work notes and tasks prioritized for next-work questions | Rehearse useful answers with the actual current project and multiple candidate documents |
| OS awareness | Independent native foreground-app collector, permitted focused-window metadata, explicit permission/status reporting | Live Accessibility-title coverage; wake-time focus freezing remains future work |
| Browser awareness | Approved-origin Chrome extension, authenticated Native Messaging host, correlated fresh tab search/read tools | Install and verify a real approved page before including it in the demo |
| Conversation | Jev advisory routing overlaps retrieval; bounded history and clarification follow-ups; configured Nemotron through Crusoe; precise errors and bounded inference retries | Human conversation quality and latency under actual venue/network conditions |
| Approved app actions | Exact approved-app requests bypass inference; typed artist lookup verifies Spotify identity and actual playing artist; native Metallica selection exercised successfully | Spoken artist selection and live hosted planning; app opening/follow-up by voice |
| Model tool runner | Real OpenAI-compatible function-call protocol; indexed search/read, fresh browser reads, public You research, approved apps, Spotify controls and artist selection, draft and file-edit delegation; bounded loops and untrusted results; adversarial fixtures pass | Exercise each actual tool through the conversational planner in the installed app |
| Native panel | Idle menu-bar presence, wake-triggered 360×140 glass card, expandable details, actual animated busy spinner, cancellation, explicit draft review | Rehearse status transitions alongside a real spoken request |
| Local voice | Streaming acoustic keyword detector, local speech-activity detection, resident Whisper command transcription, bounded microphone handoff and input-route change handling | First-attempt human wake accuracy, noisy-room endpointing and AirPods capture |
| Speech output | Native macOS speech or optional ElevenLabs Flash; cancellable owned playback and temporary audio cleanup | Natural live response quality; current MP3 output is buffered, not streamed to playback |
| Background drafts | One independent bounded TaskWorker, explicit typed/voice dispatch, shared evidence, source-version checks, cancellation, one transient retry and verified artifact publication | Rehearse spoken delegation while another conversation stays usable |
| Coding delegation | Configured clean Git project, isolated Codex worktree, bounded process, cancellation, real diff and verified review; full live model-to-code acceptance passed in 22.351 seconds | Human voice rehearsal and approval to apply reviewed changes |
| Human review | Ready-state card and Open result button; explicit review revalidates sources and file identity/content before opening | Live presenter review of a newly generated draft; no sending capability |

The production TaskWorker was checked against the configured Crusoe model and real project index: **ReadyForReview in 8.064 seconds, 4,521 bytes**, with the current project priority present. Deliberate artifact modification caused explicit review to block without opening a viewer. The newer Nano native tool protocol check passed in 1.778 seconds. Lightning previously returned model-specific HTTP 429; precise errors and bounded cooldown-aware inference retries remain implemented. This is an engine check, not proof of voice capture or end-to-end spoken latency.

Independent integration tests cover shared-snapshot consistency, source selection, provenance, concurrent conversation/drafting, cancellation, stale evidence, tampered artifacts, retry limits and explicit review. The native smoke test covers panel lifecycle and spinner visibility transitions with the microphone disabled. A passing smoke test does not establish human wake reliability.

## What runs continuously versus on demand

While enabled, file collection, permitted OS/browser collection and local microphone processing operate independently. The native voice path uses streaming acoustic keyword spotting and local speech-activity detection; Whisper transcribes commands after a wake. Hosted models are not called for idle awareness or keyword detection. Browser events are available only after a real extension installation and origin grant.

For a request, reveal the card and complete local transcription. Exact approved app requests run through native policy checks without a hosted model. Other conversational requests overlap Jev's bounded advisory routing with indexed retrieval, then call Nemotron. Typed tool proposals pass registry and argument validation; side effects recheck the original caller request. Read-only tool calls may run in parallel, while side effects execute one at a time and return their observed result without another inference call. SQLite evidence is read in one coherent transaction; this is not a claim that model prefill or every database read happens in parallel. Local collectors maintain context ahead of the request. The conversation retains bounded successful turns for clarification follow-ups.

For an explicit draft request, caller-text dispatch or a policy-checked `delegate_draft` proposal starts the independent worker with the original request. It selects at most four indexed file/browser resources, pins their versions, generates a local draft, rechecks dependencies, creates a fresh application-selected file and verifies it. The model selects neither the output path nor an executable. Conversation and awareness continue while the draft is running. At most one active draft or file edit is retained; a second work request is rejected while busy. File editing prepares a separate complete revised copy and leaves the source intact. The worker checks raw source bytes and filesystem identity as well as index revisions.

Draft readiness does not open a viewer. **Open result**, or an explicit review request, runs revalidation before the native opener receives the verified path. A changed or missing source, or a modified artifact, blocks readiness. No send, payment, deletion or final booking capability is exposed.

## Remaining order before judging

| Order | Work | Acceptance gate |
|---|---|---|
| 1 | Rehearse named artist selection plus native actions by voice | An approved app opens or Spotify changes state, the real outcome matches the acknowledgment, and the current quiet-audio/preroll build accepts ordinary speech |
| 2 | Rehearse installed tool calling | A live synthetic function proposal receives a synthetic result and returns a final answer; no private context or native action is needed for this protocol check |
| 3 | Rehearse context continuity | A truthful saved priority change produces a changed answer; the indexed resource/version explains the result |
| 4 | Rehearse concurrent delegation | Spoken draft request reaches the real provider; another short conversation works while it runs; result reflects the updated note |
| 5 | Rehearse explicit review | Open result revalidates and opens the new local artifact; the presenter confirms content and unsent status |
| 6 | Run the timed presentation and capture submission video | Three-minute live path and separate one-minute video show actual event-built functionality, with fixture data or recovery clearly labeled |

Use independent adversarial integration/e2e testers after meaningful changes, with distinct file ownership. Commit and push tested increments using git. Keep the remaining work on this complete path; existing-chat resumption and browser bookings are separate milestones.

## Review and failure contracts

Sensitive operations stop at preparation: “The draft is ready for your review. It hasn't been sent.” The worker owns one active job. A bounded, atomic, owner-only journal stores artifact/source identities and hashes. Restart restores review readiness only after revalidation; interrupted or changed work becomes blocked without replaying actions. An index-version check detects only changes already ingested by a collector. It is not an atomic lock on every external source. Later application adapters must validate their own targets as well.

Conversation permits at most three attempts for rate limits and two for other retryable inference failures within one 60-second budget. Draft inference permits at most two attempts within one 90-second budget. Both honor supported server cooldowns and cancellation; a delay that cannot fit the remaining budget is not shortened to force a retry. App actions and completed draft submission are never repeated by the provider retry loop. Authentication, rejected requests and invalid/incomplete responses are not retried automatically. Successful generation and verified readiness are separate from opening the artifact; opening is separate from any human decision to send it elsewhere.

Source content—including current-work notes—remains untrusted evidence with source, resource ID, version and observation time. A note can report a decision or deadline; its presence does not prove that an action occurred. Foreground metadata is a retrieval-time observation, not an utterance-frozen reference. Browser URL-cache associations do not prove that a page is current or active.

## Voice acceptance and measurement

The native path recognizes the Jarvis keyword acoustically, then captures a bounded command. “Hey Jarvis” includes the same keyword. Commands can follow a wake directly or after a short pause; repeated wake words alone do not become remote commands. Local speech-activity detection uses a 1.1-second speech-free endpoint grace period and checks audio arriving during inference before accepting a provisional endpoint. Commands remain capped at 15 seconds, with a bounded silent wait after wake. These settings are not a latency guarantee.

A legacy injected backend using bounded Whisper wake windows remains for portable tests. It does not describe the configured native acoustic keyword path. An earlier human wake succeeded on the first attempt; acceptance of the current quiet-audio/preroll build is still pending. Synthetic keyword and Whisper tests establish model/runtime operation, not real-user first-attempt accuracy. The system follows default microphone changes when no exact input override is set; AirPods input/output previously passed a 24 kHz mono hardware check; recheck the selected route for the current session.

Wake listening pauses during owned conversation/output to avoid self-triggering; the background draft alone does not suppress it. Full-duplex voice barge-in, speaker identity and production noise robustness remain future work. Explicit Stop speaking and Mute controls are available. Hiding the card leaves workers running; normal startup enables listening, and the mute preference is not persisted.

Measure wake recognition, command endpoint, evidence readiness, routing, model response, speech start and verified draft readiness separately. Include endpoint and local decoding time in any end-to-end number. Use repeated real trials and report sample counts before p50/p95. Record stale-context rejection and answer correctness alongside speed. A quick acknowledgment is not completion.
