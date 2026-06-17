"""Tests for swap encoding and payload generation.

Each SwapAmounts subclass encodes its own per-hop calldata.
ApprovalStrategy and PayloadComposer are pluggable at the pipeline level.
"""

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.arbitrage.encoding import (
    EncodedCall,
    generate_payloads,
)
from degenbot.arbitrage.types import (
    CurveStableSwapPoolSwapAmounts,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)
from degenbot.degenbot_rs import PyBot
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()


def test_v2_swap_amounts_encodes_swap_call() -> None:
    """UniswapV2PoolSwapAmounts.encode() produces a V2 swap() EncodedCall."""
    pool_address = ChecksumAddress(Web3.to_checksum_address("0x" + "ab" * 20))
    recipient = ChecksumAddress(Web3.to_checksum_address("0x" + "cd" * 20))

    swap = UniswapV2PoolSwapAmounts(
        pool=pool_address,
        amounts_in=(1000, 0),
        amounts_out=(0, 900),
    )

    encoded = swap.encode(recipient=recipient)

    assert isinstance(encoded, EncodedCall)
    assert encoded.to == pool_address
    assert encoded.value == 0

    # Verify the calldata starts with V2 swap() selector
    v2_swap_selector = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
    assert encoded.data[:4] == v2_swap_selector

    # Decode the parameters and verify
    params = eth_abi.abi.decode(
        ["uint256", "uint256", "address", "bytes"],
        encoded.data[4:],
    )
    # amounts_out used as swap amounts: (0, 900)
    assert params[0] == 0  # amount0Out
    assert params[1] == 900  # amount1Out
    assert Web3.to_checksum_address(params[2]) == recipient
    assert params[3] == b""


def test_v3_swap_amounts_encodes_swap_call() -> None:
    """UniswapV3PoolSwapAmounts.encode() produces a V3 swap() EncodedCall."""
    pool_address = ChecksumAddress(Web3.to_checksum_address("0x" + "ab" * 20))
    recipient = ChecksumAddress(Web3.to_checksum_address("0x" + "cd" * 20))

    swap = UniswapV3PoolSwapAmounts(
        pool=pool_address,
        amount_in=1000,
        amount_out=900,
        amount_specified=-1000,
        zero_for_one=True,
        sqrt_price_limit_x96=4295128739,
    )

    encoded = swap.encode(recipient=recipient)

    assert isinstance(encoded, EncodedCall)
    assert encoded.to == pool_address
    assert encoded.value == 0

    # Verify the calldata starts with V3 swap() selector
    v3_swap_selector = Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
    assert encoded.data[:4] == v3_swap_selector

    # Decode the parameters and verify
    params = eth_abi.abi.decode(
        ["address", "bool", "int256", "uint160", "bytes"],
        encoded.data[4:],
    )
    assert Web3.to_checksum_address(params[0]) == recipient
    assert params[1] is True  # zero_for_one
    assert params[2] == -1000  # amount_specified
    assert params[3] == 4295128739  # sqrt_price_limit_x96
    assert params[4] == b""


def test_generate_payloads_encodes_multi_swap_path() -> None:
    """generate_payloads() encodes a path with multiple swaps."""
    pool_a = ChecksumAddress(Web3.to_checksum_address("0x" + "aa" * 20))
    pool_b = ChecksumAddress(Web3.to_checksum_address("0x" + "bb" * 20))
    recipient = ChecksumAddress(Web3.to_checksum_address("0x" + "cd" * 20))

    swap_amounts = (
        UniswapV2PoolSwapAmounts(
            pool=pool_a,
            amounts_in=(1000, 0),
            amounts_out=(0, 900),
        ),
        UniswapV3PoolSwapAmounts(
            pool=pool_b,
            amount_in=900,
            amount_out=800,
            amount_specified=-900,
            zero_for_one=False,
            sqrt_price_limit_x96=4295128739,
        ),
    )

    payloads = generate_payloads(swap_amounts, recipient=recipient)

    assert len(payloads) == 2
    assert payloads[0].to == pool_a
    assert payloads[1].to == pool_b


def test_generate_payloads_with_custom_composer() -> None:
    """generate_payloads() uses the provided PayloadComposer."""
    pool = ChecksumAddress(Web3.to_checksum_address("0x" + "aa" * 20))
    recipient = ChecksumAddress(Web3.to_checksum_address("0x" + "cd" * 20))

    swap_amounts = (
        UniswapV2PoolSwapAmounts(
            pool=pool,
            amounts_in=(1000, 0),
            amounts_out=(0, 900),
        ),
    )

    # Custom composer that wraps calls in a Multicall3-style tuple
    multicall_address = ChecksumAddress(Web3.to_checksum_address("0x" + "ee" * 20))

    class Multicall3Composer:
        def compose(self, calls: list[EncodedCall]) -> list[EncodedCall]:
            """Wrap all calls in a single multicall."""
            # Simplified: just return a single call to the multicall address
            return [EncodedCall(to=multicall_address, data=b"multicall")]

    payloads = generate_payloads(
        swap_amounts,
        recipient=recipient,
        composer=Multicall3Composer(),
    )

    assert len(payloads) == 1
    assert payloads[0].to == multicall_address


def test_generate_payloads_with_custom_approval_strategy() -> None:
    """generate_payloads() uses the provided ApprovalStrategy."""
    pool = ChecksumAddress(Web3.to_checksum_address("0x" + "aa" * 20))
    token = ChecksumAddress(Web3.to_checksum_address("0x" + "11" * 20))
    recipient = ChecksumAddress(Web3.to_checksum_address("0x" + "cd" * 20))

    swap_amounts = (
        UniswapV2PoolSwapAmounts(
            pool=pool,
            amounts_in=(1000, 0),
            amounts_out=(0, 900),
        ),
    )

    # Strategy that prepends an ERC-20 approve call
    class ExactApproval:
        def approvals_for(
            self,
            swap_amounts: tuple[UniswapV2PoolSwapAmounts, ...],
            calls: list[EncodedCall],
        ) -> list[EncodedCall]:
            approve_data = Web3.keccak(text="approve(address,uint256)")[:4] + eth_abi.abi.encode(
                ["address", "uint256"], [pool, 1000]
            )
            return [EncodedCall(to=token, data=approve_data)]

    payloads = generate_payloads(
        swap_amounts,
        recipient=recipient,
        approval_strategy=ExactApproval(),
    )

    assert len(payloads) == 2  # 1 approval + 1 swap
    assert payloads[0].to == token  # approval goes to token contract
    assert payloads[1].to == pool  # swap goes to pool


def test_curve_swap_amounts_encodes_exchange_call() -> None:
    """CurveStableSwapPoolSwapAmounts.encode() produces an exchange() EncodedCall."""
    pool_address = ChecksumAddress(Web3.to_checksum_address("0x" + "ab" * 20))
    token_in = make_erc20(_PY_BOT, "0x" + "11" * 20, name="TokenA", symbol="TKA", decimals=18)
    token_out = make_erc20(_PY_BOT, "0x" + "22" * 20, name="TokenB", symbol="TKB", decimals=18)

    swap = CurveStableSwapPoolSwapAmounts(
        pool=pool_address,
        token_in=token_in,
        token_in_index=0,
        token_out=token_out,
        token_out_index=1,
        amount_in=1000,
        min_amount_out=900,
        underlying=False,
    )

    encoded = swap.encode(recipient=pool_address)

    assert isinstance(encoded, EncodedCall)
    assert encoded.to == pool_address
    assert encoded.value == 0

    # Verify selector is exchange()
    exchange_selector = Web3.keccak(text="exchange(int128,int128,uint256,uint256)")[:4]
    assert encoded.data[:4] == exchange_selector

    # Decode and verify params
    params = eth_abi.abi.decode(
        ["int128", "int128", "uint256", "uint256"],
        encoded.data[4:],
    )
    assert params[0] == 0  # token_in_index
    assert params[1] == 1  # token_out_index
    assert params[2] == 1000  # amount_in
    assert params[3] == 900  # min_amount_out


def test_curve_swap_amounts_encodes_exchange_underlying_call() -> None:
    """CurveStableSwapPoolSwapAmounts with underlying=True encodes exchange_underlying()."""
    pool_address = ChecksumAddress(Web3.to_checksum_address("0x" + "ab" * 20))
    token_in = make_erc20(_PY_BOT, "0x" + "11" * 20, name="TokenA", symbol="TKA", decimals=18)
    token_out = make_erc20(_PY_BOT, "0x" + "22" * 20, name="TokenB", symbol="TKB", decimals=18)

    swap = CurveStableSwapPoolSwapAmounts(
        pool=pool_address,
        token_in=token_in,
        token_in_index=0,
        token_out=token_out,
        token_out_index=1,
        amount_in=1000,
        min_amount_out=900,
        underlying=True,
    )

    encoded = swap.encode(recipient=pool_address)

    # Verify selector is exchange_underlying()
    underlying_selector = Web3.keccak(text="exchange_underlying(int128,int128,uint256,uint256)")[:4]
    assert encoded.data[:4] == underlying_selector
