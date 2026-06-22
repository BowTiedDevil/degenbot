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
