"""Stub for the dynamically-created ``degenbot._ffi.simulation`` submodule.

Created at runtime by ``add_simulation_module`` in the PyO3 wrapper crate
(``degenbot-python/src/simulation/mod.rs``). Holds the per-block profitability
simulation + dispatch classes over the ``degenbot-simulation`` core crate.
"""

from collections.abc import Coroutine
from typing import Any

from degenbot._ffi.provider import AsyncAlloyProvider
from degenbot._ffi.submission import PyDispatcher, PySubmitCandidate
from degenbot.arbitrage.engine_registry import ArbitrageEngine
from degenbot.arbitrage.hop_info import PathInfo

class PySimulateContext:
    """Session-static config bag for :func:`dispatch_profitable_py`.

    Construct once per session from the :class:`AsyncAlloyProvider` + the
    executor/WETH/PoolManager/Multicall3 addresses + the inject flag + the
    executor runtime bytecode. Handed to :func:`dispatch_profitable_py` each
    block alongside the per-block args.
    """

    def __init__(
        self,
        provider: AsyncAlloyProvider,
        executor_owner: str,
        executor_address: str,
        weth_address: str,
        pool_manager_address: str,
        multicall3_address: str,
        inject_code: bool,
        executor_runtime_bytecode: bytes,
        injected_address: str | None = None,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...

class PyDispatchCandidate:
    """Pre-simulation candidate builder (engine result + resolved ``PathInfo``).

    NXM2BF: the candidate resolves its ``composers::PathInfo`` from a
    registered ``path_id`` via ``PyArbitrageEngine.path_info_for_core`` — no
    Python ``PathInfo`` dataclass is threaded. ``engine`` is the
    ``ArbitrageEngine`` that owns ``path_id`` (the same engine that produced
    ``engine_profit`` / ``hop_outputs``).
    """

    def __init__(
        self,
        engine: ArbitrageEngine,
        path_id: int,
        optimal_input: int,
        engine_profit: int,
        hop_outputs: list[int],
        solve_block: int,
        *,
        erc6909_profit: bool = False,
        use_v4_batch: bool = False,
    ) -> None: ...

class PyDispatchOutcome:
    """Read-only outcome of a block's profitable-dispatch fan-out."""

    @property
    def gas_profitable(self) -> list[PySubmitCandidate]: ...
    @property
    def gas_unprofitable_count(self) -> int: ...
    @property
    def exception_count(self) -> int: ...
    @property
    def fail_count(self) -> int: ...
    @property
    def candidate_count(self) -> int: ...
    @property
    def suppressed_count(self) -> int: ...
    @property
    def thin_dropped(self) -> int: ...
    @property
    def fail_buckets(self) -> dict[str, int]: ...
    @property
    def failures(self) -> list[dict[str, Any]]: ...
    @property
    def path_infos(self) -> dict[int, PathInfo]: ...

def dispatch_profitable_py(
    candidates: list[PyDispatchCandidate],
    context: PySimulateContext,
    dispatcher: PyDispatcher,
    base_fee_next: int,
    current_block: int,
    min_profit_net: int,
    min_profit_margin_bps: int,
) -> Coroutine[Any, Any, PyDispatchOutcome]: ...

__all__ = [
    "PyDispatchCandidate",
    "PyDispatchOutcome",
    "PySimulateContext",
    "dispatch_profitable_py",
]
