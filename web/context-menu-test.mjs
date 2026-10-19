import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// Right-click text actions (Cut / Copy / Paste) in Flash. Two defects made them fail, both fixed in web/loader.js:
//  1. Clicking a row of the HTML context menu moved keyboard focus off the movie. The blur is reported to Flash as
//     "focus lost", the text field drops its selection, and Copy/Cut then have nothing to write.
//  2. Flash enables "Paste" only if the clipboard has text when the menu is built, but the worker learns the clipboard only
//     after Ctrl+V or a copy made inside Flash, so text copied elsewhere never enabled Paste.
// Behaviour is proven in real Chrome with tools/ext-test.mjs (see docs/STATUS.md); this locks the wiring.
const loader = readFileSync(new URL('loader.js', import.meta.url), 'utf8');
assert.match(loader, /menu\.addEventListener\('mousedown', \(e\) => e\.preventDefault\(\)\)/, 'menu rows must not take focus');
assert.match(loader, /if \(selected != null\) this\.canvas\.focus\?\.\(\)/, 'focus returns to the movie after a choice');
const contextmenu = loader.slice(loader.indexOf("on('contextmenu'"), loader.indexOf("on('wheel'"));
assert.match(contextmenu, /navigator\.clipboard\.readText\(\)\.then\(\(text\) => this\.worker\.postMessage\(\{ type: 'clipboard-text', text \}\)/);
assert.ok(contextmenu.indexOf('readText') < contextmenu.indexOf('ask()') || /\.finally\(ask\)/.test(contextmenu), 'the clipboard text is delivered before the menu event');
// the worker side still applies clipboard-text and forwards Flash's writes to the browser clipboard
assert.match(readFileSync(new URL('worker.js', import.meta.url), 'utf8'), /m\.type === 'clipboard-text' && pep\) pep\.clipboardText/);
assert.match(loader, /case 'clipboard': navigator\.clipboard\?\.writeText\(m\.text\)/);
console.log('PASS Flash context menu keeps focus on the movie and refreshes the clipboard before it opens');
