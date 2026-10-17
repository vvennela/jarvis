# Jarvis native awareness panel

A Rust AppKit prototype: one nonactivating, floating `NSPanel` at the top right of the main display, backed by the real bounded filesystem collector and SQLite context store. No WebView, Swift, screenshots, microphone capture, speech, model calls, or fabricated activity.

## Run

From the repository root:

```sh
cargo run -p jarvis-panel -- --root /absolute/path/to/approved-demo-folder --db /absolute/path/to/context.sqlite
```

Both paths are required. Start with a small fixture directory containing `.txt` / `.md` files. Nothing defaults to Documents, Downloads, home, or all applications. Root symlinks and non-directories are rejected before database creation. Scope exclusions and bounds are the core collector's rules.

- **Starting…**: observer setup is underway; no readiness claim yet.
- **● Awareness on — files**: the worker reported successful observer subscriptions and initial reconciliation.
- **Pausing…**: pause requested, waiting for actual collector cancellation.
- **Awareness paused**: the observer has returned and no collection is running. The SQLite connection may remain open.
- **Observer blocked**: initialization or collection failed. No success animation; restart after resolving the root/database issue.
- **Quit**: requests cancellation, keeps the UI responsive while the observer exits, then joins its worker and hides the panel.

The panel explicitly says **Microphone off · models idle**. It uses neither `activateIgnoringOtherApps` nor `makeKeyAndOrderFront`, so showing it does not intentionally take keyboard focus. Buttons are click-operated on a nonactivating panel; no text-entry UI is included yet. Display placement uses the current main display's visible frame at launch; automatic repositioning after display rearrangement is not implemented.

## Lifecycle and memory

A single worker owns the SQLite store. It calls `watch_root_controlled` for long event-driven sessions, preserving metadata fingerprints instead of restarting/re-reading every second. Pause/quit use atomics; the collector checks cancellation at loop boundaries (idle checks approximately every 150 ms). An in-progress bounded file scan or filesystem I/O is not forcefully interrupted, so pause is acknowledged only after actual return. Resume establishes subscriptions and reconciles again before showing active.

The worker exposes one mutex-protected latest-state snapshot and atomic intent flags: no growing UI event queue, raw file content, or path history in the panel. Dropping the worker requests cancellation and joins it. One-hour collector session expiry renews observation; the collector's initial scans close subscription gaps. Native objects remain on the process main thread and never cross into the worker.

## Smoke verification

On a Mac with an interactive desktop:

```sh
cargo run -p jarvis-panel -- --root /absolute/path/to/demo-fixtures --db /absolute/path/to/demo-state.sqlite --smoke-test
```

This briefly shows the actual native panel. It waits for real observer readiness, invokes the same pause handler as the button, waits for paused acknowledgment, resumes, waits for readiness, and invokes quit. A JSON summary reports whether those states were observed and confirms `microphone_active: false`. It exits nonzero if the lifecycle did not complete. Eight seconds triggers a cancellation fallback, but OS filesystem I/O itself has no hard wall-clock guarantee.

Automated worker/CLI tests use temporary directories only. The real AppKit smoke test is separate from headless Linux CI. A successful smoke test verifies native instantiation and lifecycle—not subjective layout quality, every keyboard-focus edge case, microphone interaction, browser collection, or agent execution.

## Unsafe boundary

The root Jarvis crate and this crate's worker library forbid unsafe code. The macOS executable isolates minimal Objective-C interop in `src/macos.rs`, with documented safety requirements:

- `define_class!` declares a main-thread-only NSObject subclass with correctly typed button action methods.
- NSObject initialization uses its known `init` selector after initialized ivars.
- Button target/action selectors match that retained class, whose lifetime exceeds the buttons.
- `setReleasedWhenClosed(false)` makes Rust `Retained` ownership responsible for window lifetime.
- Reading Apple's immutable `NSDefaultRunLoopMode` global is confined to the main-thread event loop.

All other AppKit calls use typed safe bindings. Dependency APIs were checked against objc2's maintained examples and generated AppKit bindings. This is a native prototype, not a signed/notarized application bundle or launch-at-login service.
