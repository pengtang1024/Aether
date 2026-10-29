const HOST = "com.pengtang1024.aether";

const DEFAULTS = {
  intercept: false,
  notify: true,
  // Cookie context for downloads (needs the optional "cookies" permission,
  // requested from the options page — never granted by default).
  sendCookies: false,
  // Newline-separated host patterns that intercept applies to
  // ("example.com" or "*.example.com"). Blank = every site.
  siteFilter: "",
};

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
  return chrome.storage.local.get(DEFAULTS);
}

/// Does `hostname` match one pattern line? Plain host or "*.host" wildcard.
function hostMatches(hostname, pattern) {
  const host = hostname.toLowerCase();
  const entry = pattern.trim().toLowerCase();
  if (!entry) return false;
  if (entry.startsWith("*.")) {
    const suffix = entry.slice(1); // ".example.com"
    return host.endsWith(suffix) || host === entry.slice(2);
  }
  return host === entry;
}

function siteAllowed(url, filter) {
  const lines = filter.split("\n").map((line) => line.trim()).filter(Boolean);
  if (lines.length === 0) return true;
  let hostname;
  try {
    hostname = new URL(url).hostname;
  } catch (_) {
    return false;
  }
  return lines.some((line) => hostMatches(hostname, line));
}

/// Cookie header for `url`, only when the user granted the permission and
/// enabled the setting. Always best effort: a download without cookies is
/// better than a failed interception.
async function cookiesFor(url, settings) {
  if (!settings.sendCookies) return undefined;
  const granted = await chrome.permissions.contains({ permissions: ["cookies"] });
  if (!granted) return undefined;
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (cookies.length === 0) return undefined;
    return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
  } catch (_) {
    return undefined;
  }
}

async function handOff({ url, referrer, filename }) {
  const settings = await getSettings();
  const message = { type: "add", url, referrer: referrer || "" };
  if (filename) message.filename = filename;
  const cookies = await cookiesFor(url, settings);
  if (cookies) message.cookies = cookies;
  const result = await sendNative(message);
  if (!result || !result.ok) throw new Error(result?.error || "Aether rejected the task");
  return settings;
}

chrome.runtime.onInstalled.addListener(async () => {
  await chrome.storage.local.set(DEFAULTS);
  chrome.contextMenus.create({
    id: "aether-download",
    title: "Download with Aether",
    contexts: ["link", "video", "audio"],
  });
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId !== "aether-download") return;
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  try {
    await handOff({
      url,
      referrer: info.pageUrl || "",
      filename: info.selectionText || undefined,
    });
  } catch (error) {
    await chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Aether",
      message: String(error),
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
  if (!siteAllowed(item.url, settings.siteFilter)) return;
  try {
    await handOff({
      url: item.url,
      referrer: item.referrer || "",
      filename: item.filename || undefined,
    });
    await chrome.downloads.cancel(item.id);
    await chrome.downloads.erase({ id: item.id });
    if (settings.notify) {
      await chrome.notifications.create({
        type: "basic",
        iconUrl: "icons/icon128.png",
        title: "Aether",
        message: "Download handed off to Aether.",
      });
    }
  } catch (_) {
    // Leave the browser download untouched if Aether is unavailable.
  }
});
