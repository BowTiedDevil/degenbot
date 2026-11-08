"""Drive a cockpit session's block clock through the shipped consumer seam.

The Python cockpit owns no consume loop: the ordered stream, the block clock,
the batch-work derivation, the loud-abort rule, and the session close are the
Rust core's (``degenbot_batch_executor::session``), driven through
:class:`degenbot.dispatch.SessionConsumer`. A cockpit test that needs the
session's clock to move therefore replays the engine's own stream shape through
:meth:`SessionConsumer.scripted` — the shared oracle's door
(``tests/standalone_parity/fixtures/session_consumption.json``) — and lets the
cockpit adapter (:func:`degenbot.runner._consume.consume_result_batches`) drain
the record stream beside it.

The executor is a REAL core executor over a dead-port provider: alloy's
transport is lazy and a block-only script resolves no path, so the drive stays
offline and deterministic. The session's dispatcher is a real
:class:`Dispatcher`, which is what makes the consumer's clock write observable
through ``Session.current_block`` — the session keeps no mirror of it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.dispatch import BatchExecutor, SessionConsumer, build_batch_executor
from degenbot.runner._consume import consume_result_batches
from tests.helpers.seam import (
    AlloyProvider,
    ArbitrageEngine,
    AsyncAlloyProvider,
    Dispatcher,
    SimulateContext,
    TxSigner,
)
from tests.helpers.session import session_for_test

if TYPE_CHECKING:
    from degenbot.runner.session import Session


#: A dead port: alloy's transport is lazy, and a block-only script dials nothing.
_RPC_URL = "http://127.0.0.1:1"

# The operator account the fixture signer holds (key ``0x11..11``). The batch
# composition refuses a deployment whose owner is not the signing account.
_OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"
_EXECUTOR = "0x" + "aa" * 20
_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
_POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"


def block_event(
    number: int,
    *,
    timestamp: int = 1_700_000_000,
    base_fee_per_gas: int = 1_000_000_000,
    gas_used: int = 15_000_000,
    gas_limit: int = 30_000_000,
) -> dict[str, Any]:
    """One stream header in the engine's Python shape (the ``block`` arm).

    Args:
        number: The header's block number.
        timestamp: The header's timestamp.
        base_fee_per_gas: The header's base fee.
        gas_used: The header's gas used.
        gas_limit: The header's gas limit.

    Returns:
        The scripted event dict the consumer's ``scripted`` door parses.

    """
    return {
        "kind": "block",
        "number": number,
        "timestamp": timestamp,
        "base_fee_per_gas": base_fee_per_gas,
        "gas_used": gas_used,
        "gas_limit": gas_limit,
    }


def offline_executor(dispatcher: Any, *, engine: Any | None = None) -> BatchExecutor:
    """Build the core session executor over a dead-port provider.

    The session's executor is the core's, so the construction needs the whole
    boundary (context, engine, signer, provider). A block-only script resolves
    no path, so a bare engine stands in for the session's and nothing dials.

    Args:
        dispatcher: The session's dispatch state (the consumer's clock).
        engine: The engine whose resolved paths the script may name; a bare one
            when the script resolves nothing.

    Returns:
        The constructed core executor.

    """
    provider = AsyncAlloyProvider(AlloyProvider(_RPC_URL))
    return build_batch_executor(
        context=SimulateContext(
            provider=provider,
            executor_owner=_OWNER,
            executor_address=_EXECUTOR,
            weth_address=_WETH,
            pool_manager_address=_POOL_MANAGER,
            inject_code=False,
        ),
        dispatcher=dispatcher,
        engine=ArbitrageEngine() if engine is None else engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        operator_nonce=5,
        dry_run=True,
        broadcast_providers=None,
    )


async def session_with_real_clock(
    *,
    start_block: int = 12_345,
    **actor_overrides: Any,
) -> tuple[Session, BatchExecutor]:
    """A started session whose block clock is a real :class:`Dispatcher`.

    The attached pipeline is the core executor built over that same dispatcher,
    so a consumer's clock write lands on the value ``Session.current_block``
    reads.

    Args:
        start_block: The block the dispatcher starts at.
        **actor_overrides: Collaborator seams the test replaces.

    Returns:
        The started session and its attached executor.

    """
    dispatcher = Dispatcher.for_block(start_block)
    executor = offline_executor(dispatcher)
    session = session_for_test(
        dispatcher=dispatcher,
        pipeline_factory=lambda _session: executor,
        **actor_overrides,
    )
    await session.build_pipeline()
    return session, executor


async def drive_session(session: Session, events: list[dict[str, Any]]) -> SessionConsumer:
    """Replay one scripted stream through the session's consumer and its drain.

    The consumer is the shipped one (scripted over the engine's stream shape);
    the drain is the cockpit adapter, so a test observes exactly what a live
    session would: the core's clock write, the render fold over the drained
    records, and the end verdict.

    Args:
        session: The started session whose clock and pipeline the drive uses.
        events: The scripted stream events, in arrival order.

    Returns:
        The consumer the drive ran (its ``beats``/``progress`` are readable).

    Raises:
        AssertionError: If the session has no attached core executor.

    """
    executor = session.sim_submit_pipeline
    assert isinstance(executor, BatchExecutor), "the session attaches a core executor"
    consumer = SessionConsumer.scripted(executor, events, allow_quiet_end=True)
    await consume_result_batches(session.block_loop(), consumer)
    return consumer
