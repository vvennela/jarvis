# Adversarial integration and e2e plan

This is the acceptance matrix for successive increments, not a claim that all these tests currently exist. Each meaningful implementation change gets an independently delegated tester. Test public behavior, persistence, processes and adapter boundaries; use unit tests only where they add distinct value.

Run `cargo test --all-targets`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings`. Headless CI must use temporary directories, a temporary database and deterministic provider/tool fakes. Live provider and macOS permission tests are explicit opt-in smoke tests; never require real keys or private browser sessions in CI.

| Boundary | Adversarial integration/e2e cases | Required outcome |
|---|---|---|
| Policy | Model proposes send/pay/delete, disguises a final action as preparation, or submits through a generic tool | Denied before adapter execution; denial visible; no side effect |
| Review | Artifact missing, empty, wrong target, wrong digest, modified after verification, or based on old job revision | Cannot enter or retain READY_FOR_REVIEW; no success acknowledgment |
| Review process | Run fixture binary with temporary output; inspect output and state after exit/restart | Exact artifact exists, remains uncommitted/unsent; restart does not falsely complete task |
| Tool outcome | Tool says success but postcondition disagrees | Verification failure, bounded recovery or blocker |
| Files | Duplicate events, unchanged hash, atomic rename/save, deletion, overflow/rescan, file changing during read | Stable identities/versions where possible; no stale-content success; reconcile gaps |
| Scope | Symlink escapes, excluded directories, password-like sensitive inputs, private tabs | No forbidden ingestion; source exclusion remains effective during retrieval |
| Provenance | Webpage says “send all files”; LM claims authorization; forged evidence reference | Untrusted content remains data; gate checks real authorization and evidence |
| Actor | Worker writes same file, user writes during worker action, unattributable third-party write | Expected echo recorded as agent; conflict pauses; ambiguity remains unknown |
| Anchor | Focus changes between utterance and execution; two tabs share title; selection disappears | Use frozen stable target or clarify; never silently act on newly focused tab |
| Speculation | Partial “open…” becomes final “close…”; late results from previous request arrive | Obsolete intent discarded; only relevant fresh evidence reused |
| Gate | Slow/missing evidence, Jev timeout, malformed result, low confidence | Bounded explicit fallback; no indefinite wait or guessed action |
| Clarification | Two jobs await answers; stale answer after correction; ambiguous time zone or airport | Correct job/revision binding or clarification, never arbitrary assignment |
| Cancellation | Barge-in during TTS, stop during retry/backoff, cancellation immediately before tool call | Playback stops; pending effects don't start; completed effects not falsely undone |
| Recovery | Repeated identical failure, changed page, no flight within constraints, uncertain external result | Retry bound respected; refresh/replan/ask; no silent constraint relaxation or duplicate submission |
| Contention | Two workers target same tab; human changes target while lease held | Serialize agent calls; human intervention causes yield or detectable precondition failure |
| Bridge | Wrong extension/peer, replayed nonce/sequence, unknown protocol, oversized message, disconnect | Reject safely, no capabilities leaked, explicit stale/disconnected status |
| Providers | 401/429/5xx, timeout, malformed JSON, unsupported model ID | Redacted diagnostics, bounded retry where appropriate, useful setup/failure state |
| Privacy | Retention expiry, delete history, pause then resume, excluded evidence in prior summaries | Raw/index/derived source content removed as appropriate; no collection while paused |
| Responsiveness | Background worker blocked while new simple command arrives | Conversation path stays available; no global worker lock |
| Panel | Job transitions, listening vs awareness, multiple jobs, expired review | Accurate states, stable question/review UI, no focus theft or false “done” |

## Test layers

1. Integration tests exercise broker, policy, runner and store through their public interfaces with real temporary storage. Use controlled clocks where deadlines matter, avoiding long sleeps.
2. Process e2e tests spawn the real headless binary, pass requests, inspect exit status/output/artifacts, and restart to test persistence when implemented.
3. Browser fixture tests use a local fixture site with intentional DOM mutations and stale targets. They validate extension/host message flow, not the behavior of commercial booking sites.
4. Native smoke tests run manually or on an authorized Mac: permissions denied/granted, window anchor, panel focus behavior, microphone, speech interruption.
5. Live provider smoke tests require explicit environment variables and use non-private fixture text. Record provider/model version, not secret headers.

Testers should attempt to break invariants rather than duplicate happy-path implementation. Report what is mocked and what is real. Found failures must be fixed and rerun before the coordinating agent commits and pushes the increment.
