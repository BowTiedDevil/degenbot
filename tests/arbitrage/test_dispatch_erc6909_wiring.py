"""Operator ERC6909 vault-capture toggle reaches the Rust assembly seam.

The sim-submit pipeline's simulate leaf (``_run_sim``) must project the
driver's ``ERC6909_PROFIT`` operator knob (``driver_constants``, env-gated
default off) into the ``assemble_dispatch_candidates(erc6909_profit=...)``
seam so the Rust strategy's ``resolve_axes`` / ``config_for_options`` axis
chain (→ ``check_mode=2`` + the pure-V4 ``V4_MINT_COMPACT`` stream) is
reachable in production. The seam's kwarg acceptance itself is pinned by
``tests/rust/test_simulation_seam_classes.py``; this test pins the driver's
projection of the knob into it.
"""

from __future__ import annotations

import pytest

from degenbot.dispatch import Dispatcher
from degenbot.runner import _dispatch as d
from degenbot.runner._sim_submit import BatchWork, _run_sim
from degenbot.runner.bot_runner import _SessionState
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.helpers.identity_env import identity_env


class _FakeAssembly:
    """Stand-in for the FFI ``CandidateAssembly`` — records seam kwargs."""

    def __init__(self, **kwargs) -> None:
        self.kwargs = kwargs
        self.empty_hop_path_ids: list[int] = []
        self.candidates = [object()]


class _EngineRegistry:
    engine = object()


def test_erc6909_default_is_off() -> None:
    """Custody capture stays the default: the declared key is off.

    ``dispatch.erc6909_profit`` declares ``false``, so a process that names no
    layer runs the custody-transfer path. The opt-in is the key (env or file).
    """
    cfg = ArbitrageConfig.build(
        live=False,
        permutation=None,
        rpc=RpcCascadeOverrides(node="ws://localhost:8546"),
    )
    assert cfg.erc6909_profit is False


async def test_run_sim_projects_erc6909_toggle(monkeypatch) -> None:
    recorded: list[dict] = []

    class _Rec(_FakeAssembly):
        def __init__(self, **kwargs) -> None:
            super().__init__(**kwargs)
            recorded.append(kwargs)

    monkeypatch.setattr(d, "assemble_dispatch_candidates", _Rec)

    # One solved result; ``sim_ctx=None`` makes the simulate leaf raise AFTER
    # candidate construction (the RuntimeError is the tripwire that the
    # constructor really ran).
    results = [(1, 100, 5, (105,), (100,), 10, (0,))]
    with identity_env(
        {
            "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
            "OPERATOR_PRIVATE_KEY": "0x" + "11" * 32,
            "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        }
    ):
        cfg = ArbitrageConfig.build(
            live=False,
            permutation=None,
            rpc=RpcCascadeOverrides(node="ws://localhost:8546"),
        )
    owner = _SessionState(
        engine_registry=_EngineRegistry(),  # type: ignore[arg-type]
        async_w3=None,  # type: ignore[arg-type] — never read before the sim gate
        sim_ctx=None,
        dispatcher=Dispatcher.for_block(0),
        cfg=cfg,
        current_block=10,
    )
    cfg_knob_state = owner.cfg.erc6909_profit
    with pytest.raises(RuntimeError, match="SimulateContext is required"):
        await _run_sim(
            owner,
            BatchWork(
                results=results,
                block_timestamp=1_700_000_000,
                base_fee_next=1_000_000_000,
                current_block=10,
            ),
        )

    assert len(recorded) == 1, "one assembly call must carry the batch"
    assert recorded[0]["erc6909_profit"] is cfg_knob_state, (
        "the operator knob must be projected into the assembly seam"
    )
