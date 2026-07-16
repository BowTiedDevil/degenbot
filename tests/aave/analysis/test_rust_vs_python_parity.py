"""§4.2 parity between the Rust `analyze_aave_user_position` PyO3 seam and the
Python `analyze_user_position` oracle (`src/degenbot/aave/analysis/core.py`).

Driven by the frozen `aave_parity.db` fixture: for every user-with-debt, both
implementations receive the SAME records (the `DatabasePositionQuery`
dataclasses → `dataclasses.asdict` dicts for the Rust seam) and must produce
byte-identical health factors, LTV ratios, totals, and per-position fields.

This is the §4.2 red-green gate that must stay green before Step C deletes the
Python `core.py` oracle. Mirrors the writer-cutover's `aave_replay_diff.py`
precedent: do not delete the Python port before the Rust leaf is
cross-checked against it on the same fixture.
"""

import dataclasses
import pathlib

import pytest

from degenbot.aave.analysis.core import analyze_user_position as py_analyze
from degenbot.aave.analysis.orchestrator import DatabasePositionQuery
from degenbot.db import analyze_aave_user_position as rust_analyze

FIXTURE_DIR = pathlib.Path("rust/crates/degenbot-db/tests/fixtures")
DB_PATH = FIXTURE_DIR / "aave_parity.db"

REL_TOL = 1e-6


@pytest.fixture
def query():
    """A `DatabasePositionQuery` over the fixture DB."""
    from degenbot.database.operations import get_scoped_sqlite_session

    scoped = get_scoped_sqlite_session(DB_PATH)
    with scoped() as s:
        yield DatabasePositionQuery(s)


@pytest.fixture
def market_id() -> int:
    return 1


def _to_dict(record) -> dict:
    """Convert a frozen dataclass record to a plain dict (the Rust seam shape)."""
    return dataclasses.asdict(record)


def _assert_collateral_parity(py_pos, rust_pos) -> None:
    assert py_pos.asset_address == rust_pos.asset_address
    assert py_pos.asset_symbol == rust_pos.asset_symbol
    assert py_pos.scaled_balance == rust_pos.scaled_balance
    assert py_pos.actual_balance == rust_pos.actual_balance
    assert py_pos.liquidation_threshold == rust_pos.liquidation_threshold
    assert py_pos.ltv == rust_pos.ltv
    assert py_pos.is_enabled_as_collateral == rust_pos.is_enabled_as_collateral
    assert py_pos.in_emode == rust_pos.in_emode
    assert py_pos.emode_category_id == rust_pos.emode_category_id
    assert py_pos.price == rust_pos.price


def _assert_debt_parity(py_pos, rust_pos) -> None:
    assert py_pos.asset_address == rust_pos.asset_address
    assert py_pos.asset_symbol == rust_pos.asset_symbol
    assert py_pos.scaled_balance == rust_pos.scaled_balance
    assert py_pos.actual_balance == rust_pos.actual_balance
    assert py_pos.stable_debt == rust_pos.stable_debt
    assert py_pos.in_emode == rust_pos.in_emode
    assert py_pos.emode_category_id == rust_pos.emode_category_id
    assert py_pos.price == rust_pos.price


def _assert_summary_parity(py_sum, rust_sum) -> None:
    assert py_sum.user_address == rust_sum.user_address
    assert py_sum.market_id == rust_sum.market_id
    assert py_sum.emode_category_id == rust_sum.emode_category_id
    assert py_sum.is_isolation_mode == rust_sum.is_isolation_mode

    # Health factor (None-safe, rel-tol float)
    if py_sum.health_factor is None:
        assert rust_sum.health_factor is None
    else:
        assert rust_sum.health_factor is not None
        assert py_sum.health_factor == pytest.approx(
            rust_sum.health_factor, rel=REL_TOL
        )

    # Max LTV ratio
    if py_sum.max_ltv_ratio is None:
        assert rust_sum.max_ltv_ratio is None
    else:
        assert rust_sum.max_ltv_ratio is not None
        assert py_sum.max_ltv_ratio == pytest.approx(
            rust_sum.max_ltv_ratio, rel=REL_TOL
        )

    # Totals (Python holds ints at runtime; Rust returns ints)
    assert int(py_sum.total_collateral_value) == int(rust_sum.total_collateral_value)
    assert int(py_sum.total_debt_value) == int(rust_sum.total_debt_value)

    # Derived boolean properties
    assert py_sum.is_at_risk == rust_sum.is_at_risk
    assert py_sum.is_liquidatable == rust_sum.is_liquidatable
    assert py_sum.has_debt == rust_sum.has_debt

    # Per-position parity
    py_col = list(py_sum.collateral_positions)
    rust_col = list(rust_sum.collateral_positions)
    assert len(py_col) == len(rust_col), "collateral count mismatch"
    for pc, rc in zip(py_col, rust_col, strict=True):
        _assert_collateral_parity(pc, rc)

    py_debt = list(py_sum.debt_positions)
    rust_debt = list(rust_sum.debt_positions)
    assert len(py_debt) == len(rust_debt), "debt count mismatch"
    for pd, rd in zip(py_debt, rust_debt, strict=True):
        _assert_debt_parity(pd, rd)


class TestRustVsPythonParity:
    """§4.2: the Rust seam produces byte-identical results to the Python oracle."""

    def test_all_users_match(self, query: DatabasePositionQuery, market_id: int) -> None:
        """Every user-with-debt produces identical HF/LTV/totals/positions."""
        users = list(query.get_users_with_debt(market_id))
        assert users, "fixture should have users with debt"
        for user in users:
            collateral = list(query.get_collateral_positions(user.id))
            debt = list(query.get_debt_positions(user.id))
            cfg = query.get_collateral_config_map(user.id)

            # Python oracle (the spec) — takes dataclasses
            py_summary = py_analyze(
                user=user,
                collateral_positions=collateral,
                debt_positions=debt,
                collateral_config_map=cfg,
                price_map=None,
            )

            # Rust seam — takes plain dicts (dataclasses.asdict)
            rust_summary = rust_analyze(
                _to_dict(user),
                [_to_dict(c) for c in collateral],
                [_to_dict(d) for d in debt],
                cfg,  # already a dict[int, bool]
                None,
            )

            _assert_summary_parity(py_summary, rust_summary)

    def test_no_debt_user_health_factor_none(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """A user with no debt returns HF=None from both implementations."""
        users = list(query.get_users_with_debt(market_id))
        # Pick the first user + pass empty debt lists
        user = users[0]
        cfg = query.get_collateral_config_map(user.id)
        collateral = list(query.get_collateral_positions(user.id))

        py_summary = py_analyze(
            user=user,
            collateral_positions=collateral,
            debt_positions=[],
            collateral_config_map=cfg,
            price_map=None,
        )
        rust_summary = rust_analyze(
            _to_dict(user),
            [_to_dict(c) for c in collateral],
            [],
            cfg,
            None,
        )
        assert py_summary.health_factor is None
        assert rust_summary.health_factor is None

    def test_empty_positions(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """A user with no collateral + no debt returns HF=None from both."""
        users = list(query.get_users_with_debt(market_id))
        user = users[0]

        py_summary = py_analyze(
            user=user,
            collateral_positions=[],
            debt_positions=[],
            collateral_config_map={},
            price_map=None,
        )
        rust_summary = rust_analyze(_to_dict(user), [], [], {}, None)
        assert py_summary.health_factor is None
        assert rust_summary.health_factor is None
        assert int(rust_summary.total_collateral_value) == 0
        assert int(rust_summary.total_debt_value) == 0
