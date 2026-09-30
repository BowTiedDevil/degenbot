"""High-performance Ethereum RPC provider using Alloy.

This module provides a Rust-based provider for fast log fetching and RPC calls.

RPC methods return the ``LogData`` / ``BlockData`` mappings defined in
:mod:`degenbot.types.rpc_types`: plain ``bytes`` for hash and data fields,
EIP-55 checksummed strings for addresses, and Python ``int`` for numeric
fields. Log dicts use camelCase keys; block/transaction dicts use
snake_case.

The provider surface is split by concurrency: the sync mixins and
:class:`AlloyProvider` live in :mod:`degenbot.provider.sync`, the async
twins in :mod:`degenbot.provider.async_provider`.

Example:
    >>> from degenbot.provider import AlloyProvider, LogFilter
    >>> provider = AlloyProvider("https://eth-mainnet.example.com")
    >>>
    >>> # Direct property access
    >>> chain_id = provider.chain_id
    >>> block_number = provider.block_number
    >>>
    >>> # Log fetching with LogFilter
    >>> logs = provider.get_logs(
    ...     LogFilter(
    ...         from_block=18_000_000,
    ...         to_block=18_010_000,
    ...         addresses=["0x..."],
    ...     )
    ... )
    >>>
    >>> # Or using keyword arguments
    >>> logs = provider.get_logs(
    ...     from_block=18_000_000,
    ...     to_block=18_010_000,
    ...     addresses=["0x..."],
    ... )

"""

from degenbot._ffi import ChainMismatchError
from degenbot.provider._rust import RustAlloyProvider, RustAsyncAlloyProvider
from degenbot.provider.async_provider import AsyncAlloyProvider
from degenbot.provider.factory import (
    ChainIdentityMismatchError,
    get_async_provider_from_config,
    get_provider_from_config,
)
from degenbot.provider.offline_provider import OfflineProvider
from degenbot.provider.sync import AlloyProvider, LogFilter

# ADR-013 barrier: the Rust pyclasses the submodule trees (and
# offline_provider) construct are bound in :mod:`degenbot.provider._rust` —
# the one module that imports ``degenbot._ffi`` — and re-exported here.

__all__ = [
    "AlloyProvider",
    "AsyncAlloyProvider",
    "ChainIdentityMismatchError",
    "ChainMismatchError",
    "LogFilter",
    "OfflineProvider",
    "RustAlloyProvider",
    "RustAsyncAlloyProvider",
    "get_async_provider_from_config",
    "get_provider_from_config",
]
