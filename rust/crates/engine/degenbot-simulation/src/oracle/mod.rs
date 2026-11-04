//! Contract-agnostic in-process evm2 **fixture driver** — deploy a pinned
//! contract artifact, run staged calls, resolve addresses, seed storage slots,
//! drive a call, and classify the verdict (Solidity `Revert` vs a verbless
//! `Halt`), then read back output + logs.
//!
//! This is the genuinely reusable spine the tier-3 on-chain pool oracles and any
//! **individual user investigation harness** share. It knows nothing about
//! pools, executors, or arbitrage strategies: it only sequences EVM transactions
//! against a fresh self-contained in-memory database, so a harness for *any*
//! contract — `UniswapV3Pool`, a user's custom executor, a lending market — is a
//! thin family-specific driver on top, not a re-derivation of the EVM plumbing.
//!
//! ADR-020: the tier-3 oracles deploy **canonical reference bytecode** and
//! assert the Rust math is byte-exact to it. ADR-091 R4 moved this driver from
//! revm to evm2 ([`InMemoryDB`] + [`Evm`]) — the engine production runs. The
//! oracle's independence comes from the reference bytecode, not from the
//! interpreter executing it, so the interpreter swap leaves the guarantee
//! intact.
//!
//! # Spec pin
//!
//! Fixtures execute at **Cancun** ([`SPEC`]): the hardfork the modern harness
//! builds target (`PUSH0`/`MCOPY` in solc 0.8.2x output), pinned here rather
//! than read from the production engine's spec. The committed Tier-3 artifacts
//! are compiled for it.
//!
//! Test/diagnostic driver only: deliberately has no FFI surface (not
//! exposed through `degenbot._ffi`).

use std::path::Path;

use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{keccak256, Address, Bytes, Log, TxKind, U256};
use serde_json::Value;

use ::evm2::env::BlockEnv;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::InMemoryDB;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, Version};

/// The hardfork fixture transactions execute at (see the module doc).
const SPEC: SpecId = SpecId::CANCUN;

/// The one caller every fixture transaction comes from: the zero address.
///
/// This is the sender every committed fixture artifact is pinned to. A
/// deployment's address is `keccak(rlp(sender, nonce))[12..]`, so the sender
/// fixes the whole deployed-address sequence a fixture sees — the capture
/// cassettes' contract/log addresses and the generators' pinned literals. A
/// different sender re-derives all of them and stops a regeneration matching
/// the corpus, so this value is data, not a free choice. The caller's own
/// account lifecycle stays out of the picture either way: a fixture never
/// asserts on the sender.
const CALLER: Address = Address::ZERO;

/// The chain id the fixture transaction envelopes declare — evm2's
/// `Version::chain_id` default (and a fixture chain is mainnet-shaped).
const CHAIN_ID: u64 = 1;

/// A fixture EVM: evm2's [`Evm`] over a self-contained [`InMemoryDB`], plus the
/// runtime [`Version`] and block environment the knob setters rewrite.
///
/// evm2 bakes the version and block env into the engine at construction and
/// offers no getter for them, so the driver keeps its own copies and reinstalls
/// them as one atomic [`Evm::set_block_and_execution_config_within_spec`]
/// update — which preserves the accepted-state overlay, so a knob change never
/// costs a fixture its deployments or seeded storage.
pub struct FixtureEvm {
    evm: Evm<'static, BaseEvmTypes>,
    version: Version,
    block: BlockEnv,
}

impl FixtureEvm {
    /// The underlying engine, for a consumer that needs a surface this driver
    /// does not wrap (state publication, inspectors).
    #[must_use]
    pub const fn engine(&self) -> &Evm<'static, BaseEvmTypes> {
        &self.evm
    }

    /// The underlying engine, mutably (see [`Self::engine`]).
    pub const fn engine_mut(&mut self) -> &mut Evm<'static, BaseEvmTypes> {
        &mut self.evm
    }

    /// Reinstall the driver's runtime version + block env on the engine,
    /// keeping its accepted state (the atomic same-spec update).
    fn install_runtime(&mut self) {
        let config = ExecutionConfig::for_spec_and_version(SPEC, self.version);
        self.evm
            .set_block_and_execution_config_within_spec(self.block, config);
    }
}

/// A successful transaction's output — the evm2-native mirror of revm's
/// `ExecutionResult::Output`, with the shape the fixture consumers read: a call
/// returns its data, a creation returns its deployed code and the created
/// address (absent when the creation consumed itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// A call's return data.
    Call(Bytes),
    /// A creation's deployed runtime bytecode + the created address.
    Create(Bytes, Option<Address>),
}

/// Verdict of a single driven transaction. A real Solidity `Revert` (a
/// math-level verdict that must be matched by the caller) is kept distinct from
/// a verbless `Halt` (OOG / no EVM verdict), per ADR-020's H1 recurrence-rule.
#[derive(Debug)]
pub enum Verdict {
    /// The transaction succeeded; the raw [`Output`] (for a `Create`, this is
    /// `Output::Create(bytes, address)`; for a `Call`, `Output::Call(bytes)`)
    /// plus the emitted logs.
    Accepted { output: Output, logs: Vec<Log> },
    /// Reverted via a Solidity `REVERT`; `reason` is the raw revert return-data.
    Reverted(Bytes),
    /// The EVM halted (OOG / invalid opcode) with no verdict.
    Halted(String),
}

/// Build a fresh, self-contained evm2 EVM over an empty in-memory database at
/// [`SPEC`]. Callers typically build one per probe so storage starts pristine.
#[must_use]
pub fn new_fixture_evm() -> FixtureEvm {
    let version = Version::new(SPEC);
    let block = BlockEnv::<BaseEvmTypes>::default();
    let evm = Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SPEC, version),
        SPEC,
        block,
        ethereum_tx_registry(SPEC),
        InMemoryDB::default(),
        Precompiles::base(SPEC),
    );
    FixtureEvm {
        evm,
        version,
        block,
    }
}

/// Toggle the EVM's nonce check. The driver already stamps each envelope with
/// the caller's live nonce, so a fixture sequence validates with the check
/// either way — the toggle is for a probe that wants the engine to accept a
/// hand-built transaction regardless of nonce.
pub fn set_disable_nonce_check(evm: &mut FixtureEvm, disable: bool) {
    if disable {
        evm.version.features.remove(EvmFeatures::NONCE_CHECK);
    } else {
        evm.version.features.insert(EvmFeatures::NONCE_CHECK);
    }
    evm.install_runtime();
}

/// Override the code-size limits (both deployed code and initcode). Pass `None`
/// to remove the cap entirely (deploying EOF/Pancake-style oversized
/// harnesses); `Some(n)` restores a specific cap.
pub fn set_code_size_limits(evm: &mut FixtureEvm, max: Option<usize>) {
    let cap = max.unwrap_or(usize::MAX);
    evm.version.max_code_size = cap;
    evm.version.max_initcode_size = cap;
    evm.install_runtime();
}

/// Override the block's gas limit. evm2's `BlockEnv::default()` sets
/// `gas_limit` to `u64::MAX` (not a constraint), so this is usually a no-op —
/// but it is the tx-vs-block ceiling if a caller ever narrows it. Keep it
/// explicit for gas-limit experiments (e.g. testing a live bot's hard-coded 5M
/// `execute()` cap against a realistic mainnet block gas limit).
pub fn set_block_gas_limit(evm: &mut FixtureEvm, gas: u64) {
    evm.block.gas_limit = U256::from(gas);
    evm.install_runtime();
}

/// Override the EIP-7825 per-tx gas-limit cap (`Version::tx_gas_limit_cap`).
/// [`SPEC`] (Cancun) predates EIP-7825, so the base version leaves it uncapped
/// (`u64::MAX`); a probe that wants the mainnet/EIP-7825 ceiling installs the
/// Osaka figure explicitly. Set it to `u64::MAX` to eliminate the per-tx
/// ceiling entirely for a gas experiment.
pub fn set_tx_gas_limit_cap(evm: &mut FixtureEvm, cap: u64) {
    evm.version.tx_gas_limit_cap = cap;
    evm.install_runtime();
}

/// Execute a raw transaction (`Create` or `Call`) and classify the result.
///
/// The caller matches on [`Verdict`] to distinguish a math-level `Reverted`
/// from a verbless `Halted`, exactly as the tier-3 oracles require (H1).
pub fn transact(evm: &mut FixtureEvm, spec: TxSpec) -> Verdict {
    run(evm, spec).0
}

/// Execute a raw transaction like [`transact`], additionally returning the gas
/// the transaction consumed (the receipt gas-used value). Lets a probe
/// attribute a `Halt` to out-of-gas (gas_used ≈ gas_limit) vs a genuine
/// infeasible revert (gas_used ≪ gas_limit).
pub fn transact_with_gas(evm: &mut FixtureEvm, spec: TxSpec) -> (Verdict, u64) {
    run(evm, spec)
}

/// One transaction (`spec`) through `evm`, settled and committed: the verdict
/// plus the gas it used. Both `transact` verbs are this function's halves.
fn run(evm: &mut FixtureEvm, spec: TxSpec) -> (Verdict, u64) {
    let nonce = caller_nonce(evm);
    let (kind, tx) = match spec {
        TxSpec::Deploy { init_code, gas } => (
            TxKind::Create,
            envelope(TxKind::Create, init_code, gas, nonce),
        ),
        TxSpec::Call { to, data, gas } => (
            TxKind::Call(to),
            envelope(TxKind::Call(to), data, gas, nonce),
        ),
    };
    let executed = match evm.evm.transact(&tx) {
        Ok(executed) => executed,
        Err(e) => return (Verdict::Halted(format!("transact error: {e:?}")), 0),
    };
    let result = executed.commit();
    let gas_used = result.tx_gas_used();
    let verdict = if result.status {
        let output = match kind {
            TxKind::Create => Output::Create(result.output, result.created_address),
            TxKind::Call(_) => Output::Call(result.output),
        };
        Verdict::Accepted {
            output,
            logs: result.logs,
        }
    } else if result.is_revert() {
        Verdict::Reverted(result.output)
    } else {
        Verdict::Halted(format!("halted: {:?}", result.stop))
    };
    (verdict, gas_used)
}

/// The caller's live account nonce (see the envelope builder for why the
/// envelope carries it).
fn caller_nonce(evm: &FixtureEvm) -> u64 {
    evm.evm
        .overlay_db()
        .account_info(&CALLER)
        .map_or(0, |info| info.nonce)
}

/// The fixture transaction envelope: a zero-fee EIP-1559 call or creation from
/// [`CALLER`] — the `eth_call`-shaped transaction a fixture sequences, not a
/// signed block transaction.
///
/// The nonce is the caller's **live account nonce** (the accepted-state
/// overlay after the previous fixture transaction), not a constant: evm2 derives
/// a transaction-level `CREATE` address from the envelope nonce, so a
/// constant-zero envelope would address every successive deployment at the same
/// contract address and collide. Reading it back from the state keeps the
/// driver correct under both nonce-check modes and after a refused transaction.
fn envelope(to: TxKind, input: Bytes, gas_limit: u64, nonce: u64) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: CHAIN_ID,
            nonce,
            gas_limit,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            to,
            value: U256::ZERO,
            input,
            ..Default::default()
        }),
        CALLER,
    )
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
    let db = evm.evm.state_mut().overlay_db_mut();
    for &(slot, value) in slots {
        db.insert_account_storage(&account, &slot, &value);
    }
}

/// Set the native (ETH) balance of an account, preserving its code + nonce + storage.
/// This is how the harness funds native-V4 flows (the PoolManager's holdings
/// that back `take` of a native delta, and the executor's native seed). The
/// plain `Token`-mint path can't mint `eth`, so native money is seeded here.
pub fn set_native_balance(evm: &mut FixtureEvm, account: Address, balance: U256) {
    let overlay = evm.evm.state_mut().overlay_db_mut();
    let mut info = overlay.account_info(&account).cloned().unwrap_or_default();
    info.balance = balance;
    overlay.insert_account_info(&account, info);
}

/// Read the native (ETH) balance of an account (the EVM account balance, not
/// an ERC-20 `balanceOf`).
pub fn native_balance_of(evm: &mut FixtureEvm, account: Address) -> U256 {
    evm.evm
        .overlay_db()
        .account_info(&account)
        .map_or(U256::ZERO, |info| info.balance)
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
