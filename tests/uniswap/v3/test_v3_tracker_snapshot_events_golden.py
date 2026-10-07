"""Uniswap V3 tracker snapshot-event application — golden record/replay.

Golden conversion of the two retired dir-snapshot tests
(`test_apply_update_to_snapshot` and
`test_pool_manager_applies_snapshot_from_dir`), which pinned the pre-builder-
retirement behavior of "drained snapshot tick maps seed the built pool." That
behavior died with the Python V3 builder (the tick maps passed to
`build_pool` were silently discarded); the tracker contract that survived is
the one this test pins:

    A `UniswapV3PoolTracker` built with `snapshot=` applies the snapshot's
    pending Mint/Burn events to a freshly-built pool via
    `_apply_pending_liquidity_updates` → `pool.update_liquidity_map`.

Scope note: `tracker.get_pool()` itself delegates to `build_pool`, whose
delegated core choreography is RPC-shaped end to end — that path is covered by
the parity suites' build contracts and is out of replay scope here. What this
test pins is the tracker's apply step (the seam that survived the builder
retirement): pool + recorded events in, chain-truth state out.

What is golden: the pool's on-chain tick state at two consecutive pinned
blocks plus the real Mint/Burn events between them. Record mode captures both
against a live archive fork; replay mode rebuilds the pool I/O-free from the
recorded cassette, feeds the recorded events through the tracker's apply
step, and asserts the application lands the exact recorded state — with a
hard dial-block so the replay is offline by contract (the same convention as
the onchain-parity suites).

- **Replay mode** (default, CI): reads the cassette + golden entry; no fork
  is created, no RPC is issued, any dial is a defect.
- **Record mode** (`--golden-mode=record`): one Anvil fork of Ethereum
  mainnet pinned to the event block walks the pool's Mint/Burn logs and the
  full tick state, writing the cassette. The test's own `assert` runs too —
  a record run is also a live parity gate.

Pinned to Ethereum mainnet blocks 24,407,814 (snapshot) and 24,407,815 (the
event block — three real WBTC/WETH Mint/Burn events, no swaps in the span so
the tracker-applied state is pure Mint/Burn arithmetic), served by a local
archive node (URI configurable via `tests.env`; see
`ETHEREUM_ARCHIVE_NODE_HTTP_URI`).
"""

from __future__ import annotations

import json
import pathlib
import urllib.request
from contextlib import AbstractContextManager
from typing import Any, Self

import pytest

from degenbot import abi_decode
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v3_types import UniswapV3LiquidityEvent
from degenbot.updater.pool_updater_configs import (
    UNISWAP_V3_BURN_EVENT_HASH,
    UNISWAP_V3_MINT_EVENT_HASH,
)

# RPC URI is overridable via tests.env (see ETHEREUM_ARCHIVE_NODE_HTTP_URI);
# only contacted in record mode — replay is fully offline.
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.golden.oracle import (
    assert_golden_keys_exact,
    replay_makes_no_network_calls,  # ruff: ignore[unused-import]
)
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

TRACKER_SNAPSHOT_BLOCK = 24_407_814  # pool state: the block before the events
TRACKER_EVENT_BLOCK = 24_407_815  # three WBTC/WETH Mint/Burn events, no swaps

WBTC_WETH_V3_POOL_ADDRESS = get_checksum_address(
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
)
UNISWAP_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x1F98431c8aD98523631AE4a59f267346ea31F984",
)
_WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
_WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

# Self-contained cassette pair: the pool's full state at the snapshot block,
# then the events + resulting state at the event block. Recorded together in
# one record-mode run — never hand-edited.
_CASSETTE_DIR = pathlib.Path(__file__).resolve().parents[2] / "fixtures" / "chain_data" / "1"
_SNAPSHOT_CASSETTE = _CASSETTE_DIR / "uniswap_v3_wbtc_weth_tracker_snapshot_block_24407814.json"
_EVENT_CASSETTE = _CASSETTE_DIR / "uniswap_v3_wbtc_weth_tracker_events_24407814_24407815.json"

_GOLDEN_TEST_NAME = "test_tracker_applies_pending_snapshot_events"

# Swap event topic (verified against the chain): a swap that crosses a tick
# changes the active-liquidity scalar without touching the tick maps, so the
# record run pins only swap-free event blocks.
_UNISWAP_V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"


class _RecordFork(AbstractContextManager):
    """One pinned Ethereum fork shared across the test's whole record pass.

    In replay `fork` stays `None` (no fork is created); no chain access
    happens at all — the cassette and golden file carry everything.
    """

    def __init__(self, *, recording: bool) -> None:
        self._recording = recording
        self.fork: AnvilFork | None = None

    def __enter__(self) -> Self:
        if self._recording:
            self.fork = AnvilFork(
                fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
                # Pinned AT the event block: anvil serves every block up to
                # the pin, so both the snapshot block (one before) and the
                # event block itself are readable.
                fork_block=TRACKER_EVENT_BLOCK,
                launch=ForkLaunchConfig(
                    storage_caching=True,
                    anvil_opts=["--accounts=0"],
                ),
            )
        return self

    def __exit__(self, *exc: object) -> None:
        if self.fork is not None:
            self.fork.close()


def _build_wbtc_weth_v3_io_free(cassette: dict[str, Any]) -> UniswapV3Pool:
    """Build the WBTC/WETH V3 pool I/O-free from a full tick-state cassette."""
    scalars = cassette["scalars"]
    # ``make_v3_pool`` normalizes dict-shaped tick entries itself.
    tick_data = cassette["tick_data"]
    py_bot = Bot()
    wbtc = make_erc20(
        py_bot,
        _WBTC_ADDRESS,
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )
    weth = make_erc20(
        py_bot,
        _WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )
    return make_v3_pool(
        WBTC_WETH_V3_POOL_ADDRESS,
        token0=wbtc,
        token1=weth,
        factory=UNISWAP_V3_FACTORY_ADDRESS,
        fee=scalars["fee"],
        tick_spacing=scalars["tick_spacing"],
        sqrt_price_x96=scalars["sqrt_price_x96"],
        tick=scalars["tick"],
        liquidity=scalars["liquidity"],
        state_block=cassette["block"],
        tick_data=tick_data,
        pool_class=UniswapV3Pool,
    )


def _assert_tick_state(pool: UniswapV3Pool, expected: dict[str, Any], *, label: str) -> None:
    """Assert the pool's tick map equals the recorded chain truth."""
    # JSON keys are strings; normalize to the pool's int tick keys.
    expected_ticks = {int(tick): info for tick, info in expected["tick_data"].items()}
    expected_liquidity = int(expected["liquidity"])
    assert set(pool.tick_data) == set(expected_ticks), (
        f"{label}: tick keys diverged: "
        f"helper={sorted(pool.tick_data)[:5]}... chain={sorted(expected_ticks)[:5]}..."
    )
    for tick, info in expected_ticks.items():
        actual = pool.tick_data[tick]
        assert actual.liquidity_net == info["liquidity_net"], (
            f"{label}: tick {tick} liquidity_net helper={actual.liquidity_net} "
            f"chain={info['liquidity_net']}"
        )
        assert actual.liquidity_gross == info["liquidity_gross"], (
            f"{label}: tick {tick} liquidity_gross helper={actual.liquidity_gross} "
            f"chain={info['liquidity_gross']}"
        )
    assert pool.liquidity == expected_liquidity, (
        f"{label}: active liquidity helper={pool.liquidity} chain={expected_liquidity}"
    )


@pytest.mark.ethereum
@pytest.mark.onchain_oracle
def test_tracker_applies_pending_snapshot_events(golden_factory) -> None:
    """Tracker-applied event state == the chain's state at the event block.

    Record mode walks the pool's Mint/Burn logs between the two pinned
    blocks, captures the chain's tick state at the event block, and records
    both into a chain_data cassette plus the golden entry. Replay mode
    rebuilds the pool I/O-free, feeds the recorded events through the
    tracker's apply step, and asserts the exact recorded state.
    """
    golden = golden_factory(chain_id=1, block_number=TRACKER_EVENT_BLOCK)

    with _RecordFork(recording=golden.is_recording) as ctx:
        if golden.is_recording:
            assert ctx.fork is not None
            _record_cassette(ctx.fork)

        # -- replay path: everything from disk, no dial ---------------------
        snapshot_cassette = _load_snapshot_cassette()
        events, expected_state = _load_event_cassette()

        # The golden entry is the cassette's expected state, checked through
        # the oracle so the golden file carries the chain-truth too (and the
        # key-set gate below pins that surface). In record mode the oracle
        # callable re-reads the freshly written cassette — no RPC here; the
        # fork work happened above in _record_cassette.
        oracle = golden.check(
            "tick_state",
            contract=lambda: _load_event_cassette()[1],
        )
        assert oracle.ok
        assert expected_state == oracle.value

        pool = _build_wbtc_weth_v3_io_free(snapshot_cassette)

        # The tracker drives the pool's pending-event application: the events
        # are queued into the snapshot exactly as the facade's event queue
        # holds them (raw Mint/Burn records, as the runner streams them), then
        # the apply step consumes them through `update_liquidity_map` — the
        # surviving tracker contract under test.
        bot = Bot(database=":memory:")
        snapshot = _snapshot_with_events(events)
        tracker = UniswapV3PoolTracker(
            factory_address=UNISWAP_V3_FACTORY_ADDRESS,
            bot=bot,
            snapshot=snapshot,
        )
        tracker._apply_pending_liquidity_updates(pool)

        _assert_tick_state(pool, expected_state, label="tracker-applied")


def _snapshot_with_events(events: list[dict[str, Any]]) -> UniswapV3LiquiditySnapshot:
    """Build a snapshot whose pending-event queue holds the recorded events."""
    snapshot = UniswapV3LiquiditySnapshot(
        source=_MinimalSnapshotSource(chain_id=1, pools={WBTC_WETH_V3_POOL_ADDRESS}),
    )
    queue = snapshot._liquidity_events[WBTC_WETH_V3_POOL_ADDRESS]
    queue.extend(
        UniswapV3LiquidityEvent(
            block_number=ev["block_number"],
            liquidity=ev["liquidity"],
            tick_lower=ev["tick_lower"],
            tick_upper=ev["tick_upper"],
            tx_index=ev["tx_index"],
            log_index=ev["log_index"],
        )
        for ev in events
    )
    return snapshot


class _MinimalSnapshotSource:
    """The smallest source the snapshot facade accepts (hermetic)."""

    storage_kind = "memory"

    def __init__(self, *, chain_id: int, pools: set[str]) -> None:
        self.chain_id = chain_id
        self._pools = pools
        self._newest_block = TRACKER_EVENT_BLOCK

    def get_newest_block(self) -> int:
        return self._newest_block

    def get_pools(self) -> set[str]:
        return set(self._pools)

    def get_liquidity_map(self, pool_address: str):
        return None


def _load_snapshot_cassette() -> dict[str, Any]:
    """Load the parity suite's snapshot-block pool cassette."""
    return json.loads(_SNAPSHOT_CASSETTE.read_bytes())


def _load_event_cassette() -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Load the recorded events + expected post-application state."""
    cassette = json.loads(_EVENT_CASSETTE.read_bytes())
    return cassette["events"], cassette["expected_state"]


def _record_cassette(fork: AnvilFork) -> None:
    """Record both cassettes against the live fork (record mode only)."""
    events = _fetch_pool_liquidity_events(fork)
    if not events:
        msg = (
            f"no Mint/Burn events for {WBTC_WETH_V3_POOL_ADDRESS} between blocks "
            f"{TRACKER_SNAPSHOT_BLOCK + 1} and {TRACKER_EVENT_BLOCK}; re-pin the "
            "blocks to a span with real liquidity events — an empty event list "
            "would degenerate this test to an untouched-pool no-op"
        )
        raise AssertionError(msg)
    # A swap in the event block changes the active-liquidity scalar by
    # crossing ticks — something Mint/Burn events alone cannot reproduce —
    # so the record run refuses to pin a swapped span.
    swaps = _fetch_logs(
        fork,
        address=WBTC_WETH_V3_POOL_ADDRESS,
        from_block=TRACKER_EVENT_BLOCK,
        to_block=TRACKER_EVENT_BLOCK,
        topics=[_UNISWAP_V3_SWAP_TOPIC],
    )
    if swaps:
        msg = (
            f"a swap in block {TRACKER_EVENT_BLOCK} changes the active-liquidity "
            "scalar beyond what Mint/Burn events express; re-pin to a swap-free "
            "event block"
        )
        raise AssertionError(msg)

    snapshot_state = _fetch_pool_tick_state(fork, TRACKER_SNAPSHOT_BLOCK)
    snapshot_payload = {
        "format": "degenbot.chain-data/v1",
        "chain_id": 1,
        "block": TRACKER_SNAPSHOT_BLOCK,
        "pool": WBTC_WETH_V3_POOL_ADDRESS,
        "scalars": {
            "fee": 3000,
            "tick_spacing": 60,
            "sqrt_price_x96": snapshot_state["sqrt_price_x96"],
            "tick": snapshot_state["tick"],
            "liquidity": snapshot_state["liquidity"],
        },
        "tick_data": snapshot_state["tick_data"],
    }
    _SNAPSHOT_CASSETTE.write_text(
        json.dumps(snapshot_payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    expected_state = _fetch_pool_tick_state(fork, TRACKER_EVENT_BLOCK)
    event_payload = {
        "format": "degenbot.chain-data/v1",
        "chain_id": 1,
        "block_number": TRACKER_EVENT_BLOCK,
        "pool": WBTC_WETH_V3_POOL_ADDRESS,
        "events": events,
        "expected_state": expected_state,
    }
    _EVENT_CASSETTE.write_text(
        json.dumps(event_payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def _fetch_pool_liquidity_events(fork: AnvilFork) -> list[dict[str, Any]]:
    """Fetch + decode the pool's Mint/Burn events between the two pinned blocks.

    Uses the sanctioned decode from the updater
    (`degenbot.updater.pool_updater_configs`): ticks come from the indexed
    topics, the liquidity delta from the event's own `uint128` amount word
    (negated for Burn).
    """
    logs = _fetch_logs(
        fork,
        address=WBTC_WETH_V3_POOL_ADDRESS,
        from_block=TRACKER_SNAPSHOT_BLOCK + 1,
        to_block=TRACKER_EVENT_BLOCK,
        topics=[
            [
                "0x" + UNISWAP_V3_MINT_EVENT_HASH.hex(),
                "0x" + UNISWAP_V3_BURN_EVENT_HASH.hex(),
            ],
        ],
    )
    events: list[dict[str, Any]] = []
    for log in logs:
        (tick_lower,) = abi_decode(["int24"], log["topics"][2])
        (tick_upper,) = abi_decode(["int24"], log["topics"][3])
        if log["topics"][0] == "0x" + UNISWAP_V3_BURN_EVENT_HASH.hex():
            (amount, _lower, _upper) = abi_decode(["uint128", "uint256", "uint256"], log["data"])
            amount = -amount
        else:
            (_sender, amount, _lower, _upper) = abi_decode(
                ["address", "uint128", "uint256", "uint256"], log["data"]
            )
        if amount == 0:
            continue
        events.append({
            "block_number": log["blockNumber"],
            "liquidity": amount,
            "tick_lower": tick_lower,
            "tick_upper": tick_upper,
            "tx_index": 0,
            "log_index": log["logIndex"],
        })
    return events


def _fetch_pool_tick_state(fork: AnvilFork, block: int) -> dict[str, Any]:
    """Capture the pool's scalars + full tick state at `block`."""
    slot0, liquidity = _fetch_slot0_and_liquidity(fork, block)
    tick_data = _fetch_all_ticks(fork, block)
    return {
        "sqrt_price_x96": slot0["sqrtPriceX96"],
        "tick": slot0["tick"],
        "liquidity": liquidity,
        "tick_data": tick_data,
    }


def _fetch_slot0_and_liquidity(fork: AnvilFork, block: int) -> tuple[dict[str, Any], int]:
    """slot0() and liquidity() raw calls at the block."""
    slot0_selector = "3850c7bd"
    liquidity_selector = "1a686502"
    slot0_raw = _eth_call(fork, WBTC_WETH_V3_POOL_ADDRESS, slot0_selector, block)
    liquidity_raw = _eth_call(fork, WBTC_WETH_V3_POOL_ADDRESS, liquidity_selector, block)
    body = slot0_raw[2:]
    sqrt_price = int(body[0:64], 16)
    tick = _signed_int(int(body[64:128], 16))
    return {"sqrtPriceX96": sqrt_price, "tick": tick}, int(liquidity_raw, base=0)


def _fetch_all_ticks(fork: AnvilFork, block: int) -> dict[str, dict[str, int]]:
    """Walk the tick bitmap and every initialized tick's data at `block`.

    Mirrors the parity cassette's full-tick walk: every initialized tick
    across every word, so the recorded state is complete rather than capped
    at the active word.
    """
    word_selector = "5339c296"  # tickBitmap(int16)
    tick_selector = "f30dba93"  # ticks(int24)

    ticks: dict[str, dict[str, int]] = {}
    spacing = 60
    for word in range(-600, 601):
        word_raw = _eth_call_int(fork, WBTC_WETH_V3_POOL_ADDRESS, word_selector, word, block)
        if not word_raw or int(word_raw, 16) == 0:
            continue
        bitmap = int(word_raw, 16)
        for bit in range(256):
            if not (bitmap >> bit) & 1:
                continue
            tick = (word * 256 + bit) * spacing
            tick_raw = _eth_call_int(fork, WBTC_WETH_V3_POOL_ADDRESS, tick_selector, tick, block)
            body = tick_raw[2:]
            gross = int(body[0:64], 16)
            net = _signed_int(int(body[64:128], 16))
            ticks[str(tick)] = {
                "liquidity_gross": gross,
                "liquidity_net": net,
                "block": block,
            }
    return ticks


def _fetch_logs(
    fork: AnvilFork,
    *,
    address: str,
    from_block: int,
    to_block: int,
    topics: list[Any],
) -> list[dict[str, Any]]:
    """Raw getLogs for the pool between the blocks."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getLogs",
        "params": [
            {
                "address": address,
                "fromBlock": hex(from_block),
                "toBlock": hex(to_block),
                "topics": topics,
            }
        ],
    }
    resp = _rpc_post(fork, payload)
    if "error" in resp:
        msg = f"getLogs failed: {resp['error']}"
        raise RuntimeError(msg)
    logs = resp["result"]
    if logs is None:
        return []
    return [
        {
            "blockNumber": int(log["blockNumber"], 16),
            "logIndex": int(log["logIndex"], 16),
            "topics": log["topics"],
            "data": log["data"],
        }
        for log in logs
    ]


def _eth_call(fork: AnvilFork, to: str, selector: str, block: int) -> str:
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{"to": to, "data": "0x" + selector}, hex(block)],
    }
    resp = _rpc_post(fork, payload)
    if "error" in resp:
        msg = f"eth_call {selector} failed: {resp['error']}"
        raise RuntimeError(msg)
    return resp["result"]


def _eth_call_int(fork: AnvilFork, to: str, selector: str, arg: int, block: int) -> str:
    """eth_call with a single int arg (padded to 32 bytes, two's complement)."""
    arg_hex = arg & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{"to": to, "data": "0x" + selector + f"{arg_hex:064x}"}, hex(block)],
    }
    resp = _rpc_post(fork, payload)
    if "error" in resp:
        msg = f"eth_call {selector}({arg}) failed: {resp['error']}"
        raise RuntimeError(msg)
    return resp["result"]


def _rpc_post(fork: AnvilFork, payload: dict[str, Any]) -> dict[str, Any]:
    """One JSON-RPC POST against the fork's HTTP endpoint."""
    req = urllib.request.Request(  # ruff: ignore[suspicious-url-open-usage]
        fork.http_url,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:  # ruff: ignore[suspicious-url-open-usage]
        return json.loads(resp.read())


def _signed_int(word: int) -> int:
    """Decode a 32-byte two's-complement word to a signed int."""
    return word - (1 << 256) if word >= (1 << 255) else word


def test_golden_keys_exactly_match_the_oracle_surface(request: pytest.FixtureRequest) -> None:
    """The golden file holds exactly the keys this module's replay drives.

    Replay fails loud on a missing key but stays silent on a stale extra one —
    nothing looks it up. Set-equality against the recorded file closes that
    drift: a shrunken case list cannot leave orphaned oracle entries behind."""
    assert_golden_keys_exact(request, _GOLDEN_TEST_NAME, {"tick_state"})
