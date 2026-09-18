"""SMOZG3: operator ERC6909 vault-capture toggle reaches the Rust seam.

``_dispatch_profitable`` must project the driver's
``ERC6909_PROFIT`` operator knob (``driver_constants``, env-gated default
off) into the ``assemble_dispatch_candidates(erc6909_profit=...)`` seam so the Rust
strategy's ``resolve_axes`` / ``config_for_options`` axis chain (→
``check_mode=2`` + the pure-V4 ``V4_MINT_COMPACT`` stream) is reachable in
production. The seam's kwarg acceptance itself is pinned by
``tests/rust/test_simulation_seam_classes.py``; this test pins the driver's
projection of the knob into it.
"""

from __future__ import annotations

import pytest

from degenbot.dispatch import Dispatcher
from degenbot.runner import _dispatch as d
from degenbot.runner.bot_runner import _SessionState
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides


class _FakeAssembly:
    """Stand-in for the FFI ``CandidateAssembly`` — records seam kwargs."""

    def __init__(self, **kwargs) -> None:
        self.kwargs = kwargs
        self.empty_hop_path_ids: list[int] = []
        self.candidates = [object()]


class _EngineRegistry:
    engine = object()


def test_erc6909_default_is_off() -> None:
    # Custody capture (the long-standing production behavior) stays the
    # default: the knob is off unless the operator opts in (dotenv or
    # DEGENBOT_ERC6909_PROFIT at the from_env layer).
    cfg = ArbitrageConfig.from_env(
        {"INJECT_EXECUTOR_CODE": "0"},
        live=False,
        permutation=None,
        rpc=RpcCascadeOverrides(cli_http="http://localhost:8545", cli_ws="ws://localhost:8546"),
    )
    assert cfg.erc6909_profit is False


async def test_dispatch_profitable_projects_erc6909_toggle(monkeypatch) -> None:
    recorded: list[dict] = []

    class _Rec(_FakeAssembly):
        def __init__(self, **kwargs) -> None:
            super().__init__(**kwargs)
            recorded.append(kwargs)

    monkeypatch.setattr(d, "assemble_dispatch_candidates", _Rec)

    # One solved result; ``sim_ctx=None`` raises AFTER candidate construction
    # (the RuntimeError is the tripwire that the constructor really ran).
    results = [(1, 100, 5, (105,), (100,), 10, (0,))]
    owner = _SessionState(
        engine_registry=_EngineRegistry(),  # type: ignore[arg-type]
        async_w3=None,  # type: ignore[arg-type] — never read before the sim gate
        sim_ctx=None,
        dispatcher=Dispatcher.for_block(0),
        cfg=ArbitrageConfig.from_env(
            {
                "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
                "OPERATOR_PRIVATE_KEY": "0x"
                + "11" * 32,  # valid secp256k1 scalar, cosmetic (sim gate first)
                "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
                "INJECT_EXECUTOR_CODE": "0",
            },
            live=False,
            permutation=None,
            rpc=RpcCascadeOverrides(cli_http="http://localhost:8545", cli_ws="ws://localhost:8546"),
        ),
        current_block=10,
    )
    cfg_knob_state = owner.cfg.erc6909_profit
    with pytest.raises(RuntimeError, match="SimulateContext is required"):
        await d._dispatch_profitable(
            owner,
            results,
            context=d.BatchContext(block_timestamp=1_700_000_000, base_fee_next=1_000_000_000),
            operator_nonce=0,
        )

    assert len(recorded) == 1, "one assembly call must carry the batch"
    assert recorded[0]["erc6909_profit"] is cfg_knob_state, (
        "the operator knob must be projected into the assembly seam"
    )
