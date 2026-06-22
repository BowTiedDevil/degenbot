"""Tracer-bullet tests for PyBotIo (ADR-005 slice 14a).

`PyBotIo` is the Rust `#[pyclass]` I/O façade that builders will receive in
place of the Python `SyncPoolIO` adapter. It holds a Python provider (the
`ProviderAdapter` the `Bot` was constructed with) + an optional DB handle, and
exposes the 7-method `PoolIO` surface (`get_block_number`, `get_block`,
`get_block_timestamp`, `get_code`, `get_balance`, `call`, `call_raw`) by
delegating to the held provider.

These tests pin the *seam* -- that delegating through the Rust pyclass yields
the same observable result as calling the provider directly. They do NOT yet
route a real builder through `PyBotIo`; that's the 14a follow-on (one builder's
`build()` via `PyBotIo`), and 14b extends it to all families.
"""

from __future__ import annotations

from typing import Any

import eth_abi.abi
import pytest
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import Web3Exception

from degenbot.builders.pool_io import SyncPoolIO
from degenbot.builders.type_resolution import fetch_factory_from_chain
from degenbot.degenbot_rs import PyBotIo


class _FakeProvider:
    """A minimal ``ProviderAdapter``-shaped double for the tracer.

    Only the 7 ``PoolIO`` methods are exercised; the rest of the
    ``ProviderAdapter`` surface is irrelevant to ``PoolIO`` conformance.
    """

    def __init__(self, *, block_number: int = 18_000_000) -> None:
        self._block_number = block_number
        self.calls: list[tuple[str, str]] = []  # (to, data_hex) audit trail

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": int(block_identifier), "timestamp": 1_700_000_000}

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x60\x80\x60\x40")  # plausible bytecode prefix

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data.hex()))
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx: Any, block: int | None = None) -> HexBytes:
        return self.call(tx["to"], tx["data"], block)


class _FakeDb:
    """A ``DatabaseSessionManager``-shaped double (cannot be called; presence only)."""


def test_pybot_io_delegates_get_block_number():
    """get_block_number delegates to the held provider verbatim."""
    provider = _FakeProvider(block_number=12_345_678)
    io = PyBotIo(provider=provider)
    assert io.get_block_number() == 12_345_678


def test_pybot_io_delegates_call_records_on_provider():
    """call(to, data, block) delegates to provider.call and returns its HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    result = io.call(to="0x" + "ab" * 20, data=b"\x12\x34\x56\x78", block=None)
    assert result == HexBytes(b"\x00" * 32)
    assert provider.calls == [("0x" + "ab" * 20, "12345678")]


def test_pybot_io_delegates_get_code():
    """get_code delegates and returns HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    code = io.get_code("0x" + "cd" * 20)
    assert code == HexBytes(b"\x60\x80\x60\x40")


def test_pybot_io_delegates_get_balance():
    """get_balance delegates and returns int (not wrapped)."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    assert io.get_balance("0x" + "ee" * 20) == 10**18


def test_pybot_io_holds_optional_db_handle():
    """PyBotIo stores the DB handle and exposes it back (held, not called yet)."""
    db = _FakeDb()
    io = PyBotIo(provider=_FakeProvider(), db=db)
    # The held handle round-trips through the pyclass.
    assert io.db is db


@pytest.mark.parametrize(
    "method",
    [
        "get_block_number",
        "get_block",
        "get_block_timestamp",
        "get_code",
        "get_balance",
        "call",
        "call_raw",
    ],
)
def test_pybot_io_satisfies_pool_io_protocol(method: str):
    """PyBotIo exposes the full 7-method PoolIO surface (runtime protocol check).

    This is the acceptance criterion for 14a: every method a builder may call
    on its ``io: PoolIO`` parameter is reachable on ``PyBotIo``.
    """
    io = PyBotIo(provider=_FakeProvider())
    assert hasattr(io, method), f"PyBotIo missing PoolIO method {method!r}"


# === I/O choreography methods (slice 14b) ===
#
# `fetch_factory_address` is the first choreography method moved into `PyBotIo`:
# the multi-step (encode `factory()` selector -> `eth_call` -> ABI-decode `address`
# -> EIP-55 checksum), previously `fetch_factory_from_chain` in
# `type_resolution.py`, now reachable as a single Rust-owned method. The RPC
# primitive (`call`) still delegates to the held provider (the native-alloy
# swap is a later slice); the *choreography* -- the orchestration of those 4
# steps -- now lives in Rust, satisfying slice 14's "methods for the builder
# I/O choreography … moved here, called from Python via PyBotIo".


class _FactoryCallProvider:
    """Provider double that returns an ABI-encoded factory address for `factory()`.

    Mirrors ``ProviderAdapter.call(*, to, data, block)`` (kw-only) so it stays
    compatible with ``PyBotIo``'s kw-only forward contract.
    """

    def __init__(self, factory_raw: str) -> None:
        # factory_raw is the 40-hex-char lowercase address (no 0x prefix),
        # ABI-encoded right-aligned in a 32-byte word -- what a real
        # `factory()` call returns.
        self._encoded = eth_abi.abi.encode(types=["address"], args=[factory_raw])
        self.calls: list[tuple[str, bytes]] = []  # (to, data)

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data))
        return HexBytes(self._encoded)


def test_pybot_io_fetch_factory_address_decodes_and_checksums():
    """PyBotIo.fetch_factory_address runs encode->call->decode->checksum in Rust
    and returns an EIP-55 checksummed address.
    """
    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"  # arbitrary 20-byte
    expected = "0x66f9664f97F2b50F62D13eA064982f936dE76657"  # EIP-55 of above
    pool_address = "0x" + "ab" * 20
    provider = _FactoryCallProvider(factory_raw)
    io = PyBotIo(provider=provider)

    result = io.fetch_factory_address(pool_address)

    assert result == expected
    # Exactly one call made, to the pool address, with the 4-byte `factory()`
    # selector (keccak256("factory()")[:4] = 0xc45a0155).
    assert len(provider.calls) == 1
    to, data = provider.calls[0]
    assert to == pool_address
    assert data[:4] == bytes.fromhex("c45a0155")


def test_pybot_io_fetch_factory_address_returns_none_on_revert():
    """On a provider-side error (revert / call failure), return None -- mirrors
    `fetch_factory_from_chain`'s `except (Web3Exception, DecodingError): return None`."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    assert io.fetch_factory_address("0x" + "ab" * 20) is None


def test_pybot_io_fetch_factory_parity_with_python_impl():
    """`PyBotIo.fetch_factory_address` returns the exact same EIP-55 checksum
    as the original Python `fetch_factory_from_chain` for the same provider
    `call` result.

    Two independent implementations (Rust on PyBotIo, Python on SyncPoolIO)
    against identical backends must agree -- this is the parity gate that lets
    `Bot.build_pool` route through `PyBotIo.fetch_factory_address` without
    behavior change. The SyncPoolIO path exercises the original Python
    decode/checksum; the PyBotIo path exercises the Rust impl."""
    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"
    pool_address = "0x" + "ab" * 20

    rust_result = PyBotIo(provider=_FactoryCallProvider(factory_raw)).fetch_factory_address(
        pool_address
    )
    py_result = fetch_factory_from_chain(
        pool_address, chain_id=1, io=SyncPoolIO(_FactoryCallProvider(factory_raw))
    )

    assert rust_result == py_result


# === ERC20 metadata choreography (slice 14c) ===
#
# `fetch_erc20_metadata` is the second choreography method: the batched
# name/symbol/decimals RPC fetch (3 selectors -> 3 eth_calls -> 3 ABI-decodes).
# Mirrors `_fetch_name_symbol_decimals_batched` in `erc20_builder.py`. The
# `Erc20Builder.build` caller's fallback contract is: if the batched call fails
# (call raised, decode failed), try individual calls with `bytes32` alternate
# prototypes. `PyBotIo.fetch_erc20_metadata` returns `None` on any such failure
# (mirrors `except (Web3Exception, DecodingError): return None` style) so the
# caller's fallback kicks in identically.


class _Erc20MetadataProvider:
    """Provider double returning ABI-encoded name/symbol/decimals for the 3 selectors."""

    def __init__(self, *, name: str, symbol: str, decimals: int) -> None:
        self._responses: dict[bytes, bytes] = {
            # keccak256("name()")[..4] = 0x06fdde03
            bytes.fromhex("06fdde03"): eth_abi.abi.encode(types=["string"], args=[name]),
            # keccak256("symbol()")[..4] = 0x95d89b41
            bytes.fromhex("95d89b41"): eth_abi.abi.encode(types=["string"], args=[symbol]),
            # keccak256("decimals()")[..4] = 0x313ce567
            bytes.fromhex("313ce567"): eth_abi.abi.encode(types=["uint256"], args=[decimals]),
        }
        self.calls: list[bytes] = []  # data received

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


def test_pybot_io_fetch_erc20_metadata_decodes_string_string_uint():
    """PyBotIo.fetch_erc20_metadata runs 3-call encode->call->decode choreography
    in Rust and returns (name, symbol, decimals) with exact ABI semantics."""
    name, symbol, decimals = "Dai Stablecoin", "DAI", 18
    io = PyBotIo(provider=_Erc20MetadataProvider(name=name, symbol=symbol, decimals=decimals))

    result = io.fetch_erc20_metadata("0x" + "ab" * 20)

    assert result is not None
    got_name, got_symbol, got_decimals = result
    assert got_name == name
    assert got_symbol == symbol
    assert got_decimals == decimals


def test_pybot_io_fetch_erc20_metadata_returns_none_on_decode_failure():
    """A truncated return (not a valid ABI string) yields None -- mirrors the
    Python batched impl's `except DecodingError` fallback contract."""

    class _MalformedProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            # selector = data[:4]; for any selector return 1 byte -- too short to decode.
            return HexBytes(b"\x00")

    io = PyBotIo(provider=_MalformedProvider())
    assert io.fetch_erc20_metadata("0x" + "ab" * 20) is None


def test_pybot_io_fetch_erc20_metadata_returns_none_on_revert():
    """A provider.call() revert (any exception) yields None -- the batched
    fallback kicks in identically to the Python `except Web3Exception` path."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    assert io.fetch_erc20_metadata("0x" + "ab" * 20) is None


from degenbot.builders.erc20_builder import _fetch_name_symbol_decimals_batched


def test_pybot_io_fetch_erc20_metadata_parity_with_python_batched():
    """`PyBotIo.fetch_erc20_metadata` returns the exact same tuple as the Python
    `_fetch_name_symbol_decimals_batched` for the same provider `call` results."""
    name, symbol, decimals = "Wrapped Ether", "WETH", 18
    address = "0x" + "cd" * 20

    rust_result = PyBotIo(
        provider=_Erc20MetadataProvider(name=name, symbol=symbol, decimals=decimals)
    ).fetch_erc20_metadata(address)
    py_result = _fetch_name_symbol_decimals_batched(
        address=address,
        io=SyncPoolIO(_Erc20MetadataProvider(name=name, symbol=symbol, decimals=decimals)),
    )

    assert rust_result is not None
    assert rust_result == py_result


# === ERC20 read methods (slice 14d) ===
#
# `fetch_token_balance` / `fetch_token_allowance` / `fetch_token_total_supply`
# move the three stateless token-read choreographies (balanceOf(address),
# allowance(owner,spender), totalSupply()) from Erc20Builder into PyBotIo.
# Each is a clean encode->call->decode atom -- pure-RPC, no DB same as 14c but
# now with ABI-encoded arguments, covering the parameterized-call pattern the
# snapshot-update choreographies (V2 getReserves, V3 slot0) are also shaped by.


class _AddressArgProvider:
    """Provider double: encodes matching call-data -> canned ABI uint256 result."""

    def __init__(self, *, response: bytes) -> None:
        self._response = response
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._response)


def _uint256_encoded(value: int) -> bytes:
    return eth_abi.abi.encode(types=["uint256"], args=[value])


def test_pybot_io_fetch_token_balance_encodes_owner_arg_and_decodes_uint256():
    """fetch_token_balance(token, owner) -- balanceOf(address) selector +
    ABI-encoded `address` arg, ABI-decoded uint256 result."""
    balance = 1_000_000 * 10**18
    provider = _AddressArgProvider(response=_uint256_encoded(balance))
    io = PyBotIo(provider=provider)

    token = "0x" + "aa" * 20
    owner = "0x" + "bb" * 20

    result = io.fetch_token_balance(token, owner)

    assert result == balance
    # balanceOf(address) selector = 0x70a08231; the data's first 4 bytes match,
    # and bytes 16..36 are the right-padded 20-byte address argument.
    assert provider.calls[0][:4] == bytes.fromhex("70a08231")
    assert provider.calls[0][16:36] == bytes.fromhex("bb" * 20)


def test_pybot_io_fetch_token_allowance_encodes_owner_spender_args():
    """fetch_token_allowance(token, owner, spender) -- allowance(address,address)
    selector + two ABI-encoded address args, decoded uint256 result."""
    allowance = 5_000 * 10**6
    provider = _AddressArgProvider(response=_uint256_encoded(allowance))
    io = PyBotIo(provider=provider)

    token = "0x" + "aa" * 20
    owner = "0x" + "bb" * 20
    spender = "0x" + "cc" * 20

    result = io.fetch_token_allowance(token, owner, spender)

    assert result == allowance
    # allowance(address,address) selector = 0xdd62ed3e.
    assert provider.calls[0][:4] == bytes.fromhex("dd62ed3e")
    # args packed right-aligned in 32-byte words: word0 (bytes 4..36) = owner,
    # word1 (bytes 36..68) = spender. Verify owner @ 16..36, spender @ 48..68.
    assert provider.calls[0][16:36] == bytes.fromhex("bb" * 20)
    assert provider.calls[0][48:68] == bytes.fromhex("cc" * 20)


def test_pybot_io_fetch_token_total_supply_no_args_decodes_uint256():
    """fetch_token_total_supply(token) -- totalSupply() selector, no args, decoded
    uint256 result."""
    total_supply = 21_000_000 * 10**18
    provider = _AddressArgProvider(response=_uint256_encoded(total_supply))
    io = PyBotIo(provider=provider)

    result = io.fetch_token_total_supply("0x" + "aa" * 20)

    assert result == total_supply
    # totalSupply() selector = 0x18160ddd; call-data is exactly 4 bytes (no arg).
    assert provider.calls[0] == bytes.fromhex("18160ddd")


def test_pybot_io_fetch_token_balance_propagates_reverts_as_exception():
    """A provider.call() revert propagates as an exception — mirrors
    Erc20Builder.get_token_balance's no-try/except contract (revert / decode
    failure surfaces untouched, the Python caller decides)."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    with pytest.raises(RuntimeError):
        io.fetch_token_balance("0x" + "aa" * 20, "0x" + "bb" * 20)


# === V2 pool immutable + reserves choreographies (slice 14e) ===
#
# `fetch_v2_immutable_data` and `fetch_v2_reserves` move the two pure-RPC
# sub-choreographies of `_fetch_v2_common_data` (factory/token0/token1 +
# getReserves) into Rust. The DB lookup, `extract_db_values`, and
# `resolve_deployer_and_init_hash` dispatch stay Python-side -- these are the
# *RPC* pieces only, building on the 14d `forward_call_to_provider`/`selector`
# seams. The DB-delegation tracer is explicitly deferred to 14f.


class _V2PoolProvider:
    """Provider double returning ABI-encoded immutable + reserves values."""

    def __init__(
        self, *, factory: str, token0: str, token1: str, reserves0: int, reserves1: int
    ) -> None:
        # Selectors for the 4 reads this provider answers.
        self._responses: dict[bytes, bytes] = {
            # keccak256("factory()")[..4] = 0xc45a0155
            bytes.fromhex("c45a0155"): eth_abi.abi.encode(types=["address"], args=[factory]),
            # keccak256("token0()")[..4] = 0x0dfe1681
            bytes.fromhex("0dfe1681"): eth_abi.abi.encode(types=["address"], args=[token0]),
            # keccak256("token1()")[..4] = 0xd21220a7
            bytes.fromhex("d21220a7"): eth_abi.abi.encode(types=["address"], args=[token1]),
            # keccak256("getReserves()")[..4] = 0x0902f1ac
            bytes.fromhex("0902f1ac"): eth_abi.abi.encode(
                types=["uint256", "uint256"], args=[reserves0, reserves1]
            ),
        }
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


def _eip55(addr: str) -> str:
    from degenbot.checksum_cache import get_checksum_address

    return get_checksum_address(addr)


def test_pybot_io_fetch_v2_immutable_data_decodes_factory_token0_token1():
    """`fetch_v2_immutable_data(address)` runs the 3-call factory/token0/token1 RPC
    choreography in Rust and returns EIP-55 checksummed addresses."""
    raw_factory = "0x" + "11" * 20
    raw_token0 = "0x" + "22" * 20
    raw_token1 = "0x" + "33" * 20
    io = PyBotIo(
        provider=_V2PoolProvider(
            factory=raw_factory, token0=raw_token0, token1=raw_token1, reserves0=0, reserves1=0
        )
    )

    factory, token0, token1 = io.fetch_v2_immutable_data("0x" + "aa" * 20)

    assert factory == _eip55(raw_factory)
    assert token0 == _eip55(raw_token0)
    assert token1 == _eip55(raw_token1)


def test_pybot_io_fetch_v2_reserves_decodes_two_uint256():
    """`fetch_v2_reserves(address)` runs getReserves() in Rust, ABI-decodes the
    (uint256, uint256) tuple, returns both as Python ints (large values stay
    exact, no float coercion)."""
    r0 = 1_234_567_890_123_456_789  # larger than i64
    r1 = 9_876_543_210_987_654_321
    io = PyBotIo(
        provider=_V2PoolProvider(
            factory="0x" + "11" * 20,
            token0="0x" + "22" * 20,
            token1="0x" + "33" * 20,
            reserves0=r0,
            reserves1=r1,
        )
    )

    result = io.fetch_v2_reserves("0x" + "aa" * 20)

    assert result == (r0, r1)


def test_pybot_io_fetch_v2_immutable_data_propagates_reverts_as_exception():
    """A provider.call revert surfaces as an exception (no swallowing) -- mirrors
    `_fetch_v2_common_data`'s `except Exception: raise LiquidityPoolError contract."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    with pytest.raises(RuntimeError):
        io.fetch_v2_immutable_data("0x" + "aa" * 20)


# === V3 pool immutable + slot0/liquidity choreographies (slice 14f) ===
#
# `fetch_v3_immutable_data` and `fetch_v3_slot0_liquidity` mirror 14e's V2
# work for UniswapV3Pool builders: 5-call immutable fetch (factory/token0/token1/
# fee/tickSpacing) + 2-call state fetch (slot0/liquidity). The no-arg address-
# returning reads reuse the 14e `fetch_address_returning_method` seam.


class _V3PoolProvider:
    """Provider double returning ABI-encoded V3 immutable + state values."""

    def __init__(
        self,
        *,
        factory: str,
        token0: str,
        token1: str,
        fee: int,
        tick_spacing: int,
        sqrt_price_x96: int,
        tick: int,
        liquidity: int,
    ) -> None:
        self._responses: dict[bytes, bytes] = {
            # factory() / token0() / token1() selectors (same as V2).
            bytes.fromhex("c45a0155"): eth_abi.abi.encode(types=["address"], args=[factory]),
            bytes.fromhex("0dfe1681"): eth_abi.abi.encode(types=["address"], args=[token0]),
            bytes.fromhex("d21220a7"): eth_abi.abi.encode(types=["address"], args=[token1]),
            # keccak256("fee()")[..4] = 0xddca3f43
            bytes.fromhex("ddca3f43"): eth_abi.abi.encode(types=["uint24"], args=[fee]),
            # keccak256("tickSpacing()")[..4] = 0xd0c93a7c
            bytes.fromhex("d0c93a7c"): eth_abi.abi.encode(types=["int24"], args=[tick_spacing]),
            # keccak256("slot0()")[..4] = 0x3850c7bd
            bytes.fromhex("3850c7bd"): eth_abi.abi.encode(
                types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
                args=[sqrt_price_x96, tick, 0, 0, 0, 0, False],
            ),
            # keccak256("liquidity()")[..4] = 0x1a686502
            bytes.fromhex("1a686502"): eth_abi.abi.encode(types=["uint128"], args=[liquidity]),
        }
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


def test_pybot_io_fetch_v3_immutable_data_decodes_factory_token0_token1_fee_tickspacing():
    """`fetch_v3_immutable_data(address)` runs the 5-call immutable choreography
    in Rust; addresses are EIP-55 checksummed; fee + tick_spacing returned as ints."""
    raw_factory = "0x" + "11" * 20
    raw_token0 = "0x" + "22" * 20
    raw_token1 = "0x" + "33" * 20
    fee = 500  # 0.05% fee tier
    tick_spacing = 10
    io = PyBotIo(
        provider=_V3PoolProvider(
            factory=raw_factory,
            token0=raw_token0,
            token1=raw_token1,
            fee=fee,
            tick_spacing=tick_spacing,
            sqrt_price_x96=0,
            tick=0,
            liquidity=0,
        )
    )

    factory, token0, token1, got_fee, got_tick_spacing = io.fetch_v3_immutable_data(
        "0x" + "aa" * 20
    )

    assert factory == _eip55(raw_factory)
    assert token0 == _eip55(raw_token0)
    assert token1 == _eip55(raw_token1)
    assert got_fee == fee
    assert got_tick_spacing == tick_spacing


def test_pybot_io_fetch_v3_immutable_data_handles_negative_tick_spacing():
    """tick_spacing is int24; decode path handles the sign-extension correctly.
    (Negative tick spacing doesn't happen on-chain but exercises the int24 decode.)"""
    io = PyBotIo(
        provider=_V3PoolProvider(
            factory="0x" + "11" * 20,
            token0="0x" + "22" * 20,
            token1="0x" + "33" * 20,
            fee=500,
            tick_spacing=-7,
            sqrt_price_x96=0,
            tick=0,
            liquidity=0,
        )
    )

    _, _, _, _, got_tick_spacing = io.fetch_v3_immutable_data("0x" + "aa" * 20)

    assert got_tick_spacing == -7


def test_pybot_io_fetch_v3_slot0_liquidity_decodes_sqrt_price_tick_liquidity():
    """`fetch_v3_slot0_liquidity(address)` runs the 2-call state choreography;
    slot0's packed tuple is decoded as (uint160 sqrtPriceX96, int24 tick, ...) and
    liquidity's uint128 → Python int (large values preserved)."""
    sqrt_price_x96 = 1_005_993_009_944_122_914_871_674_682  # ≈ real mainnet value
    tick = -5
    liquidity = 1_234_567_890_123_456_789_012_345  # > u64
    io = PyBotIo(
        provider=_V3PoolProvider(
            factory="0x" + "11" * 20,
            token0="0x" + "22" * 20,
            token1="0x" + "33" * 20,
            fee=500,
            tick_spacing=10,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            liquidity=liquidity,
        )
    )

    got_sqrt, got_tick, got_liq = io.fetch_v3_slot0_liquidity("0x" + "aa" * 20)

    assert got_sqrt == sqrt_price_x96
    assert got_tick == tick
    assert got_liq == liquidity


def test_pybot_io_fetch_v3_immutable_data_propagates_reverts_as_exception():
    """Provider revert surfaces as exception -- mirrors V3PoolBuilder's
    `except Exception: raise LiquidityPoolError` contract."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    with pytest.raises(RuntimeError):
        io.fetch_v3_immutable_data("0x" + "aa" * 20)


# === Aerodrome V2 stable+fee choreography (slice 14g) ===
#
# `fetch_aerodrome_v2_stable_and_fee` moves the 2-call Aerodrome-specific
# choreography (stable() -> bool, then getFee(address,bool) -> uint256)
# into Rust. New pattern: `getFee` takes mixed-type args (address + bool),
# and the second call depends on the first's result (data dependency).


class _AerodromeProvider:
    """Provider double for Aerodrome stable() + getFee(address,bool)."""

    def __init__(self, *, stable: bool, fee_raw: int) -> None:
        self._stable = stable
        self._fee_raw = fee_raw
        self.calls: list[tuple[str, bytes]] = []  # (to, data) audit trail

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data))
        sel = data[:4]
        # keccak256("stable()")[..4]
        _stable_sel = Web3.keccak(text="stable()")[:4]
        if sel == _stable_sel:
            return HexBytes(eth_abi.abi.encode(types=["bool"], args=[self._stable]))
        # keccak256("getFee(address,bool)")[..4]
        _get_fee_sel = Web3.keccak(text="getFee(address,bool)")[:4]
        if sel == _get_fee_sel:
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._fee_raw]))
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


def test_pybot_io_fetch_aerodrome_stable_and_fee_returns_bool_and_uint256():
    """fetch_aerodrome_v2_stable_and_fee(pool, factory) runs the 2-call
    choreography: stable() -> bool, then getFee(pool, stable) -> uint256,
    with the first call's result fed as an argument to the second."""
    stable = True
    fee_raw = 30  # Aerodrome fee numerator (denominator 10000 = 0.3%)
    pool = "0x" + "aa" * 20
    factory = "0x" + "bb" * 20

    io = PyBotIo(provider=_AerodromeProvider(stable=stable, fee_raw=fee_raw))

    got_stable, got_fee = io.fetch_aerodrome_v2_stable_and_fee(pool, factory)

    assert got_stable is True
    assert got_fee == fee_raw

    # First call: stable() to the pool address.
    assert io.provider.calls[0] == (pool, Web3.keccak(text="stable()")[:4])
    # Second call: getFee(address,bool) to the factory address.
    second_to, second_data = io.provider.calls[1]
    assert second_to == factory
    assert second_data[:4] == Web3.keccak(text="getFee(address,bool)")[:4]
    # Verify the encoded args: pool address (right-padded in word 0),
    # stable bool (word 1).
    assert second_data[16:36] == bytes.fromhex("aa" * 20)  # pool address
    assert second_data[67] == 1  # bool True (last byte of word 1)


def test_pybot_io_fetch_aerodrome_stable_and_fee_handles_false_stable():
    """When stable() returns False, getFee is called with bool=False in the
    second word (last byte = 0)."""
    io = PyBotIo(provider=_AerodromeProvider(stable=False, fee_raw=100))

    got_stable, got_fee = io.fetch_aerodrome_v2_stable_and_fee("0x" + "aa" * 20, "0x" + "bb" * 20)

    assert got_stable is False
    assert got_fee == 100
    # The bool arg in word 1 is 0.
    _, second_data = io.provider.calls[1]
    assert second_data[67] == 0


def test_pybot_io_fetch_aerodrome_stable_and_fee_propagates_reverts():
    """A provider revert on either call surfaces as an exception (no swallowing)."""

    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    with pytest.raises(RuntimeError):
        io.fetch_aerodrome_v2_stable_and_fee("0x" + "aa" * 20, "0x" + "bb" * 20)


# === ERC20 string-field fallback (slice 14h) ===
#
# `fetch_erc20_string_field` moves the per-call `_fetch_name`/`_fetch_symbol`
# fallback choreography (string decode, then bytes32 fallback) into Rust.
# The Python caller passes the selector signature dynamically ("name()" vs
# "NAME()"), so the Rust method accepts a `signature: &str` parameter.


class _StringFieldProvider:
    """Provider returning either ABI string or bytes32 for a given selector."""

    def __init__(self, response: bytes) -> None:
        self._response = response

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return HexBytes(self._response)


def test_pybot_io_fetch_erc20_string_field_decodes_string():
    """When the token returns an ABI string, fetch_erc20_string_field decodes it."""
    name = "Dai Stablecoin"
    io = PyBotIo(
        provider=_StringFieldProvider(response=eth_abi.abi.encode(types=["string"], args=[name]))
    )

    result = io.fetch_erc20_string_field("0x" + "ab" * 20, "name()")

    assert result == name


def test_pybot_io_fetch_erc20_string_field_falls_back_to_bytes32():
    """When the token returns bytes32 (not a valid ABI string), the Rust impl
    falls back to bytes32 decode + UTF-8 with errors='ignore' + strip nulls.
    Mirrors _fetch_name's `except DecodingError: decode bytes32` path."""
    # A bytes32-encoded name: "USDT" right-padded with nulls.
    raw = b"USDT" + b"\x00" * 28
    # ABI-encode as bytes32 (the 32 raw bytes, no offset/length prefix).
    # But actually some tokens return raw bytes32 without ABI string encoding.
    # eth_abi encodes bytes32 as just the 32 bytes themselves.
    io = PyBotIo(
        provider=_StringFieldProvider(response=eth_abi.abi.encode(types=["bytes32"], args=[raw]))
    )

    result = io.fetch_erc20_string_field("0x" + "ab" * 20, "symbol()")

    assert result == "USDT"


def test_pybot_io_fetch_erc20_string_field_strips_trailing_nulls():
    """bytes32 fallback strips both leading and trailing null bytes (mirrors
    Python's `.strip("\x00")`)."""
    raw = b"\x00\x00WETH\x00" + b"\x00" * 25
    io = PyBotIo(
        provider=_StringFieldProvider(response=eth_abi.abi.encode(types=["bytes32"], args=[raw]))
    )

    result = io.fetch_erc20_string_field("0x" + "ab" * 20, "symbol()")

    assert result == "WETH"


def test_pybot_io_fetch_erc20_string_field_propagates_reverts():
    """Provider revert surfaces as exception."""

    class _RevProv:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevProv())
    with pytest.raises(RuntimeError):
        io.fetch_erc20_string_field("0x" + "ab" * 20, "name()")


def test_pybot_io_fetch_erc20_uint_field_decodes_uint256():
    """fetch_erc20_uint_field(address, signature) encodes the selector dynamically,
    calls, and decodes uint256."""
    decimals = 6
    io = PyBotIo(
        provider=_StringFieldProvider(
            response=eth_abi.abi.encode(types=["uint256"], args=[decimals])
        )
    )

    result = io.fetch_erc20_uint_field("0x" + "ab" * 20, "decimals()")

    assert result == decimals


# === Pool type probing (slice 14i) ===
#
# `probe_pool_type` moves the 4-call try/catch probing choreography from
# `resolve_pool_type_by_probing` into Rust. Each probe is a no-arg call that
# either succeeds (method exists) or reverts (method doesn't exist). The Rust
# method returns a string tag identifying which probe succeeded:
# "slot0", "getReserves", "balancer_weighted", "balancer_stable", or
# "stableswap" (Curve fallback when nothing matched).


class _ProbeProvider:
    """Provider that succeeds for some selectors, reverts for others."""

    def __init__(self, *, succeed: set[bytes]) -> None:
        self._succeed = succeed

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        if data[:4] in self._succeed:
            return HexBytes(b"\x00" * 32)  # dummy non-empty response
        msg = "execution reverted"
        raise Web3Exception(msg)


def _sel(sig: str) -> bytes:
    return Web3.keccak(text=sig)[:4]


def test_pybot_io_probe_pool_type_returns_slot0_for_v3():
    """When slot0() succeeds, probe returns 'slot0'."""
    io = PyBotIo(provider=_ProbeProvider(succeed={_sel("slot0()")}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "slot0"


def test_pybot_io_probe_pool_type_returns_getreserves_for_v2():
    """When slot0() reverts but getReserves() succeeds, probe returns 'getReserves'."""
    io = PyBotIo(provider=_ProbeProvider(succeed={_sel("getReserves()")}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "getReserves"


def test_pybot_io_probe_pool_type_returns_balancer_weighted():
    """When getPoolId() + getNormalizedWeights() succeed, probe returns 'balancer_weighted'."""
    io = PyBotIo(
        provider=_ProbeProvider(succeed={_sel("getPoolId()"), _sel("getNormalizedWeights()")})
    )
    assert io.probe_pool_type("0x" + "aa" * 20) == "balancer_weighted"


def test_pybot_io_probe_pool_type_returns_balancer_stable():
    """When getPoolId() succeeds but getNormalizedWeights() reverts, probe returns 'balancer_stable'."""
    io = PyBotIo(provider=_ProbeProvider(succeed={_sel("getPoolId()")}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "balancer_stable"


def test_pybot_io_probe_pool_type_returns_stableswap_fallback():
    """When all probes revert, probe returns 'stableswap' (Curve fallback)."""
    io = PyBotIo(provider=_ProbeProvider(succeed=set()))
    assert io.probe_pool_type("0x" + "aa" * 20) == "stableswap"


# === V3 tick bitmap + tick data RPCs (slice 14j) ===
#
# `fetch_tick_bitmap` and `fetch_tick_data` move the two parameterized-call
# sub-choreographies from `_fetch_v3` (tick_data_fetcher.py) into Rust.
# New pattern: signed-integer argument encoding (int16 for word_position,
# int24 for tick). The bitmap iteration + dict-building stays Python-side.


class _TickDataProvider:
    """Provider returning canned tickBitmap + ticks responses."""

    def __init__(self, *, bitmap: int, liquidity_gross: int, liquidity_net: int) -> None:
        self._bitmap = bitmap
        self._lg = liquidity_gross
        self._ln = liquidity_net
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        # tickBitmap(int16) selector = 0x5339c296
        if sel == Web3.keccak(text="tickBitmap(int16)")[:4]:
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._bitmap]))
        # ticks(int24) selector = 0xf30dba93
        if sel == Web3.keccak(text="ticks(int24)")[:4]:
            return HexBytes(
                eth_abi.abi.encode(types=["uint128", "int128"], args=[self._lg, self._ln])
            )
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


def test_pybot_io_fetch_tick_bitmap_encodes_int16_arg_and_decodes_uint256():
    """fetch_tick_bitmap(pool, word_position) encodes tickBitmap(int16)
    with the word position as a sign-extended int16 argument, decodes uint256."""
    bitmap = 0xDEADBEEF
    word_position = -3  # negative int16 — sign extension must work
    io = PyBotIo(provider=_TickDataProvider(bitmap=bitmap, liquidity_gross=0, liquidity_net=0))

    result = io.fetch_tick_bitmap("0x" + "aa" * 20, word_position)

    assert result == bitmap
    # Verify the int16 arg is sign-extended in the 32-byte word.
    calldata = io.provider.calls[0]
    assert calldata[:4] == Web3.keccak(text="tickBitmap(int16)")[:4]
    # For -3, the 32-byte word should be all 0xFF except the last byte = 0xFD.
    assert calldata[4:36] == (b"\xff" * 31) + bytes([0xFD])


def test_pybot_io_fetch_tick_data_encodes_int24_arg_and_decodes_uint128_int128():
    """fetch_tick_data(pool, tick) encodes ticks(int24) with the tick as a
    sign-extended int24 argument, decodes (uint128, int128) — the first two
    fields of the tick struct (liquidity_gross, liquidity_net)."""
    lg = 1_000_000_000_000
    ln = -500_000_000_000  # negative liquidity_net
    tick = -887_220  # real mainnet negative tick
    io = PyBotIo(provider=_TickDataProvider(bitmap=0, liquidity_gross=lg, liquidity_net=ln))

    result = io.fetch_tick_data("0x" + "aa" * 20, tick)

    assert result == (lg, ln)
    # Verify the int24 arg is sign-extended.
    calldata = io.provider.calls[0]
    assert calldata[:4] == Web3.keccak(text="ticks(int24)")[:4]


def test_pybot_io_fetch_tick_bitmap_returns_zero_on_revert():
    """When tickBitmap reverts, the Python caller silently returns (no bitmap
    entry). The Rust method should propagate the exception so the Python
    caller's except clause handles it."""

    class _RevProv:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevProv())
    with pytest.raises(RuntimeError):
        io.fetch_tick_bitmap("0x" + "aa" * 20, 0)


# === V4 tick bitmap + tick data RPCs (slice 14k) ===
#
# `fetch_v4_tick_bitmap` and `fetch_v4_tick_data` mirror 14j's V3 methods but
# add a `bytes32 pool_id` prefix argument (V4 uses `getTickBitmap(bytes32,
# int16)` and `getTickLiquidity(bytes32,int24)` on a state-view contract).


class _V4TickDataProvider:
    """Provider returning canned getTickBitmap + getTickLiquidity responses."""

    def __init__(self, *, bitmap: int, liquidity_gross: int, liquidity_net: int) -> None:
        self._bitmap = bitmap
        self._lg = liquidity_gross
        self._ln = liquidity_net
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel == Web3.keccak(text="getTickBitmap(bytes32,int16)")[:4]:
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._bitmap]))
        if sel == Web3.keccak(text="getTickLiquidity(bytes32,int24)")[:4]:
            return HexBytes(
                eth_abi.abi.encode(types=["uint128", "int128"], args=[self._lg, self._ln])
            )
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


def test_pybot_io_fetch_v4_tick_bitmap_encodes_pool_id_and_int16_args():
    """fetch_v4_tick_bitmap(state_view, pool_id, word_position) encodes
    getTickBitmap(bytes32,int16) with pool_id in word 0 and sign-extended
    int16 in word 1."""
    bitmap = 0xCAFE
    pool_id = bytes.fromhex("db" * 32)
    word_position = -1
    io = PyBotIo(provider=_V4TickDataProvider(bitmap=bitmap, liquidity_gross=0, liquidity_net=0))

    result = io.fetch_v4_tick_bitmap("0x" + "cc" * 20, pool_id, word_position)

    assert result == bitmap
    calldata = io.provider.calls[0]
    assert calldata[:4] == Web3.keccak(text="getTickBitmap(bytes32,int16)")[:4]
    # pool_id is bytes 4..36 (already 32 bytes, used as-is).
    assert calldata[4:36] == pool_id
    # word_position -1 sign-extended in word 1 (bytes 36..68).
    assert calldata[36:68] == (b"\xff" * 32)


def test_pybot_io_fetch_v4_tick_data_encodes_pool_id_and_int24_args():
    """fetch_v4_tick_data(state_view, pool_id, tick) encodes
    getTickLiquidity(bytes32,int24) with pool_id in word 0 and sign-extended
    int24 in word 1, decodes (uint128, int128)."""
    lg = 42_000_000_000_000
    ln = -17_000_000_000_000
    pool_id = bytes.fromhex("db" * 32)
    tick = -100
    io = PyBotIo(provider=_V4TickDataProvider(bitmap=0, liquidity_gross=lg, liquidity_net=ln))

    result = io.fetch_v4_tick_data("0x" + "cc" * 20, pool_id, tick)

    assert result == (lg, ln)
    calldata = io.provider.calls[0]
    assert calldata[:4] == Web3.keccak(text="getTickLiquidity(bytes32,int24)")[:4]
    assert calldata[4:36] == pool_id
    assert calldata[36:68] == (b"\xff" * 31) + b"\x9c"  # -100 sign-extended


# === Balancer RPCs (slice 14l) ===
#
# Five Balancer pool-builder helpers move into PyBotIo: `_fetch_pool_id`
# (bytes32 decode), `_fetch_swap_fee` (uint256), `_fetch_amp` (multi-word
# return, take first), `_fetch_weights` (`uint256[]` dynamic array — new
# decode pattern), `_fetch_rate_providers` (`address[]` dynamic array with
# fallback to [] on revert).


class _BalancerProvider:
    """Provider returning canned Balancer responses based on selector."""

    def __init__(self, **responses: bytes) -> None:
        # responses keyed by the 4-byte selector
        self._responses = {Web3.keccak(text=sig)[:4]: data for sig, data in responses.items()}
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel in self._responses:
            return HexBytes(self._responses[sel])
        msg = f"unexpected selector {sel.hex()}"
        raise Web3Exception(msg)


def test_pybot_io_fetch_balancer_pool_id_decodes_bytes32():
    """fetch_balancer_pool_id(address, block) → getPoolId() → 32 raw bytes."""
    pool_id = bytes(range(32))
    io = PyBotIo(provider=_BalancerProvider(**{"getPoolId()": pool_id}))

    result = io.fetch_balancer_pool_id("0x" + "aa" * 20)

    assert bytes(result) == pool_id
    # The return should be the raw 32 bytes (bytes32).
    assert isinstance(result, (bytes, bytearray))


def test_pybot_io_fetch_balancer_swap_fee_decodes_uint256():
    """fetch_balancer_swap_fee(address, block) → getSwapFeePercentage() → uint256."""
    fee = 3 * 10**16  # 3%
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getSwapFeePercentage()": eth_abi.abi.encode(types=["uint256"], args=[fee])
        })
    )

    result = io.fetch_balancer_swap_fee("0x" + "aa" * 20)

    assert result == fee


def test_pybot_io_fetch_balancer_amp_takes_first_word_of_multi():
    """fetch_balancer_amp(address, block) → getAmplificationParameter() →
    multi-word (uint256, bool, uint256) return; we take the first uint256."""
    amp_value = 2_000
    # The full struct is (uint256 value, bool isUpdating, uint256 precision)
    # — we only care about the first word.
    payload = eth_abi.abi.encode(
        types=["uint256", "bool", "uint256"], args=[amp_value, False, 1000]
    )
    io = PyBotIo(provider=_BalancerProvider(**{"getAmplificationParameter()": payload}))

    result = io.fetch_balancer_amp("0x" + "aa" * 20)

    assert result == amp_value


def test_pybot_io_fetch_balancer_weights_decodes_uint256_array():
    """fetch_balancer_weights(address, block) → getNormalizedWeights() →
    dynamic uint256[] array. New decode pattern: skip offset word, read
    length, then N 32-byte words."""
    weights = [5 * 10**17, 5 * 10**17]  # 50/50
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getNormalizedWeights()": eth_abi.abi.encode(types=["uint256[]"], args=[weights])
        })
    )

    result = io.fetch_balancer_weights("0x" + "aa" * 20)

    assert list(result) == weights


def test_pybot_io_fetch_balancer_weights_empty_array():
    """Empty uint256[] returns offset=0x20, length=0, no data words."""
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getNormalizedWeights()": eth_abi.abi.encode(types=["uint256[]"], args=[[]])
        })
    )

    result = io.fetch_balancer_weights("0x" + "aa" * 20)

    assert list(result) == []


def test_pybot_io_fetch_balancer_rate_providers_decodes_address_array():
    """fetch_balancer_rate_providers(address, block) → getRateProviders() →
    dynamic address[] array. Each address is 20 bytes, left-padded with 12
    zero bytes in its 32-byte word."""
    addrs = [
        "0x" + "11" * 20,
        "0x" + "22" * 20,
        "0x" + "33" * 20,
    ]
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getRateProviders()": eth_abi.abi.encode(types=["address[]"], args=[addrs])
        })
    )

    result = io.fetch_balancer_rate_providers("0x" + "aa" * 20)

    assert [a.lower() for a in result] == [a.lower() for a in addrs]


def test_pybot_io_fetch_balancer_rate_providers_returns_empty_on_revert():
    """When getRateProviders reverts (WeightedPool2Tokens/MetaStablePools),
    the Python caller returns []. The Rust method should propagate the
    exception so the caller's try/except handles it (sync path). For the
    PyBotIo path, propagate so the Python helper's except catches it."""

    class _RevProv:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "execution reverted"
            raise Web3Exception(msg)

    io = PyBotIo(provider=_RevProv())
    with pytest.raises(Web3Exception):
        io.fetch_balancer_rate_providers("0x" + "aa" * 20)


# === Balancer vault tokens RPC (slice 14m) ===
#
# `fetch_balancer_vault_tokens` moves `_fetch_vault_tokens` into Rust. The
# hard part: `getPoolTokens(bytes32)` returns an ABI-encoded tuple of shape
# `(address[], uint256[], uint256)` — a tuple with TWO nested dynamic arrays.
# Only the first two members (tokens + balances) are returned.


def test_pybot_io_fetch_balancer_vault_tokens_decodes_two_dynamic_arrays():
    """fetch_balancer_vault_tokens(vault, pool_id, block) →
    getPoolTokens(bytes32) → decode (address[], uint256[], uint256) and
    return only the first two (tokens, balances)."""
    vault = "0x" + "ba" * 20
    pool_id = bytes(range(32))
    tokens = ["0x" + "11" * 20, "0x" + "22" * 20]
    balances = [10**18, 2 * 10**18]
    last_block = 17_000_000
    # Encode the full (address[], uint256[], uint256) tuple.
    payload = eth_abi.abi.encode(
        types=["address[]", "uint256[]", "uint256"],
        args=[tokens, balances, last_block],
    )
    io = PyBotIo(provider=_BalancerProvider(**{"getPoolTokens(bytes32)": payload}))

    result_tokens, result_balances = io.fetch_balancer_vault_tokens(vault, pool_id)

    assert [t.lower() for t in result_tokens] == [t.lower() for t in tokens]
    assert list(result_balances) == balances


def test_pybot_io_fetch_balancer_vault_tokens_encodes_pool_id_arg():
    """Verify the calldata passed to the provider: selector(4) + pool_id(32)."""
    vault = "0x" + "ba" * 20
    pool_id = bytes.fromhex("cd" * 32)
    payload = eth_abi.abi.encode(
        types=["address[]", "uint256[]", "uint256"],
        args=[[], [], 0],
    )
    io = PyBotIo(provider=_BalancerProvider(**{"getPoolTokens(bytes32)": payload}))

    io.fetch_balancer_vault_tokens(vault, pool_id)

    calldata = io.provider.calls[0]
    assert calldata[:4] == Web3.keccak(text="getPoolTokens(bytes32)")[:4]
    assert calldata[4:36] == pool_id


def test_pybot_io_fetch_balancer_vault_tokens_propagates_reverts():
    """When getPoolTokens reverts, the exception propagates."""

    class _RevProv:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "execution reverted"
            raise Web3Exception(msg)

    io = PyBotIo(provider=_RevProv())
    with pytest.raises(Web3Exception):
        io.fetch_balancer_vault_tokens("0x" + "ba" * 20, bytes(32))


# === Balancer rate + pool-type detect (slice 14n) ===
#
# `fetch_balancer_rate` moves the per-provider `getRate()` call out of
# `_fetch_rates`'s loop. `probe_balancer_pool_type` moves
# `_detect_pool_type`'s two-call probing: returns "weighted" if
# getNormalizedWeights() succeeds, "stable" if getAmplificationParameter()
# succeeds, or raises ValueError if neither works.


def test_pybot_io_fetch_balancer_rate_decodes_uint256():
    """fetch_balancer_rate(provider, block) → getRate() → uint256."""
    rate = 1_050_000_000_000_000_000  # 1.05e18
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getRate()": eth_abi.abi.encode(types=["uint256"], args=[rate])
        })
    )

    result = io.fetch_balancer_rate("0x" + "ee" * 20)

    assert result == rate


def test_pybot_io_fetch_balancer_rate_propagates_reverts():
    """When getRate() reverts, the exception propagates."""
    io = PyBotIo(provider=_BalancerProvider())  # no responses → raise
    with pytest.raises(Web3Exception):
        io.fetch_balancer_rate("0x" + "ee" * 20)


def test_pybot_io_probe_balancer_pool_type_returns_weighted():
    """When getNormalizedWeights() succeeds, probe returns 'weighted'."""
    weights = [5 * 10**17, 5 * 10**17]
    io = PyBotIo(
        provider=_BalancerProvider(**{
            "getNormalizedWeights()": eth_abi.abi.encode(types=["uint256[]"], args=[weights])
        })
    )

    assert io.probe_balancer_pool_type("0x" + "aa" * 20) == "weighted"


def test_pybot_io_probe_balancer_pool_type_returns_stable():
    """When getNormalizedWeights() reverts but getAmplificationParameter()
    succeeds, probe returns 'stable'."""
    amp_payload = eth_abi.abi.encode(
        types=["uint256", "bool", "uint256"], args=[2_000, False, 1000]
    )
    io = PyBotIo(provider=_BalancerProvider(**{"getAmplificationParameter()": amp_payload}))

    assert io.probe_balancer_pool_type("0x" + "aa" * 20) == "stable"


def test_pybot_io_probe_balancer_pool_type_raises_when_neither_works():
    """When both probes revert, raise ValueError (surfaces as Python error)."""
    io = PyBotIo(provider=_BalancerProvider())  # no responses
    with pytest.raises(ValueError):
        io.probe_balancer_pool_type("0x" + "aa" * 20)


# === V4 slot0 + liquidity RPCs (slice 14o) ===
#
# `fetch_v4_slot0_liquidity` mirrors `fetch_v3_slot0_liquidity` (14f) but for
# V4 pools. V4 queries `getSlot0(bytes32)` + `getLiquidity(bytes32)` on a
# state-view contract, passing `pool_id` as a bytes32 arg.


class _V4Slot0Provider:
    """Provider returning canned getSlot0 + getLiquidity responses."""

    def __init__(self, *, slot0_bytes: bytes, liquidity: int) -> None:
        self._slot0 = slot0_bytes
        self._liq = liquidity
        self.calls: list[bytes] = []

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel == Web3.keccak(text="getSlot0(bytes32)")[:4]:
            return HexBytes(self._slot0)
        if sel == Web3.keccak(text="getLiquidity(bytes32)")[:4]:
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._liq]))
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


def test_pybot_io_fetch_v4_slot0_liquidity_decodes_all_fields():
    """fetch_v4_slot0_liquidity(state_view, pool_id, block) →
    getSlot0(bytes32) + getLiquidity(bytes32) → returns all 4 slot0 fields +
    liquidity as a 5-tuple."""
    sqrt_price = 1 << 96  # price 1.0
    tick = -887_220
    protocol_fee = 0
    lp_fee = 3_000
    liquidity = 10**18
    slot0_bytes = eth_abi.abi.encode(
        types=["uint160", "int24", "uint24", "uint24"],
        args=[sqrt_price, tick, protocol_fee, lp_fee],
    )
    pool_id = bytes(range(32))
    io = PyBotIo(provider=_V4Slot0Provider(slot0_bytes=slot0_bytes, liquidity=liquidity))

    result = io.fetch_v4_slot0_liquidity("0x" + "cc" * 20, pool_id)

    assert len(result) == 5
    assert int(result[0]) == sqrt_price
    assert int(result[1]) == tick
    assert int(result[2]) == protocol_fee
    assert int(result[3]) == lp_fee
    assert int(result[4]) == liquidity


def test_pybot_io_fetch_v4_slot0_liquidity_encodes_pool_id_arg():
    """Verify both RPCs encode the pool_id arg correctly (selector+bytes32)."""
    slot0_bytes = eth_abi.abi.encode(
        types=["uint160", "int24", "uint24", "uint24"],
        args=[1 << 96, 0, 0, 0],
    )
    pool_id = bytes.fromhex("ab" * 32)
    io = PyBotIo(provider=_V4Slot0Provider(slot0_bytes=slot0_bytes, liquidity=0))

    io.fetch_v4_slot0_liquidity("0x" + "cc" * 20, pool_id)

    assert len(io.provider.calls) == 2
    for calldata in io.provider.calls:
        assert calldata[4:36] == pool_id


def test_pybot_io_fetch_v4_slot0_liquidity_propagates_reverts():
    """When getSlot0 reverts, the exception propagates."""

    class _RevProv:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "execution reverted"
            raise Web3Exception(msg)

    io = PyBotIo(provider=_RevProv())
    with pytest.raises(Web3Exception):
        io.fetch_v4_slot0_liquidity("0x" + "cc" * 20, bytes(32))
