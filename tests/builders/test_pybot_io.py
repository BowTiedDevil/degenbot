"""Tracer-bullet tests for RustBotIo (ADR-005 slice 14a).

`RustBotIo` is the Rust `#[pyclass]` I/O façade that builders receive in place
of the Python `SyncPoolIO` adapter. It holds a Python provider (the
`AlloyProvider` the `Bot` was constructed with) + an optional DB handle, and
exposes the 3-method RPC-primitive surface still on `RustBotIo`
(`get_block_number`, `get_code`, `get_balance`) by delegating to the held
provider (the raw `call`/`get_block`/`get_block_timestamp` primitives retired
with LWKLMP-S5).

These tests pin the *seam* -- that delegating through the Rust pyclass yields
the same observable result as calling the provider directly. They do NOT yet
route a real builder through `RustBotIo`; that's the 14a follow-on (one builder's
`build()` via `RustBotIo`), and 14b extends it to all families.
"""

from __future__ import annotations

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.bot import RustBotIo
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import function_selector

# A minimal real offline provider (recorded JSON, no RPC) for tests that need a
# valid `RustBotIo` provider but don't exercise specific RPC responses (DB handle
# round-trip, PoolIO protocol surface). A real `PyAlloyProvider`-backed
# provider keeps the seam honest — no Python fake double (O3).
_MIN_OFFLINE_JSON = '{"chain_id":1,"block_number":100,"timestamp":1700000000,"calls":{},"code":{}}'


def _min_offline_provider() -> RustAlloyProvider:
    """A one-block `OfflineProvider`-backed `AlloyProvider` (no RPC)."""
    return RustAlloyProvider.offline_from_json_string(_MIN_OFFLINE_JSON)


class _FakeDb:
    """A ``DatabaseSessionManager``-shaped double (cannot be called; presence only)."""


# The 7-method `PoolIO` delegation seam is exercised natively against a
# recorded `OfflineProvider` in the "Native alloy path (B1)" tests below — the
# former `_FakeProvider`-based delegation tests (get_block_number / call /
# get_code / get_balance returning arbitrary canned values) are collapsed
# there, per O3.


def test_pybot_io_holds_optional_db_handle():
    """RustBotIo stores the DB handle and exposes it back (held, not called yet)."""
    db = _FakeDb()
    io = RustBotIo(provider=_min_offline_provider(), db=db)
    # The held handle round-trips through the pyclass.
    assert io.db is db


@pytest.mark.parametrize(
    "method",
    [
        "get_block_number",
        "get_code",
        "get_balance",
    ],
)
def test_pybot_io_satisfies_pool_io_protocol(method: str):
    """RustBotIo exposes the surviving 3-method PoolIO surface (runtime check).

    The raw `call`/`call_raw`/`get_block`/`get_block_timestamp` primitives were
    retired with LWKLMP-S5 (no live `src/` caller); the remaining primitives
    are checked here.
    """
    io = RustBotIo(provider=_min_offline_provider())
    assert hasattr(io, method), f"RustBotIo missing PoolIO method {method!r}"


# === I/O choreography methods (slice 14b) ===
#
# `fetch_factory_address` is the first choreography method moved into `RustBotIo`:
# the multi-step (encode `factory()` selector -> `eth_call` -> ABI-decode `address`
# -> EIP-55 checksum), previously `fetch_factory_from_chain` in
# `type_resolution.py`, now reachable as a single Rust-owned method. The RPC
# primitive (`call`) still delegates to the held provider (the native-alloy
# swap is a later slice); the *choreography* -- the orchestration of those 4
# steps -- now lives in Rust, satisfying slice 14's "methods for the builder
# I/O choreography … moved here, called from Python via RustBotIo".


class _FactoryCallProvider:
    """Provider double that returns an ABI-encoded factory address for `factory()`.

    Mirrors ``AlloyProvider.call(*, to, data, block)`` (kw-only) so it stays
    compatible with ``RustBotIo``'s kw-only forward contract.
    """

    def __init__(self, factory_raw: str) -> None:
        # factory_raw is the 40-hex-char lowercase address (no 0x prefix),
        # ABI-encoded right-aligned in a 32-byte word -- what a real
        # `factory()` call returns.
        self._encoded = eth_abi.abi.encode(types=["address"], args=[factory_raw])
        self.calls: list[tuple[str, bytes]] = []  # (to, data)

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data))
        return HexBytes(self._encoded)


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

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


class _AddressArgProvider:
    """Provider double: encodes matching call-data -> canned ABI uint256 result."""

    def __init__(self, *, response: bytes) -> None:
        self._response = response
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._response)


def _uint256_encoded(value: int) -> bytes:
    return eth_abi.abi.encode(types=["uint256"], args=[value])


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
                types=["uint112", "uint112", "uint32"], args=[reserves0, reserves1, 0]
            ),
        }
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


def _eip55(addr: str) -> str:

    return get_checksum_address(addr)


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

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        return HexBytes(self._responses[data[:4]])


class _AerodromeProvider:
    """Provider double for Aerodrome stable() + getFee(address,bool)."""

    def __init__(self, *, stable: bool, fee_raw: int) -> None:
        self._stable = stable
        self._fee_raw = fee_raw
        self.calls: list[tuple[str, bytes]] = []  # (to, data) audit trail

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data))
        sel = data[:4]
        # keccak256("stable()")[..4]
        _stable_sel = function_selector("stable()")
        if sel == _stable_sel:
            return HexBytes(eth_abi.abi.encode(types=["bool"], args=[self._stable]))
        # keccak256("getFee(address,bool)")[..4]
        _get_fee_sel = function_selector("getFee(address,bool)")
        if sel == _get_fee_sel:
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._fee_raw]))
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _StringFieldProvider:
    """Provider returning either ABI string or bytes32 for a given selector."""

    def __init__(self, response: bytes) -> None:
        self._response = response

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return HexBytes(self._response)


def _probe_offline_provider(succeed: set[str]) -> RustAlloyProvider:
    """An `OfflineProvider` cassette answering only the given selector hexes.

    Calls not recorded surface as `RpcError` (non-revert), which the probe's
    fire-and-forget dispatch treats as "reverted" → tries the next probe. No
    Python double (O3).
    """
    import json

    pool_addr = "aa" * 20
    calls = {f"0x{pool_addr}:0x{sel}": "00" * 32 for sel in succeed}
    return RustAlloyProvider.offline_from_json_string(
        json.dumps({
            "chain_id": 1,
            "block_number": 100,
            "timestamp": 1_700_000_000,
            "calls": calls,
            "code": {},
        })
    )


_SLOT0 = function_selector("slot0()").hex()
_GET_RESERVES = function_selector("getReserves()").hex()
_GET_POOL_ID = function_selector("getPoolId()").hex()
_GET_NORMALIZED_WEIGHTS = function_selector("getNormalizedWeights()").hex()


def test_pybot_io_probe_pool_type_returns_slot0_for_v3():
    """When slot0() succeeds, probe returns 'slot0'."""
    io = RustBotIo(provider=_probe_offline_provider({_SLOT0}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "slot0"


def test_pybot_io_probe_pool_type_returns_getreserves_for_v2():
    """When slot0() reverts but getReserves() succeeds, probe returns 'getReserves'."""
    io = RustBotIo(provider=_probe_offline_provider({_GET_RESERVES}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "getReserves"


def test_pybot_io_probe_pool_type_returns_balancer_weighted():
    """When getPoolId() + getNormalizedWeights() succeed, probe returns 'balancer_weighted'."""
    io = RustBotIo(provider=_probe_offline_provider({_GET_POOL_ID, _GET_NORMALIZED_WEIGHTS}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "balancer_weighted"


def test_pybot_io_probe_pool_type_returns_balancer_stable():
    """When getPoolId() succeeds but getNormalizedWeights() reverts, probe returns 'balancer_stable'."""
    io = RustBotIo(provider=_probe_offline_provider({_GET_POOL_ID}))
    assert io.probe_pool_type("0x" + "aa" * 20) == "balancer_stable"


def test_pybot_io_probe_pool_type_returns_stableswap_fallback():
    """When all probes revert, probe returns 'stableswap' (Curve fallback)."""
    io = RustBotIo(provider=_probe_offline_provider(set()))
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

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        # tickBitmap(int16) selector = 0x5339c296
        if sel == function_selector("tickBitmap(int16)"):
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._bitmap]))
        # ticks(int24) selector = 0xf30dba93
        if sel == function_selector("ticks(int24)"):
            return HexBytes(
                eth_abi.abi.encode(types=["uint128", "int128"], args=[self._lg, self._ln])
            )
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _V4TickDataProvider:
    """Provider returning canned getTickBitmap + getTickLiquidity responses."""

    def __init__(self, *, bitmap: int, liquidity_gross: int, liquidity_net: int) -> None:
        self._bitmap = bitmap
        self._lg = liquidity_gross
        self._ln = liquidity_net
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel == function_selector("getTickBitmap(bytes32,int16)"):
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._bitmap]))
        if sel == function_selector("getTickLiquidity(bytes32,int24)"):
            return HexBytes(
                eth_abi.abi.encode(types=["uint128", "int128"], args=[self._lg, self._ln])
            )
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _BalancerOfflineProbeProvider:
    """An `OfflineProvider` cassette answering the two Balancer sub-type probes.

    `WEIGHTS_SEL` (`getNormalizedWeights()`) and `AMP_SEL`
    (`getAmplificationParameter()`) map to either an ABI-encoded result hex or
    `None` (revert), mirroring the real builder flow against the Rust
    transport (no Python double, O3).
    """

    WEIGHTS_SEL = "0xf89f27ed"  # getNormalizedWeights()
    AMP_SEL = "0x6daccffa"  # getAmplificationParameter()

    @classmethod
    def build(cls, pool_addr: str, weights_success, amp_success) -> RustAlloyProvider:
        import json as _json

        calls = {f"0x{pool_addr}:{cls.WEIGHTS_SEL}": weights_success}
        calls[f"0x{pool_addr}:{cls.AMP_SEL}"] = amp_success
        return RustAlloyProvider.offline_from_json_string(
            _json.dumps({
                "chain_id": 1,
                "block_number": 100,
                "timestamp": 1_700_000_000,
                "calls": calls,
                "code": {},
            })
        )


def test_pybot_io_probe_balancer_pool_type_returns_weighted():
    """When getNormalizedWeights() succeeds, probe returns 'weighted'."""
    pool_addr = "aa" * 20
    weights = [5 * 10**17, 5 * 10**17]
    encoded = eth_abi.abi.encode(types=["uint256[]"], args=[weights]).hex()
    provider = _BalancerOfflineProbeProvider.build(
        pool_addr, weights_success=encoded, amp_success=None
    )
    assert RustBotIo(provider=provider).probe_balancer_pool_type("0x" + pool_addr) == "weighted"


def test_pybot_io_probe_balancer_pool_type_returns_stable():
    """When getNormalizedWeights() reverts but getAmplificationParameter()
    succeeds, probe returns 'stable'."""
    pool_addr = "aa" * 20
    amp_payload = eth_abi.abi.encode(
        types=["uint256", "bool", "uint256"], args=[2_000, False, 1000]
    ).hex()
    provider = _BalancerOfflineProbeProvider.build(
        pool_addr, weights_success=None, amp_success=amp_payload
    )
    assert RustBotIo(provider=provider).probe_balancer_pool_type("0x" + pool_addr) == "stable"


def test_pybot_io_probe_balancer_pool_type_raises_when_neither_works():
    """When both probes revert, raise ValueError (surfaces as Python error)."""
    pool_addr = "aa" * 20
    provider = _BalancerOfflineProbeProvider.build(
        pool_addr, weights_success=None, amp_success=None
    )
    with pytest.raises(ValueError):
        RustBotIo(provider=provider).probe_balancer_pool_type("0x" + pool_addr)


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

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel == function_selector("getSlot0(bytes32)"):
            return HexBytes(self._slot0)
        if sel == function_selector("getLiquidity(bytes32)"):
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._liq]))
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _CamelotProvider:
    """Provider returning canned Camelot state responses by selector."""

    def __init__(self, *, stable: bool, fee_denom: int, fee0: int, fee1: int) -> None:
        t = [stable, fee_denom, fee0, fee1]
        sigs = ["stableSwap()", "FEE_DENOMINATOR()", "token0FeePercent()", "token1FeePercent()"]
        types = ["bool", "uint256", "uint16", "uint16"]
        self._responses = {
            function_selector(sig): eth_abi.abi.encode(types=[ty], args=[v])
            for sig, ty, v in zip(sigs, types, t, strict=True)
        }
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel in self._responses:
            return HexBytes(self._responses[sel])
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _CurveProvider:
    """Provider returning canned A/fee/admin_fee responses."""

    def __init__(self, *, a: int, fee: int, admin_fee: int) -> None:
        self._r = {
            function_selector("A()"): eth_abi.abi.encode(types=["uint256"], args=[a]),
            function_selector("fee()"): eth_abi.abi.encode(types=["uint256"], args=[fee]),
            function_selector("admin_fee()"): eth_abi.abi.encode(
                types=["uint256"], args=[admin_fee]
            ),
        }
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel in self._r:
            return HexBytes(self._r[sel])
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


class _CurveBalancesProvider:
    """Provider returning canned balances(uint256) responses."""

    def __init__(self, balances: list[int]) -> None:
        self._balances = balances
        self.calls: list[bytes] = []

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append(data)
        sel = data[:4]
        if sel == function_selector("balances(uint256)"):
            # uint256 arg is in word 0 (bytes 4..36). Decode it as the index.
            idx = int.from_bytes(data[4:36], "big")
            return HexBytes(eth_abi.abi.encode(types=["uint256"], args=[self._balances[idx]]))
        msg = f"unexpected selector {sel.hex()}"
        raise ValueError(msg)


def _recorded_factory_fixture() -> str:
    """Build a single-block recorded-JSON fixture with a `factory()` call."""
    import json

    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"  # 20-byte lowercase
    pool_addr = "ab" * 20
    # `factory()` selector = keccak256("factory()")[:4] = 0xc45a0155.
    # Recorded result = ABI-encoded address (32 bytes, right-aligned), no 0x.
    encoded = eth_abi.abi.encode(types=["address"], args=["0x" + factory_raw]).hex()
    calls = {f"0x{pool_addr}:0xc45a0155": encoded}
    code = {f"0x{pool_addr}": "60806040"}
    return json.dumps({
        "chain_id": 1,
        "block_number": 100,
        "timestamp": 1_700_000_000,
        "calls": calls,
        "code": code,
    })


def test_pybot_io_native_alloy_fetch_factory_address():
    """Native alloy path: `fetch_factory_address` against a recorded
    `OfflineProvider` (Rust transport) returns the EIP-55 checksum — no Python
    provider round-trip (the offline shell holds the `PyAlloyProvider`)."""
    from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider

    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"
    expected = "0x66f9664f97F2b50F62D13eA064982f936dE76657"
    pool_address = "0x" + "ab" * 20

    provider = RustAlloyProvider.offline_from_json_string(_recorded_factory_fixture())
    io = RustBotIo(provider=provider)

    assert io.fetch_factory_address(pool_address) == expected


def test_pybot_io_native_alloy_poolio_surface():
    """Native alloy path: the `PoolIO` surface (`get_block_number`, `get_code`)
    runs against the Rust offline transport and returns the expected shapes."""
    from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider

    provider = RustAlloyProvider.offline_from_json_string(_recorded_factory_fixture())
    io = RustBotIo(provider=provider)

    assert io.get_block_number() == 100

    pool_address = "0x" + "ab" * 20
    code = io.get_code(pool_address)
    assert code == HexBytes(bytes.fromhex("60806040"))


def test_pybot_io_native_alloy_revert_surfaces_contract_logic_error():
    """Native alloy path: a recorded revert (`null` result) surfaces as
    `ContractLogicError` (the alloy revert path), not a generic RuntimeError —
    driven through a live `fetch_token_balance` whose `balanceOf` call reverts."""
    import json

    from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
    from degenbot.exceptions import ContractLogicError

    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"
    token_addr = "ab" * 20
    encoded = eth_abi.abi.encode(types=["address"], args=["0x" + factory_raw]).hex()
    # balanceOf(0xff..ff): the offline provider matches by full calldata, so
    # the reverting call is keyed by the raw `balanceOf` selector + 32-byte owner.
    reverting_balance_of = "0x70a08231" + "0" * 24 + "ff" * 20
    data = {
        "chain_id": 1,
        "block_number": 100,
        "timestamp": 1_700_000_000,
        # balanceOf(address) reverted (null); factory() still succeeds.
        "calls": {
            f"0x{token_addr}:{reverting_balance_of}": None,
            f"0x{token_addr}:0xc45a0155": encoded,
        },
        "code": {f"0x{token_addr}": "60806040"},
    }
    provider = RustAlloyProvider.offline_from_json_string(json.dumps(data))
    io = RustBotIo(provider=provider)
    token = "0x" + token_addr

    with pytest.raises(ContractLogicError):
        io.fetch_token_balance(token=token, owner="0x" + "ff" * 20)
    # The factory() call still succeeds (not the reverted selector).
    assert io.fetch_factory_address(token) == "0x66f9664f97F2b50F62D13eA064982f936dE76657"
