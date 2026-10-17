# Background coding

Set `JARVIS_CODING_PROJECT` in the local `.env` to the absolute path of an approved Git repository. The native panel advertises `delegate_coding` only when a coding worker is configured. It uses the locally installed Codex CLI and its existing sign-in; no additional coding API key is required.

Ask, for example: “Use Codex to fix the failing test in this project and leave the changes for review.” The conversational model proposes the typed tool, and the runner forwards the original request unchanged. Coding runs independently while conversation remains available.

The prototype requires a clean checkout and starts from its current `HEAD`. It creates a detached worktree under `~/Library/Application Support/Jarvis/Coding`, leaving the original checkout untouched. Project-defined `.codex`, `.mcp.json`, and `.codex.toml` configuration blocks execution rather than loading additional capabilities.

Codex runs with a workspace-write sandbox, shell networking disabled, approval policy `never`, user configuration ignored, and apps, plugins, hooks, remote plugins, and web search disabled. Existing execution-policy rules remain in effect. The child inherits only local login/runtime essentials, not ambient API keys. Jobs have a ten-minute maximum, bounded process output, and cancellation of the owned process group. Incomplete worktrees are preserved.

**Open result** is available after successful process exit, a `turn.completed` event, and verification of the actual Git diff and review files. Review checks the original project, isolated changes, and artifact identities again. The summary separates agent-reported testing from what the runner verified. Nothing is automatically applied, committed, pushed, or sent.

Coding worktrees and artifacts survive process exit. Coding job state currently remains in memory; the draft/file-review journal does not restore coding jobs or rerun them.

## Verified acceptance

The opt-in `crates/jarvis-panel/tests/coding_live.rs` test exercised the configured conversation model, typed delegation, actual local Codex, and explicit review on a fictional dependency-free Rust repository. Delegation took **1.815 seconds**; the complete run took **22.351 seconds**. A separate `cargo test --offline` invocation passed against the revised worktree, and the original source stayed unchanged. The reviewer was a silent test executable; this was not a human voice or native-window rehearsal.

```sh
JARVIS_LIVE_CODEX_CONVERSATION=1 cargo test -p jarvis-panel --test coding_live -- --ignored --nocapture
```

This test invokes real inference and the signed-in Codex CLI. Ordinary test runs leave it ignored.
