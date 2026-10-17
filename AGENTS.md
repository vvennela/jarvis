# Working on Jarvis

- Use git only for repository operations. Do not use `gh` or GitHub API tools.
- Pull before starting repository work. Preserve other agents' changes; do not reset shared files.
- After every meaningful implementation change, delegate an independent agent to write adversarial integration/e2e tests. Run that work in parallel with independent implementation. Assign non-overlapping file ownership.
- Prefer integration and process-level e2e tests that verify observable outcomes, failure paths, and state transitions. Do not replace them with implementation-mirroring unit tests.
- Root/coordinating agent commits and pushes each meaningful verified increment. Subagents must not commit or push unless explicitly assigned.
- Run formatting, clippy, and relevant tests before a commit. Never claim a live adapter is verified by a mock fixture alone.
- All executable proposals, including LM/worker/retry proposals, pass policy and execution-time checks. A model cannot override policy or assert its own authorization.
- Sensitive operations stop at `READY_FOR_REVIEW`: no send, purchase, final booking submission, or deletion capability. Verify the exact artifact revision before declaring it ready.
- Capture structured state, not screenshots. Unknown actor attribution remains unknown. Never mistake worker events for user intent.
- The runner owns durable job state and revisions. Conflicting user writes pause jobs; ambiguous external outcomes require reconciliation rather than blind retries.
- Keep secrets, raw private content, actual browsing histories, and model weights out of git and diagnostic output.
- Documentation must distinguish implemented behavior, fixtures, and planned integrations. Provider model IDs must come from verified catalogs; do not invent them.
- Keep READMEs short and positive: describe working capabilities, setup, and usage. Put limitations, failure analysis, and detailed validation in engineering docs and progress reports. Keep all capability claims factual.
