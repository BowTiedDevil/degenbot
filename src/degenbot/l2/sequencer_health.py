"""
L2 Sequencer health via Phoenix Zero RTT oracle.

Public feed: https://rtt.phoenix-ai.work/api/public-feed
Probes: Arbitrum, Optimism, Base, ZKSync — 2-second intervals, p99 latency + revert rates.
"""

from __future__ import annotations

import time
import urllib.request
import json
from dataclasses import dataclass
from enum import StrEnum
from typing import Literal

PHOENIX_ZERO_URL = "https://rtt.phoenix-ai.work/api/public-feed"

P99_CONGESTION_MS: dict[str, float] = {
    "arb": 400.0,
    "op": 400.0,
    "base": 1_200.0,
    "zk": 600.0,
}
REVERT_CONGESTION_THRESHOLD = 0.10
GAS_PRESSURE_CONGESTION_THRESHOLD = 0.90


class Chain(StrEnum):
    ARBITRUM = "arb"
    OPTIMISM = "op"
    BASE = "base"
    ZKSYNC = "zk"


@dataclass(frozen=True, slots=True)
class SequencerHealth:
    ts: int
    arb_p99: float
    op_p99: float
    base_p99: float
    zk_p99: float
    blob_fee: float
    gas_pres: float
    arb_revert: float
    base_revert: float
    probe: str
    generated: int

    def p99(self, chain: Chain) -> float:
        return getattr(self, f"{chain.value}_p99")

    def revert_rate(self, chain: Chain) -> float | None:
        attr = f"{chain.value}_revert"
        return getattr(self, attr, None)

    def is_safe(
        self,
        chain: Chain,
        *,
        p99_limit_ms: float | None = None,
        revert_limit: float = REVERT_CONGESTION_THRESHOLD,
        gas_limit: float = GAS_PRESSURE_CONGESTION_THRESHOLD,
    ) -> bool:
        limit = p99_limit_ms if p99_limit_ms is not None else P99_CONGESTION_MS[chain.value]
        if self.p99(chain) > limit:
            return False
        revert = self.revert_rate(chain)
        if revert is not None and revert > revert_limit:
            return False
        if self.gas_pres > gas_limit:
            return False
        return True


class SequencerHealthFeed:
    """Fetches the latest L2 sequencer health snapshot from the Phoenix Zero RTT oracle."""

    _cache: SequencerHealth | None = None
    _cache_ts: float = 0.0
    _cache_ttl: float = 10.0  # seconds

    def __init__(
        self,
        url: str = PHOENIX_ZERO_URL,
        *,
        cache_ttl: float = 10.0,
        timeout: float = 3.0,
    ) -> None:
        self._url = url
        self._cache_ttl = cache_ttl
        self._timeout = timeout

    def _parse(self, raw: dict) -> SequencerHealth:  # type: ignore[type-arg]
        latest = raw["data"][-1]
        return SequencerHealth(
            ts=latest["ts"],
            arb_p99=latest["arb_p99"],
            op_p99=latest["op_p99"],
            base_p99=latest["base_p99"],
            zk_p99=latest["zk_p99"],
            blob_fee=latest["blob_fee"],
            gas_pres=latest["gas_pres"],
            arb_revert=latest.get("arb_revert", 0.0),
            base_revert=latest.get("base_revert", 0.0),
            probe=raw["probe"],
            generated=raw["generated"],
        )

    def latest(self) -> SequencerHealth:
        now = time.monotonic()
        if self._cache is not None and (now - self._cache_ts) < self._cache_ttl:
            return self._cache
        with urllib.request.urlopen(self._url, timeout=self._timeout) as resp:  # noqa: S310
            raw = json.loads(resp.read())
        self._cache = self._parse(raw)
        self._cache_ts = now
        return self._cache

    def is_safe(self, chain: Chain, **kwargs: object) -> bool:
        return self.latest().is_safe(chain, **kwargs)

    async def async_latest(self) -> SequencerHealth:
        try:
            import aiohttp
        except ImportError as exc:
            raise ImportError("aiohttp required for async_latest: pip install aiohttp") from exc
        now = time.monotonic()
        if self._cache is not None and (now - self._cache_ts) < self._cache_ttl:
            return self._cache
        async with aiohttp.ClientSession() as session:
            async with session.get(self._url, timeout=aiohttp.ClientTimeout(total=self._timeout)) as resp:
                raw = await resp.json(content_type=None)
        self._cache = self._parse(raw)
        self._cache_ts = now
        return self._cache

    async def async_is_safe(self, chain: Chain, **kwargs: object) -> bool:
        health = await self.async_latest()
        return health.is_safe(chain, **kwargs)
