#!/usr/bin/env python3
r"""Generate the mainnet pathfinding snapshot fixture for lossless verification.

Builds TWO files next to this script:

``mainnet_snapshot.db``
    A real Alembic-stamped SQLite database holding a deterministic,
    production-shaped slice of the live mainnet (chain 1) graph:

    - **Anchor core**: every V2/V3 pool whose two tokens are in the
      production intermediate-token whitelist (`ETH_MAINNET_ALLOWED_TOKENS`)
      plus every V4 pool whose two currencies are. This is the hub set the
      backrun discovery sweep cycles through, so the slice reproduces the
      production hub-parallel structure (USDC-WETH carries 15 parallel
      pools, WBTC-WETH 11, ...).
    - **Fringe tendrils**: for each anchor token, the lowest-pool-id pools
      connecting it to a *non-anchor* token. Degree-1 fringe tokens are
      dropped by the candidate filter / 2-core peel; fringe tokens appearing
      in >= 2 selected pools survive as chain-through nodes — dead ends and
      degree-2 chains the graph reductions must handle losslessly.
    - **Native-currency V4 pools**: V4 pools pairing native ETH (the
      ``0x0000...0000`` token row) with an anchor token, covering the
      WETH/native start-set duality the production sweep exercises.

    All rows are copied verbatim (mainnet IDs, checksummed addresses, kind
    strings, exchange identities) so the candidate-token filter, the V4
    namespace offset, and the concrete-type reconstruction run on real data.

``mainnet_snapshot_expected.json``
    The recorded baseline: selected rows, the edge list (namespaced graph
    IDs mirroring the Rust `fetch_path_graph_edges` view), candidate tokens,
    detected bridges, and — the lossless contracts — the exact path-multiset
    hash + count of the current (unoptimized) enumerator for several request
    variants. Pathfinding optimizations (distance pruning, ball contraction,
    through-pool anchoring, parallel-pool bundling) must preserve every
    recorded multiset.

Run (regenerates both files deterministically):

    uv run python tests/pathfinding/fixtures/generate_mainnet_snapshot.py

The baseline hashes are captured from the live implementation at generation
time. Regenerating AFTER an optimization lands freezes a regression in —
only regenerate when deliberately adding structures, and review the hash
diff like a spec change. This mirrors the degenbot-db convention
(``generate_pathfinding.py`` produces the frozen ``pathfinding.db`` +
``pathfinding_expected.json`` parity pair).
"""

from __future__ import annotations

import hashlib
import json
import operator
import sqlite3
from collections import defaultdict
from pathlib import Path

from degenbot.database.operations import create_new_sqlite_database
from degenbot.pathfinding import PathfindingRequest, PoolKind, find_paths
from degenbot.runner._driver_constants import ETH_MAINNET_ALLOWED_TOKENS, WETH_ADDRESS
from degenbot.types.chain import ChainId

FIXTURE_DIR = Path(__file__).resolve().parent
SNAPSHOT_PATH = FIXTURE_DIR / "mainnet_snapshot.db"
EXPECTED_PATH = FIXTURE_DIR / "mainnet_snapshot_expected.json"

LIVE_DB_PATH = Path("~/.local/state/degenbot/db/degenbot.db").expanduser()

CHAIN_ID = ChainId.ETH
NATIVE_ADDRESS = "0x0000000000000000000000000000000000000000"

# Selection caps (deterministic: lowest pool ids first; no randomness).
FRINGE_POOLS_PER_ANCHOR = 6
MAX_V4_NATIVE_POOLS = 300

# Baseline variants: (name, max_depth, pool_types, pool_type_per_depth).
#
# Depth-3 all-kinds is deliberately NOT baselined: on this hub-parallel core
# it yields ~28M paths (native↔native alone 6.4M), which would make capture
# and every future parity run cost minutes for no extra structural coverage —
# the production discovery sweep itself runs permutation-filtered
# (`V3-V4-V3`, `V3-V2`, ... — see `build_paths._parse_permutation_filter`).
# The permutation filters below ARE production shapes and keep each baseline
# in the tens-to-hundreds of thousands.
BASELINE_VARIANTS: list[
    tuple[str, int, list[PoolKind], list[set[PoolKind]] | None]
] = [
    (
        "depth2_all_kinds",
        2,
        [PoolKind.V2, PoolKind.V3, PoolKind.V4],
        None,
    ),
    (
        "depth2_v4_only",
        2,
        [PoolKind.V4],
        None,
    ),
    (
        "depth3_perm_v3v4v3",
        3,
        [PoolKind.V2, PoolKind.V3, PoolKind.V4],
        [
            {PoolKind.V3},
            {PoolKind.V4},
            {PoolKind.V3},
        ],
    ),
    (
        "depth3_perm_v3v2",
        3,
        [PoolKind.V2, PoolKind.V3, PoolKind.V4],
        [
            {PoolKind.V3},
            {PoolKind.V3},
            {PoolKind.V2},
        ],
    ),
]

V4_GRAPH_ID_OFFSET = 1 << 32  # mirrors rust degenbot_db::pathfinding::V4_POOL_ID_OFFSET


def _resolve_token_ids(cursor: sqlite3.Cursor) -> tuple[dict[str, int], list[int], int, list[str]]:
    """Anchor ids (whitelist + WETH), the address→id map, and the native id."""
    addresses = sorted(ETH_MAINNET_ALLOWED_TOKENS | {WETH_ADDRESS, NATIVE_ADDRESS})
    rows = cursor.execute(
        f"SELECT id, address FROM erc20_tokens WHERE chain = ? "
        f"AND address IN ({','.join('?' * len(addresses))})",
        (int(CHAIN_ID), *addresses),
    ).fetchall()
    by_address = {address: int(tid) for tid, address in rows}

    # The production resolver (resolve_token_ids) simply omits whitelist
    # addresses with no DB row — mirror that warn-and-continue so a live DB
    # lagging behind the whitelist still yields a valid snapshot.
    missing = sorted(set(addresses) - set(by_address))
    if missing:
        print(f"note: {len(missing)} anchor tokens absent from live DB: {missing}")

    anchor_ids = sorted(
        {by_address[a] for a in ETH_MAINNET_ALLOWED_TOKENS | {WETH_ADDRESS} if a in by_address}
    )
    return by_address, anchor_ids, by_address[NATIVE_ADDRESS], missing


def _select_pools(
    cursor: sqlite3.Cursor, anchor_ids: list[int], native_id: int
) -> dict[str, list[int]]:
    """Deterministic pool selection: anchor core + capped fringe + V4 native."""
    chain = int(CHAIN_ID)
    ph = ",".join("?" * len(anchor_ids))

    core_v2v3 = [
        int(r[0])
        for r in cursor.execute(
            f"SELECT id FROM pools WHERE chain = ? AND token0_id IN ({ph}) "
            f"AND token1_id IN ({ph}) ORDER BY id",
            (chain, *anchor_ids, *anchor_ids),
        ).fetchall()
    ]

    # Fringe: pools touching exactly ONE anchor token. The lowest pool ids
    # per anchor are almost always anchor-anchor pairs (famous early pools),
    # which the core already carries — exclude them so fringe pools actually
    # reach outside the anchor set.
    fringe_v2v3: set[int] = set()
    for anchor in anchor_ids:
        rows = cursor.execute(
            f"SELECT id FROM pools WHERE chain = ? AND (token0_id = ? OR token1_id = ?) "
            f"AND (token0_id NOT IN ({ph}) OR token1_id NOT IN ({ph})) ORDER BY id LIMIT ?",
            (chain, anchor, anchor, *anchor_ids, *anchor_ids, FRINGE_POOLS_PER_ANCHOR),
        ).fetchall()
        fringe_v2v3.update(int(r[0]) for r in rows)

    core_v4 = [
        int(r[0])
        for r in cursor.execute(
            f"SELECT mp.id FROM uniswap_v4_pools u "
            f"JOIN managed_pools mp ON mp.id = u.managed_pool_id "
            f"JOIN pool_managers pm ON pm.id = mp.manager_id "
            f"WHERE pm.chain = ? AND u.currency0_id IN ({ph}) AND u.currency1_id IN ({ph}) "
            f"ORDER BY mp.id",
            (chain, *anchor_ids, *anchor_ids),
        ).fetchall()
    ]

    v4_native = [
        int(r[0])
        for r in cursor.execute(
            f"SELECT mp.id FROM uniswap_v4_pools u "
            f"JOIN managed_pools mp ON mp.id = u.managed_pool_id "
            f"JOIN pool_managers pm ON pm.id = mp.manager_id "
            f"WHERE pm.chain = ? AND ("
            f"(u.currency0_id = ? AND u.currency1_id IN ({ph})) OR "
            f"(u.currency1_id = ? AND u.currency0_id IN ({ph}))) "
            f"ORDER BY mp.id LIMIT ?",
            (chain, native_id, *anchor_ids, native_id, *anchor_ids, MAX_V4_NATIVE_POOLS),
        ).fetchall()
    ]

    return {
        "core_v2v3": core_v2v3,
        "fringe_v2v3": sorted(fringe_v2v3),
        "core_v4": [pid for pid in core_v4 if pid not in set(v4_native)],
        "v4_native": v4_native,
    }


def _copy_snapshot(
    live: sqlite3.Connection, selection: dict[str, list[int]], live_token_ids: list[int]
) -> None:
    """Create the snapshot DB and copy the selected rows verbatim (ATTACH)."""
    if SNAPSHOT_PATH.exists():
        SNAPSHOT_PATH.unlink()
    create_new_sqlite_database(db_path=SNAPSHOT_PATH)

    pool_ids = sorted(set(selection["core_v2v3"]) | set(selection["fringe_v2v3"]))
    managed_ids = sorted(set(selection["core_v4"]) | set(selection["v4_native"]))
    ph_pools = ",".join("?" * len(pool_ids))
    ph_managed = ",".join("?" * len(managed_ids))

    live.execute("ATTACH DATABASE ? AS snap", (str(SNAPSHOT_PATH),))
    live.execute(
        "INSERT INTO snap.exchanges SELECT * FROM exchanges WHERE chain_id = ?",
        (int(CHAIN_ID),),
    )

    # Tokens: the resolved ids (anchors + native) plus every token referenced
    # by a selected pool (union over both id sources).
    ph_tokens = ",".join("?" * len(live_token_ids))
    live.execute(
        f"""INSERT INTO snap.erc20_tokens
            SELECT * FROM erc20_tokens WHERE id IN (
                SELECT token0_id FROM pools WHERE id IN ({ph_pools})
                UNION SELECT token1_id FROM pools WHERE id IN ({ph_pools})
                UNION SELECT currency0_id FROM uniswap_v4_pools
                WHERE managed_pool_id IN ({ph_managed})
                UNION SELECT currency1_id FROM uniswap_v4_pools
                WHERE managed_pool_id IN ({ph_managed})
                UNION SELECT id FROM erc20_tokens WHERE id IN ({ph_tokens})
            )""",
        (*pool_ids, *pool_ids, *managed_ids, *managed_ids, *live_token_ids),
    )
    live.execute(f"INSERT INTO snap.pools SELECT * FROM pools WHERE id IN ({ph_pools})", pool_ids)
    for subtable in ("uniswap_v2_pools", "uniswap_v3_pools"):
        live.execute(
            f"INSERT INTO snap.{subtable} SELECT * FROM {subtable} WHERE pool_id IN ({ph_pools})",
            pool_ids,
        )
    live.execute(
        f"""INSERT INTO snap.pool_managers
            SELECT DISTINCT pm.* FROM pool_managers pm
            JOIN managed_pools mp ON mp.manager_id = pm.id
            WHERE mp.id IN ({ph_managed})""",
        managed_ids,
    )
    live.execute(
        f"INSERT INTO snap.managed_pools SELECT * FROM managed_pools WHERE id IN ({ph_managed})",
        managed_ids,
    )
    live.execute(
        f"INSERT INTO snap.uniswap_v4_pools SELECT * FROM uniswap_v4_pools "
        f"WHERE managed_pool_id IN ({ph_managed})",
        managed_ids,
    )
    live.commit()
    live.execute("DETACH DATABASE snap")


def _candidate_tokens(snap: sqlite3.Connection) -> list[int]:
    """Tokens appearing in >= 2 pools (mirrors fetch_tokens_with_min_degree)."""
    chain = int(CHAIN_ID)
    rows = snap.execute(
        "SELECT token_id, count(*) AS c FROM ("
        " SELECT token0_id AS token_id FROM pools WHERE chain = ?"
        " UNION ALL SELECT token1_id FROM pools WHERE chain = ?"
        " UNION ALL SELECT u.currency0_id FROM uniswap_v4_pools u"
        " JOIN managed_pools mp ON mp.id = u.managed_pool_id"
        " UNION ALL SELECT u.currency1_id FROM uniswap_v4_pools u"
        " JOIN managed_pools mp ON mp.id = u.managed_pool_id"
        ") GROUP BY token_id HAVING c >= 2 ORDER BY token_id",
        (chain, chain),
    ).fetchall()
    return [int(r[0]) for r in rows]


def _detect_bridges(snap: sqlite3.Connection, candidates: list[int]) -> list[list[int]]:
    """Bridges of the candidate-filtered V2/V3+V4 multigraph.

    Token pairs carrying >= 2 pools can never be bridges, so the search runs
    on the simple graph of single-pool token pairs only. Iterative Tarjan
    low-link (recursive form would blow the stack at hub-degree scale).
    """
    candidate_set = set(candidates)
    pair_multiplicity: defaultdict[tuple[int, int], int] = defaultdict(int)
    chain = int(CHAIN_ID)
    for t0, t1 in snap.execute(
        "SELECT token0_id, token1_id FROM pools WHERE chain = ?", (chain,)
    ):
        if int(t0) in candidate_set and int(t1) in candidate_set:
            key = (int(t0), int(t1)) if int(t0) < int(t1) else (int(t1), int(t0))
            pair_multiplicity[key] += 1
    for c0, c1 in snap.execute(
        "SELECT u.currency0_id, u.currency1_id FROM uniswap_v4_pools u "
        "JOIN managed_pools mp ON mp.id = u.managed_pool_id"
    ):
        if int(c0) in candidate_set and int(c1) in candidate_set:
            key = (int(c0), int(c1)) if int(c0) < int(c1) else (int(c1), int(c0))
            pair_multiplicity[key] += 1

    single_pairs = [(a, b) for (a, b), m in pair_multiplicity.items() if m == 1]
    adj: defaultdict[int, list[tuple[int, int]]] = defaultdict(list)
    for eid, (a, b) in enumerate(single_pairs):
        adj[a].append((b, eid))
        adj[b].append((a, eid))

    disc: dict[int, int] = {}
    low: dict[int, int] = {}
    bridges: set[tuple[int, int]] = set()
    timer = 0
    for root in adj:
        if root in disc:
            continue
        disc[root] = low[root] = timer
        timer += 1
        # Frame: (node, arrival edge id — None for the root, next edge idx, edge count).
        stack: list[tuple[int, int | None, int, int]] = [(root, None, 0, len(adj[root]))]
        while stack:
            node, parent_edge, idx, edge_count = stack[-1]
            if idx >= edge_count:
                stack.pop()
                if stack:
                    parent_node, grandparent_edge, _idx, _count = stack[-1]
                    low[parent_node] = min(low[parent_node], low[node])
                    if low[node] > disc[parent_node] and grandparent_edge is not None:
                        a, b = single_pairs[grandparent_edge]
                        bridges.add((a, b) if a < b else (b, a))
                continue
            stack[-1] = (node, parent_edge, idx + 1, edge_count)
            child, eid = adj[node][idx]
            if eid == parent_edge:
                # Do not walk back over the arrival edge itself; with parallel
                # pairs excluded this only happens via child == parent.
                continue
            if child in disc:
                low[node] = min(low[node], disc[child])
            else:
                disc[child] = low[child] = timer
                timer += 1
                stack.append((child, eid, 0, len(adj[child])))

    return sorted([a, b] for a, b in bridges)


def _capture_baselines(snapshot_path: Path) -> dict[str, dict[str, object]]:
    """Hash the path multiset the CURRENT enumerator yields per request variant.

    Paths are canonicalized as sorted tuples of step identifiers
    (pool address, or V4 pool hash), the same convention
    ``tests/pathfinding/test_pathfinding.py::path_step_identifiers`` uses.
    """
    out: dict[str, dict[str, object]] = {}
    for name, depth, pool_types, per_depth in BASELINE_VARIANTS:
        request = PathfindingRequest(
            database_path=snapshot_path,
            chain_id=CHAIN_ID,
            start_tokens=[WETH_ADDRESS, NATIVE_ADDRESS],
            end_tokens=[WETH_ADDRESS, NATIVE_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
            pool_type_per_depth=per_depth,
        )
        paths = sorted(
            tuple((step.hash or step.address) for step in steps)
            for steps in find_paths(request=request)
        )
        canonical = "\n".join(json.dumps(p) for p in paths)
        out[name] = {
            "path_count": len(paths),
            "multiset_sha256": hashlib.sha256(canonical.encode()).hexdigest(),
            "head_sample": [json.dumps(p) for p in paths[:20]],
        }
    return out


def main() -> None:
    live = sqlite3.connect(f"file:{LIVE_DB_PATH}?mode=ro", uri=True)
    try:
        by_address, anchor_ids, native_id, missing_anchors = _resolve_token_ids(live.cursor())
        live_token_ids = sorted(set(anchor_ids) | {native_id})
        selection = _select_pools(live.cursor(), anchor_ids, native_id)

        _copy_snapshot(live, selection, live_token_ids)

        snap = sqlite3.connect(f"file:{SNAPSHOT_PATH}?mode=ro", uri=True)
        try:
            edges_v23 = snap.execute(
                "SELECT token0_id, token1_id, id, kind FROM pools WHERE chain = ? ORDER BY id",
                (int(CHAIN_ID),),
            ).fetchall()
            edges_v4 = snap.execute(
                "SELECT currency0_id, currency1_id, managed_pool_id "
                "FROM uniswap_v4_pools ORDER BY managed_pool_id",
            ).fetchall()
            candidates = _candidate_tokens(snap)
            bridges = _detect_bridges(snap, candidates)
            snap_counts = {
                table: snap.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
                for table in (
                    "exchanges",
                    "erc20_tokens",
                    "pools",
                    "uniswap_v2_pools",
                    "uniswap_v3_pools",
                    "pool_managers",
                    "managed_pools",
                    "uniswap_v4_pools",
                )
            }
        finally:
            snap.close()

        baselines = _capture_baselines(SNAPSHOT_PATH)

        expected = {
            "schema_version": 1,
            "chain_id": int(CHAIN_ID),
            "native_token_id": native_id,
            "anchor_token_ids": anchor_ids,
            "anchor_id_to_address": {
                str(tid): addr
                for addr, tid in sorted(by_address.items(), key=operator.itemgetter(1))
            },
            "selection": {
                "anchor_addresses_absent_from_live_db": missing_anchors,
                "fringe_pools_per_anchor": FRINGE_POOLS_PER_ANCHOR,
                "max_v4_native_pools": MAX_V4_NATIVE_POOLS,
                "counts": {
                    "core_v2v3_pools": len(selection["core_v2v3"]),
                    "fringe_v2v3_pools": len(selection["fringe_v2v3"]),
                    "core_v4_pools": len(selection["core_v4"]),
                    "v4_native_pools": len(selection["v4_native"]),
                    "distinct_tokens": snap_counts["erc20_tokens"],
                    "candidate_tokens": len(candidates),
                },
            },
            "candidate_tokens": candidates,
            "detected_bridges": bridges,
            "edges_v2v3": [
                {"token0_id": int(t0), "token1_id": int(t1), "pool_id": int(pid), "kind": str(kind)}
                for t0, t1, pid, kind in edges_v23
            ],
            # Mirrors the Rust consumer view: graph ids carry the V4 offset.
            "edges_v4": [
                {
                    "currency0_id": int(c0),
                    "currency1_id": int(c1),
                    "managed_pool_id": int(mp_id),
                    "graph_pool_id": int(mp_id) + V4_GRAPH_ID_OFFSET,
                }
                for c0, c1, mp_id in edges_v4
            ],
            "baselines": baselines,
        }
        EXPECTED_PATH.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")

        print(f"snapshot rows: {json.dumps(snap_counts, sort_keys=True)}")
        print(f"bridges in candidate-filtered graph: {bridges}")
        for name, info in baselines.items():
            digest = str(info["multiset_sha256"])
            print(f"baseline {name}: count={info['path_count']} sha256={digest[:16]}...")
        print(f"wrote {EXPECTED_PATH}")
    finally:
        live.close()


if __name__ == "__main__":
    main()
