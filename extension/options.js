import { normalizeOrigin } from "./protocol.js";
const status = document.querySelector("#status");
const message = document.querySelector("#message");
const paused = document.querySelector("#paused");

async function refresh() {
  const [{ origins = [] }, state] = await Promise.all([
    chrome.permissions.getAll(), chrome.storage.local.get(["paused", "bridgeStatus"]),
  ]);
  paused.checked = state.paused === true;
  status.textContent = `Bridge: ${state.bridgeStatus || "disconnected"}${paused.checked ? " · awareness paused" : ""}`;
  document.querySelector("#origins").replaceChildren(...origins.map(origin => {
    const li = document.createElement("li");
    const label = document.createElement("span");
    label.textContent = origin;
    const remove = document.createElement("button");
    remove.textContent = "Remove";
    remove.addEventListener("click", async () => {
      await chrome.permissions.remove({ origins: [origin] });
      await refresh();
    });
    li.append(label, remove);
    return li;
  }));
}

document.querySelector("#grant").addEventListener("submit", async event => {
  event.preventDefault();
  try {
    const origin = normalizeOrigin(document.querySelector("#origin").value);
    // Must remain directly within this user gesture; never request on startup.
    const granted = await chrome.permissions.request({ origins: [origin] });
    message.textContent = granted ? "Website allowed." : "Permission was not granted.";
    await refresh();
  } catch (error) { message.textContent = error.message; }
});
paused.addEventListener("change", async () => {
  await chrome.storage.local.set({ paused: paused.checked });
});
chrome.storage.onChanged.addListener(refresh);
chrome.permissions.onAdded.addListener(refresh);
chrome.permissions.onRemoved.addListener(refresh);
refresh();
