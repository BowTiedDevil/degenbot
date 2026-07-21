"""Balancer builder base class and shared data types."""

from __future__ import annotations

import dataclasses
from enum import IntEnum
from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.abi import AbiDecodeError, decode
from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS
from degenbot.balancer.libraries.constants import ONE, PowVersion
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError, RpcError

if TYPE_CHECKING:
    from degenbot.bot import PyBotIo


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
    a PyBotIo parameter — callers pass the io they receive.
    """

    INVARIANT_V1 = 1
    INVARIANT_V2 = 2

    @staticmethod
    def pow_version_to_rust(pow_version: PowVersion | int) -> int:
        """Map a ``PowVersion`` enum (or raw int) → the ``u8`` discriminant Rust stores.

        The Rust core (``register_balancer_weighted_pool``, ADR-005 slice
        12a) carries ``PowVersion`` as an opaque ``u8`` (V1=1 / V2=2) for the
        future Rust ``WeightedMath`` port (ADR-005 slice 12e). Not consumed by
        slice 12b's companion — the Python companion keeps its own
        ``PowVersion`` for legacy builder compatibility.

        Returns:
            The computed value.

        """
        if isinstance(pow_version, PowVersion):
            return 1 if pow_version is PowVersion.V1 else 2
        return int(pow_version)

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
        decoded = decode(["address[]", "uint256[]", "uint256"], raw)
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
    def _fetch_pool_id(io: PyBotIo, address: str, block: int) -> bytes:
        return bytes(io.fetch_balancer_pool_id(address, block=block))

    @staticmethod
    def _fetch_vault_tokens(
        io: PyBotIo,
        pool_id: bytes,
        block: int | None,
    ) -> tuple[list[str], list[int]]:
        tokens, balances = io.fetch_balancer_vault_tokens(
            BALANCER_V2_VAULT_ADDRESS,
            pool_id,
            block=block,
        )
        return list(tokens), [int(b) for b in balances]

    @staticmethod
    def _fetch_swap_fee(io: PyBotIo, address: str, block: int) -> Fraction:
        return Fraction(io.fetch_balancer_swap_fee(address, block=block), 10**18)

    @staticmethod
    def _fetch_weights(io: PyBotIo, address: str, block: int) -> list[int]:
        return list(io.fetch_balancer_weights(address, block=block))

    @staticmethod
    def _fetch_amp(io: PyBotIo, address: str, block: int) -> int:
        return io.fetch_balancer_amp(address, block=block)

    @staticmethod
    def _fetch_rate_providers(
        io: PyBotIo,
        address: str,
        block: int,
    ) -> list[str]:
        # ADR-005 slice 14n: delegate to Rust. WeightedPool2Tokens and
        # MetaStablePools may not have getRateProviders; the Rust impl returns
        # an empty list there (mirrors the prior `except` -> []).
        try:
            return list(io.fetch_balancer_rate_providers(address, block=block))
        except (RpcError, AbiDecodeError):
            return []

    @staticmethod
    def _fetch_rates(
        io: PyBotIo,
        rate_providers: list[str],
        block: int,
    ) -> list[int]:
        # ADR-005 slice 14n: delegate each per-provider getRate() to Rust.
        # The zero-address sentinel check stays.
        rates: list[int] = []
        for provider in rate_providers:
            if provider == "0x0000000000000000000000000000000000000000":
                rates.append(ONE)
                continue
            rates.append(int(io.fetch_balancer_rate(provider, block=block)))
        return rates

    @staticmethod
    def _detect_pool_type(
        io: PyBotIo,
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
        # ADR-005 slice 14n: delegate both probes to Rust
        # (``PyBotIo.probe_balancer_pool_type``). PyBotIo is the only executor;
        # the Python getNormalizedWeights/getAmplificationParameter probing
        # fallback is retired.
        try:
            result = io.probe_balancer_pool_type(address, block=block)
        except ValueError:
            msg = (
                f"Cannot determine Balancer pool type for {address}. "
                "Neither getNormalizedWeights() nor "
                "getAmplificationParameter() responded. "
                "Linear pools are not yet supported."
            )
            raise DegenbotValueError(message=msg) from None
        if result == "weighted":
            return _BalancerPoolType.WEIGHTED
        return _BalancerPoolType.STABLE
