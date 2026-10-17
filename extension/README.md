# Jarvis browser awareness

A Chromium extension that keeps Jarvis up to date with your approved tabs and page text. Changes flow into the local Rust context store through Chrome Native Messaging.

## Connect

1. In Chrome or Arc, open `chrome://extensions`, enable **Developer mode**, and **Load unpacked** this directory.
2. Copy the extension ID shown in the extension settings, then run from the project root:

   ```sh
   cargo build -p jarvis --bin jarvis
   mkdir -p runtime
   ./target/debug/jarvis install-browser-host --extension-id YOUR_EXTENSION_ID --db "$PWD/runtime/context.sqlite"
   ```

3. Reload the extension. Open its settings to approve websites and start awareness. Use **Reconnect and refresh approved tabs** to collect current page text.

Use the same database for the panel, browser host, and conversation commands. For the current live app, pass `--db "$PWD/runtime/live.sqlite"`.

## Controls

- **ON:** native bridge connected.
- **OFF:** collection paused.
- Settings let you approve sites and pause or resume collection.

Page changes are debounced and deduplicated. The collector captures rendered text, applies field and visibility filters, and keeps bounded queues. Structured observations carry tab identity, source, and freshness into the local search index.

## Fresh reads

`browser_search_tabs` discovers approved tabs. `browser_read_tab` reads the selected document through the existing browser session, with identity and permission checks before and after extraction. The private native bridge preserves focus and keeps page text marked as untrusted evidence.

## Test

```sh
node --test extension/tests/*.test.js
```

[Setup details](../docs/setup.md) · [Architecture](../docs/architecture.md)
