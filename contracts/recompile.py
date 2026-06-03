#!/usr/bin/env python3
"""Recompile cmd_executor.vy and inject mainnet immutables into the runtime bytecode.

Usage:
    python3 contracts/recompile.py [--no-patch]

Steps:
    1. Compile cmd_executor.vy from ~/code/executor/ using Vyper
    2. Append 3 × 32-byte immutable slots to the runtime bytecode
    3. Patch POOL_MANAGER_ADDR to the Ethereum mainnet PoolManager
    4. Copy ABI, bytecode, and runtime bytecode into contracts/

With --no-patch, skip the PM immutable patch (e.g. for testnets).
"""

import pathlib
import shutil
import subprocess
import sys

# ── Paths ──────────────────────────────────────────────────────────
EXECUTOR_SRC = pathlib.Path.home() / "code" / "executor"
VYPER_SOURCE = EXECUTOR_SRC / "contracts" / "cmd_executor.vy"
DEGENBOT_ROOT = pathlib.Path(__file__).resolve().parent.parent
CONTRACTS_DIR = DEGENBOT_ROOT / "contracts"

# ── Mainnet immutable values ──────────────────────────────────────
OWNER_ADDR = "0x9C56a29c7231974c269E24F9FB3c29203039089E"
WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER_ADDR = "0x000000000004444c5dc75cB358380D2e3De08A90"


def _left_pad_address(addr: str) -> str:
    """Left-pad a 20-byte address to 32 bytes (64 hex chars)."""
    return "0" * 24 + addr[2:].lower()


def main() -> None:
    patch_pm = "--no-patch" not in sys.argv[1:]

    if not VYPER_SOURCE.exists():
        sys.exit(f"Source not found: {VYPER_SOURCE}")

    # ── Step 1: Compile ────────────────────────────────────────────
    print("[1/4] Compiling cmd_executor.vy ...")
    try:
        abi = subprocess.check_output(
            ["uv", "run", "vyper", "-f", "abi", str(VYPER_SOURCE)],
            cwd=EXECUTOR_SRC,
            stderr=subprocess.PIPE,
        ).decode()
    except subprocess.CalledProcessError as e:
        sys.exit(f"Vyper ABI compilation failed:\n{e.stderr.decode()}")

    try:
        bytecode = subprocess.check_output(
            ["uv", "run", "vyper", "-f", "bytecode", str(VYPER_SOURCE)],
            cwd=EXECUTOR_SRC,
            stderr=subprocess.PIPE,
        ).decode().strip()
    except subprocess.CalledProcessError as e:
        sys.exit(f"Vyper bytecode compilation failed:\n{e.stderr.decode()}")

    try:
        runtime_raw = subprocess.check_output(
            ["uv", "run", "vyper", "-f", "bytecode_runtime", str(VYPER_SOURCE)],
            cwd=EXECUTOR_SRC,
            stderr=subprocess.PIPE,
        ).decode().strip()
    except subprocess.CalledProcessError as e:
        sys.exit(f"Vyper runtime bytecode compilation failed:\n{e.stderr.decode()}")

    runtime_code = runtime_raw.removeprefix("0x")
    print(f"       Runtime bytecode: {len(runtime_code) // 2} bytes")

    # ── Step 2: Append immutables ──────────────────────────────────
    print("[2/4] Appending immutables ...")
    immutables = (
        _left_pad_address(OWNER_ADDR)
        + _left_pad_address(WETH_ADDR)
        + _left_pad_address(POOL_MANAGER_ADDR)
    )
    runtime_with_immutables = f"0x{runtime_code}{immutables}"
    print(f"       OWNER={OWNER_ADDR}")
    print(f"       WETH={WETH_ADDR}")
    print(f"       PM  ={POOL_MANAGER_ADDR}")

    # ── Step 3: Patch PM immutable in runtime bytecode ─────────────
    # The compiler may use a different PM than mainnet. We always
    # overwrite it to ensure the runtime bytecode targets mainnet PM.
    if patch_pm:
        print("[3/4] Patching POOL_MANAGER_ADDR immutable to mainnet ...")
        code = runtime_with_immutables.removeprefix("0x")
        pm_padded = _left_pad_address(POOL_MANAGER_ADDR)
        # Last 192 hex chars = 3 × 32-byte slots: [OWNER:64][WETH:64][PM:64]
        tail = code[-192:]
        patched_tail = tail[:128] + pm_padded
        runtime_with_immutables = f"0x{code[:-192]}{patched_tail}"
    else:
        print("[3/4] Skipping PM patch (--no-patch)")

    # ── Step 4: Write output files ─────────────────────────────────
    print("[4/4] Writing output files ...")
    CONTRACTS_DIR.mkdir(parents=True, exist_ok=True)

    (CONTRACTS_DIR / "cmd_executor_abi.json").write_text(abi)
    (CONTRACTS_DIR / "cmd_executor_bytecode.txt").write_text(bytecode + "\n")
    (CONTRACTS_DIR / "cmd_executor_runtime_bytecode.txt").write_text(runtime_with_immutables + "\n")

    init_bytecode_src = EXECUTOR_SRC / "contracts" / "cmd_executor_init_bytecode.txt"
    if init_bytecode_src.exists():
        shutil.copy2(init_bytecode_src, CONTRACTS_DIR / "cmd_executor_init_bytecode.txt")

    print(f"       → {CONTRACTS_DIR / 'cmd_executor_abi.json'}")
    print(f"       → {CONTRACTS_DIR / 'cmd_executor_bytecode.txt'}")
    print(f"       → {CONTRACTS_DIR / 'cmd_executor_runtime_bytecode.txt'}")

    # ── Verify ─────────────────────────────────────────────────────
    code = runtime_with_immutables.removeprefix("0x")
    tail = code[-192:]
    print()
    print("Verification:")
    print(f"  Runtime code:  {len(code) // 2 - 96} bytes")
    print(f"  OWNER slot:    0x{tail[0:64][-40:]}")
    print(f"  WETH slot:     0x{tail[64:128][-40:]}")
    print(f"  PM slot:       0x{tail[128:192][-40:]}")
    print()
    print("Done ✓")


if __name__ == "__main__":
    main()
