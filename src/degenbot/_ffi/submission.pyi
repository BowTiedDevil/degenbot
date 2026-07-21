"""Stub for the dynamically-created ``degenbot._ffi.submission`` submodule.

Created at runtime by ``add_submission_module`` in the PyO3 wrapper crate
(``degenbot-python/src/submission/mod.rs``). Holds the dispatcher + signer +
tx-params + submit-candidate classes + the dispatch/submit functions.
"""

from collections.abc import Coroutine
from typing import Any

from degenbot._ffi.provider import AsyncAlloyProvider

class PyDispatcher:
    """Coordination-state value object for the dispatch loop.

    Holds the Rust-owned ``Dispatcher`` behind ``Arc<Mutex<...>>`` so the
    submit/fee/monitor Rust leaves lock the SAME state Python drives. Construct
    via :meth:`for_block`.
    """

    def __init__(self) -> None: ...
    @staticmethod
    def for_block(current_block: int) -> PyDispatcher: ...
    # ── nonce coordination ──
    def claim_nonce(self, start: int) -> int: ...
    def release_nonce(self, nonce: int) -> None: ...
    # ── pool mutual exclusion ──
    def reserve_pools(self, path_pools: set[str]) -> None: ...
    def release_pools(self, pools: set[str]) -> None: ...
    def is_path_blocked(
        self,
        path_pools: set[str],
        committed_pools: set[str],
    ) -> bool: ...
    def release_tx(self, tx: tuple[int, set[str]]) -> None: ...
    # ── task tracking ──
    def active_task_count(self) -> int: ...
    def reap_finished(self) -> int: ...
    def abort_all_tasks(self) -> None: ...
    # ── block / fee recording ──
    @property
    def current_block(self) -> int: ...
    def advance_block(self, block: int) -> None: ...
    def record_block_time(self, block: int, timestamp: int) -> None: ...
    def record_priority_fees(self, block: int, fees: dict[int, int]) -> None: ...
    def latest_priority_fees(self) -> dict[int, int]: ...
    @property
    def block_priority_fees(self) -> dict[int, dict[int, int]]: ...
    # ── block-time ring ──
    def block_time_count(self) -> int: ...
    def block_times_oldest(self) -> tuple[int, int]: ...
    # ── PathSuppression delegation ──
    def record_success(self, path_id: int) -> None: ...
    def record_failure(self, path_id: int) -> None: ...
    def is_suppressed(self, path_id: int, current_block: int) -> bool: ...
    def total_suppressed(self) -> int: ...
    def discard_path(self, path_id: int) -> None: ...
    # ── introspection ──
    def pending_nonce_count(self) -> int: ...
    def pending_pool_count(self) -> int: ...

class PyTxSigner:
    """EIP-1559 transaction signer holding the operator key once.

    The private key (hex string or 32 raw bytes) crosses into Rust at
    construction and never round-trips back to Python per transaction.
    """

    def __init__(self, key: str | bytes, chain_id: int) -> None: ...
    @property
    def address(self) -> str: ...
    @property
    def chain_id(self) -> int: ...
    def sign_eip1559(self, tx_params: PyTxParams) -> bytes: ...
    @staticmethod
    def recover_sender(raw_signed: bytes) -> str: ...

class PyTxParams:
    """EIP-1559 transaction field set (minus ``chain_id``, owned by ``PyTxSigner``).

    Constructed with the recipient/calldata/gas/nonce/value/access-list, then
    fees are set by :func:`finalize_fees`, then signed by
    :meth:`PyTxSigner.sign_eip1559`.
    """

    def __init__(
        self,
        to: str,
        data: bytes,
        gas_limit: int,
        nonce: int,
        value: int = 0,
        access_list: list[dict[str, Any]] | None = None,
    ) -> None: ...
    @property
    def max_fee_per_gas(self) -> int: ...
    @property
    def max_priority_fee_per_gas(self) -> int: ...

class PySubmitCandidate:
    """Per-path builder constructed from ``gas_profitable`` before the submit leaf."""

    def __init__(
        self,
        path_id: int,
        gross_profit: int,
        net_profit: int,
        gas_used: int,
        priority_fee: int,
        base_fee_next: int,
        execute_calldata: bytes,
        executor_address: str,
        access_list: list[dict[str, Any]] | None = None,
        path_pools: set[str] | None = None,
    ) -> None: ...
    # ── read-only getters (the `[dispatch]` per-path log reads these — A5) ──
    @property
    def path_id(self) -> int: ...
    @property
    def gross_profit(self) -> int: ...
    @property
    def net_profit(self) -> int: ...
    @property
    def gas_used(self) -> int: ...
    @property
    def priority_fee(self) -> int: ...

def dispatch_and_submit_py(
    candidates: list[PySubmitCandidate],
    dispatcher: PyDispatcher,
    provider: AsyncAlloyProvider,
    signer: PyTxSigner,
    operator_nonce: int,
    current_block: int,
    dry_run: bool,
    inject_code: bool,
) -> Coroutine[Any, Any, list[dict[str, Any]]]: ...
def fetch_fee_history_py(
    provider: AsyncAlloyProvider,
    dispatcher: PyDispatcher,
    block_count: int,
    last_block: int,
    reward_percentiles: list[float],
) -> Coroutine[Any, Any, bool]: ...
def finalize_fees(
    params: PyTxParams,
    base_fee_next: int,
    priority_fee: int,
) -> None: ...

__all__ = [
    "PyDispatcher",
    "PySubmitCandidate",
    "PyTxParams",
    "PyTxSigner",
    "dispatch_and_submit_py",
    "fetch_fee_history_py",
    "finalize_fees",
]
