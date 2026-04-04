"""
Unit tests for presets module.
"""

import json
from pathlib import Path

import pytest

from tests.arbitrage.presets import (
    ALL_FIXTURES,
    SIMPLE_FIXTURES,
    SIMPLE_SUITE,
    STRESS_FIXTURES_MULTI,
    STRESS_FIXTURES_V2,
    STRESS_FIXTURES_V3,
    STRESS_FIXTURES_V4,
    V2_STRESS_SUITE,
    V3_STRESS_SUITE,
    V4_STRESS_SUITE,
    FixtureSuite,
    generate_fixture_by_name,
    generate_simple_fixtures,
    get_fixture_names_by_type,
    load_fixture_by_name,
)


class TestFixtureNameLists:
    """Tests for fixture name lists."""

    def test_simple_fixtures_not_empty(self) -> None:
        """Test that simple fixtures list is not empty."""
        assert len(SIMPLE_FIXTURES) == 7

    def test_stress_fixtures_v2_count(self) -> None:
        """Test V2 stress fixtures count."""
        assert len(STRESS_FIXTURES_V2) == 100

    def test_stress_fixtures_v3_count(self) -> None:
        """Test V3 stress fixtures count."""
        assert len(STRESS_FIXTURES_V3) == 100

    def test_stress_fixtures_v4_count(self) -> None:
        """Test V4 stress fixtures count."""
        assert len(STRESS_FIXTURES_V4) == 100

    def test_stress_fixtures_multi_count(self) -> None:
        """Test multi-pool stress fixtures count."""
        assert len(STRESS_FIXTURES_MULTI) == 50

    def test_all_fixtures_combined(self) -> None:
        """Test that ALL_FIXTURES contains all fixture types."""
        assert len(ALL_FIXTURES) == (
            len(SIMPLE_FIXTURES)
            + len(STRESS_FIXTURES_V2)
            + len(STRESS_FIXTURES_V3)
            + len(STRESS_FIXTURES_V4)
            + len(STRESS_FIXTURES_MULTI)
        )

    def test_simple_fixtures_names_valid(self) -> None:
        """Test that simple fixture names match expected pattern."""
        for name in SIMPLE_FIXTURES:
            assert name.startswith("simple_")


class TestGenerateFixtureByName:
    """Tests for generate_fixture_by_name."""

    def test_generate_simple_v2(self) -> None:
        """Test generating simple V2 fixture."""
        fixture = generate_fixture_by_name("simple_v2_arb_profitable")
        assert fixture.id == "simple_v2_arb_profitable"
        assert fixture.cycle_type == "v2_v2"

    def test_generate_simple_v3(self) -> None:
        """Test generating simple V3 fixture."""
        fixture = generate_fixture_by_name("simple_v3_arb_same_tick_spacing")
        assert fixture.id == "simple_v3_arb_same_tick_spacing"
        assert fixture.cycle_type == "v3_v3"

    def test_generate_random_v2(self) -> None:
        """Test generating random V2 fixture."""
        fixture = generate_fixture_by_name("random_v2_pair_seed_42")
        assert fixture.id == "random_v2_pair_seed_42"
        assert fixture.cycle_type == "v2_v2"

    def test_generate_random_v3(self) -> None:
        """Test generating random V3 fixture."""
        fixture = generate_fixture_by_name("random_v3_pair_seed_123")
        assert fixture.id == "random_v3_pair_seed_123"
        assert fixture.cycle_type == "v3_v3"

    def test_generate_random_v4(self) -> None:
        """Test generating random V4 fixture."""
        fixture = generate_fixture_by_name("random_v4_pair_seed_999")
        assert fixture.id == "random_v4_pair_seed_999"
        assert fixture.cycle_type == "v4_v4"

    def test_generate_multi_pool_cycle(self) -> None:
        """Test generating multi-pool cycle fixture."""
        fixture = generate_fixture_by_name("random_multi_pool_cycle_seed_5_pools_3")
        assert fixture.id == "random_multi_pool_cycle_seed_5_pools_3"
        assert len(fixture.pool_states) == 3

    def test_generate_unknown_fixture_raises(self) -> None:
        """Test that unknown fixture name raises ValueError."""
        with pytest.raises(ValueError, match="Unknown fixture name"):
            generate_fixture_by_name("unknown_fixture")


class TestLoadFixtureByName:
    """Tests for load_fixture_by_name."""

    def test_load_from_disk(self, tmp_path: Path) -> None:
        """Test loading fixture from disk."""
        # Generate and save a fixture
        fixture = generate_fixture_by_name("simple_v2_arb_profitable")
        fixture.save(tmp_path / "simple_v2_arb_profitable.json")

        # Load it back
        loaded = load_fixture_by_name("simple_v2_arb_profitable", tmp_path)
        assert loaded.id == fixture.id

    def test_generate_when_not_on_disk(self) -> None:
        """Test generating fixture when not on disk."""
        fixture = load_fixture_by_name("simple_v2_arb_profitable", None)
        assert fixture.id == "simple_v2_arb_profitable"

    def test_generate_when_file_missing(self, tmp_path: Path) -> None:
        """Test generating fixture when file doesn't exist."""
        fixture = load_fixture_by_name("simple_v2_arb_profitable", tmp_path)
        assert fixture.id == "simple_v2_arb_profitable"


class TestGenerateSimpleFixtures:
    """Tests for generate_simple_fixtures."""

    def test_generate_simple_fixtures(self, tmp_path: Path) -> None:
        """Test generating simple fixtures to disk."""
        paths = generate_simple_fixtures(tmp_path)

        assert len(paths) == len(SIMPLE_FIXTURES)
        for name, path in paths.items():
            assert path.exists()
            assert path.name == f"{name}.json"

    def test_generated_fixtures_valid_json(self, tmp_path: Path) -> None:
        """Test that generated fixtures are valid JSON."""
        paths = generate_simple_fixtures(tmp_path)

        for path in paths.values():
            content = path.read_text(encoding="utf-8")
            data = json.loads(content)
            assert "id" in data
            assert "cycle_type" in data
            assert "pool_states" in data


class TestGetFixtureNamesByType:
    """Tests for get_fixture_names_by_type."""

    def test_get_simple_fixtures(self) -> None:
        """Test getting simple fixture names."""
        names = get_fixture_names_by_type("simple")
        assert names == SIMPLE_FIXTURES

    def test_get_v2_fixtures(self) -> None:
        """Test getting V2 fixture names."""
        names = get_fixture_names_by_type("v2")
        assert names == STRESS_FIXTURES_V2

    def test_get_v3_fixtures(self) -> None:
        """Test getting V3 fixture names."""
        names = get_fixture_names_by_type("v3")
        assert names == STRESS_FIXTURES_V3

    def test_get_v4_fixtures(self) -> None:
        """Test getting V4 fixture names."""
        names = get_fixture_names_by_type("v4")
        assert names == STRESS_FIXTURES_V4

    def test_get_multi_fixtures(self) -> None:
        """Test getting multi-pool fixture names."""
        names = get_fixture_names_by_type("multi")
        assert names == STRESS_FIXTURES_MULTI

    def test_get_all_fixtures(self) -> None:
        """Test getting all fixture names."""
        names = get_fixture_names_by_type("all")
        assert names == ALL_FIXTURES

    def test_get_unknown_type_raises(self) -> None:
        """Test that unknown type raises ValueError."""
        with pytest.raises(ValueError, match="Unknown fixture type"):
            get_fixture_names_by_type("unknown")


class TestFixtureSuite:
    """Tests for FixtureSuite."""

    def test_suite_iteration(self) -> None:
        """Test iterating over fixture suite."""
        suite = FixtureSuite(SIMPLE_FIXTURES[:3])
        fixtures = list(suite)
        assert len(fixtures) == 3

    def test_suite_length(self) -> None:
        """Test suite length."""
        suite = FixtureSuite(SIMPLE_FIXTURES)
        assert len(suite) == len(SIMPLE_FIXTURES)

    def test_suite_getitem(self) -> None:
        """Test getting fixture by name."""
        suite = FixtureSuite(SIMPLE_FIXTURES)
        fixture = suite["simple_v2_arb_profitable"]
        assert fixture.id == "simple_v2_arb_profitable"

    def test_suite_get_existing(self) -> None:
        """Test getting existing fixture."""
        suite = FixtureSuite(SIMPLE_FIXTURES[:3])
        fixture = suite.get("simple_v2_arb_profitable")
        assert fixture is not None
        assert fixture.id == "simple_v2_arb_profitable"

    def test_suite_get_nonexistent(self) -> None:
        """Test getting nonexistent fixture returns None."""
        suite = FixtureSuite(SIMPLE_FIXTURES[:3])
        fixture = suite.get("nonexistent_fixture")
        assert fixture is None

    def test_suite_caching(self) -> None:
        """Test that suite caches fixtures."""
        suite = FixtureSuite(SIMPLE_FIXTURES[:3])

        # Get fixture twice
        fixture1 = suite["simple_v2_arb_profitable"]
        fixture2 = suite["simple_v2_arb_profitable"]

        # Should be the same object (cached)
        assert fixture1 is fixture2

    def test_suite_clear_cache(self) -> None:
        """Test clearing suite cache."""
        suite = FixtureSuite(SIMPLE_FIXTURES[:3])

        # Get fixture to cache it
        _ = suite["simple_v2_arb_profitable"]
        assert len(suite._cache) == 1

        # Clear cache
        suite.clear_cache()
        assert len(suite._cache) == 0


class TestPredefinedSuites:
    """Tests for pre-defined fixture suites."""

    def test_simple_suite(self) -> None:
        """Test SIMPLE_SUITE."""
        assert len(SIMPLE_SUITE) == len(SIMPLE_FIXTURES)
        for name in SIMPLE_FIXTURES:
            fixture = SIMPLE_SUITE.get(name)
            assert fixture is not None

    def test_v2_stress_suite(self) -> None:
        """Test V2_STRESS_SUITE."""
        assert len(V2_STRESS_SUITE) == len(STRESS_FIXTURES_V2)

    def test_v3_stress_suite(self) -> None:
        """Test V3_STRESS_SUITE."""
        assert len(V3_STRESS_SUITE) == len(STRESS_FIXTURES_V3)

    def test_v4_stress_suite(self) -> None:
        """Test V4_STRESS_SUITE."""
        assert len(V4_STRESS_SUITE) == len(STRESS_FIXTURES_V4)
