#!/usr/bin/env python3
"""Scaffold a standalone, per-contract revm investigation harness at the USER
layer, built on the genuinely reusable `degenbot_simulation::oracle` driver
(deploy → seed storage → drive a call → classify Revert-vs-Halt → read back).

This is deliberately contract-agnostic. degenbot's own path investigations
(V2/V3/V4 pool oracles, an executor payload) are just *one* instance of the
pattern; this script emits a fresh Cargo project for *your* contract — a custom
executor, a lending market, a NEW pool family — without baking any of it into
the core crates.

Usage:
    python3 scripts/scaffold_revm_harness.py \\
        --name my_executor \\
        --artifact /path/to/out/MyExecutor.sol/MyExecutor.json \\
        [--slot 0=0x… --slot 5=0x…]

It writes `investigations/<name>/` (a standalone Cargo project) and prints the
next steps. Edit `src/main.rs` to add your contract's real call/flow logic; the
driver + FixtureEvm plumbing is already done for you.
"""

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT_ROOT = REPO / "investigations"

CARGO_TEMPLATE = """\
[package]
name = "@@NAME@@"
version = "0.1.0"
edition = "2021"

[dependencies]
# The genuinely reusable revm fixture driver (deploy -> seed -> call -> classify).
degenbot-simulation = { path = "../../rust/crates/degenbot-simulation" }
# alloy primitives (Address / U256 / Bytes) used in the harness body.
alloy = { version = "^2.3", features = ["full"] }
"""

MAIN_TEMPLATE = r"""//! Per-contract revm investigation harness for `@@NAME@@` — SCAFFOLDED by
//! scripts/scaffold_revm_harness.py. Built on the reusable
//! `degenbot_simulation::oracle` driver: deploy your contract's real bytecode,
//! seed its storage slots, drive a call, classify the verdict (Solidity
//! `Revert` vs a verbless `Halt`), and read back output + logs.
//!
//! This scaffold is CONTRACT-AGNOSTIC: nothing degenbot-specific is baked in.
//! `cargo add degenbot-simulation` + this driver is the whole foundation. Edit
//! the sections marked ⬇ EDIT HERE for YOUR contract's real call/flow.
#![allow(clippy::print_stdout)]

use alloy::primitives::{Bytes, U256};
use degenbot_simulation::oracle::{
    call_bytes, deploy, new_fixture_evm, read_address, seed_slots, selector, set_disable_nonce_check,
    transact, Verdict,
};

fn main() -> Result<(), String> {
    // ── your contract's creation bytecode ──────────────────────────────────
    // Load a foundry artifact (bytecode.object) OR paste creation hex here.
    let init_code_hex = std::env::var("@@ENV@@_ARTIFACT").unwrap_or_else(|_| "0x".to_string());
    let init_code = if init_code_hex.trim_start_matches("0x").is_empty() {
        Bytes::from(YOUR_CREATION_BYTECODE.to_vec())
    } else {
        Bytes::from(alloy::hex::decode(init_code_hex.trim_start_matches("0x")).map_err(|e| e.to_string())?)
    };
    // Constructor args, e.g. an owner address — append as abi words:
    // let mut create_code = init_code.to_vec();
    // create_code.extend_from_slice(&U256::from(YOUR_OWNER).to_be_bytes::<32>());
    let create_code = init_code;

    let mut evm = new_fixture_evm();
    set_disable_nonce_check(&mut evm, true);

    // 1. Deploy the contract.
    let contract = deploy(&mut evm, create_code, 16_700_000)?;
    println!("deployed @@NAME@@ @ {contract}");

    // 2. OPTIONAL: seed raw storage slots (no pool/executor interpretation).
    let slots: &[(U256, U256)] = &[
        // U256::from(slot), U256::from_limbs([lo, hi, 0, 0]) — YOUR layout
    ];
    seed_slots(&mut evm, contract, slots);

    // 3. Drive YOUR call and classify the verdict. A Solidity `Revert` is a
    //    math-level verdict; a `Halt` (OOG) has none — the tier-3 H1 rule.
    println!("--- drive call (⬇ EDIT for YOUR contract) ---");
    let verdict = transact(&mut evm, degenbot_simulation::oracle::TxSpec::Call {
        to: contract,
        data: Bytes::from(selector("YOUR_SELECTOR(bytes)").to_vec()),
        gas: 16_700_000,
    });
    match verdict {
        Verdict::Accepted { output, logs } => {
            println!("accepted: output={output:?} logs={}", logs.len());
        }
        Verdict::Reverted(r) => println!("REVERTED (verdict): {r:?}"),
        Verdict::Halted(h) => println!("HALTED (no verdict / OOG): {h}"),
    }

    // 4. Resolve a nested/reference address from a getter if your flow needs it.
    // let nested = read_address(&mut evm, contract,
    //     Bytes::from(selector("target()").to_vec()), 2_000_000)?;

    Ok(())
}

/// Replace with YOUR contract's creation bytecode hex (see init_code above).
const YOUR_CREATION_BYTECODE: &[u8] = @@BYTECODE@@;
"""


def main() -> int:
    p = argparse.ArgumentParser(description="Scaffold a per-contract revm investigation harness")
    p.add_argument("--name", required=True, help="crate/harness name, e.g. my_executor")
    p.add_argument("--artifact", help="foundry artifact JSON (contract creation bytecode)")
    p.add_argument("--slot", action="append", default=[], help="storage slot as SLOT=VALUE hex")
    args = p.parse_args()

    out = OUT_ROOT / args.name
    if out.exists():
        print(f"refusing to overwrite existing {out}", file=sys.stderr)
        return 1
    (out / "src").mkdir(parents=True)

    bytecode_rust = "&[] // paste YOUR creation bytecode hex (see init_code above)"
    if args.artifact:
        ap = REPO / args.artifact if not Path(args.artifact).is_absolute() else Path(args.artifact)
        data = json.loads(ap.read_text())
        bc = data.get("bytecode", {}).get("object", "")
        if bc and bc != "0x":
            raw = bc[2:] if bc.startswith("0x") else bc
            bytecode_rust = 'b"' + raw + '"'
            print(f"preloaded {len(raw)//2} creation bytes from {ap}")

    cargo = CARGO_TEMPLATE.replace("@@NAME@@", args.name)
    main = (
        MAIN_TEMPLATE.replace("@@NAME@@", args.name)
        .replace("@@ENV@@", args.name.upper())
        .replace("@@BYTECODE@@", bytecode_rust)
    )

    (out / "Cargo.toml").write_text(cargo)
    (out / "src" / "main.rs").write_text(main)
    print(f"scaffolded {out}")
    print(f"  cd {out.relative_to(REPO)}")
    print("  # edit src/main.rs for YOUR contract, then:")
    print("  cargo run")
    return 0


if __name__ == "__main__":
    sys.exit(main())
