# Native foreground awareness

Collects the foreground macOS app and permitted focused-window metadata into Jarvis's context database. App identity remains available when Accessibility permission is missing. The CLI prints collection counts and permission status.

```sh
cargo run -p jarvis-awareness -- --db runtime/context.sqlite --seconds 30
```

Collection uses native NSWorkspace and Accessibility APIs, with one current-focus resource and a 250 ms polling interval. Browser context comes through the approved extension. See [DESIGN.md](DESIGN.md) for implementation details and verification.
