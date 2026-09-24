"""Build identity for the Rust extension — the stale-`.so` detector.

Every compile of `degenbot_rs` runs `rust/crates/shells/degenbot-python/build.rs`,
which computes a fingerprint (content hash) of the crate's sources and embeds
`<counter, fingerprint>` — the counter advancing only when the fingerprint
changes — into both the compiled library and the repo receipt file
(`.build-number`). This module reads that identity back out of the *installed*
`degenbot._ffi` and compares it against the receipt, so a stale cached
artifact (the maturin/uv failure mode documented in AGENTS.md "Rebuilding the
Rust `.so` after edits") is detected instead of silently shipped. Comparing
fingerprints (not just numbers) means a no-change recompile (`cargo test`,
`clippy`, feature variants) never false-positives, while any artifact actually
built from different sources carries a different fingerprint.

Usage:

    # one-line check (exit 0 fresh / 1 stale)
    uv run --no-sync python -m degenbot.build_info

    # or:
    just verify-build-fresh

    # from Python
    from degenbot.build_info import verify_build_fresh
    verify_build_fresh()  # raises BuildStaleError on a stale extension
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

# Receipt file written by rust/crates/shells/degenbot-python/build.rs. Keep the name
# and the "<count> <fingerprint-hex>" format in sync with that script (and the
# .gitignore entry).
_COUNTER_NAME = ".build-number"


def installed_build_number() -> int:
    """Return the build counter baked into the installed `degenbot._ffi`.

    0 means `build.rs` did not run (the tagging is broken) — callers should
    treat 0 as "unknown", never as "old-but-plausible".

    Returns:
        The monotonic build counter; advances only when the crate's source
        content changes (see `build.rs`).

    """
    from degenbot._ffi import build_number

    return build_number()


def installed_fingerprint() -> str | None:
    """Return the source fingerprint baked into the installed extension.

    None when the running extension predates fingerprint tagging (or the
    `_ffi` surface lacks it — itself a sign of a stale artifact under an old
    build system).

    Returns:
        The 16-hex-char fingerprint string, or None when unavailable.

    """
    try:
        from degenbot._ffi import build_fingerprint
    except ImportError:
        return None
    return build_fingerprint() or None


@dataclass(frozen=True)
class Receipt:
    """The repo-side `<count, fingerprint>` receipt from `.build-number`."""

    count: int
    fingerprint: str | None  # None for a legacy pre-fingerprint file


def read_receipt() -> Receipt | None:
    """Read the latest repo receipt.

    Only an editable/checkout install has the receipt next to the Rust
    sources; published wheels and a fresh clone without a first build get
    None, and every freshness check here is a no-op there.

    Returns:
        The `Receipt`, or None when unavailable.

    """
    # src/degenbot/build_info/__init__.py -> parents[3] = the repo root (the
    # receipt lives next to the Rust sources it fingerprints).
    receipt_path = Path(__file__).resolve().parents[3] / _COUNTER_NAME
    try:
        tokens = receipt_path.read_text().split()
        count = int(tokens[0])
    except (OSError, ValueError, IndexError):
        return None
    return Receipt(count=count, fingerprint=tokens[1] if len(tokens) > 1 else None)


class BuildStaleError(RuntimeError):
    """The installed Rust extension predates the latest build of the crate."""


def build_is_fresh() -> bool:
    """Report whether the installed extension matches the repo receipt.

    Returns True whenever there is no ground truth to compare against (no
    receipt file, non-checkout install): absence of evidence is not staleness.
    With a fingerprint on both sides, identity is compared; the count only
    breaks ties for legacy/pre-fingerprint states.

    Returns:
        True if the installed extension is no older than the receipt.

    """
    receipt = read_receipt()
    if receipt is None:
        return True
    inst_fp = installed_fingerprint()
    if receipt.fingerprint is not None and inst_fp is not None:
        return inst_fp == receipt.fingerprint
    # Fallback (legacy receipt or pre-fingerprint extension): make do with the
    # count. '>=' tolerates a failed receipt write while build.rs still baked
    # an incremented value.
    return installed_build_number() >= receipt.count


def verify_build_fresh() -> None:
    """Fail loudly if the installed `degenbot._ffi` is stale.

    Raises:
        BuildStaleError: the installed extension predates the latest build.

    """
    receipt = read_receipt()
    if receipt is None:
        return
    if not build_is_fresh():
        inst_fp = installed_fingerprint()
        installed = (
            f"fingerprint {inst_fp}"
            if inst_fp is not None
            else f"number {installed_build_number()}"
        )
        expected = (
            f"fingerprint {receipt.fingerprint}"
            if receipt.fingerprint is not None
            else f"number {receipt.count}"
        )
        msg = (
            f"stale Rust extension: installed {installed}, latest build "
            f"{expected} — rebuild with 'uv sync --reinstall-package degenbot'"
        )
        raise BuildStaleError(msg)


def main() -> int:
    """Run the `python -m degenbot.build_info` CLI.

    Returns:
        0 when the extension is fresh (or no receipt exists), 1 when stale.

    """
    inst_fp = installed_fingerprint()
    print(f"installed degenbot._ffi build: {installed_build_number()} ({inst_fp or '-'})")
    receipt = read_receipt()
    if receipt is None:
        print(f"no {_COUNTER_NAME} receipt (non-editable install): check skipped")
        return 0
    print(f"latest repo build receipt:     {receipt.count} ({receipt.fingerprint or '-'})")
    try:
        verify_build_fresh()
    except BuildStaleError as e:
        print(f"STALE: {e}", file=sys.stderr)
        return 1
    print("extension is fresh")
    return 0


if __name__ == "__main__":
    sys.exit(main())
