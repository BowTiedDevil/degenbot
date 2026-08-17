"""WKKMJM step-1: recorded-RPC parity harness for the Rust Curve builder.

Drives the Rust ``Bot.build_curve_pool`` (the FFI adapter over core
``builder::build_curve_pool`` + ``RpcCurveDataProvider``) through a
byte-accurate ``OfflineProvider`` cassette (recorded ``eth_call`` reads, no
network), then asserts the registered pool params against the same pinned
values the Rust test ``build_curve_pool_assembles_plain_pool_params``
(pool_builder/tests.rs) checks byte-for-byte.

This is the Python twin of that Rust `ConstructionIo` double. It validates the
full seam no I/O-free test covers: `Bot` → `attach_construction_io`
(AlloyRpcConstruction) → core detection choreography (coin discovery, A/fee/
admin_fee, per-coin decimals, ramping/crypto/lending/lp/metapool probes) →
`register_curve_pool` → `struct LiquidityPool` handle.

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
from degenbot._ffi import Bot
from degenbot.builders.curve_pool_builder import CurvePoolBuilder

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


def _str_word(value: str) -> str:
    """A 32-byte ABI dynamic-``string`` return word (length head + data)."""
    raw = value.encode()
    data_len = ((len(raw) + 31) // 32) * 32
    return f"{len(raw):064x}" + raw.ljust(data_len, b"\x00").hex()


def _plain_full_cassette_json() -> str:
    """Full plain-pool cassette: every read, incl. token metadata + optional probes.

    Unlike the minimal `_plain_cassette_json` (empty registries, optional
    probes reverted), the full cassette records EVERY read the real
    `CurvePoolBuilder.build` issues (including ERC20 name/symbol and the
    ramping/crypto/lending/lp/metapool probes against the real registry
    addresses). The Python builder cannot tolerate an offline revert via
    `BotIo.call_raw` (it raises a non-catchable `RuntimeError`), so every
    optional probe must return an explicit "not present" value. The same
    cassette drives both the Rust `build_curve_pool` and the Python builder,
    so the two paths can be compared on identical recorded data.
    """
    import json

    from degenbot.builders.curve_pool_builder import _REGISTRY_ADDRESSES

    calls = {
        **{
            _call_key(POOL, "coins(uint256)", i): _word_address(coin)
            for i, coin in ((0, COIN0), (1, COIN1), (2, ZERO))
        },
        _call_key(POOL, "balances(uint256)", 0): _word_uint(1_000_000),
        _call_key(POOL, "balances(uint256)", 1): _word_uint(2_000_000),
        _call_key(POOL, "A()"): _word_uint(2000),
        _call_key(POOL, "fee()"): _word_uint(1_000_000),
        _call_key(POOL, "admin_fee()"): _word_uint(500_000_000),
        # ERC20 metadata for the two coins (Erc20Builder.fetch_erc20_metadata).
        _call_key(COIN0, "decimals()"): _word_uint(6),
        _call_key(COIN0, "name()"): _str_word("Coin Zero"),
        _call_key(COIN0, "symbol()"): _str_word("CZ0"),
        _call_key(COIN1, "decimals()"): _word_uint(6),
        _call_key(COIN1, "name()"): _str_word("Coin One"),
        _call_key(COIN1, "symbol()"): _str_word("CO1"),
        # Optional A-ramping probes → all zero (not ramping).
        _call_key(POOL, "initial_A()"): _word_uint(0),
        _call_key(POOL, "initial_A_time()"): _word_uint(0),
        _call_key(POOL, "future_A()"): _word_uint(0),
        _call_key(POOL, "future_A_time()"): _word_uint(0),
        # Crypto probes → fee_gamma 0 (not crypto) + separate offpeg_fee.
        _call_key(POOL, "fee_gamma()"): _word_uint(0),
        _call_key(POOL, "offpeg_fee_multiplier()"): _word_uint(0),
        # Lending probes → not a cToken/yToken.
        _call_key(COIN0, "isCToken()"): _word_uint(0),
        _call_key(COIN0, "token()"): _word_address(ZERO),
        _call_key(COIN1, "isCToken()"): _word_uint(0),
        _call_key(COIN1, "token()"): _word_address(ZERO),
    }
    # Registry probes: no LP token, not a metapool (per real registry address).
    for registry in _REGISTRY_ADDRESSES:
        calls[_call_key_word_arg(registry, "get_lp_token(address)", POOL)] = _word_address(ZERO)
        calls[_call_key_word_arg(registry, "is_meta(address)", POOL)] = _word_uint(0)
    code = {COIN0.lower(): "60806040", COIN1.lower(): "60806040"}
    return json.dumps({
        "chain_id": 1,
        "block_number": _BLOCK,
        "timestamp": _TIMESTAMP,
        "calls": calls,
        "code": code,
    })


def _make_curve_builder(
    provider: RustAlloyProvider,
) -> tuple[CurvePoolBuilder, Bot]:
    """A real `CurvePoolBuilder` wired over an offline provider + the shared Bot.

    Mirrors `Bot.__init__` wiring (Erc20Builder → BuilderContext →
    CurvePoolBuilder) so `build` runs the full production I/O choreography:
    detection, ERC20 token building, `register_curve_pool`, `_from_py_pool`.
    Returns the builder and the shared `Bot`.
    """
    from degenbot.builders.context import BuilderContext
    from degenbot.builders.erc20_builder import Erc20Builder
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.registry import PoolRegistry, TokenRegistry

    py_bot = Bot(chain_id=1)
    py_bot.attach_construction_io(provider, None)
    fake_db = object.__new__(DatabaseSessionManager)
    tokens = TokenRegistry()
    pools = PoolRegistry(py_bot=py_bot)
    erc20 = Erc20Builder(
        default_chain_id=1,
        db=fake_db,
        tokens=tokens,
        py_bot=py_bot,
    )
    ctx = BuilderContext(
        db=fake_db,
        pools=pools,
        tokens=tokens,
        erc20_builder=erc20,
        py_bot=py_bot,
        default_chain_id=1,
    )
    return CurvePoolBuilder(ctx), py_bot


def test_build_curve_pool_plain_over_offline_cassette() -> None:
    """Rust ``build_curve_pool`` over a recorded cassette pins the plain-pool params.

    Asserts the exact values from the Rust test that owns the same contract
    `build_curve_pool_assembles_plain_pool_params` — proving the FFI + alloy
    `ConstructionIo` + Rust detection + register + handle round-trip are
    byte-exact (rate 10^30, precision 10^12, STANDARD/NONE strategies,
    Rust `data_provider` attached).
    """
    provider = RustAlloyProvider.offline_from_json_string(_plain_cassette_json())
    py_bot = Bot(chain_id=1)
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
    py_bot = Bot(chain_id=1)
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
    py_bot = Bot(chain_id=1)
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


def test_curve_pool_builder_build_matches_rust_path_over_cassette() -> None:
    """Dual-driver: real `CurvePoolBuilder.build` equals the Rust path (plain).

    Drives the CURRENT Python `CurvePoolBuilder.build` (detection + ERC20 token
    building + `register_curve_pool` + `_from_py_pool`) over the full plain
    cassette, and independently drives the Rust `build_curve_pool` over the
    SAME cassette. Asserts the resulting pool identity state is identical on
    both sides — the parity gate that lets `build` be retargeted to the Rust
    path without a silent behavior change.
    """
    from degenbot._ffi import BotIo
    from degenbot.builders.curve_pool_builder import _REGISTRY_ADDRESSES
    from degenbot.builders.request import BuildPoolRequest

    # Path A — Rust `build_curve_pool` + handle.
    provider_a = RustAlloyProvider.offline_from_json_string(_plain_full_cassette_json())
    bot_a = Bot(chain_id=1)
    bot_a.attach_construction_io(provider_a, None)
    handle_a = bot_a.get_pool(bot_a.build_curve_pool(POOL, list(_REGISTRY_ADDRESSES), _BLOCK))
    assert handle_a is not None

    # Path B — current Python `CurvePoolBuilder.build` over the same cassette.
    provider_b = RustAlloyProvider.offline_from_json_string(_plain_full_cassette_json())
    builder, bot_b = _make_curve_builder(provider_b)
    pybot_io = BotIo(provider=provider_b)
    pybot_io.attach_construction_io(bot_b)
    pool = builder.build(
        POOL,
        chain_id=1,
        io=pybot_io,
        request=BuildPoolRequest(state_block=_BLOCK, silent=True),
    )

    # Identity state must be identical across the two consumers of the Rust
    # core: the Rust handle (Path A) and the Python companion (Path B).
    assert pool.a_coefficient == handle_a.curve_a_coefficient == 2000
    assert pool.fee == handle_a.curve_fee == 1_000_000
    assert pool.admin_fee == handle_a.curve_admin_fee == 500_000_000
    assert tuple(pool.balances) == tuple(handle_a.balances) == (1_000_000, 2_000_000)
    assert tuple(pool.rate_multipliers) == tuple(handle_a.curve_rate_multipliers)
    assert tuple(pool.precision_multipliers) == tuple(handle_a.curve_precision_multipliers)
    assert pool.rate_multipliers[0] == 10**30
    assert len(pool.tokens) == 2


def _metapool_build_cassette_json() -> str:
    """Recorded reads for a metapool + its tripool base (builder recursion).

    Both pools live in one cassette (keyed by ``(to, calldata)``): the metapool
    M (coin → 3Crv LP, is_meta true on the real registry, underlying coins,
    base pool → TRIPOOL) and the base tripool (3 coins). Because the retargeted
    builder routes ALL detection through Rust `build_curve_pool` (whose
    `call_opt` probes tolerate unrecorded reads), only the real pool reads +
    the positive metapool probes + ERC20 metadata need recording — the optional
    ramping/crypto/lending/lp probes are left unrecorded (revert → None).
    """
    import json

    from degenbot.builders.curve_pool_builder import _REGISTRY_ADDRESSES

    registry = _REGISTRY_ADDRESSES[0]
    calls = {
        # --- Metapool M ---
        _call_key(META, "coins(uint256)", 0): _word_address(META_COIN0),
        _call_key(META, "coins(uint256)", 1): _word_address(THREE_CRV_LP),
        _call_key(META, "coins(uint256)", 2): _word_address(ZERO),
        _call_key(META, "balances(uint256)", 0): _word_uint(500_000_000_000_000),
        _call_key(META, "balances(uint256)", 1): _word_uint(1_000_000_000_000_000_000),
        _call_key(META, "A()"): _word_uint(500),
        _call_key(META, "fee()"): _word_uint(1_000_000),
        _call_key(META, "admin_fee()"): _word_uint(500_000_000),
        _call_key(META, "base_pool()"): _word_address(TRIPOOL),
        _call_key(META_COIN0, "decimals()"): _word_uint(6),
        _call_key(META_COIN0, "name()"): _str_word("Meta Coin"),
        _call_key(META_COIN0, "symbol()"): _str_word("MCO"),
        _call_key(THREE_CRV_LP, "decimals()"): _word_uint(18),
        _call_key(THREE_CRV_LP, "name()"): _str_word("Curve Dai USD Coin USD Tether"),
        _call_key(THREE_CRV_LP, "symbol()"): _str_word("3Crv"),
        # Real registry: metapool positive + underlying + no dedicated LP token.
        _call_key_word_arg(registry, "is_meta(address)", META): _word_uint(1),
        _call_key_word_arg(registry, "get_underlying_coins(address)", META): _word_addresses([
            META_COIN0,
            DAI,
            USDC,
            USDT,
        ]),
        _call_key_word_arg(registry, "get_lp_token(address)", META): _word_address(ZERO),
        # --- Base tripool (recursively built via the same builder) ---
        _call_key(TRIPOOL, "coins(uint256)", 0): _word_address(DAI),
        _call_key(TRIPOOL, "coins(uint256)", 1): _word_address(USDC),
        _call_key(TRIPOOL, "coins(uint256)", 2): _word_address(USDT),
        _call_key(TRIPOOL, "balances(uint256)", 0): _word_uint(1_000_000_000_000_000_000_000),
        _call_key(TRIPOOL, "balances(uint256)", 1): _word_uint(2_000_000_000_000),
        _call_key(TRIPOOL, "balances(uint256)", 2): _word_uint(3_000_000_000_000),
        _call_key(TRIPOOL, "A()"): _word_uint(3000),
        _call_key(TRIPOOL, "fee()"): _word_uint(1_000_000),
        _call_key(TRIPOOL, "admin_fee()"): _word_uint(500_000_000),
        _call_key(DAI, "decimals()"): _word_uint(18),
        _call_key(DAI, "name()"): _str_word("Dai Stablecoin"),
        _call_key(DAI, "symbol()"): _str_word("DAI"),
        _call_key(USDC, "decimals()"): _word_uint(6),
        _call_key(USDC, "name()"): _str_word("USD Coin"),
        _call_key(USDC, "symbol()"): _str_word("USDC"),
        _call_key(USDT, "decimals()"): _word_uint(6),
        _call_key(USDT, "name()"): _str_word("Tether USD"),
        _call_key(USDT, "symbol()"): _str_word("USDT"),
    }
    code = {
        META_COIN0.lower(): "60806040",
        THREE_CRV_LP.lower(): "60806040",
        DAI.lower(): "60806040",
        USDC.lower(): "60806040",
        USDT.lower(): "60806040",
    }
    return json.dumps({
        "chain_id": 1,
        "block_number": _BLOCK,
        "timestamp": _TIMESTAMP,
        "calls": calls,
        "code": code,
    })


def test_curve_pool_builder_build_metapool_recurses_base_over_cassette() -> None:
    """Retargeted `CurvePoolBuilder.build` recurses into the base pool (metapool).

    Drives the builder on a metapool whose second coin is the 3Crv LP. The
    Rust `build_curve_pool` detects the metapool + underlying coins, then the
    builder's `_resolve_metapool_base` recursively builds the tripool base pool
    in the SAME `Bot`, so the metapool handle's `curve_base_pool()` go-between
    resolves. Asserts the metapool companion's base pool + underlying coins are
    recovered, and its identity params equal the direct Rust path.
    """
    from degenbot._ffi import BotIo
    from degenbot.builders.curve_pool_builder import _REGISTRY_ADDRESSES
    from degenbot.builders.request import BuildPoolRequest

    # Path A — direct Rust build_curve_pool (oracle for M's identity params).
    provider_a = RustAlloyProvider.offline_from_json_string(_metapool_build_cassette_json())
    bot_a = Bot(chain_id=1)
    bot_a.attach_construction_io(provider_a, None)
    handle_a = bot_a.get_pool(bot_a.build_curve_pool(META, list(_REGISTRY_ADDRESSES), _BLOCK))
    assert handle_a is not None
    assert handle_a.curve_base_pool_address() == TRIPOOL

    # Path B — the retargeted builder, over the same cassette.
    provider_b = RustAlloyProvider.offline_from_json_string(_metapool_build_cassette_json())
    builder, bot_b = _make_curve_builder(provider_b)
    pybot_io = BotIo(provider=provider_b)
    pybot_io.attach_construction_io(bot_b)
    pool = builder.build(
        META,
        chain_id=1,
        io=pybot_io,
        request=BuildPoolRequest(state_block=_BLOCK, silent=True),
    )

    # The base pool was recursively built + registered in the same Bot, so
    # the metapool companion's base_pool go-between resolves (a lazy proxy over
    # the tripool handle; `.tokens` delegates to the resolved companion).
    assert pool.base_pool is not None
    assert [t.address.lower() for t in pool.base_pool.tokens] == [
        DAI.lower(),
        USDC.lower(),
        USDT.lower(),
    ]
    # Underlying coins recovered (META_COIN0 + the tripool's 3 coins).
    assert pool.tokens_underlying is not None
    assert [t.address.lower() for t in pool.tokens_underlying] == [
        META_COIN0.lower(),
        DAI.lower(),
        USDC.lower(),
        USDT.lower(),
    ]
    # Identity params equal the direct Rust path.
    assert pool.a_coefficient == handle_a.curve_a_coefficient == 500
    assert pool.fee == handle_a.curve_fee == 1_000_000
    assert pool.admin_fee == handle_a.curve_admin_fee == 500_000_000
    assert tuple(pool.balances) == tuple(handle_a.balances)
