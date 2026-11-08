#![expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
// Solidity/EVM identifiers (`PoolManager`, `balanceOf`, `msg.value`) and the
// receipt struct's field names are ubiquitous in this file's prose.
#![expect(clippy::doc_markdown)]

//! Receipt-vs-seven-call parity for the built-in executor's assessment
//! (ADR-098 D7).
//!
//! The built-in `cmd_executor` transaction returns
//! `ExecutionReceipt { gross_profit, bribe_paid }` — two ABI words. Its Assess
//! reads them from the simulated call's return data and prices the RETAINED
//! profit (`gross_profit − bribe_paid`, before gas). The legacy projection
//! beside it is the pre-cutover settlement bundle: three declared pre/post
//! balance reads (WETH `balanceOf(executor)`, Multicall3 `getEthBalance`, and
//! PoolManager ERC6909 `balanceOf(executor, weth_id)`) summed into one gross
//! figure measured AFTER the whole transaction — so it already had the bribe
//! deducted, because the bribe left the executor's physical balances.
//!
//! The two must agree on every case, and the parity is the reason the receipt
//! projection is `gross − bribe` rather than `gross`: the public
//! `ExecutionResult::gross_profit` / `SimResult::gross_profit` means retained
//! after bribe and before gas, exactly as the probe-era figure did. Gas, base
//! fee, and the market-aware priority fee then apply ONCE above the seam.
//!
//! # The mode-2 domain difference is intentional, not double-counted
//!
//! `profit_mode` selects the receipt's accounting domain: 1 measures physical
//! WETH + native ETH, 2 measures PoolManager ERC6909 WETH claims. The legacy
//! probes summed all three legs regardless of mode, so in mode 2 the native
//! bribe (paid from physical ETH, withdrawing WETH when the ETH balance is
//! short) showed up on the physical legs while the gross showed up on the
//! ERC6909 leg. The receipt reports the ERC6909 increase and the bribe
//! separately, and `gross − bribe` reproduces the same total. The subtraction
//! therefore crosses domains on purpose: the bribe is real native value that
//! leaves the executor, and ignoring it (or subtracting it twice) would report
//! a profit the operator did not retain.

use alloy::primitives::{address, Address, Bytes, U256};
use degenbot_arbitrage::{
    compute_priority_fee, BlockPriorityFees, CmdExecutorBinding, MIN_PROFIT_NET,
};
use degenbot_execution::{
    AssessError, AssessOptions, AssessRule, ExecutionAdapter, ExecutionObservation,
    ExecutionResult, ExecutionStatus, FeePolicy, ProbeReading, ProbeSpec, TransactionRequest,
};
use degenbot_executor::composers::EncodeOptions;

const OWNER: Address = address!("0x9c56a29c7231974c269e24f9fb3c29203039089e");
const EXECUTOR: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");
const MULTICALL3: Address = address!("0xc411372f0b8ae58585e33b78aea9e0596da9a6f1");

const GAS_USED: u64 = 180_000;
const BASE_FEE_NEXT: u128 = 1_000_000_000;
const SOLVE_BLOCK: u64 = 100;
const CURRENT_BLOCK: u64 = 100;

fn binding() -> CmdExecutorBinding {
    CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
}

/// The built-in adapter's own pricing floor: base fee only, no priority fee.
/// The operator's market-aware priority fee is applied once above the seam by
/// the settlement strategy.
const fn pricing_floor() -> FeePolicy {
    FeePolicy {
        market_percentile: false,
        override_priority_fee: 0,
    }
}

fn block_priority_fees() -> BlockPriorityFees {
    BlockPriorityFees {
        block: CURRENT_BLOCK,
        p10: U256::from(500_000_000u64),
        p50: U256::from(2_000_000_000u64),
    }
}

fn observation(status: ExecutionStatus) -> ExecutionObservation {
    ExecutionObservation::new(
        TransactionRequest::new(EXECUTOR, Bytes::new(), U256::ZERO),
        status,
        GAS_USED,
        BASE_FEE_NEXT,
    )
}

// ── the seven-call legacy projection (test-only oracle) ──────────────────

/// The contract's `profit_mode`: the balance domain `gross_profit` measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Domain {
    /// Mode 1 — physical WETH + native ETH.
    Physical,
    /// Mode 2 — PoolManager ERC6909 WETH claims.
    Claims,
}

/// One settlement opportunity as the executor saw it: the pre-existing
/// balances, the receipt's two words, and the accounting domain.
#[derive(Clone, Copy, Debug)]
struct Case {
    name: &'static str,
    domain: Domain,
    weth_before: U256,
    eth_before: U256,
    erc6909_before: U256,
    /// The receipt's first word: the selected-domain increase after the
    /// command stream and BEFORE the bribe.
    gross: U256,
    /// The receipt's second word: the native ETH the executor actually paid.
    bribe: U256,
}

/// The physical legs after the bribe: ETH first, WETH withdrawn for any
/// shortfall — the contract's own order (`raw_call` from `self.balance`, then
/// `IWETH.withdraw` for the remainder).
fn pay_bribe(weth: U256, eth: U256, bribe: U256) -> (U256, U256) {
    let from_eth = bribe.min(eth);
    let shortfall = bribe - from_eth;
    (
        weth.checked_sub(shortfall)
            .expect("the case funds the bribe"),
        eth - from_eth,
    )
}

impl Case {
    /// The three post-transaction legs the retired seven-call bundle measured.
    fn legacy_after(&self) -> (U256, U256, U256) {
        match self.domain {
            // The gross lands as physical WETH, then the bribe leaves the pair.
            Domain::Physical => {
                let (weth, eth) =
                    pay_bribe(self.weth_before + self.gross, self.eth_before, self.bribe);
                (weth, eth, self.erc6909_before)
            }
            // The gross stays in ERC6909 claims; only the bribe touches the
            // physical legs.
            Domain::Claims => {
                let (weth, eth) = pay_bribe(self.weth_before, self.eth_before, self.bribe);
                (weth, eth, self.erc6909_before + self.gross)
            }
        }
    }

    /// The legacy seven-call observation: three declared pre/post reads over
    /// the settlement's balance vector, in the bundle's fixed order.
    fn legacy_observation(&self) -> ExecutionObservation {
        let (weth_after, eth_after, erc6909_after) = self.legacy_after();
        let reads = vec![
            ProbeReading::new(
                ProbeSpec::call("WETH", WETH, Bytes::from_static(&[0x70, 0xa0, 0x82, 0x31])),
                Some(self.weth_before),
                Some(weth_after),
            ),
            ProbeReading::new(
                ProbeSpec::call(
                    "ETH",
                    MULTICALL3,
                    Bytes::from_static(&[0x4d, 0x23, 0x01, 0xcc]),
                ),
                Some(self.eth_before),
                Some(eth_after),
            ),
            ProbeReading::new(
                ProbeSpec::call(
                    "ERC6909_WETH",
                    POOL_MANAGER,
                    Bytes::from_static(&[0x00, 0xfd, 0xd5, 0x8e]),
                ),
                Some(self.erc6909_before),
                Some(erc6909_after),
            ),
        ];
        observation(ExecutionStatus::Success).with_probe_reads(reads)
    }

    /// The receipt evidence `execute(bytes,uint256)` actually returns.
    fn receipt_data(&self) -> Bytes {
        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&self.gross.to_be_bytes::<32>());
        data.extend_from_slice(&self.bribe.to_be_bytes::<32>());
        Bytes::from(data)
    }

    fn receipt_observation(&self) -> ExecutionObservation {
        observation(ExecutionStatus::Success).with_return_data(self.receipt_data())
    }
}

/// The retired built-in assessment, expressed with the seam's own
/// sum-of-deltas rule over the seven-call readings: `gross = Σ (after −
/// before)`, floored at zero, priced by the adapter's base-fee-only floor.
fn legacy_assess(case: Case) -> ExecutionResult {
    let observation = case.legacy_observation();
    AssessOptions {
        rule: AssessRule::SumOfDeltas,
        min_net_profit: U256::ZERO,
    }
    .assess(&observation, &pricing_floor())
    .expect("the seven-call bundle observed every leg")
}

/// The strategy's market-aware re-pricing, applied once above the seam — the
/// same primitives `degenbot_arbitrage::simulator::price_gross` composes.
fn market_priced(result: &ExecutionResult, observation: &ExecutionObservation) -> ExecutionResult {
    let priority_fee = compute_priority_fee(
        result.gross_profit,
        observation.gas_used,
        observation.base_fee_next,
        SOLVE_BLOCK,
        CURRENT_BLOCK,
        Some(&block_priority_fees()),
    );
    ExecutionResult::price(
        result.gross_profit,
        result.deltas.clone(),
        observation,
        U256::from(MIN_PROFIT_NET),
        &FeePolicy {
            market_percentile: false,
            override_priority_fee: priority_fee,
        },
    )
}

fn u(x: u64) -> U256 {
    U256::from(x)
}

/// Every profit-mode/balance/bribe shape the settlement bundle distinguishes.
fn cases() -> Vec<Case> {
    let eth = 1_000_000_000_000_000_000u64;
    vec![
        // Mode 1: no bribe requested, freshly funded executor.
        Case {
            name: "mode1_basic_profit",
            domain: Domain::Physical,
            weth_before: U256::ZERO,
            eth_before: U256::ZERO,
            erc6909_before: U256::ZERO,
            gross: u(eth),
            bribe: U256::ZERO,
        },
        // Mode 1: pre-existing balances on every leg, small gross.
        Case {
            name: "mode1_preexisting_balances",
            domain: Domain::Physical,
            weth_before: u(3 * eth),
            eth_before: u(eth / 2),
            erc6909_before: u(7 * eth),
            gross: u(eth / 5),
            bribe: U256::ZERO,
        },
        // Mode 1: the requested bribe (25% of gross) is fully paid from ETH.
        Case {
            name: "mode1_requested_bribe",
            domain: Domain::Physical,
            weth_before: U256::ZERO,
            eth_before: u(eth),
            erc6909_before: U256::ZERO,
            gross: u(eth),
            bribe: u(eth / 4),
        },
        // Mode 1: capped bribe — the requested share exceeds the available
        // physical balance, so the contract pays what it holds.
        Case {
            name: "mode1_capped_bribe",
            domain: Domain::Physical,
            weth_before: U256::ZERO,
            eth_before: u(eth / 100),
            erc6909_before: U256::ZERO,
            gross: u(eth),
            bribe: u(eth / 100),
        },
        // Mode 1: zero profit and no bribe — both projections read zero.
        Case {
            name: "mode1_zero_profit",
            domain: Domain::Physical,
            weth_before: u(eth),
            eth_before: u(eth),
            erc6909_before: U256::ZERO,
            gross: U256::ZERO,
            bribe: U256::ZERO,
        },
        // Mode 2: claims only, physical legs untouched.
        Case {
            name: "mode2_claims_profit",
            domain: Domain::Claims,
            weth_before: U256::ZERO,
            eth_before: U256::ZERO,
            erc6909_before: u(2 * eth),
            gross: u(eth / 2),
            bribe: U256::ZERO,
        },
        // Mode 2: the bribe is native, outside the receipt's ERC6909 domain —
        // the legacy total saw it on the physical leg.
        Case {
            name: "mode2_native_bribe",
            domain: Domain::Claims,
            weth_before: U256::ZERO,
            eth_before: u(eth),
            erc6909_before: U256::ZERO,
            gross: u(eth / 2),
            bribe: u(eth / 10),
        },
        // Mode 2: the bribe withdraws WETH for the shortfall (no ETH at all).
        Case {
            name: "mode2_weth_withdrawn_bribe",
            domain: Domain::Claims,
            weth_before: u(eth),
            eth_before: U256::ZERO,
            erc6909_before: U256::ZERO,
            gross: u(eth / 2),
            bribe: u(eth / 5),
        },
    ]
}

/// The parity contract: the receipt's retained figure reproduces the retired
/// seven-call gross on every case, and the final gas/fee net is unchanged
/// because both grosses are the same.
#[test]
fn receipt_assessment_matches_the_legacy_seven_call_projection() {
    let adapter = binding().adapter();

    for case in cases() {
        let legacy = legacy_assess(case);
        let legacy_observation = case.legacy_observation();
        let receipt_observation = case.receipt_observation();
        let receipt = adapter
            .assess(&receipt_observation)
            .unwrap_or_else(|error| panic!("{}: receipt assessment failed: {error}", case.name));

        // The intentional semantics, stated independently of either
        // projection: the reported gross is the retained profit before gas.
        assert_eq!(
            legacy.gross_profit,
            case.gross - case.bribe,
            "{}: the seven-call bundle measured gross − bribe",
            case.name
        );
        assert_eq!(
            receipt.gross_profit, legacy.gross_profit,
            "{}: the receipt's retained figure matches the seven-call gross",
            case.name
        );
        assert_eq!(
            receipt.gross_profit,
            case.gross - case.bribe,
            "{}: retained = gross_profit − bribe_paid",
            case.name
        );

        // Evidence shape: the receipt assessment consumes no probe deltas; the
        // legacy one is exactly three signed leg deltas.
        assert!(
            receipt.deltas.is_empty(),
            "{}: the receipt assessment reports no per-probe deltas",
            case.name
        );
        assert_eq!(legacy.deltas.len(), 3, "{}: three legs", case.name);
        assert_eq!(
            legacy.deltas.iter().sum::<i128>(),
            i128::try_from(case.gross - case.bribe).unwrap(),
            "{}: the leg deltas sum to retained",
            case.name
        );

        // Gas is measured, not asserted: both projections assess one run.
        assert_eq!(receipt.gas_used, legacy.gas_used);
        assert_eq!(receipt.base_fee_next, legacy.base_fee_next);

        // The adapter's own pricing floor: base fee only, priority 0, applied
        // once. A gross below the gas cost is a fail with zero net, never a
        // zero-profit pass.
        let gas_at_base = U256::from(GAS_USED) * U256::from(BASE_FEE_NEXT);
        assert_eq!(
            receipt.priority_fee, 0,
            "{}: no adapter priority fee",
            case.name
        );
        match case
            .gross
            .checked_sub(case.bribe)
            .unwrap()
            .checked_sub(gas_at_base)
        {
            Some(net) => assert_eq!(receipt.net_profit, net, "{}", case.name),
            None => assert_eq!(receipt.net_profit, U256::ZERO, "{}: gas loss", case.name),
        }

        // The final, strategy-priced net: identical, because the retained
        // figure is identical and the priority fee is sized from it.
        let legacy_market = market_priced(&legacy, &legacy_observation);
        let receipt_market = market_priced(&receipt, &receipt_observation);
        assert_eq!(
            receipt_market.gross_profit, legacy_market.gross_profit,
            "{}: retained before gas",
            case.name
        );
        assert_eq!(
            receipt_market.net_profit, legacy_market.net_profit,
            "{}: final net after gas and the market priority fee",
            case.name
        );
        assert_eq!(
            receipt_market.passed, legacy_market.passed,
            "{}: same verdict",
            case.name
        );
        assert_eq!(
            receipt_market.priority_fee, legacy_market.priority_fee,
            "{}: one priority-fee sizing, applied once",
            case.name
        );
        let gas_at_market = U256::from(GAS_USED)
            * (U256::from(BASE_FEE_NEXT) + U256::from(receipt_market.priority_fee));
        if let Some(net) = receipt_market.gross_profit.checked_sub(gas_at_market) {
            assert_eq!(
                receipt_market.net_profit, net,
                "{}: gas and priority applied exactly once",
                case.name
            );
        }
    }
}

/// `execute(bytes,uint256)` returns exactly two ABI words. Anything else — no
/// captured return data, a short payload, a trailing word — is absent or
/// undecodable evidence, never a zero-profit success.
#[test]
fn malformed_receipts_are_assess_errors_never_zero_profit() {
    let adapter = binding().adapter();
    let gross = u(1_000_000_000_000_000_000);

    let cases: Vec<(&str, ExecutionObservation)> = vec![
        (
            "no captured return data",
            observation(ExecutionStatus::Success),
        ),
        (
            "empty return data",
            observation(ExecutionStatus::Success).with_return_data(Bytes::new()),
        ),
        (
            "one word only (no bribe_paid)",
            observation(ExecutionStatus::Success)
                .with_return_data(Bytes::from(gross.to_be_bytes::<32>().to_vec())),
        ),
        (
            "three words (a trailing word is not a receipt)",
            observation(ExecutionStatus::Success).with_return_data(Bytes::from(
                [
                    gross.to_be_bytes::<32>().to_vec(),
                    U256::ZERO.to_be_bytes::<32>().to_vec(),
                    gross.to_be_bytes::<32>().to_vec(),
                ]
                .concat(),
            )),
        ),
        (
            "short payload",
            observation(ExecutionStatus::Success)
                .with_return_data(Bytes::from(gross.to_be_bytes::<32>()[..31].to_vec())),
        ),
    ];

    for (name, observation) in cases {
        let error = adapter
            .assess(&observation)
            .expect_err("an unmeasured receipt is not a verdict");
        assert!(
            matches!(error, AssessError::Evidence(_) | AssessError::Decode(_)),
            "{name}: expected an evidence/decode refusal, got {error:?}"
        );
    }
}

/// The receipt's own consistency: a bribe above the measured gross is
/// impossible evidence. The contract requests `gross_profit * bribe_bips /
/// 10000` with `bribe_bips <= 10000`, and the available-balance cap can only
/// reduce the amount it actually sends, so the requested share bounds the paid
/// bribe no matter how much the executor already holds. A receipt that breaks
/// that bound is refused rather than saturated into a false arb.
#[test]
fn an_inconsistent_receipt_is_refused_not_saturated() {
    let adapter = binding().adapter();
    let case = Case {
        name: "bribe_above_gross",
        domain: Domain::Physical,
        weth_before: U256::ZERO,
        eth_before: u(1_000_000_000_000_000_000),
        erc6909_before: U256::ZERO,
        gross: u(1_000),
        bribe: u(1_001),
    };
    let observation = case
        .receipt_observation()
        .with_return_data(case.receipt_data());
    let error = adapter
        .assess(&observation)
        .expect_err("bribe_paid above gross_profit is inconsistent evidence");
    assert!(
        matches!(error, AssessError::Evidence(_)),
        "expected an evidence refusal, got {error:?}"
    );
}

/// A reverted observation keeps the seam's terminal zero/decline whatever the
/// revert payload holds: simulation return data is evidence only for a
/// completed call, so a revert cannot be decoded into a profit.
#[test]
fn a_reverted_observation_keeps_the_terminal_zero_decline() {
    let adapter = binding().adapter();
    let case = cases()
        .into_iter()
        .find(|case| case.name == "mode1_basic_profit")
        .expect("the fixture names a profitable case");
    let observation = case
        .receipt_observation()
        .with_return_data(case.receipt_data());
    let reverted = ExecutionObservation {
        status: ExecutionStatus::Revert,
        ..observation
    };
    let result = adapter
        .assess(&reverted)
        .expect("a revert is a decline, not an assessment failure");
    assert_eq!(result.gross_profit, U256::ZERO);
    assert_eq!(result.net_profit, U256::ZERO);
    assert!(!result.passed);
    assert_eq!(result.gas_used, GAS_USED, "the measured gas is preserved");
}

/// The built-in adjudicates its own receipt, not a balance snapshot: no probe
/// reads are declared for its transaction.
#[test]
fn the_builtin_sim_adapter_declares_no_probes() {
    let adapter = binding().adapter();
    assert!(
        adapter.probe_spec().is_empty(),
        "the built-in assessment reads the execute receipt, not pre/post balances: {:?}",
        adapter
            .probe_spec()
            .iter()
            .map(ProbeSpec::label)
            .collect::<Vec<_>>()
    );
}
