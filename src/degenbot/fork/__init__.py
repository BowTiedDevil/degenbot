"""Anvil fork management utilities for local test chains.

A thin companion shell (ADR-005 three-layer Python layer) over the rust core
`degenbot._ffi.AnvilFork` PyO3 seam (`degenbot_fork::AnvilFork`). The rust
core (added in epic `NXYVYU` FF2/FF3) owns the spawned anvil subprocess
(via `alloy::node_bindings::Anvil`) + a connected alloy `DynProvider`
(over IPC) + the 12 anvil dev-RPC methods. This Python shell:

- preserves the legacy Python-facing constructor config surface (so
  existing callsites don't need to change which kwargs they pass);
- re-uses rust-driven dev-method invocations (`mine` / `reset` /
  `set_snapshot` / `return_to_snapshot` / `set_balance` / ...);
- re-exposes general RPC against the forked node as
  `self.provider` — a `degenbot._ffi.AlloyProvider` constructed over the
  fork's HTTP endpoint (replaces the legacy `self.w3` Web3 handle; the
  rust-owned core's dev-RPC runs over its own IPC `DynProvider`, while the
  companion general-RPC provider is HTTP so borrowed consumers never hold
  an alloy pubsub).

The breaking change vs the pre-0.7 Python `AnvilFork` is `self.w3` →
`self.provider` (and the dropped capture-file / middlewares / free-port
management).

`PyAnvilFork` exposes the IPC path the rust core resolved for the spawned
anvil process. Tests + callers using `fork.w3.eth.X` should be migrated to
`fork.provider.X` (task FF5 / `ECKJE2`) — the legacy attribute is gone.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Literal

from pydantic import validate_call

from degenbot._ffi.fork import AnvilFork as PyAnvilFork
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.infrastructure import AnvilError
from degenbot.logging import logger
from degenbot.provider import AlloyProvider
from degenbot.types.aliases import BlockNumber  # ruff:ignore[typing-only-first-party-import]
from degenbot.validation.evm_values import (
    ValidatedUint256,  # ruff:ignore[typing-only-first-party-import]
)

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Iterable

    from degenbot.types.chain import HexAddress, HexStr


def _coerce_address_to_str(address: HexAddress | bytes) -> str:
    """Coerce a legacy Python `HexAddress | bytes` to a 0x-prefixed hex str.

    Returns:
        The address as a 0x-prefixed hex string.

    """
    if isinstance(address, bytes):
        s = address.hex()
        return s if s.startswith("0x") else f"0x{s}"
    return address


def _coerce_storage_value_to_int(value: HexStr | bytes | int) -> int:
    """Coerce a legacy Python `HexStr | bytes | int` storage value to an int.

    Returns:
        The storage value as an integer.

    """
    if isinstance(value, int):
        return value
    if isinstance(value, bytes):
        return int.from_bytes(value, "big")
    # HexStr — strip 0x prefix + parse as base-16.
    return int(value, 16) if value.startswith("0x") else int(value)


class AnvilFork:
    """A rust-owned Anvil fork subprocess + IPC `DynProvider` + dev-RPC surface.

    Companion shell (ADR-005 Python layer) over `degenbot._ffi.AnvilFork`
    (the PyO3 seam on `degenbot_fork::AnvilFork`). The rust core owns the
    subprocess lifecycle (drop = kill) + an alloy `DynProvider` (IPC
    transport) + the 12 anvil dev-RPC methods. The shell preserves the
    legacy Python-facing constructor config surface + dev-method names,
    raises the legacy `AnvilError` exception subclass on rust-side
    RPC failures, and exposes general RPC through the `AlloyProvider`
    pyclass at :attr:`provider` (replacing the legacy `self.w3` Web3 handle
    — that handle is gone; callers should migrate to
    ``fork.provider.get_block_number()`` etc., covered in task `ECKJE2`).

    The rust core (`degenbot_fork::AnvilFork`) owns:

    - the anvil subprocess lifecycle (drop = kill);
    - a connected alloy `DynProvider` over IPC (transport);
    - the 12 dev-RPC methods (``evm_mine``/``anvil_reset``/``evm_snapshot``/
      ``evm_revert``/``anvil_set_balance``/...), exposed on this shell as
      the legacy Python names (``mine``/``reset``/``set_snapshot``/
      ``return_to_snapshot``/``set_balance``/...).

    The Python shell keeps:

    - the legacy constructor kwargs + the multi-override queueing
      (``balance_overrides`` / ``bytecode_overrides`` / ``nonce_overrides``
      / ``storage_overrides`` / ``coinbase``) — applied post-spawn via the
      dev-method delegations;
    - the legacy Python dev-method names so existing API surface doesn't
      reshuffle (the rust core's name for `set_snapshot` is `snapshot`,
      `return_to_snapshot` → `revert`, `set_storage` → `set_storage_at`,
      `set_next_base_fee` → `set_next_base_fee` — these translations happen
      in-shell);
    - the legacy `AnvilError` exception on rust RPC failures (translated
      from rust's `PyRuntimeError` at the boundary, so the legacy
      ``AnvilError(method=..., error=...)`` shape + the exception-subclass
      identity stay intact);
    - the `@validate_call` + `ValidatedUint256` annotations on
      `set_balance` / `set_next_base_fee` / `set_next_block_timestamp`
      so input-range validation raises pydantic `ValidationError` before
      the rust round-trip (matches the legacy pre-0.7 surface).

    Dropped (no longer needed now that subprocess lifecycle is rust-owned):
    the `--auto-impersonate` CLI arg list construction, the
    `_setup_process` / `_setup_w3` / `_close_ipc_socket` block, the
    `socket`-based free-port allocation, the `tenacity` spawn retry,
    the `subprocess.Popen` capture-file management
    (`capture_path` / `preserve_capture` / `*_capture_filename` properties),
    the `web3.IPCProvider` client + `middleware_onion.inject(middleware)`
    plumbing (alloy's `DynProvider` is the only transport layer under
    the rust core), the `ipc_provider_kwargs` shim, and `http_url` /
    `ws_url` / `port` / `ipc_filename` (anvil-CLI control knobs that the
    rust core controls directly now — the IPC path is the only socket
    surfaced back, via :attr:`ipc_path`).

    The one breaking change for callers: ``self.w3`` is gone. Use
    :attr:`provider` (a `degenbot._ffi.AlloyProvider`) for general RPC.
    """

    def __init__(
        self,
        *,
        localhost: str = "127.0.0.1",
        fork_url: str | None = None,
        fork_block: BlockNumber | None = None,
        fork_transaction_hash: str | None = None,
        mining_mode: Literal["auto", "interval", "none"] = "auto",
        mining_interval: int | None = None,
        storage_caching: bool = True,
        base_fee: int | None = None,
        ipc_path: pathlib.Path | None = None,
        # Default mnemonic used by Brownie for Ganache forks
        mnemonic: str = (
            "patient rude simple dog close planet oval animal hunt sketch suspect slim"
        ),
        chain_id: int | None = None,
        balance_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        bytecode_overrides: Iterable[tuple[HexAddress, bytes]] | None = None,
        nonce_overrides: Iterable[tuple[HexAddress, int]] | None = None,
        storage_overrides: (
            Iterable[tuple[HexAddress | bytes, int, HexStr | bytes | int]] | None
        ) = None,
        coinbase: HexAddress | None = None,
        anvil_opts: list[str] | None = None,
    ) -> None:
        """Initialize the instance.

        Spawns the rust-owned anvil subprocess + connects the alloy
        `DynProvider` over IPC + (queued post-spawn state overrides
        applied after spawn). The rust core raises `RuntimeError` on
        spawn/IPC-connect failure, `ValueError` on invalid `mining_mode`; the
        post-spawn override calls propagate `AnvilError` (RPC) and
        `DegenbotValueError` (interval-without-value).

        """
        # Capture spawn-time config so `reset(fork_url=...)` /
        # `reset(transaction_hash=...)` can recreate the subprocess with
        # ALL the original params (the legacy Python `AnvilFork.reset`
        # supported re-spawning to a new fork URL + block + txn hash).
        self._init_kwargs: dict[str, object] = {
            "host": localhost,
            "fork_url": fork_url,
            "fork_block": fork_block,
            "fork_transaction_hash": fork_transaction_hash,
            "mining_mode": mining_mode,
            "mining_interval": mining_interval,
            "storage_caching": storage_caching,
            "base_fee": base_fee,
            "ipc_path": str(ipc_path) if ipc_path is not None else None,
            "mnemonic": mnemonic,
            "chain_id": chain_id,
            "anvil_opts": tuple(anvil_opts) if anvil_opts else None,
        }

        self._fork_url = fork_url
        # Spawn the rust core. The rust core raises `RuntimeError` /
        # `ValueError` on failure — let those propagate unchanged
        # (`ValueError` for the `mining_mode` parse; `RuntimeError` for
        # the spawn / IPC connect).
        self._fork: PyAnvilFork | None = PyAnvilFork(**self._init_kwargs)  # type: ignore[arg-type]  # ty: ignore[invalid-argument-type]

        # General-RPC `AlloyProvider` over the fork's HTTP endpoint. The
        # rust core's dev-RPC (`mine`/`reset`/`anvil_*`) runs over its own
        # IPC-bound `DynProvider`; the companion general-RPC provider is HTTP
        # so that consumers which borrow it (bots, pools, tracks) never hold
        # an alloy IPC/WS pubsub. An IPC/WS provider keeps a live pubsub
        # service that, when the fork subprocess dies on close, reconnects
        # into the dead socket (the ``alloy_pubsub::service Reconnection
        # attempt …`` log storm). HTTP has no pubsub, so a borrowed provider
        # over a closed fork simply fails calls instead of reconnecting.
        # Kept in `_provider` so `close()`/`reset()` can drop it before the
        # rust subprocess is killed — the `provider` property exposes it and
        # guards the closed state like the other accessors.
        self._provider: AlloyProvider | None = AlloyProvider(self._fork.http_url)

        # Post-spawn state overrides — applied via the registered dev
        # methods so the rust `AnvilApi` calls go through the same
        # IPC-bound `DynProvider`. Matches the legacy Python semantics.
        if balance_overrides is not None:
            for account, balance in balance_overrides:
                self.set_balance(account, balance)
        if bytecode_overrides is not None:
            for account, bytecode in bytecode_overrides:
                self.set_code(account, bytecode)
        if nonce_overrides is not None:
            for account, nonce in nonce_overrides:
                self.set_nonce(account, nonce)
        if storage_overrides is not None:
            for address, position, value in storage_overrides:
                self.set_storage(address=address, position=position, value=value)
        if coinbase is not None:
            self.set_coinbase(coinbase)
        if mining_interval is not None and mining_mode == "interval":
            self.set_block_timestamp_interval(mining_interval)

    # ------------------------------------------------------------------
    # Properties
    # ------------------------------------------------------------------

    @property
    def provider(self) -> AlloyProvider:
        """The general-RPC `AlloyProvider` bound to the fork over HTTP.

        Use for retry-aware ``eth_call`` / ``get_logs`` against the in-memory
        fork (mirrors the legacy ``self.w3`` handle). HTTP (not the rust
        core's IPC `DynProvider`) so that consumers which borrow it never
        hold an alloy IPC/WS pubsub that would reconnect into a dead socket
        when the fork closes. Raising once the fork is closed guarantees a
        provider over a dead subprocess is never silently handed out.

        Raises:
            AnvilError: If the fork was closed.

        """
        if self._provider is None:
            raise AnvilError(
                method="<closed>",
                error="AnvilFork was closed; spawn a new instance to use it again.",
            )
        return self._provider

    @property
    def fork_url(self) -> str | None:
        """Fork URL the spawned anvil process forks from, or `None` for in-memory."""
        return self._fork_url

    @property
    def ipc_path(self) -> str:
        """Resolved IPC socket path the spawned anvil process listens on.

        Use to construct a second `AlloyProvider` (or other transport)
        against the same forked node. Replaces the legacy
        ``AnvilFork.ipc_filename`` (a `pathlib.Path` derived from a
        user-supplied `ipc_path` parent + the allocated HTTP port — that
        bookkeeping is gone; the rust core owns the IPC socket directly
        + reports the resolved path here).

        Raises:
            AnvilError: If the fork was closed.

        """
        if self._fork is None:
            raise AnvilError(
                method="<closed>",
                error="AnvilFork was closed; spawn a new instance to use it again.",
            )
        return self._fork.ipc_path

    @property
    def http_url(self) -> str:
        """HTTP endpoint of the spawned anvil node (e.g. ``http://127.0.0.1:PORT``).

        Use to construct a separate ``Web3``/``HTTPProvider`` over the
        rust-owned anvil subprocess when the IPC-bound `provider` is not
        enough (e.g. test code still on web3.py contract patterns, or a
        second `AlloyProvider` over HTTP transport).
        """
        return self._require_fork().http_url

    @property
    def ws_url(self) -> str:
        """WebSocket endpoint of the spawned anvil node (e.g. ``ws://127.0.0.1:PORT``)."""
        return self._require_fork().ws_url

    @property
    def port(self) -> int:
        """TCP port the spawned anvil node listens on."""
        return self._require_fork().port

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def close(self) -> None:
        """Drop the rust-owned subprocess + IPC handle.

        The rust core (`degenbot_fork::AnvilFork`) owns the subprocess via
        alloy's `AnvilInstance`; dropping the `PyAnvilFork` pyclass handle
        terminates the anvil process + closes the IPC transport. This
        method just clears the Python-side reference so the GC finalizes
        the rust handle — there is no separate IPC-socket cleanup needed
        (alloy's `AnvilInstance` `Drop` impl handles it).
        """
        # Teardown ORDER matters: shut down our own `AlloyProvider` FIRST, while
        # the anvil subprocess is still alive, so no consumer that borrowed it
        # can touch a dead endpoint. The companion provider is HTTP (no alloy
        # pubsub) and the rust core's dev-RPC `DynProvider` is cleanly closed
        # in its own `Drop` -- so no reconnect loop is possible. Drop the
        # provider reference, then the rust-core handle, which kills the
        # subprocess.
        self._provider = None
        # Clearing the Python-side reference forces the rust-side `Drop`
        # (the `AnvilInstance` `Drop` kills the spawned anvil subprocess +
        # closes the IPC transport). Any subsequent call surfaces a clear
        # `AnvilError` via the `ipc_path` getter / dev-method calls.
        self._fork = None

    def __del__(self) -> None:
        """Drop the rust core handle on instance finalization (best-effort).

        The rust-side `Drop` (`AnvilInstance::drop`) kills the spawned
        anvil subprocess; Python's GC handles `PyAnvilFork` cleanup. The
        explicit :meth:`close` is the canonical lifecycle primitive —
        `__del__` factories no-defensive cleanup because alloy's
        `AnvilInstance` already implements `Drop` correctly.
        """

    # ------------------------------------------------------------------
    # Dev-RPC methods (delegate to PyAnvilFork, translate exceptions)
    # ------------------------------------------------------------------

    def _require_fork(self) -> PyAnvilFork:
        """Return the open `PyAnvilFork` handle.

        Returns:
            The open `PyAnvilFork` handle.

        Raises:
            AnvilError: If the fork was closed (the rust handle was dropped).

        """
        if self._fork is None:
            raise AnvilError(
                method="<closed>",
                error="AnvilFork was closed; spawn a new instance to use it again.",
            )
        return self._fork

    def mine(self) -> None:
        """Mine a single block (`evm_mine`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().mine()
        except RuntimeError as exc:
            raise AnvilError(method="evm_mine", error=str(exc)) from exc

    def reset(
        self,
        fork_url: str | None = None,
        block_number: BlockNumber | None = None,
        transaction_hash: str | None = None,
    ) -> None:
        """Fork from a new endpoint, block number, or transaction hash.

        Resetting to a new block number only is done in-place (no
        subprocess relaunch). Resetting to a new endpoint or from a
        transaction hash tears down the existing rust-owned subprocess
        + spawns a new one with the new fork parameters. Slower than
        in-place.

        Raises:
            AnvilError: If the in-place RPC call fails.
            DegenbotValueError: If no reset options are provided.

        """
        if fork_url is not None or transaction_hash is not None:
            # Tear down + respawn with the new fork params.
            logger.info(
                "Resetting AnvilFork to a new fork_url/transaction_hash — "
                "the rust subprocess is being recreated (legacy Python"
                " `reset()` semantics).",
            )
            new_kwargs = dict(self._init_kwargs)
            new_kwargs["fork_url"] = fork_url or self._fork_url
            if block_number is not None and transaction_hash is not None:
                logger.warning(
                    f"Forking from transaction hash {transaction_hash},"
                    " ignoring provided block number.",
                )
            # When a transaction_hash is requested, the block_number
            # must not be passed (anvil's `--fork-transaction-hash`
            # conflicts with `--fork-block-number`).
            new_kwargs["fork_block"] = block_number if transaction_hash is None else None
            new_kwargs["fork_transaction_hash"] = transaction_hash

            # Drop the old `AlloyProvider` (HTTP — no pubsub to leak) then
            # the old handle (kills the old subprocess) BEFORE spawning the
            # new one — frees the HTTP/IPC socket the old instance was
            # listening on.
            self._provider = None
            self._fork = None
            self._fork = PyAnvilFork(**new_kwargs)  # type: ignore[arg-type]  # ty: ignore[invalid-argument-type]
            self._fork_url = new_kwargs["fork_url"]  # type: ignore[assignment]
            # Replace the general-RPC provider over the new fork's HTTP
            # endpoint (HTTP: no IPC/WS pubsub, so consumers that borrow it
            # cannot keep a reconnect loop alive across fork teardown).
            self._provider = AlloyProvider(self._fork.http_url)
        elif block_number is not None:
            # In-place anvil_reset to a new block.
            try:
                self._require_fork().reset(block_number)
            except RuntimeError as exc:
                raise AnvilError(method="anvil_reset", error=str(exc)) from exc
        else:
            raise DegenbotValueError(message="No options provided.")

    @validate_call
    def set_balance(
        self,
        address: str,
        balance: ValidatedUint256,
    ) -> None:
        """Set balance (`anvil_setBalance`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_balance(address, balance)
        except RuntimeError as exc:
            raise AnvilError(method="anvil_setBalance", error=str(exc)) from exc

    def set_code(self, address: str, bytecode: bytes) -> None:
        """Set code (`anvil_setCode`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_code(address, bytecode)
        except RuntimeError as exc:
            raise AnvilError(method="anvil_setCode", error=str(exc)) from exc

    def set_coinbase(self, address: str) -> None:
        """Set coinbase (`anvil_setCoinbase`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_coinbase(address)
        except RuntimeError as exc:
            raise AnvilError(method="anvil_setCoinbase", error=str(exc)) from exc

    def set_block_timestamp_interval(self, interval: int) -> None:
        """Set block timestamp interval (`anvil_setBlockTimestampInterval`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_block_timestamp_interval(interval)
        except RuntimeError as exc:
            raise AnvilError(
                method="anvil_setBlockTimestampInterval",
                error=str(exc),
            ) from exc

    @validate_call
    def set_next_base_fee(
        self,
        fee: ValidatedUint256,
    ) -> None:
        """Set the next block base fee (`anvil_setNextBlockBaseFeePerGas`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_next_base_fee(fee)
        except RuntimeError as exc:
            raise AnvilError(
                method="anvil_setNextBlockBaseFeePerGas",
                error=str(exc),
            ) from exc

    @validate_call
    def set_next_block_timestamp(
        self,
        timestamp: ValidatedUint256,
    ) -> None:
        """Set the next block timestamp (`evm_setNextBlockTimestamp`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_next_block_timestamp(timestamp)
        except RuntimeError as exc:
            raise AnvilError(
                method="evm_setNextBlockTimestamp",
                error=str(exc),
            ) from exc

    def set_nonce(self, address: str, nonce: int) -> None:
        """Set nonce (`anvil_setNonce`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        try:
            self._require_fork().set_nonce(address, nonce)
        except RuntimeError as exc:
            raise AnvilError(method="anvil_setNonce", error=str(exc)) from exc

    def set_storage(
        self,
        address: HexAddress | bytes,
        position: int,
        value: HexStr | bytes | int,
    ) -> None:
        """Set storage (`anvil_setStorageAt`).

        Raises:
            AnvilError: If the RPC call fails.

        """
        address_str = _coerce_address_to_str(address)
        value_int = _coerce_storage_value_to_int(value)
        try:
            self._require_fork().set_storage_at(address_str, position, value_int)
        except RuntimeError as exc:
            raise AnvilError(method="anvil_setStorageAt", error=str(exc)) from exc
