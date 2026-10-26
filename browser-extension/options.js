const HOST = "com.pengtang1024.aether";

async function load() {
  const values = await chrome.storage.local.get({ intercept: false, notify: true });
  document.getElementById("intercept").checked = values.intercept;
  document.getElementById("notify").checked = values.notify;
}

document.getElementById("intercept").addEventListener("change", async (event) => {
  await chrome.storage.local.set({ intercept: event.target.checked });
});
document.getElementById("notify").addEventListener("change", async (event) => {
  await chrome.storage.local.set({ notify: event.target.checked });
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
