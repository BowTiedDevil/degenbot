"""ERC-20 token builder that fetches on-chain metadata."""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token, NativeToken, token_handle
from degenbot.exceptions import DegenbotValueError, PoolRegistrationError
from degenbot.logging import logger

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot._ffi import Bot
    from degenbot.registry import TokenRegistry
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


#: The raw protocol spellings of native value — V4's `address(0)` currency and
#: the old Vyper Curve pools' all-Es coin. Neither is ever a token contract
#: address: generic construction refuses both loudly (ADR-097 D1), and only
#: the owning protocol translates its own spelling.
NATIVE_SENTINEL_ADDRESSES = (
    ZERO_ADDRESS,
    get_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"),
)


def _is_native_sentinel(address: str) -> bool:
    """Whether `address` is a raw protocol native-asset sentinel."""
    return address in NATIVE_SENTINEL_ADDRESSES


class Erc20Builder:
    """Builds Erc20Token instances from Rust-backed metadata and RPC calls.

    Owns the companion I/O choreography: check the registry → ask the Rust
    construction core for metadata → wrap and register the token.
    """

    def __init__(
        self,
        *,
        default_chain_id: ChainId | None = None,
        tokens: TokenRegistry,
        py_bot: Bot,
    ) -> None:
        """Initialize the instance."""
        self._default_chain_id = default_chain_id
        self._tokens = tokens
        self._py_bot = py_bot

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Construct an ERC-20 token, delegating metadata resolution to the Rust core.

        The DB-first + on-chain resolution, write-back and `BotState`
        registration are Rust-owned: `Bot.build_erc20_token`
        runs `build_erc20_metadata` over the attached `ConstructionIo`. This
        Python shell keeps only the *companion* concerns: the `TokenRegistry`
        idempotent short-circuit, the loud raw-sentinel refusal (ADR-097 D1),
        and the `Erc20Token.from_handle` display wrapper.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: The address is a raw protocol native-asset
                sentinel, no contract is deployed at the address, or the token
                metadata could not be resolved on-chain.

        """
        address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        # A raw protocol sentinel is never a token contract address: fail
        # loudly instead of resolving it to the native token (which is named
        # through `build_native`, and translated privately at each protocol's
        # own call sites).
        if _is_native_sentinel(address):
            msg = (
                f"{address} is a protocol native-asset sentinel, not a token "
                "contract address; use Bot.get_native_token(chain_id)"
            )
            raise DegenbotValueError(message=msg)

        # Check registry first
        if (existing := self._tokens.get(token_address=address, chain_id=chain_id)) is not None:
            # ADR-006: ensure the token is registered in the shared Bot
            # (Rust BotState.tokens) — a token pre-registered in the Python
            # registry might not be in the Bot yet. Pool companions recover
            # tokens via py_pool.get_token0/get_token1, which look up the
            # identity's token address in the Rust BotState.tokens registry.
            if self._py_bot.get_token(address) is None:
                self._py_bot.register_token(
                    address,
                    existing.name,
                    existing.symbol,
                    existing.decimals,
                    chain_id,
                )
            return existing

        # DB-first + on-chain + write-back + register: Rust-owned.
        # `Bot.build_erc20_token` resolves name/symbol/decimals (DB row -> on-
        # chain batched read -> alternate-prototype fallback -> UNKNOWN, with a
        # blank-row write-back) and registers into the shared Rust
        # `BotState.tokens` (ADR-006) in one core call over the attached
        # `ConstructionIo`. Returns a thin `Erc20Token` handle.
        try:
            py_token = self._py_bot.build_erc20_token(address, chain_id)
        except (PoolRegistrationError, RuntimeError) as exc:
            # Preserve the documented DegenbotValueError contract. The binding
            # raises the typed build-failure taxonomy for a build that fails
            # before admission — `PoolBuildRpcError` (the read failed) and
            # `PoolBuildDecodeError` (the read answered an impossible value) —
            # and every arm hangs off `PoolRegistrationError`, so catching the
            # parent is what keeps this call's contract whole. That parent is a
            # ValueError, not a RuntimeError, so a RuntimeError-only arm let a
            # typed refusal escape. RuntimeError stays as the belt-and-braces
            # arm for the still-flattened DB refusal and any plain seam
            # failure; `__cause__` carries the typed refusal for a caller that
            # classifies by type instead of message.
            raise DegenbotValueError(message=_build_failure_message(exc)) from exc
        token = Erc20Token.from_handle(py_token)

        # Register idempotently: a concurrent worker may have
        # built + registered this same token first; use the canonical instance
        # so a path sharing it is not lossily skipped.
        token = self._tokens.get_or_add(token_address=token.address, chain_id=chain_id, token=token)

        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")

        return token

    def build_many(
        self,
        addresses: Sequence[str],
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> list[Erc20Token]:
        """Build MANY tokens over the core's batch construction entry.

        The DB-first / network / fallback / write-back / registration
        choreography is Rust-owned (`Bot.build_erc20_tokens`, the batch call
        shape over the same per-token core :meth:`build` uses — one batched
        metadata read serves the network-missing set): one token per input
        address, in input order, duplicates get-or-create, and a token that
        cannot be built is a failure of that entry alone — the successfully
        built tokens stay registered and the whole batch completes before the
        failure surfaces (no all-or-nothing abort).

        Returns:
            One :class:`Erc20Token` per input address, in order.

        Raises:
            DegenbotValueError: An address is a raw protocol native-asset
                sentinel, or one or more tokens could not be built (the
                message names each failed address).

        """
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        checksums = [get_checksum_address(address) for address in addresses]
        for address in checksums:
            if _is_native_sentinel(address):
                msg = (
                    f"{address} is a protocol native-asset sentinel, not a token "
                    "contract address; use Bot.get_native_token(chain_id)"
                )
                raise DegenbotValueError(message=msg)

        # Registry-first per token (the idempotent short-circuit :meth:`build`
        # keeps); everything else is the core batch's per-token choreography.
        resolved: list[Erc20Token | None] = [None] * len(checksums)
        needed: list[tuple[int, str]] = []
        for index, address in enumerate(checksums):
            existing = self._tokens.get(token_address=address, chain_id=chain_id)
            if existing is None:
                needed.append((index, address))
                continue
            if self._py_bot.get_token(address) is None:
                self._py_bot.register_token(
                    address,
                    existing.name,
                    existing.symbol,
                    existing.decimals,
                    chain_id,
                )
            resolved[index] = existing

        failures: list[str] = []
        if needed:
            results = self._py_bot.build_erc20_tokens(
                addresses=[address for _, address in needed], chain_id=chain_id
            )
            for (index, address), (handle, error) in zip(needed, results, strict=True):
                if handle is None:
                    failures.append(f"{address}: {error}")
                    continue
                token = Erc20Token.from_handle(handle)
                resolved[index] = self._tokens.get_or_add(
                    token_address=token.address, chain_id=chain_id, token=token
                )

        if failures:
            msg = f"token build failed: {'; '.join(failures)}"
            raise DegenbotValueError(message=msg)

        if not silent:
            for token in resolved:
                assert token is not None
                logger.info(f"• {token.symbol} ({token.name})")

        return [token for token in resolved if token is not None]

    def build_native(
        self,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> NativeToken:
        """The chain's conceptual native token (ADR-097).

        Metadata resolves through the core chain-facts manifest — no contract
        read, no DB row, and no operator config. The Python shell keeps only
        the registry/display adaptation and logging.

        Returns:
            The chain's :class:`NativeToken`.

        """
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        if (existing := self._tokens.get_native(chain_id=chain_id)) is not None:
            return existing

        token = NativeToken.from_handle(self._py_bot.native_token(chain_id))
        token = self._tokens.get_or_add_native(chain_id=chain_id, token=token)
        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")
        return token

    def get_token_balance(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token | NativeToken,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the token balance for the given address.

        The read-through is core-owned (`Bot.fetch_token_balance`): typed
        block resolution, the session account cache, the stateless read, and
        the cache-fill — one choreography for an ERC-20 and for the
        conceptual native token (whose balance is the account's native
        balance). A cache hit answers without any I/O.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        return cast(
            "int",
            self._py_bot.fetch_token_balance(token_handle(token), address, block_identifier),
        )

    def get_token_approval(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.

        Same core-owned read-through as :meth:`get_token_balance`.

        Returns:
            The computed value.

        """
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        return cast(
            "int",
            self._py_bot.fetch_token_allowance(
                token_handle(token), owner, spender, block_identifier
            ),
        )

    def get_token_total_supply(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token.

        Same core-owned read-through as :meth:`get_token_balance`.

        Returns:
            The computed value.

        """
        return cast(
            "int",
            self._py_bot.fetch_token_total_supply(token_handle(token), block_identifier),
        )

    def get_ether_balance(  # ruff:ignore[no-self-use]
        self,
        chain_id: ChainId,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native asset balance for the given address.

        The native account balance is exactly
        :meth:`get_token_balance` on the chain's conceptual `NativeToken`
        (ADR-097) — one account-query surface, one read-through.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        return cast(
            "int",
            self._py_bot.fetch_token_balance(
                self.build_native(chain_id=chain_id)._py_native, address, block_identifier
            ),
        )


# --- Build-failure message rendering ---

# The seam renders a build refusal as "pool build <arm>: <reason>". The two
# I/O arms' reason is the whole caller-facing message, so their prefix is
# stripped here; the DB arm keeps its historical text unchanged.
_BUILD_FAILURE_PREFIXES = (
    "pool build decode failure: ",
    "pool build RPC error: ",
)


def _build_failure_message(exc: BaseException) -> str:
    """Render a build refusal without the seam's I/O-arm prefix.

    Returns:
        The rendered message.

    """
    message = str(exc)
    for prefix in _BUILD_FAILURE_PREFIXES:
        if message.startswith(prefix):
            return message.removeprefix(prefix)
    return message
