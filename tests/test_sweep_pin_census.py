"""The sweep pin is spelled in exactly one place.

``tests/sweep_pin.py`` owns ``SWEEP_FORK_BLOCK`` and every sweep module imports
it. This gate greps ``tests/`` for the block number spelled out anywhere else, so
advancing the pin cannot leave a stale copy behind in a module nobody re-ran — the
six-way synchronized edit the constant exists to replace.

Recorded data is the one legitimate exception: the Balancer registry corpus under
``tests/balancer/data/`` carries the block in its filename and its ``pin_block``
field. It is allowlisted by path, and the sweep that reads it asserts the corpus
pin equals ``SWEEP_FORK_BLOCK``, so the corpus cannot drift away from the home
silently.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from tests.sweep_pin import SWEEP_FORK_BLOCK

ROOT = Path(__file__).resolve().parents[1]

# The pin's home, plus recorded data whose filename and fields name the block.
_PIN_HOME = "tests/sweep_pin.py"
_RECORDED_DATA_PREFIX = "tests/balancer/data/"


def _pin_literal_hits() -> list[str]:
    """Every ``path:line:text`` under ``tests/`` that spells out the pin."""
    completed = subprocess.run(  # noqa: S603 - fixed argv, no shell
        [
            "rg",
            "--no-heading",
            "--line-number",
            "--fixed-strings",
            "--glob",
            "!**/__pycache__/**",
            str(SWEEP_FORK_BLOCK),
            "tests",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert completed.returncode in (0, 1), completed.stderr
    return [
        line
        for line in completed.stdout.splitlines()
        if line and not line.startswith((_PIN_HOME, _RECORDED_DATA_PREFIX))
    ]


def test_the_sweep_pin_is_spelled_only_in_its_home() -> None:
    hits = _pin_literal_hits()
    assert not hits, (
        f"the sweep pin {SWEEP_FORK_BLOCK} is spelled outside its home "
        f"({_PIN_HOME}) — import SWEEP_FORK_BLOCK instead of copying the number. "
        f"Offending sites:\n" + "\n".join(hits)
    )
