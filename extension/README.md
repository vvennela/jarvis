# Jarvis browser awareness extension

Unpacked Chrome Manifest V3 source. This adapter collects structured, explicitly permitted browser evidence; it does not execute actions, capture screenshots, call models, or implement the native host. Live browser behavior must be tested separately from simulated Chrome integration tests.

## Install and permissions

1. Build/install the Rust Native Messaging host described by the main project. Its registered name is `com.jarvis.bridge`; restrict its `allowed_origins` to the actual installed extension ID.
2. Open `chrome://extensions`, enable Developer mode, choose **Load unpacked**, and select this directory.
3. Click Jarvis's extension action to open settings. Enter each approved website and grant Chrome's permission prompt. No host permissions are granted by default. No content scripts are injected before a site is approved.
4. Toolbar badge **ON** means native bridge connected; **!** means disconnected; **OFF** means collection paused. This is browser awareness, not microphone status. Settings shows explicit connection state.

Chrome requires `tabs` for URL/title metadata and `webNavigation` for document navigation identity. Metadata sent to Jarvis is also restricted to granted sites. Granting an origin includes all paths on that origin. Port-specific grants are rejected because Chrome match patterns cannot promise port isolation. Private windows are disabled by the manifest and rejected again by the worker.

## Collection boundary

Only top-frame HTTP(S) documents are accepted. Content is rendered text capped at **65,536 UTF-8 bytes**, without selection or form values. Input, textarea, select, button, contenteditable, textbox/searchbox, hidden, and explicitly `data-jarvis-private` subtrees are excluded. Text visibly rendered outside those fields may still be sensitive: choose sites carefully. Full URLs can contain private query parameters. CSS visibility checks and DOM node/byte budgets limit collection; shadow DOM, iframe text, canvas, PDFs, and suspended pages are unsupported, not evidence that those surfaces are empty. The host must represent freshness and incomplete coverage honestly.

DOM mutations schedule a debounced extraction; unchanged normalized text is not resent. Browser events use Chrome-derived tab/window IDs, URL and title. Content messages cannot choose those fields: the worker checks the sender extension, main frame, exact current URL/origin, current document ID when known, and current permissions. Navigation and permission changes invalidate pending state. Page scripts have no `window.postMessage` or external-message bridge.

A bounded in-memory queue coalesces newest observations per tab, up to 32 tabs. Cached tab metadata and document IDs each cap at 256. Queue loss/worker suspension is possible; this is a latest-state feed, not an audit log. Reconnection re-observes open approved tabs. Delivery is not acknowledged or exactly-once. Native connection badges show transport handshake state, not durable host persistence.

All events use `actor: "unknown"` and `owner: "unknown"`. Tab activation is not proof of human action. A future owned-tab API must mark agent tabs from a trusted runner registry; no current path pretends to identify user versus agent activity. Host action-ledger correlation remains necessary.

Pause/revoke clears pending observations and stops injected collectors. Already delivered host history is not deleted. Retention, history deletion, and provider-context exclusions belong to the Rust service. The extension never stores raw browsing content in `chrome.storage`.

## Native Messaging protocol v1

Native Messaging supplies the transport's little-endian 32-bit length prefix around UTF-8 JSON. The extension does not manually frame messages.

Client hello:

```json
{"type":"hello","protocol_version":1,"client_nonce":"random-UUID"}
```

Host acknowledgment within 5 seconds:

```json
{"type":"hello_ack","protocol_version":1,"client_nonce":"same-random-UUID","session_id":"host-generated-opaque-nonce-at-least-16-characters"}
```

The host's session ID must be 16–128 characters. Wrong version, nonce, message type, or session ID disconnects. After handshake, no host commands are accepted; this observation-only adapter cannot send/pay/book/delete or run page scripts on host instruction. Reconnect uses 1/2/4/8/16/30-second backoff plus a Chrome alarm fallback if the worker suspends.

Observation:

```json
{
  "type": "observation",
  "protocol_version": 1,
  "session_id": "host-generated-session-nonce",
  "sequence": 1,
  "event": {
    "kind": "document.changed",
    "tab_id": 42,
    "window_id": 3,
    "url": "https://example.com/spec",
    "title": "Example specification",
    "observed_at_ms": 1792256400000,
    "actor": "unknown",
    "owner": "unknown",
    "document_id": "Chrome-document-UUID-if-available",
    "source": "browser_content_script",
    "trust": "untrusted_page",
    "content": "Normalized rendered page text"
  }
}
```

Kinds: `tab.activated`, `tab.updated`, `tab.closed`, `document.changed`. `content`, `document_id`, `navigation_id`, `source`, and `trust` are optional; all page titles, URLs and content are untrusted regardless of whether the optional fields appear. Navigation ID, when present, is `<tab_id>:<Chrome-navigation-timestamp>`. Metadata limits: URL 8,192 UTF-8 bytes; title 2,048; document ID 128. Content JSON escaping may expand wire size beyond 64 KiB; host frame limit should be bounded but permit this (e.g. 512 KiB).

Sequence strictly increases per handshake, restarting at 1 for a new session. Host rejects unknown sessions, duplicates/out-of-order messages and malformed/oversized input. Session handshake scopes transport; it is not a substitute for Native Messaging host allowed-origins restrictions and authenticated/owner-only native-host-to-service transport.

## Testing

`protocol.js` exports pure validation/queue helpers. `background.js` uses standard Chrome APIs and can be imported under a simulated Chrome harness; content and options scripts can be exercised with DOM fixtures. Independent adversarial integration tests should cover forged senders, old navigation messages, revoked permissions, private tabs, handshake mismatch, disconnect/reconnect, bounded offline queue, and page-text exclusions. Passing such tests does not certify a loaded Chrome/native-host session.
