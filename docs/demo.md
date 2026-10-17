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
- **Public research:** ask “Jarvis, what is NVIDIA Nemotron?” The `search_web` tool sends the original request to You, returns source-linked snippets, and feeds them back into the answer. Indexed private content and model-written search queries are excluded from that request.
- **Spotify:** ask for an artist. The typed tool resolves the name, verifies Spotify's canonical identity, plays that artist URI, and confirms matching playback before acknowledging it. Rehearse the complete spoken route on the current build before presenting it.

### Browser page change

The browser transport, private socket, fresh-read tools, and model continuation are tested. **Installed Arc page ingestion is still pending:** the UI automation guard detected active user browsing, so installation was left untouched.

When the browser is available, follow [setup](setup.md) to load the extension and register its exact ID against `runtime/live.sqlite`. Then:

```sh
target/debug/jarvis browser-status
python3 examples/browser-demo/serve.py
```

Approve `http://localhost` in the extension settings and open `http://localhost:8765`. Chrome's host grant covers localhost ports; enter the grant without a port. Ask which project has highest priority, publish the page's edited priority, then ask again. The page is explicitly synthetic: Atlas changes to Beacon. Its input and marked private field are excluded from extraction. The model can search approved tabs and read a fresh, document-checked page without changing browser focus.

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
| Fresh browser tools | Native CLI/socket/model-loop and extension tests pass | Installed browser round trip still requires extension installation and an explicit site grant. |

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
