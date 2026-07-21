"""Rust-raised verification exception types, re-exported under companion names.

These are the Python exception *types* raised by the Rust engine when the
on-chain-truth verification of pool snapshot data fails. The Rust seam
(``degenbot_rs``) splits verification failures into two Python exception
types (both subclassing ``RuntimeError`` so broad ``except RuntimeError``
handlers still catch them):

- :class:`VerificationMismatchError` — a genuine on-chain tick-data
  divergence. Fatal; never retried (trading on stale/incorrect tick data
  loses money).
- :class:`VerificationRpcError` — a per-call RPC transport failure
  (``eth_call`` / ``getTickBitmap`` / ``getTickLiquidity`` could not reach
  the node) or a verify-provider construction failure. Transient; a
  retry/backoff candidate (see :mod:`degenbot.arbitrage.verification_retry`).

.. note::

    These are **direct alias re-exports** of the Rust ``#[pyclass]`` types
    (``from degenbot._ffi import X as Y``), not Python subclasses. The Rust
    engine *raises* the ``degenbot_rs`` pyclass instances, so Python-side
    ``except VerificationRpcError:`` / ``isinstance(exc, VerificationRpcError)``
    must match the exact type the Rust side raises. A subclass re-export
    would silently break that catch — the Rust-raised instance would not be
    an instance of the Python subclass. The companion's only job here is to
    give the type a stable, seam-agnostic home in ``degenbot.exceptions`` so
    driver / policy code does not import ``degenbot_rs``.

Layering note: these are **verification-layer** exceptions — the Rust
verifier's truth-divergence / transport-failure categories. They are distinct
from the provider-layer exceptions in :mod:`degenbot.exceptions.rpc`
(``RpcError`` / ``ContractLogicError`` / ``TransactionNotFound``), which are
the degenbot-owned equivalents of ``web3.exceptions`` types raised at the
provider adapter seam. :class:`VerificationRpcError` is *not* an
:class:`~degenbot.exceptions.rpc.RpcError` subclass — it is the verifier's
own transport-failure category and stays rooted at ``RuntimeError`` so the
retry policy branches on the verification-specific type, not a generic
provider-base catch.
"""

from degenbot._ffi import VerificationMismatchError, VerificationRpcError

__all__ = [
    "VerificationMismatchError",
    "VerificationRpcError",
]
