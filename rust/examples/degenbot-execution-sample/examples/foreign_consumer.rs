#![expect(clippy::expect_used, clippy::print_stdout)]

//! Runnable sample: a standalone-Rust user-defined `ExecutionAdapter` for a
//! FOREIGN contract (not `cmd_executor`).
//!
//! Run: `cargo run -p degenbot-execution-sample --example foreign_consumer`
//!
//! Constructs a `SimpleExecutor` strategy, projects a solved path into the
//! sealed `SolveResult` view, and encodes ONE complete transaction — target,
//! calldata, native value — through the four-part seam (Encode + declared Probe
//! reads + Assess + Fee) with no `degenbot-arbitrage` and no pyo3.
//!
//! It then repeats the encode through `ValidatedAdapter`, the ADR-100 tier-1
//! wrapper: the declaration is derived from the SAME `sol!` call type that
//! produced the calldata, so the check costs one decode and cannot disagree with
//! the encoder. An adapter that declares nothing skips this entirely.

use alloy::primitives::{address, Address, U256};
use degenbot_execution::solve_result::SolveResult;
use degenbot_execution::{ExecutionAdapter, PathInfo, ValidatedAdapter, ValuePolicy};
use degenbot_execution_sample::{simple_executor_selector, SimpleExecutorStrategy};

const EXECUTOR: Address = address!("7777777777777777777777777777777777777777");

fn main() {
    // The foreign strategy for our own `SimpleExecutor` contract.
    let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
    println!(
        "foreign strategy: executor={EXECUTOR} probes={} assess={:?} fee_market_percentile={}",
        strat.probes.len(),
        strat.assess_options.rule,
        strat.fee_policy.market_percentile
    );

    // A solved path's amounts, sealed into the typed SolveResult view.
    let opt = 1_000_000_000_000_000_000u128;
    let out = 1_210_000_000_000_000_000u128;
    let result = SolveResult {
        path_id: 42,
        hop_count: 2,
        optimal_input: U256::from(opt),
        hop_outputs: vec![U256::from(opt), U256::from(out)],
        consumed_inputs: vec![U256::from(opt), U256::from(out)],
        net_profit: U256::from(10),
        hop_descriptors: Vec::new(),
    };

    let transaction = strat
        .encode_solution(&result)
        .expect("encode the foreign transaction");
    let calldata_hex = alloy::hex::encode(transaction.data.as_ref());
    println!(
        "foreign transaction: to={} value={} data ({} bytes): 0x{}…",
        transaction.to,
        transaction.value,
        transaction.data.len(),
        &calldata_hex[..16]
    );
    assert_eq!(
        transaction.to, EXECUTOR,
        "the sample contract is the target"
    );
    assert_eq!(
        &transaction.data[..4],
        &simple_executor_selector(),
        "payload leads with the foreign execute(uint256,uint256,uint256[]) selector"
    );
    // Tier 1 (ADR-100): the SAME `sol!` call type declares the interface, so the
    // encoder and its declaration are one fact. The wrapper forwards the
    // strategy's encode and validates the transaction it produced.
    let interface = strat
        .composer
        .interface()
        .expect("the sample's declaration builds")
        .with_value_policy(ValuePolicy::Zero);
    let validated = ValidatedAdapter::new(strat, interface).expect("the declarations agree");
    let checked = validated
        .compose_view(&PathInfo::new(Vec::new()), &result)
        .expect("the declaration accepts the strategy's own encoding");
    assert_eq!(
        checked, transaction,
        "validation forwards the transaction it checked, byte for byte"
    );
    println!(
        "declared interface: selectors={:?} target={} value_policy={} — accepted",
        validated.interface().selectors(),
        checked.to,
        validated.interface().value_policy().name()
    );

    println!(
        "foreign consumer OK — one complete transaction, distinct from the default cmd_executor adapter"
    );
}
