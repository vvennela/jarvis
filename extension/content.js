(() => {
  // Isolated-world script: no window.postMessage bridge or page-provided commands.
  if (globalThis.__jarvisAwarenessInstalled) return;
  globalThis.__jarvisAwarenessInstalled = true;
  const MAX_BYTES = 64 * 1024;
  const MAX_NODES = 12000;
  const EXCLUDED = "input,textarea,select,option,button,[contenteditable]:not([contenteditable='false']),[role='textbox'],[role='searchbox'],script,style,noscript,template,[hidden],[aria-hidden='true'],[data-jarvis-private]";
  const encoder = new TextEncoder();
  let timer = null;
  let previous = null;
  let collecting = true;

  function extract() {
    if (!document.body) return "";
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    let text = "";
    let bytes = 0;
    let seen = 0;
    let node;
    while ((node = walker.nextNode()) && seen++ < MAX_NODES && bytes < MAX_BYTES) {
      const element = node.parentElement;
      if (!element || element.closest(EXCLUDED)) continue;
      const style = getComputedStyle(element);
      if (style.visibility !== "visible" || style.display === "none" || element.getClientRects().length === 0) continue;
      const fragment = node.textContent.slice(0, MAX_BYTES).replace(/\s+/g, " ").trim();
      if (!fragment) continue;
      // Cap before iteration to avoid giant text nodes consuming unbounded work.
      for (const point of `${fragment.slice(0, MAX_BYTES)} `) {
        const size = encoder.encode(point).length;
        if (bytes + size > MAX_BYTES) return text.trim();
        text += point;
        bytes += size;
      }
    }
    return text.trim();
  }

  async function capture() {
    timer = null;
    if (!collecting) return;
    const content = extract();
    if (content === previous) return;
    try {
      const response = await chrome.runtime.sendMessage({ type: "jarvis.content", content });
      if (response?.accepted) previous = content;
    } catch {
      // An unloaded extension must not leave a hot observer running forever.
      collecting = false;
      observer.disconnect();
    }
  }

  function schedule() {
    if (!collecting || timer !== null) return;
    timer = setTimeout(capture, 1000);
  }
  const observer = new MutationObserver(schedule);
  observer.observe(document.documentElement, {
    subtree: true, childList: true, characterData: true, attributes: true,
    attributeFilter: ["hidden", "aria-hidden", "class", "style", "contenteditable"],
  });
  chrome.runtime.onMessage.addListener((message, sender, reply) => {
    if (sender.id !== chrome.runtime.id) return;
    if (message?.type === "jarvis.pause") {
      collecting = false;
      previous = null;
      clearTimeout(timer);
      timer = null;
      reply({ ok: true });
    } else if (message?.type === "jarvis.resume") {
      collecting = true;
      previous = null;
      schedule();
      reply({ ok: true });
    }
  });
  schedule();
})();
