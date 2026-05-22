"""Balancer V2 pool builder.

Owns the full I/O choreography: RPC fetch → decode → construct → register.
Pool type is determined via _detect_pool_type() which probes the contract
for characteristics (has getNormalizedWeights → WEIGHTED,
has getAmplificationParameter → STABLE). Raises a clear error for
unknown types instead of defaulting to stable.
"""

from __future__ import annotations

import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from web3.exceptions import Web3Exception

from degenbot.balancer.deployments import (
    BALANCER_V2_VAULT_ADDRESS,
    BROKEN_BALANCER_V2_POOLS,
)
from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.scaling_helpers import _compute_scaling_factor
from degenbot.balancer.pools import BalancerV2Pool, detect_pow_version
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.balancer.types import (
    BalancerV2StablePoolExternalUpdate,
    BalancerV2WeightedPoolExternalUpdate,
)
from degenbot.builders.balancer_builder_base import (
    BalancerBuilderBase,
    _BalancerPoolType,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import BrokenPool
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildPoolRequest, BuildRequest
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
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from RPC and construct an I/O-free Balancer pool."""
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block
            if request.state_block is not None
            else io.get_block_number()
        )

        # 1. Check broken pools
        if pool_address in BROKEN_BALANCER_V2_POOLS:
            raise BrokenPool

        # 2. Fetch pool ID from chain
        pool_id = self._fetch_pool_id(io, pool_address, state_block)

        # 3. Decode specialization from pool_id
        pool_id_decoded = self.decode_pool_id(pool_id)

        # 4. Fetch tokens and balances from Vault
        token_addresses, balances = self._fetch_vault_tokens(io, pool_id, state_block)

        # 5. Fetch fee
        fee = self._fetch_swap_fee(io, pool_address, state_block)

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
        pool_type = self._detect_pool_type(io, pool_address, state_block)
        if pool_type == _BalancerPoolType.WEIGHTED:
            return self._build_weighted(io, build_ctx, request)

        if pool_type == _BalancerPoolType.STABLE:
            return self._build_stable(
                io, build_ctx, request, pool_id_decoded.specialization,
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
        weights = self._fetch_weights(io, ctx.address, ctx.state_block)

        # Detect PowVersion from bytecode
        bytecode = io.get_code(ctx.address, block=ctx.state_block).hex()
        pow_version = detect_pow_version(bytecode)

        pool = BalancerV2Pool(
            address=ctx.address,
            pool_id=ctx.pool_id,
            vault=BALANCER_V2_VAULT_ADDRESS,
            tokens=tokens,
            balances=ctx.balances,
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
        assert isinstance(request, BuildPoolRequest)  # Balancer never receives BuildManagedPoolRequest
        # Build tokens
        tokens = [
            self._erc20_builder.build(addr, chain_id=ctx.chain_id, silent=request.silent, io=io)
            for addr in ctx.token_addresses
        ]

        # Fetch amp
        amp = self._fetch_amp(io, ctx.address, ctx.state_block)

        # Detect BPT index
        bpt_idx = (
            request.bpt_idx
            if request.bpt_idx is not None
            else self.detect_bpt_index(ctx.token_addresses, ctx.address)
        )

        # Fetch rate providers and compute scaling factors
        rate_provider_addresses = self._fetch_rate_providers(io, ctx.address, ctx.state_block)
        base_sf = tuple(_compute_scaling_factor(t) for t in tokens)

        if rate_provider_addresses:
            rates = self._fetch_rates(io, rate_provider_addresses, ctx.state_block)
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

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        io: PoolIO | None = None,
        block_number: int | None = None,
    ) -> bool:
        """Fetch new balances from Vault and update the pool."""
        assert io is not None

        if isinstance(pool, BalancerV2Pool):
            _, new_balances = self._fetch_vault_tokens(io, pool.pool_id, block_number)

            if pool.balances == tuple(new_balances):
                return False

            update = BalancerV2WeightedPoolExternalUpdate(
                block_number=block_number or 0,
                balances=tuple(new_balances),
            )
            pool.external_update(update)
            return True

        if isinstance(pool, BalancerV2StablePool):
            _, new_balances = self._fetch_vault_tokens(io, pool.pool_id, block_number)

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

    # --- I/O helpers ---

    @staticmethod
    def _fetch_pool_id(io: PoolIO, address: str, block: int) -> bytes:
        data = encode_function_calldata("getPoolId()", None)
        result = io.call(to=address, data=data, block=block)
        decoded = eth_abi.abi.decode(["bytes32"], result)
        return decoded[0]

    @staticmethod
    def _fetch_vault_tokens(
        io: PoolIO, pool_id: bytes, block: int | None,
    ) -> tuple[list[str], list[int]]:
        data = encode_function_calldata(
            "getPoolTokens(bytes32)", [pool_id],
        )
        result = io.call(
            to=BALANCER_V2_VAULT_ADDRESS,
            data=data,
            block=block,
        )
        decoded = eth_abi.abi.decode(["address[]", "uint256[]", "uint256"], result)
        return decoded[0], decoded[1]

    @staticmethod
    def _fetch_swap_fee(io: PoolIO, address: str, block: int) -> Fraction:
        data = encode_function_calldata("getSwapFeePercentage()", None)
        result = io.call(to=address, data=data, block=block)
        decoded = eth_abi.abi.decode(["uint256"], result)
        return Fraction(decoded[0], 10**18)

    @staticmethod
    def _fetch_weights(io: PoolIO, address: str, block: int) -> list[int]:
        data = encode_function_calldata("getNormalizedWeights()", None)
        result = io.call(to=address, data=data, block=block)
        decoded = eth_abi.abi.decode(["uint256[]"], result)
        return list(decoded[0])

    @staticmethod
    def _fetch_amp(io: PoolIO, address: str, block: int) -> int:
        data = encode_function_calldata("getAmplificationParameter()", None)
        result = io.call(to=address, data=data, block=block)
        decoded = eth_abi.abi.decode(["uint256", "bool"], result)
        return decoded[0]

    @staticmethod
    def _fetch_rate_providers(
        io: PoolIO, address: str, block: int,
    ) -> list[str]:
        try:
            data = encode_function_calldata("getRateProviders()", None)
            result = io.call(to=address, data=data, block=block)
            decoded = eth_abi.abi.decode(["address[]"], result)
            return list(decoded[0])
        except (Web3Exception, DecodingError):
            # WeightedPool2Tokens and MetaStablePools may not have getRateProviders
            return []

    @staticmethod
    def _fetch_rates(
        io: PoolIO, rate_providers: list[str], block: int,
    ) -> list[int]:
        rates: list[int] = []
        for provider in rate_providers:
            if provider == "0x0000000000000000000000000000000000000000":
                rates.append(ONE)
                continue
            data = encode_function_calldata("getRate()", None)
            result = io.call(to=provider, data=data, block=block)
            decoded = eth_abi.abi.decode(["uint256"], result)
            rates.append(decoded[0])
        return rates

    @staticmethod
    def _detect_pool_type(
        io: PoolIO, address: str, block: int,
    ) -> _BalancerPoolType:
        """Determine weighted vs stable by probing contract methods.

        Probes in order:
        1. getNormalizedWeights() → WEIGHTED
        2. getAmplificationParameter() → STABLE
        3. Neither → raise (don't default to stable)
        """
        try:
            data = encode_function_calldata("getNormalizedWeights()", None)
        except (Web3Exception, DecodingError):
            pass
        else:
            try:
                io.call(to=address, data=data, block=block)
            except Web3Exception:
                pass
            else:
                return _BalancerPoolType.WEIGHTED

        try:
            data = encode_function_calldata("getAmplificationParameter()", None)
        except (Web3Exception, DecodingError):
            pass
        else:
            try:
                io.call(to=address, data=data, block=block)
            except Web3Exception:
                pass
            else:
                return _BalancerPoolType.STABLE

        msg = (
            f"Cannot determine Balancer pool type for {address}. "
            f"Neither getNormalizedWeights() nor getAmplificationParameter() responded. "
            f"Linear pools are not yet supported."
        )
        raise DegenbotValueError(message=msg)
