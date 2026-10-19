// "This movie wants to go online but the socket helper is not installed": a small dismissible bar over the running movie
// (shared by the in-page player and the full-tab viewer) plus a "!" on the toolbar button, because movies that open sockets are
// often 1x1 or hidden bridges whose iframe nobody would see. Appended to <body> so it never disturbs the player's own elements.
let dismissed = false;

export const helperNotice = {
  missing() {
    try { chrome.runtime.sendMessage({ type: 'flash-runner:helper', state: 'missing' }).catch(() => {}); } catch { /* extension reloaded */ }
    if (dismissed || document.querySelector('[data-helper-notice]')) return;
    const bar = document.createElement('div');
    bar.setAttribute('data-helper-notice', '');
    Object.assign(bar.style, {
      position: 'fixed', left: '8px', right: '8px', bottom: '8px', zIndex: '2147483647', boxSizing: 'border-box', display: 'flex',
      flexWrap: 'wrap', alignItems: 'center', justifyContent: 'center', gap: '8px 12px', padding: '8px 12px', borderRadius: '8px',
      font: '13px/1.35 system-ui, sans-serif', color: '#e8e8e8', background: 'rgba(32,33,36,.96)', boxShadow: '0 2px 10px rgba(0,0,0,.4)',
    });
    const text = document.createElement('span');
    text.textContent = 'This game needs the helper to play online.';
    const install = document.createElement('button');
    install.textContent = 'Install';
    Object.assign(install.style, { font: 'inherit', padding: '4px 12px', border: '0', borderRadius: '6px', background: '#1a73e8', color: '#fff', cursor: 'pointer' });
    install.onclick = () => { chrome.runtime.sendMessage({ type: 'flash-runner:open-setup' }).catch(() => {}); };
    const close = document.createElement('button');
    close.textContent = '×'; close.title = 'Dismiss'; close.setAttribute('aria-label', 'Dismiss');
    Object.assign(close.style, { font: '18px/1 system-ui, sans-serif', padding: '0 4px', border: '0', background: 'transparent', color: '#9aa0a6', cursor: 'pointer' });
    close.onclick = () => { dismissed = true; bar.remove(); };
    bar.append(text, install, close);
    document.body.append(bar);
  },
  /** The helper answered after all (installed while the page was open): take the notice back. */
  found() {
    try { chrome.runtime.sendMessage({ type: 'flash-runner:helper', state: 'ok' }).catch(() => {}); } catch { /* extension reloaded */ }
    document.querySelector('[data-helper-notice]')?.remove();
  },
};
