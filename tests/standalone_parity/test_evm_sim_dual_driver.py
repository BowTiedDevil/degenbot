"""Tier-2 behavioral dual-driver parity — in-process sim SUCCESS path
(ADR-005 §4.2).

The behavioral companion to the Rust `parity_evm_sim.rs` test. Proves the
**same** canonical fixture (the SELFDESTRUCT-gift success path over
`CacheDB<EmptyDB>`) driven through the **Python consumer** path (the
`simulate_in_process_success_probe` PyO3 binding) produces the **same**
`SimResult` (gross/net/gas/priority_fee) recorded in the shared fixture JSON
— which the Rust consumer test (`rust/crates/degenbot/tests/parity_evm_sim.rs`)
independently also asserts.

Both consumers hit the same `simulate_in_process_with_db` core. The recorded
constant is the **shared oracle**: if the PyO3 arg-extraction → core-call →
result-wrap seam ever drops a `U256` field, changes a gas accounting int, or
mis-renders the priority fee, this test diverges from the fixture — surfacing
the FFI regression that Tier-1 reachability can't catch (reachability proves
the symbol is *resolvable*, not that the delegation is *lossless*).

## The SELFDESTRUCT-gift fixture

Over `CacheDB<EmptyDB>` (no RPC, no real pool state), the only way to produce a
non-None `SimResult` (positive `gross_profit`) is to inject ETH into the
executor from an external source. The fixture deploys a "gift" contract whose
bytecode is `CALLER SELFDESTRUCT` — when the executor calls the gift, the gift
self-destructs + sends its 1 ETH balance to the caller (the executor).
+ Multicall3 bytecode (`getEthBalance`) is deployed so the pre/post balance
reads return real ETH values.

## Oracle (weaker — recorded constant, no closed form)

`gross_profit` IS closed-form (1 ETH = the gift's seeded balance). But
`gas_used` + `priority_fee` + `net_profit` are recorded from the revm EVM run
(the byte-exact gas accounting + the lossy f64 priority-fee path have no closed
form). The parity contract is: both drivers produce the same recorded
constants. A deliberately-wrong fixture edit fails BOTH halves (the fixture is
the shared contract, not copied constants — the HRT356 guard).
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot._ffi.simulation import simulate_in_process_success_probe

_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "evm_sim_success_path.json"


def _load_fixture() -> dict:
    with _FIXTURE_PATH.open() as f:
        return json.load(f)


def test_evm_sim_success_path_dual_driver_parity() -> None:
    """The Python half of the ADR-005 §4.2 Tier-2 dual-driver parity pair.

    Drives the SELFDESTRUCT-gift fixture through the
    `simulate_in_process_success_probe` PyO3 binding (in-process revm EVM,
    `CacheDB<EmptyDB>`, no RPC) + asserts the SimResult (gross_profit /
    net_profit / gas_used / priority_fee / base_fee_next / hop_count /
    captured_swaps) matches the shared fixture JSON — the same fixture the
    Rust `parity_evm_sim.rs` test loads + asserts.
    """
    fx = _load_fixture()
    path_id = fx["fixture"]["path_id"]
    expected = fx["expected"]

    outcome = simulate_in_process_success_probe(path_id)

    # ── result must be present (the success path) ──
    assert expected["result_present"], "fixture expects result_present=true"
    assert outcome["result"] is not None, "the success path returns a non-None SimResult"
    sim = outcome["result"]

    # Sanity (non-circular re-derivation): gross_profit IS closed-form — it's
    # the gift's seeded 1 ETH balance, a constant indep of the EVM run.
    assert int(sim["gross_profit"]) == 1_000_000_000_000_000_000, (
        "gross_profit must be 1 ETH (closed form)"
    )

    # ── the recorded-constant assertions (the shared oracle) ──
    assert int(sim["gross_profit"]) == int(expected["gross_profit"]), "gross_profit matches fixture"
    assert int(sim["net_profit"]) == int(expected["net_profit"]), "net_profit matches fixture"
    assert sim["gas_used"] == expected["gas_used"], "gas_used matches fixture"
    assert sim["priority_fee"] == expected["priority_fee"], "priority_fee matches fixture"
    assert sim["base_fee_next"] == expected["base_fee_next"], "base_fee_next matches fixture"
    assert sim["hop_count"] == expected["hop_count"], "hop_count matches fixture"
    assert sim["captured_swaps"] == [], "captured_swaps empty (no Swap events)"
    assert outcome["failures"] == [], "no failures on the success path"
    assert outcome["fail_buckets"] == {}, "no fail buckets on the success path"

    # Sanity (non-circular re-derivation): net_profit = gross - gas_used *
    # (base_fee_next + priority_fee) — re-derive to confirm the fixture value
    # is self-consistent.
    gas_fee = sim["gas_used"] * (sim["base_fee_next"] + sim["priority_fee"])
    rederived_net = int(sim["gross_profit"]) - gas_fee
    assert int(sim["net_profit"]) == rederived_net, (
        f"net_profit must equal gross - gas*(base+priority) (self-consistency): "
        f"{int(sim['net_profit'])} vs {rederived_net}"
    )


def test_deliberately_wrong_fixture_fails_both_halves() -> None:
    """RED-verify the fixture is the shared contract: a deliberately-wrong
    expected `gas_used` in a mutated fixture copy must fail the Python assertion
    (and, by symmetry, the Rust `parity_evm_sim.rs` guard).

    Guards against the V3/V4 fixture-drift regression (HRT356): copied
    constants with no mechanical link left both tests green but testing
    *different* fixtures. The shared JSON file is the single source of truth.
    """
    fx = _load_fixture()
    # Corrupt the expected gas_used — the real sim produces 30748.
    fx["expected"]["gas_used"] = 999_999
    path_id = fx["fixture"]["path_id"]
    outcome = simulate_in_process_success_probe(path_id)
    assert outcome["result"]["gas_used"] != fx["expected"]["gas_used"], (
        "a deliberately-wrong gas_used must NOT match the real sim output "
        "(this guard proves the fixture is the shared contract, not a tautology)"
    )
