"""The sweep pin is spelled in exactly one place.

``tests/sweep_pin.py`` owns ``SWEEP_FORK_BLOCK`` and every sweep module imports
it. This gate scans ``tests/`` for the block number spelled out anywhere else, so
advancing the pin cannot leave a stale copy behind in a module nobody re-ran — the
six-way synchronized edit the constant exists to replace.

Recorded data is the one legitimate exception: the Balancer registry corpus under
``tests/balancer/data/`` carries the block in its filename and its ``pin_block``
field. It is allowlisted by path, and the sweep that reads it asserts the corpus
pin equals ``SWEEP_FORK_BLOCK``, so the corpus cannot drift away from the home
silently.
"""

from __future__ import annotations

from tests.source_scan import literal_hits
from tests.sweep_pin import SWEEP_FORK_BLOCK

# The pin's home, plus recorded data whose filename and fields name the block.
_PIN_HOME = "tests/sweep_pin.py"
_RECORDED_DATA_PREFIX = "tests/balancer/data/"


def _pin_literal_hits() -> list[str]:
    """Every ``path:line:text`` under ``tests/`` that spells out the pin."""
    return [
        hit
        for hit in literal_hits(str(SWEEP_FORK_BLOCK), ["tests"])
        if not hit.startswith((_PIN_HOME, _RECORDED_DATA_PREFIX))
    ]


def test_the_sweep_pin_is_spelled_only_in_its_home() -> None:
    hits = _pin_literal_hits()
    assert not hits, (
        f"the sweep pin {SWEEP_FORK_BLOCK} is spelled outside its home "
        f"({_PIN_HOME}) — import SWEEP_FORK_BLOCK instead of copying the number. "
        f"Offending sites:\n" + "\n".join(hits)
    )


def test_the_scan_sees_the_pin_in_its_home() -> None:
    """The positive control: a scan that read nothing would pass the gate above.

    The gate filters the pin's home out of its hits, so an empty scan and a clean
    tree look identical; finding the pin where it is declared separates them.
    """

    home_hits = [
        hit for hit in literal_hits(str(SWEEP_FORK_BLOCK), ["tests"]) if hit.startswith(_PIN_HOME)
    ]
    assert home_hits, f"the source scan did not find {SWEEP_FORK_BLOCK} in {_PIN_HOME}"
