#!/usr/bin/env python3
"""Repair a mis-aligned Mach-O `__LINKEDIT` string pool (macOS arm64).

Darwin 27's dyld rejects an extension whose `LC_SYMTAB.stroff` is not 8-byte
aligned with `dlopen(...): mis-aligned LINKEDIT string pool`. The pre-release
Apple `ld-1328` / Homebrew `lld-22` linkers on this box emit a 4-aligned offset
for the larger `degenbot_rs` binary (alignment is layout/luck dependent — a
smaller build happens to land aligned). `strip` / re-signing do not move it.

This tool repairs an already-built `.so` in place:

  1. `codesign --remove-signature` so the string table is the file's tail.
  2. Insert `pad = (8 - stroff % 8) % 8` zero bytes just before the string
     table, bump `LC_SYMTAB.stroff` and the `__LINKEDIT` segment `filesize`.
  3. `codesign --force --sign -` (ad-hoc re-sign).

It is idempotent (a no-op once `stroff` is already 8-aligned). Run it after any
extension rebuild (`just dev`, or a maturin import-hook rebuild during tests):

  uv run python -m degenbot.devtools.fix_macho_linkedit_alignment \
      src/degenbot/degenbot_rs.abi3.so

This is a workaround for a pre-release-toolchain bug; drop it once the system
linker emits an 8-aligned LINKEDIT string pool (or dyld relaxes the check).
"""

from __future__ import annotations

import struct
import subprocess
import sys
from pathlib import Path

LC_SYMTAB = 0x2
LC_SEGMENT_64 = 0x19
MH_MAGIC_64 = 0xFEEDFACF


def _find_load_commands(data: bytes | bytearray) -> tuple[int, int]:
    magic = struct.unpack_from("<I", data, 0)[0]
    if magic != MH_MAGIC_64:
        msg = f"not a 64-bit little-endian Mach-O (magic={magic:#x})"
        raise ValueError(msg)
    ncmds = struct.unpack_from("<I", data, 16)[0]
    symtab_lc: int | None = None
    linkedit_lc: int | None = None
    off = 32  # after mach_header_64
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", data, off)
        if cmd == LC_SYMTAB:
            symtab_lc = off
        elif cmd == LC_SEGMENT_64:
            segname = data[off + 8 : off + 24].split(b"\x00", 1)[0]
            if segname == b"__LINKEDIT":
                linkedit_lc = off
        off += cmdsize
    if symtab_lc is None or linkedit_lc is None:
        msg = "missing LC_SYMTAB or __LINKEDIT segment"
        raise ValueError(msg)
    return symtab_lc, linkedit_lc


def align_linkedit_stroff(path: Path) -> bool:
    """Pad the string table so `stroff` is 8-aligned. Returns True if patched."""
    data = bytearray(path.read_bytes())
    symtab_lc, linkedit_lc = _find_load_commands(data)
    stroff = struct.unpack_from("<I", data, symtab_lc + 16)[0]
    le_filesize = struct.unpack_from("<Q", data, linkedit_lc + 48)[0]
    pad = (8 - (stroff % 8)) % 8
    if pad == 0:
        return False
    new = data[:stroff] + b"\x00" * pad + data[stroff:]
    struct.pack_into("<I", new, symtab_lc + 16, stroff + pad)
    struct.pack_into("<Q", new, linkedit_lc + 48, le_filesize + pad)
    path.write_bytes(new)
    return True


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write(f"usage: {argv[0]} <path-to-extension.so>\n")
        return 2
    path = Path(argv[1])
    if not path.exists():
        sys.stderr.write(f"no such file: {path}\n")
        return 2
    subprocess.run(["codesign", "--remove-signature", str(path)], check=False)  # noqa: S603,S607
    patched = align_linkedit_stroff(path)
    subprocess.run(["codesign", "--force", "--sign", "-", str(path)], check=True)  # noqa: S603,S607
    print(f"{'aligned + re-signed' if patched else 'already aligned (re-signed)'}: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
