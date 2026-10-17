// Keep this module independent of Chrome APIs for adversarial protocol tests.
export const PROTOCOL_VERSION = 1;
export const HOST_NAME = "com.jarvis.bridge";
export const MAX_CONTENT_BYTES = 64 * 1024;
export const MAX_QUEUE = 32;
const encoder = new TextEncoder();

export function cappedText(value, maxBytes) {
  if (typeof value !== "string") return "";
  // Slice first to bound work before UTF-8 measurement. Never split a code point.
  const candidate = value.slice(0, maxBytes);
  let bytes = 0;
  let result = "";
  for (const point of candidate) {
    const size = encoder.encode(point).length;
    if (bytes + size > maxBytes) break;
    result += point;
    bytes += size;
  }
  return result;
}

export function httpUrl(value) {
  try {
    const url = new URL(value);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) return null;
    return url;
  } catch { return null; }
}

export function allowedTab(tab) {
  return !!tab && Number.isInteger(tab.id) && Number.isInteger(tab.windowId)
    && tab.incognito === false && httpUrl(tab.url) !== null;
}

export function normalizeOrigin(value) {
  const url = httpUrl(value);
  if (!url) throw new Error("Use a complete http:// or https:// website address without credentials.");
  // Chrome match patterns do not support port-scoped grants. Do not misrepresent one.
  if (url.port) throw new Error("Port-specific site permissions are unsupported. Choose a standard-port website.");
  return `${url.origin}/*`;
}

export function validContentSender(sender, tab, extensionId) {
  const senderUrl = httpUrl(sender?.url);
  const tabUrl = httpUrl(tab?.url);
  return sender?.id === extensionId && sender?.frameId === 0
    && sender?.tab?.id === tab?.id && allowedTab(tab)
    && senderUrl !== null && tabUrl !== null
    && senderUrl.href === tabUrl.href
    && (sender.origin === undefined || sender.origin === tabUrl.origin);
}

export function tabEvent(kind, tab, extra = {}) {
  return {
    kind,
    tab_id: tab.id,
    window_id: tab.windowId,
    url: cappedText(tab.url, 8192),
    title: cappedText(tab.title, 2048),
    observed_at_ms: Date.now(),
    actor: "unknown",
    owner: "unknown",
    source: "browser_metadata",
    trust: "untrusted_page",
    ...extra,
  };
}

// Latest state per tab, bounded globally. No historical page bodies accumulate offline.
export class ObservationQueue {
  constructor(limit = MAX_QUEUE) { this.limit = limit; this.items = new Map(); }
  put(event) {
    // A metadata event must not retain content from an old navigation.
    const previous = this.items.get(event.tab_id);
    if (previous?.url === event.url && previous?.kind !== "tab.closed"
        && event.kind !== "tab.closed" && event.content === undefined) {
      event = { ...event, content: previous.content, document_id: previous.document_id };
    }
    this.items.delete(event.tab_id);
    this.items.set(event.tab_id, event);
    while (this.items.size > this.limit) this.items.delete(this.items.keys().next().value);
  }
  take() {
    const first = this.items.entries().next();
    if (first.done) return null;
    this.items.delete(first.value[0]);
    return first.value[1];
  }
  delete(tabId) { this.items.delete(tabId); }
  clear() { this.items.clear(); }
  get size() { return this.items.size; }
}

// Keep native-host failures useful without persisting arbitrary browser error text.
export function nativeFailure(message) {
  if (typeof message !== "string") return "The browser bridge disconnected. Reconnect to try again.";
  if (/native messaging host not found/i.test(message)) return "Register the Jarvis native host, then reconnect.";
  if (/access .* native messaging host is forbidden/i.test(message)) return "The registered extension ID does not match. Check the native host registration.";
  return "The browser bridge disconnected. Reconnect to try again.";
}
