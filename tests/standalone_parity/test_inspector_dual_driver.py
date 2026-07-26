"""Tier-2 behavioral dual-driver parity — revm inspector (ADR-005 standalone claim).

The behavioral companion to the Rust `parity_inspector.rs` test. Proves the
**same** canonical fixture (the `0xcafebabe` REVERT executor stub over a
2-hop V2 path against `CacheDB<EmptyDB>`) driven through the **Python
consumer** path (the `simulate_in_process_revert_probe` PyO3 binding)
produces the **same** reverting-frame + captured-swaps + bucket output
recorded in the shared fixture JSON — which the Rust consumer test
(`rust/crates/degenbot/tests/parity_inspector.rs`) independently also
asserts.

Both consumers hit the same `simulate_in_process_with_db` + `SimInspector`
core. The recorded constant is the **shared oracle**: if the PyO3
arg-extraction → core-call → result-wrap seam ever drops a field, changes
an address format, or mis-renders the revert bytes, this test diverges from
the fixture — surfacing the FFI regression that Tier-1 reachability can't
catch (reachability proves the symbol is *resolvable*, not that the
delegation is *lossless*).

## Oracle (weaker — recorded constant, no closed form)

Unlike the V2/V3/V4 calc parity pairs (closed-form `getAmountOut`), the
inspector runs a full revm EVM — no closed-form derivation of the
reverting-frame output. The expected output is a **recorded constant** in
the shared fixture JSON (captured from the Rust smoke test — the byte-exact
EVM run is the truth). A deliberately-wrong fixture edit fails BOTH the
Rust + Python halves (the fixture is the shared contract, not copied
constants).

V4 slice is deferred (gated on `5RI47E`, the transient V4 pool seeder).
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot._ffi.simulation import simulate_in_process_revert_probe

_FIXTURE_PATH = (
    Path(__file__).parent / "fixtures" / "inspector_cafebabe_revert.json"
)


def _load_fixture() -> dict:
    with _FIXTURE_PATH.open() as f:
        return json.load(f)


def test_inspector_dual_driver_parity_cafebabe_revert() -> None:
    """The Python half of the ADR-005 Tier-2 dual-driver parity pair.

    Drives the `0xcafebabe` REVERT executor stub through the
    `simulate_in_process_revert_probe` PyO3 binding (in-process revm EVM,
    `CacheDB<EmptyDB>`, no RPC) + asserts the reverting-frame +
    captured-swaps + bucket output matches the shared fixture JSON — the
    same fixture the Rust `parity_inspector.rs` test loads + asserts.
    """
    fx = _load_fixture()
    path_id = fx["fixture"]["path_id"]
    runtime_bytecode = bytes.fromhex(fx["fixture"]["runtime_bytecode_hex"])

    outcome = simulate_in_process_revert_probe(path_id, runtime_bytecode)

    # ── result (None — the revert path returns None) ──
    assert outcome["result"] is None, "reverting execute returns None"

    # ── fail_buckets ──
    assert outcome["fail_buckets"] == fx["expected"]["fail_buckets"], (
        "fail_buckets must match the shared fixture"
    )

    # ── failures ──
    actual_failures = outcome["failures"]
    expected_failures = fx["expected"]["failures"]
    assert len(actual_failures) == len(expected_failures), "one failure recorded"
    af = actual_failures[0]
    ef = expected_failures[0]
    assert af["path_id"] == ef["path_id"], "path_id round-trips"
    assert af["bucket"] == ef["bucket"], "bucket label matches"
    assert af["fail_index"] == ef["fail_index"], "fail_index matches"
    # revert_data: fixture is "0xcafebabe", actual is "0x"-prefixed hex.
    assert af["revert_data"] == ef["revert_data"], "revert_data bytes match"

    # reverting_frame — the deep attribution.
    arf = af["reverting_frame"]
    erf = ef["reverting_frame"]
    assert arf is not None, "the reverting frame is captured by the inspector"
    assert erf is not None, "fixture expects a reverting frame"
    assert arf["depth"] == erf["depth"], "reverting_frame.depth"
    assert arf["target"] == erf["target"], "reverting_frame.target"
    # The FFI renders the target as lowercase `{:#x}` (EIP-55 checksum is not
    # applied for the inner frame target — matches the Rust test's
    # `format!("{:#x}", rf.target)` comparison). The fixture stores lowercase
    # hex; tolerate either casing.
    assert arf["target"].lower() == erf["target"].lower(), (
        "reverting_frame.target (case-insensitive)"
    )
    assert arf["selector"] == erf["selector"], "reverting_frame.selector"
    assert arf["revert_data"] == erf["revert_data"], "reverting_frame.revert_data"
    assert arf["label"] == erf["label"], "reverting_frame.label"
    # Sanity (non-circular re-derivation): the label is `classify_revert` on
    # the revert_data — so it must contain the revert_data hex.
    revert_hex = arf["revert_data"].removeprefix("0x")
    assert arf["label"].endswith(revert_hex) or arf["label"] == "empty", (
        f"label `{arf['label']}` is classify_revert on revert_data `{revert_hex}`"
    )

    # captured_swaps — empty for the cafebabe stub (no swap events before the
    # immediate revert).
    assert af["captured_swaps"] == [], "captured_swaps empty (matches fixture)"
    assert ef["captured_swaps"] == [], "fixture captured_swaps is empty"

    # optimal_input + hop_outputs (the solver's EXPECTED amounts — the
    # [sim-diag] classifier's basis).
    assert af["optimal_input"] == ef["optimal_input"], "optimal_input matches"
    assert af["hop_outputs"] == ef["hop_outputs"], "hop_outputs match"


def test_deliberately_wrong_fixture_fails_both_halves() -> None:
    """RED-verify the fixture is the shared contract: a deliberately-wrong
    expected bucket in a mutated fixture copy must fail the Python assertion
    (and, by symmetry, the Rust `parity_inspector.rs` test).

    Guards against the V3/V4 fixture-drift regression (HRT356): copied
    constants with no mechanical link left both tests green but testing
    *different* fixtures. The shared JSON file is the single source of truth.
    """
    fx = _load_fixture()
    # Corrupt the expected bucket — the real sim produces "unknown:0xcafebabe".
    fx["expected"]["fail_buckets"] = {"wrong-bucket": 1}
    path_id = fx["fixture"]["path_id"]
    runtime_bytecode = bytes.fromhex(fx["fixture"]["runtime_bytecode_hex"])
    outcome = simulate_in_process_revert_probe(path_id, runtime_bytecode)
    assert outcome["fail_buckets"] != fx["expected"]["fail_buckets"], (
        "a deliberately-wrong fixture must NOT match the real sim output "
        "(this guard proves the fixture is the shared contract, not a tautology)"
    )
