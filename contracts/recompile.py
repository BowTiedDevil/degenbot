#!/usr/bin/env python3
"""Bake the cmd_executor bytecode files into contracts/ (X6OKMV / TGUZCT re-sync).

Usage:
    uv run python contracts/recompile.py            # bake from the committed
                                                    #   tier3-oracle/artifacts/executor/
                                                    #   (toolchain-free, default)
    uv run python contracts/recompile.py --compile  # additionally compile the
                                                    #   in-repo executor/contracts/cmd_executor.vy
                                                    #   (pinned vyper 0.5.0a3, executor uv
                                                    #   project) and FAIL on any sha256
                                                    #   mismatch vs the artifacts
    uv run python contracts/recompile.py --no-patch # bake the POOL_MANAGER slot as
                                                    #   zero (testnet/dev; patch afterwards
                                                    #   with the README recipe)

Files written (existing file format: 0x-prefixed lowercase hex + trailing
newline; the ABI is copied verbatim):
    cmd_executor_runtime_bytecode.txt  = 0x + runtime hex + 5 x 32B immutables
    cmd_executor_bytecode.txt          = 0x + creation hex (deployment)
    cmd_executor_init_bytecode.txt     = 0x + creation hex (pre-constructor-args)
    cmd_executor_abi.json              = the artifact ABI

Vyper bytecode layout (re-derived for the current artifact — the code section
moved with U3WVLL / 767TN5 / TGUZCT; do NOT hand-bake offsets, they are
compiler-generated):
    runtime  = [code_section][CBOR_metadata]            (16,097 B @ this commit)
    deployed = runtime + [5 x 32-byte immutable slots]  (16,257 B)

The runtime code's CODECOPY instructions read the immutable slots from
offsets at/after the end of the runtime section, so the 160-byte tail
appended to `cmd_executor_runtime_bytecode.txt` lands exactly where the
compiler expects it. The CBOR metadata MUST NOT be stripped (it doubles
as the dispatch jump table + JUMPDEST targets), and the tail must come
AFTER the CBOR — never replace it.

Immutable layout (5 slots, 160 bytes, appended after the CBOR):
    [0] OWNER_ADDR          0x9C56a29c7231974c269E24F9FB3c29203039089E
    [1] WETH_ADDR           0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2
    [2] POOL_MANAGER_ADDR   0x000000000004444c5dc75cB358380D2e3De08A90 (mainnet;
                            zero with --no-patch)
    [3] WETH_DELTA_SLOT     keccak256(padded20(self), padded20(WETH))
    [4] NATIVE_DELTA_SLOT   keccak256(padded20(self), padded20(0x0))
`self` = INJECTED_EXECUTOR_ADDRESS, the code-injection address — must match
degenbot.runner._driver_constants.INJECTED_EXECUTOR_ADDRESS. Delta slots are
precomputed exactly as __init__ computes them (keccak256 of the packed
bytes32 pair, v4-core CurrencyDelta._computeSlot).

The old pipeline compiled from ~/code/executor/ (retired 5ddf2f05b, vendored
in-repo); the re-sync derives from the COMMITTED tier-3 artifacts instead,
which fixes U3WVLL + 767TN5 + 0x43 in one bake.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys

from degenbot.crypto import keccak256

# ── Paths ──────────────────────────────────────────────────────────
DEGENBOT_ROOT = pathlib.Path(__file__).resolve().parent.parent
CONTRACTS_DIR = DEGENBOT_ROOT / "contracts"
ARTIFACTS_DIR = DEGENBOT_ROOT / "tier3-oracle" / "artifacts" / "executor"
EXECUTOR_DIR = DEGENBOT_ROOT / "executor"
VYPER_SOURCE = EXECUTOR_DIR / "contracts" / "cmd_executor.vy"

# ── Mainnet immutable values ───────────────────────────────────────
OWNER_ADDR = "0x9C56a29c7231974c269E24F9FB3c29203039089E"
WETH_ADDR = "0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER_ADDR = "0x000000000004444c5dc75cB358380D2e3De08A90"
NATIVE_ADDRESS = "0x0000000000000000000000000000000000000000"

# The injected executor address — used as `self` for delta slot precomputation.
# Must match INJECTED_EXECUTOR_ADDRESS in degenbot.runner._driver_constants.
INJECTED_EXECUTOR_ADDRESS = "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"

NUM_IMMUTABLE_SLOTS = 5
IMMUTABLE_HEX_LEN = NUM_IMMUTABLE_SLOTS * 64  # 320 hex chars = 160 bytes


def _hex_read(path: pathlib.Path) -> str:
    """Read a hex file, normalize to bare lowercase hex (no 0x / whitespace)."""
    data = path.read_text().strip()
    if data.startswith("0x") or data.startswith("0X"):
        data = data[2:]
    data = "".join(data.split()).lower()
    if len(data) % 2 or any(c not in "0123456789abcdef" for c in data):
        sys.exit(f"{path}: not even-length hex")
    return data


def _padded20(addr: str) -> str:
    return "0" * 24 + addr[2:].lower()


def _compute_delta_slot(target: str, currency: str) -> str:
    """V4 CurrencyDelta slot: keccak256(padded20(target) || padded20(currency))."""
    packed = bytes.fromhex(_padded20(target) + _padded20(currency))
    return keccak256(packed).hex()


def _check_manifest() -> None:
    """Toolchain-free source pin: the manifest's sha256 column records the
    SOURCE sha (cmd_executor.vy) that the committed cmd_executor artifacts
    were built from. Verifying it here (without a toolchain) pins the
    artifacts to the current in-repo source; the authoritative
    compile-vs-use gate is just verify-tier3-executor-artifact.sh (CI).
    """
    manifest = json.loads((ARTIFACTS_DIR / "manifest.json").read_text())
    pin = None
    for entry, meta in manifest.get("artifacts", {}).items():
        if entry.startswith("executor/cmd_executor."):
            pin = meta["sha256"]
            break
    if pin is None:
        sys.exit("manifest.json: no executor/cmd_executor.* pin found")
    actual = hashlib.sha256((DEGENBOT_ROOT / "executor" / "contracts" / "cmd_executor.vy").read_bytes()).hexdigest()
    if actual != pin:
        sys.exit(
            f"source/artifact drift: cmd_executor.vy sha256 {actual[:16]} != manifest pin "
            f"{pin[:16]} — rebuild via just rebuild-tier3-artifacts first"
        )
    print("       manifest source pin: OK (artifacts match committed source)")


def _verify_compile_matches_artifacts() -> None:
    """Compile the in-repo source and fail-closed on any drift from the
    committed artifacts (the toolchain path is a consistency check, not a
    bake source)."""
    if not VYPER_SOURCE.exists():
        sys.exit(f"source not found: {VYPER_SOURCE} (drop --compile?)")
    want = {
        "bytecode": _hex_read(ARTIFACTS_DIR / "cmd_executor.creation.hex"),
        "bytecode_runtime": _hex_read(ARTIFACTS_DIR / "cmd_executor.runtime.hex"),
    }
    for fmt, want_hex in want.items():
        try:
            out = subprocess.check_output(
                ["uv", "run", "vyper", "-f", fmt, str(VYPER_SOURCE)],
                cwd=EXECUTOR_DIR,
                stderr=subprocess.PIPE,
            ).decode().strip()
        except subprocess.CalledProcessError as e:
            sys.exit(f"vyper ({fmt}) failed:\n{e.stderr.decode()}")
        got = out.removeprefix("0x").strip().lower()
        if got != want_hex:
            sys.exit(
                f"source/artifact drift: vyper -f {fmt} sha256 "
                f"{hashlib.sha256(got.encode()).hexdigest()[:16]} != artifact "
                f"{hashlib.sha256(want_hex.encode()).hexdigest()[:16]} — "
                "run just rebuild-tier3-artifacts (or revert the source)"
            )
        print(f"       vyper -f {fmt}: matches artifact")


def main() -> None:
    patch_pm = "--no-patch" not in sys.argv[1:]
    if "--compile" in sys.argv[1:]:
        _verify_compile_matches_artifacts()

    if not (ARTIFACTS_DIR / "cmd_executor.runtime.hex").exists():
        sys.exit(f"artifacts not found under {ARTIFACTS_DIR}")
    _check_manifest()

    runtime = _hex_read(ARTIFACTS_DIR / "cmd_executor.runtime.hex")
    creation = _hex_read(ARTIFACTS_DIR / "cmd_executor.creation.hex")
    abi = (ARTIFACTS_DIR / "cmd_executor.abi.json").read_text()

    pm = POOL_MANAGER_ADDR if patch_pm else NATIVE_ADDRESS
    immutables = (
        _padded20(OWNER_ADDR)                      # [0] OWNER_ADDR
        + _padded20(WETH_ADDR)                     # [1] WETH_ADDR
        + _padded20(pm)                            # [2] POOL_MANAGER_ADDR
        + _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, WETH_ADDR)              # [3]
        + _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, NATIVE_ADDRESS)         # [4]
    )
    runtime_baked = runtime + immutables

    CONTRACTS_DIR.mkdir(parents=True, exist_ok=True)
    (CONTRACTS_DIR / "cmd_executor_abi.json").write_text(abi)
    (CONTRACTS_DIR / "cmd_executor_bytecode.txt").write_text("0x" + creation + "\n")
    (CONTRACTS_DIR / "cmd_executor_init_bytecode.txt").write_text("0x" + creation + "\n")
    (CONTRACTS_DIR / "cmd_executor_runtime_bytecode.txt").write_text("0x" + runtime_baked + "\n")

    tail = runtime_baked[-IMMUTABLE_HEX_LEN:]
    print()
    print("Baked into contracts/ (from tier3-oracle/artifacts/executor/):")
    print(f"  runtime (code+CBOR): {len(runtime) // 2} B")
    print(f"  immutables:          {IMMUTABLE_HEX_LEN // 2} B (first CODECOPY tail offset {hex(len(runtime) // 2)})")
    print(f"  runtime + immutables: {len(runtime_baked) // 2} B")
    print(f"  OWNER         = {tail[0:64][-40:]}")
    print(f"  WETH          = {tail[64:128][-40:]}")
    print(f"  PM            = {tail[128:192][-40:]}")
    print(f"  WETH_DELTA    = 0x{tail[192:256]}")
    print(f"  NATIVE_DELTA  = 0x{tail[256:320]}")
    print("  creation:          " + hashlib.sha256(creation.encode()).hexdigest()[:16])
    print("Done ✓")


if __name__ == "__main__":
    main()
