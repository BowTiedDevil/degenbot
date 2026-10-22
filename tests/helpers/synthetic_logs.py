"""Synthetic pool-event logs for `Bot.dispatch_log`-driven state changes.

Test suites do not hand-write pool state: state arrives either as a
registration parameter (`register_v*_pool` seeds scalars + tick data inline) or
as a log pushed through the pump's own door — `Bot.dispatch_log` →
`LogDispatcher` decode → apply. These helpers encode the exact wire shape each
decoder in `degenbot-decoders` expects, so a test states an event and lets the
production decode/apply path move the state.

Each builder returns the `topics=` / `data=` keywords `Bot.dispatch_log` takes::

    bot.dispatch_log(address=pool.address, block_number=..., **v3_mint(...))
"""

from __future__ import annotations

from degenbot.abi import encode as abi_encode

ZERO_ADDRESS = "0x" + "00" * 20

# Keccak256 signatures, mirrored from degenbot-decoders (v2_sync_decoder,
# v3_swap_decoder, v3_mint_burn_decoder, v4_swap_decoder,
# v4_modify_liquidity_decoder).
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V3_MINT_TOPIC = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
V3_BURN_TOPIC = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
V4_SWAP_TOPIC = "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"
V4_MODIFY_LIQUIDITY_TOPIC = "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"


def word(value: int) -> str:
    """A 32-byte ABI word — two's-complement for negative values.

    The indexed `int24` topics and the sign-extended data words both use it,
    so a negative tick round-trips through the decoders' low-4-byte read.
    """
    return f"0x{value & ((1 << 256) - 1):064x}"


def v2_sync(*, reserve0: int, reserve1: int) -> dict[str, object]:
    """`Sync(uint112,uint112)` — absolute reserves (V2 pair contract)."""
    return {
        "topics": [V2_SYNC_TOPIC],
        "data": "0x" + abi_encode(types=["uint112", "uint112"], args=[reserve0, reserve1]).hex(),
    }


def v3_swap(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    tick: int,
    amount0: int = 0,
    amount1: int = 0,
    sender: str = ZERO_ADDRESS,
    recipient: str = ZERO_ADDRESS,
) -> dict[str, object]:
    """`Swap(address,address,int256,int256,uint160,uint128,int24)` — post-swap state."""
    return {
        "topics": [V3_SWAP_TOPIC, _address_topic(sender), _address_topic(recipient)],
        "data": "0x"
        + abi_encode(
            types=["int256", "int256", "uint160", "uint128", "int24"],
            args=[amount0, amount1, sqrt_price_x96, liquidity, tick],
        ).hex(),
    }


def v3_mint(
    *,
    tick_lower: int,
    tick_upper: int,
    amount: int,
    owner: str = ZERO_ADDRESS,
    sender: str = ZERO_ADDRESS,
) -> dict[str, object]:
    """`Mint(address,address,int24,int24,uint128,uint256,uint256)`."""
    return {
        "topics": [V3_MINT_TOPIC, _address_topic(owner), word(tick_lower), word(tick_upper)],
        "data": "0x"
        + abi_encode(
            types=["address", "uint128", "uint256", "uint256"],
            args=[sender, amount, 0, 0],
        ).hex(),
    }


def v3_burn(
    *,
    tick_lower: int,
    tick_upper: int,
    amount: int,
    owner: str = ZERO_ADDRESS,
) -> dict[str, object]:
    """`Burn(address,int24,int24,uint128,uint256,uint256)` — a negative delta."""
    return {
        "topics": [V3_BURN_TOPIC, _address_topic(owner), word(tick_lower), word(tick_upper)],
        "data": "0x"
        + abi_encode(types=["uint128", "uint256", "uint256"], args=[amount, 0, 0]).hex(),
    }


def v4_swap(
    *,
    pool_id: str,
    sqrt_price_x96: int,
    liquidity: int,
    tick: int,
    fee: int = 0,
    amount0: int = 0,
    amount1: int = 0,
    sender: str = ZERO_ADDRESS,
) -> dict[str, object]:
    """`Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`."""
    return {
        "topics": [V4_SWAP_TOPIC, pool_id, _address_topic(sender)],
        "data": "0x"
        + abi_encode(
            types=["int128", "int128", "uint160", "uint128", "int24", "uint24"],
            args=[amount0, amount1, sqrt_price_x96, liquidity, tick, fee],
        ).hex(),
    }


def v4_modify_liquidity(
    *,
    pool_id: str,
    tick_lower: int,
    tick_upper: int,
    liquidity_delta: int,
    sender: str = ZERO_ADDRESS,
    salt: bytes = b"\x00" * 32,
) -> dict[str, object]:
    """`ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)`."""
    return {
        "topics": [V4_MODIFY_LIQUIDITY_TOPIC, pool_id, _address_topic(sender)],
        "data": "0x"
        + abi_encode(
            types=["int24", "int24", "int256", "bytes32"],
            args=[tick_lower, tick_upper, liquidity_delta, salt],
        ).hex(),
    }


def _address_topic(address: str) -> str:
    """An indexed `address` topic (left-padded to the 32-byte word)."""
    return "0x" + "00" * 12 + address.removeprefix("0x").lower()


__all__ = [
    "V2_SYNC_TOPIC",
    "V3_BURN_TOPIC",
    "V3_MINT_TOPIC",
    "V3_SWAP_TOPIC",
    "V4_MODIFY_LIQUIDITY_TOPIC",
    "V4_SWAP_TOPIC",
    "ZERO_ADDRESS",
    "v2_sync",
    "v3_burn",
    "v3_mint",
    "v3_swap",
    "v4_modify_liquidity",
    "v4_swap",
    "word",
]
