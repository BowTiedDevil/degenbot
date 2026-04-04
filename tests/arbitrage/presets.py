"""
Preset fixture lists and batch generation utilities.

Provides pre-defined fixture suites for testing and benchmarking.
"""

from collections.abc import Iterator
from pathlib import Path

from tests.arbitrage.generator.fixtures import (
    ArbitrageCycleFixture,
    FixtureFactory,
)

# =============================================================================
# Fixture Name Lists
# =============================================================================

SIMPLE_FIXTURES: list[str] = [
    "simple_v2_arb_profitable",
    "simple_v2_arb_cross_fee",
    "simple_v3_arb_same_tick_spacing",
    "simple_v3_arb_cross_fee_tier",
    "simple_mixed_v2_v3",
    "simple_v4_arb",
    "simple_v4_vs_v3",
]

# Stress test fixtures use seed ranges
STRESS_FIXTURES_V2: list[str] = [f"random_v2_pair_seed_{i}" for i in range(100)]
STRESS_FIXTURES_V3: list[str] = [f"random_v3_pair_seed_{i}" for i in range(100)]
STRESS_FIXTURES_V4: list[str] = [f"random_v4_pair_seed_{i}" for i in range(100)]
STRESS_FIXTURES_MULTI: list[str] = [
    f"random_multi_pool_cycle_seed_{i}_pools_3" for i in range(50)
]

ALL_FIXTURES: list[str] = (
    SIMPLE_FIXTURES
    + STRESS_FIXTURES_V2
    + STRESS_FIXTURES_V3
    + STRESS_FIXTURES_V4
    + STRESS_FIXTURES_MULTI
)


# =============================================================================
# Fixture Generation
# =============================================================================


def _get_factory() -> FixtureFactory:
    """Create a new fixture factory."""
    return FixtureFactory()


def generate_fixture_by_name(name: str) -> ArbitrageCycleFixture:
    """
    Generate a fixture by its name.

    Parameters
    ----------
    name : str
        The fixture name (e.g., "simple_v2_arb_profitable" or "random_v2_pair_seed_42").

    Returns
    -------
    ArbitrageCycleFixture
        The generated fixture.

    Raises
    ------
    ValueError
        If the fixture name is not recognized.
    """
    factory = _get_factory()

    # Simple fixtures
    if name == "simple_v2_arb_profitable":
        return factory.simple_v2_arb_profitable()
    if name == "simple_v2_arb_cross_fee":
        return factory.simple_v2_arb_cross_fee()
    if name == "simple_v3_arb_same_tick_spacing":
        return factory.simple_v3_arb_same_tick_spacing()
    if name == "simple_v3_arb_cross_fee_tier":
        return factory.simple_v3_arb_cross_fee_tier()
    if name == "simple_mixed_v2_v3":
        return factory.simple_mixed_v2_v3()
    if name == "simple_v4_arb":
        return factory.simple_v4_arb()
    if name == "simple_v4_vs_v3":
        return factory.simple_v4_vs_v3()

    # Random V2 fixtures
    if name.startswith("random_v2_pair_seed_"):
        seed = int(name.removeprefix("random_v2_pair_seed_"))
        return factory.random_v2_pair(seed=seed)

    # Random V3 fixtures
    if name.startswith("random_v3_pair_seed_"):
        seed = int(name.removeprefix("random_v3_pair_seed_"))
        return factory.random_v3_pair(seed=seed)

    # Random V4 fixtures
    if name.startswith("random_v4_pair_seed_"):
        seed = int(name.removeprefix("random_v4_pair_seed_"))
        return factory.random_v4_pair(seed=seed)

    # Multi-pool cycles
    if name.startswith("random_multi_pool_cycle_seed_"):
        # Parse: random_multi_pool_cycle_seed_{seed}_pools_{num_pools}
        parts = name.split("_")
        seed = int(parts[5])
        num_pools = int(parts[7])
        return factory.random_multi_pool_cycle(seed=seed, num_pools=num_pools)

    msg = f"Unknown fixture name: {name}"
    raise ValueError(msg)


def generate_all_fixtures(
    output_dir: Path,
    fixtures: list[str] | None = None,
) -> dict[str, Path]:
    """
    Generate fixture JSON files.

    Parameters
    ----------
    output_dir : Path
        Directory to write fixture files.
    fixtures : list[str] | None
        List of fixture names to generate. If None, generates ALL_FIXTURES.

    Returns
    -------
    dict[str, Path]
        Mapping of fixture names to their file paths.
    """
    if fixtures is None:
        fixtures = ALL_FIXTURES

    output_dir.mkdir(parents=True, exist_ok=True)
    paths: dict[str, Path] = {}

    for name in fixtures:
        fixture = generate_fixture_by_name(name)
        path = output_dir / f"{name}.json"
        fixture.save(path)
        paths[name] = path

    return paths


def generate_simple_fixtures(output_dir: Path) -> dict[str, Path]:
    """
    Generate simple fixture JSON files.

    Parameters
    ----------
    output_dir : Path
        Directory to write fixture files.

    Returns
    -------
    dict[str, Path]
        Mapping of fixture names to their file paths.
    """
    return generate_all_fixtures(output_dir, SIMPLE_FIXTURES)


def generate_stress_fixtures(
    output_dir: Path,
    pool_types: list[str] | None = None,
) -> dict[str, Path]:
    """
    Generate stress test fixture JSON files.

    Parameters
    ----------
    output_dir : Path
        Directory to write fixture files.
    pool_types : list[str] | None
        Pool types to generate ("v2", "v3", "v4", "multi"). If None, generates all.

    Returns
    -------
    dict[str, Path]
        Mapping of fixture names to their file paths.
    """
    if pool_types is None:
        pool_types = ["v2", "v3", "v4", "multi"]

    fixtures: list[str] = []
    if "v2" in pool_types:
        fixtures.extend(STRESS_FIXTURES_V2)
    if "v3" in pool_types:
        fixtures.extend(STRESS_FIXTURES_V3)
    if "v4" in pool_types:
        fixtures.extend(STRESS_FIXTURES_V4)
    if "multi" in pool_types:
        fixtures.extend(STRESS_FIXTURES_MULTI)

    return generate_all_fixtures(output_dir, fixtures)


def load_fixture_by_name(
    name: str,
    fixture_dir: Path | None = None,
) -> ArbitrageCycleFixture:
    """
    Load a fixture by its name.

    If fixture_dir is provided and the file exists, loads from disk.
    Otherwise, generates the fixture on-the-fly.

    Parameters
    ----------
    name : str
        The fixture name.
    fixture_dir : Path | None
        Directory containing fixture JSON files.

    Returns
    -------
    ArbitrageCycleFixture
        The loaded or generated fixture.
    """
    if fixture_dir is not None:
        path = fixture_dir / f"{name}.json"
        if path.exists():
            return ArbitrageCycleFixture.load(path)

    # Generate on-the-fly if not on disk
    return generate_fixture_by_name(name)


def get_fixture_names_by_type(fixture_type: str) -> list[str]:
    """
    Get fixture names filtered by type.

    Parameters
    ----------
    fixture_type : str
        One of "simple", "v2", "v3", "v4", "multi", or "all".

    Returns
    -------
    list[str]
        List of fixture names.
    """
    if fixture_type == "simple":
        return SIMPLE_FIXTURES.copy()
    if fixture_type == "v2":
        return STRESS_FIXTURES_V2.copy()
    if fixture_type == "v3":
        return STRESS_FIXTURES_V3.copy()
    if fixture_type == "v4":
        return STRESS_FIXTURES_V4.copy()
    if fixture_type == "multi":
        return STRESS_FIXTURES_MULTI.copy()
    if fixture_type == "all":
        return ALL_FIXTURES.copy()

    msg = f"Unknown fixture type: {fixture_type}"
    raise ValueError(msg)


class FixtureSuite:
    """
    A collection of fixtures for testing.

    Provides iteration and access methods for a set of fixtures.
    """

    def __init__(
        self,
        names: list[str],
        fixture_dir: Path | None = None,
    ) -> None:
        """
        Initialize the fixture suite.

        Parameters
        ----------
        names : list[str]
            Fixture names in the suite.
        fixture_dir : Path | None
            Directory containing fixture JSON files.
        """
        self.names = names
        self.fixture_dir = fixture_dir
        self._cache: dict[str, ArbitrageCycleFixture] = {}

    def __iter__(self) -> Iterator[ArbitrageCycleFixture]:
        """Iterate over fixtures in the suite."""
        for name in self.names:
            yield self[name]

    def __len__(self) -> int:
        """Number of fixtures in the suite."""
        return len(self.names)

    def __getitem__(self, name: str) -> ArbitrageCycleFixture:
        """Get a fixture by name."""
        if name not in self._cache:
            self._cache[name] = load_fixture_by_name(name, self.fixture_dir)
        return self._cache[name]

    def get(self, name: str) -> ArbitrageCycleFixture | None:
        """Get a fixture by name, or None if not found."""
        if name not in self.names:
            return None
        return self[name]

    def clear_cache(self) -> None:
        """Clear the fixture cache."""
        self._cache.clear()


# Pre-defined fixture suites
SIMPLE_SUITE = FixtureSuite(SIMPLE_FIXTURES)
V2_STRESS_SUITE = FixtureSuite(STRESS_FIXTURES_V2)
V3_STRESS_SUITE = FixtureSuite(STRESS_FIXTURES_V3)
V4_STRESS_SUITE = FixtureSuite(STRESS_FIXTURES_V4)
