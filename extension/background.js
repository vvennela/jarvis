import {
  PROTOCOL_VERSION, HOST_NAME, MAX_CONTENT_BYTES, ObservationQueue,
  cappedText, httpUrl, allowedTab, validContentSender, tabEvent, nativeFailure,
} from "./protocol.js";

const queue = new ObservationQueue();
const knownTabs = new Map();
const documents = new Map();
let port = null;
let sessionId = null;
let clientNonce = null;
let sequence = 0;
let paused = true; // Fail closed until persisted settings load.
let generation = 0;
let retryMs = 1000;
let retryTimer = null;
let handshakeTimer = null;
let injecting = new Set();
const ready = chrome.storage.local.get("paused").then(state => { paused = state.paused === true; });

async function status(value) {
  await chrome.storage.local.set({ bridgeStatus: value });
  await chrome.action.setBadgeText({ text: paused ? "OFF" : value === "connected" ? "ON" : "!" });
  await chrome.action.setBadgeBackgroundColor({ color: paused ? "#777777" : value === "connected" ? "#257d50" : "#a16400" });
  await chrome.action.setTitle({ title: `Jarvis: ${paused ? "awareness paused" : value} — click to configure` });
}

async function permitted(tab) {
  if (paused || !allowedTab(tab)) return false;
  return chrome.permissions.contains({ origins: [`${httpUrl(tab.url).origin}/*`] });
}

function flush() {
  if (!port || !sessionId || paused) return;
  let event;
  while ((event = queue.take())) {
    try {
      port.postMessage({ type: "observation", protocol_version: PROTOCOL_VERSION,
        session_id: sessionId, sequence: ++sequence, event });
    } catch {
      queue.put(event);
      disconnect();
      return;
    }
  }
}

function emit(event) { queue.put(event); flush(); }

function disconnect(problem = "") {
  clearTimeout(handshakeTimer);
  handshakeTimer = null;
  const old = port;
  port = null;
  sessionId = null;
  clientNonce = null;
  if (old) { try { old.disconnect(); } catch { /* already disconnected */ } }
  chrome.storage.local.set({ bridgeProblem: problem });
  status("disconnected");
  if (retryTimer === null) {
    retryTimer = setTimeout(() => { retryTimer = null; connect(); }, retryMs);
    retryMs = Math.min(retryMs * 2, 30000);
    // Durable fallback when Chrome suspends the service worker before setTimeout.
    chrome.alarms.create("jarvis-reconnect", { delayInMinutes: 0.5 });
  }
}

async function connect() {
  await ready;
  if (port) return;
  try {
    const current = chrome.runtime.connectNative(HOST_NAME);
    port = current;
    clientNonce = crypto.randomUUID();
    sequence = 0;
    status("connecting");
    current.onMessage.addListener(message => {
      if (port !== current) return;
      if (sessionId !== null) {
        if (message?.type === "browser.request") handleBrowserRequest(message, current);
        return;
      }
      if (message?.type !== "hello_ack" || message.protocol_version !== PROTOCOL_VERSION
          || message.client_nonce !== clientNonce || typeof message.session_id !== "string"
          || message.session_id.length < 16 || message.session_id.length > 128) {
        disconnect();
        return;
      }
      clearTimeout(handshakeTimer);
      handshakeTimer = null;
      sessionId = message.session_id;
      retryMs = 1000;
      chrome.alarms.clear("jarvis-reconnect");
      chrome.storage.local.set({ bridgeProblem: "" });
      status("connected");
      flush();
      reconcile(); // Re-observe open permitted tabs after connection gaps.
    });
    current.onDisconnect.addListener(() => {
      // Reading lastError prevents noisy logs; never log page payloads.
      const problem = nativeFailure(chrome.runtime.lastError?.message);
      if (port === current) disconnect(problem);
    });
    current.postMessage({ type: "hello", protocol_version: PROTOCOL_VERSION, client_nonce: clientNonce, capabilities: ["fresh_reads"] });
    handshakeTimer = setTimeout(() => { if (port === current && !sessionId) disconnect(); }, 5000);
  } catch { disconnect(); }
}

async function inject(tabId) {
  if (injecting.has(tabId)) return;
  const epoch = generation;
  injecting.add(tabId);
  try {
    const tab = await chrome.tabs.get(tabId);
    if (!await permitted(tab) || epoch !== generation) return;
    await chrome.scripting.executeScript({ target: { tabId, allFrames: false }, files: ["content.js"] });
    const current = await chrome.tabs.get(tabId);
    const resume = epoch === generation && await permitted(current);
    await chrome.tabs.sendMessage(tabId, { type: resume ? "jarvis.resume" : "jarvis.pause" }, { frameId: 0 });
  } catch { /* Closed, suspended, restricted, or permission changed; remain unknown. */ }
  finally { injecting.delete(tabId); }
}

async function observe(tabId, kind, extra = {}) {
  await ready;
  const epoch = generation;
  try {
    const tab = await chrome.tabs.get(tabId);
    if (!await permitted(tab) || epoch !== generation) {
      queue.delete(tabId);
      knownTabs.delete(tabId);
      return;
    }
    knownTabs.set(tabId, tabEvent(kind, tab, extra));
    // Metadata cache bounded independently of the offline outbound queue.
    if (knownTabs.size > 256) knownTabs.delete(knownTabs.keys().next().value);
    emit(tabEvent(kind, tab, extra));
    await inject(tabId);
  } catch { /* Tab vanished during observation. */ }
}

async function reconcile() {
  await ready;
  if (paused) return;
  const tabs = await chrome.tabs.query({});
  // Sequential injection bounds pressure on Chrome when many tabs are open.
  for (const tab of tabs) await observe(tab.id, "tab.updated");
}

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message?.type === "jarvis.refresh") {
    if (sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL("options.html")) {
      reply({ accepted: false });
      return;
    }
    (async () => {
      await ready;
      clearTimeout(retryTimer);
      retryTimer = null;
      await connect();
      await reconcile();
      return { accepted: true, paused };
    })().then(reply, () => reply({ accepted: false }));
    return true;
  }
  if (message?.type !== "jarvis.content") return;
  (async () => {
    await ready;
    const epoch = generation;
    if (typeof message.content !== "string" || message.content.length > MAX_CONTENT_BYTES
        || sender.tab?.id === undefined) return false;
    const tab = await chrome.tabs.get(sender.tab.id);
    if (!validContentSender(sender, tab, chrome.runtime.id) || !await permitted(tab)
        || epoch !== generation) return false;
    const latestDocument = documents.get(tab.id);
    if (latestDocument && latestDocument !== sender.documentId) return false;
    const content = cappedText(message.content, MAX_CONTENT_BYTES);
    emit(tabEvent("document.changed", tab, {
      content,
      document_id: typeof sender.documentId === "string" ? cappedText(sender.documentId, 128) : undefined,
      source: "browser_content_script",
      trust: "untrusted_page",
    }));
    return true;
  })().then(accepted => reply({ accepted }), () => reply({ accepted: false }));
  return true;
});

chrome.tabs.onActivated.addListener(info => observe(info.tabId, "tab.activated"));
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.url || change.title || change.status === "complete") observe(tabId, "tab.updated");
});
chrome.tabs.onRemoved.addListener(tabId => {
  const last = knownTabs.get(tabId);
  knownTabs.delete(tabId);
  documents.delete(tabId);
  queue.delete(tabId);
  if (last && !paused) emit({ ...last, kind: "tab.closed", observed_at_ms: Date.now() });
});
chrome.webNavigation.onCommitted.addListener(details => {
  if (details.frameId !== 0) return;
  if (details.documentId) documents.set(details.tabId, details.documentId);
  else documents.delete(details.tabId);
  if (documents.size > 256) documents.delete(documents.keys().next().value);
  queue.delete(details.tabId);
  observe(details.tabId, "tab.updated", {
    document_id: details.documentId,
    navigation_id: `${details.tabId}:${details.timeStamp}`,
  });
});
chrome.webNavigation.onHistoryStateUpdated.addListener(details => {
  if (details.frameId === 0) observe(details.tabId, "tab.updated", { document_id: details.documentId });
});

async function resetCollection() {
  generation++;
  queue.clear();
  knownTabs.clear();
  documents.clear();
  const tabs = await chrome.tabs.query({});
  for (const tab of tabs) {
    // Stop previously injected scripts too. Removal alone does not unload them.
    try { await chrome.tabs.sendMessage(tab.id, { type: "jarvis.pause" }, { frameId: 0 }); }
    catch { /* Not injected. */ }
  }
  await status(sessionId ? "connected" : "disconnected");
  await reconcile();
}
chrome.permissions.onAdded.addListener(resetCollection);
chrome.permissions.onRemoved.addListener(resetCollection);
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.paused) {
    paused = changes.paused.newValue === true;
    resetCollection();
  }
});
chrome.alarms.onAlarm.addListener(alarm => {
  if (alarm.name === "jarvis-reconnect") {
    clearTimeout(retryTimer);
    retryTimer = null;
    connect();
  }
});
chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());
connect();


let readLease = null;
async function handleBrowserRequest(message, current) {
  if (message.protocol_version !== PROTOCOL_VERSION || message.session_id !== sessionId
      || typeof message.request_id !== "string" || !/^[a-f0-9]{64}$/.test(message.request_id)) return;
  const replySession = sessionId;
  const epoch = generation;
  let finished = false;
  const send = (status, data = {}) => {
    if (finished || port !== current || sessionId !== replySession) return;
    finished = true;
    try { current.postMessage({ type: "browser.result", protocol_version: PROTOCOL_VERSION,
      session_id: replySession, request_id: message.request_id, status, tabs: [], page: null, ...data }); }
    catch { disconnect(); }
  };
  if (readLease !== null || paused) { send("unavailable"); return; }
  if (!["search_tabs", "read_tab"].includes(message.operation)) { send("unavailable"); return; }
  const lease = message.request_id;
  readLease = lease;
  const expiry = setTimeout(() => {
    if (readLease === lease) readLease = null;
    send("timeout");
  }, 4500);
  const stillValid = () => readLease === lease && epoch === generation && !paused && port === current && sessionId === replySession;
  const identity = async tab => {
    if (!stillValid() || !await permitted(tab)) return null;
    const frame = await chrome.webNavigation.getFrame({ tabId: tab.id, frameId: 0 });
    if (!stillValid() || typeof frame?.documentId !== "string" || !frame.documentId.length
        || frame.documentId.length > 128 || frame.url !== tab.url) return null;
    return { resource_id: `browser-live:${replySession}:${tab.id}`, tab_id: tab.id, window_id: tab.windowId,
      document_id: frame.documentId, url: cappedText(tab.url, 8192), title: cappedText(tab.title, 2048) };
  };
  try {
    const args = message.arguments;
    if (message.operation === "search_tabs") {
      if (typeof args?.query !== "string" || !args.query.trim() || new TextEncoder().encode(args.query).length > 256) { send("unavailable"); return; }
      const needle = args.query.toLocaleLowerCase();
      const matches = [];
      for (const tab of (await chrome.tabs.query({})).slice(0, 256)) {
        if (matches.length === 16 || !stillValid()) break;
        if (!`${tab.title || ""} ${tab.url || ""}`.toLocaleLowerCase().includes(needle)) continue;
        try {
          const found = await identity(tab);
          if (found && stillValid() && await permitted(await chrome.tabs.get(tab.id))) matches.push(found);
        } catch { /* A closed or replaced document is not a search result. */ }
      }
      if (!stillValid()) { send("cancelled"); return; }
      send("ok", { tabs: matches });
      return;
    }
    const prefix = `browser-live:${replySession}:`;
    if (typeof args?.resource_id !== "string" || !args.resource_id.startsWith(prefix)
        || typeof args.expected_document_id !== "string" || args.expected_document_id.length > 128) { send("stale_document"); return; }
    const id = args.resource_id.slice(prefix.length);
    if (!/^(0|[1-9][0-9]{0,9})$/.test(id) || Number(id) > 2147483647) { send("stale_document"); return; }
    const tab = await chrome.tabs.get(Number(id));
    if (!await permitted(tab)) { send("permission_required"); return; }
    const before = await identity(tab);
    if (!before || before.document_id !== args.expected_document_id) { send("stale_document"); return; }
    await inject(tab.id);
    if (!stillValid()) { send("cancelled"); return; }
    const response = await chrome.tabs.sendMessage(tab.id, { type: "jarvis.read" }, { documentId: before.document_id });
    const after = await identity(await chrome.tabs.get(tab.id));
    if (!stillValid()) { send("cancelled"); return; }
    if (!after || after.document_id !== before.document_id || after.url !== before.url) { send("stale_document"); return; }
    if (typeof response?.content !== "string" || new TextEncoder().encode(response.content).length > MAX_CONTENT_BYTES || typeof response.truncated !== "boolean") { send("unavailable"); return; }
    send("ok", { page: { tab: after, content: response.content, truncated: response.truncated, trust: "untrusted_page" } });
  } catch { send(stillValid() ? "unavailable" : "cancelled"); }
  finally { clearTimeout(expiry); if (readLease === lease) readLease = null; }
}
