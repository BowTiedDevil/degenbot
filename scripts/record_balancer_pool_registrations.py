#!/usr/bin/env python3
"""Record the pinned Balancer V2 pool-registration corpus for the registry sweep.

Balancer V2 exposes NO enumerable on-chain registry: ``getPoolCount()``,
``getPools()`` and ``getPoolAt()`` revert on every factory revision, so a sweep
cannot discover pools from a contract. The Vault's own ``PoolRegistered`` event
is the authoritative registration log, so the corpus is one log scan of

    Vault  0xBA12222222228d8Ba445958a75a0704d566BF2C8
    event  PoolRegistered(bytes32 indexed poolId, address indexed poolAddress,
                          uint8 specialization)
    topic0 0x3c13bc30b8e878c53fd2a36b679409c073afd75950be43d8858768e956fbc20e

from the Vault's deployment block to the sweep pin, plus a family classification
per pool at the pin through ``BotIo.probe_balancer_pool_type`` (the same probe
the production resolver and the Balancer builders use).

The sweep (``tests/balancer/test_balancer_registry_sweep.py``) replays this
corpus at the pin. Re-record only when the pin moves:

    uv run --no-sync python scripts/record_balancer_pool_registrations.py --block 26204126

Requires a reachable archive node (``TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`` from
``tests.env`` / the process environment).
"""

from __future__ import annotations

import argparse
import json
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from degenbot._ffi import BotIo
from degenbot.balancer.deployments import BALANCER_V2_VAULT_ADDRESS
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import keccak256
from degenbot.provider import AlloyProvider
from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI

# `PoolRegistered(bytes32,address,uint8)` — the Vault's `PoolSpecialization` is
# a Solidity enum, which ABI-encodes as `uint8`. The `uint256` spelling of the
# same event name hashes to a topic the Vault never emits (measured: zero logs
# over the Vault's entire history).
POOL_REGISTERED_SIGNATURE = "PoolRegistered(bytes32,address,uint8)"
POOL_REGISTERED_TOPIC = "0x" + keccak256(POOL_REGISTERED_SIGNATURE.encode()).hex()

# The Vault's deployment block: the first `PoolRegistered` log lands at
# 12274556, three hundred blocks later.
VAULT_DEPLOY_BLOCK = 12_272_146

# Block-range per `eth_getLogs` request. Measured against the local reth: 200k
# blocks answers in ~3 s, 2M in ~45 s, and a single full-history request times
# out — the range limit is a response-time budget, not a hard cap.
DEFAULT_CHUNK = 200_000
DEFAULT_WORKERS = 8


def _rpc(url: str, method: str, params: list[object]) -> object:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    request = urllib.request.Request(  # noqa: S310 — archive URI from the test env
        url, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(request, timeout=120) as response:  # noqa: S310
        payload = json.loads(response.read())
    if "error" in payload:
        raise RuntimeError(payload["error"])
    return payload["result"]


def _scan_registrations(url: str, *, end_block: int, chunk: int, workers: int) -> list[dict]:
    ranges = [
        (start, min(start + chunk - 1, end_block))
        for start in range(VAULT_DEPLOY_BLOCK, end_block + 1, chunk)
    ]

    def fetch(block_range: tuple[int, int]) -> list[dict]:
        for attempt in range(4):
            try:
                return _rpc(  # type: ignore[return-value]
                    url,
                    "eth_getLogs",
                    [
                        {
                            "address": BALANCER_V2_VAULT_ADDRESS,
                            "topics": [POOL_REGISTERED_TOPIC],
                            "fromBlock": hex(block_range[0]),
                            "toBlock": hex(block_range[1]),
                        }
                    ],
                )
            except Exception:  # noqa: BLE001 — retry, then give up loudly
                if attempt == 3:
                    raise
                time.sleep(1 + attempt)
        raise AssertionError("unreachable")

    with ThreadPoolExecutor(max_workers=workers) as pool:
        batches = list(pool.map(fetch, ranges))
    return [log for batch in batches for log in batch]


def _decode(log: dict) -> dict:
    """Decode one `PoolRegistered` log into the corpus row's chain facts."""
    pool_id = log["topics"][1]
    pool_address = get_checksum_address("0x" + log["topics"][2][-40:])
    specialization = int(log["data"], 16)
    # The pool id's high 20 bytes are the pool address; a mismatch means the log
    # is not the event this corpus claims.
    assert get_checksum_address("0x" + pool_id[2:42]) == pool_address, (
        f"pool id {pool_id} does not embed address {pool_address}"
    )
    return {
        "address": pool_address,
        "pool_id": pool_id,
        "specialization": specialization,
        "registered_block": int(log["blockNumber"], 16),
    }


class _FamilyProbe:
    """One worker's probe: a provider + BotIo, constructed once per thread.

    `BotIo` holds the provider's Rust client (connection pool included), so
    building one per pool would pay a fresh `eth_chainId` handshake 2000 times.
    """

    def __init__(self, url: str, block: int) -> None:
        self._io = BotIo(provider=AlloyProvider(url))
        self._block = block

    def __call__(self, address: str) -> str:
        """Classify a pool's family with the production probe, at the pin.

        Returns ``"weighted"``/``"stable"`` for the two families a construction
        arm models, ``"unmodeled"`` for a positively identified Balancer pool
        whose family probe answers nothing (a linear pool, a boosted-pool
        constituent, a gyro/quant-amm shape), and ``"unknown"`` for a
        registration whose address answers no pool identity at all.

        `getPoolId()` gates the Balancer claim. It decodes or the row is
        `"unknown"`, because the family probe treats a *successful empty*
        `eth_call` as an answer: a registered address holding only an EIP-7702
        delegation designator (`0xef0100 || address`, no pool code) decodes its
        empty `getNormalizedWeights()` return as an empty weight vector and
        would be labelled "weighted".

        Returns:
            ``"weighted"``, ``"stable"``, ``"unmodeled"``, or ``"unknown"``.
        """
        try:
            self._io.fetch_balancer_pool_id(address)
        except Exception:  # noqa: BLE001 — no decodable pool id means no pool identity
            return "unknown"
        try:
            return self._io.probe_balancer_pool_type(address, block=self._block)
        except Exception:  # noqa: BLE001 — a revert is the family probe's negative
            # A Balancer pool (the pool id decoded) whose family probe did not:
            # the identity is positive, the shape is one no arm models. Kept
            # apart from "unknown" so the sweep can assert the refusal names it.
            return "unmodeled"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--block", type=int, required=True, help="Sweep pin (classification block)")
    parser.add_argument("--chunk", type=int, default=DEFAULT_CHUNK, help="Blocks per log request")
    parser.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args()

    out = args.out or Path(f"tests/balancer/data/balancer_v2_pool_registrations_{args.block}.json")
    url = TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI

    print(f"scanning {BALANCER_V2_VAULT_ADDRESS} PoolRegistered logs -> {args.block}")
    started = time.time()
    logs = _scan_registrations(url, end_block=args.block, chunk=args.chunk, workers=args.workers)
    print(f"{len(logs)} registrations in {time.time() - started:.1f}s")

    rows: dict[str, dict] = {}
    duplicate_registrations = 0
    for log in logs:
        row = _decode(log)
        # The Vault does not forbid registering a pool contract twice (each
        # call mints a fresh pool id, while the contract keeps answering the
        # first), so the corpus is keyed by pool ADDRESS and keeps the earliest
        # registration — the one the contract itself reports.
        if row["address"] in rows:
            duplicate_registrations += 1
            continue
        rows[row["address"]] = row
    if duplicate_registrations:
        print(f"{duplicate_registrations} repeat registrations collapsed by address")

    print(f"classifying {len(rows)} pools at block {args.block}")
    started = time.time()
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        families = list(
            pool.map(_FamilyProbe(url, args.block), (r["address"] for r in rows.values()))
        )
    for row, family in zip(rows.values(), families, strict=True):
        row["family"] = family
    print(f"classified in {time.time() - started:.1f}s")

    census: dict[str, int] = {}
    for row in rows.values():
        census[row["family"]] = census.get(row["family"], 0) + 1
    print(f"family census: {census}")

    document = {
        "_comment": (
            "Balancer V2 pool-registration corpus for the pinned registry sweep. "
            "Generated once by scripts/record_balancer_pool_registrations.py; do not "
            "hand-edit. One row per pool ADDRESS in the Vault's PoolRegistered log "
            "between the Vault's deployment and the pin (a repeat registration is "
            "collapsed to the earliest; duplicate_registrations counts them). family "
            "is the production identity probe's answer at the pin, gated on a "
            "decodable getPoolId: 'weighted'/'stable' are the families a construction "
            "arm models; 'unmodeled' is a positively identified Balancer pool whose "
            "family probe answers nothing (a linear pool, a boosted-pool constituent, "
            "a gyro/quant-amm shape), which the sweep builds and expects to be refused "
            "under its own name; 'unknown' means the address answers no pool identity "
            "at all (an EIP-7702 delegation designator, a junk self-registration) and "
            "the sweep excludes it from the build attempt."
        ),
        "vault": BALANCER_V2_VAULT_ADDRESS,
        "event": POOL_REGISTERED_SIGNATURE,
        "topic0": POOL_REGISTERED_TOPIC,
        "scan_from_block": VAULT_DEPLOY_BLOCK,
        "pin_block": args.block,
        "census": census,
        "duplicate_registrations": duplicate_registrations,
        "pools": sorted(rows.values(), key=lambda row: row["address"]),
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(document, indent=1) + "\n")
    print(f"wrote {out} ({out.stat().st_size} bytes, {len(rows)} pools)")


if __name__ == "__main__":
    main()
