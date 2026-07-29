//! Tier-3 forge→revm smoke test (ergo task `767HYN`, epic `UP5NH6`).
//!
//! Proves the full Tier-3 toolchain loop with zero pool math in a single
//! `#[test]`:
//!
//! 1. The `tier3-oracle/` Foundry project is compiled by `forge build`
//!    (run by the `just test-tier3-smoke` recipe) into standard `out/` JSON.
//! 2. This test parses the `deployedBytecode.object` hex from
//!    `tier3-oracle/out/Echo.sol/Echo.json`.
//! 3. It deploys that runtime bytecode into an offline revm `CacheDB<EmptyDB>`
//!    (the same `CacheDB<EmptyDB>` + `MainBuilder` + `transact` stack proven
//!    in `inspector_composition.rs`, but without the inspector — a plain
//!    state-changing `transact` is enough for a pure read).
//! 4. It calls `Echo.double(21)` and asserts the revm return decodes to `42`.
//!
//! Success means `forge build` → bytecode load → revm transact → argument
//! marshalling → assertion all work — every prerequisite the real oracle
//! tiers (3a `computeSwapStep`, 3b `Pool.swap`) need, validated before any
//! pool math lands.
//!
//! ## Why this is `#[ignore]`d (and the recipe un-ignores it)
//!
//! Plain `cargo test --workspace` (the pre-push hook + CI `test-rust`) does
//! NOT run `forge build` first, so `out/Echo.json` would be absent. The test
//! is therefore gated behind `#[ignore]` so the existing gate stays green
//! while the Tier-3 work is additive-only. The `just test-tier3-smoke` recipe
//! builds the harness via forge, then runs this test with `--include-ignored`.
//! The enforcement task (BQ43DK) is what eventually wires `test-tier3` into
//! `test-rust` so the foundry build is a hard CI prerequisite.

use std::path::PathBuf;

use alloy::primitives::{Address, Bytes, U256};
use revm::bytecode::Bytecode;
use revm::context::TxEnv;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use revm::{ExecuteEvm, MainBuilder, MainContext};

/// Path to the forge-emitted `Echo.json` artifact, resolved from this crate's
/// `CARGO_MANIFEST_DIR` (`rust/crates/degenbot-simulation`) up to the repo
/// root then into `tier3-oracle/out/`.
fn forge_echo_artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tier3-oracle/out/Echo.sol/Echo.json")
}

/// Load the runtime (`deployedBytecode.object`) hex from the forge JSON
/// artifact and return it as revm `Bytecode`. Panics with a recipe pointer if
/// the artifact is absent — the `just test-tier3-smoke` recipe runs `forge
/// build` before invoking this test, so a missing file means the caller ran
/// `--include-ignored` without the recipe.
fn load_echo_deployed_bytecode() -> Bytecode {
    let path = forge_echo_artifact_path();
    let path_display = path.display();
    let json_str = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "Tier-3 smoke: forge artifact not found at {path_display} ({err}). \
             Run `just test-tier3-smoke` (it builds the harness via `forge \
             build` before invoking this test), not bare `cargo test \
             --include-ignored`."
        )
    });
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("forge Echo.json must be valid JSON");
    let hex_str = parsed
        .get("deployedBytecode")
        .and_then(|v| v.get("object"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("forge Echo.json missing deployedBytecode.object: {parsed:?}"));
    let bytes = alloy::primitives::hex::decode(hex_str.trim_start_matches("0x"))
        .expect("deployedBytecode.object must be valid 0x-prefixed hex");
    Bytecode::new_raw(Bytes::from(bytes))
}

/// Calldata for `Echo.double(uint256 x)`: selector `0xeee97206` (`cast sig
/// "double(uint256)"`) followed by the 32-byte big-endian ABI-encoded `x`.
fn double_calldata(x: U256) -> Bytes {
    let mut data = Vec::with_capacity(4 + 32);
    // 0xeee97206 = selector.
    data.extend_from_slice(&[0xee, 0xe9, 0x72, 0x06]);
    // 32-byte big-endian argument.
    data.extend_from_slice(&x.to_be_bytes::<32>());
    Bytes::from(data)
}

/// Deploy `code` at `addr` into a fresh `CacheDB<EmptyDB>` (the exact offline
/// pattern from `inspector_composition.rs::db_with_contract`).
fn db_with_contract(addr: Address, code: Bytecode) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        addr,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            code: Some(code),
            ..Default::default()
        },
    );
    db
}

#[test]
#[ignore = "Tier-3 smoke: run via `just test-tier3-smoke` (runs `forge build` first)"]
fn forge_compiled_echo_contract_returns_doubled_value_in_revm() {
    let echo = Address::repeat_byte(0xEC);
    let code = load_echo_deployed_bytecode();
    let db = db_with_contract(echo, code);

    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    let tx = TxEnv::builder()
        .kind(TxKind::Call(echo))
        .gas_limit(1_000_000)
        .data(double_calldata(U256::from(21)))
        .build()
        .expect("well-formed tx");
    let result = evm.transact(tx).expect("revm transact must succeed");
    assert!(
        result.result.is_success(),
        "Echo.double must not revert: {:?}",
        result.result
    );
    let out = result
        .result
        .into_output()
        .expect("success must have output");
    let expected = U256::from(42);
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out.as_ref()[..32]);
    let got = U256::from_be_bytes(buf);
    assert_eq!(got, expected, "Echo.double(21) == 42; revm returned {got}");
}
