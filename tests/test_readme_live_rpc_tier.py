"""README live-RPC examples belong to the on-demand tier, not the offline gate.

The README is a pytest testpath, so its code blocks run as doctests. A block
inside `<!-- live-rpc: start -->` talks to a live archive node, which the
default gate (`-m 'not ... online_rpc ...'`, ADR-074) must not depend on: one RPC
hiccup would otherwise fail `just test` before any Rust or Python test reported.
`just test-fork` runs `-m online_rpc`, so the block still executes there.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def _collect(*marker_args: str) -> str:
    completed = subprocess.run(  # noqa: S603 - fixed argv, no shell
        [sys.executable, "-m", "pytest", "README.md", "--collect-only", "-q", *marker_args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return completed.stdout


def test_the_live_rpc_block_is_collected_only_on_demand() -> None:
    online = _collect("-m", "online_rpc")
    offline = _collect("-m", "not online_rpc")

    assert "README.md::line:" in online, f"no README item is marked online_rpc:\n{online}"
    offline_items = set(offline.split())
    for item in online.split():
        if item.startswith("README.md::line:"):
            assert item not in offline_items, (
                f"{item} runs a live RPC call but is part of the offline gate"
            )


def test_the_offline_gate_still_collects_the_rustless_examples() -> None:
    offline = _collect("-m", "not online_rpc")
    assert "README.md::line:" in offline, "the cassette-backed README examples must still run"
