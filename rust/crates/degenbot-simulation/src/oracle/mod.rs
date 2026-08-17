//! Contract-agnostic in-process revm **fixture driver** — deploy a pinned
//! contract artifact, run staged calls, resolve addresses, seed storage slots,
//! drive a call, and classify the verdict (Solidity `Revert` vs a verbless
//! `Halt`), then read back output + logs.
//!
//! This is the genuinely reusable spine the tier-3 on-chain pool oracles and any
//! **individual user investigation harness** share. It knows nothing about
//! pools, executors, or arbitrage strategies: it only sequences EVM transactions against a
//! fresh self-contained `CacheDB`, so a harness for *any* contract —
//! `UniswapV3Pool`, a user's custom executor, a lending market — is a thin
//! family-specific driver on top, not a re-derivation of the EVM plumbing.
//!
//! ADR-020: the tier-3 oracles deploy **canonical reference bytecode** into a
//! revm `CacheDB` and assert the Rust math is byte-exact to it. This module is
//! that oracle's landing zone relocated out of the test commons so it is real
//! crate surface (`cargo add degenbot-simulation`) instead of test-only.
//!
//! Test/diagnostic driver only: deliberately has no FFI surface (not
//! exposed through `degenbot._ffi`).

use std::path::Path;

use alloy::primitives::{keccak256, Address, Bytes, Log, U256};
use revm::context::TxEnv;
use revm::context_interface::result::ExecutionResult;
pub use revm::context_interface::result::Output;
use revm::context_interface::ContextTr;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};
use serde_json::Value;

/// Concrete revm EVM over a self-contained empty `CacheDB` — what the fixture
/// driver sequences calls against. Mirrors the crate's production
/// [`evm::simulator::BlockEvm`](evm::simulator) shape on an empty DB.
pub type FixtureEvm = revm::MainnetEvm<revm::handler::MainnetContext<CacheDB<EmptyDB>>>;

/// Verdict of a single driven transaction. A real Solidity `Revert` (a
/// math-level verdict that must be matched by the caller) is kept distinct from
/// a verbless `Halt` (OOG / no EVM verdict), per ADR-020's H1 recurrence-rule.
#[derive(Debug)]
pub enum Verdict {
    /// The transaction succeeded; the raw `Output` (for a `Create`, this is
    /// `Output::Create(bytes, address)`; for a `Call`, `Output::Call(bytes)`)
    /// plus the emitted logs.
    Accepted { output: Output, logs: Vec<Log> },
    /// Reverted via a Solidity `REVERT`; `reason` is the raw revert return-data.
    Reverted(Bytes),
    /// The EVM halted (OOG / invalid opcode) with no verdict.
    Halted(String),
}

/// Build a fresh, self-contained revm EVM over an empty `CacheDB`. Callers
/// typically build one per probe so storage starts pristine.
#[must_use]
pub fn new_fixture_evm() -> FixtureEvm {
    let db = CacheDB::new(EmptyDB::default());
    revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet()
}

/// Toggle revm's nonce check (the fixture driver usually wants it off so it can
/// send arbitrary staged transactions).
pub fn set_disable_nonce_check(evm: &mut FixtureEvm, disable: bool) {
    evm.ctx.cfg.disable_nonce_check = disable;
}

/// Override revm's code-size limits (both contract and initcode). Pass `None`
/// to remove the cap entirely (deploying EOF/Pancake-style oversized harnesses);
/// `Some(n)` restores a specific cap.
pub fn set_code_size_limits(evm: &mut FixtureEvm, max: Option<usize>) {
    evm.ctx.cfg.limit_contract_code_size = max;
    evm.ctx.cfg.limit_contract_initcode_size = max;
}

/// Override the block's gas limit. revm's default `BlockEnv::default()` sets
/// `block.gas_limit` to `u64::MAX` (not a constraint), so this is usually a
/// no-op — but it is the tx-vs-block ceiling if a caller ever narrows it.
/// Keep it explicit for gas-limit experiments (e.g. testing a live bot's
/// hard-coded 5M `execute()` cap against a realistic mainnet block gas limit).
pub fn set_block_gas_limit(evm: &mut FixtureEvm, gas: u64) {
    evm.ctx.modify_block(|block| {
        block.gas_limit = gas;
    });
}

/// Override revm's EIP-7825 per-tx gas-limit cap (the `tx_gas_limit_cap` cfg
/// knob). On modern specs (Osaka+) this defaults to `TX_GAS_LIMIT_CAP`
/// (16,777,216) and is the *binding* `TxGasLimitGreaterThanCap` ceiling for a
/// fixture tx — independent of `block.gas_limit`. Set it to `u64::MAX` to
/// eliminate the artificial per-tx ceiling entirely for a gas experiment.
pub fn set_tx_gas_limit_cap(evm: &mut FixtureEvm, cap: u64) {
    evm.ctx.cfg.tx_gas_limit_cap = Some(cap);
}

/// Execute a raw transaction (`Create` or `Call`) and classify the result.
///
/// The caller matches on [`Verdict`] to distinguish a math-level `Reverted`
/// from a verbless `Halted`, exactly as the tier-3 oracles require (H1).
///
/// # Panics
///
/// Panics if the revm transaction-environment builder rejects the provided
/// spec (a programmer error — both `Deploy` and `Call` specs are valid).
pub fn transact(evm: &mut FixtureEvm, spec: TxSpec) -> Verdict {
    #[expect(clippy::expect_used)] // valid tx env by construction (documented)
    let tx = match spec {
        TxSpec::Deploy { init_code, gas } => TxEnv::builder()
            .kind(TxKind::Create)
            .gas_limit(gas)
            .data(init_code)
            .build()
            .expect("valid deploy tx env"),
        TxSpec::Call { to, data, gas } => TxEnv::builder()
            .kind(TxKind::Call(to))
            .gas_limit(gas)
            .data(data)
            .build()
            .expect("valid call tx env"),
    };
    match evm.transact(tx) {
        Ok(res) => {
            let out = match res.result {
                ExecutionResult::Success { output, logs, .. } => {
                    evm.commit(res.state);
                    Verdict::Accepted { output, logs }
                }
                ExecutionResult::Revert { output, .. } => {
                    evm.commit(res.state);
                    Verdict::Reverted(output)
                }
                ExecutionResult::Halt { reason, .. } => {
                    evm.commit(res.state);
                    Verdict::Halted(format!("halted: {reason:?}"))
                }
            };
            out
        }
        Err(e) => Verdict::Halted(format!("transact error: {e:?}")),
    }
}

/// Execute a raw transaction like [`transact`], additionally returning the
/// gas the transaction consumed (`res.gas_used`). Lets a probe attribute a
/// `Halt` to out-of-gas (gas_used ≈ gas_limit) vs a genuine infeasible revert
/// (gas_used ≪ gas_limit).
///
/// # Panics
///
/// Panics if the revm transaction-environment builder rejects the provided
/// spec (see [`transact`]).
pub fn transact_with_gas(evm: &mut FixtureEvm, spec: TxSpec) -> (Verdict, u64) {
    #[expect(clippy::expect_used)] // valid tx env by construction (documented)
    let tx = match spec {
        TxSpec::Deploy { init_code, gas } => TxEnv::builder()
            .kind(TxKind::Create)
            .gas_limit(gas)
            .data(init_code)
            .build()
            .expect("valid deploy tx env"),
        TxSpec::Call { to, data, gas } => TxEnv::builder()
            .kind(TxKind::Call(to))
            .gas_limit(gas)
            .data(data)
            .build()
            .expect("valid call tx env"),
    };
    match evm.transact(tx) {
        Ok(res) => {
            let gas_used = res.result.tx_gas_used();
            let out = match res.result {
                ExecutionResult::Success { output, logs, .. } => {
                    evm.commit(res.state);
                    Verdict::Accepted { output, logs }
                }
                ExecutionResult::Revert { output, .. } => {
                    evm.commit(res.state);
                    Verdict::Reverted(output)
                }
                ExecutionResult::Halt { reason, .. } => {
                    evm.commit(res.state);
                    Verdict::Halted(format!("halted: {reason:?}"))
                }
            };
            (out, gas_used)
        }
        Err(e) => (Verdict::Halted(format!("transact error: {e:?}")), 0),
    }
}

/// One transaction to sequence through [`transact`].
pub enum TxSpec {
    /// Contract creation from `init_code`.
    Deploy { init_code: Bytes, gas: u64 },
    /// A call to an existing contract.
    Call { to: Address, data: Bytes, gas: u64 },
}

/// Deploy a contract and return its address, or a descriptive error (the
/// harness artifact deploy failing is a fixture problem, not a verdict).
///
/// # Errors
///
/// Returns an `Err` if the deployment reverts, halts, or succeeds without
/// yielding a `CREATE` address.
pub fn deploy(evm: &mut FixtureEvm, init_code: Bytes, gas: u64) -> Result<Address, String> {
    match transact(evm, TxSpec::Deploy { init_code, gas }) {
        Verdict::Accepted {
            output: Output::Create(_, Some(addr)),
            ..
        } => Ok(addr),
        Verdict::Accepted {
            output: Output::Create(_, None),
            ..
        } => Err("deploy succeeded without an address (Create output missing addr)".to_string()),
        Verdict::Accepted { output, .. } => Err(format!(
            "deploy succeeded with non-Create output {output:?}"
        )),
        Verdict::Reverted(r) => Err(format!("deploy reverted: {r:?}")),
        Verdict::Halted(h) => Err(format!("deploy halted: {h}")),
    }
}

/// Extract an `address` from a call that returned a 32-byte word (e.g. a Solidity
/// `pool()` getter). Reads the trailing 20 bytes after the leading 12 zero bytes.
///
/// # Errors
///
/// Returns an `Err` if the getter call reverts/halts or returns fewer than 32
/// bytes.
pub fn read_address(
    evm: &mut FixtureEvm,
    to: Address,
    data: Bytes,
    gas: u64,
) -> Result<Address, String> {
    let out = call_bytes(evm, to, data, gas)?;
    if out.len() < 32 {
        return Err(format!("address getter returned {} bytes", out.len()));
    }
    Ok(Address::from_slice(&out.as_ref()[12..32]))
}

/// Run a call and return the raw success output bytes, or a descriptive error
/// on revert/halt (for calls that must succeed, like setters/getters).
///
/// # Errors
///
/// Returns an `Err` if the call reverts or halts, or if it unexpectedly returns
/// a `Create` output.
pub fn call_bytes(
    evm: &mut FixtureEvm,
    to: Address,
    data: Bytes,
    gas: u64,
) -> Result<Bytes, String> {
    match transact(evm, TxSpec::Call { to, data, gas }) {
        Verdict::Accepted {
            output: Output::Call(b),
            ..
        } => Ok(b),
        Verdict::Accepted {
            output: Output::Create(..),
            ..
        } => Err("call unexpectedly returned a Create output".to_string()),
        Verdict::Reverted(r) => Err(format!("call reverted: {r:?}")),
        Verdict::Halted(h) => Err(format!("call halted: {h}")),
    }
}

/// Seed one storage slot of an account in the evm's DB. `slots` are
/// `(slot_index, value)` pairs — the raw contract storage layout, independent
/// of any high-level pool/executor interpretation.
pub fn seed_slots(evm: &mut FixtureEvm, account: Address, slots: &[(U256, U256)]) {
    let db = evm.ctx.db_mut();
    for &(slot, value) in slots {
        db.insert_account_storage(account, slot, value).ok();
    }
}

/// Set the native (ETH) balance of an account, preserving its code + nonce + storage.
/// This is how the harness funds native-V4 flows (the PoolManager's holdings
/// that back `take` of a native delta, and the executor's native seed). The
/// plain `Token`-mint path can't mint `eth`, so native money is seeded here.
pub fn set_native_balance(evm: &mut FixtureEvm, account: Address, balance: U256) {
    use revm::DatabaseRef as _;
    let db = evm.ctx.db_mut();
    let existing = db.basic_ref(account).ok().flatten().unwrap_or_default();
    db.insert_account_info(
        account,
        revm::state::AccountInfo {
            balance,
            ..existing
        },
    );
}

/// Read the native (ETH) balance of an account (the revm account balance, not
/// an ERC-20 `balanceOf`).
pub fn native_balance_of(evm: &mut FixtureEvm, account: Address) -> U256 {
    use revm::DatabaseRef as _;
    let db = evm.ctx.db_mut();
    db.basic_ref(account)
        .ok()
        .flatten()
        .map(|a| a.balance)
        .unwrap_or_default()
}

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
#[must_use]
pub fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Decode a Solidity `Error(string)` revert payload to its message. Returns
/// `None` for anything that isn't that exact ABI shape (bare reasonless revert,
/// `Panic(uint256)`, …).
#[must_use]
pub fn decode_error_string(reason: &[u8]) -> Option<String> {
    if reason.len() < 4 || reason[..4] != [0x08, 0xc3, 0x79, 0xa0] {
        return None;
    }
    let data = &reason[4..];
    if data.len() < 64 {
        return None;
    }
    let len = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?).to::<usize>();
    if 64 + len > data.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&data[64..64 + len]).into_owned())
}

/// Extract a foundry artifact's creation bytecode (`bytecode.object`) from its
/// JSON. The artifact is whatever was placed at the leaf `bytecode.object` in
/// the standard foundry `out/<File>.sol/<Contract>.json` shape.
///
/// # Errors
///
/// Returns an `Err` if the JSON is malformed, lacks `bytecode.object`, or the
/// hex fails to decode.
pub fn parse_foundry_creation_bytecode(artifact_json: &str) -> Result<Vec<u8>, String> {
    let v: Value =
        serde_json::from_str(artifact_json).map_err(|e| format!("invalid artifact JSON: {e}"))?;
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| "artifact has no bytecode.object".to_string())?;
    alloy::hex::decode(hex_str.trim_start_matches("0x")).map_err(|e| format!("hex decode: {e}"))
}

/// Load a foundry build artifact from an `out/` tree at `dir`:
/// `dir/<File>.sol/<Contract>.json`, and return its creation bytecode.
///
/// # Errors
///
/// Returns an `Err` if the file cannot be read or its creation bytecode parsed.
pub fn load_foundry_creation_bytecode(
    dir: &Path,
    file: &str,
    contract: &str,
) -> Result<Vec<u8>, String> {
    let path = dir.join(file).join(format!("{contract}.json"));
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("read artifact {}: {e}", path.display()))?;
    parse_foundry_creation_bytecode(&raw)
}
