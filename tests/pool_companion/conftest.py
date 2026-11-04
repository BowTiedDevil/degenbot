"""Fixtures for the collapsed companion guard / io-free suite.

The token companions need a handle-less engine to register metadata in, and
that engine is per-test: it owns mutable Rust state (registered tokens, their
cached balances/approvals), so one engine shared across the module would carry
one test's registrations into the next and make the module order-dependent.
"""

from __future__ import annotations

import pytest

from tests.helpers.seam import Bot


@pytest.fixture
def py_bot() -> Bot:
    """A fresh handle-less engine for one test's token companions."""
    return Bot()
