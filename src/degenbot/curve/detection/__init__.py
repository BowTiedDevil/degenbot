"""Curve pool detection sub-modules.

Each detector is independently testable with a fake w3 instance.
The builder orchestrator calls them in sequence and feeds results
to the CurveStableswapPool constructor.
"""

from degenbot.curve.detection.a_ramping import detect_a_ramping
from degenbot.curve.detection.coin_discovery import discover_coins
from degenbot.curve.detection.crypto_detector import detect_crypto_params
from degenbot.curve.detection.lending_detector import detect_lending_tokens
from degenbot.curve.detection.lp_token import find_lp_token
from degenbot.curve.detection.metapool_detector import detect_metapool
from degenbot.curve.detection.types import (
    ARampingResult,
    CoinDiscoveryResult,
    CryptoDetectionResult,
    LendingDetectionResult,
    MetapoolDetectionResult,
)

__all__ = [
    "ARampingResult",
    "CoinDiscoveryResult",
    "CryptoDetectionResult",
    "LendingDetectionResult",
    "MetapoolDetectionResult",
    "detect_a_ramping",
    "detect_crypto_params",
    "detect_lending_tokens",
    "detect_metapool",
    "discover_coins",
    "find_lp_token",
]
