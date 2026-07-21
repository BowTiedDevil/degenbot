#!/usr/bin/env python3
"""Recompile cmd_executor.vy and inject mainnet immutables into the runtime bytecode.

Usage:
    python3 contracts/recompile.py [--no-patch]

Steps:
    1. Compile cmd_executor.vy from ~/code/executor/ using Vyper
    2. Append 5 × 32-byte immutable slots after the CBOR metadata
    3. Patch POOL_MANAGER_ADDR to the Ethereum mainnet PoolManager
    4. Copy ABI, bytecode, and runtime bytecode into contracts/

Vyper bytecode layout
--------------------
The Vyper compiler outputs runtime bytecode as:
    [code_section][CBOR_metadata]

The CBOR metadata occupies the last N bytes (variable length;
the last 2 bytes encode the CBOR data length). The code section
contains CODECOPY instructions with hardcoded offsets that assume
the CBOR metadata is present in the deployed bytecode.

In a normal deployment, the initcode appends immutable values
AFTER the CBOR metadata:
    [code_section][CBOR_metadata][immutable_data]

The CODECOPY offset 0x405c (= 16476 = code_section + CBOR)
reads the first immutable slot; subsequent CODECOPY offsets read
later slots. The CBOR bytes also serve as the function dispatch
jump table (e.g., 0x404a) and a JUMPDEST target (0x4046).

IMPORTANT: The CBOR metadata MUST NOT be stripped. Removing it
breaks the jump table, the JUMPDEST at 0x4046, and the CODECOPY
offsets for immutables.

Immutable layout (5 slots, 160 bytes, placed after CBOR):
    [0]  OWNER_ADDR          — msg.sender at deploy time
    [1]  WETH_ADDR           — constructor param
    [2]  POOL_MANAGER_ADDR   — constructor param
    [3]  WETH_DELTA_SLOT     — precomputed keccak256(self, WETH)
    [4]  NATIVE_DELTA_SLOT   — precomputed keccak256(self, NATIVE)

The constructor is __init__(weth, pool_manager) — no per-path token
immutables. Only the two hot protocol currencies (WETH, NATIVE) get
precomputed delta slots; all other currencies compute the slot on-chain
via keccak256. The delta slots are precomputed in __init__ as
keccak256(abi.encodePacked(self, currency)), matching v4-core
CurrencyDelta._computeSlot. With code injection, self = the injected
executor address.

With --no-patch, skip the PM immutable patch (e.g. for testnets).
"""

import pathlib
import shutil
import subprocess
import sys

from web3 import Web3

# ── Paths ──────────────────────────────────────────────────────────
EXECUTOR_SRC = pathlib.Path.home() / "code" / "executor"
VYPER_SOURCE = EXECUTOR_SRC / "contracts" / "cmd_executor.vy"
DEGENBOT_ROOT = pathlib.Path(__file__).resolve().parent.parent
CONTRACTS_DIR = DEGENBOT_ROOT / "contracts"

# ── Mainnet immutable values ──────────────────────────────────────
OWNER_ADDR = "0x9C56a29c7231974c269E24F9FB3c29203039089E"
WETH_ADDR = "0xC02aaA39b223Fe8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER_ADDR = "0x000000000004444c5dc75cB358380D2e3De08A90"
NATIVE_ADDRESS = "0x0000000000000000000000000000000000000000"

# The injected executor address — used as `self` for delta slot precomputation.
# Must match INJECTED_EXECUTOR_ADDRESS in the backrun bot.
INJECTED_EXECUTOR_ADDRESS = "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"


def _left_pad_address(addr: str) -> str:
    """Left-pad a 20-byte address to 32 bytes (64 hex chars)."""
    return "0" * 24 + addr[2:].lower()


def _left_pad_bytes32(value: bytes) -> str:
    """Encode a 32-byte value as 64 hex chars (no 0x prefix)."""
    return value.hex()


def _compute_delta_slot(target: str, currency: str) -> bytes:
    """Compute a V4 CurrencyDelta slot.

    Matches v4-core CurrencyDelta._computeSlot:
        keccak256(abi.encodePacked(target, currency))
    where target and currency are left-padded to bytes32.

    In Vyper: keccak256(concat(convert(self, bytes32),
        convert(convert(currency, uint160), bytes32)))
    """
    target_bytes = bytes.fromhex(target[2:].lower().zfill(64))
    currency_bytes = bytes.fromhex(currency[2:].lower().zfill(64))
    return Web3.keccak(target_bytes + currency_bytes)


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
    print(f"       Runtime bytecode: {len(runtime_code) // 2} bytes (code + CBOR)")

    # ── Step 2: Append immutables AFTER the CBOR metadata ──────────
    # The CBOR metadata MUST stay in place. Vyper's CODECOPY offsets
    # are computed assuming the deployed bytecode layout is:
    #   [code_section][CBOR_metadata][immutable_data]
    # The CBOR bytes also serve as the function dispatch jump table
    # and JUMPDEST targets used by the code section.
    print("[2/4] Appending immutables after CBOR metadata ...")

    # Precompute delta slots using the injected executor address as `self`
    weth_delta_slot = _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, WETH_ADDR)
    native_delta_slot = _compute_delta_slot(INJECTED_EXECUTOR_ADDRESS, NATIVE_ADDRESS)

    immutables = (
        _left_pad_address(OWNER_ADDR)          # [0] OWNER_ADDR
        + _left_pad_address(WETH_ADDR)         # [1] WETH_ADDR
        + _left_pad_address(POOL_MANAGER_ADDR) # [2] POOL_MANAGER_ADDR
        + _left_pad_bytes32(weth_delta_slot)    # [3] WETH_DELTA_SLOT
        + _left_pad_bytes32(native_delta_slot)  # [4] NATIVE_DELTA_SLOT
    )
    runtime_with_immutables = f"0x{runtime_code}{immutables}"
    print(f"       OWNER        ={OWNER_ADDR}")
    print(f"       WETH         ={WETH_ADDR}")
    print(f"       PM           ={POOL_MANAGER_ADDR}")
    print(f"       WETH_DELTA   =0x{weth_delta_slot.hex()}")
    print(f"       NATIVE_DELTA =0x{native_delta_slot.hex()}")

    # ── Step 3: Patch PM immutable in runtime bytecode ─────────────
    # The compiler may use a different PM than mainnet. We always
    # overwrite it to ensure the runtime bytecode targets mainnet PM.
    NUM_IMMUTABLE_SLOTS = 5
    IMMUTABLE_HEX_LEN = NUM_IMMUTABLE_SLOTS * 64  # 320 hex chars = 160 bytes
    PM_SLOT_INDEX = 2  # POOL_MANAGER_ADDR is the 3rd immutable (index 2)

    if patch_pm:
        print("[3/4] Patching POOL_MANAGER_ADDR immutable to mainnet ...")
        code = runtime_with_immutables.removeprefix("0x")
        pm_padded = _left_pad_address(POOL_MANAGER_ADDR)
        # The immutable tail is the last IMMUTABLE_HEX_LEN hex chars
        tail = code[-IMMUTABLE_HEX_LEN:]
        # PM is at slot index 2: offset 2*64, replace 64 hex chars
        pm_offset = PM_SLOT_INDEX * 64
        patched_tail = tail[:pm_offset] + pm_padded + tail[pm_offset + 64:]
        runtime_with_immutables = f"0x{code[:-IMMUTABLE_HEX_LEN]}{patched_tail}"
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
    tail = code[-IMMUTABLE_HEX_LEN:]
    print()
    print("Verification:")
    print(f"  Code + CBOR:   {len(runtime_code) // 2} bytes")
    print(f"  Immutables:    {NUM_IMMUTABLE_SLOTS * 32} bytes")
    print(f"  Total:         {len(code) // 2} bytes")
    print(f"  OWNER slot:        0x{tail[0:64][-40:]}")
    print(f"  WETH slot:         0x{tail[64:128][-40:]}")
    print(f"  PM slot:           0x{tail[128:192][-40:]}")
    print(f"  WETH_DELTA slot:   0x{tail[192:256]}")
    print(f"  NATIVE_DELTA slot: 0x{tail[256:320]}")
    # Validate total bytecode length
    expected_len = len(runtime_code) // 2 + NUM_IMMUTABLE_SLOTS * 32
    actual_len = len(code) // 2
    if actual_len != expected_len:
        print(f"  WARNING: bytecode length {actual_len} != expected {expected_len}")
    else:
        print(f"  Bytecode length: {actual_len} bytes ✓")
    print()
    print("Done ✓")


if __name__ == "__main__":
    main()
