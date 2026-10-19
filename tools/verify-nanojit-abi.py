#!/usr/bin/env python3
"""Read-only, exact-build checks for docs/NANOJIT-ABI.md; no target execution.

This validates static hook coordinates, not dynamic LIR semantics or execution.
"""

import argparse
import hashlib
from pathlib import Path
import struct
import sys


DIGEST = "e357ccdc5f1bf81b89eaf10a6071a69bef5f758c9511a69586157cd3b72ea9f8"
READ_SIZES = bytes.fromhex(
    "00040408080108080108080808010808080808080808080c0c0c0c0c010c0c0c0c10101001101010100c0c010c0c0c0c0c0c1004010c0c0c0c08010c0814010c0c0c0c0c0c0c0c0c0101010101010101010c0c0c0c0c010c0c0c0c0c0c080c0c0c0c08080c0c0c0c0c0c01010101010101010808080c0c0c0c0c0808080c0c0c0c0808080c0c0c0c08080c0c0c0c0c0c0c0c08080c0c0c0c0c10011010100101010108080808080808080814080808080c0101101010101010010101010101040c040404"
)
HOOKS = {
    0x173F02: "5589e557565383ec5c",
    0x173FD3: "e832b30600",
    0x174002: "e859de0600",
    0x174011: "e8f0a20600",
    0x174016: "31c08b577885d2",
    0x18A224: "5589e5565383ec10",
    0x18A25C: "894604",
    0x18A5D3: "8d83d383e2ff",
    0x16A88B: "8b41080fb65003",
    0x16A8A9: "8b52fc895108807a0303",
    0x1883CF: "ff5004",
    0x188462: "ff5004dd5c2404",
}


def verify(path):
    blob = path.read_bytes()
    actual = hashlib.sha256(blob).hexdigest()
    if actual != DIGEST:
        raise ValueError(f"unsupported binary SHA-256 {actual}")
    if blob[:7] != b"\x7fELF\x01\x01\x01":
        raise ValueError("not ELF32 little-endian")
    phoff = struct.unpack_from("<I", blob, 28)[0]
    phsize, phnum = struct.unpack_from("<HH", blob, 42)
    loads = []
    for index in range(phnum):
        kind, offset, va, _, filesz, _, flags, _ = struct.unpack_from(
            "<8I", blob, phoff + index * phsize
        )
        if kind == 1:
            loads.append((offset, va, filesz, flags))

    def at(va, size):
        matches = [off + va - base for off, base, length, _ in loads
                   if base <= va and va + size <= base + length]
        if len(matches) != 1:
            raise ValueError(f"non-unique file mapping at VA {va:#x}")
        return blob[matches[0]:matches[0] + size]

    for va, expected_hex in HOOKS.items():
        expected = bytes.fromhex(expected_hex)
        if at(va, len(expected)) != expected:
            raise ValueError(f"hook instruction mismatch at ELF VA {va:#x}")
    if at(0xCB41C0, len(READ_SIZES)) != READ_SIZES:
        raise ValueError("LirReader step table mismatch")
    if at(0xE33218, 16) != struct.pack("<4I", 0x16A85C, 0x16B9E6, 0x16A878, 0x16A8BA):
        raise ValueError("LirReader vtable mismatch")
    if at(0xCB43C7, 11) != b"JIT failed\0":
        raise ValueError("JIT error anchor mismatch")
    print(f"PASS: {path}")
    print(f"SHA-256 {actual}")
    print(f"{len(HOOKS)} instruction windows; {len(READ_SIZES)} read-size entries; reader vtable")
    print("capture: load_bias+0x174002; installation: load_bias+0x18a224")
    print("Static identity checks only; this script does not test live capture or scalar semantics.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    args = parser.parse_args()
    try:
        verify(args.plugin)
    except (OSError, ValueError, struct.error) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
