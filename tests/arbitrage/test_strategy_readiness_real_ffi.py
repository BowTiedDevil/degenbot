"""Real-FFI pin for the empty-fleet strategy readiness refusal.

``validate_strategy_readiness`` reads this process's resolved config through
``ResolvedConfig::strategy_readiness``, which delegates to ``degenbot-config``'s
``validate_hosted_strategy_readiness``. The suite's other empty-fleet pin
monkeypatches the Python function, which never exercises that wiring. This
test runs a fresh interpreter with ``DEGENBOT_CONFIG`` pinned to a no-facet
operator file, so the refusal is produced by the real FFI resolution.

A fresh interpreter is required because the config installs once at FFI module
init: the file layer must be selected before ``degenbot._ffi`` is imported.
"""

from __future__ import annotations

from tests.helpers.verdict_probe import operator_file, run

# The child catches the refusal to prove its TYPE: a ValueError exits 0, any
# other exception and a non-raising resolution exit non-zero.
_CHILD = """
import sys

from degenbot.strategy import validate_strategy_readiness

try:
    validate_strategy_readiness()
except ValueError as exc:
    print(str(exc))
    sys.exit(0)
except BaseException as exc:
    print(f"WRONG-TYPE {type(exc).__name__}: {exc}", file=sys.stderr)
    sys.exit(3)
else:
    print("NO-REFUSAL", file=sys.stderr)
    sys.exit(2)
"""


def test_empty_fleet_refuses_through_the_real_ffi() -> None:
    """A no-facet operator file makes the real FFI readiness resolution raise
    ``ValueError`` carrying both remediation anchors."""
    # No [strategy.*] sections: every arm is inactive, which a hosted runner
    # refuses.
    with operator_file("# no [strategy.*] sections -> every arm inactive\n") as path:
        proc = run(_CHILD, operator_file=path)

    assert proc.returncode == 0, (
        "the empty fleet must refuse as a ValueError through the real FFI; "
        f"returncode={proc.returncode} stdout={proc.stdout!r} stderr={proc.stderr!r}"
    )
    assert "no strategy facet is active" in proc.stdout
    assert "degenbot strategy activate" in proc.stdout
