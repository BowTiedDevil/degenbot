"""The strategy activation surface: readiness resolution over the Rust core.

The PyO3 backing for these lives in ``degenbot._ffi`` (registered by
``degenbot-python`` under the ``strategy`` context); this package is its
stable Python home so consumer modules never import the raw FFI (ADR-013).
"""

from degenbot._ffi import settlement_broadcast_endpoints, validate_strategy_readiness

__all__ = ["settlement_broadcast_endpoints", "validate_strategy_readiness"]
