"""The fee-percentile pair has one home, and the FFI verdict carries it.

The advisory ``eth_feeHistory`` sample rides the core session consumer now: its
policy builder reads ``degenbot_core::fee_percentiles::PRIORITY_FEE_PERCENTILES``
at build time (pinned Rust-side as ``the_fee_policy_reads_the_core_percentile_pair``),
and Python exposes no percentile parameter a mirror could hide behind.

What stays observable from here is the chain's two ends: the engine derives its
clamp from the foundation pair instead of copying it, and the FFI verdict
carries the pair back to Python unchanged.

"""

from __future__ import annotations

import re
from pathlib import Path

from degenbot.config import resolved_config

_REPO_ROOT = Path(__file__).resolve().parents[2]

_SIMULATOR_RS = _REPO_ROOT / "rust/crates/engine/degenbot-arbitrage/src/simulator.rs"

_CORE_FEE_PERCENTILES_RS = (
    _REPO_ROOT / "rust/crates/foundation/degenbot-core/src/fee_percentiles.rs"
)


def _core_percentiles() -> tuple[int, int]:
    source = _CORE_FEE_PERCENTILES_RS.read_text(encoding="utf-8")
    pair = re.search(
        r"PRIORITY_FEE_PERCENTILES:\s*\[u64;\s*2\]\s*=\s*\[(\d+),\s*(\d+)\]",
        source,
    )
    assert pair is not None, (
        "the shared priority-fee percentile declarations moved; update this pin"
    )
    return (int(pair.group(1)), int(pair.group(2)))


# live-invariant gate (ADR-087 D2): watches the live `MIN`/`MAX_PRIORITY_FEE_PERCENTILE`
# declarations in `degenbot-arbitrage/src/simulator.rs`, which must derive from the
# foundation pair; not tripwire precedent.
def test_engine_carries_no_second_percentile_literal() -> None:
    """The pair has one home; the engine derives from it instead of copying it."""
    source = _SIMULATOR_RS.read_text(encoding="utf-8")
    assert not re.search(r"MIN_PRIORITY_FEE_PERCENTILE: u64 = \d+", source), (
        "the engine percentile literal is a second home; derive it from the "
        "shared foundation declaration"
    )

    assert not re.search(r"MAX_PRIORITY_FEE_PERCENTILE: u64 = \d+", source), (
        "the engine percentile literal is a second home; derive it from the "
        "shared foundation declaration"
    )


def test_the_verdict_carries_the_core_pair() -> None:
    """The verdict is the one FFI door for the pair, not a module function."""
    assert resolved_config().fee_percentiles == _core_percentiles()
