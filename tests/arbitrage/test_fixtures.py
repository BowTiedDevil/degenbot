"""
Unit tests for arbitrage fixtures.
"""

import json
import tempfile
from pathlib import Path

import pytest

from tests.arbitrage.generator.fixtures import (
    ArbitrageCycleFixture,
    FixtureFactory,
)


@pytest.fixture
def factory() -> FixtureFactory:
    return FixtureFactory()


class TestFixtureSerialization:
    """Tests for fixture serialization."""

    def test_to_json_v2_fixture(self, factory: FixtureFactory) -> None:
        """Test JSON serialization of V2 fixture."""
        fixture = factory.simple_v2_arb_profitable()

        json_str = fixture.to_json()
        data = json.loads(json_str)

        assert data["id"] == "simple_v2_arb_profitable"
        assert data["cycle_type"] == "v2_v2"
        assert len(data["pool_states"]) == 2
        assert data["expected_optimal_input"] == 0

    def test_to_json_v3_fixture(self, factory: FixtureFactory) -> None:
        """Test JSON serialization of V3 fixture."""
        fixture = factory.simple_v3_arb_same_tick_spacing()

        json_str = fixture.to_json()
        data = json.loads(json_str)

        assert data["id"] == "simple_v3_arb_same_tick_spacing"
        assert data["cycle_type"] == "v3_v3"
        assert len(data["pool_states"]) == 2

    def test_to_json_v4_fixture(self, factory: FixtureFactory) -> None:
        """Test JSON serialization of V4 fixture."""
        fixture = factory.simple_v4_arb()

        json_str = fixture.to_json()
        data = json.loads(json_str)

        assert data["id"] == "simple_v4_arb"
        assert data["cycle_type"] == "v4_v4"

    def test_round_trip_v2(self, factory: FixtureFactory) -> None:
        """Test V2 fixture serialization round-trip."""
        fixture = factory.simple_v2_arb_profitable()

        json_str = fixture.to_json()
        restored = ArbitrageCycleFixture.from_json(json_str)

        assert restored.id == fixture.id
        assert restored.cycle_type == fixture.cycle_type
        assert len(restored.pool_states) == len(fixture.pool_states)

    def test_round_trip_v3(self, factory: FixtureFactory) -> None:
        """Test V3 fixture serialization round-trip."""
        fixture = factory.simple_v3_arb_same_tick_spacing()

        json_str = fixture.to_json()
        restored = ArbitrageCycleFixture.from_json(json_str)

        assert restored.id == fixture.id
        assert restored.cycle_type == fixture.cycle_type

    def test_round_trip_v4(self, factory: FixtureFactory) -> None:
        """Test V4 fixture serialization round-trip."""
        fixture = factory.simple_v4_arb()

        json_str = fixture.to_json()
        restored = ArbitrageCycleFixture.from_json(json_str)

        assert restored.id == fixture.id
        assert restored.cycle_type == fixture.cycle_type


class TestFixtureFileIO:
    """Tests for fixture file I/O."""

    def test_save_and_load_v2(self, factory: FixtureFactory) -> None:
        """Test V2 fixture file save/load."""
        fixture = factory.simple_v2_arb_profitable()

        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test_fixture.json"
            fixture.save(path)

            assert path.exists()

            loaded = ArbitrageCycleFixture.load(path)
            assert loaded.id == fixture.id
            assert loaded.cycle_type == fixture.cycle_type

    def test_save_and_load_v3(self, factory: FixtureFactory) -> None:
        """Test V3 fixture file save/load."""
        fixture = factory.simple_v3_arb_same_tick_spacing()

        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "test_v3_fixture.json"
            fixture.save(path)

            loaded = ArbitrageCycleFixture.load(path)
            assert loaded.id == fixture.id

    def test_save_creates_parent_dirs(self, factory: FixtureFactory) -> None:
        """Test that save creates parent directories."""
        fixture = factory.simple_v2_arb_profitable()

        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "subdir" / "nested" / "fixture.json"
            fixture.save(path)

            assert path.exists()


class TestFixtureValidation:
    """Tests for fixture validation."""

    def test_validate_valid_fixture(self, factory: FixtureFactory) -> None:
        """Test validation of valid fixture."""
        fixture = factory.simple_v2_arb_profitable()
        assert fixture.validate() is True

    def test_validate_v3_fixture(self, factory: FixtureFactory) -> None:
        """Test validation of V3 fixture."""
        fixture = factory.simple_v3_arb_same_tick_spacing()
        assert fixture.validate() is True

    def test_validate_v4_fixture(self, factory: FixtureFactory) -> None:
        """Test validation of V4 fixture."""
        fixture = factory.simple_v4_arb()
        assert fixture.validate() is True


class TestSimpleFixtures:
    """Tests for simple fixture generation."""

    def test_simple_v2_arb_profitable(self, factory: FixtureFactory) -> None:
        """Test simple V2 arbitrage fixture."""
        fixture = factory.simple_v2_arb_profitable()

        assert fixture.id == "simple_v2_arb_profitable"
        assert fixture.cycle_type == "v2_v2"
        assert len(fixture.pool_states) == 2
        fixture.validate()

    def test_simple_v2_arb_cross_fee(self, factory: FixtureFactory) -> None:
        """Test cross-fee V2 arbitrage fixture."""
        fixture = factory.simple_v2_arb_cross_fee()

        assert fixture.id == "simple_v2_arb_cross_fee"
        assert fixture.cycle_type == "v2_v2"
        fixture.validate()

    def test_simple_v3_arb_same_tick_spacing(self, factory: FixtureFactory) -> None:
        """Test V3 same tick spacing arbitrage fixture."""
        fixture = factory.simple_v3_arb_same_tick_spacing()

        assert fixture.id == "simple_v3_arb_same_tick_spacing"
        assert fixture.cycle_type == "v3_v3"
        fixture.validate()

    def test_simple_v3_arb_cross_fee_tier(self, factory: FixtureFactory) -> None:
        """Test V3 cross fee tier arbitrage fixture."""
        fixture = factory.simple_v3_arb_cross_fee_tier()

        assert fixture.id == "simple_v3_arb_cross_fee_tier"
        fixture.validate()

    def test_simple_mixed_v2_v3(self, factory: FixtureFactory) -> None:
        """Test mixed V2/V3 arbitrage fixture."""
        fixture = factory.simple_mixed_v2_v3()

        assert fixture.id == "simple_mixed_v2_v3"
        assert fixture.cycle_type == "v2_v3"
        fixture.validate()

    def test_simple_v4_arb(self, factory: FixtureFactory) -> None:
        """Test V4 arbitrage fixture."""
        fixture = factory.simple_v4_arb()

        assert fixture.id == "simple_v4_arb"
        assert fixture.cycle_type == "v4_v4"
        fixture.validate()

    def test_simple_v4_vs_v3(self, factory: FixtureFactory) -> None:
        """Test V4 vs V3 arbitrage fixture."""
        fixture = factory.simple_v4_vs_v3()

        assert fixture.id == "simple_v4_vs_v3"
        assert fixture.cycle_type == "v3_v4"
        fixture.validate()


class TestRandomFixtures:
    """Tests for random fixture generation."""

    def test_random_v2_pair_basic(self, factory: FixtureFactory) -> None:
        """Test basic random V2 pair generation."""
        fixture = factory.random_v2_pair(seed=42)

        assert "random_v2_pair_seed_42" in fixture.id
        assert fixture.cycle_type == "v2_v2"
        assert len(fixture.pool_states) == 2
        fixture.validate()

    def test_random_v2_pair_deterministic(self, factory: FixtureFactory) -> None:
        """Test that random V2 pair is deterministic with same seed."""
        fixture1 = factory.random_v2_pair(seed=123)
        fixture2 = factory.random_v2_pair(seed=123)

        assert fixture1.id == fixture2.id
        assert fixture1.pool_states.keys() == fixture2.pool_states.keys()

    def test_random_v2_pair_different_seeds(self, factory: FixtureFactory) -> None:
        """Test that different seeds produce different fixtures."""
        fixture1 = factory.random_v2_pair(seed=1)
        fixture2 = factory.random_v2_pair(seed=2)

        assert fixture1.id != fixture2.id

    def test_random_v3_pair_basic(self, factory: FixtureFactory) -> None:
        """Test basic random V3 pair generation."""
        fixture = factory.random_v3_pair(seed=42)

        assert "random_v3_pair_seed_42" in fixture.id
        assert fixture.cycle_type == "v3_v3"
        fixture.validate()

    def test_random_v3_pair_deterministic(self, factory: FixtureFactory) -> None:
        """Test that random V3 pair is deterministic with same seed."""
        fixture1 = factory.random_v3_pair(seed=456)
        fixture2 = factory.random_v3_pair(seed=456)

        assert fixture1.id == fixture2.id

    def test_random_v4_pair_basic(self, factory: FixtureFactory) -> None:
        """Test basic random V4 pair generation."""
        fixture = factory.random_v4_pair(seed=42)

        assert "random_v4_pair_seed_42" in fixture.id
        assert fixture.cycle_type == "v4_v4"
        fixture.validate()

    def test_random_v4_pair_deterministic(self, factory: FixtureFactory) -> None:
        """Test that random V4 pair is deterministic with same seed."""
        fixture1 = factory.random_v4_pair(seed=789)
        fixture2 = factory.random_v4_pair(seed=789)

        assert fixture1.id == fixture2.id


class TestMultiPoolCycle:
    """Tests for multi-pool cycle generation."""

    def test_random_multi_pool_cycle_basic(self, factory: FixtureFactory) -> None:
        """Test basic multi-pool cycle generation."""
        fixture = factory.random_multi_pool_cycle(seed=42, num_pools=3)

        assert "random_multi_pool_cycle" in fixture.id
        assert len(fixture.pool_states) == 3
        fixture.validate()

    def test_random_multi_pool_cycle_v3_pools(self, factory: FixtureFactory) -> None:
        """Test multi-pool cycle with V3 pools."""
        fixture = factory.random_multi_pool_cycle(
            seed=42,
            num_pools=3,
            pool_types=["v3", "v3", "v3"],
        )

        assert len(fixture.pool_states) == 3
        fixture.validate()

    def test_random_multi_pool_cycle_mixed_types(self, factory: FixtureFactory) -> None:
        """Test multi-pool cycle with mixed pool types."""
        fixture = factory.random_multi_pool_cycle(
            seed=42,
            num_pools=3,
            pool_types=["v2", "v3", "v4"],
        )

        assert len(fixture.pool_states) == 3
        fixture.validate()

    def test_random_multi_pool_cycle_deterministic(self, factory: FixtureFactory) -> None:
        """Test that multi-pool cycle is deterministic."""
        fixture1 = factory.random_multi_pool_cycle(seed=999, num_pools=3)
        fixture2 = factory.random_multi_pool_cycle(seed=999, num_pools=3)

        assert fixture1.id == fixture2.id
        assert fixture1.pool_states.keys() == fixture2.pool_states.keys()

    def test_random_multi_pool_cycle_minimum_pools(self, factory: FixtureFactory) -> None:
        """Test that minimum pools requirement is enforced."""
        with pytest.raises(ValueError, match="must be at least 3"):
            factory.random_multi_pool_cycle(seed=42, num_pools=2)

    def test_random_multi_pool_cycle_pool_types_mismatch(
        self, factory: FixtureFactory
    ) -> None:
        """Test that pool_types length must match num_pools."""
        with pytest.raises(ValueError, match="must match"):
            factory.random_multi_pool_cycle(
                seed=42,
                num_pools=4,
                pool_types=["v2", "v3"],
            )
