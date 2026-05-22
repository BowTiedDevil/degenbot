"""Structured CurveDataProvider implementation.

Replaces the closure-based CurveFetcherFactory with a class where each
CurveDataProvider protocol method is a real method. Shared I/O patterns
(call → decode → cast, block-identifier routing, error handling for reverts)
become private helpers, collapsing ~15 near-identical closure patterns
into shared infrastructure.
"""

# ruff: noqa: ANN401
from __future__ import annotations

from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import ContractLogicError

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.types import LendingRateStyle
from degenbot.exceptions.pool import EVMRevertError
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.builders.pool_io import PoolIO


class CurveDataProviderImpl:
    """Production implementation of CurveDataProvider.

    Each protocol method is a real method on this class. Shared I/O patterns
    (provider call → ABI decode → type cast) are concentrated in private helpers.
    """

    def __init__(
        self,
        *,
        io: PoolIO,
        pool_address: ChecksumAddress,
        base_pool_address: ChecksumAddress | None = None,
        n_coins: int = 2,
        lending_rate_style: LendingRateStyle = LendingRateStyle.NONE,
        token_addresses: list[ChecksumAddress] | None = None,
        use_lending: list[bool] | None = None,
        precision_multipliers: list[int] | None = None,
        rate_multipliers: tuple[int, ...] | None = None,
    ) -> None:
        self._io = io
        self._pool_address = pool_address
        self._base_pool_address = base_pool_address
        self._n_coins = n_coins
        self._lending_rate_style = lending_rate_style
        self._token_addresses = tuple(token_addresses) if token_addresses else ()
        self._use_lending = tuple(use_lending) if use_lending else ()
        self._precision_multipliers = tuple(precision_multipliers) if precision_multipliers else ()
        self._rate_multipliers = rate_multipliers

        # Lending rate cache (keyed by block_number)
        self._lending_rate_cache: dict[int, tuple[int, ...]] = {}

    # ── Shared helpers ──

    def _call(
        self,
        to: ChecksumAddress,
        method_sig: str,
        return_types: list[str],
        block_number: int,
    ) -> tuple[Any, ...]:
        """Call a contract method and decode the result."""
        data = self._io.call_raw(
            {"to": to, "data": Web3.keccak(text=method_sig)[:4]},
            block=block_number,
        )
        return eth_abi.abi.decode(types=return_types, data=data)

    def _call_single(
        self,
        to: ChecksumAddress,
        method_sig: str,
        return_type: str,
        block_number: int,
    ) -> Any:
        """Call a method returning a single value. Returns the unwrapped value."""
        (result,) = self._call(to, method_sig, [return_type], block_number)
        return result

    def _call_raw_single(
        self,
        to: ChecksumAddress,
        data: bytes,
        return_type: str,
        block_number: int,
    ) -> Any:
        """Call with raw data, decode a single return value."""
        result = self._io.call_raw(
            {"to": to, "data": data},
            block=block_number,
        )
        (value,) = eth_abi.abi.decode(types=[return_type], data=result)
        return value

    @staticmethod
    def _wrap_revert(callable_fn: Any, *args: Any, **kwargs: Any) -> Any:
        """Call a method, converting ContractLogicError to EVMRevertError."""
        try:
            return callable_fn(*args, **kwargs)
        except ContractLogicError as e:
            raise EVMRevertError(error=str(e)) from e

    # ── CurveDataProvider protocol methods ──

    def virtual_price(self, block_number: int) -> int:
        target = self._base_pool_address or self._pool_address
        return cast(
            "int",
            self._wrap_revert(
                self._call_single, target, "get_virtual_price()", "uint256", block_number
            ),
        )

    def base_virtual_price(self, block_number: int) -> int:
        return cast(
            "int",
            self._wrap_revert(
                self._call_single,
                self._pool_address,
                "base_virtual_price()",
                "uint256",
                block_number,
            ),
        )

    def base_cache_updated(self, block_number: int) -> int:
        return cast(
            "int",
            self._wrap_revert(
                self._call_single,
                self._pool_address,
                "base_cache_updated()",
                "uint256",
                block_number,
            ),
        )

    def admin_balances(self, block_number: int) -> tuple[int, ...]:
        balances: list[int] = []
        for token_index in range(8):  # max 8 tokens for Curve V1
            try:
                balance = self._io.call(
                    to=self._pool_address,
                    data=encode_function_calldata(
                        function_prototype="admin_balances(uint256)",
                        function_arguments=[token_index],
                    ),
                    block=block_number,
                )
                (admin_balance,) = eth_abi.abi.decode(types=["uint256"], data=balance)
                balances.append(admin_balance)
            except Exception:  # noqa: BLE001
                break
        return tuple(balances)

    def d(self, block_number: int) -> int:
        return cast(
            "int",
            self._wrap_revert(
                self._call_single, self._pool_address, "D()", "uint256", block_number
            ),
        )

    def gamma(self, block_number: int) -> int:
        return cast(
            "int",
            self._wrap_revert(
                self._call_single, self._pool_address, "gamma()", "uint256", block_number
            ),
        )

    def price_scale(self, block_number: int) -> tuple[int, ...]:
        price_scale = [0] * (self._n_coins - 1)
        for token_index in range(self._n_coins - 1):
            data = Web3.keccak(text="price_scale(uint256)")[:4] + eth_abi.abi.encode(
                types=["uint256"], args=[token_index]
            )
            result = self._io.call_raw(
                {"to": self._pool_address, "data": data},
                block=block_number,
            )
            (price_scale[token_index],) = eth_abi.abi.decode(types=["uint256"], data=result)
        return tuple(price_scale)

    def block_timestamp(self, block_number: int) -> int:
        return self._io.get_block_timestamp(block=block_number)

    def block_number(self) -> int:
        return self._io.get_block_number()

    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:
        data = encode_function_calldata(
            "balanceOf(address)", [get_checksum_address(holder_address)]
        )
        result = self._io.call(
            to=get_checksum_address(token_address),
            data=data,
            block=block_number,
        )
        (balance,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", balance)

    def token_total_supply(self, token_address: str, block_number: int) -> int:
        data = encode_function_calldata("totalSupply()", None)
        result = self._io.call(
            to=get_checksum_address(token_address),
            data=data,
            block=block_number,
        )
        (total_supply,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", total_supply)

    def lending_rates(self, block_number: int) -> tuple[int, ...]:
        if self._lending_rate_style == LendingRateStyle.NONE:
            msg = "lending_rates() not available for this pool type"
            raise ValueError(msg)

        # Check cache
        try:
            return self._lending_rate_cache[block_number]
        except KeyError:
            pass

        match self._lending_rate_style:
            case LendingRateStyle.CTOKEN:
                result = self._fetch_ctoken_rates(block_number)
            case LendingRateStyle.YTOKEN:
                result = self._fetch_ytoken_rates(block_number)
            case LendingRateStyle.CYTOKEN:
                result = self._fetch_cytoken_rates(block_number)
            case LendingRateStyle.AETH:
                result = self._fetch_aeth_rates(block_number)
            case LendingRateStyle.RETH:
                result = self._fetch_reth_rates(block_number)
            case LendingRateStyle.ORACLE:
                result = self._fetch_oracle_rates(block_number)
            case _:
                msg = f"Unsupported lending rate style: {self._lending_rate_style}"
                raise ValueError(msg)

        self._lending_rate_cache[block_number] = result
        return result

    # ── Lending rate implementations ──

    PRECISION: int = 10**18

    def _fetch_ctoken_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch cToken rates: exchangeRateStored + supply rate accrual."""
        result: list[int] = []
        for token_addr, is_lending, multiplier in zip(
            self._token_addresses, self._use_lending, self._precision_multipliers, strict=True
        ):
            if not is_lending:
                rate = self.PRECISION
            else:
                rate = cast(
                    "int",
                    self._call_single(token_addr, "exchangeRateStored()", "uint256", block_number),
                )
                supply_rate = cast(
                    "int",
                    self._call_single(token_addr, "supplyRatePerBlock()", "uint256", block_number),
                )
                old_block = cast(
                    "int",
                    self._call_single(token_addr, "accrualBlockNumber()", "uint256", block_number),
                )
                rate += rate * supply_rate * (block_number - old_block) // self.PRECISION
            result.append(multiplier * rate)
        return tuple(result)

    def _fetch_ytoken_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch yToken rates: getPricePerFullShare."""
        result: list[int] = []
        for token_addr, multiplier, is_lending in zip(
            self._token_addresses, self._precision_multipliers, self._use_lending, strict=True
        ):
            if is_lending:
                rate = cast(
                    "int",
                    self._call_single(
                        token_addr, "getPricePerFullShare()", "uint256", block_number
                    ),
                )
            else:
                rate = self.PRECISION
            result.append(rate * multiplier)
        return tuple(result)

    def _fetch_cytoken_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch cyToken rates: all tokens are lending, same as cToken but no is_lending check."""
        result: list[int] = []
        for token_addr, precision_multiplier in zip(
            self._token_addresses, self._precision_multipliers, strict=True
        ):
            rate = cast(
                "int",
                self._call_single(token_addr, "exchangeRateStored()", "uint256", block_number),
            )
            supply_rate = cast(
                "int",
                self._call_single(token_addr, "supplyRatePerBlock()", "uint256", block_number),
            )
            old_block = cast(
                "int",
                self._call_single(token_addr, "accrualBlockNumber()", "uint256", block_number),
            )
            rate += rate * supply_rate * (block_number - old_block) // self.PRECISION
            result.append(precision_multiplier * rate)
        return tuple(result)

    def _fetch_reth_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch rETH rates: getExchangeRate on the second token."""
        ratio = cast(
            "int",
            self._call_single(
                self._token_addresses[1], "getExchangeRate()", "uint256", block_number
            ),
        )
        return (self.PRECISION, ratio)

    def _fetch_aeth_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch aETH rates: ratio() on second token, inverted."""
        ratio = cast(
            "int",
            self._call_single(self._token_addresses[1], "ratio()", "uint256", block_number),
        )
        return (self.PRECISION, self.PRECISION * self.PRECISION // ratio)

    def _fetch_oracle_rates(self, block_number: int) -> tuple[int, ...]:
        """Fetch oracle rates: oracle_method() bitmask dispatch."""
        assert self._rate_multipliers is not None

        oracle_method_value = cast(
            "int",
            self._call_single(self._pool_address, "oracle_method()", "uint256", block_number),
        )

        if oracle_method_value == 0:
            return self._rate_multipliers

        oracle_bit_mask = (2**32 - 1) * 256**28
        oracle_rate = cast(
            "int",
            self._wrap_revert(
                self._call_raw_single,
                get_checksum_address(HexBytes(oracle_method_value % 2**160)),
                HexBytes(oracle_method_value & oracle_bit_mask),
                "uint256",
                block_number,
            ),
        )
        return (
            self._rate_multipliers[0],
            self._rate_multipliers[1] * oracle_rate // self.PRECISION,
        )

    def redemption_price(self, block_number: int) -> int:
        (snap_contract_address,) = self._call(
            self._pool_address, "redemption_price_snap()", ["address"], block_number
        )
        rate = self._call_single(
            get_checksum_address(snap_contract_address),
            "snappedRedemptionPrice()",
            "uint256",
            block_number,
        )
        return cast("int", rate // 10**9)
