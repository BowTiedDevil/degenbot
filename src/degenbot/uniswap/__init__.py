"""Uniswap V2/V3/V4 pool type registration and exports."""

# Deployment resolver functions — bridged from the Rust ``degenbot-core``
# deployments resolver (ADR-013: ``_ffi`` only in ``__init__.py`` barrier).
# Pool-address derivation — the keccak/create2 math lives in the Rust core
# (ADR-013: `_ffi` only in the `__init__.py` barrier; leaf modules import
# from this stable home).
from degenbot._ffi import PoolTickCoverage, SwapRead, SwapRefusalKind, SwapRefused
from degenbot._ffi import (
    generate_v2_pool_address as _rs_generate_v2_pool_address,
)
from degenbot._ffi import (
    generate_v3_pool_address as _rs_generate_v3_pool_address,
)
from degenbot._ffi.deployments import (
    resolve_deployer,
    resolve_v2_init_hash,
    resolve_v3_init_hash,
)
from degenbot.exceptions.pool import IncompleteSwap, MissingLiquidityData

# Set default pool classes — UniswapV2Pool and UniswapV3Pool serve as the
# fallback when no factory-specific registration exists.
# Deployment data (chain_id, factory → deployer / init_hash / variant /
# dex_identity) is loaded from the shipped deployments.json by the top-level
# `degenbot` package init via `register_from_deployments(load_deployments())`.
from degenbot.registry.pool_type import pool_type_registry


def swap_refusal_to_error(exc: SwapRefused) -> IncompleteSwap | MissingLiquidityData:
    """Map a swap-simulation refusal to the companions' exception tree.

    The ONE kind-mapping home (ADR-037/t2): the Rust seam raises
    `SwapRefused` with a typed `kind`; this renders it as the historical
    exception the companions' callers catch. The fetch arms carry the bitmap
    word the walk stalled on — distinct from the swap being impossible
    (`NotComputable`, the real pool's revert-analog).

    Returns:
        The typed exception the caller raises (`from exc`).

    """
    kind, word = exc.args[0], exc.args[1]
    if kind in {SwapRefusalKind.FetchFailed, SwapRefusalKind.FetchExhausted}:
        return MissingLiquidityData(word=int(word) if word is not None else -1)
    return IncompleteSwap(amount_in=0, amount_out=0)


# The pool leaves import THIS module's swap-refusal vocabulary, so the
# adapter binds before them (the leaf imports below are deliberately not at
# top of file).
from .v2_liquidity_pool import UniswapV2Pool  # ruff:ignore[module-import-not-at-top-of-file]
from .v2_types import (  # ruff:ignore[module-import-not-at-top-of-file]
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from .v3_liquidity_pool import UniswapV3Pool  # ruff:ignore[module-import-not-at-top-of-file]
from .v3_snapshot import UniswapV3LiquiditySnapshot  # ruff:ignore[module-import-not-at-top-of-file]
from .v3_types import (  # ruff:ignore[module-import-not-at-top-of-file]
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)
from .v4_liquidity_pool import UniswapV4Pool  # ruff:ignore[module-import-not-at-top-of-file]
from .v4_snapshot import UniswapV4LiquiditySnapshot  # ruff:ignore[module-import-not-at-top-of-file]
from .v4_types import UniswapV4PoolExternalUpdate, UniswapV4PoolState  # ruff:ignore[module-import-not-at-top-of-file]

pool_type_registry.set_default_v2_class(UniswapV2Pool)
pool_type_registry.set_default_v3_class(UniswapV3Pool)


__all__ = (
    "PoolTickCoverage",
    "SwapRead",
    "SwapRefusalKind",
    "SwapRefused",
    "UniswapV2Pool",
    "UniswapV2PoolExternalUpdate",
    "UniswapV2PoolSimulationResult",
    "UniswapV2PoolState",
    "UniswapV3LiquiditySnapshot",
    "UniswapV3Pool",
    "UniswapV3PoolExternalUpdate",
    "UniswapV3PoolSimulationResult",
    "UniswapV3PoolState",
    "UniswapV4LiquiditySnapshot",
    "UniswapV4Pool",
    "UniswapV4PoolExternalUpdate",
    "UniswapV4PoolState",
    "_rs_generate_v2_pool_address",
    "_rs_generate_v3_pool_address",
    "resolve_deployer",
    "resolve_v2_init_hash",
    "resolve_v3_init_hash",
    "swap_refusal_to_error",
)
