"""Install operator/executor identity for an ``ArbitrageConfig`` build.

``ArbitrageConfig.build`` reads operator and executor identity from
``os.environ`` now that ``examples/mainnet.env`` is retired as a config source.
Tests carry an identity mapping instead of a dotenv file, so this context
manager installs it for the build. The other identity keys are blanked first so
an ambient shell export cannot leak into a test that means "unset".
"""

from __future__ import annotations

import os
from contextlib import contextmanager
from typing import TYPE_CHECKING
from unittest import mock

if TYPE_CHECKING:
    from collections.abc import Iterator, Mapping

#: The operator/executor keys ``ArbitrageConfig.build`` reads from the process environment.
IDENTITY_KEYS = (
    "OPERATOR_ADDRESS",
    "OPERATOR_PRIVATE_KEY",
    "EXECUTOR_CONTRACT_ADDRESS",
    "EXECUTOR_OWNER_ADDRESS",
    "INJECTED_EXECUTOR_ADDRESS",
    "EXECUTOR_RUNTIME",
)


@contextmanager
def identity_env(values: Mapping[str, str] | None = None) -> Iterator[None]:
    """Install ``values`` as the identity layer for the duration of the block."""

    merged = dict.fromkeys(IDENTITY_KEYS, "")
    merged.update(values or {})
    with mock.patch.dict(os.environ, merged, clear=False):
        yield


__all__ = ("IDENTITY_KEYS", "identity_env")
