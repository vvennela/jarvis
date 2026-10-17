# Jarvis browser awareness

A Chrome extension that keeps Jarvis up to date with your approved tabs and page text. Changes flow into the local Rust context store through Chrome Native Messaging.

## Connect

1. Open `chrome://extensions`, enable **Developer mode**, and **Load unpacked** this directory.
2. Copy the extension ID, then run from the project root:

   ```sh
   cargo build --release
   mkdir -p runtime
   ./target/release/jarvis install-browser-host --extension-id YOUR_EXTENSION_ID --db "$PWD/runtime/context.sqlite"
   ```

3. Reload the extension. Open its settings to approve websites and start awareness.

Use the same database for the panel, browser host, and conversation commands.

## Controls

- **ON:** native bridge connected.
- **OFF:** collection paused.
- Settings let you approve sites and pause or resume collection.

Page changes are debounced and deduplicated. The collector captures rendered text, applies field and visibility filters, and keeps bounded queues. Structured observations carry tab identity, source, and freshness into the local search index.

## Test

```sh
node --test extension/tests/*.test.js
```

[Setup details](../docs/setup.md) · [Architecture](../docs/architecture.md)
