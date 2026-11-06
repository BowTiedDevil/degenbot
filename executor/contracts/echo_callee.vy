"""
EchoCallee — callback payload double for the fake Uniswap contracts.

Stands in for any swap callee or unlock-callback implementer. Callback
payloads are self-verifying: word 0 carries the payload length and word 1
the keccak256 digest of everything except that digest word, so a callback
can prove the payload arrived intact even though a revert rolls back any
state it writes.

The verified marker is a string revert for that reason: revert data is the
only thing that survives the rollback. unlockCallback instead records and
returns the payload, because that path succeeds and keeps its state.
"""

#pragma version ^0.5.0b2
#pragma experimental-codegen

  
interface IPoolManagerLike:
    def unlock(data: Bytes[INF]) -> Bytes[INF]: nonpayable


interface IV3PoolLike:
    def swap(
        recipient: address,
        zero_for_one: bool,
        amount_specified: int256,
        sqrt_price_limit_x96: uint160,
        data: Bytes[INF],
    ): nonpayable


last_len: public(uint256)
last_digest: public(bytes32)


@internal
def _digest(data: Bytes[INF]) -> bytes32:
    return keccak256(concat(slice(data, 0, 32), slice(data, 64, len(data) - 64)))


@internal
def _verify(data: Bytes[INF]):
    expected_len: uint256 = convert(slice(data, 0, 32), uint256)
    expected_digest: bytes32 = convert(slice(data, 32, 32), bytes32)
    assert len(data) == expected_len, "ECHO_BAD_LEN"
    assert self._digest(data) == expected_digest, "ECHO_BAD_DIGEST"


@external
def unlockCallback(data: Bytes[INF]) -> Bytes[INF]:
    self._verify(data)
    self.last_len = len(data)
    self.last_digest = self._digest(data)
    return data


@external
def do_unlock(pool_manager: address, data: Bytes[INF]) -> Bytes[INF]:
    return extcall IPoolManagerLike(pool_manager).unlock(data)


@external
def do_swap_v3(
    pool: address,
    recipient: address,
    zero_for_one: bool,
    amount_specified: int256,
    sqrt_price_limit_x96: uint160,
    data: Bytes[INF],
):
    extcall IV3PoolLike(pool).swap(
        recipient, zero_for_one, amount_specified, sqrt_price_limit_x96, data
    )


@external
def uniswapV2Call(sender: address, amount0Out: uint256, amount1Out: uint256, data: Bytes[INF]):
    self._verify(data)
    raise "ECHO_VERIFIED"


@external
def hook(sender: address, amount0Out: uint256, amount1Out: uint256, data: Bytes[INF]):
    self._verify(data)
    raise "ECHO_VERIFIED"


@external
def pancakeCall(sender: address, amount0Out: uint256, amount1Out: uint256, data: Bytes[INF]):
    self._verify(data)
    raise "ECHO_VERIFIED"


@external
def uniswapV3SwapCallback(amount0: int256, amount1: int256, data: Bytes[INF]):
    self._verify(data)
    raise "ECHO_VERIFIED"


@external
def pancakeV3SwapCallback(amount0: int256, amount1: int256, data: Bytes[INF]):
    self._verify(data)
    raise "ECHO_VERIFIED"
