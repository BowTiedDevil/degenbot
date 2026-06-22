"""Balancer V2 pool builder.

Owns the full I/O choreography: RPC fetch → decode → construct → register.
Pool type is determined via _detect_pool_type() which probes the contract
for characteristics (has getNormalizedWeights → WEIGHTED,
has getAmplificationParameter → STABLE). Raises a clear error for
unknown types instead of defaulting to stable.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS, BROKEN_BALANCER_V2_POOLS
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.scaling_helpers import _compute_scaling_factor
from degenbot.balancer.pools import BalancerV2Pool, detect_pow_version
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.balancer.types import (
    BalancerV2StablePoolExternalUpdate,
    BalancerV2WeightedPoolExternalUpdate,
)
from degenbot.builders.balancer_builder_base import BalancerBuilderBase, _BalancerPoolType
from degenbot.builders.request import BuildPoolRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import BrokenPool

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)
class _BuildContext:
    """Common data gathered during build() for use by _build_weighted/_build_stable."""

    address: str
    pool_id: bytes
    token_addresses: list[str]
    balances: list[int]
    fee: Fraction
    chain_id: ChainId
    state_block: int


class BalancerBuilder(BalancerBuilderBase):
    """Builds and updates Balancer V2 pools (weighted, stable, composable).

    Owns the full I/O choreography: RPC fetch → decode → construct →
    register.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from RPC and construct an I/O-free Balancer pool.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If the operation fails.
            BrokenPool: If the operation fails.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )

        # 1. Check broken pools
        if pool_address in BROKEN_BALANCER_V2_POOLS:
            raise BrokenPool

        # 2. Fetch pool ID from chain
        pool_id = BalancerBuilderBase._fetch_pool_id(  # noqa: SLF001
            io, pool_address, state_block
        )

        # 3. Decode specialization from pool_id
        pool_id_decoded = self.decode_pool_id(pool_id)

        # 4. Fetch tokens and balances from Vault
        token_addresses, balances = BalancerBuilderBase._fetch_vault_tokens(  # noqa: SLF001
            io, pool_id, state_block
        )

        # 5. Fetch fee
        fee = BalancerBuilderBase._fetch_swap_fee(  # noqa: SLF001
            io, pool_address, state_block
        )

        # 6. Build shared context
        build_ctx = _BuildContext(
            address=pool_address,
            pool_id=pool_id,
            token_addresses=token_addresses,
            balances=balances,
            fee=fee,
            chain_id=chain_id,
            state_block=state_block,
        )

        # 7. Detect pool type and build
        pool_type = BalancerBuilderBase._detect_pool_type(  # noqa: SLF001
            io, pool_address, state_block
        )
        if pool_type == _BalancerPoolType.WEIGHTED:
            return self._build_weighted(io, build_ctx, request)

        if pool_type == _BalancerPoolType.STABLE:
            return self._build_stable(
                io,
                build_ctx,
                request,
                pool_id_decoded.specialization,
            )

        msg = f"Unknown Balancer pool type at {pool_address}"
        raise DegenbotValueError(message=msg)

    def _build_weighted(
        self,
        io: PoolIO,
        ctx: _BuildContext,
        request: BuildRequest,
    ) -> BalancerV2Pool:
        # Build tokens
        tokens = [
            self._erc20_builder.build(addr, chain_id=ctx.chain_id, silent=request.silent, io=io)
            for addr in ctx.token_addresses
        ]

        # Fetch weights
        weights = BalancerBuilderBase._fetch_weights(  # noqa: SLF001
            io, ctx.address, ctx.state_block
        )

        # Detect PowVersion from bytecode
        bytecode = io.get_code(ctx.address, block=ctx.state_block).hex()
        pow_version = detect_pow_version(bytecode)

        # ADR-005 slice 12b: register the weighted pool in the Rust core +
        # build the companion over the resulting PyLiquidityPool handle
        # (production-path twin of make_balancer_weighted_pool).
        scaling_factors = [_compute_scaling_factor(t) for t in tokens]
        fee_scaled_int = int(ctx.fee * BalancerV2Pool.FEE_DENOMINATOR)
        pool_id_rust = self._py_bot.register_balancer_weighted_pool(
            address=ctx.address,
            vault=BALANCER_V2_VAULT_ADDRESS,
            pool_id_hex="0x" + ctx.pool_id.hex(),
            tokens=[t.address for t in tokens],
            weights=list(weights),
            scaling_factors=scaling_factors,
            swap_fee=fee_scaled_int,
            pow_version=BalancerBuilderBase.pow_version_to_rust(pow_version),
            balances=list(ctx.balances),
            update_block=ctx.state_block,
        )
        py_pool = self._py_bot.get_pool(pool_id_rust)
        assert py_pool is not None, (
            "register_balancer_weighted_pool returned a pool_id with no handle"
        )

        pool = BalancerV2Pool(
            py_pool,
            address=ctx.address,
            pool_id=ctx.pool_id,
            vault=BALANCER_V2_VAULT_ADDRESS,
            tokens=tokens,
            fee=ctx.fee,
            weights=weights,
            pow_version=pow_version,
            chain_id=ctx.chain_id,
            state_block=ctx.state_block,
        )

        self._pools.add(pool, chain_id=ctx.chain_id, pool_address=pool.address)
        return pool

    def _build_stable(
        self,
        io: PoolIO,
        ctx: _BuildContext,
        request: BuildRequest,
        specialization: int,
    ) -> BalancerV2StablePool:
        assert isinstance(
            request, BuildPoolRequest
        )  # Balancer never receives BuildManagedPoolRequest
        # Build tokens
        tokens = [
            self._erc20_builder.build(addr, chain_id=ctx.chain_id, silent=request.silent, io=io)
            for addr in ctx.token_addresses
        ]

        # Fetch amp
        amp = BalancerBuilderBase._fetch_amp(  # noqa: SLF001
            io, ctx.address, ctx.state_block
        )

        # Detect BPT index
        bpt_idx = (
            request.bpt_idx
            if request.bpt_idx is not None
            else self.detect_bpt_index(ctx.token_addresses, ctx.address)
        )

        # Fetch rate providers and compute scaling factors
        rate_provider_addresses = BalancerBuilderBase._fetch_rate_providers(  # noqa: SLF001
            io, ctx.address, ctx.state_block
        )
        base_sf = tuple(_compute_scaling_factor(t) for t in tokens)

        if rate_provider_addresses:
            rates = BalancerBuilderBase._fetch_rates(  # noqa: SLF001
                io, rate_provider_addresses, ctx.state_block
            )
            scaling_factors = tuple(
                bsf * rate // ONE for bsf, rate in zip(base_sf, rates, strict=True)
            )
        else:
            scaling_factors = base_sf

        # Resolve invariant version
        invariant_version = self.resolve_invariant_version(
            specialization=specialization,
            override=request.invariant_version,
        )

        pool = BalancerV2StablePool(
            address=ctx.address,
            pool_id=ctx.pool_id,
            vault=BALANCER_V2_VAULT_ADDRESS,
            tokens=tokens,
            balances=ctx.balances,
            fee=ctx.fee,
            amp=amp,
            scaling_factors=scaling_factors,
            bpt_idx=bpt_idx,
            base_scaling_factors=base_sf,
            invariant_version=invariant_version,
            chain_id=ctx.chain_id,
            state_block=ctx.state_block,
        )

        self._pools.add(pool, chain_id=ctx.chain_id, pool_address=pool.address)
        return pool

    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        io: PoolIO | None = None,
        block_number: int | None = None,
    ) -> bool:
        """Fetch new balances from Vault and update the pool.

        Returns:
            The computed value.

        Raises:
            TypeError: If the operation fails.

        """
        assert io is not None

        if isinstance(pool, BalancerV2Pool):
            _, new_balances = BalancerBuilderBase._fetch_vault_tokens(  # noqa: SLF001
                io, pool.pool_id, block_number
            )

            if pool.balances == tuple(new_balances):
                return False

            update = BalancerV2WeightedPoolExternalUpdate(
                block_number=block_number or 0,
                balances=tuple(new_balances),
            )
            pool.external_update(update)
            return True

        if isinstance(pool, BalancerV2StablePool):
            _, new_balances = BalancerBuilderBase._fetch_vault_tokens(  # noqa: SLF001
                io, pool.pool_id, block_number
            )

            if pool.balances == tuple(new_balances):
                return False

            update = BalancerV2StablePoolExternalUpdate(
                block_number=block_number or 0,
                balances=tuple(new_balances),
            )
            pool.external_update(update)
            return True

        msg = f"BalancerBuilder cannot update {type(pool).__name__}"
        raise TypeError(msg)
