"""Parity + delegation tests for the V3/V4 snapshot DB seam.

Driven by the frozen `parity_expected.json` oracle (dumped from the
prior legacy reader-backed `DatabaseSnapshot`) + a freshly-regenerated
`parity.db` fixture: the Rust-backed `DatabaseSnapshot` (delegating through
`DatabaseSnapshot`) produces identical results to the frozen legacy reader
oracle. Plus round-trip tests pinning that each PUBLIC read returns exactly
the family's own Rust engine read — the seam wiring, by value: a seam
re-pointed at another family's reader (the pair-keyed V4 one) changes the
value or the key shape, never just a call name.
"""

import json
import pathlib
from collections.abc import Iterator

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import DatabaseSnapshot as EngineSnapshot
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot

FIXTURE_DIR = pathlib.Path("rust/crates/foundation/degenbot-db/tests/fixtures")
DB_PATH = FIXTURE_DIR / "parity.db"
EXPECTED_PATH = FIXTURE_DIR / "parity_expected.json"


def _expected() -> dict:
    return json.loads(EXPECTED_PATH.read_text())


@pytest.fixture(scope="module")
def v3_snapshot() -> Iterator[V3DatabaseSnapshot]:
    snapshot = V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)
    try:
        yield snapshot
    finally:
        snapshot.close()


@pytest.fixture(scope="module")
def v4_snapshot() -> Iterator[V4DatabaseSnapshot]:
    snapshot = V4DatabaseSnapshot(chain_id=8453, database_path=DB_PATH)
    try:
        yield snapshot
    finally:
        snapshot.close()


# ── parity: Rust-backed reads == frozen legacy reader oracle ─────────


class TestV3SnapshotParity:
    """V3 DatabaseSnapshot (Rust-backed) matches the frozen legacy reader oracle."""

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
        """get_liquidity_map returns pydantic LiquidityAtTick rows matching the oracle."""
        oracle = _expected()["v3_liquidity_map"]
        if oracle is None:
            pytest.skip("oracle has no V3 liquidity map")
        addr = _expected()["v3_pool_address"]
        result = v3_snapshot.get_liquidity_map(addr)
        assert result is not None
        # The word-VALUE machinery is retired: the DB stores no per-word bitmap
        # (the `initialization_maps` tables are dropped), so the seam emits a
        # WORD-KEY set that is empty for every DB-backed pool.
        assert result["tick_bitmap"] == set()
        # The tick values are pydantic models (attribute access) — proves the
        # seam wraps into LiquidityAtTick(**entry).
        for tick, entry in result["tick_data"].items():
            exp = oracle["tick_data"][str(tick)]
            assert int(entry.liquidity_gross) == int(exp["liquidity_gross"])
            assert int(entry.liquidity_net) == int(exp["liquidity_net"])


class TestV4SnapshotParity:
    """V4 DatabaseSnapshot (Rust-backed) matches the frozen legacy reader oracle."""

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
            assert {int(t): tuple(map(int, v)) for t, v in entry["ticks"].items()} == rust_ticks


# ── delegation: each public read round-trips the family's engine read ─────


class TestSnapshotDelegation:
    """The Python DatabaseSnapshot returns exactly the V3 engine read.

    The family declares its Rust read seam once (``DatabaseSnapshotSeams``), so
    the public read and the engine's own V3 read are the two sides of one
    delegation: equality proves the declared seam is the V3 one AND pins what
    the facade adds on top (V3's checksum normalization of stored pool keys).
    """

    def test_get_newest_block_round_trips_the_engine_read(self) -> None:
        """get_newest_block returns the engine's V3 newest-block read."""
        engine = EngineSnapshot(chain_id=8453, database_path=str(DB_PATH))
        engine_block = engine.get_newest_block_v3()
        assert engine_block is not None, "the fixture holds a V3 update block"
        with V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH) as snap:
            assert snap.get_newest_block() == engine_block

    def test_get_pools_round_trips_the_engine_read(self) -> None:
        """get_pools returns the engine's V3 pool set, checksum-normalized."""
        engine = EngineSnapshot(chain_id=8453, database_path=str(DB_PATH))
        engine_pools = engine.get_pools_v3()
        assert engine_pools, "the fixture holds V3 pools; an empty read proves nothing"
        expected = {get_checksum_address(pool) for pool in engine_pools}
        with V3DatabaseSnapshot(chain_id=8453, database_path=DB_PATH) as snap:
            assert snap.get_pools() == expected
