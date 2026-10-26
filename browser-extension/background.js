const HOST = "com.pengtang1024.aether";

function sendNative(message) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendNativeMessage(HOST, message, (response) => {
      const error = chrome.runtime.lastError;
      if (error) {
        reject(new Error(error.message));
        return;
      }
      resolve(response);
    });
  });
}

async function getSettings() {
  const values = await chrome.storage.local.get({ intercept: false, notify: true });
  return values;
}

chrome.runtime.onInstalled.addListener(async () => {
  await chrome.storage.local.set({ intercept: false, notify: true });
  chrome.contextMenus.create({
    id: "aether-download",
    title: "Download with Aether",
    contexts: ["link", "video", "audio"]
  });
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId !== "aether-download") return;
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  try {
    const result = await sendNative({
      type: "add",
      url,
      referrer: info.pageUrl || "",
      filename: info.selectionText || undefined
    });
    if (!result || !result.ok) throw new Error(result?.error || "Aether rejected the task");
  } catch (error) {
    await chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Aether",
      message: String(error)
    });
  }
});

chrome.action.onClicked.addListener(() => {
  chrome.runtime.openOptionsPage();
});

chrome.downloads.onCreated.addListener(async (item) => {
  const settings = await getSettings();
  if (!settings.intercept) return;
  if (!/^https?:\/\//i.test(item.url)) return;
  try {
    const result = await sendNative({
      type: "add",
      url: item.url,
      referrer: item.referrer || "",
      filename: item.filename || undefined
    });
    if (!result || !result.ok) return;
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
    if (settings.notify) {
      await chrome.notifications.create({
        type: "basic",
        iconUrl: "icons/icon128.png",
        title: "Aether",
        message: "Download handed off to Aether."
      });
    }
  } catch (_) {
    // Leave the browser download untouched if Aether is unavailable.
  }
});
