"""The cockpit session's collaborator builders — config or injected.

One :class:`~degenbot.runner.session.Actors` record per session, assembled from
the frozen :class:`~degenbot.runner.session.InjectedActors` DI value: a seam
carries the injected value when one is set, else the production default below.
The builders here are construction mechanics only — they read the typed config
and the actor handles, answer a value, and keep no state; the state they feed
lives on the session.

The production actor trio is built here too (:func:`build_bot` /
:func:`build_async_w3`), so the "injected or from cfg" decision has exactly one
home rather than a facade-side mirror per seam.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot import Bot
from degenbot.dispatch import SimulateContext
from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.runner._dispatch import _load_executor_runtime_bytecode
from degenbot.runner._relay_posture import RelayPosture
from degenbot.runner.identity import (
    MULTICALL3_ADDRESS,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.arbitrage.engine_registry import EngineRegistry
    from degenbot.runner.config import ArbitrageConfig
    from degenbot.strategy import StrategyReadinessView


class ActivationGateRefused(RuntimeError):
    """Strategy readiness (the activation gate) refused a live session.

    The gate refuses an activated facet that fails readiness, so a live boot
    either aborts here or degrades a broadcast to the public mempool; the
    gate's ValueError text becomes the message so the operator keeps the
    gate's own diagnostic unchanged.
    """

    def __init__(self, refusal: ValueError) -> None:
        """Bind the gate's ``ValueError`` text as the refusal message."""
        super().__init__(f"activation gate refused: {refusal}")


def build_bot(cfg: ArbitrageConfig) -> Bot:
    """Build the Python-companion bot actor over the configured node.

    ADR-005: the Bot's build path (ERC20 + V2/V3/V4 pool construction) issues
    many ``eth_call``s via ``BotIo`` → ``provider.call``. A web3.py sync backend
    (``from_web3``) holds the GIL through every ``requests.post`` on the
    event-loop thread, starving the asyncio loop during ``build_paths``. Use the
    Rust ``AlloyProvider`` instead — ``PyAlloyProvider.call`` releases the GIL
    (``py.detach``) and does HTTP in Rust, so the pump/consumer can proceed and
    RPC is faster.

    Returns:
        The bot actor over an Alloy-backed provider.

    """
    # The session chain and the database path resolve through the installed
    # typed config's cascade; `cfg.node_http` is the endpoint the runner's own
    # cascade already resolved, handed over rather than re-resolved.
    alloy = AlloyProvider(cfg.node_http, chain_id=cfg.chain_id)
    return Bot(chain_id=cfg.chain_id, provider=alloy)


async def build_async_w3(cfg: ArbitrageConfig) -> AsyncAlloyProvider:
    """Build the dispatch-path RPC provider.

    An ``AsyncAlloyProvider`` wrapping a Rust ``AsyncAlloyProvider`` — every
    dispatch-side ``eth_*`` call the hot loop makes goes through Rust
    (releasing the GIL), not raw ``AsyncWeb3(AsyncHTTPProvider(...))``. The two
    typed calls (``eth_feeHistory`` / ``eth_sendRawTransaction``) route via
    ``make_request`` on the alloy backend; the generic ones (``get_block`` /
    ``get_transaction_count`` / ``eth_call`` / ``get_code`` /
    ``get_transaction_receipt``) route via the adapter's typed methods.

    Returns:
        An ``AsyncAlloyProvider`` (alloy backend) for the dispatch path.

    """
    return await AsyncAlloyProvider.create(cfg.node_http)


def build_sim_ctx(
    cfg: ArbitrageConfig,
    async_w3: AsyncAlloyProvider,
    engine_registry: EngineRegistry,
) -> SimulateContext | None:
    """Build the session's sim context (``None`` for non-Alloy providers).

    One SimulateContext per session, held alongside the dispatcher. The
    runtime-bytecode file-load stays Python (``stays-python``); the bytes cross
    here. The AsyncAlloyProvider handle is taken from the session's provider so
    the dispatch path shares one provider with the rest of the pipeline.
    Inline-sim wiring rides the same call.

    Args:
        cfg: The session's typed config.
        async_w3: The session's async provider.
        engine_registry: The session's engine registry (the inline-sim install
            target).

    Returns:
        The built sim context, or ``None`` for a non-Alloy provider.

    """
    async_alloy = async_w3.as_async_alloy()
    if async_alloy is None:
        # Non-Alloy provider (test fakes). Defer the sim context:
        # production sessions are Alloy-backed + build it eagerly here;
        # dispatch raises a clear error if reached without one.
        return None
    runtime_code = _load_executor_runtime_bytecode(cfg)
    sim_ctx = SimulateContext(
        provider=async_alloy,
        executor_owner=cfg.executor_owner,
        executor_address=cfg.executor_address,
        weth_address=WETH_ADDRESS,
        pool_manager_address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
        multicall3_address=MULTICALL3_ADDRESS,
        inject_code=cfg.inject_executor_code,
        executor_runtime_bytecode=bytes.fromhex(runtime_code[2:]),
        injected_address=cfg.injected_address if cfg.inject_executor_code else None,
    )
    # The inline-sim stance (`DEGENBOT_SOLVE_INLINE_SIM`) needs the ENGINE hook
    # installed from this session's sim config — without it stance=1 carries no
    # payloads (harmless but inert). Cheap Arc-clone wiring; the engine only
    # calls the hook under the stance, so installing it unconditionally is a
    # no-op when off. The object installed is the PyO3 adapter, not engine
    # policy: it wraps this session's `PySimulateContext` and the Python
    # escalation port, so only this binding layer can build it. That is the
    # permanent reason the install stays here — the core owns the simulator
    # contract and the stance-gated call, and this adapter is its thin bridge.
    # The before-resume placement is structural rather than a separate rule: it
    # is the point where the session's sim context and the engine both exist.
    engine_registry.engine.install_inline_simulator(
        sim_ctx,
        erc6909_profit=cfg.erc6909_profit,
    )
    return sim_ctx


def resolve_readiness(resolver: Callable[[], StrategyReadinessView]) -> StrategyReadinessView:
    """Resolve the activation gate once; a readiness refusal is a boot refusal.

    The refusal rule itself (empty fleet, unsettled endpoints) is owned by the
    Rust config validation the resolver reaches; a caller that needs HOW a
    cascade would resolve an input uses the hypothetical seam instead.

    Args:
        resolver: The readiness resolution to call (the typed-config gate, or
            an injected double).

    Returns:
        The resolved strategy-readiness view.

    Raises:
        ActivationGateRefused: When the Rust validation refuses the boot.

    """
    try:
        return resolver()
    except ValueError as refusal:
        raise ActivationGateRefused(refusal) from refusal


def mint_relay_posture(
    *,
    settlement_active: bool,
    live: bool,
    endpoints: Callable[[], list[str]],
) -> RelayPosture | None:
    """Mint the session's settlement broadcast posture for this boot.

    The readiness gate already refused an empty fleet and an unsettled arm, so
    this reads the resolved settlement arm as DATA: an active arm mints the
    endpoints the Rust settlement composition resolved; an inactive arm (the
    gate guarantees a backrun arm is active) is a backrun-only boot that runs no
    settlement seam and mints no posture. Only a live, settlement-active boot
    mints the posture — a dry-run boot signs nothing.

    Args:
        settlement_active: The arm the readiness resolution settled on.
        live: Whether this boot carries a signing surface.
        endpoints: The settlement broadcast endpoint resolution.

    Returns:
        The settlement broadcast posture, or ``None`` for an inactive arm or a
        dry-run boot.

    Raises:
        ActivationGateRefused: When the endpoint resolution refuses.

    """
    if not settlement_active:
        return None
    try:
        resolved = endpoints()
    except ValueError as refusal:
        raise ActivationGateRefused(refusal) from refusal
    return RelayPosture(relay_urls=resolved) if live else None
