"""Type stubs for the degenbot Rust extension module.

Python module: `degenbot._ffi`
Rust cdylib crate `degenbot_rs`

This module provides high-performance implementations of common operations used by the degenbot
Python package.
"""

from collections.abc import Callable, Coroutine
from typing import Any, overload

from hexbytes import HexBytes

from . import aave as aave

# ------------------------------------------------------------------
# ── Balancer V2 math (feature = "balancer-math"). ──
# ------------------------------------------------------------------
# Pure-math wrappers over the degenbot-balancer-math leaf, registered on a
# real Python submodule (`degenbot._ffi.balancer_math`) with un-prefixed names
# — the `balancer_` prefix was an artifact of the old flat root registration.
# The `version` discriminant (1=V1, 2=V2) is the bytecode-detected PowVersion;
# `round_up` is the stable-invariant V1(always-roundDown)/V2(roundUp) axis.
# Reverts surface as ValueError/OverflowError carrying the Solidity revert tag.
from . import balancer_math as balancer_math
from . import cancel as cancel

# ------------------------------------------------------------------
# ABI encoding / decoding
# ------------------------------------------------------------------
# Concentrated-liquidity math (feature = "concentrated-liquidity-math").
# Registered on a real Python submodule (`degenbot._ffi.concentrated_liquidity_math`) with
# un-prefixed names — the `cl_` prefix was an artifact of the flat root
# registration. See `concentrated_liquidity_math.pyi` for the 21 function signatures + the 4
# tick-boundary constants (MIN_TICK/MAX_TICK/MIN_SQRT_RATIO/MAX_SQRT_RATIO).
from . import concentrated_liquidity_math as concentrated_liquidity_math
from . import contract as contract
from . import curve_dy as curve_dy
from . import curve_math as curve_math
from . import db as db
from . import deployments as deployments
from . import dex_identity as dex_identity
from . import diagnostics as diagnostics
from . import eip_1559 as eip_1559
from . import execution as execution
from . import executor as executor
from . import fork as fork
from . import pool as pool
from . import price as price
from . import provider as provider
from . import simulation as simulation
from . import solady as solady
from . import solidly_math as solidly_math
from . import submission as submission
from . import subscriber as subscriber
from . import v2_math as v2_math
from .db import (
    ExchangeRow,
    LiquidityPoolRow,
)
from .dex_identity import DexIdentity
from .provider import (
    AlloyProvider as AlloyProvider,
)
from .provider import (
    AlloySubscription as AlloySubscription,
)
from .provider import (
    AsyncAlloyProvider as AsyncAlloyProvider,
)
from .provider import (
    LogFilter as LogFilter,
)
from .submission import (
    TxParams as TxParams,
)

# ── Curve StableSwap math (feature = "curve-math"). ──
# Pure-math wrappers over the degenbot-curve-math leaf, registered on a real
# Python submodule (`degenbot._ffi.curve_math`) with un-prefixed names — the
# `curve_` prefix was an artifact of the flat root registration.
# See `curve_math.pyi` for the function signatures.
# The variant discriminants (`d_variant`/`y_variant`/`yd_variant`) are 1-based
# `auto()` enum `.value`s matching the Rust `try_from_u8`. Vyper reverts
# (overflow / non-convergence / index / unsafe-value) all surface as
# `ValueError` — the shape the Python `DyCalculator` catches and wraps as
# `EVMRevertError(error=str(e))`.

# ── Solidly / Aerodrome / Camelot stable math (feature = "solidly-math"). ──
# Pure-math wrappers over the degenbot-solidly-math leaf, registered on a real
# Python submodule (`degenbot._ffi.solidly_math`) with un-prefixed names — the
# `solidly_` prefix was an artifact of the flat root registration; `camelot_`
# stays because camelot is a distinct variant in the same crate.
# See `solidly_math.pyi` for the function signatures.
# The Solidly / Camelot invariant math (calc_d / calc_k / calc_f /
# get_y_solidly / camelot_f / camelot_k / camelot_get_y_camelot) is pure
# integer arithmetic (Newton's method on the Solidly `x^3*y + y^3*x >= k`
# invariant); code that reverts on uint256 overflow / non-convergence /
# invalid token_in surfaces as ValueError / ZeroDivisionError carrying the
# Solidity revert tag. The Solidly + Camelot amount-out orchestration is
# exposed as two pre-baked entrypoints (one per (k_func, get_y_func) pair the
# companions actually use); `fee` is split into ``fee_numer``/``fee_denom`` at
# the seam so the pure-math leaf stays `num-rational`-free.

# ── Solady LibZip (FastLZ) (degenbot-core, no feature gate). ──
# Thin wrappers over `degenbot_core::libzip`. Accept a hex string (with
# optional `0x` prefix), `bytes`, `bytearray`, or `HexBytes`; return `HexBytes`.
# Truncated/invalid back-references surface as `ValueError`. The Python
# companion `degenbot.utils.solady.libzip` delegates here (sub-step C routing).
def build_path_graph(
    database_path: str,
    chain_id: int,
    pool_kinds: set[int],
    allowed_intermediate_token_ids: set[int] | None = ...,
) -> dict[str, Any]: ...
def find_paths_rust(
    edges: list[tuple[int, int, int, int]],
    start_token_id: int,
    end_token_id: int,
    min_depth: int,
    max_depth: int | None,
    include_reverse: bool,
    pool_type_per_depth: list[set[int] | None] | None = ...,
) -> PathIterator: ...

# ------------------------------------------------------------------
# SQLite file operations (feature = "db").
# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot_db::ops`. The CLI (`degenbot.cli.database`)
# delegates here; the GIL is released during file I/O. Raise `ValueError` on
# any connection / DDL / backup / integrity-check failure.
# `db_upgrade_database` returns a discriminant string.

# The CancelHandle class lives on the degenbot._ffi.cancel submodule
# (stub: cancel.pyi) — do not define it at this top level.
# ------------------------------------------------------------------
# V3/V4 DB-aware liquidity updater seam (feature = "db").
# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot-db`'s `apply_v3_liquidity_updates` /
# `apply_v4_liquidity_updates` (the Rust apply-and-persist core). The Python
# `cli/pool.py::apply_v3/v4_liquidity_updates` decode the raw `LogReceipt`s
# (Burn/Mint negation) + delegate the reconstitute→apply-math→persist here.
# Each call opens its own write handle on `database_path` (SQLite WAL allows
# the concurrent connection); the driver's session stays open for its reads.
# Events are pre-decoded ``(block_number, log_index, tick_lower, tick_upper,
# liquidity_delta)`` tuples — the ABI decode stays in Python per the seam
# boundary (`degenbot-db` is pure I/O+math, no ABI decode).

# ------------------------------------------------------------------
# Pool discovery writers (WR7EA6 — split out of QJSCA5).
# Thin PyO3 wrappers over `degenbot-db`'s `discovery` substrate
# (`upsert_v2/v3/v4_pools` + `set_exchange_last_update_block`). The Python
# `cli/pool_updater_configs.py::update_v2/v3/v4_pools` shells decode the raw
# `PoolCreated` `LogReceipt`s + do the RPC fee lookup, then build row-input
# lists + delegate here — the Rust core owns the `erc20_tokens` get-or-create
# escalate + the polymorphic pool-row insert + the exchange stamp. Raises
# ``ValueError`` on a DB failure or an unknown `kind` discriminator.

# ------------------------------------------------------------------
# Thin PyO3 wrappers over `degenbot_executor` (the cmd-executor core).
# The encode path lives in the Rust core: `dispatch_profitable_py` calls
# `composers::encode_cmd_stream` (ADR-005), and the candidate resolves
# `composers::PathInfo` from `path_id` via `path_info_for_core`. Rust-side
# output is pinned by golden-file tests in `cargo test -p degenbot-executor`.
# The GIL is released during the warmup-slot compute.

type WarmupDict = dict[str, dict[str, Any]]

class PathIterator:
    def __iter__(self) -> PathIterator: ...
    def __next__(self) -> list[tuple[int, int]]: ...

# A plain ``str`` holding an EIP-55 checksummed 20-byte hex address
# (``0x`` + 40 chars with the correct checksum casing). Rust FFI entry
# points that return an address decode + checksum it in Rust
# (``Address::to_checksum``) and cross the boundary as a ``PyString`` —
# hence a plain ``str`` at runtime, not an ``eth_typing.ChecksummedAddress``
# instance. It is equivalent to an ``eth_typing.ChecksummedAddress`` where one is
# expected (content-based equality) but is not an ``isinstance`` match;
# use this alias to document the checksum guarantee without claiming the
# ``eth_typing`` class.
type ChecksummedAddress = str

@overload
def to_checksum_address(address: str) -> ChecksummedAddress: ...
@overload
def to_checksum_address(address: bytes) -> str: ...
def compute_aerodrome_v2_pool_address(
    deployer_address: str,
    token0: str,
    token1: str,
    stable: bool,
    implementation_address: str,
) -> str: ...
def compute_aerodrome_v3_pool_address(
    deployer_address: str,
    token0: str,
    token1: str,
    tick_spacing: int,
    implementation_address: str,
) -> str: ...

# ── ABI decoder/encoder (feature = "abi"). ──
# Pure-math wrappers over the degenbot-abi leaf, registered on a real Python
# submodule (`degenbot._ffi.abi`) with un-prefixed names.
# See `abi.pyi` for the function signatures (decode/decode_single/encode/
# encode_single). ValueError on invalid data; NotImplementedError for
# unsupported types (e.g. fixed-point).
from . import abi as abi  # ruff:ignore[module-import-not-at-top-of-file]

# The AsyncContract class lives on the degenbot._ffi.contract submodule
# (stub: contract.pyi) — do not define it at this top level.
class Erc20Token:
    """Thin PyO3 handle to a token registered in the Rust `Bot`.

    All metadata lives in Rust; reads cross PyO3 on every access. Not directly
    constructible — obtain one via `Bot.register_token` / `Bot.get_token`.
    """

    @property
    def address(self) -> str: ...
    @property
    def decimals(self) -> int: ...
    @property
    def symbol(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def chain_id(self) -> int: ...

class LiquidityPool:
    """Thin PyO3 handle to a pool registered in the Rust `Bot`.

    Owns no state — calculation/encoding calls cross PyO3 on every access,
    reading the shared Rust-owned `Bot` under a read guard. Not directly
    constructible — obtain one via `Bot.get_pool`.
    """

    @property
    def pool_id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def factory(self) -> str: ...
    @property
    def token0_address(self) -> str: ...
    @property
    def token1_address(self) -> str: ...
    @property
    def deployer(self) -> str: ...
    @property
    def init_hash(self) -> str: ...
    @property
    def fee_token0(self) -> tuple[int, int]: ...
    @property
    def fee_token1(self) -> tuple[int, int]: ...
    @property
    def variant(self) -> str: ...
    @property
    def pool_family(self) -> str: ...
    @property
    def stable_swap(self) -> bool: ...
    @property
    def fee_denominator(self) -> int | None: ...
    @property
    def dex(self) -> DexIdentity | None: ...
    def get_token0(self) -> Erc20Token | None: ...
    def get_token1(self) -> Erc20Token | None: ...
    @property
    def reserve0(self) -> int: ...
    @property
    def reserve1(self) -> int: ...
    @property
    def update_block(self) -> int: ...
    #: The **liquidity** clock — block the tick map reflects (two-stamp OB7UNY).
    #: CL (V3/V4) only; 0 otherwise. A pool whose ``tick_data_block`` lags its
    #: ``update_block`` is the staged-clock desync class (fresh price, stale
    #: liquidity map).
    @property
    def tick_data_block(self) -> int: ...
    @property
    def sqrt_price_x96(self) -> int: ...
    @property
    def liquidity(self) -> int: ...
    @property
    def tick(self) -> int: ...
    @property
    def fee(self) -> int: ...
    @property
    def tick_spacing(self) -> int: ...
    @property
    def pool_manager_address(self) -> str: ...
    @property
    def pool_id_hex(self) -> str: ...
    @property
    def hook_address(self) -> str: ...
    @property
    def balancer_address(self) -> str: ...
    @property
    def balancer_vault(self) -> str: ...
    @property
    def balancer_pool_id_hex(self) -> str: ...
    @property
    def balancer_token_addresses(self) -> list[str]: ...
    @property
    def balancer_weights(self) -> list[int]: ...
    @property
    def balancer_scaling_factors(self) -> list[int]: ...
    @property
    def balancer_swap_fee(self) -> int: ...
    @property
    def balancer_pow_version(self) -> int: ...
    def get_balancer_tokens(self) -> list[Erc20Token] | None: ...
    @property
    def dex_name(self) -> str | None: ...
    @property
    def aerodrome_stable(self) -> bool: ...
    @property
    def aerodrome_fee(self) -> tuple[int, int]: ...
    @property
    def aerodrome_token0_decimals(self) -> int: ...
    @property
    def aerodrome_token1_decimals(self) -> int: ...
    @property
    def aerodrome_reserve0(self) -> int: ...
    @property
    def aerodrome_reserve1(self) -> int: ...
    def snapshot_aerodrome(self) -> tuple[int, int, int] | None: ...
    def apply_aerodrome_sync(self, reserve0: int, reserve1: int, block_number: int) -> None: ...
    def discard_aerodrome_before_block(self, block: int) -> None: ...
    def restore_aerodrome_before_block(self, block: int) -> tuple[int, int, int] | None: ...
    def snapshot(self) -> tuple[int, int, int] | None: ...
    def snapshot_v3(self) -> tuple[int, int, int, int] | None: ...
    def calculate_tokens_out(self, zero_for_one: bool, amount_in: int) -> int: ...
    def calculate_tokens_in(self, zero_for_one: bool, amount_out: int) -> int: ...
    def calculate_tokens_out_with_fetch(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
    ) -> int: ...
    def simulate_swap_with_fetch(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_swap_with_override(
        self,
        zero_for_one: bool,
        amount_in: int,
        block: int,
        override_sqrt_price_x96: int,
        override_liquidity: int,
        override_tick: int,
        override_tick_data: dict[int, tuple[int, int, int]],
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_exact_output_swap_with_override(
        self,
        zero_for_one: bool,
        amount_out: int,
        block: int,
        override_sqrt_price_x96: int,
        override_liquidity: int,
        override_tick: int,
        override_tick_data: dict[int, tuple[int, int, int]],
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def simulate_exact_output_swap_with_fetch(
        self,
        zero_for_one: bool,
        amount_out: int,
        block: int,
        sqrt_price_limit_x96: int | None = None,
    ) -> tuple[int, int, int, int, int] | None: ...
    def encode_swap(
        self,
        zero_for_one: bool,
        amount_out: int,
        recipient: str,
    ) -> tuple[str, str, int] | None: ...
    def sync_reserves(self, reserve0: int, reserve1: int, block_number: int) -> None: ...
    def apply_swap(
        self,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block_number: int,
    ) -> bool: ...
    def seed_genesis(self, block_number: int) -> bool: ...
    def apply_liquidity_update(
        self,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
        block_number: int,
    ) -> bool: ...
    def update_tick_data(
        self,
        tick_bitmap: dict[int, Any],
        tick_data: dict[int, tuple[int, int, int]],
        block: int,
    ) -> bool: ...
    def tick_data_snapshot(self) -> dict[int, tuple[int, int, int]]: ...
    def tick_bitmap_snapshot(self) -> dict[int, tuple[int, int]]: ...
    @property
    def n_coins(self) -> int: ...
    @property
    def balances(self) -> list[int]: ...
    def snapshot_curve(self) -> tuple[list[int], int] | None: ...
    def apply_curve_balance_update(self, balances: list[int], block_number: int) -> bool: ...
    def curve_a_ramp(
        self,
    ) -> tuple[int | None, int | None, int | None, int | None, int | None] | None: ...
    def curve_crypto_fees(
        self,
    ) -> tuple[int | None, int | None, int | None, int | None, int | None] | None: ...
    def curve_lp_token(self) -> str | None: ...
    @property
    def curve_use_lending(self) -> list[bool]: ...
    @property
    def curve_precision_multipliers(self) -> list[int]: ...
    @property
    def curve_has_data_provider(self) -> bool: ...
    @property
    def curve_a_coefficient(self) -> int: ...
    @property
    def curve_fee(self) -> int: ...
    @property
    def curve_admin_fee(self) -> int: ...
    @property
    def curve_rate_multipliers(self) -> list[int]: ...
    @property
    def curve_swap_style(self) -> int: ...
    @property
    def curve_lending_rate_style(self) -> int: ...
    @property
    def curve_d_variant(self) -> int: ...
    @property
    def curve_y_variant(self) -> int: ...
    @property
    def curve_yd_variant(self) -> int: ...
    @property
    def curve_metapool_rate_style(self) -> int: ...
    @property
    def curve_metapool_underlying_style(self) -> int: ...
    def curve_base_pool_address(self) -> str | None: ...
    def get_curve_tokens(self) -> list[Erc20Token] | None: ...
    def get_curve_tokens_underlying(self) -> list[Erc20Token] | None: ...
    def get_curve_lp_token(self) -> Erc20Token | None: ...
    def curve_token_addresses(self) -> list[str] | None: ...
    def curve_token_addresses_underlying(self) -> list[str] | None: ...
    def curve_lp_token_address(self) -> str | None: ...
    def curve_base_pool(self) -> LiquidityPool | None: ...
    def curve_get_dy(
        self,
        i: int,
        j: int,
        dx: int,
        block_number: int,
        override_balances: list[int] | None = None,
    ) -> int: ...
    def curve_get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_number: int,
        override_balances: list[int] | None = None,
    ) -> int: ...
    def curve_calc_token_amount(
        self, amounts: list[int], deposit: bool, block_number: int
    ) -> int: ...
    def curve_calc_withdraw_one_coin(self, token_amount: int, i: int, block_number: int) -> int: ...
    def fetch_curve_block_number(self) -> int | None: ...
    def fetch_curve_block_timestamp(self, block_number: int) -> int | None: ...
    def fetch_curve_token_balance(
        self, token_address: str, holder_address: str, block_number: int
    ) -> int | None: ...
    def fetch_curve_token_total_supply(
        self, token_address: str, block_number: int
    ) -> int | None: ...
    def fetch_curve_lending_rates(self, block_number: int) -> list[int]: ...
    def fetch_curve_d(self, block_number: int) -> int | None: ...
    def fetch_curve_gamma(self, block_number: int) -> int | None: ...
    def fetch_curve_price_scale(self, block_number: int) -> list[int]: ...
    def fetch_curve_admin_balances(self, block_number: int) -> list[int]: ...
    def fetch_curve_redemption_price(self, block_number: int) -> int | None: ...
    def fetch_curve_base_cache_updated(self, block_number: int) -> int | None: ...
    def fetch_curve_base_virtual_price(self, block_number: int) -> int | None: ...
    def fetch_curve_virtual_price(self, block_number: int) -> int | None: ...
    @property
    def n_balancer_tokens(self) -> int: ...
    @property
    def balancer_balances(self) -> list[int]: ...
    def snapshot_balancer_weighted(self) -> tuple[list[int], int] | None: ...
    def apply_balancer_weighted_balance_update(
        self,
        balances: list[int],
        block_number: int,
    ) -> bool: ...
    @property
    def n_balancer_stable_tokens(self) -> int: ...
    @property
    def balancer_stable_balances(self) -> list[int]: ...
    @property
    def balancer_bpt_index(self) -> int | None: ...
    @property
    def balancer_amp(self) -> int: ...
    @property
    def balancer_invariant_version(self) -> int: ...
    @property
    def balancer_stable_vault(self) -> str: ...
    @property
    def balancer_stable_pool_id_hex(self) -> str: ...
    @property
    def balancer_stable_token_addresses(self) -> list[str]: ...
    def get_balancer_stable_tokens(self) -> list[Erc20Token] | None: ...
    @property
    def balancer_stable_scaling_factors(self) -> list[int]: ...
    @property
    def balancer_stable_swap_fee(self) -> int: ...
    @property
    def balancer_stable_rate_provider_is_static(self) -> bool: ...
    def fetch_balancer_stable_rates(self, block_identifier: int | None) -> list[int] | None: ...
    def snapshot_balancer_stable(self) -> tuple[list[int], int] | None: ...
    def apply_balancer_stable_balance_update(
        self,
        balances: list[int],
        block_number: int,
    ) -> bool: ...
    def journal_len(self) -> int: ...
    def discard_before_block(self, block: int) -> None: ...
    def restore_before_block(self, block: int) -> tuple[int, int, int] | None: ...
    def discard_v3_before_block(self, block: int) -> None: ...
    def restore_v3_before_block(self, block: int) -> tuple[int, int, int, int] | None: ...

class Erc20TokenRow:
    """A typed `erc20_tokens` DB row (QVMWQC).

    Returned by `BotIo.fetch_erc20_token`; mirrors the SQLAlchemy
    `Erc20TokenTable` ORM attributes the builders read.
    """

    @property
    def id(self) -> int: ...
    @property
    def chain(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def name(self) -> str | None: ...
    @property
    def symbol(self) -> str | None: ...
    @property
    def decimals(self) -> int | None: ...

class BotIo:
    """PyO3 wrapper (exposed as `BotIo` in Python) holding an alloy provider + optional DB.

    The Rust I/O facade for pool builders (ADR-005 slice 14a). Builders receive
    this as the single construction-I/O executor (Rust-backed, 65 methods: the
    7 RPC primitives below + ~50 ``fetch_*`` DB/RPC-choreography helpers). It
    routes the RPC surface through the core ``ConstructionIo`` trait (the
    attached handle, else a transient ``(NoDb, AlloyRpcConstruction)`` over the
    held alloy provider); non-alloy providers error loudly (ADR-023 D1). The
    calling convention is positional leading args + ``block=`` kwarg.
    """

    def __init__(
        self, provider: object, db: object | None = None, database_path: str | None = None
    ) -> None: ...
    def attach_construction_io(self, py_bot: Bot) -> None:
        """Source the ``ConstructionIo`` handle from ``py_bot`` (slice A).

        After this call the 12 DB + 7 generic RPC methods delegate through
        the core trait objects; the 27 choreography wrappers stay unchanged.
        """
    @property
    def db(self) -> object | None: ...
    @property
    def database_path(self) -> str | None: ...
    def fetch_erc20_token(self, chain_id: int, address: str) -> Erc20TokenRow | None: ...
    def update_erc20_token_metadata(
        self,
        chain_id: int,
        address: str,
        name: str | None,
        symbol: str | None,
        decimals: int | None,
    ) -> None: ...
    def fetch_pool_row(self, chain_id: int, address: str) -> LiquidityPoolRow | None: ...
    def fetch_exchange(self, exchange_id: int) -> ExchangeRow | None: ...
    # --- ADR-005 build-path RPC delegation (slice 14) ---
    def fetch_factory_address(self, address: str) -> ChecksummedAddress | None: ...
    def fetch_erc20_metadata_batch(
        self, addresses: list[str]
    ) -> list[tuple[str, str, int] | None]: ...
    def fetch_token_balance(self, token: str, owner: str, block: int | None = None) -> int: ...
    def fetch_token_allowance(
        self, token: str, owner: str, spender: str, block: int | None = None
    ) -> int: ...
    def fetch_token_total_supply(self, token: str, block: int | None = None) -> int: ...
    def fetch_v2_reserves(self, pool_address: str, block: int | None = None) -> tuple[int, int]: ...
    def fetch_v3_slot0_liquidity(
        self, pool_address: str, block: int | None = None
    ) -> tuple[int, int, int]: ...
    def fetch_v4_slot0_liquidity(
        self, state_view_address: str, pool_id: bytes, block: int | None = None
    ) -> tuple[int, int, int, int, int]: ...
    def fetch_tick_bitmap(
        self, pool_address: str, word_position: int, block: int | None = None
    ) -> int: ...
    def fetch_tick_data(
        self, pool_address: str, tick: int, block: int | None = None
    ) -> tuple[int, int]: ...
    def fetch_v4_tick_bitmap(
        self, state_view_address: str, pool_id: bytes, word_position: int, block: int | None = None
    ) -> int: ...
    def fetch_v4_tick_data(
        self, state_view_address: str, pool_id: bytes, tick: int, block: int | None = None
    ) -> tuple[int, int]: ...
    def fetch_curve_balances(
        self, pool_address: str, count: int, block: int | None = None
    ) -> list[int]: ...
    def fetch_balancer_pool_id(self, address: str, block: int | None = None) -> bytes: ...
    def fetch_balancer_swap_fee(self, address: str, block: int | None = None) -> int: ...
    def fetch_balancer_amp(self, address: str, block: int | None = None) -> int: ...
    def fetch_balancer_weights(self, address: str, block: int | None = None) -> list[int]: ...
    def fetch_balancer_rate_providers(
        self, address: str, block: int | None = None
    ) -> list[str]: ...
    def fetch_balancer_vault_tokens(
        self, vault_address: str, pool_id: bytes, block: int | None = None
    ) -> tuple[list[str], list[int]]: ...
    def fetch_balancer_rate(self, provider_address: str, block: int | None = None) -> int: ...
    def probe_pool_type(self, address: str, block: int | None = None) -> str: ...
    def probe_balancer_pool_type(self, address: str, block: int | None = None) -> str: ...
    def get_block_number(self) -> int: ...
    def get_code(self, address: str, block: int | None = None) -> HexBytes: ...
    def get_balance(self, address: str, block: int | None = None) -> int: ...

class Bot:
    """PyO3 wrapper (exposed as `Bot` in Python) holding `Arc<RwLock<Bot>>`.

    The Polars-style middle layer between the pure-Rust `Bot` and the Python
    `Bot` session. The Python ``Bot.__init__`` constructs one; `LiquidityPool`/`Erc20Token`
    handles share the same `Arc`. Queries/calcs take a read guard on the
    shared `Bot`; mutations take a write guard.
    """

    def __init__(self, chain_id: int = 0) -> None:
        """Construct a ``Bot`` for ``chain_id`` (ADR-006 D4).

        The default ``chain_id = 0`` keeps the bare ``Bot()`` test fixtures
        (which only exercise the Rust core) working without a chain invariant.
        """
    def load_snapshot_from_db(self, db_path: str, chain_id: int) -> None:
        """Load `S` from the DB into the core `BotState` at construction time.

        Called from Python ``Bot.__init__`` when ``config.database.path`` exists.
        Opens a ``SnapshotDb`` — a read-only handle with a held deferred read
        transaction (epic ``XEANMB``) — and calls the core
        ``Bot::load_snapshot_from_db`` which reads ``S =
        min(fetch_newest_update_block(V3), V4)`` INSIDE the held tx so ``S`` +
        every per-pool ``assemble_*_tick_map`` Db-arm read during ``build_paths``
        share one frozen DB snapshot (the consistency replacement for the retired
        ``SnapshotStore``). WAL MVCC: a concurrent ``pool_updater`` write cannot
        perturb the held snapshot.

        After this, pool registration reads tick data through the held tx via
        ``assemble_*_tick_map``'s Db arm. Call ``close_snapshot_tx()`` after
        ``build_paths`` to release the WAL snapshot.

        ``None``/cold-start (no pools) is NOT an error — the pump anchors on
        ``first_observed_block`` at resume.
        """
    def close_snapshot_tx(self) -> None:
        """Commit + drop the held snapshot read transaction (epic ``XEANMB``).

        Call after ``build_paths`` finishes so the WAL snapshot is released +
        the ``pool_updater``'s checkpoint can reclaim ``-wal`` space for the
        hot loop. After this, ``db_handle()`` returns ``None`` — the
        ``assemble_*`` Db arm is gone for any late registrations (they'd need a
        fresh handle). Idempotent on a not-loaded bot (no-op).

        Operator-discipline canary (task 5.7): after committing the held tx,
        re-reads ``S_live = min(fetch_newest_update_block(V3), V4)`` in a fresh
        autocommit tx on the same connection (now seeing the live DB). If
        ``S_live > S_snapshot`` (captured at startup inside the held tx), emits
        a ``log::warn!`` that the DB advanced during startup — the
        ``pool_updater`` committed concurrently with ``build_paths``.
        Correctness was already preserved by the held tx; the canary only
        surfaces the discipline violation.
        """
    @property
    def snapshot_seed_block(self) -> int | None:
        """The snapshot seed block `S` (or ``None`` cold-start).

        Python reads this in ``engine_registry.start()`` to stash
        ``_verify_snapshot_block``.
        """

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, block: int | None) -> None:
        """Set the snapshot seed block `S` (non-DB path, 2SM4Y7).

        Called by ``engine_registry.start()`` after ``load_*_from_py`` when a
        non-DB (file/memory) snapshot is supplied — records ``S =
        min(newest_block)`` so the core auto-backfill inside
        ``BlockPump::resume_from_subscribe`` closes the snapshot→WS gap (J3FMDO).
        The DB path's ``load_snapshot_from_db`` already sets `S` itself.
        """
    def attach_construction_io(self, provider: object, database_path: str | None = None) -> None:
        """Attach the core ``ConstructionIo`` handle to ``Bot`` (slice A).

        Builds the native adapters from the extracted ``AlloyProvider`` + an
        optional held ``DegenbotDbConstruction`` and attaches them to ``Bot``
        via ``Bot::set_construction_io``. Soft-skip for non-alloy providers
        (preserves the test-double path). A missing DB file → ``NoDb``.
        """

    def assemble_v3_tick_map(
        self,
        address: str,
        *,
        tick: int = 0,
        tick_spacing: int = 0,
        block: int = 0,
        io: BotIo | None = None,
    ) -> tuple[dict[int, tuple[int, int, int]], str] | None:
        """Assemble a V3 pool's tick map with `Store → Db → Chain` precedence.

        Probes the bulk-loaded `SnapshotStore` (consumed once per pool); on a
        miss falls back to `DegenbotDb::fetch_liquidity_map`; on a further miss
        + ``io`` provided, runs the Chain arm (`AlloyTickBootstrapRpc` — sparse
        RPC word read). Returns ``(tick_data, coverage)`` on a hit, ``None`` on
        a miss. Coverage is ``"tracked"`` on a Store/Db hit, ``"sparse"`` on a
        Chain hit.

        ``tick_data`` has the same ``{tick: (liquidity_gross, liquidity_net,
        block)}`` shape as ``register_v3_pool``'s ``tick_data`` arg — pass it
        straight back.

        ``io`` extracts the native alloy provider (no GIL re-entry per RPC);
        ``None`` or a non-alloy provider → no Chain arm (Store + Db only).

        Raises:
            RuntimeError: on a Db read failure or Chain-arm RPC failure
                (Decision 8 (A) — loud error over silent degradation).

        """

    def assemble_v4_tick_map(
        self,
        pool_manager: str,
        pool_id: str | bytes,
        state_view: str,
        *,
        tick: int = 0,
        tick_spacing: int = 0,
        block: int = 0,
        io: BotIo | None = None,
    ) -> tuple[dict[int, tuple[int, int, int]], str] | None:
        """Assemble a V4 pool's tick map — V4 twin of `assemble_v3_tick_map`.

        ``pool_id`` is the V4 pool id — a 0x-prefixed 66-char hex `str` or 32-byte
        `bytes`. ``state_view`` is the V4 StateView contract address (the
        Chain-arm RPC target; distinct from the `PoolManager`). Same precedence,
        miss, and error semantics as the V3 variant.
        """

    def register_v2_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        reserve0: int,
        reserve1: int,
        gamma_numer0: int,
        fee_denom0: int,
        gamma_numer1: int,
        fee_denom1: int,
        factory: str,
        update_block: int = 0,
        variant: str = "uniswap-v2",
        stable_swap: bool = False,
        fee_denominator: int | None = None,
    ) -> int: ...
    def build_v2_pool(
        self, address: str, block: int | None = None
    ) -> tuple[int, str, str, str, str]: ...
    def build_aerodrome_v2_pool(self, address: str, block: int | None = None) -> int: ...
    def build_balancer_weighted_pool(
        self, address: str, vault: str, block: int | None = None
    ) -> int: ...
    def build_balancer_stable_pool(
        self,
        address: str,
        vault: str,
        block: int | None = None,
        invariant_version: int | None = None,
    ) -> int: ...
    def build_curve_pool(
        self, address: str, registry_addresses: list[str], block: int | None = None
    ) -> int: ...
    def build_v3_pool(
        self,
        address: str,
        block: int | None = None,
        db: bool = True,
        tick_data_fetcher: Callable | None = None,
    ) -> tuple[int, str, str, str, str]: ...
    def resolve_v4_identity(
        self,
        chain_id: int,
        pool_manager: str,
        pool_id_hex: str,
        currency0: str | None = None,
        currency1: str | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        state_view_address: str | None = None,
    ) -> tuple[str, str, int, int, int, str]: ...
    def build_v4_pool(
        self,
        pool_manager: str,
        pool_id_hex: str,
        currency0: str,
        currency1: str,
        fee: int,
        tick_spacing: int,
        hook_flags: int,
        state_view_address: str,
        block: int | None = None,
        db: bool = True,
        tick_data_fetcher: Callable | None = None,
    ) -> tuple[
        int,
        str,
        str,
        str,
        str,
        int,
        int,
        int,
        str,
        int,
        int,
    ]: ...
    def update_v2_pool(
        self,
        address: str,
        reserve0: int,
        reserve1: int,
        block_number: int,
    ) -> None: ...
    def calculate_tokens_out(self, pool_id: int, zero_for_one: bool, amount_in: int) -> int: ...
    def calculate_tokens_in(self, pool_id: int, zero_for_one: bool, amount_out: int) -> int: ...
    def pool_count(self) -> int: ...
    def chain_id(self) -> int: ...
    def dispatch_log(
        self,
        address: str,
        topics: list[str],
        data: str,
        block_number: int = 0,
    ) -> None: ...
    def get_pool(self, pool_id: int) -> LiquidityPool | None: ...
    def unregister_pool(self, address: str, pool_id: bytes | None = None) -> bool: ...
    def register_v3_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        fee: int,
        tick_spacing: int,
        factory: str,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        tick_data: dict[int, tuple[int, int, int]] | None = None,
        update_block: int = 0,
        coverage: str = "sparse",
        tick_data_fetcher: Callable[[int, int], dict[int, tuple[int, int, int]] | None]
        | None = None,
        tick_data_block: int | None = None,
    ) -> int: ...
    def update_v3_pool(
        self,
        address: str,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block_number: int,
    ) -> None: ...
    def register_v4_pool(
        self,
        pool_manager: str,
        pool_id_hex: str,
        currency0: str,
        currency1: str,
        fee: int,
        tick_spacing: int,
        hook_flags: int,
        sqrt_price_x96: int,
        liquidity: int,
        tick: int,
        block: int,
        tick_data: dict[int, tuple[int, int, int]] | None = None,
        coverage: str = "tracked",
        tick_data_fetcher: Callable[[int, int], dict[int, tuple[int, int, int]] | None]
        | None = None,
        protocol_fee: int = 0,
        tick_data_block: int | None = None,
    ) -> int: ...

    # ADR-005 / Option 2: seed the Rust-owned V4 StateView registry (the
    # solver-state accuracy gate verifies V4 hops via getSlot0/getLiquidity).
    def register_v4_state_view(self, pool_manager: str, state_view: str) -> None: ...
    # PumpState attached when a ArbitrageEngine(py_bot=...) is constructed.
    def subscribe(self, rpc_url: str) -> int: ...
    def resume(self) -> None: ...
    def stop(self) -> None: ...
    def set_verify_rpc_url(self, rpc_url: str) -> None: ...
    def set_verify_state_view(self, state_view_address: str) -> None: ...
    def run_v3_registration_lifecycle(
        self,
        address: str,
        snapshot_block: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def run_v4_registration_lifecycle(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        snapshot_block: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def register_curve_pool(
        self,
        address: str,
        tokens: list[str],
        a_coefficient: int,
        a_precision: int,
        fee: int,
        admin_fee: int,
        rate_multipliers: list[int],
        balances: list[int],
        update_block: int,
        swap_style: int = 0,
        lending_rate_style: int = 0,
        d_variant: int = 0,
        y_variant: int = 0,
        yd_variant: int = 0,
        base_pool: str | None = None,
        initial_a_coefficient: int | None = None,
        future_a_coefficient: int | None = None,
        initial_a_coefficient_time: int | None = None,
        future_a_coefficient_time: int | None = None,
        create_timestamp: int | None = None,
        fee_gamma: int | None = None,
        mid_fee: int | None = None,
        offpeg_fee_multiplier: int | None = None,
        out_fee: int | None = None,
        gamma: int | None = None,
        lp_token: str | None = None,
        use_lending: list[bool] | None = None,
        precision_multipliers: list[int] | None = None,
        tokens_underlying: list[str] | None = None,
        metapool_rate_style: int = 1,
        metapool_underlying_style: int = 1,
        data_provider: Any = None,  # ruff:ignore[any-type] — pyo3 accepts PyAny
    ) -> int: ...
    def register_balancer_weighted_pool(
        self,
        address: str,
        vault: str,
        pool_id_hex: str,
        tokens: list[str],
        weights: list[int],
        scaling_factors: list[int],
        swap_fee: int,
        pow_version: int,
        balances: list[int],
        update_block: int,
    ) -> int: ...
    def register_balancer_stable_pool(
        self,
        address: str,
        vault: str,
        pool_id_hex: str,
        tokens: list[str],
        amp: int,
        scaling_factors: list[int],
        swap_fee: int,
        bpt_idx: int | None,
        invariant_version: int,
        balances: list[int],
        update_block: int,
        rate_provider: Any = None,  # ruff:ignore[any-type] — pyo3 accepts PyAny
    ) -> int: ...
    def register_aerodrome_pool(
        self,
        address: str,
        token0: str,
        token1: str,
        factory: str,
        variant: str,
        stable: bool,
        fee_numer: int,
        fee_denom: int,
        token0_decimals: int,
        token1_decimals: int,
        reserve0: int,
        reserve1: int,
        update_block: int,
    ) -> int: ...
    def v3_journal_len(self, pool_id: int) -> int: ...
    def v3_discard_before_block(self, pool_id: int, block: int) -> None: ...
    def v3_restore_before_block(
        self,
        pool_id: int,
        block: int,
    ) -> tuple[int, int, int, int] | None: ...
    def register_token(
        self,
        address: str,
        name: str,
        symbol: str,
        decimals: int,
        chain_id: int,
    ) -> Erc20Token: ...
    def get_token(self, address: str) -> Erc20Token | None: ...
    def build_erc20_token(
        self,
        address: str,
        chain_id: int,
        block: int | None = None,
    ) -> Erc20Token: ...
    def encode_swap(
        self,
        pool_id: int,
        zero_for_one: bool,
        amount_out: int,
        recipient: str,
    ) -> tuple[str, str, int] | None: ...
    def v2_journal_len(self, pool_id: int) -> int: ...
    def v2_discard_before_block(self, pool_id: int, block: int) -> None: ...
    def v2_restore_before_block(self, pool_id: int, block: int) -> tuple[int, int, int] | None: ...

class ArbitrageEngine:
    """Rust-side engine for Uniswap arbitrage path solving.

    ADR-006 D1+D4: ``py_bot`` adopts a shared ``Bot``'s ``BotState`` so the
    engine reads/writes the SAME core that ``Bot``/``LiquidityPool``/
    ``Erc20Token`` share — dissolving the dual-``BotState`` split (the
    ``rust-owned-bot.md`` §17 stale-state root cause). Omitted → a standalone
    core + fresh ``Bot`` (legacy / no-pyo3 path).
    """

    def __init__(self, py_bot: Bot | None = None) -> None: ...

    # ── Snapshot ingestion surface RETIRED (epic XEANMB). ──
    # The `load_*_from_py` / `clear_*_snapshot` methods are gone: the
    # in-memory `SnapshotStore` they fed is replaced by a WAL held read
    # transaction (`SnapshotDb`) for the DB path, + the Chain arm (RPC) for
    # the non-DB path. `start()` now computes `S = min(newest_block)` + sets
    # `snapshot_seed_block` before `subscribe()` (so `after_subscribe`
    # advances the phase to `SnapshotLoaded`).

    @property
    def snapshot_seed_block(self) -> int | None:
        """The snapshot seed block `S` (set at `Bot.__init__` by `load_snapshot_from_db`).

        ``None`` = cold-start. `engine_registry.start()` reads this to drive
        the snapshot→WS backfill + stash `_verify_snapshot_block`.
        """

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, block: int | None) -> None:
        """Set the snapshot seed block `S` (non-DB path, 2SM4Y7)."""

    # ── Phase / startup ritual (Plan 102: the canonical two-phase flow). ──
    def subscribe(self, rpc_url: str) -> int: ...
    def resume(self) -> None: ...
    def stop(self) -> None: ...
    def last_processed_block(self) -> int | None: ...
    def set_last_processed_block(self, block: int) -> None: ...

    # ── Solve / pool counts / state sync (test + backfill seams). ──
    def solve_all_paths(self, block_number: int) -> None: ...
    def set_event_buffer_max_age(self, max_age: int | None) -> None: ...
    def flush_event_buffer(self) -> None: ...
    def v2_pool_count(self) -> int: ...
    def v3_pool_count(self) -> int: ...
    def v4_pool_count(self) -> int: ...
    def path_count(self) -> int: ...
    def sync_v3_pool_states(
        self,
        v3_sync_updates: list[tuple[str, int, int, int, dict[int, tuple[int, int]]]],
        block_number: int,
    ) -> None: ...
    def sync_v4_pool_states(
        self,
        v4_sync_updates: list[tuple[str, str, int, int, int, dict[int, tuple[int, int]]]],
        block_number: int,
    ) -> None: ...

    # ── Backfill/pump buffer drain (restores what the d65c43f6 bypass dropped). ──
    def apply_buffer_v3(self, pool_address: str) -> None: ...
    def apply_buffer_v4(self, pool_manager: str, pool_id_hex: str) -> None: ...

    # ── Pool-registration lifecycle FSM (6N7XVR: Quarantined→Live). ──
    # set_quarantined at register-start (before the first RPC await);
    # set_live after step-2 post-drain verify passes.
    def set_v3_pool_quarantined(self, pool_address: str) -> None: ...
    def set_v4_pool_quarantined(self, pool_manager: str, pool_id_hex: str) -> None: ...
    def set_v3_pool_live(self, pool_address: str) -> None: ...
    def set_v4_pool_live(self, pool_manager: str, pool_id_hex: str) -> None: ...
    def release_all_v3_v4_quarantined(self) -> None: ...
    def debug_buffer_v3_liquidity_update(
        self,
        pool_address: str,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
        block_number: int,
    ) -> None: ...
    def debug_v3_buffer_count(self, pool_address: str) -> int: ...
    def debug_v3_tick_data(self, pool_address: str) -> dict[int, tuple[int, int]] | None: ...

    # ── Result channel (path inspection + async result-batch iteration). ──
    def latest_results(
        self,
    ) -> tuple[list[tuple[int, int, int, list[int], list[int]]], int]: ...
    def inspect_path(self, path_id: int) -> dict[str, Any] | None: ...
    def deregister_path(self, path_id: int) -> bool: ...
    def set_profit_thresholds(
        self,
        min_profit: int,
        max_profit: int | None = None,
    ) -> None: ...
    def diag_v2_pool(self, address_hex: str) -> tuple[int, str, str] | None: ...
    def diagnostic_inspect_path(
        self,
        path_id: int,
        rpc_url: str | None = None,
    ) -> dict[str, Any]: ...
    def block_stream(self) -> BlockStream: ...
    def __aiter__(self) -> ArbitrageEngine: ...
    def __anext__(self) -> Coroutine[Any, Any, dict[str, Any]]: ...

    # ── Verify config (consumer-safe: nothing emits before resume). ──
    def set_verify_rpc_url(self, rpc_url: str) -> None: ...
    def set_verify_state_view(self, state_view_address: str) -> None: ...
    def verify_v3_pool(
        self,
        address: str,
        rpc_url: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def run_v3_registration_lifecycle(
        self,
        address: str,
        snapshot_block: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def run_v4_registration_lifecycle(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        snapshot_block: int | None,
    ) -> Coroutine[Any, Any, None]: ...
    def verify_v4_pool(
        self,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> Coroutine[Any, Any, None]: ...

    # ── Pool + path registration ──
    def register_path(self, pool_refs: list[tuple[int, bool]]) -> int: ...
    def register_and_solve_path(self, pool_refs: list[tuple[int, bool]]) -> int: ...

class BlockStream:
    """Async iterator over `newHeads` block notifications from the pump.

    The authoritative block clock for the settlement-arbitrage bot (epic 6W35AI) —
    obtained via `ArbitrageEngine.block_stream()`. Yields one dict per
    accepted block header: `number`, `timestamp`, `base_fee_per_gas`
    (int | None), `gas_used`, `gas_limit`. Raises `StopAsyncIteration` when
    the channel closes (pump stopped).
    """

    def __aiter__(self) -> BlockStream: ...
    def __anext__(self) -> Coroutine[Any, Any, dict[str, Any]]: ...

class VerificationMismatchError(RuntimeError):
    """On-chain verification mismatch: engine tick data != on-chain state.

    Raised by the Rust seam (`VerifyError::Snapshot`) when
    ``verify_on_register`` finds the engine's tick data does not match
    on-chain. Fatal at the bot level — do not trade on stale tick data.
    Subclasses ``RuntimeError`` so broad ``except RuntimeError`` handlers
    still catch it; classify by ``isinstance`` for the fatal path.
    """

class VerificationRpcError(RuntimeError):
    """RPC/transport failure during on-chain verification.

    Covers provider construction failure, etc. Raised by the Rust seam
    (`VerifyError::Provider`). The bot could not reach the node to verify —
    also not safe to silently skip (an unverifiable pool is no safer than a
    mismatched one), but a distinct type so the caller can choose
    retry/backoff vs abort. Subclasses ``RuntimeError``.
    """

class PoolRegistrationError(ValueError):
    """Pool registration was refused at admission time.

    The unified base of the F2EVV6 hierarchy — a pool was rejected at
    ``register_vx_pool`` for one of five reasons (duplicate address, out-of-spec
    field, V4 amount-modifying hook, V4 dynamic fee, or V4 high static fee)
    and the specific reason is conveyed by the subclass:

    - :class:`PoolAlreadyRegisteredError` — V2/V3/V4 duplicate address.
    - :class:`SpecViolationError` — V2/V3/V4 out-of-spec field.
    - :class:`HookedPoolRejectedError` — V4 amount-modifying hook.
    - :class:`DynamicFeePoolRejectedError` — V4 dynamic fee.
    - :class:`HighFeePoolRejectedError` — V4 static fee > 65535.

    Subclasses ``ValueError`` so broad ``except ValueError:`` handlers (which
    skip one rejected pool at a time in ``build_paths``) keep working; scope
    admission refusals specifically with ``except PoolRegistrationError:``.
    """

class PoolAlreadyRegisteredError(PoolRegistrationError):
    """A pool at this address is already registered.

    Raised by the Rust seam (``RegisterV{2,3}PoolError::AlreadyRegistered``
    / ``RegisterV4PoolError::AlreadyRegistered``) when ``register_vx_pool``
    sees a duplicate address. Replaces the previous ``assert!`` panic on V2
    (MSTAT2) and V3 (24KNGF), and replaces the plain ``ValueError`` that
    ``register_v4_pool`` previously raised for duplicates (F2EVV6). Subclasses
    :class:`PoolRegistrationError` so a broad admission catch covers all four
    admission categories together.
    """

class SpecViolationError(PoolRegistrationError):
    """A field on the pool registration params violates its on-chain bound.

    Raised by the Rust seam when ``register_vx_pool`` sees an out-of-spec
    field — e.g. V2 ``reserve{0,1} > uint112(-1)``, V3/V4 ``sqrtPriceX96``
    outside ``[MIN_SQRT_RATIO, MAX_SQRT_RATIO)``, V3/V4 ``tick`` outside
    ``[MIN_TICK, MAX_TICK]``, V3/V4 ``fee`` above the family bound
    (V3 ``< 1_000_000``; V4 static-fee ``< 1 << 24``), or V3/V4
    ``tickSpacing`` outside ``[1, 32_767]``. The message names the offending
    field, its value, and the bound it violates. Replaces the
    ``PyValueError("Vx pool registration failed: …")`` stop-gap mappers
    (MSTAT2 / 24KNGF / K3IICB) with a typed exception in F2EVV6.
    """

class HookedPoolRejectedError(PoolRegistrationError):
    """A V4 pool with an amount-modifying hook was rejected at registration.

    Raised by the Rust seam (`RegisterV4PoolError::HookedPool`) when
    ``register_v4_pool`` sees ``hook_flags & 0xCC != 0``. The solver's V3-CL
    math assumes no hook intervention, so a hooked pool would produce phantom
    profits — admission is a *correctness floor* (enforced in the Rust core
    so a standalone Rust consumer is protected, per ADR-005). Reparented
    under :class:`PoolRegistrationError` in F2EVV6 (was directly under
    ``ValueError`` in Plan 102); a broad ``except ValueError:`` still catches
    it; classify by ``isinstance``.
    """

class DynamicFeePoolRejectedError(PoolRegistrationError):
    """A V4 pool with a dynamic fee was rejected at registration.

    Raised by the Rust seam (`RegisterV4PoolError::DynamicFee`) when
    ``register_v4_pool`` sees ``fee == 0x100000``. The solver assumes a fixed
    fee, so a dynamic-fee pool cannot be priced. Like
    :class:`HookedPoolRejectedError`, a correctness floor enforced in the
    Rust core (ADR-005) and reparented under :class:`PoolRegistrationError`
    in F2EVV6.
    """

class HighFeePoolRejectedError(PoolRegistrationError):
    """A V4 pool whose static fee exceeds the executor's encoding limit.

    Raised by the Rust seam
    (``RegisterV4PoolError::FeeExceedsEncoderLimit``) when
    ``register_v4_pool`` sees a static ``fee > 65535`` (``u16::MAX``).
    The cmd_executor encodes V4 ``fee`` as a 2-byte field in both
    ``V4_SWAP_COMPACT`` and ``V4_SWAP_DYNAMIC`` (the contract masks
    ``& 65535``), so any static fee above 65535 is un-encodable. Such fees
    are also unprofitable (32%+ per swap). Rejected at admission (ergo
    DPODAZ) so the pool never enters the path graph and wastes a
    solve + ``encode-failed`` cycle.
    """

# ------------------------------------------------------------------
# Structural pool handle + read-only state views (feature = "bot").
# Thin PyO3 handles over the Rust BotState pool entries; all state lives
# in Rust. Pool is the structural (not identity-based) handle that
# mirrors the Rust degenbot_pools::Pool handle.
# ------------------------------------------------------------------
class Pool:
    """Structural pool handle mirroring the Rust degenbot_pools::Pool."""

    def structure(self) -> str: ...
    def identity(self) -> tuple[str, str | None]: ...
    @property
    def dex_name(self) -> str | None: ...
    def reserve_pair(self) -> ReservePairView: ...
    def concentrated_liquidity(self) -> ConcentratedLiquidityView: ...
    def balance_vector(self) -> BalanceVectorView: ...

class ReservePairView:
    """Read-only V2 reserve-pair view (token0/1 + reserve0/1)."""

    @property
    def token0(self) -> str: ...
    @property
    def token1(self) -> str: ...
    @property
    def reserve0(self) -> int: ...
    @property
    def reserve1(self) -> int: ...

class ConcentratedLiquidityView:
    """Read-only V3/V4 concentrated-liquidity slot0 view."""

    @property
    def token0(self) -> str: ...
    @property
    def token1(self) -> str: ...
    @property
    def fee(self) -> int: ...
    @property
    def tick_spacing(self) -> int: ...
    @property
    def sqrt_price_x96(self) -> int: ...
    @property
    def liquidity(self) -> int: ...
    @property
    def tick(self) -> int: ...

class BalanceVectorView:
    """Read-only multi-token balance vector (Balancer family)."""

    @property
    def tokens(self) -> list[str]: ...
    @property
    def balances(self) -> list[int]: ...
    @property
    def n_tokens(self) -> int: ...

# ------------------------------------------------------------------
# Drainer lifecycle pyfunctions (top-level, module-lifecycle). Registered
# outside c_api::register: shutdown_log_drainer by the tracing log layer
# (python_log_layer::register_pyfunction, called from the module init)
# and shutdown_subscriber_drainer by the pub/sub subscriber seam.
# Both are idempotent; call before interpreter finalization.
# ------------------------------------------------------------------
def shutdown_log_drainer() -> None:
    """Flush + stop the batched Python log drainer thread (idempotent)."""

def shutdown_subscriber_drainer() -> None:
    """Stop the pool-state subscriber drainer thread (idempotent)."""

# ------------------------------------------------------------------
# Simulation seam (per-block profitability pipeline)
# ------------------------------------------------------------------

# ------------------------------------------------------------------
# QuantAMM closed-form N-token Balancer weighted basket solver (feature = "bot").
# Thin PyO3 wrapper over `degenbot_bot::solvers::balancer_weighted_basket`.
# ------------------------------------------------------------------
def solve_balancer_weighted_basket(
    reserves: list[int],
    weights: list[int],
    fee_numer: int,
    fee_denom: int,
    decimals: list[int],
    market_prices: list[float],
    max_input: float | None = None,
) -> tuple[list[int], float, bool, list[int], int]:
    """Closed-form N-token Balancer weighted basket arbitrage (QuantAMM Eq. 9).

    Returns ``(trades, profit, success, signature, iterations)``. ``trades``
    are integer native-token amounts (positive = deposit, negative =
    withdraw). FFI boundary over the Rust core
    `degenbot-bot::solvers::balancer_weighted_basket::solve_balancer_weighted`;
    see `degenbot.arbitrage.solve_balancer_weighted_basket` for the stable
    re-export (ADR-013).
    """

__all__ = [
    "ArbitrageEngine",
    "BalanceVectorView",
    "BlockStream",
    "Bot",
    "BotIo",
    "ConcentratedLiquidityView",
    "DynamicFeePoolRejectedError",
    "Erc20Token",
    "Erc20TokenRow",
    "HighFeePoolRejectedError",
    "HookedPoolRejectedError",
    "LiquidityPool",
    "PathIterator",
    "Pool",
    "PoolAlreadyRegisteredError",
    "PoolRegistrationError",
    "ReservePairView",
    "SpecViolationError",
    "VerificationMismatchError",
    "VerificationRpcError",
    "aave",
    "abi",
    "balancer_math",
    "build_path_graph",
    "cancel",
    "compute_aerodrome_v2_pool_address",
    "compute_aerodrome_v3_pool_address",
    "concentrated_liquidity_math",
    "contract",
    "curve_dy",
    "curve_math",
    "db",
    "deployments",
    "dex_identity",
    "diagnostics",
    "execution",
    "executor",
    "find_paths_rust",
    "fork",
    "pool",
    "price",
    "provider",
    "shutdown_log_drainer",
    "shutdown_subscriber_drainer",
    "simulation",
    "solady",
    "solidly_math",
    "solve_balancer_weighted_basket",
    "submission",
    "subscriber",
    "to_checksum_address",
]
