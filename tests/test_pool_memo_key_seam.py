"""Pool memo keys are built by the ledger seam, never formatted by hand.

The registration ledger keys its memos on one typed identity; Python carries
the canonical text form. Before that, the pipeline, the example and the driver
each formatted their own variant (`v3:0x…`, `v4:0x…`, `p:0x…`), so a verify
fact written by one path was invisible to the others. This gate keeps the
constructor the only place a key is built.
"""

from __future__ import annotations

import re
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src"
SEAM = "runner/_registration_ledger.py"

_HAND_FORMATTED = (
    re.compile(r"""f"v[234]:"""),
    re.compile(r'"p:0x'),
    re.compile(r'"v4id:0x'),
)


def _hand_formatted_keys(text: str) -> list[str]:
    return [match.group(0) for pattern in _HAND_FORMATTED for match in pattern.finditer(text)]


def test_no_hand_formatted_pool_memo_keys() -> None:
    violations: list[str] = []
    for path in sorted(SRC.rglob("*.py")):
        relative = path.relative_to(SRC).as_posix()
        if relative == SEAM:
            continue
        for found in _hand_formatted_keys(path.read_text()):
            violations.append(f"{relative}: {found}")

    assert not violations, (
        "pool memo keys must come from RegistrationLedger.pool_memo_key / "
        "address_memo_key / pool_id_memo_key:\n  " + "\n  ".join(violations)
    )


def test_the_gate_goes_red_on_the_old_shape() -> None:
    assert _hand_formatted_keys('key = f"v3:{pool.address}"') == ['f"v3:']
    assert _hand_formatted_keys('memoize("v4id:0xdead")') == ['"v4id:0x']
    assert _hand_formatted_keys("key = ledger.address_memo_key(pool.address)") == []
