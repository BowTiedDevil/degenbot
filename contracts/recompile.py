#!/usr/bin/env python3
# ruff: file-ignore[print]
# The bake's product is its stdout report: every line is operator-facing evidence
# of what was written where, so `print` is the point of this script.

"""Bake the cmd_executor bytecode files from the committed tier-3 artifacts.

Usage:
    uv run python contracts/recompile.py            # bake with the release owner
                                                    #   and the mainnet PoolManager
                                                    #   into the release deployment/
                                                    #   (toolchain-free, default)
    uv run python contracts/recompile.py --compile  # additionally run the
                                                    #   AUTHORITATIVE compile-vs-use
                                                    #   gate (tier3-oracle/verify-
                                                    #   tier3-executor-artifact.sh):
                                                    #   a fresh vyper 0.5.0b2 + solc
                                                    #   0.7.6 compile byte-compared
                                                    #   against ALL committed
                                                    #   executor artifacts + the V3
                                                    #   topology harness. Opt-in;
                                                    #   requires the toolchain.
    uv run python contracts/recompile.py --no-patch # bake the POOL_MANAGER slot as
                                                    #   zero (testnet/dev; patch
                                                    #   afterwards with the README
                                                    #   recipe)
    uv run python contracts/recompile.py --owner ADDRESS --output-dir DIR
                                                    # bake a signer-compatible probe
                                                    #   runtime: the same accepted
                                                    #   code+CBOR, with immutable
                                                    #   word 0 (OWNER_ADDR) set to
                                                    #   the session signer

Files written (existing file format: 0x-prefixed lowercase hex + trailing
newline; the ABI is copied verbatim):
    cmd_executor_runtime_bytecode.txt  = 0x + runtime hex + 7 x 32B immutables
    cmd_executor_bytecode.txt          = 0x + creation hex (deployment)
    cmd_executor_abi.json              = the artifact ABI

A non-default owner produces a PROBE runtime, not the release runtime: `execute()`
admits the runtime's own OWNER_ADDR alone, so a runtime owned by another address is
how a dry run about that account composes at all. Such a bake therefore needs an
explicit --output-dir and is refused when that directory is the release deployment,
so the committed artifact always keeps the release owner. This script never reads,
accepts, or prints key material; the owner is a public address.

Vyper bytecode layout (re-derived for the current artifact — the code section
moved with U3WVLL / 767TN5 / TGUZCT; do NOT hand-bake offsets, they are
compiler-generated):
    runtime  = [code_section][CBOR_metadata]
    deployed = runtime + [7 x 32-byte immutable slots]

The runtime code's CODECOPY instructions read the immutable slots from
offsets at/after the end of the runtime section, so the 224-byte tail
appended to `cmd_executor_runtime_bytecode.txt` lands exactly where the
compiler expects it. The CBOR metadata MUST NOT be stripped (it doubles
as the dispatch jump table + JUMPDEST targets), and the tail must come
AFTER the CBOR — never replace it.

Immutable layout (7 slots, 224 bytes, appended after the CBOR):
    [0] OWNER_ADDR          the release owner, or --owner for a probe runtime
    [1] WETH_ADDR           0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2
    [2] POOL_MANAGER_ADDR   0x000000000004444c5dc75cB358380D2e3De08A90 (mainnet;
                            zero with --no-patch)
    [3] USDC_ADDR           0xA0b86991c6218b36c1d19d4a2e9eb0ce3606eb48
    [4] USDT_ADDR           0xdac17f958d2ee523a2206206994597c13d831ec7
    [5] WETH_DELTA_SLOT     keccak256(padded20(self), padded20(WETH))
    [6] NATIVE_DELTA_SLOT   keccak256(padded20(self), padded20(0x0))

`self` = INJECTED_EXECUTOR_ADDRESS, the code-injection address — must match
degenbot.runner._driver_constants.INJECTED_EXECUTOR_ADDRESS. Delta slots are
precomputed exactly as __init__ computes them (keccak256 of the packed
bytes32 pair, v4-core CurrencyDelta._computeSlot).

The old pipeline compiled from ~/code/executor/ (vendored in-repo); the re-sync
derives from the COMMITTED tier-3 artifacts instead, which fixes U3WVLL + 767TN5
+ 0x43 in one bake.

Manifest convention (tier-3-wide, see write-harness-manifest.sh and the
build script): each manifest `sha256` field records the sha256 of the
artifact's git-tracked SOURCE (not the artifact file) — the pin proves the
committed artifacts were built from the current source. Artifact-BYTE
integrity is the job of the compile-vs-use gate above (CI job tier3-oracle).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.crypto import keccak256

if TYPE_CHECKING:
    from collections.abc import Sequence


# ── Paths ──────────────────────────────────────────────────────────
DEGENBOT_ROOT = pathlib.Path(__file__).resolve().parent.parent
RELEASE_DIR = DEGENBOT_ROOT / "rust/crates/foundation/degenbot-executor/releases/v2"
CONTRACTS_DIR = RELEASE_DIR / "deployment"
ARTIFACTS_DIR = RELEASE_DIR / "artifacts"
EXECUTOR_DIR = DEGENBOT_ROOT
VYPER_SOURCE = RELEASE_DIR / "contract.vy"

RUNTIME_FILE = "cmd_executor_runtime_bytecode.txt"
CREATION_FILE = "cmd_executor_bytecode.txt"
ABI_FILE = "cmd_executor_abi.json"


# ── Mainnet immutable values ───────────────────────────────────────
OWNER_ADDR = "0x9C56a29c7231974c269E24F9FB3c29203039089E"
WETH_ADDR = "0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER_ADDR = "0x000000000004444c5dc75cB358380D2e3De08A90"
NATIVE_ADDRESS = "0x0000000000000000000000000000000000000000"
USDC_ADDR = "0xA0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
USDT_ADDR = "0xdac17f958d2ee523a2206206994597c13d831ec7"

# The injected executor address — used as `self` for delta slot precomputation.
# Must match INJECTED_EXECUTOR_ADDRESS in degenbot.runner._driver_constants.
INJECTED_EXECUTOR_ADDRESS = "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"

NUM_IMMUTABLE_SLOTS = 7
IMMUTABLE_HEX_LEN = NUM_IMMUTABLE_SLOTS * 64  # 448 hex chars = 224 bytes

#: The shape a baked owner must have: `0x` and exactly 20 bytes of hex.
ADDRESS_SHAPE = re.compile(r"0x[0-9a-fA-F]{40}")


@dataclass(frozen=True)
class BakeRequest:
    """One bake's resolved owner and destination.

    Attributes:
        owner: The address baked as immutable word 0 (``OWNER_ADDR``).
        output_dir: The directory the three artifact files are written into.
        patch_pm: Whether the mainnet PoolManager address fills slot 2.

    """

    owner: str
    output_dir: pathlib.Path
    patch_pm: bool


def _hex_read(path: pathlib.Path) -> str:
    """Read a hex file, normalize to bare lowercase hex (no 0x / whitespace).

    Args:
        path: The hex file.

    Returns:
        The file's hex digits, without a prefix and without whitespace.

    """
    data = path.read_text(encoding="utf-8").strip()
    if data.startswith(("0x", "0X")):
        data = data[2:]
    data = "".join(data.split()).lower()
    if len(data) % 2 or any(c not in "0123456789abcdef" for c in data):
        sys.exit(f"{path}: not even-length hex")
    return data


def _padded20(addr: str) -> str:
    """Left-pad one address to an immutable word.

    Args:
        addr: The address, ``0x`` plus 40 hex digits.

    Returns:
        The 64-hex-digit word an ``address`` occupies in the immutable tail.

    """
    return "0" * 24 + addr[2:].lower()


def _compute_delta_slot(target: str, currency: str) -> str:
    """Compute a V4 ``CurrencyDelta`` slot.

    The slot is ``keccak256(padded20(target) || padded20(currency))``.

    Args:
        target: The executor address the slot is keyed by.
        currency: The currency address the slot is keyed by.

    Returns:
        The slot as 64 lowercase hex digits.

    """
    packed = bytes.fromhex(_padded20(target) + _padded20(currency))
    return keccak256(packed).hex()


def _display(path: pathlib.Path) -> str:
    """Render a path the way the bake report spells it.

    Args:
        path: The directory to report.

    Returns:
        The path relative to the repository root when it lives inside it, with a
        trailing separator; otherwise the path as given.

    """
    try:
        return str(path.resolve().relative_to(DEGENBOT_ROOT)) + "/"
    except ValueError:
        return str(path)


def _owner_argument(value: str) -> str:
    """Validate one ``--owner`` value.

    Args:
        value: The argument as given.

    Returns:
        The address, unchanged.

    Raises:
        argparse.ArgumentTypeError: If the value is not ``0x`` plus 40 hex digits.

    """
    if ADDRESS_SHAPE.fullmatch(value) is None:
        msg = f"owner {value!r} is not an address (expected 0x plus 40 hex digits)"
        raise argparse.ArgumentTypeError(msg)
    return value


def _parser() -> argparse.ArgumentParser:
    """Build the bake's command-line parser.

    Returns:
        A parser for this script's arguments.

    """
    parser = argparse.ArgumentParser(
        description="Bake the cmd_executor runtime, creation bytecode, and ABI.",
    )
    parser.add_argument(
        "--owner",
        type=_owner_argument,
        default=OWNER_ADDR,
        help=(
            "address baked as immutable word 0 (OWNER_ADDR); the release owner by "
            "default, and any other value is a probe runtime that needs --output-dir"
        ),
    )
    parser.add_argument(
        "--output-dir",
        default=None,
        metavar="DIR",
        help="directory the artifact files are written into (the release deployment/ by default)",
    )
    parser.add_argument(
        "--no-patch",
        action="store_true",
        help="bake the POOL_MANAGER slot as zero (testnet/dev)",
    )
    parser.add_argument(
        "--compile",
        action="store_true",
        help="run the authoritative compile-vs-use gate before baking",
    )
    return parser


def _resolve_request(args: argparse.Namespace) -> BakeRequest:
    """Resolve parsed arguments into a bake, refusing an unsafe destination.

    A custom owner yields a probe runtime, so it is only ever written where the
    operator named it: the release deployment keeps the release owner, and an
    unnamed destination would be exactly that directory. Either refusal exits
    through ``sys.exit`` naming the owner, before anything is written.

    Args:
        args: The parsed command line.

    Returns:
        The bake to run.

    """
    output_dir = CONTRACTS_DIR if args.output_dir is None else pathlib.Path(args.output_dir)
    if args.owner.lower() != OWNER_ADDR.lower():
        if args.output_dir is None:
            sys.exit(
                f"owner {args.owner} is not this release's owner {OWNER_ADDR}: a "
                "custom-owner bake is a signer-compatible probe runtime, not the "
                "release runtime, so it needs an explicit --output-dir and never "
                "rewrites the release deployment"
            )
        if output_dir.resolve() == CONTRACTS_DIR.resolve():
            sys.exit(
                f"--output-dir {output_dir} is the release deployment: it keeps owner "
                f"{OWNER_ADDR}, so a probe runtime owned by {args.owner} is written "
                "elsewhere"
            )
    return BakeRequest(owner=args.owner, output_dir=output_dir, patch_pm=not args.no_patch)


def _immutables(owner: str, *, patch_pm: bool) -> str:
    """Build the seven immutable words appended after the runtime's CBOR.

    Args:
        owner: The address baked into slot 0, the only per-run substitution.
        patch_pm: Whether slot 2 carries the mainnet PoolManager address, or the
            zero address for a testnet/dev bake.

    Returns:
        The 448-hex-digit tail, in the layout the compiler's CODECOPY expects.

    """
    pm = POOL_MANAGER_ADDR if patch_pm else NATIVE_ADDRESS
    return (
        _padded20(owner)  # [0] OWNER_ADDR
        + _padded20(WETH_ADDR)  # [1] WETH_ADDR
        + _padded20(pm)  # [2] POOL_MANAGER_ADDR
        + _padded20(USDC_ADDR)  # [3] USDC_ADDR
        + _padded20(USDT_ADDR)  # [4] USDT_ADDR
        + _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, WETH_ADDR)  # [5]
        + _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, NATIVE_ADDRESS)  # [6]
    )


def _check_manifest() -> None:
    """Verify the artifacts against the committed source pin.

    The manifest's sha256 column records the SOURCE sha (contract.vy) that the
    committed cmd_executor artifacts were built from. Verifying it here (without
    a toolchain) pins the artifacts to the current in-repo source; the
    authoritative compile-vs-use gate is just verify-tier3-executor-artifact.sh
    (CI).
    """
    manifest = json.loads((ARTIFACTS_DIR / "manifest.json").read_text(encoding="utf-8"))
    pin = None
    for entry, meta in manifest.get("artifacts", {}).items():
        if entry.startswith("executor/cmd_executor."):
            pin = meta["sha256"]
            break
    if pin is None:
        sys.exit("manifest.json: no executor/cmd_executor.* pin found")
    actual = hashlib.sha256(VYPER_SOURCE.read_bytes()).hexdigest()
    if actual != pin:
        sys.exit(
            f"source/artifact drift: cmd_executor.vy sha256 {actual[:16]} != manifest pin "
            f"{pin[:16]} — rebuild via just rebuild-tier3-artifacts first"
        )
    print("       manifest source pin: OK (artifacts match committed source)")


def _verify_compile_matches_artifacts() -> None:
    """Run the authoritative compile-vs-use gate before baking.

    tier3-oracle/verify-tier3-executor-artifact.sh recompiles a fresh vyper
    0.5.0b2 + solc 0.7.6 and byte-compares the result against ALL committed
    executor artifacts (creation, runtime, abi, error_map, immutables) plus the
    V3 topology harness, so it replaces the former inline 2-format vyper check.
    Requires the toolchain (executor uv project + svm-cached solc).
    """
    gate = DEGENBOT_ROOT / "tier3-oracle" / "verify-tier3-executor-artifact.sh"
    try:
        # The gate owns the toolchain and the compile flags, so the bake shells out
        # to it; `bash` is the operator's PATH lookup for the interpreter.
        subprocess.run(["bash", str(gate)], check=True)  # ruff: ignore[subprocess-without-shell-equals-true, start-process-with-partial-path]
    except subprocess.CalledProcessError:
        sys.exit(
            "verify-tier3-executor-artifact.sh failed — source/artifact/toolchain "
            "drift; run just rebuild-tier3-artifacts"
        )


def _bake(request: BakeRequest) -> None:
    """Write the three artifacts into the request's directory and report them.

    The committed artifacts must be present and pinned to the in-repo source: a
    missing artifact or a drifted manifest exits before anything is written.

    Args:
        request: The resolved bake to run.

    """
    if not (ARTIFACTS_DIR / "cmd_executor.runtime.hex").exists():
        sys.exit(f"artifacts not found under {ARTIFACTS_DIR}")
    _check_manifest()

    runtime = _hex_read(ARTIFACTS_DIR / "cmd_executor.runtime.hex")
    creation = _hex_read(ARTIFACTS_DIR / "cmd_executor.creation.hex")
    abi = (ARTIFACTS_DIR / "cmd_executor.abi.json").read_text(encoding="utf-8")

    runtime_baked = runtime + _immutables(request.owner, patch_pm=request.patch_pm)

    request.output_dir.mkdir(parents=True, exist_ok=True)
    runtime_path = request.output_dir / RUNTIME_FILE
    (request.output_dir / ABI_FILE).write_text(abi, encoding="utf-8")
    (request.output_dir / CREATION_FILE).write_text("0x" + creation + "\n", encoding="utf-8")
    runtime_path.write_text("0x" + runtime_baked + "\n", encoding="utf-8")

    tail = runtime_baked[-IMMUTABLE_HEX_LEN:]
    tail_offset = hex(len(runtime) // 2)
    print()
    print(f"Baked into {_display(request.output_dir)} (from {_display(ARTIFACTS_DIR)}):")
    print(f"  runtime (code+CBOR): {len(runtime) // 2} B")
    print(
        f"  immutables:          {IMMUTABLE_HEX_LEN // 2} B "
        f"(first CODECOPY tail offset {tail_offset})"
    )
    print(f"  runtime + immutables: {len(runtime_baked) // 2} B")
    print(f"  OWNER         = {tail[0:64][-40:]}")
    print(f"  WETH          = {tail[64:128][-40:]}")
    print(f"  PM            = {tail[128:192][-40:]}")
    print(f"  USDC          = {tail[192:256][-40:]}")
    print(f"  USDT          = {tail[256:320][-40:]}")
    print(f"  WETH_DELTA    = 0x{tail[320:384]}")
    print(f"  NATIVE_DELTA  = 0x{tail[384:448]}")
    print("  creation:          " + hashlib.sha256(creation.encode()).hexdigest()[:16])
    print(f"  runtime sha256:    {hashlib.sha256(runtime_path.read_bytes()).hexdigest()}")
    print("Done ✓")


def main(argv: Sequence[str] | None = None) -> None:
    """Parse the command line and bake, running the compile gate first if asked.

    An unusable argument, a custom owner with no separate destination, a failing
    compile gate, and missing artifacts each exit through ``sys.exit`` without
    touching the release deployment.

    Args:
        argv: The command line to parse; argparse reads ``sys.argv[1:]`` when absent.

    """
    args = _parser().parse_args(argv)
    request = _resolve_request(args)
    if args.compile:
        _verify_compile_matches_artifacts()
    _bake(request)


if __name__ == "__main__":
    main()
