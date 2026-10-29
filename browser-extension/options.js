const HOST = "com.pengtang1024.aether";
const DEFAULTS = { intercept: false, notify: true, sendCookies: false, siteFilter: "" };

async function cookiePermissionState() {
  return chrome.permissions.contains({ permissions: ["cookies"] });
}

async function refreshCookieStatus() {
  const status = document.getElementById("cookieStatus");
  const granted = await cookiePermissionState();
  status.textContent = granted
    ? "Cookie permission granted."
    : "Cookie permission not granted — cookies will not be sent.";
}

async function load() {
  const values = await chrome.storage.local.get(DEFAULTS);
  document.getElementById("intercept").checked = values.intercept;
  document.getElementById("notify").checked = values.notify;
  document.getElementById("sendCookies").checked = values.sendCookies;
  document.getElementById("siteFilter").value = values.siteFilter;
  await refreshCookieStatus();
}

document.getElementById("intercept").addEventListener("change", async (event) => {
  await chrome.storage.local.set({ intercept: event.target.checked });
});
document.getElementById("notify").addEventListener("change", async (event) => {
  await chrome.storage.local.set({ notify: event.target.checked });
});
document.getElementById("siteFilter").addEventListener("change", async (event) => {
  await chrome.storage.local.set({ siteFilter: event.target.value });
});

document.getElementById("sendCookies").addEventListener("change", async (event) => {
  if (event.target.checked) {
    // Turning it on must go through the browser's own permission prompt.
    const granted = await chrome.permissions.request({ permissions: ["cookies"] });
    if (!granted) {
      event.target.checked = false;
      await chrome.storage.local.set({ sendCookies: false });
      await refreshCookieStatus();
      return;
    }
  }
  await chrome.storage.local.set({ sendCookies: event.target.checked });
  await refreshCookieStatus();
});

document.getElementById("ping").addEventListener("click", async () => {
  const status = document.getElementById("status");
  status.textContent = "Connecting…";
  chrome.runtime.sendNativeMessage(HOST, { type: "ping" }, (response) => {
    const error = chrome.runtime.lastError;
    if (error) {
      status.textContent = `Native host error: ${error.message}`;
      return;
    }
    status.textContent = JSON.stringify(response, null, 2);
  });
});

load();
