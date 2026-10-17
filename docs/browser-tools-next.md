# Fresh browser tools — implementation contract

**Status: pending implementation.** The current extension publishes approved browser context to the index. This increment adds fresh reads through that extension session while the user keeps working.

## Tools and behavior

| Tool | Arguments | Verified result |
|---|---|---|
| `browser_search_tabs` | `query`: 1–256 UTF-8 bytes | Up to 16 currently open, permitted tabs whose title or URL matches the literal query; each result includes a session-scoped resource ID, tab ID, document ID, URL and title. |
| `browser_read_tab` | `resource_id`, `expected_document_id` from discovery | Up to 64 KiB of freshly extracted visible text from that exact document, with a truncation flag and provenance. The model-facing excerpt is capped at 8 KiB. |

Preserve focus, selection, existing extraction filters, host grants and private-window exclusions. Changed documents trigger rediscovery or clarification. Page content remains evidence, never authorization.

## Request path and identity

1. The runtime validates typed arguments and retains the originating user request ID. Reads accept only session-scoped handles discovered for that request.
2. Connect the panel to the Chrome-launched host through a Unix socket in a user-owned `0700` application-support directory. Require `0600` socket permissions, same-user peers and no symlink endpoints. Register after extension-origin validation and handshake.
3. Send `browser.request` over the native port: `protocol_version`, `session_id`, `request_id`, `operation`, `arguments`. Return `browser.result` with matching IDs, typed status and bounded data. Negotiate fresh-read support; older bridges retain observation support.
4. Query live tabs and permissions. Before reading, check tab, URL, document ID and host grant; use `chrome.tabs.sendMessage` targeting that document. Validate reply extension/tab/frame/origin, then recheck document identity and permission before publication.

Identity is `(session_id, tab_id, document_id)`. Tag page-derived fields `untrusted_page`, assign host receipt time and retain actor `unknown`. Only validated fresh results update the index; failed reads preserve cached timestamps.

## Bounds and lifecycle

Allow one active command, four queued commands, 512 KiB frames and a five-second end-to-end deadline. Multiplex observations and commands so idle stdin cannot block requests. A per-tab read lease tracks expected identity without locking the browser. Navigation, permission removal, pause, disconnect or cancellation invalidates it. Drop late/duplicate results; reconnect invalidates old handles. Statuses: `ok`, `stale_document`, `permission_required`, `unavailable`, `cancelled`, `timeout`.

## Acceptance

- Drive the real native-host process, framed transport and extension handlers against controlled pages: discover a permitted tab, read changed text, and verify that focus stays unchanged.
- Navigate during extraction, reuse a tab ID after reconnect, revoke a host grant, pause collection and cancel a request; each case rejects late or mismatched content.
- Exercise private tabs, unauthorized origins, forged document IDs and page-text instructions; verify zero additional capabilities or unintended reads.
- Exercise output caps, queue saturation, duplicate responses, disconnect and a stalled peer; verify bounded memory, deadline completion and recovery after reconnect.
- Validate an unpacked-extension round trip in Chrome on an explicitly approved fixture site before advertising live integration. Process/JavaScript fixtures establish protocol behavior separately.

API references: [Chrome Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging), [document-targeted tab messaging](https://developer.chrome.com/docs/extensions/reference/api/tabs#method-sendMessage).
