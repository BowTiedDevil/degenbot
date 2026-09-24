#!/usr/bin/env python3
"""Regenerate the SciPy golden traces pinned in ``tests/bounded_brent.rs``.

The Rust port of SciPy 1.17 ``_minimize_scalar_bounded`` must reproduce these
traces: identical objective evaluations, identical ``nfev``, identical
``success``. This script runs the real SciPy implementation over a deterministic
objective set and prints the constants in Rust source form; it emits to stdout
rather than writing a fixture, because the pinned values live inline in the test
so a divergence fails at the exact assertion site.

Run from the repo root::

    uv run --with scipy==1.17 python \
        rust/crates/engine/degenbot-solvers/tests/fixtures/generate_bounded_brent_goldens.py

SciPy's bounded method ignores any ``bracket`` argument; only ``bounds`` and
``xatol`` feed the algorithm.
"""

from __future__ import annotations

import scipy.optimize._optimize as _optimize

# (rust_name, expression, lower, upper) — expressions use explicit
# multiplication for squares so the Python and Rust arithmetic sequences are
# the same.
OBJECTIVES = [
    ("SQUARE_SHIFTED", "(x - 12345.678) * (x - 12345.678)", 1.0, 20000.0),
    (
        "SCALED_1E18_PLUS_LINEAR",
        "1e18 * (x / 12345.678 - 1.0) * (x / 12345.678 - 1.0) + 7.5e-3 * x",
        1.0,
        30000.0,
    ),
    (
        "QUARTIC_FLAT_TOP",
        "(x - 5000.0) * (x - 5000.0) * (x - 5000.0) * (x - 5000.0)"
        " + 1e-3 * (x - 5000.0) * (x - 5000.0)",
        1.0,
        10000.0,
    ),
    ("SHIFTED_LEFT_MIN", "(x - 11111.25) * (x - 11111.25)", 1.0, 200000.0),
]


def main() -> None:
    for name, expr, lower, upper in OBJECTIVES:
        objective = eval("lambda x: " + expr, {"__builtins__": {}})  # noqa: S307
        result = _optimize._minimize_scalar_bounded(
            objective, (lower, upper), xatol=1.0
        )
        print(
            f"    Golden {{ name: \"{name}\", "
            f"bounds: ({lower!r}, {upper!r}), "
            f"x: {float(result.x)!r}, fun: {float(result.fun)!r}, "
            f"nfev: {result.nfev}, success: {result.success} }},"
        )


if __name__ == "__main__":
    main()
