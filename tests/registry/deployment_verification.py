"""Cast seam, tier input tables, and golden-capture store for deployment verification.

One home for the facts the on-chain deployment verification tiers consume:

- :class:`CastHarness` — the injectable cast seam. The live ``online_rpc``
  harness and the golden record driver bind :class:`FoundryCast` (the real
  ``cast`` binary); nothing else in the codebase touches it.
- ``EXPECTED_FACTORY_SELECTORS`` / ``KNOWN_PAIRS`` — the tier-2 interface
  fingerprints and the tier-4 known deployed pairs/pools. The live harness
  and the hermetic replay module (``test_deployment_golden_verification.py``)
  both derive expectations from these tables, so the two verification paths
  cannot drift apart.
- :class:`DeploymentGoldenCapture` — record/replay handle over the committed
  capture JSON backing the replay tests.

Capture row schema (keyed ``"<chain_id>:<factory lowercase>"``):

.. code-block:: json

    {
      "name": "Uniswap V2",
      "pool_type": "uniswap-v2",
      "bytecode_present": true,
      "selectors": ["0xc9c65396", "0xe6a43905", "0x574f2ba3"],
      "pool": {
        "kind": "v2",
        "tokens": ["0x…", "0x…"],
        "fee": 0,
        "deployer": "0x…",
        "init_hash": "0x…",
        "on_chain_address": "0x…",
        "computed_address": "0x…"
      }
    }

``pool`` is ``{"skip": "<reason>"}`` when tier 4 does not apply (no known
pair registered, or the factory reports the zero address for the known pair).
Rows whose chain was unreachable at record time have no entry, which the
replay tests surface as loud skips, never silent passes.
"""

from __future__ import annotations

import json
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Protocol

from degenbot._ffi.deployments import (
    resolve_deployer,
    resolve_v2_init_hash,
    resolve_v3_init_hash,
)
from degenbot.uniswap.v2_functions import generate_v2_pool_address
from degenbot.uniswap.v3_functions import generate_v3_pool_address
from tests.golden.oracle import GOLDEN_ROOT

if TYPE_CHECKING:
    from pathlib import Path

SCHEMA_VERSION = 1

GOLDEN_CAPTURE_PATH = GOLDEN_ROOT / "registry" / "deployment_onchain_verification.json"

# ---------------------------------------------------------------------------
# Cast seam
# ---------------------------------------------------------------------------


class CastHarness(Protocol):
    """The on-chain reads the verification tiers are built from.

    The live harness and record driver bind :class:`FoundryCast`; a hermetic
    consumer binds recorded responses instead. Everything here is a pure data
    fetch — all tier *logic* lives in the test modules and shared helpers.
    """

    def block_number(self, *, rpc_url: str) -> int:
        """Confirm the RPC responds; raises when unreachable."""
        ...

    def code(self, factory: str, *, rpc_url: str) -> str:
        """Return the raw bytecode hex (``0x…``) deployed at ``factory``."""
        ...

    def selectors(self, bytecode: str) -> set[str]:
        """Return the 4-byte function selectors embedded in ``bytecode``."""
        ...

    def call(self, factory: str, signature: str, *args: str, rpc_url: str) -> str:
        """Return the decoded result of a read-only contract call."""
        ...


def _cast(*args: str, timeout: int = 30) -> str:
    """Run ``cast`` with the given args, return stdout, raising on failure."""
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        ["cast", *args],  # ruff: ignore[start-process-with-partial-path]
        check=True,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return result.stdout.strip()


class FoundryCast:
    """CastHarness binding backed by the Foundry ``cast`` binary.

    The only place in the verification stack that spawns a process — the
    seam exists so the record driver can bind it while nothing hermetic does.
    """

    def block_number(self, *, rpc_url: str) -> int:
        """Confirm the RPC responds; raises when unreachable."""
        return int(
            _cast("block-number", "--rpc-url", rpc_url, timeout=15),
        )

    def code(self, factory: str, *, rpc_url: str) -> str:
        """Return the raw bytecode hex (``0x…``) via ``cast code``."""
        return _cast("code", factory, "--rpc-url", rpc_url)

    def selectors(self, bytecode: str) -> set[str]:
        """Return the selector set via ``cast selectors``.

        ``cast selectors`` prints ``<selector>  <offset>`` lines; keep the
        selector column, lowercased.
        """
        out = _cast("selectors", bytecode)
        return {line.split()[0].lower() for line in out.splitlines() if line.strip()}

    def call(self, factory: str, signature: str, *args: str, rpc_url: str) -> str:
        """Return the decoded result of ``cast call``."""
        return _cast("call", factory, signature, *args, "--rpc-url", rpc_url)


# ---------------------------------------------------------------------------
# Tier 4 — CREATE2 init_code_hash verification helpers
# ---------------------------------------------------------------------------
# Known deployed pairs/pools, one per factory we can verify the init_hash
# against. Keyed by (chain_id, lowercase factory). Each entry is a uniform
# 4-tuple ``(kind, token0, token1, fee)`` — ``kind`` is "v2" or "v3";
# ``fee`` is the V3 fee tier in pips (unused for v2, kept 0). The token order
# doesn't matter (both the in-process generator and the on-chain
# getPair/getPool sort internally). If getPair/getPool returns the zero address
# (pair not deployed), tier 4 skips that factory.
#
# These are long-lived, high-liquidity, canonical pairs — independent ground
# truth. Keep this table curated: a stale/no-longer-deployed pair just skips;
# a wrong pair (returning a real but different address) would fail loudly.
_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
_DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
# Base native WETH + USDC
_BASE_WETH = "0x4200000000000000000000000000000000000006"
_BASE_USDC = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA20429"

# fee tiers as uint24 (Pip)
_FEE_500 = 500  # 0.05%

KnownPair = tuple[str, str, str, int]

KNOWN_PAIRS: dict[tuple[int, str], KnownPair] = {
    # Ethereum mainnet V2 — DAI/WETH (the canonical Uniswap V2 pair) + USDC/WETH.
    (1, "0x5c69bee701ef814a2b6a3edd4b1652cb9cc5aa6f"): ("v2", _DAI, _WETH, 0),  # Uniswap V2
    (1, "0xc0aee478e3658e2610c5f7a4a2e1777ce9e4f2ac"): ("v2", _DAI, _WETH, 0),  # Sushi V2
    (1, "0x1097053fd2ea711dad45cacc45eff7548fcb362"): ("v2", _USDC, _WETH, 0),  # Pancake V2
    # Ethereum mainnet V3 — WETH/USDC 0.05%.
    (1, "0x1f98431c8ad98523631ae4a59f267346ea31f984"): ("v3", _WETH, _USDC, _FEE_500),  # Uniswap V3
    (1, "0xbaceb8ec6b9355dfc0269c18bac9d6e2bdc29c4f"): ("v3", _WETH, _USDC, _FEE_500),  # Sushi V3
    (1, "0x0bfbcf9fa4f9c56b0f40a671ad40e0805a091865"): ("v3", _WETH, _USDC, _FEE_500),  # Pancake V3
    # Base — WETH/USDC 0.05%.
    (8453, "0x33128a8fc17869897dce68ed026d694621f6fdfd"): ("v3", _BASE_WETH, _BASE_USDC, _FEE_500),
    # Arbitrum — Uniswap V3 WETH/USDC 0.05%.
    (42161, "0x1f98431c8ad98523631ae4a59f267346ea31f984"): ("v3", _WETH, _USDC, _FEE_500),
}


def is_zero_address(addr: str) -> bool:
    """True if ``addr`` is the zero address (0x0…0)."""
    cleaned = addr.lower().removeprefix("0x")
    return not cleaned or int(cleaned, 16) == 0


def onchain_pool_address(
    harness: CastHarness,
    factory: str,
    known: KnownPair,
    *,
    rpc_url: str,
) -> str:
    """Query the factory on-chain for the known pair's pool address.

    Returns:
        The ``getPair``/``getPool`` result (checksummed by cast). For V2 the
        signature is ``getPair(address,address)``; for V3
        ``getPool(address,address,uint24)``.

    """
    kind, t0, t1, fee = known
    if kind == "v2":
        return harness.call(
            factory,
            "getPair(address,address)(address)",
            t0,
            t1,
            rpc_url=rpc_url,
        )
    return harness.call(
        factory,
        "getPool(address,address,uint24)(address)",
        t0,
        t1,
        str(fee),
        rpc_url=rpc_url,
    )


def resolve_runtime_create2_fields(chain_id: int, factory: str, pool_type: str) -> tuple[str, str]:
    """Source the CREATE2 deployer + init_hash the runtime pool lookups use.

    Reads the Rust resolver — the runtime value production pool discovery
    consumes — so tier 4 verifies *that* value reproduces ground truth, not
    the JSON text the lookup key came from.

    Returns:
        The ``(deployer, init_hash)`` pair.

    """
    deployer = resolve_deployer(chain_id, factory)
    if "v3" in pool_type:
        init_hash = resolve_v3_init_hash(chain_id, factory)
    else:
        init_hash = resolve_v2_init_hash(chain_id, factory)
    return deployer, init_hash


def compute_pool_address(deployer: str, init_hash: str, known: KnownPair) -> str:
    """Recompute the CREATE2 pool address in-process from deployer + init_hash.

    Uses the codebase's own auditable ``generate_v2/v3_pool_address`` (which
    builds the salt via native packed encoding + ``create2_address``), so the
    computation is the same code path production pool discovery uses.

    Returns:
        The checksummed CREATE2 pool address.

    """
    kind, t0, t1, fee = known
    if kind == "v2":
        return generate_v2_pool_address(deployer, (t0, t1), init_hash)
    return generate_v3_pool_address(deployer, (t0, t1), fee, init_hash)


# ---------------------------------------------------------------------------
# Expected factory interfaces per pool_type (Tier 2 fingerprints)
# ---------------------------------------------------------------------------
# Each value is a set of 4-byte selectors the deployed factory *must* expose.
# Selectors derived from the canonical deployed factories (queried via
# ``cast selectors`` on the bytecode + ``cast 4byte`` for signatures).

# Uniswap V2 family (ConstantProduct, V2-fork): getPair / createPair /
# allPairsLength — the canonical pair-factory interface shared by every
# `pool_type: "uniswap-v2"` deployment (Sushi, Pancake V2, Camelot, SwapBased…).
_UNISWAP_V2_SELECTORS = {
    "0xe6a43905",  # getPair(address,address)
    "0xc9c65396",  # createPair(address,address)
    "0x574f2ba3",  # allPairsLength()
}

# Uniswap V3 family (ConcentratedLiquidity, V3-fork): getPool(addr,addr,uint24)
# + feeAmountTickSpacing(uint24) + enableFeeAmount(uint24,int24) — shared by
# Uniswap V3 / Pancake V3 / Sushi V3 factories.
_UNISWAP_V3_SELECTORS = {
    "0x1698ee82",  # getPool(address,address,uint24)
    "0x22afcccb",  # feeAmountTickSpacing(uint24)
    "0x8a7c195f",  # enableFeeAmount(uint24,int24)
}

# Aerodrome V2 (Solidly-fork): getPool(addr,addr,bool) + createPool(addr,addr,bool)
# + isPool(addr). Distinct from the V2 pair-factory (bool stable flag).
_AERODROME_V2_SELECTORS = {
    "0x79bc57d5",  # getPool(address,address,bool)
    "0x36bf95a0",  # createPool(address,address,bool)
    "0x5b16ebb7",  # isPool(address)
}

# Aerodrome V3: getPool(addr,addr,int24) + createPool(addr,addr,int24,uint160)
# + isPool(addr). int24 tick-spacing (not uint24 fee) — distinct selector.
_AERODROME_V3_SELECTORS = {
    "0x28af8d0b",  # getPool(address,address,int24)
    "0x232aa5ac",  # createPool(address,address,int24,uint160)
    "0x5b16ebb7",  # isPool(address)
}

# Balancer V2 factory interface — universal across all revisions (rev1
# through rev6). The older rev1 factories (Weighted rev1 / Weighted2Tokens
# / Stable rev1) predate `getCreationCode()` / `getCreationCodeContracts()`
# (a rev2+ addition), so the robust cross-revision invariant is
# `isPoolFromFactory(address)` + `getPauseConfiguration()` — present in every
# Balancer factory from the original 2021 deployments to the latest rev6.
_BALANCER_FACTORY_SELECTORS = {
    "0x6634b753",  # isPoolFromFactory(address)
    "0x2da47c40",  # getPauseConfiguration()
}

# pool_type → expected selector set. A deployment's deployed bytecode must
# contain *all* selectors in its set (subset assertion).
EXPECTED_FACTORY_SELECTORS: dict[str, set[str]] = {
    "uniswap-v2": _UNISWAP_V2_SELECTORS,
    "uniswap-v3": _UNISWAP_V3_SELECTORS,
    "pancakeswap-v3": _UNISWAP_V3_SELECTORS,
    "sushiswap-v3": _UNISWAP_V3_SELECTORS,
    "aerodrome-v2": _AERODROME_V2_SELECTORS,
    "aerodrome-v3": _AERODROME_V3_SELECTORS,
    "balancer-weighted": _BALANCER_FACTORY_SELECTORS,
    "balancer-stable": _BALANCER_FACTORY_SELECTORS,
}


# ---------------------------------------------------------------------------
# Golden capture store
# ---------------------------------------------------------------------------


def row_key(chain_id: int, factory: str) -> str:
    """The capture-map key for one deployment row (factory lowercased)."""
    return f"{chain_id}:{factory.lower()}"


class DeploymentGoldenCapture:
    """Record/replay handle over the golden deployment-verification capture.

    One JSON file at :data:`GOLDEN_CAPTURE_PATH`, keyed per factory row.
    Record mode starts from the existing file (a partial or per-chain re-run
    *accumulates*, mirroring :class:`tests.golden.oracle.GoldenOracle`) and
    flushes after every row so an interrupted run still persists what it
    proved. Replay mode is strictly read-only and returns ``None`` for
    absent rows — the replay tests turn that into a loud skip.
    """

    def __init__(self, *, path: Path = GOLDEN_CAPTURE_PATH, mode: str) -> None:
        self._path = path
        self._recording = mode == "record"
        self._data = self._load()

    def _load(self) -> dict:
        if self._path.exists():
            return json.loads(self._path.read_text(encoding="utf-8"))
        return {"schema": SCHEMA_VERSION, "chains": {}, "rows": {}}

    def _flush(self) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        payload = dict(self._data)
        payload["schema"] = SCHEMA_VERSION
        payload["recorded_at"] = datetime.now(UTC).isoformat(timespec="seconds")
        self._path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def bind_chain(self, chain_id: int, rpc_url: str) -> None:
        """Record the provenance of a chain's rows (record mode only)."""
        self._data["chains"][str(chain_id)] = rpc_url

    def rows(self) -> dict[str, dict]:
        """All captured rows, keyed by :func:`row_key`."""
        return self._data["rows"]

    def chains(self) -> dict[str, str]:
        """The chains whose rows were captured at record time."""
        return self._data["chains"]

    def row(self, chain_id: int, factory: str) -> dict | None:
        """The captured row for a deployment, or None when unrecorded."""
        return self._data["rows"].get(row_key(chain_id, factory))

    def put_row(self, chain_id: int, factory: str, row: dict) -> None:
        """Persist one row's capture (record mode only)."""
        if not self._recording:
            msg = "put_row called on a replay-mode capture"
            raise AssertionError(msg)
        self._data["rows"][row_key(chain_id, factory)] = row
        self._flush()
