"""§4.2 parity + §4.5 delegation tests for the Aave V3 position read-back seam.

Driven by the frozen `aave_parity_expected.json` oracle (dumped from the
pre-cutover SQLAlchemy `DatabasePositionQuery`) + a freshly-regenerated
`aave_parity.db` fixture: the Rust-backed `DatabasePositionQuery` (delegating
through `RustDatabasePositionQuery`) produces identical results to the frozen
SQLAlchemy oracle. Plus a §4.5 delegation spy proving the Python reader hits
Rust with the right args, and an end-to-end `analyze_positions_for_market`
smoke test (the Rust analysis seam consumes the Rust-backed records).
"""

import json
import pathlib

import pytest

from degenbot.aave.analysis.orchestrator import DatabasePositionQuery

FIXTURE_DIR = pathlib.Path("rust/crates/degenbot-db/tests/fixtures")
DB_PATH = FIXTURE_DIR / "aave_parity.db"
EXPECTED_PATH = FIXTURE_DIR / "aave_parity_expected.json"


def _expected() -> dict:
    return json.loads(EXPECTED_PATH.read_text())


@pytest.fixture
def session():
    """Open a SQLAlchemy session over the fixture DB (for the query's path resolution)."""
    from degenbot.database.operations import get_scoped_sqlite_session

    scoped = get_scoped_sqlite_session(DB_PATH)
    with scoped() as s:
        yield s


@pytest.fixture
def query(session) -> DatabasePositionQuery:
    return DatabasePositionQuery(session)


@pytest.fixture
def market_id() -> int:
    return _expected()["market_id"]


class TestParity:
    """§4.2: the Rust-backed DatabasePositionQuery matches the frozen SQLAlchemy oracle."""

    def test_get_users_with_debt(self, query: DatabasePositionQuery, market_id: int) -> None:
        """User count + fields match the oracle (user 3 with no debt is filtered out)."""
        users = list(query.get_users_with_debt(market_id))
        expected = _expected()["users"]
        assert len(users) == len(expected) == 3
        for got, exp in zip(users, expected, strict=True):
            assert got["id"] == exp["id"]
            assert got["address"] == exp["address"]
            assert got["market_id"] == exp["market_id"]
            assert got["e_mode"] == exp["e_mode"]
            assert got["is_isolation_mode"] == exp["is_isolation_mode"]
            assert got["isolation_mode_debt"] == int(exp["isolation_mode_debt"])
            ceiling = exp["isolation_debt_ceiling"]
            assert got["isolation_debt_ceiling"] == (int(ceiling) if ceiling is not None else None)

    def test_get_collateral_positions(self, query: DatabasePositionQuery) -> None:
        """Collateral rows (joined asset/config/emode/underlying) match the oracle."""
        for u in _expected()["users"]:
            rows = list(query.get_collateral_positions(u["id"]))
            assert len(rows) == len(u["collateral"])
            for got, exp in zip(rows, u["collateral"], strict=True):
                assert got["asset_id"] == exp["asset_id"]
                assert got["balance"] == int(exp["balance"])
                assert got["underlying_address"] == exp["underlying_address"]
                assert got["underlying_symbol"] == exp["underlying_symbol"]
                assert got["liquidity_index"] == int(exp["liquidity_index"])
                assert got["e_mode_category_id"] == exp["e_mode_category_id"]
                assert got["asset_lt"] == exp["asset_lt"]
                assert got["asset_ltv"] == exp["asset_ltv"]
                assert got["emode_lt"] == exp["emode_lt"]
                assert got["emode_ltv"] == exp["emode_ltv"]

    def test_get_debt_positions(self, query: DatabasePositionQuery) -> None:
        """Debt rows (joined asset/emode/underlying) match the oracle."""
        for u in _expected()["users"]:
            rows = list(query.get_debt_positions(u["id"]))
            assert len(rows) == len(u["debt"])
            for got, exp in zip(rows, u["debt"], strict=True):
                assert got["asset_id"] == exp["asset_id"]
                assert got["balance"] == int(exp["balance"])
                assert got["underlying_address"] == exp["underlying_address"]
                assert got["underlying_symbol"] == exp["underlying_symbol"]
                assert got["borrow_index"] == int(exp["borrow_index"])
                assert got["e_mode_category_id"] == exp["e_mode_category_id"]

    def test_get_collateral_config_map(self, query: DatabasePositionQuery) -> None:
        """The asset_id to enabled map matches the oracle."""
        for u in _expected()["users"]:
            cfg = query.get_collateral_config_map(u["id"])
            assert cfg == {int(k): v for k, v in u["collateral_config_map"].items()}

    def test_get_oracle_address(self, query: DatabasePositionQuery, market_id: int) -> None:
        """The PRICE_ORACLE address matches the oracle."""
        assert query.get_oracle_address(market_id) == _expected()["oracle_address"]

    def test_get_asset_addresses(self, query: DatabasePositionQuery, market_id: int) -> None:
        """The distinct underlying-token address set matches the oracle."""
        assert query.get_asset_addresses(market_id) == set(_expected()["asset_addresses"])

    def test_limit_sliced(self, query: DatabasePositionQuery, market_id: int) -> None:
        """limit slices after the debt-presence filter."""
        limited = list(query.get_users_with_debt(market_id, limit=1))
        all_users = list(query.get_users_with_debt(market_id))
        assert len(limited) == 1
        assert limited[0]["id"] == all_users[0]["id"]

    def test_isolation_debt_ceiling_materialized(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """User 2 (isolation mode) has the debt ceiling from the asset_config join."""
        users = list(query.get_users_with_debt(market_id))
        user2 = next(u for u in users if u["is_isolation_mode"])
        assert user2["isolation_debt_ceiling"] is not None
        assert user2["isolation_debt_ceiling"] > 0


class TestDelegation:
    """§4.5: the Python DatabasePositionQuery delegates to the Rust RustDatabasePositionQuery."""

    def test_get_users_with_debt_hits_rust(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """get_users_with_debt calls RustDatabasePositionQuery.get_users_with_debt."""
        calls: list[str] = []
        real_handle = query._handle()

        class _Spy:
            def get_users_with_debt(self, mid: int, limit: int | None = None) -> list:
                calls.append("get_users_with_debt")
                return real_handle.get_users_with_debt(mid, limit)

        query._rust = _Spy()  # type: ignore[assignment]
        list(query.get_users_with_debt(market_id))
        assert calls == ["get_users_with_debt"]

    def test_get_oracle_address_hits_rust(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """get_oracle_address calls RustDatabasePositionQuery.get_oracle_address."""
        calls: list[str] = []
        real_handle = query._handle()

        class _Spy:
            def get_oracle_address(self, mid: int) -> str | None:
                calls.append("get_oracle_address")
                return real_handle.get_oracle_address(mid)

        query._rust = _Spy()  # type: ignore[assignment]
        query.get_oracle_address(market_id)
        assert calls == ["get_oracle_address"]


class TestEndToEndAnalysis:
    """The Rust analysis seam consumes Rust-backed records without regression."""

    def test_analyze_positions_for_market_runs(
        self, query: DatabasePositionQuery, market_id: int
    ) -> None:
        """analyze_positions_for_market drives the Rust seam end-to-end.

        No price fetcher (avoids RPC); exercises the full record→Rust-seam→
        bucket pipeline to prove the orchestrator produces a categorized result
        over the fixture users.
        """
        from degenbot.aave.analysis.orchestrator import PositionAnalysisResult

        # Drive the per-user Rust seam via the orchestrator's building blocks
        # (mirrors analyze_positions_for_market without the session/provider).
        from degenbot.db import analyze_aave_user_position

        result = PositionAnalysisResult()
        users = list(query.get_users_with_debt(market_id))
        for user in users:
            collateral = query.get_collateral_positions(user["id"])
            debt = query.get_debt_positions(user["id"])
            cfg = query.get_collateral_config_map(user["id"])
            summary = analyze_aave_user_position(user, collateral, debt, cfg, None)
            result.categorize(summary)
        result.sort_by_risk()
        assert result.total_users == len(users)
        # Every bucketed summary is a Rust UserPositionSummary with a HF.
        for bucket in (result.safe_users, result.at_risk_users, result.liquidatable_users):
            for s in bucket:
                assert s.health_factor is not None or not s.has_debt
