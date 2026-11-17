"""One-off compile of the canonical Uniswap sources with their pinned compilers.

`forge build` cannot resolve pre-0.8 solc in this environment, but nothing about
the contracts requires forge's compiler manager. The pinned binaries are fetched
into the `~/.local/share/svm` cache the tier3 harness scripts share and driven
through `py-solc-x`'s standard-JSON interface, which owns the argv plumbing and
the error surfacing. The emitted artifact shape is the one this lane already
deploys, so a test can call canonical bytecode exactly like a Vyper artifact.

Targets (the contract types the executor actually calls):
  UniswapV2Pair     solc 0.5.16   tier3-oracle/lib/v2-core
  UniswapV3Pool     solc 0.7.6    tier3-oracle/lib/v3-core
  UniswapV3Factory  solc 0.7.6    (the pool's deployer role)
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

# (solc version, long version, evm version, entry source, contract names)
# `evmVersion` is stated rather than defaulted: the compiler default moves
# between releases, and istanbul is the fork the deployed v2/v3 bytecode targets
# (both solc 0.5.16 and 0.7.6 default to it).
TARGETS = [
    (
        "0.5.16",
        "0.5.16+commit.9c3226ce",
        "istanbul",
        "lib/v2-core/contracts/UniswapV2Factory.sol",
        ["UniswapV2Factory", "UniswapV2Pair"],
    ),
    (
        "0.7.6",
        "0.7.6+commit.7338295f",
        "istanbul",
        "lib/v3-core/contracts/UniswapV3Factory.sol",
        ["UniswapV3Factory", "UniswapV3Pool"],
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


def _sources_digest(entry: str) -> str:
    """Hash every `.sol` under the library the entry source belongs to."""
    lib = TIER3 / "lib" / entry.split("/")[1]
    digest = hashlib.sha256()
    for path in sorted(lib.rglob("*.sol")):
        digest.update(path.read_bytes())
    return digest.hexdigest()


def _artifact_hash(path: Path) -> str | None:
    """Whole-file SHA-256 of a generated artifact, or None if unreadable."""
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def _cache_hit(recorded: object, digest: str, names: list[str]) -> bool:
    """A cache hit requires the recorded source digest AND every generated
    artifact to byte-match the hash recorded when it was written, so corruption
    anywhere in an artifact — ABI included — reads as cold. Stamps from the old
    existence-only schema and malformed stamps do the same."""
    if not isinstance(recorded, dict) or recorded.get("sources") != digest:
        return False
    hashes = recorded.get("artifacts")
    if not isinstance(hashes, dict):
        return False
    return all(hashes.get(name) == _artifact_hash(OUT / f"{name}.json") for name in names)


def compile_one(
    version: str,
    long_version: str,
    evm_version: str,
    entry: str,
    names: list[str],
    *,
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
    digest = _sources_digest(entry)
    key = f"{version}:{evm_version}:{entry}"
    if not force and _cache_hit(stamps.get(key), digest, names):
        print(f"{entry} up to date (solc {version}, evm {evm_version})")
        return
    payload = {
        "language": "Solidity",
        "sources": {entry: {"urls": [entry]}},
        "settings": {
            "optimizer": {"enabled": True, "runs": 200},
            "evmVersion": evm_version,
            "outputSelection": {
                "*": {"*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"]}
            },
            "remappings": ["v2-core/=lib/v2-core/", "v3-core/=lib/v3-core/"],
        },
    }
    # pre-0.6.9 solc has no `--base-path`: imports resolve against the working
    # directory, so the compile is rooted at tier3-oracle and allowed to read it.
    with contextlib.chdir(TIER3):
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
        artifact = {
            "contractName": name,
            "source": f"tier3-oracle/{source_file}",
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
        "artifacts": {name: _artifact_hash(OUT / f"{name}.json") for name in names},
    }
    stamp.write_text(json.dumps(stamps, indent=2) + "\n")


def main() -> int:
    force = "--force" in sys.argv[1:]
    for version, long_version, evm_version, entry, names in TARGETS:
        compile_one(version, long_version, evm_version, entry, names, force=force)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
