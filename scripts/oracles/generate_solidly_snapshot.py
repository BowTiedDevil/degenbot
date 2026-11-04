"""Regenerate the Solidly / Camelot oracle snapshot.

Authority
---------
The oracle math is the retired Python reference implementation, restored verbatim
beside this file (`solidly_stable_reference.py`, `camelot_reference.py`). Both are
direct ports of the deployed pool contracts' invariant math — the Solidly-family
`Pool.sol` (Aerodrome / Velodrome lineage) and Camelot's variant of it
(`calc_d`, `calc_f` / `f_camelot`, `calc_k` / `k_camelot`, `get_y_solidly` /
`get_y_camelot`, and the exact-in wrappers over them). The reference imports
nothing from the Rust leaf it checks.

Why the inputs are read back
----------------------------
The frozen corpus was drawn from a seeded RNG whose draw sequence was never
archived, so the *inputs* cannot be re-derived and are read from the frozen
snapshot. The *results* are recomputed here from the reference math alone: this
generator re-derives every `result` field from its case's `args`, which is what a
regeneration has to reproduce.

Row format
----------
Compact JSON, one object, no trailing newline, no whitespace between tokens, and
every integer rendered as a decimal string (so >u64 values survive
`serde_json`'s `u64`/`i64`/`f64` ladder — see `args_as_u256` in
`tests/oracle_crosscheck_solidly.rs`):

    {"<fn>":[{"args":["<a>","<b>",...],"result":"<r>"},...],...}

`args` are the case's positional arguments (for the stable exact-in rows:
`amount_in, token_in, reserves0, reserves1, decimals0, decimals1, fee_numerator,
fee_denominator`). A case that reverts renders `"result":"ERROR:<REASON>"`; the
frozen corpus has no revert cases, so such a row is itself a finding.

Usage
-----
    just py python scripts/oracles/generate_solidly_snapshot.py           # write
    just py python scripts/oracles/generate_solidly_snapshot.py --check   # compare

The output path is named by `--out`, defaulting to
`target/oracle-regen/oracle_solidly_snapshot.txt`. The frozen snapshot under
`rust/crates/foundation/degenbot-math/tests/` is only ever read.
"""

from __future__ import annotations

import json
import sys
from collections.abc import Callable
from fractions import Fraction
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import camelot_reference as camelot
import solidly_stable_reference as solidly
from evm_shim import EVMRevertError
from snapshot_io import run_generator

REPO_ROOT = Path(__file__).resolve().parents[2]
SNAPSHOT = REPO_ROOT / "rust/crates/foundation/degenbot-math/tests/oracle_solidly_snapshot.txt"


def _fee(args: list[str], index: int) -> Fraction:
    """Build the fee fraction the exact-in wrappers take.

    Args:
        args: The case's string arguments.
        index: Index of the fee numerator (the denominator follows it).

    Returns:
        The fee as an exact rational.
    """
    return Fraction(int(args[index]), int(args[index + 1]))


def _exact_in(
    k_func: Callable[[int, int, int, int], int],
    get_y_func: Callable[[int, int, int, int, int], int],
) -> Callable[[list[str]], int]:
    """Bind the generic exact-in wrapper to one DEX's `k` / `get_y`.

    Args:
        k_func: The DEX's k-invariant function.
        get_y_func: The DEX's Newton solver.

    Returns:
        A callable mapping a case's string arguments to its result.
    """

    def solve(args: list[str]) -> int:
        return solidly.calc_exact_in_stable(
            amount_in=int(args[0]),
            token_in=int(args[1]),
            reserves0=int(args[2]),
            reserves1=int(args[3]),
            decimals0=int(args[4]),
            decimals1=int(args[5]),
            fee=_fee(args, 6),
            k_func=k_func,
            get_y_func=get_y_func,
        )

    return solve


# Every tag in the frozen corpus, mapped to the reference call that recomputes
# its result from the case's arguments.
RECOMPUTERS: dict[str, Callable[[list[str]], int]] = {
    "calc_d": lambda a: solidly.calc_d(int(a[0]), int(a[1])),
    "calc_k": lambda a: solidly.calc_k(int(a[0]), int(a[1]), int(a[2]), int(a[3])),
    "calc_f": lambda a: solidly.calc_f(int(a[0]), int(a[1])),
    "calc_exact_in_volatile": lambda a: solidly.calc_exact_in_volatile(
        int(a[0]), int(a[1]), int(a[2]), int(a[3]), _fee(a, 4)
    ),
    "get_y_solidly": lambda a: solidly.get_y_solidly(
        int(a[0]), int(a[1]), int(a[2]), int(a[3]), int(a[4])
    ),
    "f_camelot": lambda a: camelot.f_camelot(int(a[0]), int(a[1])),
    "k_camelot": lambda a: camelot.k_camelot(int(a[0]), int(a[1]), int(a[2]), int(a[3])),
    "get_y_camelot": lambda a: camelot.get_y_camelot(
        int(a[0]), int(a[1]), int(a[2]), int(a[3]), int(a[4])
    ),
    "calc_exact_in_stable_solidly": _exact_in(solidly.calc_k, solidly.get_y_solidly),
    "calc_exact_in_stable_camelot": _exact_in(camelot.k_camelot, camelot.get_y_camelot),
}


def render() -> str:
    """Recompute every frozen case's result through the oracle reference.

    Returns:
        The snapshot body: compact JSON, no trailing newline.
    """
    frozen = json.loads(SNAPSHOT.read_text())
    regenerated: dict[str, list[dict[str, object]]] = {}

    for tag, cases in frozen.items():
        recompute = RECOMPUTERS[tag]
        rows: list[dict[str, object]] = []
        for case in cases:
            args = [str(arg) for arg in case["args"]]
            try:
                result = str(recompute(args))
            except EVMRevertError as exc:
                result = f"ERROR:{exc.error}"
            rows.append({"args": args, "result": result})
        regenerated[tag] = rows

    return json.dumps(regenerated, separators=(",", ":"))


if __name__ == "__main__":
    sys.exit(run_generator(sys.argv, snapshot=SNAPSHOT, render=render))
