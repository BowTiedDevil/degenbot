//! The `eth_simulateV1` `SimulatePayload` 7-call builder (B2).
//!
//! Ports the `SimulateV1Payload` / `BlockStateCallV1` 7-call vector assembly
//! from `examples/eth_backrun_v2_v3_v4_rust.py` (L2021–L2048). The vector is:
//!
//! ```text
//! [0] WETH9    balanceOf(executor)   — physical ERC-20 balance before
//! [1] Multicall3 getEthBalance(exec)  — ETH balance before
//! [2] PM       balanceOf(exec, weth) — V4-minted ERC6909 balance before
//! [3] cmd_executor execute(commands) — the backrun (CONSUMED from YQORTM)
//! [4] WETH9    balanceOf(executor)   — physical ERC-20 balance after
//! [5] Multicall3 getEthBalance(exec) — ETH balance after
//! [6] PM       balanceOf(exec, weth) — V4-minted ERC6909 balance after
//! ```
//!
//! All seven calls share a single `BlockStateCallV1` so they execute against
//! ONE applied `stateOverrides` (the executor's code injection + ETH/WETH
//! funding + warmup slots — built by [`build_simulation_state_overrides`]).
//! The orchestration diffs `[0]/[2]` vs `[4]/[6]` to detect profit.
//!
//! The dispatch owns NO business logic: the `execute()` ABI tone (selector +
//! `(bytes, uint256)` encoding) lives in `degenbot_executor::encode_execute_call`
//! (the §YQORTM leaf); the calldata builders live in [`calldata`]; the
//! `stateOverrides` live in [`build_simulation_state_overrides`]. This module
//! only ASSEMBLES them into the Rust `SimulatePayload` shape the typed
//! `degenbot_rpc::AlloyProvider::eth_simulate_v1` consumes.

// Solidity/RPC identifiers (eth_simulateV1, BlockStateCallV1, etc.) are
// ubiquitous here.
#![allow(clippy::doc_markdown)]

use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::eth::simulate::{SimBlock, SimulatePayload};
use alloy::rpc::types::eth::state::StateOverride;
use alloy::rpc::types::TransactionRequest;
use degenbot_core::errors::AbiDecodeError;
use degenbot_executor::composers::encode_execute_call;

use crate::calldata::{
    encode_balance_of_calldata, encode_erc6909_balance_of_calldata, encode_get_eth_balance_calldata,
};

/// The number of calls in the standard simulation vector (3 pre + 1 execute + 3 post).
pub const SIM_CALL_COUNT: usize = 7;

/// The gas cap the Python oracle grants the `execute()` call (ports the
/// `gas=5_000_000` literal at L1993 — "Generous gas for V3 tick-crossing
/// swaps").
pub const EXECUTE_CALL_GAS: u64 = 5_000_000;

/// Parameters for building the simulation payload.
///
/// All inputs that vary per-simulation are gathered here; the `execute()` ABI
/// encoding, the calldata builders, and the `stateOverrides` construction are
/// delegated to their owning leaves.
#[derive(Debug, Clone)]
pub struct SimulationParams {
    /// The operator key's address — the `from` of the `execute()` call + the
    /// owner funded with ETH in `stateOverrides`.
    pub executor_owner: Address,
    /// The cmd_executor contract address — the `to` of the `execute()` call +
    /// the address whose balances are diffed.
    pub executor_address: Address,
    /// WETH9 contract address — `balanceOf` target + `stateOverrides` balanceOf slot host.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address — ERC6909 `balanceOf` host + warmup host.
    pub pool_manager_address: Address,
    /// Multicall3 contract address — the `getEthBalance(address)` target for
    /// the folded ETH balance read.
    pub multicall3_address: Address,
    /// The `execute(bytes, uint256)` ABI-wrapped command-stream bytes (the
    /// `cmd_bytes` already encoded by `degenbot_executor::encode_cmd_stream` /
    /// `encode_execute_call`).
    pub execute_calldata: Bytes,
    /// Whether the `execute()` call carries an EIP-2930 `accessList` (pre-computed
    /// via `eth_createAccessList` so `gas_used` reflects warm-slot savings).
    /// `None` simulates without an access list.
    pub access_list: Option<alloy::rpc::types::AccessList>,
}

/// Build the `eth_simulateV1` `SimulatePayload` for the 7-call profit vector.
///
/// Assembles a single `SimBlock` carrying:
/// - the 7-call `calls` vector (`[0..3]` pre-balance reads, `[3]` the
///   `execute()` call, `[4..7]` post-balance reads), and
/// - the `stateOverrides` (built by [`build_simulation_state_overrides`]
///   for `params.executor_owner` / inject flags).
///
/// The payload is exactly the shape the typed
/// `degenbot_rpc::AlloyProvider::eth_simulate_v1` consumes; the caller passes
/// `block = "pending"` at the dispatch layer (the §4.2 wire-shape parity
/// covers the `blockStateCalls` JSON).
///
/// # Arguments
///
/// - `params` — the per-simulation inputs (see [`SimulationParams`]).
/// - `state_overrides` — the `stateOverrides` map from
///   [`build_simulation_state_overrides`] (the sibling TCTUAW leaf).
#[must_use]
pub fn build_simulate_payload(
    params: &SimulationParams,
    state_overrides: StateOverride,
) -> SimulatePayload {
    let weth_call = balance_call(params.weth_address, params.executor_address);
    let eth_call = balance_call(params.multicall3_address, params.executor_address);
    let erc6909_call = erc6909_balance_call(
        params.pool_manager_address,
        params.executor_address,
        params.weth_address,
    );
    let execute_call = execute_call(params);

    let calls = vec![
        weth_call.clone(),    // [0] WETH balance before
        eth_call.clone(),     // [1] ETH balance before
        erc6909_call.clone(), // [2] ERC6909 WETH balance before
        execute_call,         // [3] execute(commands)
        weth_call,            // [4] WETH balance after
        eth_call,             // [5] ETH balance after
        erc6909_call,         // [6] ERC6909 WETH balance after
    ];

    let block = SimBlock::default()
        .with_state_overrides(state_overrides)
        .extend_calls(calls);

    SimulatePayload::default().extend(block)
}

/// Build a read-only `balanceOf(address)` `TransactionRequest` for a contract.
fn balance_call(target: Address, account: Address) -> TransactionRequest {
    TransactionRequest {
        to: Some(target.into()),
        input: alloy::rpc::types::eth::TransactionInput::new(
            encode_balance_of_calldata(account).expect("valid address encodes"),
        ),
        ..Default::default()
    }
}

/// Build the Multicall3 `getEthBalance(address)` `TransactionRequest`.
fn _eth_balance_call(multicall3: Address, account: Address) -> TransactionRequest {
    TransactionRequest {
        to: Some(multicall3.into()),
        input: alloy::rpc::types::eth::TransactionInput::new(
            encode_get_eth_balance_calldata(account).expect("valid address encodes"),
        ),
        ..Default::default()
    }
}

/// Build the PoolManager ERC6909 `balanceOf(address,uint256)` `TransactionRequest`.
fn erc6909_balance_call(
    pool_manager: Address,
    account: Address,
    weth: Address,
) -> TransactionRequest {
    TransactionRequest {
        to: Some(pool_manager.into()),
        input: alloy::rpc::types::eth::TransactionInput::new(
            encode_erc6909_balance_of_calldata(account, weth).expect("valid address + weth encode"),
        ),
        ..Default::default()
    }
}

/// Build the `execute(bytes, uint256)` `TransactionRequest` (the backrun call).
///
/// Mirrors the `tx_params` block at L1983–L1997: `to = executor_address`,
/// `data = execute_calldata`, `chainId = 1`, `type = 2`, `value = 0`,
/// `gas = 5_000_000`, zero fees, `nonce = 0`, `from = executor_owner`, and the
/// optional `accessList`.
fn execute_call(params: &SimulationParams) -> TransactionRequest {
    TransactionRequest {
        from: Some(params.executor_owner),
        to: Some(params.executor_address.into()),
        input: alloy::rpc::types::eth::TransactionInput::new(params.execute_calldata.clone()),
        chain_id: Some(1),
        transaction_type: Some(2),
        value: Some(U256::ZERO),
        gas: Some(EXECUTE_CALL_GAS),
        max_fee_per_gas: Some(0),
        max_priority_fee_per_gas: Some(0),
        nonce: Some(0),
        access_list: params.access_list.clone(),
        ..Default::default()
    }
}

/// Convenience: ABI-wrap the `execute(bytes, uint256)` call from its parts.
///
/// Delegates to `degenbot_executor::composers::encode_execute_call` (the
/// §YQORTM leaf) — the selector + the `(bytes, uint256)` ABI encoding live
/// there.
///
/// # Errors
///
/// Returns [`AbiDecodeError`] if the `(bytes, uint256)` encoding fails.
pub fn wrap_execute_calldata(
    executor_address: Address,
    cmd_bytes: &[u8],
    config: U256,
) -> Result<Bytes, AbiDecodeError> {
    let encoded = encode_execute_call(executor_address, cmd_bytes, config)?;
    Ok(Bytes::from(encoded.data))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::build_simulation_state_overrides;
    use alloy::primitives::address;
    use degenbot_executor::WarmupSlots;

    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
    const RUNTIME_BYTECODE: &[u8] = &0xDEAD_BEEF_u32.to_be_bytes();

    fn warmup() -> WarmupSlots {
        degenbot_executor::compute_simulation_warmup_slots(EXECUTOR, WETH, PM)
    }

    fn params(execute: Bytes) -> SimulationParams {
        SimulationParams {
            executor_owner: OWNER,
            executor_address: EXECUTOR,
            weth_address: WETH,
            pool_manager_address: PM,
            multicall3_address: MULTICALL3,
            execute_calldata: execute,
            access_list: None,
        }
    }

    // ---------------------------------------------------------------------
    // §4.2 wire-shape parity vs the Python oracle's `SimulateV1Payload` /
    // `BlockStateCallV1` (L2021–L2048). The Rust `SimulatePayload` serde
    // output must carry: one `blockStateCalls`, 7 `calls`, and
    // `stateOverrides` carrying the executor injection + funding.
    // ---------------------------------------------------------------------

    #[test]
    fn payload_has_one_block_with_seven_calls() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let payload = build_simulate_payload(&params(Bytes::new()), overrides);

        assert_eq!(payload.block_state_calls.len(), 1);
        let block = &payload.block_state_calls[0];
        assert_eq!(block.calls.len(), SIM_CALL_COUNT);
        assert!(block.state_overrides.is_some());
    }

    #[test]
    fn call_three_is_the_execute_call_targeting_executor() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let payload = build_simulate_payload(&params(Bytes::new()), overrides);
        let execute = &payload.block_state_calls[0].calls[3];

        // The execute call targets the executor contract + comes from the owner.
        assert_eq!(execute.from, Some(OWNER));
        assert_eq!(execute.to, Some(EXECUTOR.into()));
        // type 2 + chain 1 + the 5M gas cap (ports L1990–L1993).
        assert_eq!(execute.transaction_type, Some(2));
        assert_eq!(execute.chain_id, Some(1));
        assert_eq!(execute.gas, Some(EXECUTE_CALL_GAS));
        assert_eq!(execute.value, Some(U256::ZERO));
    }

    #[test]
    fn pre_and_post_balance_calls_are_identical_pairs() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let payload = build_simulate_payload(&params(Bytes::new()), overrides);
        let calls = &payload.block_state_calls[0].calls;

        // [0]==[4] (WETH), [1]==[5] (Multicall3 ETH), [2]==[6] (ERC6909).
        assert_eq!(calls[0], calls[4]);
        assert_eq!(calls[1], calls[5]);
        assert_eq!(calls[2], calls[6]);
    }

    #[test]
    fn balance_calls_target_their_respective_contracts() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let payload = build_simulate_payload(&params(Bytes::new()), overrides);
        let calls = &payload.block_state_calls[0].calls;

        // [0]/[4] WETH9, [1]/[5] Multicall3, [2]/[6] PoolManager.
        assert_eq!(calls[0].to, Some(WETH.into()));
        assert_eq!(calls[1].to, Some(MULTICALL3.into()));
        assert_eq!(calls[2].to, Some(PM.into()));
    }

    #[test]
    fn execute_call_carries_the_calldata_verbatim() {
        let calldata = Bytes::from_static(&[0xab, 0x58, 0x98, 0xe8, 0xde, 0xad]);
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let payload = build_simulate_payload(&params(calldata.clone()), overrides);
        let execute = &payload.block_state_calls[0].calls[3];

        assert_eq!(execute.input.input, Some(calldata));
    }

    #[test]
    fn access_list_propagates_to_the_execute_call() {
        let access_list = alloy::rpc::types::AccessList::default();
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(EXECUTOR),
            RUNTIME_BYTECODE.into(),
            warmup(),
            WETH,
            PM,
        );
        let mut p = params(Bytes::new());
        p.access_list = Some(access_list.clone());
        let payload = build_simulate_payload(&p, overrides);
        let execute = &payload.block_state_calls[0].calls[3];

        assert_eq!(execute.access_list, Some(access_list));
    }

    #[test]
    fn wrap_execute_calldata_reuses_executor_leaf() {
        // The execute selector + (bytes, uint256) ABI wrapping live in the
        // §YQORTM leaf (encode_execute_call). This thin convenience just surfaces
        // the bytes; the parity corpus for the wrapping itself is pinned in
        // `cargo test -p degenbot-executor`.
        let calldata = wrap_execute_calldata(EXECUTOR, &[0x01, 0x02, 0x03], U256::ZERO).unwrap();
        // EXECUTE_SELECTOR = 0xab5898e8 (from the executor leaf).
        assert_eq!(&calldata[..4], &[0xab, 0x58, 0x98, 0xe8]);
    }
}
