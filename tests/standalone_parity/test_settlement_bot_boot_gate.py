"""RSP-8 fixture boot gate — Python-consumer half.

The Python companion to the Rust gate
(rust/examples/settlement_bot/tests/boot_gate.rs): both read the SAME
shared oracle (fixtures/settlement_bot_boot.json) and assert their
driver reaches the recorded decisions. This is the fixture-only,
offline, CI-safe half of the running dual-driver parity gate; the
recorded/anvil half lives in dual_driver_gate.py (gated behind
DEGENBOT_DUAL_DRIVER_GATE=1).

The Python consumer path reaches the fixture boot through the PyO3
seams the Python driver uses:

* S (ledger row 5) via degenbot._ffi.Bot.load_snapshot_from_db —
  the Rust-owned snapshot load, read back through the shared state.
* the candidate-pool graph via degenbot._ffi.build_path_graph — the
  Rust-owned build_path_graph seam the Python pathfinding wrapper
  calls (src/degenbot/pathfinding/_pathfinding.py).

The Rust-example boot applies the 15-token ETH-mainnet discovery
allowlist (parity ledger row 13), which filters the fixture's
candidate token out; the Python probe reads the UNFILTERED graph, so
python_reachable.graph_candidate_tokens is [1] while
expected.graph.candidate_tokens is 0. That deliberate
filtered/unfiltered difference is the documented permitted divergence.

A seeded divergence must fail the comparator: mutate one expected
value in an in-memory copy (the checked-in oracle is never touched)
and assert the real Python decision does not match it.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot._ffi import Bot, build_path_graph

_FIXTURE_DIR = Path(__file__).parent / "fixtures"
_ORACLE_PATH = _FIXTURE_DIR / "settlement_bot_boot.json"
_DB_PATH = Path("rust/crates/degenbot-db/tests/fixtures/parity.db")
_DISCOVERY_CHAIN_ID = 8453
_ALLOWED_STATUSES = {
    "REACHABLE",
    "REACHED-via-EngineDriver",
    "DRIVER-POLICY",
    "PARTIAL",
    "BLOCKED",
}


def _load_oracle() -> dict:
    with _ORACLE_PATH.open() as handle:
        return json.load(handle)


def _python_seed_block() -> int | None:
    """The Python driver's snapshot seed block against the fixture DB."""
    bot = Bot(1)
    bot.load_snapshot_from_db(str(_DB_PATH), 1)
    return bot.snapshot_seed_block


def _python_graph() -> dict:
    """The Python driver's candidate-pool graph against the fixture DB."""
    return build_path_graph(
        database_path=str(_DB_PATH),
        chain_id=_DISCOVERY_CHAIN_ID,
        pool_kinds={0, 1, 2},
        allowed_intermediate_token_ids=None,
    )


def test_python_consumer_boot_decisions_match_shared_oracle() -> None:
    """The Python consumer reaches the shared oracle boot decisions."""
    oracle = _load_oracle()
    expected = oracle["expected"]
    python_reachable = oracle["python_reachable"]

    assert _python_seed_block() == expected["snapshot_seed_block"]
    assert _python_seed_block() == python_reachable["snapshot_seed_block"]

    graph = _python_graph()
    # The Python discovery enumeration count and the Rust discovery count
    # agree on the fixture: one V3 + one V4 pool (ledger row 11).
    assert len(graph["pool_id_to_kind"]) == expected["discovery_count"]
    assert len(graph["pool_id_to_kind"]) == python_reachable["graph_nodes"]
    assert sorted(graph["candidate_tokens"]) == python_reachable["graph_candidate_tokens"]
    assert len(graph["edges"]) == python_reachable["graph_edges"]


def test_oracle_ledger_is_complete_and_typed() -> None:
    """The oracle carries all 20 ledger rows with a typed status."""
    rows = _load_oracle()["expected"]["ledger_rows"]
    assert len(rows) == 20, "one row per Python-driver surface in the ledger"
    assert set(rows.values()) <= _ALLOWED_STATUSES, "statuses use the ledger vocabulary"
    assert rows["06-engine-subscribe-resume"] == "REACHED-via-EngineDriver"


def test_seeded_divergence_in_oracle_fails_the_python_comparator() -> None:
    """Teeth proof: a mutated expected value must not match the real decision.

    Mirrors the Rust seeded_divergence_oracle_mutation_fails test. The
    checked-in oracle is never written; the mutation lives only in memory.
    """
    oracle = _load_oracle()
    mutated = json.loads(json.dumps(oracle))
    mutated["expected"]["snapshot_seed_block"] = 12_340_000
    assert _python_seed_block() != mutated["expected"]["snapshot_seed_block"], (
        "a mutated expected S must fail the Python comparator"
    )

    graph = _python_graph()
    mutated_nodes = oracle["python_reachable"]["graph_nodes"] + 1
    assert len(graph["pool_id_to_kind"]) != mutated_nodes, (
        "a mutated expected graph-node count must fail the Python comparator"
    )
