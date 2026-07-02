"""§4.2 parity + §4.5 delegation tests for the V3/V4 snapshot DB seam.

Driven by SLHSM4's frozen `parity_expected.json` oracle (dumped from the
prior SQLAlchemy-backed `DatabaseSnapshot`) + a freshly-regenerated
`parity.db` fixture: the Rust-backed `DatabaseSnapshot` (delegating through
`PyDatabaseSnapshot`) produces identical results to the frozen SQLAlchemy
oracle. Plus a §4.5 delegation spy proving the Python reader hits Rust with
the right args.
"""

import json
import pathlib

import pytest

from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot

FIXTURE_DIR = pathlib.Path("rust/crates/degenbot-db/tests/fixtures")
DB_PATH = FIXTURE_DIR / "parity.db"
EXPECTED_PATH = FIXTURE_DIR / "parity_expected.json"


def _expected() -> dict:
    return json.loads(EXPECTED_PATH.read_text())


@pytest.fixture(scope="module")
def v3_snapshot() -> V3DatabaseSnapshot:
    return V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)


@pytest.fixture(scope="module")
def v4_snapshot() -> V4DatabaseSnapshot:
    return V4DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)


# ── §4.2 parity: Rust-backed reads == frozen SQLAlchemy oracle ─────────


class TestV3SnapshotParity:
    """V3 DatabaseSnapshot (Rust-backed) matches the frozen SQLAlchemy oracle."""

    def test_get_newest_block(self, v3_snapshot: V3DatabaseSnapshot) -> None:
        """get_newest_block matches the oracle."""
        assert v3_snapshot.get_newest_block() == _expected()["v3_newest_block"]

    def test_get_pools(self, v3_snapshot: V3DatabaseSnapshot) -> None:
        """get_pools matches the oracle."""
        assert v3_snapshot.get_pools() == set(_expected()["v3_pools"])

    def test_get_all_liquidity_maps(self, v3_snapshot: V3DatabaseSnapshot) -> None:
        """get_all_liquidity_maps matches the oracle tick tuples."""
        oracle = _expected()["v3_all_liquidity_maps"]
        rust = v3_snapshot.get_all_liquidity_maps()
        # Both keyed by address; compare the per-tick (gross, net) int tuples.
        assert set(rust) == set(oracle)
        for addr, ticks in oracle.items():
            assert {int(t): tuple(map(int, v)) for t, v in ticks.items()} == rust[addr]

    def test_get_liquidity_map_match_oracle(self, v3_snapshot: V3DatabaseSnapshot) -> None:
        """get_liquidity_map returns pydantic BitmapAtWord/LiquidityAtTick matching the oracle."""
        oracle = _expected()["v3_liquidity_map"]
        if oracle is None:
            pytest.skip("oracle has no V3 liquidity map")
        addr = _expected()["v3_pool_address"]
        result = v3_snapshot.get_liquidity_map(addr)
        assert result is not None
        # The tick values are pydantic models (attribute access) — proves the
        # seam wraps into BitmapAtWord(**entry) / LiquidityAtTick(**entry).
        for word, entry in result["tick_bitmap"].items():
            assert int(entry.bitmap) == int(oracle["tick_bitmap"][str(word)]["bitmap"])
        for tick, entry in result["tick_data"].items():
            exp = oracle["tick_data"][str(tick)]
            assert int(entry.liquidity_gross) == int(exp["liquidity_gross"])
            assert int(entry.liquidity_net) == int(exp["liquidity_net"])


class TestV4SnapshotParity:
    """V4 DatabaseSnapshot (Rust-backed) matches the frozen SQLAlchemy oracle."""

    def test_get_newest_block(self, v4_snapshot: V4DatabaseSnapshot) -> None:
        """get_newest_block matches the oracle."""
        assert v4_snapshot.get_newest_block() == _expected()["v4_newest_block"]

    def test_get_pools(self, v4_snapshot: V4DatabaseSnapshot) -> None:
        """get_pools matches the oracle."""
        assert v4_snapshot.get_pools() == set(_expected()["v4_pools"])

    def test_get_all_liquidity_maps(self, v4_snapshot: V4DatabaseSnapshot) -> None:
        """get_all_liquidity_maps matches the oracle tick tuples."""
        oracle_entries = _expected()["v4_all_liquidity_maps"]
        rust = v4_snapshot.get_all_liquidity_maps()
        rust_keys = set(rust)  # (pm_addr, pool_hash)
        for entry in oracle_entries:
            key = (entry["pool_manager"], entry["pool_hash"])
            assert key in rust_keys, f"missing V4 all-maps key {key}"
            rust_ticks = rust[key]
            assert {
                int(t): tuple(map(int, v)) for t, v in entry["ticks"].items()
            } == rust_ticks


# ── §4.5 delegation: Python reader hits the Rust seam with the right args ─


class TestSnapshotDelegation:
    """§4.5: the Python DatabaseSnapshot delegates to the Rust PyDatabaseSnapshot."""

    def test_get_newest_block_delegates_to_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """get_newest_block calls PyDatabaseSnapshot.get_newest_block_v3.

        Patches the lazily-cached `_rust` handle to a recording wrapper so the
        pyclass (whose methods are read-only) need not be monkeypatched.
        """
        calls: list[str] = []
        snap = V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)
        real_rust = snap._rust()

        class _Spy:
            def get_newest_block_v3(self) -> int | None:
                calls.append("get_newest_block_v3")
                return real_rust.get_newest_block_v3()

        monkeypatch.setattr(snap, "_rust_snapshot", _Spy())
        snap.get_newest_block()
        assert calls == ["get_newest_block_v3"]

    def test_get_pools_delegates_to_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """get_pools calls PyDatabaseSnapshot.get_pools_v3."""
        calls: list[str] = []
        snap = V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)
        real_rust = snap._rust()

        class _Spy:
            def get_pools_v3(self) -> set[str]:
                calls.append("get_pools_v3")
                return real_rust.get_pools_v3()

        monkeypatch.setattr(snap, "_rust_snapshot", _Spy())
        snap.get_pools()
        assert calls == ["get_pools_v3"]
