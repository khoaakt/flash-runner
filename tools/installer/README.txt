flash-runner @VERSION@

Installed by FlashRunner.pkg
  @EXTENSION_DIR@
      the extension
  /Library/Application Support/FlashRunnerHelper
      the online game helper (only if you left it selected), registered for Chrome, Chromium, Edge, Brave and Vivaldi

Adding the extension to your browser (once; Chrome does not let an installer do this)
  1. Open chrome://extensions (Edge: edge://extensions) and turn on Developer mode.
  2. Click Load unpacked.
  3. Choose the folder @EXTENSION_DIR@
     (in the file dialog press Command-Shift-G, then paste or type that path)
  If you installed the online game helper, the extension's settings page shows "Online games: Installed".

Updating
  Install the newer package, then click the reload icon on the extension's card (or restart the browser).
  The version number changes with every installer, so the card shows which one is loaded.
  If the extension was loaded from another folder before (a build folder, say), loading this one replaces it:
  same extension ID, one card. In a test it kept the extension's settings.

Removing
  Double-click Uninstall.command in this folder, then remove the extension's card in chrome://extensions.
