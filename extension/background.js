// Background script (a service worker in Chrome, an event page in Firefox): keeps the "open .swf URLs in the player" redirect rule in sync with the settings.
const VIEWER = chrome.runtime.getURL('player/viewer.html');
// Requests the extension makes carry its own host as initiator: the extension id in Chrome, a per-install UUID in Firefox (runtime.id is the add-on id there).
const EXT_HOST = new URL(chrome.runtime.getURL('')).hostname;

async function syncRules() {
  const { enabled = true, openSwfLinks = true } = await chrome.storage.local.get(['enabled', 'openSwfLinks']);
  const rules = enabled && openSwfLinks ? [{
    id: 1,
    priority: 1,
    action: { type: 'redirect', redirect: { regexSubstitution: VIEWER + '?src=\\1' } },
    condition: {
      regexFilter: '^(https?://[^#]*\\.swf(?:\\?[^#]*)?)(?:#.*)?$',
      isUrlFilterCaseSensitive: false,
      resourceTypes: ['main_frame', 'sub_frame'],
      excludedInitiatorDomains: [EXT_HOST],
    },
  }] : [];
  await chrome.declarativeNetRequest.updateDynamicRules({ removeRuleIds: [1], addRules: rules });
}

chrome.runtime.onInstalled.addListener((d) => {
  syncRules();
  if (d.reason === 'install') chrome.runtime.openOptionsPage();     // first run: ask for the Flash runtime
});
chrome.runtime.onStartup.addListener(syncRules);
chrome.storage.onChanged.addListener((changes, area) => { if (area === 'local' && ('enabled' in changes || 'openSwfLinks' in changes)) syncRules(); });

// Requests the emulated player makes (URLLoader -> fetch) come from the extension, but Flash content expects them to look
// like the embedding page made them (hot-link protection, CSRF checks on Origin). Each tab that hosts a player gets a session
// rule that sets Referer/Origin to that page for extension-initiated requests.
const RULE_BASE = 1000;
chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  if (!msg || msg.type !== 'flash-runner:page-url' || !sender.tab) return false;
  let origin = '', url = '';
  try { const u = new URL(String(msg.pageUrl)); if (/^https?:$/.test(u.protocol)) { origin = u.origin; url = u.origin + u.pathname + u.search; } } catch { /* not a web page */ }
  const id = RULE_BASE + sender.tab.id;
  const addRules = url ? [{
    id, priority: 2,
    action: { type: 'modifyHeaders', requestHeaders: [{ header: 'Referer', operation: 'set', value: url }, { header: 'Origin', operation: 'set', value: origin }] },
    condition: { tabIds: [sender.tab.id], initiatorDomains: [EXT_HOST], resourceTypes: ['xmlhttprequest', 'media', 'other'] },
  }] : [];
  chrome.declarativeNetRequest.updateSessionRules({ removeRuleIds: [id], addRules }).then(() => reply({ ok: true }), (e) => reply({ ok: false, error: String(e) }));
  return true;                                                      // async reply
});
chrome.tabs.onRemoved.addListener((tabId) => { chrome.declarativeNetRequest.updateSessionRules({ removeRuleIds: [RULE_BASE + tabId] }).catch(() => {}); });

// A movie wanted a socket while the socket helper is not installed (see player/helper-notice.js): mark the tab's toolbar button
// (Chrome drops a tab-specific badge when the tab navigates) and let the popup explain it. 'ok' = the helper answered after all.
chrome.runtime.onMessage.addListener((msg, sender) => {
  if (!msg || msg.type !== 'flash-runner:helper' || !sender.tab) return false;
  const tabId = sender.tab.id;
  if (msg.state === 'missing') {
    chrome.action.setBadgeText({ tabId, text: '!' }).catch(() => {});
    chrome.action.setBadgeBackgroundColor({ tabId, color: '#d93025' }).catch(() => {});
    chrome.action.setTitle({ tabId, title: 'flash-runner: this page needs the helper to play online' }).catch(() => {});
  } else {
    chrome.action.setBadgeText({ tabId, text: '' }).catch(() => {});
    chrome.action.setTitle({ tabId, title: '' }).catch(() => {});
  }
  return false;
});

// "Install the helper": the settings page scrolled to its Connection section, reusing a settings tab that is already open.
chrome.runtime.onMessage.addListener((msg) => {
  if (!msg || msg.type !== 'flash-runner:open-setup') return false;
  const url = chrome.runtime.getURL('options/options.html');
  chrome.tabs.query({}).then(async (tabs) => {
    const tab = tabs.find((t) => (t.url || '').split('#')[0] === url);
    if (!tab) { await chrome.tabs.create({ url: url + '#connection' }); return; }
    await chrome.tabs.update(tab.id, { active: true, url: url + '#connection' });
    await chrome.windows.update(tab.windowId, { focused: true });
  }).catch(() => {});
  return false;
});
