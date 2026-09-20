"""Relay-posture helpers for the submission seam.

The relay posture decides *where* signed bytes are broadcast: with
``DEGENBOT_SUBMIT_RELAY_URLS`` set, submissions fan out to private builder
endpoints instead of the public mempool. That posture is all this module owns
now.

Nonce issuance is *not* here. The process-wide Rust ``NonceAuthority`` is the
one issuer for every signing path, and the settlement seam seeds it from the
submission-time chain read before stamping. The Python-side reservation ledger
that once overlaid relay-pending nonces was retired: it was a second
reservation table with a different algorithm, exactly the divergence the
authority exists to prevent. ``RelayPosture.reserve_base`` remains only as a
loud deprecation shim for out-of-tree callers; it performs no reservation.
"""

from __future__ import annotations

import os
import warnings

#: First match wins; both spellings persist from the relay rollout.
RELAY_URL_ENV_VARS = ("DEGENBOT_SUBMIT_RELAY_URLS", "DEGENBOT_SUBMIT_RELAY_URL")


def relay_urls_from_env() -> list[str]:
    """The configured relay broadcast URLs (the one home for the env shape).

    Comma-separated either env var; blank entries dropped.
    """
    for var in RELAY_URL_ENV_VARS:
        raw = os.environ.get(var)
        if raw and raw.strip():
            return [url.strip() for url in raw.split(",") if url.strip()]
    return []


class RelayPosture:
    """The session's relay posture.

    Constructed once per session from the configured relay URLs; an empty list
    disables relay broadcast. This is a posture holder only — the nonce itself
    is issued by the Rust authority at sign time.
    """

    def __init__(self, relay_urls: list[str] | tuple[str, ...]) -> None:
        self._relay_urls = list(relay_urls)

    @property
    def relay_urls(self) -> list[str]:
        """The posture-defining relay URLs (a defensive copy)."""
        return list(self._relay_urls)

    @property
    def enabled(self) -> bool:
        """True iff relay-postured."""
        return bool(self._relay_urls)

    @staticmethod
    def reserve_base(local_nonce: int, *, size: int) -> int:
        """Deprecated passthrough; nonces are issued by the Rust authority.

        Retained only so an out-of-tree caller does not silently lose the
        method. The caller's nonce is returned unchanged and no reservation is
        recorded: the settlement seam seeds the authority from the chain read
        and lets the authority lease the nonce.
        """
        del size  # the reservation range no longer exists
        warnings.warn(
            "RelayPosture.reserve_base is retired: nonce issuance moved to the Rust "
            "NonceAuthority; the relay posture no longer keeps a Python reservation "
            "table. The caller's nonce is returned unchanged.",
            DeprecationWarning,
            stacklevel=2,
        )
        return local_nonce
