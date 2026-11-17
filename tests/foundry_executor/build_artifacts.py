"""Compile the Vyper sources with the repository's pinned vyper into forge-readable artifacts.

The Foundry lane does not use forge's own Vyper compiler: forge's import resolver
only understands ``from <dir> import <module>.vy`` and cannot resolve this tree's
``from .interfaces.UniswapV4 import IPoolManager`` name-imports of ``.vyi``
interface files. Compiling through the ``vyper`` module keeps this lane on the
same compiler identity as the release artifacts
(``just verify-tier3-executor-artifact``).
"""

import hashlib
import json
import sys
import warnings
from pathlib import Path

import vyper
from vyper.exceptions import VyperException
from vyper.warnings import VyperWarning

# `vyper` is a direct repository dependency (pyproject: `vyper == 0.5.0b2`), so the
# module imported here is the pinned compiler every other lane certifies against.
OUTPUT_FORMATS = ["abi", "bytecode", "bytecode_runtime"]

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
VYPER_DIR = ROOT / "contracts" / "vyper"
OUT = HERE / "artifacts"

SOURCES = [
    "cmd_executor.vy",
    "fake_erc20.vy",
    "fake_weth.vy",
    "fake_uniswap_v2_pair.vy",
    "fake_uniswap_v3_pool.vy",
    "fake_uniswap_v4_pool_manager.vy",
    "echo_callee.vy",
    "reentering_destination.vy",
    "test_harness.vy",
]


def compile_source(path: Path) -> dict:
    """Compile one Vyper source into its ABI and creation/runtime bytecode.

    `contract_path` is what resolves this tree's imports: vyper anchors
    ``from .interfaces.X import I`` on the source file's own directory, which is
    the resolution forge's Vyper import resolver cannot perform.
    """
    with warnings.catch_warnings():
        # The fixtures log events positionally and read immutables unqualified;
        # vyper's deprecation warnings for both are expected here.
        warnings.simplefilter("ignore", VyperWarning)
        return vyper.compile_code(
            path.read_text(encoding="utf-8"),
            contract_path=path,
            output_formats=OUTPUT_FORMATS,
        )


def check_release_identity() -> int:
    """Compare the fresh `CmdExecutor` compile with the tracked release artifact.

    A second gate on the same fact as `verify-tier3-executor-artifact` would be
    a duplicate; this is the diagnostic that says whether THIS lane's taken
    bytecode is the released bytecode.
    """
    release = ROOT / "rust/crates/foundation/degenbot-executor/releases/v2/artifacts"
    out = {
        "creation": release / "cmd_executor.creation.hex",
        "runtime": release / "cmd_executor.runtime.hex",
    }
    fresh = json.loads((OUT / "CmdExecutor.json").read_text())
    fields = {"creation": "bytecode", "runtime": "deployedBytecode"}
    rc = 0
    for label, path in out.items():
        tracked = path.read_text().strip().lower().removeprefix("0x")
        ours = fresh[fields[label]].strip().lower().removeprefix("0x")
        same = tracked == ours
        print(f"{label}: {'identical' if same else 'DIVERGED'} ({len(ours)} hex chars)")
        rc |= 0 if same else 1
    return rc


def _inputs_digest() -> str:
    """Digest every Vyper input the compiles can transitively pull in.

    The nine top-level SOURCES are not the whole input: they import `.vyi`
    interface files (e.g. `from .interfaces.UniswapV4 import IPoolManager`), so
    a cache keyed on the top-level sources alone would accept artifacts built
    from a stale interface. Hash the entire contracts/vyper `.vy`/`.vyi` tree
    instead — conservative and deterministic.
    """
    digest = hashlib.sha256()
    inputs = sorted(
        path for path in VYPER_DIR.rglob("*") if path.is_file() and path.suffix in (".vy", ".vyi")
    )
    for path in inputs:
        digest.update(path.relative_to(ROOT).as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
    return digest.hexdigest()


def _up_to_date(manifest_path: Path, vyper_version: str) -> bool:
    """A cache hit requires every generated artifact to byte-match the hash
    recorded when it was written, so corruption anywhere in an artifact — ABI
    included — reads as cold. Malformed manifests and unreadable files do the
    same; validation never raises."""
    if not manifest_path.is_file():
        return False
    try:
        existing = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        return False
    if not isinstance(existing, dict):
        return False
    if existing.get("__vyper__") != vyper_version:
        return False
    try:
        inputs_digest = _inputs_digest()
    except OSError:
        return False
    if existing.get("__inputs__") != inputs_digest:
        return False
    for src in SOURCES:
        if _artifact_name(src) not in existing:
            return False
    for name, entry in existing.items():
        if not isinstance(entry, dict):
            continue  # the __vyper__ / __inputs__ markers
        artifact_path = OUT / f"{name}.json"
        try:
            artifact_sha256 = hashlib.sha256(artifact_path.read_bytes()).hexdigest()
        except OSError:
            return False
        if entry.get("artifact_sha256") != artifact_sha256:
            return False
    return True


def _artifact_name(src: str) -> str:
    return "".join(part.capitalize() for part in Path(src).stem.split("_"))


def main() -> int:
    args = sys.argv[1:]
    vyper_version = vyper.__long_version__
    if "--force" not in args and _up_to_date(OUT / "manifest.json", vyper_version):
        print(f"artifacts up to date ({vyper_version})")
        if "--check-release" in args:
            return check_release_identity()
        return 0
    compiled = {}
    for src in SOURCES:
        try:
            compiled[src] = compile_source(VYPER_DIR / src)
        except VyperException as exc:
            print(exc, file=sys.stderr)
            return 1
    OUT.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for src, entry in compiled.items():
        name = _artifact_name(src)
        artifact = {
            "contractName": name,
            "source": f"contracts/vyper/{src}",
            "abi": entry["abi"],
            "bytecode": entry["bytecode"],
            "deployedBytecode": entry["bytecode_runtime"],
        }
        payload = (json.dumps(artifact, indent=2) + "\n").encode("utf-8")
        (OUT / f"{name}.json").write_bytes(payload)
        manifest[name] = {
            "source": f"contracts/vyper/{src}",
            "source_sha256": hashlib.sha256((VYPER_DIR / src).read_bytes()).hexdigest(),
            "bytecode_sha256": hashlib.sha256(bytes.fromhex(entry["bytecode"][2:])).hexdigest(),
            "runtime_sha256": hashlib.sha256(
                bytes.fromhex(entry["bytecode_runtime"][2:])
            ).hexdigest(),
            "artifact_sha256": hashlib.sha256(payload).hexdigest(),
        }
    manifest["__vyper__"] = vyper_version
    manifest["__inputs__"] = _inputs_digest()
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    for name, info in manifest.items():
        if not isinstance(info, dict):
            print(f"{name}: {info}")
            continue
        print(f"{name:34s} runtime_sha256={info['runtime_sha256'][:16]}")
    if "--check-release" in args:
        return check_release_identity()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
