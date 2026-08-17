from degenbot.uniswap.math import (
    MAX_SQRT_RATIO as RS_MAX_SQRT_RATIO,
)
from degenbot.uniswap.math import (
    MAX_TICK as RS_MAX_TICK,
)
from degenbot.uniswap.math import (
    MIN_SQRT_RATIO as RS_MIN_SQRT_RATIO,
)
from degenbot.uniswap.math import (
    MIN_TICK as RS_MIN_TICK,
)
from degenbot.uniswap.math import (
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)
from degenbot.uniswap.v3_libraries import MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK

type Tick = int
type Ratio = int

TICK_MATH_VECTORS: list[tuple[Tick, Ratio]] = [
    (-887272, 4295128739),  # MIN_TICK
    (-887271, 4295343490),
    (-100000, 533968626430936354154228408),
    (-10000, 48055510970269007215549348797),
    (-1000, 75364347830767020784054125655),
    (-100, 78833030112140176575862854579),
    (-10, 79188560314459151373725315960),
    (-1, 79224201403219477170569942574),
    (0, 79228162514264337593543950336),
    (1, 79232123823359799118286999568),
    (10, 79267784519130042428790663799),
    (100, 79625275426524748796330556128),
    (1000, 83290069058676223003182343270),
    (10000, 130621891405341611593710811006),
    (100000, 11755562826496067164730007768450),
    (500000, 5697689776495288729098254600827762987878),
    (887271, 1461373636630004318706518188784493106690254656249),
    (887272, 1461446703485210103287273052203988822378723970342),  # MAX_TICK
]


def test_get_sqrt_ratio_at_tick_fixed_vectors():
    for tick, expected_ratio in TICK_MATH_VECTORS:
        assert get_sqrt_ratio_at_tick(tick) == expected_ratio


SQRT_RATIO_VECTORS: list[tuple[Ratio, Tick]] = [
    (4295128739, -887272),  # MIN_SQRT_RATIO
    (4295343490, -887271),
    (533968626430936354154228408, -100000),
    (79228162514264337593543950336, 0),  # sqrt(1.0 * 2^96)
    (11755562826496067164730007768450, 100000),
    (1461373636630004318706518188784493106690254656249, 887271),
    (1461446703485210103287273052203988822378723970341, 887271),  # MAX_SQRT_RATIO - 1
]


def test_get_tick_at_sqrt_ratio_fixed_vectors():
    for sqrt_ratio, expected_tick in SQRT_RATIO_VECTORS:
        assert get_tick_at_sqrt_ratio(sqrt_ratio) == expected_tick


def test_tick_math_constants_exposed_from_rust_seam():
    """The four TickMath boundary constants are exposed by the Rust seam.

    ADR-005 standalone-Rust-core: the canonical home for these constants is
    `degenbot-concentrated-liquidity-math` (Rust core), surfaced through the `degenbot_rs` PyO3
    seam. The `v3_libraries` package re-exports them, but the seam must own
    them so a standalone Rust consumer and the Python companion share one
    source of truth.
    """
    assert RS_MIN_TICK == -887272
    assert RS_MAX_TICK == 887272
    assert RS_MIN_SQRT_RATIO == 4295128739
    assert RS_MAX_SQRT_RATIO == 1461446703485210103287273052203988822378723970342


def test_tick_math_constants_match_package_reexport():
    """The package re-export and the seam constant are the same object."""
    assert MIN_TICK is RS_MIN_TICK or MIN_TICK == RS_MIN_TICK
    assert MAX_TICK is RS_MAX_TICK or MAX_TICK == RS_MAX_TICK
    assert MIN_SQRT_RATIO == RS_MIN_SQRT_RATIO
    assert MAX_SQRT_RATIO == RS_MAX_SQRT_RATIO


def test_tick_boundaries():
    assert get_sqrt_ratio_at_tick(MIN_TICK) == MIN_SQRT_RATIO
    assert get_sqrt_ratio_at_tick(MAX_TICK) == MAX_SQRT_RATIO


def test_sqrt_ratio_boundaries():
    assert get_tick_at_sqrt_ratio(MIN_SQRT_RATIO) == MIN_TICK
    assert get_tick_at_sqrt_ratio(MAX_SQRT_RATIO - 1) == MAX_TICK - 1


def test_roundtrip_tick_to_ratio_and_back():
    """Test that tick -> sqrt_ratio -> tick roundtrip is consistent."""
    for tick in [
        -500000,
        -100000,
        -10000,
        -1000,
        -100,
        -10,
        -1,
        0,
        1,
        10,
        100,
        1000,
        10000,
        100000,
        500000,
    ]:
        sqrt_ratio = get_sqrt_ratio_at_tick(tick)
        tick_back = get_tick_at_sqrt_ratio(sqrt_ratio)
        assert tick_back == tick, f"Roundtrip failed for tick={tick}: got {tick_back}"


def test_roundtick_boundary_roundtrip():
    """Test roundtrip at boundary values."""
    min_sqrt_ratio = get_sqrt_ratio_at_tick(MIN_TICK)
    assert get_tick_at_sqrt_ratio(min_sqrt_ratio) == MIN_TICK

    max_sqrt_ratio = get_sqrt_ratio_at_tick(MAX_TICK)
    assert get_tick_at_sqrt_ratio(max_sqrt_ratio - 1) == MAX_TICK - 1
