"""Record/replay golden harness for whole-pool state (T0).

On-chain pool-math tests build a pool at a pinned block and assert against it.
The fork is only a *static data source* — the assertions are fixed, so the zero
inputs the pool needs (immutables + full state) can be captured once in record
mode and replayed offline with **no RPC** and **no anvil**.

This is the construction-state analogue of :mod:`tests.golden.oracle`
(``GoldenOracle``), which records a single contract-call result rather than a
whole pool. In record mode the caller hands us a live fork-built pool and we
serialize its constructor inputs; in replay mode we rebuild an I/O-free pool via
the ``tests/helpers/{v2,v3,v4}_pool_factory`` helpers.

Supported families: Uniswap v2, v3 and v4.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import UTC, datetime
from fractions import Fraction
from typing import TYPE_CHECKING, Any

from degenbot.bot import RustBot
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    from pathlib import Path

_POOL_SCHEMA_VERSION = 1

_CHAIN_MISMATCH_MSG = (
    "pool golden file {path} recorded for chain_id {recorded} but test binds {bound}"
)
_BLOCK_MISMATCH_MSG = (
    "pool golden file {path} recorded at block {recorded} but test binds {bound}; "
    "re-record with --golden-mode=record against a fork pinned to block {block}"
)


class PoolGoldenError(AssertionError):
    """Raised when a recorded-pool golden entry is missing or the harness is mis-used."""


def _token_dict(token: Any) -> dict[str, Any]:
    return {
        "address": token.address,
        "name": token.name,
        "symbol": token.symbol,
        "decimals": token.decimals,
        "chain_id": token.chain_id,
    }


def _tick_data_dict(tick_data: dict[int, Any] | None) -> dict[str, Any]:
    """Serialize ``{tick: LiquidityAtTick | dict}`` to plain JSON dicts."""
    out: dict[str, Any] = {}
    if not tick_data:
        return out
    for tick, info in tick_data.items():
        if isinstance(info, dict):
            out[str(int(tick))] = {
                "liquidity_gross": int(info["liquidity_gross"]),
                "liquidity_net": int(info["liquidity_net"]),
                "block": int(info.get("block", 0)),
            }
        else:
            out[str(int(tick))] = {
                "liquidity_gross": int(info.liquidity_gross),
                "liquidity_net": int(info.liquidity_net),
                "block": int(info.block),
            }
    return out


def _tick_bitmap_dict(tick_bitmap: dict[int, Any] | None) -> dict[str, Any]:
    out: dict[str, Any] = {}
    if not tick_bitmap:
        return out
    for word, info in tick_bitmap.items():
        if isinstance(info, dict):
            out[str(int(word))] = {
                "bitmap": int(info["bitmap"]),
                "block": int(info.get("block", 0)),
            }
        else:
            out[str(int(word))] = {"bitmap": int(info.bitmap), "block": int(info.block)}
    return out


def _family_of(pool: Any) -> str:
    if hasattr(pool, "pool_id"):
        return "v4"
    if hasattr(pool, "tick") and hasattr(pool, "tick_spacing"):
        return "v3"
    return "v2"


def extract_pool_state(pool: Any, *, block: int) -> dict[str, Any]:
    """Serialize a live pool's constructor inputs to a plain-JSON dict."""
    family = _family_of(pool)
    base = {
        "schema": _POOL_SCHEMA_VERSION,
        "family": family,
        "block": int(block),
        "address": pool.address,
        "chain_id": (pool.token0.chain_id if hasattr(pool, "token0") else 1),
    }
    tokens = [_token_dict(pool.token0), _token_dict(pool.token1)]

    if family == "v2":
        return {
            **base,
            "tokens": tokens,
            "factory": pool.factory,
            "init_hash": getattr(pool, "init_hash", None),
            "fee_token0": str(pool.fee_token0),
            "fee_token1": str(pool.fee_token1),
            "reserves_token0": int(pool.reserves_token0),
            "reserves_token1": int(pool.reserves_token1),
        }

    if family == "v3":
        return {
            **base,
            "tokens": tokens,
            "factory": pool.factory,
            "fee": int(pool.fee),
            "tick_spacing": int(pool.tick_spacing),
            "sqrt_price_x96": int(pool.sqrt_price_x96),
            "tick": int(pool.tick),
            "liquidity": int(pool.liquidity),
            "tick_data": _tick_data_dict(getattr(pool, "tick_data", None)),
            "tick_bitmap": _tick_bitmap_dict(getattr(pool, "tick_bitmap", None)),
        }

    # v4
    return {
        **base,
        "pool_id": pool.pool_id.hex(),
        "pool_manager_address": pool.address,
        "tokens": tokens,
        "fee": int(pool.fee),
        "tick_spacing": int(pool.tick_spacing),
        "hook_address": getattr(pool, "hook_address", None),
        "state_view_address": getattr(pool, "state_view_address", None),
        "sqrt_price_x96": int(pool.sqrt_price_x96),
        "tick": int(pool.tick),
        "liquidity": int(pool.liquidity),
        "protocol_fee_zero_for_one": int(getattr(pool, "protocol_fee_zero_for_one", 0)),
        "protocol_fee_one_for_zero": int(getattr(pool, "protocol_fee_one_for_zero", 0)),
        "lp_fee": int(getattr(pool, "lp_fee", 0)),
        "tick_data": _tick_data_dict(getattr(pool, "tick_data", None)),
        "tick_bitmap": _tick_bitmap_dict(getattr(pool, "tick_bitmap", None)),
    }


def _make_token(tok: dict[str, Any]) -> Any:
    return make_erc20(
        RustBot(),
        tok["address"],
        name=tok["name"],
        symbol=tok["symbol"],
        decimals=tok["decimals"],
        chain_id=tok["chain_id"],
    )


def reconstruct_pool(state: dict[str, Any]) -> Any:
    """Rebuild an I/O-free pool from a recorded state dict (no RPC, no anvil)."""
    if state.get("schema") != _POOL_SCHEMA_VERSION:
        msg = f"unsupported pool golden schema {state.get('schema')!r} in {state!r}"
        raise PoolGoldenError(msg)

    family = state["family"]
    block: int = state["block"]
    token0 = _make_token(state["tokens"][0])
    token1 = _make_token(state["tokens"][1])

    if family == "v2":
        from tests.helpers.v2_pool_factory import make_v2_pool

        return make_v2_pool(
            state["address"],
            token0=token0,
            token1=token1,
            factory=state["factory"],
            init_hash=state.get("init_hash"),
            fee_token0=Fraction(state["fee_token0"]),
            fee_token1=Fraction(state["fee_token1"]),
            reserves_token0=state["reserves_token0"],
            reserves_token1=state["reserves_token1"],
            chain_id=state["chain_id"],
            state_block=block,
        )

    if family == "v3":
        from tests.helpers.v3_pool_factory import make_v3_pool

        pool = make_v3_pool(
            state["address"],
            token0=token0,
            token1=token1,
            factory=state["factory"],
            fee=state["fee"],
            tick_spacing=state["tick_spacing"],
            sqrt_price_x96=state["sqrt_price_x96"],
            tick=state["tick"],
            liquidity=state["liquidity"],
            state_block=block,
            tick_data=state.get("tick_data"),
        )
        # ``make_v3_pool`` seeds tick_data but not the Rust bitmap, so swap
        # simulation can't tell where liquidity ends (swap-for-all /
        # IncompleteSwap). ``update_tick_data`` seeds the Rust bitmap AND the
        # companion override from the recorded words, restoring fork parity.
        tick_data = state.get("tick_data")
        tick_bitmap = state.get("tick_bitmap")
        if tick_bitmap or tick_data:
            pool.update_tick_data(
                tick_bitmap={int(word): info for word, info in (tick_bitmap or {}).items()},
                tick_data=({int(t): v for t, v in tick_data.items()} if tick_data else {}),
                block=block,
            )
        return pool

    if family == "v4":
        from tests.helpers.v4_pool_factory import make_v4_pool

        # make_v4_pool expects ``{tick: (gross, net, block)}`` tuples, not the
        # plain-dict form we persist.
        tick_data = state.get("tick_data")
        v4_tick_data = (
            {
                int(t): (v["liquidity_gross"], v["liquidity_net"], v["block"])
                for t, v in tick_data.items()
            }
            if tick_data
            else None
        )

        return make_v4_pool(
            pool_id=state["pool_id"],
            pool_manager_address=state["pool_manager_address"],
            token0=token0,
            token1=token1,
            fee=state["fee"],
            tick_spacing=state["tick_spacing"],
            hook_address=state.get("hook_address"),
            state_view_address=state.get("state_view_address"),
            sqrt_price_x96=state["sqrt_price_x96"],
            tick=state["tick"],
            liquidity=state["liquidity"],
            protocol_fee_zero_for_one=state.get("protocol_fee_zero_for_one", 0),
            protocol_fee_one_for_zero=state.get("protocol_fee_one_for_zero", 0),
            lp_fee=state.get("lp_fee", 0),
            tick_data=v4_tick_data,
            tick_bitmap=state.get("tick_bitmap"),
            state_block=block,
        )

    msg = f"unsupported pool family {family!r}"
    raise PoolGoldenError(msg)


def record_pool(pool: Any, path: Path, *, block: int) -> dict[str, Any]:
    """Persist a live pool's constructor inputs to a JSON golden file."""
    state = extract_pool_state(pool, block=block)
    payload = {
        "recorded_at": datetime.now(UTC).isoformat(timespec="seconds"),
        **state,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return state


def load_pool(path: Path, *, chain_id: int, block: int) -> Any:
    """Read a recorded-pool golden file and rebuild the I/O-free pool."""
    state = json.loads(path.read_text(encoding="utf-8"))
    if state.get("chain_id") != chain_id:
        msg = _CHAIN_MISMATCH_MSG.format(
            path=path,
            recorded=state.get("chain_id"),
            bound=chain_id,
        )
        raise PoolGoldenError(msg)
    if state.get("block") != block:
        msg = _BLOCK_MISMATCH_MSG.format(
            path=path,
            recorded=state.get("block"),
            bound=block,
            block=block,
        )
        raise PoolGoldenError(msg)
    return reconstruct_pool(state)


@dataclass
class RecordedPool:
    """Bind a pool golden file to a (chain_id, block) and record/replay it.

    Mirrors :class:`tests.golden.oracle.GoldenOracle`'s bind/record/replay shape
    so conversion tasks can use ``golden_factory``-style ergonomics.
    """

    path: Path
    chain_id: int
    block: int
    mode: str

    @property
    def is_recording(self) -> bool:
        return self.mode == "record"

    def record(self, pool: Any) -> None:
        """Record the fork-built ``pool``'s state (record mode only)."""
        record_pool(pool, self.path, block=self.block)

    def load(self) -> Any:
        """Return an I/O-free pool rebuilt from the golden file (replay mode)."""
        if not self.path.exists():
            msg = (
                f"no recorded pool golden at {self.path}; "
                f"re-record with --golden-mode=record against a fork pinned to block {self.block}"
            )
            raise PoolGoldenError(msg)
        return load_pool(self.path, chain_id=self.chain_id, block=self.block)
