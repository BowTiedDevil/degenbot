"""Relay-posture holder for the settlement submission seam.

The posture decides *where* signed bytes are broadcast: the resolved
``strategy.settlement.endpoints`` of the typed config (explicit set, or the
pinned revert-protecting default set) — owned by the Rust readiness
resolution, never by driver-side env reads. A live runner that booted past
the readiness gate always carries a non-empty posture here.

Nonce issuance is *not* here. The process-wide Rust ``NonceAuthority`` is
the one issuer for every signing path; the settlement seam seeds it from the
submission-time chain read before stamping.
"""

from __future__ import annotations

import warnings


class RelayPostureUnsettled(RuntimeError):
    """The relay posture was constructed without settled settlement endpoints.

    A live runner that boots past the readiness gate always carries non-empty
    endpoints, so reaching this constructor means a gate was bypassed; the
    session must abort rather than degrade a broadcast to the public mempool.
    """

    def __init__(self) -> None:
        super().__init__(
            "RelayPosture requires settled settlement endpoints; the boot gate "
            "should have refused a live session before reaching this constructor"
        )


class RelayPosture:
    """The session's relay posture.

        Constructed once per session from the resolved settlement endpoints;
    an
        empty list is impossible past the boot gate (a live runner without
        settled endpoints refuses to start rather than degrade to the public
        mempool). This is a posture holder only; the nonce itself is issued by
        the Rust authority at sign time.
    """

    def __init__(self, relay_urls: list[str] | tuple[str, ...]) -> None:
        self._relay_urls = list(relay_urls)
        if not self._relay_urls:
            raise RelayPostureUnsettled

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
        method. The caller's nonce is returned unchanged and no reservation
        is recorded: the settlement seam seeds the authority from the chain
        read and lets the authority lease the nonce.
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
