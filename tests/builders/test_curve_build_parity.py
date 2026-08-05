"""WKKMJM step-1: recorded-RPC parity harness for the Rust Curve builder.

Drives the Rust ``PyBot.build_curve_pool`` (the FFI adapter over core
``builder::build_curve_pool`` + ``RpcCurveDataProvider``) through a
byte-accurate ``OfflineProvider`` cassette (recorded ``eth_call`` reads, no
network), then asserts the registered pool params against the same pinned
values the Rust test ``build_curve_pool_assembles_plain_pool_params``
(pool_builder/tests.rs) checks byte-for-byte.

This is the Python twin of that Rust `ConstructionIo` double. It validates the
full seam no I/O-free test covers: `PyBot` → `attach_construction_io`
(AlloyRpcConstruction) → core detection choreography (coin discovery, A/fee/
admin_fee, per-coin decimals, ramping/crypto/lending/lp/metapool probes) →
`register_curve_pool` → `struct PyLiquidityPool` handle.

Cassette semantics that differ from the Rust `FakeRpc` (selector stubs): the
`OfflineProvider` keys `eth_call` by full `(to, calldata)` and treats an
*unrecorded* call as a JSON-RPC error. A plain Curve pool, driven with an empty
registry list, only requires the reads below; every other detection probe
(ramping/crypto/lending/lp/metapool) reverts (unrecorded) and is tolerated. See
`curve_choreography.rs` for the read sequence this mirrors.
"""

from __future__ import annotations

from eth_utils import keccak

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.bot import PyBot

# 20-byte (40 hex) addresses matching the Rust plain-pool test.
POOL = "0x" + "a1" + "00" * 19
COIN0 = "0x" + "ac" + "00" * 18 + "01"
COIN1 = "0x" + "ac" + "00" * 18 + "02"
ZERO = "0x" + "00" * 20

_BLOCK = 100
_TIMESTAMP = 1_700_000_000


def _selector(signature: str) -> str:
    """4-byte function selector (lowercase hex, no ``0x``)."""
    return "0x" + keccak(text=signature)[:4].hex()


def _call_key(to: str, signature: str, arg: int | None = None) -> str:
    """The ``OfflineProvider`` ``(to, calldata)`` ``eth_call`` lookup key."""
    calldata = _selector(signature)[2:]
    if arg is not None:
        calldata += f"{arg:064x}"
    return f"{to.lower()}:0x{calldata}"


def _word_address(address: str) -> str:
    """A 20-byte address left-padded to a 32-byte ABI ``address`` word."""
    return "000000000000000000000000" + address.lower()[2:]


def _word_uint(value: int) -> str:
    """A 32-byte ABI ``uint256`` word."""
    return f"{value:064x}"


# Metapool: a 2-coin pool whose second coin is the 3Crv LP token, sitting on
# the canonical tripool base. Driven with one registry to exercise the
# base-pool + underlying-coins + lp-token detection.
META = "0x" + "b2" + "00" * 19
REGISTRY = "0x" + "cc" + "00" * 19
THREE_CRV_LP = "0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490"
TRIPOOL = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
META_COIN0 = "0x" + "ac" + "00" * 18 + "11"
DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
USDT = "0xdAC17F958D2ee523a2206206994597C13D831ec7"


def _word_addresses(addresses: list[str]) -> str:
    """A packed 8 x 32-byte ``address[8]`` return word (zero-stopped)."""
    words = ["00" * 32] * 8
    for i, addr in enumerate(addresses):
        words[i] = _word_address(addr)
    return "".join(words)


def _call_key_word_arg(to: str, signature: str, arg: str) -> str:
    """An ``OfflineProvider`` lookup key with a 20-byte ``address`` argument."""
    calldata = _selector(signature)[2:] + _word_address(arg)
    return f"{to.lower()}:0x{calldata}"


def _metapool_cassette_json() -> str:
    """Recorded reads for a 3Crv-underlying metapool (tripool base)."""
    import json

    calls = {
        # Coin/balance enumeration stops after the LP-token coin.
        _call_key(META, "coins(uint256)", 0): _word_address(META_COIN0),
        _call_key(META, "coins(uint256)", 1): _word_address(THREE_CRV_LP),
        _call_key(META, "coins(uint256)", 2): _word_address(ZERO),
        _call_key(META, "balances(uint256)", 0): _word_uint(500_000_000_000_000),
        _call_key(META, "balances(uint256)", 1): _word_uint(1_000_000_000_000_000),
        _call_key(META, "A()"): _word_uint(500),
        _call_key(META, "fee()"): _word_uint(1_000_000),
        _call_key(META, "admin_fee()"): _word_uint(500_000_000),
        _call_key(META_COIN0, "decimals()"): _word_uint(6),
        _call_key(THREE_CRV_LP, "decimals()"): _word_uint(18),
        # Metapool: is_meta → true; base pool via the pool's own base_pool();
        # underlying coins; LP token from the registry.
        _call_key_word_arg(REGISTRY, "is_meta(address)", META): _word_uint(1),
        _call_key(META, "base_pool()"): _word_address(TRIPOOL),
        _call_key_word_arg(REGISTRY, "get_underlying_coins(address)", META): _word_addresses([
            META_COIN0,
            DAI,
            USDC,
            USDT,
        ]),
        _call_key_word_arg(REGISTRY, "get_lp_token(address)", META): _word_address(META),
    }
    return json.dumps({
        "chain_id": 1,
        "block_number": _BLOCK,
        "timestamp": _TIMESTAMP,
        "calls": calls,
        "code": {},
    })


# Lending: coin0 is a cToken (isCToken → true, underlying() 6-dec), coin1 a
# plain 18-dec token. The precision multiplier for the cToken coin comes from
# the UNDERLYING decimals, not the wrapped token's.
LPOOL = "0x" + "c1" + "00" * 19
CTOKEN0 = "0x" + "dd" + "00" * 18 + "01"
UNDERLYING0 = "0x" + "ee" + "00" * 18 + "01"
LP_COIN1 = "0x" + "ac" + "00" * 18 + "02"  # 18-dec plain coin


def _lending_cassette_json() -> str:
    """Recorded reads for a 2-coin pool whose coin0 is a 6-dec-underlying cToken."""
    import json

    calls = {
        _call_key(LPOOL, "coins(uint256)", 0): _word_address(CTOKEN0),
        _call_key(LPOOL, "coins(uint256)", 1): _word_address(LP_COIN1),
        _call_key(LPOOL, "coins(uint256)", 2): _word_address(ZERO),
        _call_key(LPOOL, "balances(uint256)", 0): _word_uint(1_000_000),
        _call_key(LPOOL, "balances(uint256)", 1): _word_uint(2_000_000_000_000_000_000),
        _call_key(LPOOL, "A()"): _word_uint(100),
        _call_key(LPOOL, "fee()"): _word_uint(1_000_000),
        _call_key(LPOOL, "admin_fee()"): _word_uint(500_000_000),
        _call_key(CTOKEN0, "decimals()"): _word_uint(8),  # wrapped token decimals
        _call_key(LP_COIN1, "decimals()"): _word_uint(18),
        # cToken detection on coin0 → positive; coin1 reverts (not lending).
        _call_key(CTOKEN0, "isCToken()"): _word_uint(1),
        _call_key(CTOKEN0, "underlying()"): _word_address(UNDERLYING0),
        _call_key(UNDERLYING0, "decimals()"): _word_uint(6),  # underlier decimals
    }
    return json.dumps({
        "chain_id": 1,
        "block_number": _BLOCK,
        "timestamp": _TIMESTAMP,
        "calls": calls,
        "code": {},
    })


def _plain_cassette_json() -> str:
    """Recorded reads for a plain 2-coin Curve stableswap pool.

    Mirrors `build_curve_pool_assembles_plain_pool_params`: 2 coins with 6
    decimals each, `A = 2000`, `fee = 1e6`, `admin_fee = 5e8`. Triggered with
    an empty registry list, the pool needs exactly these reads; unrecorded
    detection probes revert and are tolerated.
    """
    import json

    calls = {
        _call_key(POOL, "coins(uint256)", 0): _word_address(COIN0),
        _call_key(POOL, "coins(uint256)", 1): _word_address(COIN1),
        _call_key(POOL, "coins(uint256)", 2): _word_address(ZERO),
        _call_key(POOL, "balances(uint256)", 0): _word_uint(1_000_000),
        _call_key(POOL, "balances(uint256)", 1): _word_uint(2_000_000),
        _call_key(POOL, "A()"): _word_uint(2000),
        _call_key(POOL, "fee()"): _word_uint(1_000_000),
        _call_key(POOL, "admin_fee()"): _word_uint(500_000_000),
        _call_key(COIN0, "decimals()"): _word_uint(6),
        _call_key(COIN1, "decimals()"): _word_uint(6),
    }
    return json.dumps({
        "chain_id": 1,
        "block_number": _BLOCK,
        "timestamp": _TIMESTAMP,
        "calls": calls,
        "code": {},
    })


def test_build_curve_pool_plain_over_offline_cassette() -> None:
    """Rust ``build_curve_pool`` over a recorded cassette pins the plain-pool params.

    Asserts the exact values from the Rust test that owns the same contract
    `build_curve_pool_assembles_plain_pool_params` — proving the FFI + alloy
    `ConstructionIo` + Rust detection + register + handle round-trip are
    byte-exact (rate 10^30, precision 10^12, STANDARD/NONE strategies,
    Rust `data_provider` attached).
    """
    provider = RustAlloyProvider.offline_from_json_string(_plain_cassette_json())
    py_bot = PyBot(chain_id=1)
    py_bot.attach_construction_io(provider, None)

    pool_id = py_bot.build_curve_pool(POOL, [], _BLOCK)
    assert pool_id == 1

    handle = py_bot.get_pool(pool_id)
    assert handle is not None
    assert handle.pool_family == "curve"

    assert handle.curve_a_coefficient == 2000
    assert handle.curve_fee == 1_000_000
    assert handle.curve_admin_fee == 500_000_000
    assert handle.balances == [1_000_000, 2_000_000]

    r30 = 10**30
    r12 = 10**12
    assert handle.curve_rate_multipliers == [r30, r30]
    assert handle.curve_precision_multipliers == [r12, r12]

    # Strategy defaults for a plain, unmapped pool.
    assert handle.curve_swap_style == 1  # STANDARD
    assert handle.curve_lending_rate_style == 1  # NONE
    assert handle.curve_d_variant == 1

    # The Rust `RpcCurveDataProvider` is attached to the handle.
    assert handle.curve_has_data_provider is True


def test_build_curve_pool_metapool_over_offline_cassette() -> None:
    """Rust builder recovers a metapool's base pool + underlying coins + LP token.

    A second-coin == 3Crv LP triggers the metapool branch: `is_meta` true,
    base pool resolved via the pool's own `base_pool()`, underlying `address[8]`
    decoded zero-stopped, and the LP token from the registry.
    """
    provider = RustAlloyProvider.offline_from_json_string(_metapool_cassette_json())
    py_bot = PyBot(chain_id=1)
    py_bot.attach_construction_io(provider, None)

    pool_id = py_bot.build_curve_pool(META, [REGISTRY], _BLOCK)
    assert pool_id == 1

    handle = py_bot.get_pool(pool_id)
    assert handle is not None
    assert handle.pool_family == "curve"
    assert handle.get_curve_tokens() is None  # ERC20 companions not registered here

    # Base pool + underlying coins + LP token recovered by Rust detection.
    assert handle.curve_base_pool_address() == TRIPOOL
    assert handle.curve_metapool_rate_style == 1  # default for unmapped address
    assert handle.curve_metapool_underlying_style == 1


def test_build_curve_pool_lending_ctoken_over_offline_cassette() -> None:
    """Rust builder applies the cToken underlying-decimals precision override.

    The cToken coin's precision/rate multipliers derive from the UNDERLYING
    token decimals (6 → 10^12 / 10^30), while the plain 18-dec coin gets the
    default 10^0 / 10^18 — and `use_lending` is [True, False].
    """
    provider = RustAlloyProvider.offline_from_json_string(_lending_cassette_json())
    py_bot = PyBot(chain_id=1)
    py_bot.attach_construction_io(provider, None)

    pool_id = py_bot.build_curve_pool(LPOOL, [], _BLOCK)
    assert pool_id == 1

    handle = py_bot.get_pool(pool_id)
    assert handle is not None
    assert handle.pool_family == "curve"

    # cToken coin0: from underlying 6 decimals → pm 10^12, rate 10^30.
    # plain coin1: 18 decimals → pm 10^0, rate 10^18.
    assert handle.curve_precision_multipliers == [10**12, 10**0]
    assert handle.curve_rate_multipliers == [10**30, 10**18]
    assert handle.curve_use_lending == [True, False]
