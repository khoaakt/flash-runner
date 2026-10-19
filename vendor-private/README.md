# vendor-private/

Local-only inputs. **Everything here except this file is git-ignored and must never be committed or distributed.**

| file | what | used by |
|---|---|---|
| `libpepflashplayer-32.0.0.363.so` | Flash Player 32.0.0.363, ia32 Linux PPAPI, SHA-256 `e357ccdc5f1bf81b89eaf10a6071a69bef5f758c9511a69586157cd3b72ea9f8`: the build the accelerators and most tests are verified against | `tools/flash-accelerators-test.mjs`, `web/flash-smoke.mjs`, `tools/build-extension.mjs`, `tools/build-plugin-aot.mjs` |
| `libpepflashplayer-26.0.0.151.so` | Flash Player 26.0.0.151, ia32 Linux PPAPI | `tools/build-extension.mjs` (bundled as a second choice) |
| `libpepflashplayer.so` | any runtime you want the dev server and `ext-test` to use by default | `web/serve.mjs`, `tools/ext-test.mjs` (see its `--bundled` / `--runtime`) |
| `extension-key.pem` | the RSA private key whose public half is `key` in `extension/manifest.json`; it fixes the extension's ID and signs the Linux CRX | `tools/build-installer-linux.mjs`, the two installer tests |

No Flash runtime comes with the project and it does not say where to get one. Use a copy you are licensed to use; the licence belongs to Adobe/HARMAN.
Builds from 32.0.0.465 on show Adobe's "blocked" screen and are not supported.

## Your own extension ID (forks)

The ID in `extension/manifest.json` belongs to the original key, which is not published. A fork generates its own:

```sh
openssl genrsa -out vendor-private/extension-key.pem 2048
openssl rsa -in vendor-private/extension-key.pem -pubout -outform DER | base64 | tr -d '\n'   # -> the new "key" value in extension/manifest.json
```

then replaces the ID everywhere it is written down: `extOrigin` in `native/helper/main.go`, `EXT_ID` in `tools/install-native-host.sh`, `ORIGIN` in
`native/helper-test.mjs`
(the ID is the first 16 bytes of the SHA-256 of the DER public key, written with the letters a-p; `tools/installer/crx.mjs` has `extensionId()`).
Update the allowlist in `.gitleaks.toml` only if the key's DER prefix changes (it does not for RSA-2048).
