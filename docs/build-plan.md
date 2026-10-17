# Today's build plan

The goal is one honest end-to-end demonstration: Jarvis observes a work session before being asked, reconstructs it, delegates a small task, accepts a correction, verifies the result, and leaves a sensitive follow-up ready for human review. Six hours and fifteen minutes is a constrained core-demo budget, not a promise of general desktop autonomy.

## Implementation order

| Time budget | Build | Acceptance / go-no-go |
|---|---|---|
| 0:00–0:45 | Typed request, resource, evidence, action and job records; policy gate; runner state; headless review fixture | CLI fixture produces verified review artifact, never final send/pay/delete. Adversarial tests reject forbidden proposals and stale review state. If this fails, do not wire external actions. |
| 0:45–1:35 | SQLite resources/events/FTS5, one filesystem watcher, normalized text hashes and provenance | A real temporary-file edit changes one resource version; unchanged content does not. Replay and agent echoes do not fabricate user activity. Scope live collection to text/Markdown. |
| 1:35–2:25 | Chrome MV3 Native Messaging bridge, focused tab anchor, permitted page text | Observe before command, survive navigation and disconnect, reject wrong extension/session. If bridge slips, use file awareness and label browser context unavailable; never fake capture. |
| 2:25–3:10 | Jev routing + parallel broker retrieval; Crusoe conversational adapter; job-bound clarification | Real provider smoke test succeeds with available model IDs. Missing/invalid keys yield explicit setup state. Final transcript invalidates wrong speculation. Typed input remains a fallback. |
| 3:10–4:10 | One background worker; dependency invalidation; narrow test tool; local message-draft review | Worker handles a changed requirement without overwriting user edits. Test failures trigger bounded repair. Verified draft reflects latest requirement and remains unsent. If safe shell confinement is not ready, use an explicitly bounded fixture worker and label it. |
| 4:10–5:00 | Native top-right panel, push-to-talk, local Whisper, native speech, barge-in | Panel reports actual state without focus theft. Speech interruption stops playback. If local speech latency is poor, preserve typed commands and show real state; do not burn remaining time on wake words. |
| 5:00–6:15 | Adversarial e2e runs, failure injection, latency traces, rehearsal and documentation | Entire story passes from fresh state; no unreviewed external action. Record known limits and real measurements. Freeze features; fix failures. |

Run independent tester work throughout each block. One implementer owns core; another can own the extension/UI boundary after contracts stabilize; a tester owns integration/e2e files and probes misuse. Avoid concurrent edits to the same module. Commit and push each tested meaningful increment using git. A failing meaningful increment is fixed before push, not hidden by skipping tests.

## Demo script

1. Start Jarvis visibly; the top-right indicator says awareness is on. Open a permitted browser specification and edit a small local requirements note.
2. Switch away. Ask “Where was I?” Jarvis identifies the unfinished work with evidence references from before the question.
3. “Implement that.” It asks one materially necessary clarification. Answer naturally; the pending question and job state update together.
4. Worker acts in its own project workspace. Issue an independent simple command while it runs to show the conversation stays responsive.
5. Change a watched requirement. Show the relevant resource revision and worker adaptation. If the user changes the worker's output file instead, show a conflict pause instead of an overwrite.
6. Tool-specific tests verify the revised behavior. Ask for an update to Brian; use a local unsent draft initially, explicitly labeled as such.
7. Jarvis says “The update is ready for review. It hasn't been sent.” The Review button opens the exact verified artifact. No final send capability exists.

Use a small genuine change and genuine tests. Fixtures support repeatability but are not presented as live Gmail or live booking integration. The broad flight-booking conversation can be a contract test; real purchase execution is deliberately absent.

## What we defer

Universal accessibility coverage, arbitrary browser action generation, real checkout/booking preparation across sites, PDF OCR, embeddings, autonomous proactive interruptions, wake-word reliability, cloud sync, multi-user identity, and unrestricted shell access. A stored index plus temporal history is enough to prove the core awareness loop.

## Measurements

Trace utterance end, evidence ready, route ready, tool start, verification complete, speech start and review ready. Measure p50/p95 when enough repeated runs exist; show sample counts. Compare sequential retrieval, concurrent on-demand retrieval and maintained context on the same fixed tasks. Report correctness and stale-target rejection alongside latency. A spoken “On it” does not count as task completion.

The first success criterion is not a promised millisecond target. It is correct contextual handoff plus verified response to a mid-task change. Then optimize the observed critical path. Jev itself incurs inference latency; remove it from a route if measurement shows it provides no useful benefit there.
