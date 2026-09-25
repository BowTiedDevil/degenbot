"""Lossless-contract tests over the frozen mainnet snapshot.

The snapshot DB (`mainnet_snapshot.db`) is a deterministic, production-shaped
slice of the live mainnet graph — anchor-token hub core with heavy pool
parallelism (15x USDC-WETH), V4 native-currency pools, and fringe tendrils
that contribute degree-1 dead ends, degree-2 chain-through tokens, and four
bridges inside the candidate-filtered graph (see
`generate_mainnet_snapshot.py` for the selection contract).

`mainnet_snapshot_expected.json` records, per request variant, the exact
path count + multiset hash of the CURRENT (unoptimized) enumerator. Every
pathfinding optimization (distance pruning, ball contraction, through-pool
anchoring, parallel-pool bundling) MUST preserve every recorded multiset —
these hashes are the lossless contract, captured before any optimization
landed. Do not regenerate the fixture to make a failing test pass: a changed
hash is a spec change to be reviewed, not a failure to be silenced.
"""

import hashlib
import json
import shutil
import sqlite3
from collections import defaultdict
from pathlib import Path

import pytest

from degenbot.database.models.pools import LiquidityPoolTable, UniswapV4PoolTable
from degenbot.pathfinding import PathfindingRequest, find_paths

FIXTURE_DIR = Path(__file__).resolve().parent / "fixtures"
SNAPSHOT_PATH = FIXTURE_DIR / "mainnet_snapshot.db"
EXPECTED_PATH = FIXTURE_DIR / "mainnet_snapshot_expected.json"

WETH_MAINNET = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
NATIVE_MAINNET = "0x0000000000000000000000000000000000000000"

pytestmark = pytest.mark.skipif(
    not SNAPSHOT_PATH.exists(),
    reason="run tests/pathfinding/fixtures/generate_mainnet_snapshot.py to build the snapshot",
)


def _expected() -> dict:
    return json.loads(EXPECTED_PATH.read_text())


def _snapshot_copy(tmp_path: Path) -> Path:
    """Per-test writable copy (SQLite journals need a writable file)."""
    work = tmp_path / "mainnet_snapshot.db"
    shutil.copy(SNAPSHOT_PATH, work)
    return work


def _multiset_sha256(request: PathfindingRequest) -> tuple[int, str]:
    paths = sorted(
        tuple((step.hash or step.address) for step in steps)
        for steps in find_paths(request=request)
    )
    canonical = "\n".join(json.dumps(p) for p in paths)
    return len(paths), hashlib.sha256(canonical.encode()).hexdigest()


def test_snapshot_graph_facts_match_expected() -> None:
    """The committed DB must match its own expected JSON — this catches
    accidental regeneration with different selection parameters."""
    expected = _expected()
    counts = expected["selection"]["counts"]
    snap = sqlite3.connect(f"file:{SNAPSHOT_PATH}?mode=ro", uri=True)
    try:
        n_pools = snap.execute("SELECT count(*) FROM pools WHERE chain = 1").fetchone()[0]
        assert n_pools >= counts["core_v2v3_pools"]
        n_tokens = snap.execute("SELECT count(*) FROM erc20_tokens").fetchone()[0]
        assert n_tokens == counts["distinct_tokens"]
        n_v4 = snap.execute("SELECT count(*) FROM uniswap_v4_pools").fetchone()[0]
        assert n_v4 == counts["core_v4_pools"] + counts["v4_native_pools"]
        candidates = set(expected["candidate_tokens"])
        assert len(candidates) == counts["candidate_tokens"]
        for bridge in expected["detected_bridges"]:
            a, b = bridge
            assert a in candidates
            assert b in candidates
    finally:
        snap.close()


def test_depth2_all_kinds_matches_baseline(tmp_path: Path) -> None:
    """THE lossless wall: enumeration over the snapshot must reproduce the
    recorded pre-optimization multiset exactly."""
    expected = _expected()["baselines"]["depth2_all_kinds"]
    database_path = _snapshot_copy(tmp_path)
    count, digest = _multiset_sha256(
        PathfindingRequest(
            database_path=database_path,
            chain_id=1,
            start_tokens=[WETH_MAINNET, NATIVE_MAINNET],
            end_tokens=[WETH_MAINNET, NATIVE_MAINNET],
            max_depth=2,
            pool_types=[LiquidityPoolTable, UniswapV4PoolTable],
        )
    )
    assert count == expected["path_count"]
    assert digest == expected["multiset_sha256"]


def test_recorded_bridges_are_single_pool_pairs() -> None:
    """Soundness canary for the bridge/2-edge-connected-component
    optimization: a recorded bridge must be a token pair carried by exactly
    one pool (parallel pools make a pair uncuttable)."""
    expected = _expected()
    snap = sqlite3.connect(f"file:{SNAPSHOT_PATH}?mode=ro", uri=True)
    try:
        multiplicity: dict[tuple[int, int], int] = defaultdict(int)
        for t0, t1 in snap.execute(
            "SELECT token0_id, token1_id FROM pools WHERE chain = 1"
        ):
            key = (t0, t1) if t0 < t1 else (t1, t0)
            multiplicity[key] += 1
        for c0, c1 in snap.execute(
            "SELECT u.currency0_id, u.currency1_id FROM uniswap_v4_pools u "
            "JOIN managed_pools mp ON mp.id = u.managed_pool_id"
        ):
            key = (c0, c1) if c0 < c1 else (c1, c0)
            multiplicity[key] += 1
    finally:
        snap.close()
    for bridge in expected["detected_bridges"]:
        a, b = bridge
        assert multiplicity[a, b] == 1, f"({a}, {b}) must be a single-pool pair"
