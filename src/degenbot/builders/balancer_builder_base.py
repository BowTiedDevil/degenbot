"""Balancer builder base class and shared data types."""

from __future__ import annotations

import dataclasses
from enum import IntEnum
from fractions import Fraction
from typing import TYPE_CHECKING

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from web3.exceptions import Web3Exception

from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS
from degenbot.balancer.libraries.constants import ONE
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from degenbot.builders.pool_io import PoolIO


class _BalancerPoolType(IntEnum):
    """Internal enum for _detect_pool_type return values.

    Used instead of string literals to enable type-checker
    exhaustiveness checking and prevent typos.
    """

    WEIGHTED = 1
    STABLE = 2


@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)
class DecodedPoolId:
    """Result of decoding a 32-byte Balancer pool ID."""

    pool_address: str
    specialization: int
    nonce: int


@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)
class VaultTokensResult:
    """Result of decoding Vault.getPoolTokens() response."""

    tokens: list[str]
    balances: list[int]
    last_change_block: int


class BalancerBuilderBase:
    """Shared helpers for Balancer pool builders.

    Sync and async builders call these @staticmethod helpers
    without duplicating decode/extract logic. I/O helpers take
    a PoolIO parameter — callers pass the io they receive.
    """

    INVARIANT_V1 = 1
    INVARIANT_V2 = 2

    @staticmethod
    def decode_pool_id(raw: bytes) -> DecodedPoolId:
        """Decode a 32-byte pool ID into typed components.

        Returns:
            The computed value.

        """
        pool_address = get_checksum_address(raw[:20])
        specialization = int.from_bytes(raw[20:22], byteorder="big")
        nonce = int.from_bytes(raw[22:32], byteorder="big")
        return DecodedPoolId(
            pool_address=pool_address,
            specialization=specialization,
            nonce=nonce,
        )

    @staticmethod
    def decode_vault_tokens(raw: bytes) -> VaultTokensResult:
        """Decode getPoolTokens() response.

        Returns:
            The computed value.

        """
        decoded = eth_abi.abi.decode(["address[]", "uint256[]", "uint256"], raw)
        return VaultTokensResult(
            tokens=decoded[0],
            balances=decoded[1],
            last_change_block=decoded[2],
        )

    @staticmethod
    def detect_bpt_index(
        token_addresses: list[str] | tuple[str, ...],
        pool_address: str,
    ) -> int | None:
        """Detect the BPT index for ComposableStablePools.

        Heuristic: the token whose address matches the pool address is BPT.
        Returns None for MetaStablePools (no BPT in token list).

        Returns:
            The computed value.

        """
        checksummed_pool = get_checksum_address(pool_address)
        for i, addr in enumerate(token_addresses):
            if get_checksum_address(addr) == checksummed_pool:
                return i
        return None

    @staticmethod
    def resolve_invariant_version(
        *,
        specialization: int,
        override: int | None = None,
    ) -> int:
        """Determine which StableMath invariant version to use.

        specialization comes from the decoded pool ID:
        - 0 (General): most likely ComposableStablePool → INVARIANT_V1
        - 1 (MinimalSwapInfo): most likely MetaStablePool → INVARIANT_V2
        - 2 (TwoToken): older WeightedPool2Tokens → not a stable pool

        The override parameter from BuildPoolRequest.invariant_version
        takes precedence over heuristics.

        Returns:
            The computed value.

        """
        if override is not None:
            return override
        # MetaStablePools use specialization=1 and INVARIANT_V2.
        # ComposableStablePools use specialization=0 and INVARIANT_V1.
        if specialization == 1:
            return BalancerBuilderBase.INVARIANT_V2
        return BalancerBuilderBase.INVARIANT_V1

    # --- I/O helpers ---

    @staticmethod
    def _fetch_pool_id(io: PoolIO, address: str, block: int) -> bytes:
        data = encode_function_calldata("getPoolId()", None)
        result = io.call(to=address, data=data, block=block)
        decoded = eth_abi.abi.decode(["bytes32"], result)
        return decoded[0]

    @staticmethod
    def _fetch_vault_tokens(
        io: PoolIO,
        pool_id: bytes,
        block: int | None,
    ) -> tuple[list[str], list[int]]:
        data = encode_function_calldata(
            "getPoolTokens(bytes32)",
            [pool_id],
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
        io: PoolIO,
        address: str,
        block: int,
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
        io: PoolIO,
        rate_providers: list[str],
        block: int,
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
        io: PoolIO,
        address: str,
        block: int,
    ) -> _BalancerPoolType:
        """Determine weighted vs stable by probing contract methods.

        Probes in order:
        1. getNormalizedWeights() → WEIGHTED
        2. getAmplificationParameter() → STABLE
        3. Neither → raise (don't default to stable)

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If the operation fails.

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
