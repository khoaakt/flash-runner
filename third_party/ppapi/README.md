# PPAPI C headers

Verbatim copy of `ppapi/c/` from Chromium tag `86.0.4240.198`
(https://chromium.googlesource.com/chromium/src/+/refs/tags/86.0.4240.198/ppapi/c/) — the last
release that still shipped the Flash-specific interfaces. Licensed under Chromium's BSD-3-Clause
(see each file's header). Used only as the input of `tools/gen-ppapi.py`, which derives the
signature tables the JavaScript Pepper host decodes guest calls with.

`LICENSE` in this directory is Chromium's top-level licence file at that tag; the per-file headers refer to it.
