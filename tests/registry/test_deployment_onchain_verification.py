"""On-chain verification of shipped deployment addresses (cast harness).

Every factory address in ``registry/deployments.json`` is checked against the
chain it claims to live on, using Foundry's ``cast``. Three tiers, each
progressively stronger and more expensive:

**Tier 1 — bytecode presence** (default, cheapest, key-less).
    ``cast code <factory> --rpc-url <chain_rpc>`` returns non-empty bytecode
    (length > 2, i.e. not the bare ``"0x"`` of an empty account / EOA). This is
    the check that caught the Balancer factory corruption — three addresses
    that were *never deployed contracts* (zero on-chain bytecode, zero Etherscan
    tx history), wholesale-fabricated placeholders that looked plausible but
    resolved to nothing. Runs against a local RPC; no API key needed.

**Tier 2 — selector fingerprint** (opt-in via ``DEGENBOT_VERIFY_DEPLOYMENTS=2``).
    The deployed bytecode is decoded into its 4-byte function selectors
    (``cast selectors``) and asserted to expose the *factory interface* expected
    for the deployment's ``pool_type`` (e.g. a ``uniswap-v2`` factory must expose
    ``getPair(address,address)`` + ``createPair(address,address)``; a
    ``_balancer-weighted`` factory must expose ``getCreationCode()`` +
    ``isPoolFromFactory(address)``). A fabricated address that happens to land on
    *some* deployed contract still fails here unless it impersonates the right
    factory interface. Key-less (selectors resolve via ``cast 4byte`` /
    openchain.xyz, but the assertion is on raw 4-byte presence, so no network
    beyond the RPC is strictly required).

**Tier 3 — Etherscan source name** (opt-in via ``DEGENBOT_VERIFY_DEPLOYMENTS=3``,
    requires ``ETHERSCAN_API_KEY``). ``getsourcecode`` returns the verified
    Solidity ``ContractName``, asserted to match the expected name for the
    ``pool_type`` (e.g. ``WeightedPoolFactory``, ``UniswapV2Factory``). The
    deepest signal — but unreliable for unverified contracts and unavailable
    without an API key.

**Tier 4 — init_code_hash reproduces pool address** (opt-in via
    ``DEGENBOT_VERIFY_DEPLOYMENTS=4``). The CREATE2 ``init_code_hash`` is the
    one field that's silently breakable and unverified by Tiers 1-3: a wrong
    hash means CREATE2 yields the wrong pair/pool address — silently broken
    pool discovery. This tier recomputes the CREATE2 address of a *known
    deployed* pair/pool in-process (the codebase's own auditable
    ``generate_v2/v3_pool_address``) and asserts it equals the address the
    on-chain factory reports via ``getPair`` / ``getPool`` — proving the stored
    hash reproduces ground truth. Factories without a registered known pair
    skip (not fail); a zero address (pair absent) skips too. Key-less.

Running
-------
The whole module is marked ``online_rpc`` and so is **deselected by default**
(``pyproject.toml`` → ``-m "not slow and not base and not online_rpc"``) — CI
runs offline and never needs a node. Run on demand against a local/mainnet RPC::

    just test-python -m online_rpc tests/registry/test_deployment_onchain_verification.py

Escalate tiers with the env var (default ``1``)::

    # Tier 2 (+selector fingerprint)
    DEGENBOT_VERIFY_DEPLOYMENTS=2 just test-python -m online_rpc \
        tests/registry/test_deployment_onchain_verification.py
    # Tier 3 (+Etherscan source name; needs key)
    DEGENBOT_VERIFY_DEPLOYMENTS=3 ETHERSCAN_API_KEY=... just test-python \
        -m online_rpc tests/registry/test_deployment_onchain_verification.py
    # Tier 4 (+init_code_hash reproduces pool address)
    DEGENBOT_VERIFY_DEPLOYMENTS=4 just test-python -m online_rpc \
        tests/registry/test_deployment_onchain_verification.py

Per-chain rows skip individually when their chain's RPC is unreachable (e.g.
on a machine with only a local Ethereum node, the Base/Arbitrum rows skip).
"""

from __future__ import annotations

import os
import subprocess  # noqa: S404
from functools import cache

import pytest

from degenbot.registry.deployment_loader import DeploymentRecord, load_deployments
from tests.conftest import (
    ARBITRUM_FULL_NODE_HTTP_URI,
    BASE_ARCHIVE_NODE_HTTP_URI,
    ETHEREUM_ARCHIVE_NODE_HTTP_URI,
)

# ---------------------------------------------------------------------------
# Tier selection
# ---------------------------------------------------------------------------

_VERIFY_TIER = int(os.environ.get("DEGENBOT_VERIFY_DEPLOYMENTS", "1"))
"""Verification tier: 1 = bytecode presence (default), 2 = +selector
fingerprint, 3 = +Etherscan source ContractName, 4 = +init_code_hash
reproduces pool address."""

_ETHERSCAN_API_KEY = os.environ.get("ETHERSCAN_API_KEY", "")


def _tier(gte: int) -> bool:
    """True when the active verification tier is >= ``gte``."""
    return gte <= _VERIFY_TIER


# ---------------------------------------------------------------------------
# Per-chain RPC resolution + reachability
# ---------------------------------------------------------------------------

# Mirrors tests/conftest.py URIs (env var → tests.env → built-in default).
# The local Ethereum node (tests.env → http://localhost:8545) covers chain 1;
# Base/Arbitrum default to public RPCs and skip when unreachable.
_CHAIN_RPC: dict[int, str] = {
    1: ETHEREUM_ARCHIVE_NODE_HTTP_URI,
    8453: BASE_ARCHIVE_NODE_HTTP_URI,
    42161: ARBITRUM_FULL_NODE_HTTP_URI,
}


@cache
def _rpc_reachable(rpc_url: str) -> bool:
    """Quick ``cast block-number`` to confirm the RPC responds.

    Cached per RPC URL so the 32-row parametrization makes one reachability
    check per *chain*, not per row.
    """
    try:
        subprocess.run(  # noqa: S603 — trusted binary, no shell
            ["cast", "block-number", "--rpc-url", rpc_url],  # noqa: S607
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return False
    return True


def _skip_if_unreachable(chain_id: int) -> str | None:
    """Return a skip reason if ``chain_id``'s RPC is unreachable, else None."""
    entry = _CHAIN_RPC.get(chain_id)
    if entry is None:
        return f"no RPC mapping for chain_id={chain_id} (harness covers {sorted(_CHAIN_RPC)})"
    if not _rpc_reachable(entry):
        return f"chain_id={chain_id} RPC ({entry}) not reachable — set tests.env / env var"
    return None


# ---------------------------------------------------------------------------
# cast helpers
# ---------------------------------------------------------------------------

def _cast(*args: str, timeout: int = 30) -> str:
    """Run ``cast`` with the given args, return stdout, raising on failure."""
    result = subprocess.run(  # noqa: S603 — trusted binary, args list, no shell
        ["cast", *args],  # noqa: S607
        check=True,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return result.stdout.strip()


def _cast_code(factory: str, rpc_url: str) -> str:
    """Return the raw bytecode hex (``0x…``) at ``factory`` via ``cast code``."""
    return _cast("code", factory, "--rpc-url", rpc_url)


def _cast_selectors(bytecode: str) -> set[str]:
    """Return the set of 4-byte selectors embedded in ``bytecode``.

    ``cast selectors`` prints ``<selector>  <offset>`` lines; we keep the
    selector column and lowercase it.
    """
    out = _cast("selectors", bytecode)
    return {line.split()[0].lower() for line in out.splitlines() if line.strip()}


def _etherscan_source_name(chain_id: int, factory: str) -> str | None:
    """Fetch the verified ``ContractName`` from Etherscan, or None if unverified."""
    key = _ETHERSCAN_API_KEY
    if not key:
        pytest.skip("Tier 3 requires ETHERSCAN_API_KEY")  # pragma: no cover
    import json
    import urllib.request

    url = (
        f"https://api.etherscan.io/v2/api?chainid={chain_id}"
        f"&module=contract&action=getsourcecode&address={factory}&apikey={key}"
    )
    with urllib.request.urlopen(url, timeout=30) as resp:
        data = json.loads(resp.read())
    result = data.get("result")
    if not isinstance(result, list) or not result:
        return None
    name = result[0].get("ContractName")
    return name if isinstance(name, str) and name else None


# ---------------------------------------------------------------------------
# Tier 4 — CREATE2 init_code_hash verification helpers
# ---------------------------------------------------------------------------
# Known deployed pairs/pools, one per factory we can verify the init_hash
# against. Keyed by (chain_id, lowercase factory). Each entry is a uniform
# 4-tuple ``(kind, token0, token1, fee)`` — ``kind`` is "v2" or "v3";
# ``fee`` is the V3 fee tier in pips (unused for v2, kept 0). The token order
# doesn't matter (both the in-process generator and the on-chain
# getPair/getPool sort internally). If getPair/getPool returns the zero address
# (pair not deployed), the test skips that factory.
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

KNOWN_PAIRS: dict[tuple[int, str], tuple[str, str, str, int]] = {
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


def _is_zero_address(addr: str) -> bool:
    """True if ``addr`` is the zero address (0x0…0)."""
    cleaned = addr.lower().removeprefix("0x")
    return not cleaned or int(cleaned, 16) == 0


def _onchain_pool_address(
    factory: str, known: tuple[str, str, str, int], rpc_url: str
) -> str:
    """Query the factory on-chain for the known pair's address.

    Returns:
        The ``getPair``/``getPool`` result (checksummed by cast). For V2 the
        signature is ``getPair(address,address)``; for V3
        ``getPool(address,address,uint24)``.

    """
    kind, t0, t1, fee = known
    if kind == "v2":
        return _cast(
            "call", factory, "getPair(address,address)(address)",
            t0, t1, "--rpc-url", rpc_url,
        )
    return _cast(
        "call", factory, "getPool(address,address,uint24)(address)",
        t0, t1, str(fee), "--rpc-url", rpc_url,
    )


def _compute_pool_address(
    deployer: str, init_hash: str, known: tuple[str, str, str, int]
) -> str:
    """Recompute the CREATE2 pool address in-process from deployer + init_hash.

    Uses the codebase's own auditable ``generate_v2/v3_pool_address`` (which
    builds the salt via ``eth_abi`` + ``create2_address``), so the computation
    is the same code path production pool discovery uses.
    """
    from degenbot.uniswap.v2_functions import generate_v2_pool_address
    from degenbot.uniswap.v3_functions import generate_v3_pool_address

    kind, t0, t1, fee = known
    if kind == "v2":
        return generate_v2_pool_address(deployer, (t0, t1), init_hash)
    return generate_v3_pool_address(deployer, (t0, t1), fee, init_hash)


# ---------------------------------------------------------------------------
# Expected factory interfaces per pool_type (Tier 2 fingerprints)
# ---------------------------------------------------------------------------
# Each value is a set of 4-byte selectors the deployed factory *must* expose.
# Selectors derived from the canonical deployed factories (queried this session
# via ``cast selectors`` on the bytecode + ``cast 4byte`` for signatures).

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

# Tier 3 — expected verified ContractName substring per pool_type. None entries
# skip the name assertion (the contract is still asserted deployed via Tiers
# 1-2). Names verified via Etherscan getsourcecode this session.
EXPECTED_CONTRACT_NAME: dict[str, str | None] = {
    "uniswap-v2": "Factory",
    "uniswap-v3": "UniswapV3Factory",
    "pancakeswap-v3": "PancakeV3Factory",
    "sushiswap-v3": "UniswapV3Factory",  # Sushi V3 forks the V3 factory contract
    "aerodrome-v2": "PoolFactory",
    "aerodrome-v3": "PoolFactory",
    "balancer-weighted": "WeightedPool",  # WeightedPoolFactory / WeightedPool2TokensFactory
    "balancer-stable": None,  # StablePoolFactory OR ComposableStablePoolFactory
}


# ---------------------------------------------------------------------------
# Parametrization
# ---------------------------------------------------------------------------

_RECORDS: list[DeploymentRecord] = load_deployments()
"""All shipped deployments, loaded once at collection time."""

_RECORD_IDS = [
    f"{r.chain_id}-{r.factory[:10]}…-{r.name}" for r in _RECORDS
]


pytestmark = pytest.mark.online_rpc


@pytest.mark.online_rpc
@pytest.mark.parametrize(
    "record",
    _RECORDS,
    ids=_RECORD_IDS,
)
class TestDeploymentOnchainVerification:
    """Verify every shipped deployment address against its chain."""

    # --- Tier 1: bytecode presence ---------------------------------------

    def test_tier1_factory_has_bytecode(self, record: DeploymentRecord) -> None:
        """Tier 1: the factory address resolves to deployed bytecode.

        This is the cheapest, key-less check and the one that catches
        fabricated / typo'd / never-deployed addresses. ``cast code`` returns
        ``"0x"`` (length 2) for empty accounts; any deployed contract returns
        a longer hex string.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        rpc_url = _CHAIN_RPC[record.chain_id]
        code = _cast_code(record.factory, rpc_url)
        assert len(code) > 2, (
            f"{record.name} ({record.factory}, chain {record.chain_id}) has no "
            f"bytecode — `cast code` returned {code!r}. The address is not a "
            f"deployed contract. This is the Balancer-corruption bug class: a "
            f"plausible-looking address that resolves to nothing on-chain."
        )

    # --- Tier 2: selector fingerprint ------------------------------------

    @pytest.mark.skipif(not _tier(2), reason="Tier 2 needs DEGENBOT_VERIFY_DEPLOYMENTS>=2")
    def test_tier2_factory_selector_fingerprint(self, record: DeploymentRecord) -> None:
        """Tier 2: the deployed bytecode exposes the expected factory interface.

        The factory's 4-byte selector set (decoded from its bytecode via
        ``cast selectors``) must be a *superset* of the canonical selectors for
        its ``pool_type``. A fabricated address that happened to land on some
        unrelated deployed contract fails here unless it impersonates the right
        factory interface.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        expected = EXPECTED_FACTORY_SELECTORS.get(record.pool_type)
        if expected is None:
            pytest.skip(
                f"no selector fingerprint defined for pool_type={record.pool_type!r}"
            )
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployed = _cast_selectors(_cast_code(record.factory, rpc_url))
        missing = expected - deployed
        assert not missing, (
            f"{record.name} ({record.factory}, chain {record.chain_id}, "
            f"pool_type={record.pool_type!r}) is missing expected factory "
            f"selectors {sorted(missing)}. Deployed selectors: {sorted(deployed)[:12]}…"
        )

    # --- Tier 3: Etherscan source ContractName --------------------------

    @pytest.mark.skipif(not _tier(3), reason="Tier 3 needs DEGENBOT_VERIFY_DEPLOYMENTS>=3")
    def test_tier3_etherscan_contract_name(self, record: DeploymentRecord) -> None:
        """Tier 3: the Etherscan-verified ContractName matches the pool_type.

        The deepest signal — the verified Solidity source's contract name.
        Unverified contracts (ContractName=="") are skipped, not failed: source
        verification is optional and absent for some legitimate factories.
        """
        if not _ETHERSCAN_API_KEY:
            pytest.skip("Tier 3 requires ETHERSCAN_API_KEY")
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        name = _etherscan_source_name(record.chain_id, record.factory)
        expected_substring = EXPECTED_CONTRACT_NAME.get(record.pool_type)
        if expected_substring is None:
            pytest.skip(
                f"no expected ContractName defined for pool_type={record.pool_type!r}"
            )
        if not name:
            pytest.skip(
                f"{record.name} ({record.factory}) source not verified on Etherscan"
            )
        assert expected_substring.lower() in name.lower(), (
            f"{record.name} ({record.factory}, chain {record.chain_id}) "
            f"Etherscan ContractName={name!r} does not contain "
            f"{expected_substring!r} (expected for pool_type={record.pool_type!r})"
        )

    # --- Tier 4: init_code_hash reproduces pool address ----------------

    @pytest.mark.skipif(not _tier(4), reason="Tier 4 needs DEGENBOT_VERIFY_DEPLOYMENTS>=4")
    def test_tier4_init_hash_reproduces_pool_address(
        self, record: DeploymentRecord
    ) -> None:
        """Tier 4: the stored CREATE2 ``init_code_hash`` reproduces a real pool.

        Recomputes the CREATE2 address of a *known deployed* pair/pool
        in-process (the codebase's auditable ``generate_v2/v3_pool_address``
        using the JSON's ``init_hash`` + ``deployer``) and asserts it equals the
        address the on-chain factory reports via ``getPair`` / ``getPool``.

        This is the only tier that exercises ``init_code_hash`` — the field
        silently breakable when wrong (a wrong hash → wrong CREATE2 address →
        pool lookups silently miss). Skips when the factory has no registered
        known pair, when the pair isn't deployed (on-chain returns zero), or
        when the deployment has no init_hash.
        """
        skip = _skip_if_unreachable(record.chain_id)
        if skip is not None:
            pytest.skip(skip)
        if not record.init_hash:
            pytest.skip(f"{record.name}: no init_hash (CREATE2 not used)")
        known = KNOWN_PAIRS.get((record.chain_id, record.factory.lower()))
        if known is None:
            pytest.skip(
                f"{record.name}: no known pair registered for this factory — "
                "add one to KNOWN_PAIRS to cover it"
            )
        rpc_url = _CHAIN_RPC[record.chain_id]
        deployer = record.deployer or record.factory
        on_chain = _onchain_pool_address(record.factory, known, rpc_url)
        if _is_zero_address(on_chain):
            pytest.skip(
                f"{record.name}: getPair/getPool returned zero — "
                "the known pair is not deployed on this chain/factory"
            )
        computed = _compute_pool_address(deployer, record.init_hash, known)
        assert computed == on_chain, (
            f"{record.name} ({record.factory}, chain {record.chain_id}): "
            f"CREATE2 with stored init_hash produced {computed}, but the "
            f"factory reports {on_chain} for the known pair. The stored "
            f"init_code_hash does not reproduce ground truth."
        )
