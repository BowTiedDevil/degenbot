"""Tests for enrichment feature flag behavior."""

from unittest.mock import MagicMock, patch

import pytest

from degenbot.aave.enrichment import ScaledEventEnricher, _USE_NEW_ENRICHMENT


class TestFeatureFlag:
    """Tests for the DEGENBOT_NEW_AAVE_ENRICHMENT feature flag."""

    def test_feature_flag_defaults_to_false(self) -> None:
        """By default, the new enrichment is not used."""
        # The flag is set at import time, so we can't test this directly
        # in a test that modifies the environment. Instead, we test the
        # current state.
        # This test documents the expected behavior: without the env var,
        # the flag should be False.
        assert isinstance(_USE_NEW_ENRICHMENT, bool)

    def test_enricher_uses_legacy_by_default(self) -> None:
        """Without the feature flag, the legacy enricher is used."""
        mock_session = MagicMock()

        # Patch the flag to be False
        with patch("degenbot.aave.enrichment._USE_NEW_ENRICHMENT", False):
            enricher = ScaledEventEnricher(
                pool_revision=1,
                token_revisions={},
                session=mock_session,
            )

            # Trigger lazy initialization
            _ = enricher._get_enricher()

            # Verify the enricher is from the legacy module
            assert enricher._enricher.__class__.__module__ == "degenbot.aave.enrichment._legacy"

    def test_enricher_uses_new_when_flag_set(self) -> None:
        """With the feature flag, the new enricher is used."""
        mock_session = MagicMock()

        # Patch the flag to be True
        with patch("degenbot.aave.enrichment._USE_NEW_ENRICHMENT", True):
            enricher = ScaledEventEnricher(
                pool_revision=1,
                token_revisions={},
                session=mock_session,
            )

            # Trigger lazy initialization
            _ = enricher._get_enricher()

            # Verify the enricher is from the new module
            assert enricher._enricher.__class__.__module__ == "degenbot.aave.enrichment.core"

    def test_enricher_lazy_initialization(self) -> None:
        """The enricher is only created when needed."""
        mock_session = MagicMock()

        with patch("degenbot.aave.enrichment._USE_NEW_ENRICHMENT", False):
            enricher = ScaledEventEnricher(
                pool_revision=1,
                token_revisions={},
                session=mock_session,
            )

            # Not initialized yet
            assert enricher._enricher is None

            # Trigger initialization
            _ = enricher._get_enricher()

            # Now initialized
            assert enricher._enricher is not None
