"""One-off compile of the canonical Uniswap sources with their pinned compilers.

`forge build` cannot resolve pre-0.8 solc in this environment, but nothing about
the contracts requires forge's compiler manager. The pinned binaries are fetched
into the `~/.local/share/svm` cache the tier3 harness scripts share and driven
through `py-solc-x`'s standard-JSON interface, which owns the argv plumbing and
the error surfacing. The emitted artifact shape is the one this lane already
deploys, so a test can call canonical bytecode exactly like a Vyper artifact.

Targets (the contract types the executor actually calls, plus the committed
SolReference math-harness artifact the cross-implementation ports deploy):
  UniswapV2Pair     solc 0.5.16   tier3-oracle/lib/v2-core
  UniswapV3Pool     solc 0.7.6    tier3-oracle/lib/v3-core
  UniswapV3Factory  solc 0.7.6    (the pool's deployer role)
  SolReference      solc 0.7.6    contracts/vyper/sol_reference (committed by
                    contracts/scripts/build-sol-reference-artifact.sh; rebuilt
                    here in this lane's artifact shape so the V3-library ports
                    deploy the original Solidity libraries next to the Vyper
                    TestHarness, as the V3 library differential suite does here)
  PoolManager       solc 0.8.26   tier3-oracle/lib/v4-core (the repository's
                    vendored v4-core, the same source the Tier-3 V4 oracle
                    harness compiles; no second PoolManager implementation)

PoolManager identity decisions, stated because the artifact is only as canonical
as the compile that produced it:

  - Source: `lib/v4-core/src/PoolManager.sol` plus everything it transitively
    imports from the `lib/v4-core` tree, including the vendored
    `lib/v4-core/lib/solmate` that `ProtocolFees` pulls `Owned` from. The digest
    tree is the whole `lib/v4-core` checkout, so a v4-core edit or a solmate
    bump reads as cold.
  - Compiler: solc 0.8.26 with its pinned long version — the version v4-core's
    own `foundry.toml` declares and the one the Tier-3 V4 harness fetches.
  - EVM fork: `cancun`, stated rather than defaulted. v4-core keeps its lock,
    nonzero-delta count, currency deltas and currency reserves in EIP-1153
    transient storage (`tstore`/`tload`), so a pre-cancun artifact would deploy
    and then fail every delta read.
  - Optimizer: this file's single optimizer profile (enabled, `runs: 200`), no
    via-IR. v4-core's own profile (`runs: 44444444`, via-IR) targets production
    gas, not source fidelity, and its bytecode would still not be a mainnet
    deployment's (metadata hash, constructor owner); what this lane needs from
    the artifact is the canonical source's behavior. Without via-IR the runtime
    is ~19.8 KB, inside the EIP-170 limit, so no size-limit override is needed.
  - Remappings: `solmate/=lib/v4-core/lib/solmate/`, the one import prefix the
    compile graph needs. `@openzeppelin` and `forge-std` stay unremapped because
    only v4-core's own test tree imports them, and that tree is not in this
    graph. The table is declared per target, not shared: remappings are compile
    settings, Solidity appends a settings metadata hash to the bytecode, and a
    shared table would therefore move the V2/V3/SolReference artifact bytes the
    moment a target was added.
"""

import contextlib
import hashlib
import json
import sys
from pathlib import Path

import requests
import solcx
from solcx.exceptions import SolcError

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
TIER3 = ROOT / "tier3-oracle"
OUT = HERE / "artifacts"
SVM = Path.home() / ".local/share/svm"
SOLC_BINARIES = "https://binaries.soliditylang.org/linux-amd64"

# Import-prefix tables, declared per target because they are part of the compile
# settings — and therefore of Solidity's appended metadata hash, i.e. of the
# artifact's bytes. The pre-V4 targets keep exactly the table they were built
# with, so adding a target cannot move their artifact identity; the V4 target
# declares only the prefix its graph needs (`ProtocolFees` pulls solmate's
# `Owned`).
V2_V3_REMAPPINGS = ["v2-core/=lib/v2-core/", "v3-core/=lib/v3-core/"]
V4_REMAPPINGS = ["solmate/=lib/v4-core/lib/solmate/"]

# (solc version, long version, evm version, entry source, contract names,
# digest tree, compile root, optimizer enabled, remappings)
# `evmVersion` is stated rather than defaulted for the v2/v3 cores: the compiler
# default moves between releases, and istanbul is the fork the deployed v2/v3
# bytecode targets (both solc 0.5.16 and 0.7.6 default to it). SolReference is
# compiled with the solc default (istanbul for 0.7.6), matching the committed
# artifact build, which states no evmVersion either.
TARGETS = [
    (
        "0.5.16",
        "0.5.16+commit.9c3226ce",
        "istanbul",
        "lib/v2-core/contracts/UniswapV2Factory.sol",
        ["UniswapV2Factory", "UniswapV2Pair"],
        "lib/v2-core",
        TIER3,
        True,
        V2_V3_REMAPPINGS,
    ),
    (
        "0.7.6",
        "0.7.6+commit.7338295f",
        "istanbul",
        "lib/v3-core/contracts/UniswapV3Factory.sol",
        ["UniswapV3Factory", "UniswapV3Pool"],
        "lib/v3-core",
        TIER3,
        True,
        V2_V3_REMAPPINGS,
    ),
    (
        "0.7.6",
        "0.7.6+commit.7338295f",
        None,
        "contracts/vyper/sol_reference/SolReference.sol",
        ["SolReference"],
        "contracts/vyper/sol_reference",
        ROOT,
        False,
        V2_V3_REMAPPINGS,
    ),
    (
        "0.8.26",
        "0.8.26+commit.8a97fa7a",
        "cancun",
        "lib/v4-core/src/PoolManager.sol",
        ["PoolManager"],
        "lib/v4-core",
        TIER3,
        True,
        V4_REMAPPINGS,
    ),
]


def solc_binary(version: str, long_version: str) -> Path:
    """The pinned solc binary, fetched once into the svm cache the harnesses share.

    The path (`~/.local/share/svm/<version>/solc-<version>`) and the pinned URL
    are the same ones `tier3-oracle/build-tier3-*-harness.sh` use, so the two
    lanes reuse each other's download instead of keeping a binary per toolchain.
    """
    path = SVM / version / f"solc-{version}"
    if path.is_file():
        return path
    print(f"fetching solc {version} …")
    response = requests.get(f"{SOLC_BINARIES}/solc-linux-amd64-v{long_version}", timeout=300)
    response.raise_for_status()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(response.content)
    path.chmod(0o755)
    return path


def _sources_digest(tree: Path) -> str:
    """Hash every `.sol` under the source tree the entry source belongs to.

    Fail loudly on a missing or .sol-free tree: an empty digest would let any
    source edit pass the cache check and serve stale artifacts.
    """
    if not tree.is_dir():
        print(f"digest tree {tree} does not exist", file=sys.stderr)
        raise SystemExit(1)
    # rglob can match forge cache/out directories named `X.sol`; keep files only.
    sources = sorted(path for path in tree.rglob("*.sol") if path.is_file())
    if not sources:
        print(f"digest tree {tree} contains no .sol files", file=sys.stderr)
        raise SystemExit(1)
    digest = hashlib.sha256()
    for path in sources:
        digest.update(path.read_bytes())
    return digest.hexdigest()


def _artifact_hash(path: Path) -> str | None:
    """Whole-file SHA-256 of a generated artifact, or None if unreadable."""
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def _cache_hit(recorded: object, digest: str, settings_digest: str, names: list[str]) -> bool:
    """A cache hit requires the recorded source digest, the recorded compile
    settings, AND every generated artifact to byte-match the hash recorded when
    it was written, so an edit anywhere in the source tree, a settings change
    (remappings and the EVM fork are compile inputs — Solidity folds them into
    the metadata hash it appends to the bytecode), or corruption anywhere in an
    artifact — ABI included — reads as cold. Stamps from the older schemas and
    malformed stamps do the same."""
    if not isinstance(recorded, dict) or recorded.get("sources") != digest:
        return False
    if recorded.get("settings") != settings_digest:
        return False
    hashes = recorded.get("artifacts")
    if not isinstance(hashes, dict):
        return False
    return all(hashes.get(name) == _artifact_hash(OUT / f"{name}.json") for name in names)


def compile_one(
    version: str,
    long_version: str,
    evm_version: str | None,
    entry: str,
    names: list[str],
    digest_tree: Path,
    compile_root: Path,
    remappings: list[str],
    *,
    optimize: bool,
    force: bool = False,
) -> None:
    stamp = HERE / ".cache" / "solc-stamps.json"
    stamp.parent.mkdir(parents=True, exist_ok=True)
    try:
        stamps = json.loads(stamp.read_text(encoding="utf-8")) if stamp.is_file() else {}
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        stamps = {}
    if not isinstance(stamps, dict):
        stamps = {}
    digest = _sources_digest(digest_tree)
    settings = {
        "optimizer": {"enabled": optimize, "runs": 200},
        "outputSelection": {
            "*": {"*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"]}
        },
        "remappings": remappings,
    }
    if evm_version is not None:
        settings["evmVersion"] = evm_version
    settings_digest = hashlib.sha256(
        json.dumps({"entry": entry, "settings": settings}, sort_keys=True).encode("utf-8")
    ).hexdigest()
    key = f"{version}:{evm_version}:{entry}"
    if not force and _cache_hit(stamps.get(key), digest, settings_digest, names):
        print(f"{entry} up to date (solc {version}, evm {evm_version})")
        return
    payload = {
        "language": "Solidity",
        "sources": {entry: {"urls": [entry]}},
        "settings": settings,
    }
    # pre-0.6.9 solc has no `--base-path`: imports resolve against the working
    # directory, so each compile is rooted at its target tree and allowed to read it.
    with contextlib.chdir(compile_root):
        try:
            raw = solcx.compile_standard(
                payload,
                solc_binary=solc_binary(version, long_version),
                allow_paths=".",
            )
        except SolcError as exc:
            print(f"solc {version}: {exc.message}", file=sys.stderr)
            raise SystemExit(1) from exc
    OUT.mkdir(parents=True, exist_ok=True)
    for name in names:
        hits = [
            (file, contracts[name])
            for file, contracts in raw["contracts"].items()
            if name in contracts
        ]
        if not hits:
            print(f"ERROR: {name} not in the {entry} compile graph", file=sys.stderr)
            raise SystemExit(1)
        source_file, inner = hits[0]
        # The artifact's source is repo-relative regardless of which tree the
        # compile was rooted in (tier3-oracle for the cores, the repo root for
        # SolReference).
        source = (compile_root / source_file).resolve().relative_to(ROOT.resolve()).as_posix()
        artifact = {
            "contractName": name,
            "source": source,
            "solc": version,
            "abi": inner["abi"],
            "bytecode": "0x" + inner["evm"]["bytecode"]["object"],
            "deployedBytecode": "0x" + inner["evm"]["deployedBytecode"]["object"],
        }
        (OUT / f"{name}.json").write_text(json.dumps(artifact, indent=2) + "\n")
        print(
            f"{name:20s} solc {version}  evm {evm_version}  "
            f"runtime {len(artifact['deployedBytecode']) // 2} bytes"
        )
    stamps[key] = {
        "sources": digest,
        "settings": settings_digest,
        "artifacts": {name: _artifact_hash(OUT / f"{name}.json") for name in names},
    }
    stamp.write_text(json.dumps(stamps, indent=2) + "\n")


def main() -> int:
    force = "--force" in sys.argv[1:]
    for (
        version,
        long_version,
        evm_version,
        entry,
        names,
        digest_tree,
        compile_root,
        optimize,
        remappings,
    ) in TARGETS:
        compile_one(
            version,
            long_version,
            evm_version,
            entry,
            names,
            # digest trees are declared relative to each target's compile root
            # (tier3-oracle for the core libraries, the repo root for
            # SolReference) — resolving against ROOT would look for
            # ROOT/lib/v2-core, which does not exist.
            compile_root / digest_tree,
            compile_root,
            remappings,
            optimize=optimize,
            force=force,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
