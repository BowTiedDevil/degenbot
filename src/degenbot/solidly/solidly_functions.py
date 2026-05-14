"""Solidly stable pool calculations.

.. deprecated::
    Import from :mod:`degenbot.calculations.solidly_stable` instead.
    This module is retained for backwards compatibility and will be removed
    in a future release.
"""

from degenbot.calculations.solidly_stable import (  # noqa: F401
    calc_d as general_calc_d,
    calc_exact_in_stable as general_calc_exact_in_stable,
    calc_exact_in_volatile as general_calc_exact_in_volatile,
    calc_f,
    calc_k as general_calc_k,
    get_y_solidly,
)
