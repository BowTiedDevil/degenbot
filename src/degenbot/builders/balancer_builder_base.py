"""Balancer builder base class and shared data types."""

from __future__ import annotations

import dataclasses
from enum import IntEnum

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address


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
    """Shared pure-logic helpers for Balancer pool builders.

    Sync and async builders call these @staticmethod helpers
    without duplicating decode/extract logic. No I/O — all
    chain access is mediated by the PoolIO parameter at
    the builder level, not here.
    """

    INVARIANT_V1 = 1
    INVARIANT_V2 = 2

    @staticmethod
    def decode_pool_id(raw: bytes) -> DecodedPoolId:
        """Decode a 32-byte pool ID into typed components."""
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
        """Decode getPoolTokens() response."""
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
        """
        if override is not None:
            return override
        # MetaStablePools use specialization=1 and INVARIANT_V2.
        # ComposableStablePools use specialization=0 and INVARIANT_V1.
        if specialization == 1:
            return BalancerBuilderBase.INVARIANT_V2
        return BalancerBuilderBase.INVARIANT_V1
