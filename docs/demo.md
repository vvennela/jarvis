# Jarvis: three-minute live demo

**Story:** Jarvis stays present while you work. One maintained context serves conversation and background work; a changed priority reaches the next answer, and delegated writing ends with an exact result ready for review.

This follows the participant guide's **Shared Brain** theme. Show the shared evidence changing and the worker producing a reviewable artifact. The one-hour progress report is due **October 17, 22:23 UTC / 3:23 PM PDT**; the following two hours are for product fixes and rehearsal.

## Prepare the live path

1. Run `cargo run -- doctor` to check configuration presence without displaying keys. The current rehearsal uses `nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B` for conversation and workers. Jev handles optional structured routing; You powers public research and artist discovery.
2. Use the existing app with watched root `docs/` and database `runtime/live.sqlite`. Read `docs/CURRENT_WORK.md` and make sure its priorities are accurate. If a new app session is needed:

   ```sh
   ./scripts/launch-jarvis.command --root "$PWD/docs" --db "$PWD/runtime/live.sqlite"
   ```

   Keep one app instance. Let local models and permission prompts finish before presenting.
3. Confirm the intended microphone under macOS Sound → Input and in Jarvis's menu-bar tooltip. AirPods selected for output alone do not select their microphone. Rehearse one wake, a short question, and one spoken reply on the current build. Speak after the reply finishes; voice barge-in is a separate milestone.
4. Keep the current-work note and the menu-bar control visible. Rehearse a newly generated handoff and its **Open result** action. Display the actual result produced by that run.

## The live three minutes

| Time | Action | Visible evidence |
|---|---|---|
| 0:00–0:20 | Show the current-work note. “Jarvis keeps a versioned picture of my work. Conversation and background work share it.” | Explicit watched project, actual note, unobtrusive menu-bar presence. |
| 0:20–0:45 | “Jarvis, what should I work on next?” | The card appears, the spinner shows processing, and the answer identifies a documented unfinished priority. |
| 0:45–1:15 | Edit the note near the top: “For this rehearsal, prepare Brian's handoff before reviewing the demo.” Save. Ask: “Jarvis, what's my next step for this rehearsal now?” | The answer reflects the saved change without pasting it into chat. |
| 1:15–1:55 | “Jarvis, draft a short handoff for Brian about the current priorities and unfinished work.” Ask a short follow-up while it runs. | Independent background progress and an available conversational worker. |
| 1:55–2:30 | When the result is ready, click **Open result**. | The worker rechecks source versions and artifact integrity, then opens that local result for review. The draft is unsent. |
| 2:30–3:00 | Inspect whether the handoff includes the changed priority. Explain the review boundary. | One shared context served both requests; the artifact has a concrete verification and review step. |

“Brian” is a recipient label in a draft, not a resolved email address. **Open result** is a local review action. Sending, deletion, payment, and booking submission remain human actions.

## Optional demonstrations

- **File work:** ask Jarvis to revise an explicitly named, indexed Markdown file. The worker prepares a separate revised artifact, preserves the original, and enables **Open result** after verification.
- **Coding:** with an explicitly configured clean Git project, ask “Jarvis, ask Codex to fix the failing test in the configured project.” Jarvis delegates to an isolated worktree while conversation remains available. Ask for job status; when ready, use **Open code** or “open code review” to inspect the verified summary. Changes remain in the separate worktree. Rehearse against a small known task; the live fixture completed in 22.351 seconds.
- **Public research:** ask “Jarvis, what is NVIDIA Nemotron?” The `search_web` tool sends the original request to You, returns source-linked snippets, and feeds them back into the answer. Indexed private content and model-written search queries are excluded from that request.
- **Spotify:** ask for an artist. The typed tool resolves the name, verifies Spotify's canonical identity, plays that artist URI, and confirms matching playback before acknowledging it. Rehearse the complete spoken route on the current build before presenting it.

### Browser page change

**Installed Arc is connected and the live browser demonstration passed.** Its current grant is only `http://localhost/*`; the registered native host writes to `runtime/live.sqlite`. Atlas → Beacon advanced the indexed page from version 1 to 2. A configured Nano conversation made an actual browser search and fresh read, then answered “Beacon” in 3.316 seconds.

For a fresh rehearsal, check the existing bridge and keep the fictional page server running. Follow [setup](setup.md) only when configuring another browser or installation:

```sh
target/debug/jarvis browser-status
python3 examples/browser-demo/serve.py
```

Open `http://localhost:8765`; its existing localhost grant covers that port. Use this sequence:

1. Reload the fictional page to restore **Atlas, Revision 1**.
2. Ask: “Jarvis, search browser tabs for Jarvis demo project priorities, read that page fresh, and tell me the highest-priority project.”
3. Publish the prepared Beacon text into the visible page. Confirm **Beacon, Revision 2**.
4. Ask the same question. Show that the answer now identifies Beacon.

The text box is excluded until its contents become ordinary published page text. The marked private section remains excluded. Fresh reads check document identity and keep browser focus unchanged. The live acceptance used an empty model database and a fixture-only relay, so unrelated tabs and private context were never sent to the model. The complete spoken version still needs a human rehearsal.

### Compound requests

Clear independent “and” clauses are bounded to three and retain separate authorization. “Then” requests stay intact to preserve their dependency. Integration fixtures cover parallel execution, ordered acknowledgments, partial failures, artist-name conjunctions, and one-shot clarification. The full live Wikipedia-plus-Taylor-Swift request passed in **5.747 seconds**: two parallel Nano tool loops opened the canonical Napoleon page and verified the exact track artist playing. A separate switch from paused Metallica to Taylor Swift passed in **6.033 seconds**.

Optional rehearsal: “Jarvis, open the Wikipedia page for Napoleon and play Taylor Swift on Spotify.” Check the opened page and matching playback; acknowledgments retain the original clause order. The live acceptance used text input, so rehearse the spoken route before presenting it.

## Evidence and acceptance

| Check | Observed result | Scope |
|---|---|---|
| Live Nemotron Nano tool-protocol round trip | **1.778 s** | Hosted tool-call protocol; not a human voice latency measurement. |
| Live revised-file worker | **1.556 s** | Fictional Markdown fixture, original preserved, review tampering blocked. |
| Live draft worker | **1.065 s** | Fictional handoff, ready for review, no send or automatic viewer. |
| Live You public research | **3.45 s**, five results | Production Rust adapter, one public Nemotron query, no result-page fetches. |
| Live artist discovery and playback | **3.276 s** | You → canonical Spotify identity → matching Metallica playback; voice and model planning excluded. |
| Generated voice composition | **8.19 s** | Real local keyword/VAD/Whisper → handoff → fixture LM → native speech AIFF; no human microphone or live hosted-model latency claim. |
| Durable task journal | Restart and tamper tests pass | Real SQLite/filesystem fixtures; not yet a full live app-restart rehearsal. |
| Installed Arc fresh browser context | **61 ms** Atlas; **44 ms** Beacon, indexed version **1 → 2** | Actual approved localhost page, native host and live SQLite; input/private fields excluded. |
| Live browser model composition | **3.316 s** | Configured Nano → one real tab search → one real fresh page read → Beacon answer; no human voice or unrelated private context. |
| Live conversational artist playback | **4.531 s** | Model → typed tool → You/Spotify identity → verified Metallica playback; human voice excluded. |
| Live independent compound | **5.747 s** | Two actual Nano tool loops → canonical Napoleon page open and verified Taylor Swift playback; text input, human voice excluded. |
| Live artist switch | **6.033 s** | Paused Metallica → verified Taylor Swift track playing; real native playback adapter. |
| Live conversational coding | **22.351 s**; delegation **1.815 s** | Model → real Codex in isolated worktree → independent offline Rust tests → explicit verified review; original checkout unchanged. |
| Live conversational file edit | **2.098 s** | Model → worker → correct revised artifact → restart restores verified readiness; fictional source unchanged. |
| Live public research composition | **6.958 s** | Configured model → actual You request → answer cites a returned URL; public query only. |

Workspace test counts belong to the final report for the exact committed build. Earlier live project-context and worker checks remain useful component evidence, but the full changed-note → spoken conversation → background result walkthrough still needs a current-build rehearsal.

## Recovery during presentation

- If wake is missed after one repeat, use the menu-bar typed input and label that recovery honestly.
- If context is stale, check the saved file and indexed version; explain any manual reconciliation used.
- Respect provider retry delays rather than submitting duplicates. Verified exact app controls work independently of hosted inference.
- If review is blocked by a changed source or artifact, show the reason and regenerate from current evidence.
- If speech fails, show the actual answer in Details. Optional ElevenLabs output is buffered before playback.

## Submission

The guide calls for a **one-minute submission video** and roughly **three minutes of live judging**. Keep the video to four beats: current priority, visible edit and changed answer, delegated work, explicit review. Label cuts and fictional fixtures. Credit Whisper, sherpa-onnx/Silero, Jev, Nemotron/Crusoe, You, and optional ElevenLabs.

Use the public repository history to identify work built during the event and verify that the video is accessible. The RTX 5090 category is **Best Use of Nemotron**; model choice alone does not establish a winning entry.
