"""The ERC6909 encode-axis projection at the executor construction boundary.

The driver's ``dispatch.erc6909_profit`` operator knob projects into the
core batch executor's construction (``build_batch_executor_py``), which
stamps it onto every assembled candidate's ``EncodeOptions`` — the same
``resolve_axes``/``config_for_options`` axis chain the pre-cut-over
``assemble_dispatch_candidates(erc6909_profit=...)`` seam carried. The seam's
kwarg acceptance itself is pinned by ``tests/rust/test_simulation_seam_classes.py``;
this test pins the driver's projection of the knob into the construction.
"""

from __future__ import annotations

from typing import Any

import pytest

from degenbot._ffi.provider import AlloyProvider, AsyncAlloyProvider
from degenbot._ffi.simulation import SimulateContext
from degenbot.dispatch import Dispatcher, TxSigner
from degenbot.runner import _sim_submit as sim_submit_module
from degenbot.runner._sim_submit import build_batch_executor
from degenbot.runner.bot_runner import _SessionState
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.helpers.identity_env import identity_env

_RPC_URL = "http://127.0.0.1:1"  # a dead port — alloy's transport is lazy


def _provider() -> AsyncAlloyProvider:
    return AsyncAlloyProvider(AlloyProvider(_RPC_URL))


def _ctx() -> SimulateContext:
    return SimulateContext(
        provider=_provider(),
        executor_owner="0x9c56a29c7231974c269e24f9fb3c29203039089e",
        executor_address="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        weth_address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        pool_manager_address="0x000000000004444C5DC75cB358380d2e3dE08a90",
        multicall3_address="0xcA11bde05977b3631167028862bE2a173976CA11",
        inject_code=False,
        executor_runtime_bytecode=b"\xde\xad\xbe\xef",
    )


class _FakeW3:
    """The construction boundary's provider reads: the alloy handle + nonce."""

    def as_async_alloy(self) -> AsyncAlloyProvider:
        return _provider()

    async def get_transaction_count(self, address: str) -> int:
        return 7


def _cfg() -> ArbitrageConfig:
    with identity_env({
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "11" * 32,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
    }):
        return ArbitrageConfig.build(
            live=False,
            permutation=None,
            rpc=RpcCascadeOverrides(node="ws://localhost:8546"),
        )


def _session(engine: Any, cfg: ArbitrageConfig) -> _SessionState:
    return _SessionState(
        engine_registry=engine,  # type: ignore[arg-type]
        async_w3=_FakeW3(),  # type: ignore[arg-type]
        sim_ctx=_ctx(),
        dispatcher=Dispatcher.for_block(0),
        cfg=cfg,
        current_block=10,
    )


def test_erc6909_default_is_off() -> None:
    """Custody capture stays the default: the declared key is off."""
    assert _cfg().erc6909_profit is False


async def test_construction_projects_the_erc6909_toggle(monkeypatch) -> None:
    """The operator knob rides the construction boundary into the executor."""
    recorded: dict[str, Any] = {}

    def recorder(**kwargs: Any) -> str:
        recorded.update(kwargs)
        return "executor"

    monkeypatch.setattr(sim_submit_module, "build_batch_executor_py", recorder)

    engine_registry = type("R", (), {"engine": "engine"})()
    cfg = _cfg()
    await build_batch_executor(_session(engine_registry, cfg))

    assert recorded["erc6909_profit"] is cfg.erc6909_profit, (
        "the operator knob must project into the executor construction"
    )
    assert recorded["min_profit_margin_bps"] == cfg.min_profit_margin_bps
    assert recorded["dry_run"] is cfg.dry_run
    assert recorded["inject_code_guard"] is cfg.inject_executor_code


async def test_a_session_without_a_sim_context_refuses_construction() -> None:
    """The pinned loud refusal survives the cut-over (the pre-cut-over
    ``_run_sim`` tripwire)."""
    engine_registry = type("R", (), {"engine": "engine"})()
    session = _session(engine_registry, _cfg())
    session.sim_ctx = None  # type: ignore[assignment]
    with pytest.raises(RuntimeError, match="SimulateContext is required"):
        await build_batch_executor(session)


def _unused_tx_signer_guard() -> None:
    # TxSigner construction stays driver-side (the key never leaves Python's
    # config); import-keep so the surface is checked.
    _ = TxSigner
