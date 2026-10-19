// What only the player frame's main thread can do. The emulator runs in a worker, which has no gamepads, file pickers, camera or print dialog,
// so it asks the frame through the 'svc' message (see FlashPlayer.service in loader.js and frameService in worker.js).
// A service is called as fn(args, { active }) where `active` says whether the user has a transient activation (a click or key press a moment ago):
// pickers and print dialogs need one, and a worker round trip is slow enough that the check has to happen here, not there.

export function createFrameServices(player) {
  return {
    /** Round trip check and a look at the activation state. */
    ping() { return { pong: true, active: navigator.userActivation?.isActive ?? null }; },
  };
}

/** One sample of the connected pads in the layout the worker copies into PP_GamepadsSampleData (at most 4 pads, 16 axes, 32 buttons). */
export function readGamepads() {
  const pads = [];
  for (const g of (navigator.getGamepads?.() || [])) {
    if (!g || !g.connected || pads.length >= 4) continue;
    pads.push({ id: String(g.id), timestamp: g.timestamp, axes: Array.from(g.axes).slice(0, 16), buttons: Array.from(g.buttons, (b) => b.value).slice(0, 32) });
  }
  return pads;
}

/**
 * Poll the Gamepad API and call send(pads) when something changed. Chrome only exposes a pad after a button press, so a
 * pad that appears later is picked up by the 250 ms scan; once a pad exists the poll runs every 16 ms.
 * Returns a function that stops it.
 */
export function watchGamepads(send, { intervalMs = 16 } = {}) {
  if (typeof navigator === 'undefined' || !navigator.getGamepads) return () => {};
  let last = '[]', fast = false;
  const tick = () => {
    const pads = readGamepads();
    const key = JSON.stringify(pads.map((p) => [p.id, p.axes, p.buttons]));
    if (key !== last) { last = key; send(pads); }
    return pads.length > 0;
  };
  let handle = setInterval(tick, 250);
  const watch = setInterval(() => {        // switch between the slow scan and the fast poll as pads come and go
    const have = tick();
    if (have !== fast) { fast = have; clearInterval(handle); handle = setInterval(tick, have ? intervalMs : 250); }
  }, 250);
  tick();
  return () => { clearInterval(handle); clearInterval(watch); };
}
