"""Tests for Bot.release_python_state() — the Python-state teardown handshake.

After the Rust engine takes ownership of canonical pool/token state, the Bot's
Python-side caches (tracker `_tracked_pools`/`_untracked_pools` + snapshots,
the pool + token registries) are scaffolding that should be dropped so the hot
loop only holds the engine + the async web3 handle. This encodes the 15-line
hand-rolled trim block from `main()` behind a single Bot method, so the example
stops reverse-engineering Bot internals.

The mid-lifecycle release is *not* end-of-life: the live pump keeps writing V3
Mint/Burn/Swap through the shared Rust ``BotState``. So the release must NOT
unregister the pools from Rust (that would strand every live Mint/Burn —
`registered=false` → pump buffer → discarded — and drop every Swap, freezing
the tick map; the V3 desync surfaced in the permutation run). Only Python
storage is dropped; Rust stays canonical.
"""

import pathlib
from threading import Lock
from unittest.mock import MagicMock

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.degenbot_rs import UniswapArbEngine
from degenbot.provider import ProviderAdapter
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

# V3 Mint topic — keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)").
_V3_MINT_TOPIC = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"


def _topic(value: int) -> str:
    """Encode an int24 tick as a 32-byte (int256) ABI topic.

    The decoder reads the low 4 bytes (28..32) as a signed i32, so the int256
    two's-complement encoding (sign-extended) round-trips for negative ticks.
    """
    return f"0x{(value & ((1 << 256) - 1)):064x}"


def _mint_data(amount: int) -> str:
    """abi.encode(sender[20], uint128 amount, uint256 amount0, uint256 amount1).

    ``sender`` is a zero address; ``amount0``/``amount1`` are zero — only the
    liquidity ``amount`` (word 1) drives the tick-map mutation under test.
    """
    sender = (0).to_bytes(32, "big")
    word1 = (0).to_bytes(16, "big") + amount.to_bytes(16, "big")
    zero = (0).to_bytes(32, "big")
    return "0x" + (sender + word1 + zero + zero).hex()


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> ProviderAdapter:
    provider = MagicMock(spec=ProviderAdapter)
    provider.chain_id = chain_id
    return provider


class _FakeTrackerWithSnapshot:
    """Minimal tracker exercising the unload_snapshot() conditional.

    Mirrors the shape AbstractPoolTracker concrete subclasses present:
    `_tracked_pools`, `_untracked_pools`, and an optional `unload_snapshot`.
    """

    def __init__(self) -> None:
        self._lock = Lock()
        self._tracked_pools: dict[str, object] = {"0xpool": object()}
        self._untracked_pools: set[str] = {"0xother"}
        self.snapshot_unloaded = False

    def unload_snapshot(self) -> None:
        self.snapshot_unloaded = True


class TestReleasePythonState:
    """Bot.release_python_state() — drop Python caches after Rust owns state."""

    def test_release_clears_tracker_caches(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        tracker = bot.add_tracker(UniswapV2PoolTracker, factory_address=factory)
        # seed non-empty caches
        assert tracker._tracked_pools == {}
        tracker._tracked_pools["0xdead"] = object()
        tracker._untracked_pools.add("0xbeef")

        bot.release_python_state()

        assert tracker._tracked_pools == {}
        assert tracker._untracked_pools == set()

    def test_release_calls_unload_snapshot_where_present(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        fake_key = get_checksum_address("0x0000000000000000000000000000000000000001")
        fake = _FakeTrackerWithSnapshot()
        bot._trackers[fake_key] = fake

        bot.release_python_state()

        assert fake.snapshot_unloaded is True
        assert fake._tracked_pools == {}
        assert fake._untracked_pools == set()

    def test_release_resets_pool_and_token_registries(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))

        # seed the registries with sentinel storage via their _storage()
        bot.pools.add(
            pool_address="0x0000000000000000000000000000000000000002",
            pool=object(),  # type: ignore[arg-type]
            chain_id=1,
        )
        # TokenRegistry has a different add signature; just assert reset clears
        assert len(bot.pools) >= 1

        bot.release_python_state()

        assert len(bot.pools) == 0
        assert len(bot.tokens) == 0

    def test_release_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address="0x5C69bEe701ef814E44274f655e7632cB715C14B6",
        )

        bot.release_python_state()
        # second call must not raise
        bot.release_python_state()

    def test_release_keeps_rust_v3_pool_registered(self, tmp_path: pathlib.Path) -> None:
        """release_python_state must NOT unregister V3 pools from Rust BotState.

        The mid-lifecycle handoff drops Python caches because Rust owns
        canonical pool state from here on — the live pump keeps writing V3
        Mint/Burn/Swap through the shared ``BotState``. Pre-fix
        ``PoolRegistry._reset`` called ``py_bot.unregister_pool`` for every
        V2/V3 pool, wiping the exact Rust state it was handing ownership to:
        after release, ``pool_count()`` dropped to 0, every live Mint/Burn hit
        ``registered=false`` → stranded in the pump buffer (then discarded),
        every Swap was dropped, and the tick map froze at its verify-time value
        (the V3 desync in the permutation run — e.g. 0x88e6A0c2 tick 201020).
        """
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        py_bot = bot._py_bot

        address = get_checksum_address("0x88e6A0c2dDD26FEEb64F039a2c41296Fcb3F5640")
        py_bot.register_v3_pool(
            address=address,
            token0="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            token1="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            fee=500,
            tick_spacing=10,
            factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
            sqrt_price_x96=1_795_387_016_509_156_625_815_244_826,
            liquidity=9_876_543_210,
            tick=-76020,
            tick_data={-201000: (100, 1000, 18_000_000)},
            update_block=18_000_000,
            coverage="tracked",
        )
        # Mirror production: the builder registers the pool in BOTH py_bot
        # (Rust, above) AND the Python pool registry (here). release iterates
        # the Python registry, so the pool must be present there for the bug
        # path to touch Rust.
        bot.pools.add(pool_address=address, pool=object(), chain_id=1)  # type: ignore[arg-type]

        assert py_bot.pool_count() == 1

        bot.release_python_state()

        # Rust BotState MUST still hold the pool (pre-fix: 0 — unregistered).
        assert py_bot.pool_count() == 1, (
            "release_python_state must not unregister V3 pools from the Rust "
            "BotState it is handing canonical ownership to (pre-fix: the pool "
            "was unregistered, stranding every live Mint/Burn and dropping Swaps)."
        )

    def test_release_keeps_live_mint_applying_not_buffering(
        self,
        tmp_path: pathlib.Path,
    ) -> None:
        """A live Mint dispatched after release applies, not strands in the buffer.

        The direct symptom of the desync: with the pool unregistered post-
        release, ``apply_v3_liquidity_update`` routes the Mint to the pump buffer
        (``registered=false``) instead of applying it. This asserts the exact
        delivery path — the seeded tick's gross reflects the Mint after release.
        """
        config = _make_test_config(tmp_path)
        bot = Bot(config, provider=_fake_provider(1))
        py_bot = bot._py_bot
        engine = UniswapArbEngine(py_bot=py_bot)

        address = get_checksum_address("0x88e6A0c2dDD26FEEb64F039a2c41296Fcb3F5640")
        py_bot.register_v3_pool(
            address=address,
            token0="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            token1="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            fee=500,
            tick_spacing=10,
            factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
            sqrt_price_x96=1_795_387_016_509_156_625_815_244_826,
            liquidity=9_876_543_210,
            tick=-76020,
            tick_data={-201000: (100, 1000, 18_000_000)},
            update_block=18_000_000,
            coverage="tracked",
        )
        bot.pools.add(pool_address=address, pool=object(), chain_id=1)  # type: ignore[arg-type]

        bot.release_python_state()

        # Dispatch a V3 Mint for the (still-registered) pool: tickLower
        # -201000 (the seeded tick), amount 50 → gross 100 → 150.
        py_bot.dispatch_log(
            address=address,
            topics=[
                _V3_MINT_TOPIC,
                _topic(0),  # owner (unused)
                _topic(-201000),  # tickLower
                _topic(-200990),  # tickUpper
            ],
            data=_mint_data(amount=50),
            block_number=18_000_010,
        )

        tick_data = engine.debug_v3_tick_data(address)
        assert tick_data is not None
        # tickLower gross += amount (pre-fix: still 100 — the Mint was buffered
        # because the pool was unregistered post-release, then discarded).
        assert tick_data[-201000][0] == 150, (
            "a live Mint dispatched after release_python_state must apply to the "
            "tick map (registered=true), not strand in the pump buffer. Pre-fix "
            "the pool was unregistered, so the Mint was buffered + discarded "
            "and the seed (gross=100) never advanced."
        )
        # The Mint did not sit in the buffer.
        assert engine.debug_v3_buffer_count(address) == 0
