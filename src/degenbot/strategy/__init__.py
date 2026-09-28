"""The strategy activation surface: readiness resolution over the Rust core.

Both answers come from the resolved config verdict -- one frozen object built
from the load the process published at FFI module init -- so a driver's
activation gate reads the same facet postures the console read (ADR-062 D7).

Both REFUSE rather than degrade, and a refusal is the contract callers depend
on: an activated facet with an unsettled endpoint set raises ``ValueError``
naming the ``degenbot strategy activate`` remedies, and a hosted runner whose
fleet has no active facet is refused too -- the readiness resolution is the
hosted validation, so an empty fleet never reaches a posture resolver. That is
why they are functions and not verdict getters -- a getter that could raise
would make the verdict unconstructible in exactly the processes that need it.
"""

from degenbot._ffi import StrategyReadinessView
from degenbot.config import resolved_config

__all__ = [
    "StrategyReadinessView",
    "settlement_broadcast_endpoints",
    "validate_strategy_readiness",
]


def validate_strategy_readiness() -> StrategyReadinessView:
    """Resolve the strategy readiness of this process's config.

    The per-arm activity and settled endpoint posture, read through the same
    ``degenbot-config`` authority the ``degenbot strategy`` verbs and the
    backrun driver boot use.

    Raises ``ValueError`` when an activated facet's endpoint set is unsettled,
    or when the hosted fleet has no active facet at all (the remediation names
    the ``degenbot strategy activate`` verbs).

    Returns:
        The readiness view.

    """
    return resolved_config().strategy_readiness()


def settlement_broadcast_endpoints() -> list[str]:
    """Return the resolved settlement broadcast endpoints (this process's arm).

    Raises ``ValueError`` when the settlement facet is inactive or its endpoint
    set is unsettled: a hosted runner IS the settlement arm, so its broadcast
    posture is never optional.

    Returns:
        The broadcast endpoints the settlement composition resolved.

    """
    return resolved_config().settlement_broadcast_endpoints()
