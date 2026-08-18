"""
ExttloadComparator — Vyper

Compares exttload() output between the real on-chain V4 PoolManager
and the fake Vyper implementation.

Uses IPoolManager typed interface for all PM interactions.
Currency is encoded identically to address at the EVM level,
so the same interface works for both PMs.
"""

from .interfaces.UniswapV4 import IPoolManager, IPoolManagerExttload, IUnlockCallback

# V4 transient storage slot constants
T_LOCK_SLOT: constant(bytes32) = 0xc090fc4683624cfc3884e9d8de5eca132f2d0ec062aff75d43c0465d5ceeab23
T_NONZERO_DELTA_COUNT_SLOT: constant(bytes32) = 0x7d4b3164c6e45b97e7d87b7125a44c5828d005af88f9d751cfd78729c5d99a0b

ONE_ETH: constant(uint256) = 1000000000000000000

real_pm: immutable(address)
fake_pm: immutable(address)
NATIVE_DELTA_SLOT: immutable(bytes32)

real_exttload_value: public(bytes32)
fake_exttload_value: public(bytes32)

s_is_real_pm: transient(bool)
s_result: transient(bytes32)

s_real_lock: public(bytes32)
s_real_count: public(bytes32)
s_real_delta: public(bytes32)
s_fake_lock: public(bytes32)
s_fake_count: public(bytes32)
s_fake_delta: public(bytes32)


@deploy
def __init__(_real_pm: address, _fake_pm: address):
    real_pm = _real_pm
    fake_pm = _fake_pm
    NATIVE_DELTA_SLOT = keccak256(
        concat(
            convert(convert(self, uint160), bytes32),
            convert(convert(empty(address), uint160), bytes32),
        )
    )


@external
@view
def computeSlot(target: address, currency: address) -> bytes32:
    return keccak256(
        concat(
            convert(convert(target, uint160), bytes32),
            convert(convert(currency, uint160), bytes32),
        )
    )


@external
@view
def nativeDeltaSlot() -> bytes32:
    return NATIVE_DELTA_SLOT


@external
@payable
def compare() -> bool:
    # Fake PM must already be funded by the caller before invoking compare().

    # Real PM: unlock -> callback reads delta
    self.s_is_real_pm = True
    extcall IPoolManager(real_pm).unlock(abi_encode(True))
    real_value: bytes32 = self.s_result

    # Fake PM: unlock -> callback reads delta
    self.s_is_real_pm = False
    extcall IPoolManager(fake_pm).unlock(abi_encode(False))
    fake_value: bytes32 = self.s_result

    self.real_exttload_value = real_value
    self.fake_exttload_value = fake_value

    assert real_value == fake_value, "exttload mismatch"
    return True


@external
@payable
def readSlotsInsideCallback():
    extcall IPoolManager(real_pm).unlock(abi_encode(True))
    self.s_is_real_pm = False
    extcall IPoolManager(fake_pm).unlock(abi_encode(False))


@external
def unlockCallback(data: Bytes[128]) -> Bytes[512]:
    is_real_pm: bool = abi_decode(data, bool)

    if is_real_pm:
        self._callback_real_pm()
    else:
        self._callback_fake_pm()

    return abi_encode(self.s_result)


@internal
def _callback_real_pm():
    pm: address = real_pm

    extcall IPoolManager(pm).take(empty(address), self, ONE_ETH)

    delta_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(NATIVE_DELTA_SLOT)
    lock_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(T_LOCK_SLOT)
    count_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(T_NONZERO_DELTA_COUNT_SLOT)

    self.s_result = delta_value
    self.s_real_lock = lock_value
    self.s_real_count = count_value
    self.s_real_delta = delta_value

    extcall IPoolManager(pm).sync(empty(address))
    extcall IPoolManager(pm).settle(value=ONE_ETH)


@internal
def _callback_fake_pm():
    pm: address = fake_pm

    extcall IPoolManager(pm).take(empty(address), self, ONE_ETH)

    delta_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(NATIVE_DELTA_SLOT)
    lock_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(T_LOCK_SLOT)
    count_value: bytes32 = staticcall IPoolManagerExttload(pm).exttload(T_NONZERO_DELTA_COUNT_SLOT)

    self.s_result = delta_value
    self.s_fake_lock = lock_value
    self.s_fake_count = count_value
    self.s_fake_delta = delta_value

    extcall IPoolManager(pm).sync(empty(address))
    extcall IPoolManager(pm).settle(value=ONE_ETH)


@external
@view
def readArbitrarySlot(slot: bytes32) -> (bytes32, bytes32):
    return (
        staticcall IPoolManagerExttload(real_pm).exttload(slot),
        staticcall IPoolManagerExttload(fake_pm).exttload(slot),
    )


@external
@payable
def __default__():
    return
