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
import re
import sys
import warnings
from pathlib import Path

import vyper
from eth_utils import keccak
from vyper.exceptions import VyperException
from vyper.warnings import VyperWarning

# `vyper` is a direct repository dependency (pyproject: `vyper == 0.5.0b2`), so the
# module imported here is the pinned compiler every other lane certifies against.
OUTPUT_FORMATS = ["abi", "bytecode", "bytecode_runtime", "source_map_runtime"]

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
    "ExttloadComparator.vy",
]

# The release artifact family this lane's executor compile must agree with.
RELEASE_ARTIFACTS = ROOT / "rust/crates/foundation/degenbot-executor/releases/v2/artifacts"

# The compiler revert-label vocabulary the error map can carry, and what each
# label reverts with. The compiler-emitted checks (bare `assert`, safe-math
# guards) revert with empty revert data or the INVALID opcode — no selectable
# identity. Only "user revert with custom error" PCs push a 4-byte selector,
# and those selectors are the executor's own ABI errors, listed in the
# generated Errors.sol. A label outside this table fails the build loudly
# rather than silently shrinking the generated vocabulary.
COMPILER_REVERT_LABELS = {
    "user assert": "empty-or-invalid",
    "safediv": "empty-or-invalid",
    "safemul": "empty-or-invalid",
    "safemod": "empty-or-invalid",
    "user revert with custom error": "custom",
}


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


def _executor_error_map() -> dict:
    """The tracked release error map (PC -> compiler revert label).

    The release source and contracts/vyper/cmd_executor.vy are the same file
    (same sha256, pinned in the release manifest), so this is the error map of
    the bytecode this lane deploys.
    """
    return json.loads((RELEASE_ARTIFACTS / "cmd_executor.error_map.json").read_text())


def _error_signature(entry: dict) -> str:
    return f"{entry['name']}({','.join(i['type'] for i in entry['inputs'])})"


def _constant_name(error_name: str) -> str:
    # CamelCase error name -> SCREAMING_SNAKE constant (InvalidCallback -> INVALID_CALLBACK).
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", error_name).upper()


def _error_selector(entry: dict) -> str:
    return "0x" + keccak(text=_error_signature(entry))[:4].hex()


def check_error_map_vocabulary(compiled: dict) -> int:
    """Cross-check the fresh compile's revert-label vocabulary against the
    tracked release error map.

    The generated Errors.sol encodes the tracked vocabulary; if a rebuild
    produces labels the tracked map does not carry, the lane's revert-identity
    substrate would silently disagree with the release artifact, so this fails
    loudly instead. (Per-PC assignments are a code-layout fact already pinned
    by the bytecode identity check; the vocabulary is the fact this generator
    consumes.)
    """
    fresh_labels = sorted(
        str(v) for v in compiled["cmd_executor.vy"]["source_map_runtime"]["error_map"].values()
    )
    tracked_labels = sorted(str(v) for v in _executor_error_map().values())
    if fresh_labels != tracked_labels:
        print(
            "error_map vocabulary DIVERGED from the release artifact: "
            f"fresh={fresh_labels} tracked={tracked_labels}",
            file=sys.stderr,
        )
        return 1
    return 0


def generate_errors_module() -> None:
    """Emit src/generated/Errors.sol from the executor's revert-identity inputs.

    Selector tables are generated, never hand-written a second time: custom
    error selectors come from the built CmdExecutor artifact ABI, and the
    compiler revert-label vocabulary comes from the tracked release error map
    (cross-checked against the fresh compile on every rebuild). The output is
    byte-deterministic for a given artifact.
    """
    tracked = _executor_error_map()
    labels = sorted({str(v) for v in tracked.values()})
    unknown = [label for label in labels if label not in COMPILER_REVERT_LABELS]
    if unknown:
        print(
            f"unknown compiler revert labels {unknown} in cmd_executor.error_map.json",
            file=sys.stderr,
        )
        raise SystemExit(1)
    abi = json.loads((OUT / "CmdExecutor.json").read_text())["abi"]
    errors = [
        (e["name"], _error_signature(e), _error_selector(e)) for e in abi if e["type"] == "error"
    ]

    selector_lines = "\n".join(
        f"    bytes4 internal constant {_constant_name(name)} = {sel}; // {sig}"
        for name, sig, sel in errors
    )
    name_lines = "\n".join(
        f'        if (selector == {_constant_name(name)}) return "{name}";' for name, _, _ in errors
    )
    label_lines = "\n".join(f"//   {label} -> {COMPILER_REVERT_LABELS[label]}" for label in labels)
    body = ERRORS_TEMPLATE.format(
        label_lines=label_lines, selector_lines=selector_lines, name_lines=name_lines
    )
    (HERE / "src" / "generated" / "Errors.sol").write_text(body)
    print(f"src/generated/Errors.sol: {len(errors)} executor errors, {len(labels)} compiler labels")


ERRORS_TEMPLATE = """// SPDX-License-Identifier: MIT

// Generated from the pinned-vyper CmdExecutor compile by build_artifacts.py — do not edit.
//
// The executor lane's revert-identity vocabulary, generated rather than
// maintained by hand a second time:
//   - custom-error selectors come from artifacts/CmdExecutor.json's ABI entries;
//   - the compiler revert-label vocabulary comes from the tracked release
//     cmd_executor.error_map.json, cross-checked against this lane's fresh
//     compile on every artifact rebuild.
//
// Compiler revert-label vocabulary (cmd_executor.error_map.json):
{label_lines}
//
// The compiler-emitted check labels revert with empty revert data or the
// INVALID opcode — they carry no selector. Only the executor's `raise`
// statements produce selectable custom errors, listed below.
library ExecutorErrors {{
    // Standard revert envelopes.
    bytes4 internal constant ERROR_STRING = 0x08c379a0; // Error(string)
    bytes4 internal constant PANIC = 0x4e487b71; // Panic(uint256)
    uint256 internal constant PANIC_ARITHMETIC = 0x11;

{selector_lines}

    /// Name a revert selector from the generated vocabulary, or "unknown".
    function nameOf(bytes4 selector) internal pure returns (string memory) {{
        if (selector == ERROR_STRING) return "(Error)";
        if (selector == PANIC) return "(Panic)";
{name_lines}
        return "unknown";
    }}
}}
"""


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

    The top-level SOURCES are not the whole input: they import `.vyi`
    interface files (e.g. `from .interfaces.UniswapV4 import IPoolManager`), so
    a cache keyed on the top-level sources alone would accept artifacts built
    from a stale interface. Hash the entire contracts/vyper `.vy`/`.vyi` tree
    instead — conservative and deterministic.

    The tracked release error map is a cache input too: on a cache hit there
    is no fresh compile to validate the revert-label vocabulary against, and
    `generate_errors_module` consumes the tracked map directly. Including it
    here means any edit to it forces a cold rebuild, so the vocabulary is
    re-validated against the fresh compile instead of silently trusted — while
    a hand edit still regenerates Errors.sol (generation runs on cache hits).
    """
    digest = hashlib.sha256()
    inputs = sorted(
        path for path in VYPER_DIR.rglob("*") if path.is_file() and path.suffix in (".vy", ".vyi")
    )
    inputs.append(RELEASE_ARTIFACTS / "cmd_executor.error_map.json")
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
    # Capitalize each underscore-separated part without degrading parts that
    # already carry internal capitals (e.g. the CamelCase
    # `ExttloadComparator.vy` source must not become `Exttloadcomparator`).
    return "".join(part[:1].upper() + part[1:] for part in Path(src).stem.split("_"))


def main() -> int:
    args = sys.argv[1:]
    vyper_version = vyper.__long_version__
    if "--force" not in args and _up_to_date(OUT / "manifest.json", vyper_version):
        print(f"artifacts up to date ({vyper_version})")
        # The generated error vocabulary is cheap and always rewritten, so a
        # deleted or hand-edited Errors.sol cannot survive a cache hit.
        generate_errors_module()
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
    rc = check_error_map_vocabulary(compiled)
    if rc != 0:
        return rc
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
    generate_errors_module()
    if "--check-release" in args:
        return check_release_identity()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
