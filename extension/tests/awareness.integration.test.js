import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { webcrypto } from "node:crypto";
import * as protocol from "../protocol.js";

// Executes production scripts with simulated Chrome/DOM/native transports.
// These tests do not claim installed-browser or Rust native-host end-to-end coverage.
const backgroundSource = (await readFile(new URL("../background.js", import.meta.url), "utf8"))
  .replace(/^import \{[\s\S]*?\} from "\.\/protocol\.js";\n/, "");
const contentSource = await readFile(new URL("../content.js", import.meta.url), "utf8");
const tick = async () => { for (let i = 0; i < 8; i++) await new Promise(setImmediate); };

class Event {
  listeners = [];
  addListener(fn) { this.listeners.push(fn); }
  emit(...args) { return this.listeners.map(fn => fn(...args)); }
}

function timers() {
  let next = 1;
  const pending = new Map();
  return {
    setTimeout(fn, ms) { const id = next++; pending.set(id, { fn, ms }); return id; },
    clearTimeout(id) { pending.delete(id); },
    async fire(ms) {
      for (const [id, entry] of [...pending]) {
        if (entry.ms === ms && pending.delete(id)) await entry.fn();
      }
      await tick();
    },
    pending,
  };
}

const tab = (id = 1, extra = {}) => ({ id, windowId: 8, incognito: false,
  url: `https://example.com/page/${id}`, title: `Tab ${id}`, ...extra });

async function background({ tabs = [tab()], approved = true, paused = false } = {}) {
  const clock = timers();
  const ports = [];
  const tabMap = new Map(tabs.map(item => [item.id, item]));
  const injections = [];
  const controls = [];
  const storage = {};
  let permission = approved;
  let checkPermission = async () => permission;
  const chrome = {
    runtime: {
      id: "jarvis-extension", onMessage: new Event(),
      connectNative() {
        const port = { onMessage: new Event(), onDisconnect: new Event(), messages: [], disconnected: false,
          postMessage(message) { if (this.disconnected) throw new Error("disconnected"); this.messages.push(structuredClone(message)); },
          disconnect() { this.disconnected = true; },
        };
        ports.push(port); return port;
      },
      openOptionsPage: async () => {},
    },
    storage: { local: { get: async () => ({ paused }), set: async value => Object.assign(storage, value) }, onChanged: new Event() },
    action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {}, setTitle: async () => {}, onClicked: new Event() },
    permissions: { contains: async args => checkPermission(args), onAdded: new Event(), onRemoved: new Event() },
    tabs: { get: async id => { if (!tabMap.has(id)) throw new Error("missing tab"); return structuredClone(tabMap.get(id)); },
      query: async () => [...tabMap.values()].map(item => structuredClone(item)),
      sendMessage: async (...args) => { controls.push(args); return { ok: true }; },
      onActivated: new Event(), onUpdated: new Event(), onRemoved: new Event(),
    },
    scripting: { executeScript: async args => { injections.push(args); } },
    webNavigation: { onCommitted: new Event(), onHistoryStateUpdated: new Event() },
    alarms: { create() {}, clear: async () => true, onAlarm: new Event() },
  };
  vm.runInNewContext(backgroundSource, { ...protocol, chrome, crypto: webcrypto, URL, TextEncoder,
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout, console });
  await tick();
  return {
    chrome, ports, tabMap, injections, controls, storage, clock,
    permission(value) { permission = value; },
    permissionChecker(fn) { checkPermission = fn; },
    async handshake(extra = {}) {
      const port = ports.at(-1);
      const hello = port.messages.find(message => message.type === "hello");
      port.onMessage.emit({ type: "hello_ack", protocol_version: 1, client_nonce: hello.client_nonce,
        session_id: "session-fixture-123456789", ...extra });
      await tick(); return port;
    },
    observations() { return ports.flatMap(port => port.messages.filter(message => message.type === "observation")); },
    async content(message = {}, sender = {}) {
      return new Promise(resolve => chrome.runtime.onMessage.listeners[0](
        { type: "jarvis.content", content: "public document", ...message },
        { id: chrome.runtime.id, frameId: 0, tab: { id: 1 }, url: tabMap.get(1)?.url,
          origin: "https://example.com", documentId: "document-current", ...sender }, resolve));
    },
    async pause() { chrome.storage.onChanged.emit({ paused: { newValue: true } }, "local"); await tick(); },
  };
}

test("no observations precede native handshake; nonce/version/session mismatches disconnect", async () => {
  for (const invalid of [ { client_nonce: "wrong" }, { protocol_version: 999 }, { session_id: "short" }, { type: "execute" } ]) {
    const harness = await background();
    harness.chrome.tabs.onActivated.emit({ tabId: 1 }); await tick();
    assert.equal(harness.observations().length, 0);
    const port = await harness.handshake(invalid);
    assert.equal(port.disconnected, true);
    assert.equal(harness.observations().length, 0);
  }
});

test("valid native observations have monotonic sequence and untrusted unknown attribution", async () => {
  const harness = await background();
  const port = await harness.handshake();
  assert.equal((await harness.content()).accepted, true);
  harness.chrome.tabs.onActivated.emit({ tabId: 1 }); await tick();
  const observations = harness.observations();
  assert.ok(observations.length >= 3);
  assert.deepEqual(observations.map(message => message.sequence), observations.map((_, i) => i + 1));
  for (const { event, session_id } of observations) {
    assert.equal(session_id, "session-fixture-123456789");
    assert.equal(event.actor, "unknown");
    assert.equal(event.trust, "untrusted_page");
  }
  const before = observations.length;
  port.onMessage.emit({ type: "execute", action: "send_message" }); await tick();
  assert.equal(harness.observations().length, before);
});

test("private tabs, unapproved sites and non-web URLs never inject or emit", async () => {
  for (const options of [ { approved: false }, { tabs: [tab(1, { incognito: true })] },
    { tabs: [tab(1, { url: "chrome://settings" })] }, { paused: true } ]) {
    const harness = await background(options); await harness.handshake();
    harness.chrome.tabs.onActivated.emit({ tabId: 1 }); await tick();
    assert.equal((await harness.content()).accepted, false);
    assert.equal(harness.injections.length, 0);
    assert.equal(harness.observations().length, 0);
  }
});

test("forged sender IDs, frames, URLs, origins, tabs and old documents cannot supply content", async () => {
  const harness = await background(); await harness.handshake();
  harness.chrome.webNavigation.onCommitted.emit({ tabId: 1, frameId: 0, documentId: "document-current", timeStamp: 1 });
  await tick();
  const before = harness.observations().length;
  for (const sender of [{ id: "other-extension" }, { frameId: 1 }, { url: "https://example.com/old" },
    { origin: "https://evil.example" }, { tab: { id: 99 } }, { documentId: "previous-document" }, { documentId: undefined }]) {
    assert.equal((await harness.content({}, sender)).accepted, false, JSON.stringify(sender));
  }
  assert.equal(harness.observations().length, before);
  assert.equal((await harness.content({ tab_id: 99, actor: "user", trust: "trusted", url: "https://evil.example" })).accepted, true);
  const event = harness.observations().at(-1).event;
  assert.equal(event.tab_id, 1); assert.equal(event.actor, "unknown");
  assert.equal(event.url, tab().url); assert.equal(event.trust, "untrusted_page");
});

test("pause and revocation clear offline content and stop already injected scripts", async () => {
  for (const revoke of [false, true]) {
    const harness = await background();
    assert.equal((await harness.content({ content: "secret queued content" })).accepted, true);
    if (revoke) { harness.permission(false); harness.chrome.permissions.onRemoved.emit({ origins: ["https://example.com/*"] }); await tick(); }
    else await harness.pause();
    await harness.handshake();
    assert.equal(harness.observations().length, 0);
    assert.ok(harness.controls.some(([, message]) => message.type === "jarvis.pause"));
    assert.equal((await harness.content()).accepted, false);
    assert.ok(!JSON.stringify(harness.storage).includes("secret queued content"));
  }
});

test("pause invalidates content whose permission check was already in flight", async () => {
  const harness = await background(); await harness.handshake();
  let release;
  harness.permissionChecker(() => new Promise(resolve => { release = resolve; }));
  const pending = harness.content({ content: "must not escape after pause" }); await tick();
  assert.equal(typeof release, "function");
  await harness.pause(); release(true);
  assert.equal((await pending).accepted, false);
  assert.ok(!JSON.stringify(harness.observations()).includes("must not escape"));
});

test("disconnect queues latest bounded state and reconnect starts a fresh session sequence", async () => {
  const harness = await background({ tabs: [] });
  for (let id = 1; id <= 50; id++) {
    harness.tabMap.set(id, tab(id));
    harness.chrome.tabs.onUpdated.emit(id, { title: `Tab ${id}` });
  }
  await tick();
  // Avoid reconcile producing another 50 observations; queued snapshots remain.
  harness.tabMap.clear();
  const first = await harness.handshake();
  assert.equal(harness.observations().length, 32);
  assert.deepEqual(harness.observations().map(message => message.event.tab_id), Array.from({ length: 32 }, (_, i) => i + 19));
  first.onDisconnect.emit(); await tick();
  harness.tabMap.set(1, tab()); harness.chrome.tabs.onActivated.emit({ tabId: 1 }); await tick();
  harness.chrome.alarms.onAlarm.emit({ name: "jarvis-reconnect" }); await tick();
  const second = await harness.handshake({ session_id: "session-second-123456789" });
  assert.equal(second.messages.find(message => message.type === "observation").sequence, 1);
  assert.equal(second.messages.find(message => message.type === "observation").session_id, "session-second-123456789");
});

test("navigation never carries a previous document's queued content into the next page", () => {
  const queue = new protocol.ObservationQueue(2);
  queue.put({ kind: "document.changed", tab_id: 1, url: "https://example.com/old", content: "old secret" });
  queue.put({ kind: "tab.updated", tab_id: 1, url: "https://example.com/new" });
  assert.equal(queue.take().content, undefined);
  queue.put({ kind: "document.changed", tab_id: 1, url: "https://example.com/new", content: "new" });
  queue.put({ kind: "tab.closed", tab_id: 1, url: "https://example.com/new" });
  assert.equal(queue.take().content, undefined);
});

test("UTF-8 byte caps and origin normalization cannot create overbroad explicit port promises", () => {
  const capped = protocol.cappedText("🦀".repeat(50000), 65536);
  assert.ok(Buffer.byteLength(capped) <= 65536);
  assert.ok(!capped.includes("\ufffd"));
  assert.throws(() => protocol.normalizeOrigin("https://example.com:8443/path"));
  assert.throws(() => protocol.normalizeOrigin("https://user:pass@example.com"));
  assert.equal(protocol.normalizeOrigin("https://example.com/path?q=hello"), "https://example.com/*");
});

function domElement({ tag = "p", attrs = {}, parent = null, visible = true } = {}) {
  const element = { tag, attrs, parent, visible,
    closest(selector) {
      for (let current = this; current; current = current.parent) {
        if (selector.split(",").some(part => {
          if (/^[a-z]+$/.test(part)) return part === current.tag;
          if (part.startsWith("[contenteditable]")) return "contenteditable" in current.attrs && current.attrs.contenteditable !== "false";
          const match = part.match(/^\[([\w-]+)(?:='([^']*)')?\]$/);
          return match && match[1] in current.attrs && (match[2] === undefined || current.attrs[match[1]] === match[2]);
        })) return current;
      }
      return null;
    },
    getClientRects() { return this.visible ? [{}] : []; },
  };
  return element;
}

function contentHarness(nodes, { accepted = true } = {}) {
  const clock = timers();
  const sent = [];
  const onMessage = new Event();
  let mutation;
  const observers = [];
  class Observer {
    constructor(callback) { mutation = callback; this.disconnected = false; observers.push(this); }
    observe() {} disconnect() { this.disconnected = true; }
  }
  const chrome = { runtime: { id: "jarvis-extension", onMessage, sendMessage: async message => {
    sent.push(structuredClone(message));
    if (accepted === "throw") throw new Error("extension unloaded");
    return { accepted };
  } } };
  const context = vm.createContext({ chrome, TextEncoder, MutationObserver: Observer, NodeFilter: { SHOW_TEXT: 4 },
    getComputedStyle: element => ({ visibility: element.visible ? "visible" : "hidden", display: "block" }),
    document: { body: {}, documentElement: {}, createTreeWalker() { let i = 0; return { nextNode: () => nodes[i++] ?? null }; } },
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
  });
  vm.runInContext(contentSource, context);
  return { clock, sent, observers, mutate: () => mutation(),
    control(type, id = "jarvis-extension") { onMessage.emit({ type }, { id }, () => {}); },
    reinject() { vm.runInContext(contentSource, context); },
  };
}

test("content extraction excludes fields, editable/private/hidden subtrees and caps bytes", async () => {
  const nodes = [{ textContent: " Public   specification \n", parentElement: domElement() }];
  for (const options of [{ tag: "input", attrs: { type: "password" } }, { tag: "textarea" }, { attrs: { contenteditable: "true" } },
    { attrs: { role: "textbox" } }, { attrs: { "data-jarvis-private": "" } }, { attrs: { "aria-hidden": "true" } },
    { attrs: { hidden: "" } }, { tag: "script" }]) {
    nodes.push({ textContent: "SECRET", parentElement: domElement({ parent: domElement(options) }) });
  }
  // CSS-hidden direct element too; this minimal DOM fixture does not implement CSS inheritance.
  nodes.push({ textContent: "SECRET", parentElement: domElement({ visible: false }) });
  const harness = contentHarness(nodes); await harness.clock.fire(1000);
  assert.equal(harness.sent.length, 1);
  assert.equal(harness.sent[0].content, "Public specification");
  nodes.push({ textContent: "🦀".repeat(50000), parentElement: domElement() });
  harness.mutate(); await harness.clock.fire(1000);
  assert.ok(Buffer.byteLength(harness.sent.at(-1).content) <= 65536);
});

test("unchanged DOM churn is not resent; pause, resume and failed delivery are observable", async () => {
  const nodes = [{ textContent: "same", parentElement: domElement() }];
  const harness = contentHarness(nodes); await harness.clock.fire(1000);
  for (let i = 0; i < 100; i++) harness.mutate();
  assert.equal(harness.clock.pending.size, 1);
  await harness.clock.fire(1000); assert.equal(harness.sent.length, 1);
  harness.control("jarvis.pause", "evil-extension");
  nodes[0].textContent = "changed"; harness.mutate(); await harness.clock.fire(1000);
  assert.equal(harness.sent.length, 2);
  harness.control("jarvis.pause"); nodes[0].textContent = "paused change";
  harness.mutate(); await harness.clock.fire(1000); assert.equal(harness.sent.length, 2);
  harness.control("jarvis.resume"); await harness.clock.fire(1000);
  assert.equal(harness.sent.at(-1).content, "paused change");
  harness.reinject(); assert.equal(harness.observers.length, 1);
  const rejected = contentHarness(nodes, { accepted: false });
  await rejected.clock.fire(1000); rejected.mutate(); await rejected.clock.fire(1000);
  assert.equal(rejected.sent.length, 2, "unaccepted observations must remain eligible for delivery");
  const unloaded = contentHarness(nodes, { accepted: "throw" }); await unloaded.clock.fire(1000);
  assert.equal(unloaded.observers[0].disconnected, true);
});
