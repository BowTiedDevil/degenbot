//! `degenbot-execution-sample` — TLQ5VH: a **standalone-Rust user-defined
//! strategy** for a FOREIGN contract.
//!
//! This crate is a sample consumer of the [`degenbot_execution`] seam
//! (ADR-025). It implements the full four-part `ExecutionStrategy` for a
//! sample **`SimpleExecutor`** contract of our own design — deliberately NOT
//! `cmd_executor`:
//!
//! - **Encode** — [`SimpleExecutorComposer`] turns a solved path into the ABI
//!   `execute(uint256,uint256,uint256[])` calldata for the sample's own
//!   contract. The payload shape (a plain ABI call with `(optimal_input,
//!   final_output, hop_outputs[])`) is **structurally distinct** from the
//!   default adapter's yul-command stream — the distinct-payload doctrine
//!   (UQ6WOG) that proves a foreign strategy is a genuinely different path,
//!   never a re-derivation of `cmd_executor`.
//! - **Probe** — [`SimpleExecutorStrategy::probes`]: declared pre/post
//!   read-calls (`WETH.balanceOf`) the engine snapshots (declared data).
//! - **Assess** — the built-in gate (sum-of-deltas + `min_net_profit`).
//! - **Fee** — the defaulted pricing half of Assess (built-in
//!   market-percentile policy).
//!
//! Guardrails observed:
//! - **No `degenbot-settlement-strategy` dependency** (this crate uses only the
//!   thin seam + its descriptor/solver input types).
//! - **No `pyo3`** — a pure-Rust consumer, per AGENTS.md consumer #1.
//! - **Encode is unconditional user code**; Probe/Assess/Fee are data/defaults
//!   the foreign searcher wires (ADR-025 D2/D5).

use alloy::primitives::{keccak256, Address, Bytes, U256};
use degenbot_execution::solve_result::SolveResult;
use degenbot_execution::{
    AssessRule, ComposeError, ComposeOptions, ComposerInputs, ExecutionResult, ExecutionStrategy,
    FeePolicy, PayloadComposer, ProbeSpec, ProbeSpecs,
};
use degenbot_executor::composers::PathInfo;

// ════════════════════════════════════════════════════════════════════════════
// Encode — the foreign `SimpleExecutor` composer (ADR-025 D2)
// ════════════════════════════════════════════════════════════════════════════

/// The `SimpleExecutor.execute` function selector:
/// `selector = keccak256("execute(uint256,uint256,uint256[])")[..4]`.
///
/// A foreign contract names its own selector. Distinct from `cmd_executor`'s
/// `execute(commands, config)` (which packs a YUL command stream) by both
/// selector and argument shape.
#[must_use]
pub fn simple_executor_selector() -> [u8; 4] {
    let digest = keccak256(b"execute(uint256,uint256,uint256[])");
    let mut sel = [0u8; 4];
    sel.copy_from_slice(&digest[..4]);
    sel
}

/// Error type for the foreign composer (wraps the seam's [`ComposeError`]).
#[derive(Debug, thiserror::Error)]
pub enum SampleError {
    /// The path's amount bundle could not be encoded for the foreign contract.
    #[error(transparent)]
    Compose(#[from] ComposeError),
}

/// The **Encode** part (ADR-025 D2) for the sample's own `SimpleExecutor`
/// contract. Holds the contract address (its own, not `cmd_executor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimpleExecutorComposer {
    /// The sample contract that receives the composed payload.
    pub executor: Address,
}

impl SimpleExecutorComposer {
    /// Build the `execute(uint256,uint256,uint256[])` calldata:
    /// `selector || abi.encode(optimal_input, final_output, hop_outputs)`.
    ///
    /// Each value is a 32-byte big-endian uint256 word; the dynamic `uint256[]`
    /// is ABI-encoded with a static offset then a length-prefixed element
    /// run. `hop_outputs` carries the per-hop outputs (`[i]` = after hop `i`),
    /// so the final element is the path's final output — the foreign
    /// contract's settlement figure.
    fn build_calldata(inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        let n: usize = inputs.hop_outputs.len();
        // static head: opt, final_out, array-offset
        let final_out = inputs
            .hop_outputs
            .last()
            .copied()
            .ok_or_else(|| ComposeError::encode("path has no hops"))?;
        let mut words: Vec<[u8; 32]> = Vec::with_capacity(4 + n);
        words.push(to_word(inputs.optimal_input));
        words.push(to_word(final_out));
        // array data offset: 3 static words (0x00/0x20/0x40) → data at 0x60
        words.push(U256::from(0x60u64).to_be_bytes());
        words.push(U256::from(n as u64).to_be_bytes());
        for out in inputs.hop_outputs {
            words.push(to_word(*out));
        }
        let mut calldata = Vec::with_capacity(4 + words.len() * 32);
        calldata.extend_from_slice(&simple_executor_selector());
        for w in &words {
            calldata.extend_from_slice(w);
        }
        Ok(Bytes::from(calldata))
    }
}

/// Encode a `u128` as a 32-byte big-endian ABI word (the `cmd_executor`
/// int128 convention — amounts are integer fixed-point, never floats).
fn to_word(value: u128) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[16..].copy_from_slice(&value.to_be_bytes());
    word
}

impl PayloadComposer for SimpleExecutorComposer {
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        // Encode is user code for the foreign contract; `path`/`inputs` are the
        // seam's Encode intake. The foreign composer ignores the hop
        // descriptors for the payload itself but reads the amounts.
        let _ = path;
        Self::build_calldata(inputs)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ExecutionStrategy — the full four-part seam (ADR-025 D2)
// ════════════════════════════════════════════════════════════════════════════

/// The full foreign `ExecutionStrategy` — wires all four parts explicitly
/// (rather than the `PayloadComposer` blanket default) to demonstrate a
/// searcher owning its Encode + declared Probe + Assess + Fee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleExecutorStrategy {
    /// The Encode part.
    pub composer: SimpleExecutorComposer,
    /// Declared pre/post read-calls the engine snapshots (Probe — declared
    /// data, not code).
    pub probes: ProbeSpecs,
    /// The built-in gate shape + min-net-profit threshold (Assess).
    pub assess_rule: AssessRule,
    /// Minimum net profit (wei) for a pass.
    pub min_net_profit: U256,
    /// The pricing policy (Fee — the defaulted half of Assess).
    pub fee_policy: FeePolicy,
}

impl Default for SimpleExecutorStrategy {
    fn default() -> Self {
        Self {
            composer: SimpleExecutorComposer {
                executor: Address::ZERO,
            },
            probes: Vec::new(),
            assess_rule: AssessRule::SumOfDeltas,
            min_net_profit: U256::ZERO,
            fee_policy: FeePolicy::default(),
        }
    }
}

impl SimpleExecutorStrategy {
    /// Construct for a sample contract with the canonical probe set — a
    /// `WETH.balanceOf(executor)` declared read (label, address, selector),
    /// exactly the `ProbeSpec` shape the engine runs/warms.
    #[must_use]
    pub fn for_executor(executor: Address) -> Self {
        Self {
            composer: SimpleExecutorComposer { executor },
            probes: vec![ProbeSpec::new(
                "WETH".to_string(),
                Some(alloy::primitives::address!(
                    "C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
                )),
                [0x70, 0xa0, 0x82, 0x31], // balanceOf(address)
            )],
            ..Self::default()
        }
    }

    /// Convenience entry point: a sealed [`SolveResult`] -> payload `bytes`
    /// for the foreign contract (the ADR-025 `compose_view` path most foreign
    /// strategies use).
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the path/amounts cannot be encoded.
    pub fn compose_solution(&self, result: &SolveResult) -> Result<Bytes, ComposeError> {
        // Reconstruct the seam's borrowed `ComposerInputs` over the view's
        // u128-narrowed amounts (mirrors the seam's own `compose_view`
        // default) and run the Encode part directly.
        let optimal_input = degenbot_execution::payload::narrow_u256_to_u128(result.optimal_input)
            .ok_or_else(|| ComposeError::encode("optimal_input does not fit int128"))?;
        let hop_outputs: Vec<u128> = result
            .hop_outputs
            .iter()
            .map(|v| {
                degenbot_execution::payload::narrow_u256_to_u128(*v)
                    .ok_or_else(|| ComposeError::encode("hop_output does not fit int128"))
            })
            .collect::<Result<_, _>>()?;
        let consumed_inputs: Vec<u128> = result
            .consumed_inputs
            .iter()
            .map(|v| {
                degenbot_execution::payload::narrow_u256_to_u128(*v)
                    .ok_or_else(|| ComposeError::encode("consumed_input does not fit int128"))
            })
            .collect::<Result<_, _>>()?;
        let inputs = ComposerInputs {
            optimal_input,
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed_inputs,
            opts: ComposeOptions,
        };
        // Build a PathInfo-less call — the foreign composer doesn't read hops.
        let empty = PathInfo::new(Vec::new());
        PayloadComposer::compose(&self.composer, &empty, &inputs)
    }

    /// Run the gate over probe deltas with the configured Ass/Cee/Fee — the
    /// seam's engine-free built-in assessor (ADR-019): `gross = Σ deltas`,
    /// `net = gross − gas×(base_fee_next + priority_fee)`,
    /// `passed = net ≥ min_net_profit`.
    #[must_use]
    pub fn assess(&self, deltas: &[i128], gas_used: u64, base_fee_next: u128) -> ExecutionResult {
        ExecutionStrategy::assess(self, deltas, gas_used, base_fee_next)
    }
}

impl ExecutionStrategy for SimpleExecutorStrategy {
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        PayloadComposer::compose(&self.composer, path, inputs)
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess_rule(&self) -> Option<AssessRule> {
        Some(self.assess_rule)
    }

    fn min_net_profit(&self) -> U256 {
        self.min_net_profit
    }

    fn fee_policy(&self) -> FeePolicy {
        self.fee_policy
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tests — distinct-payload doctrine + four-part wiring (UQ6WOG seed corpus)
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_execution::solve_result::{HopDescriptor, HopFamily};

    const EXECUTOR: Address =
        alloy::primitives::address!("7777777777777777777777777777777777777777");

    fn sample_inputs() -> (Vec<u128>, Vec<u128>) {
        (
            vec![1_000_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
            vec![1_000_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
        )
    }

    /// The foreign Encode output is a plain ABI `execute(...)` call — its
    /// 4-byte selector is the `execute(uint256,uint256,uint256[])` one, which
    /// structurally differs from `cmd_executor`'s `execute(commands, config)`
    /// selector and YUL-command argument stream. This is the distinct-path
    /// doctrine the foreign corpus (UQ6WOG) pins.
    #[test]
    fn foreign_payload_is_distinct_from_cmd_executor() {
        let composer = SimpleExecutorComposer { executor: EXECUTOR };
        let (hop_outputs, consumed) = sample_inputs();
        let inputs = ComposerInputs {
            optimal_input: hop_outputs[0],
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts: ComposeOptions,
        };
        let path = PathInfo::new(Vec::new());
        let bytes = composer.compose(&path, &inputs).unwrap();
        // Starts with the foreign execute(uint256,uint256,uint256[]) selector.
        assert_eq!(&bytes[..4], &simple_executor_selector());
        // Total length = selector(4) + 3 static words + array length + 2 elems.
        assert_eq!(bytes.len(), 4 + 4 * 32 + 2 * 32);
        // Argument-shape distinctness: the ABI array-offset word sits right
        // after `selector(4) + opt(32) + final_out(32)` = calldata[68..100],
        // and reads 0x60 — an ABI dynamic-array head, NOT a YUL command stream
        // (cmd_executor packs a 1-byte command list first).
        let offset = U256::from_be_slice(&bytes[68..100]);
        assert_eq!(
            offset,
            U256::from(0x60u64),
            "ABI array offset for the foreign call"
        );
    }

    /// **Own expected-bytes corpus** (UQ6WOG): the foreign `SimpleExecutor`
    /// payload for the canonical 2-hop sample path is a RECORDED constant —
    /// its own golden, distinct from the default adapter's. The Python mirror
    /// (OULU5O) must reproduce these exact bytes (the cross-layer oracle), so
    /// this constant is the one source of truth both layers assert against.
    #[test]
    fn foreign_payload_matches_recorded_corpus() {
        const CORPUS: &str = "ead35cae\
            0000000000000000000000000000000000000000000000000de0b6b3a7640000\
            00000000000000000000000000000000000000000000000010cac896d2390000\
            0000000000000000000000000000000000000000000000000000000000000060\
            0000000000000000000000000000000000000000000000000000000000000002\
            0000000000000000000000000000000000000000000000000de0b6b3a7640000\
            00000000000000000000000000000000000000000000000010cac896d2390000";
        let composer = SimpleExecutorComposer {
            executor: Address::ZERO,
        };
        let (hop_outputs, consumed) = sample_inputs();
        let inputs = ComposerInputs {
            optimal_input: hop_outputs[0],
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts: ComposeOptions,
        };
        let bytes = composer
            .compose(&PathInfo::new(Vec::new()), &inputs)
            .unwrap();
        let expected = Bytes::from(alloy::hex::decode(format!("0x{CORPUS}")).unwrap());
        assert_eq!(
            bytes, expected,
            "foreign payload must match its recorded corpus"
        );
    }

    /// `cmd_executor.execute` selector — known to differ from the foreign one
    /// (the default adapter's `execute(commands, config)` ABI signature).
    #[test]
    fn foreign_selector_differs_from_default_adapter_selector() {
        let cmd_executor_selector: [u8; 4] = {
            let digest = keccak256(b"execute(bytes,uint256)");
            let mut sel = [0u8; 4];
            sel.copy_from_slice(&digest[..4]);
            sel
        };
        assert_ne!(
            simple_executor_selector(),
            cmd_executor_selector,
            "the foreign selector must differ from cmd_executor's"
        );
    }

    /// The full four-part seam wires Probe (declared reads), Assess (gate
    /// rule + min), Fee (pricing): the strategy reports them and the built-in
    /// assessor resolves gross → net → pass.
    #[test]
    fn four_part_wiring_and_builtin_assess() {
        let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
        // Probe — declared data (WETH balanceOf selector).
        assert_eq!(strat.probes.len(), 1);
        assert_eq!(strat.probes[0].label, "WETH");
        assert_eq!(strat.probes[0].selector, [0x70, 0xa0, 0x82, 0x31]);
        // Assess — built-in sum-of-deltas + zero min threshold.
        assert_eq!(strat.assess_rule, AssessRule::SumOfDeltas);
        // Fee — built-in market-percentile default.
        assert!(strat.fee_policy.market_percentile);

        // Built-in assess: gross = Σ deltas, net = gross − gas×fee.
        // gas_cost = 100_000 × (1e9 + 1e9) = 2e14; gross 4e15 clears it.
        let deltas = vec![4_000_000_000_000_000i128, -1_000_000_000_000_000i128];
        let result = strat.assess(&deltas, 100_000, 1_000_000_000);
        assert_eq!(result.gross_profit, U256::from(3_000_000_000_000_000u64));
        assert!(result.passed);
        // A genuine loss (gross < gas cost) is a FAIL, not a zero pass.
        let loss = strat.assess(&[-10_000i128], 100_000, 1_000_000_000);
        assert!(!loss.passed);
    }

    /// The sealed-view entry (`compose_solution`) projects a real
    /// `SolveResult` (via `from_solve_path`) and produces the same foreign
    /// calldata — the `(path, SolveResult) -> bytes` user path.
    #[test]
    fn compose_solution_projects_solve_result_view() {
        use degenbot_solvers::mixed::SolvePathResult;
        let (hop_outputs, consumed) = sample_inputs();
        let result = SolvePathResult {
            optimal_input: U256::from(hop_outputs[0]),
            profit: U256::from(10u64),
            hop_outputs: hop_outputs.iter().map(|v| U256::from(*v)).collect(),
            consumed_inputs: consumed.iter().map(|v| U256::from(*v)).collect(),
            ..Default::default()
        };
        let view = SolveResult {
            path_id: 9,
            hop_count: 2,
            optimal_input: result.optimal_input,
            hop_outputs: result.hop_outputs.clone(),
            consumed_inputs: result.consumed_inputs.clone(),
            net_profit: result.profit,
            hop_descriptors: vec![HopDescriptor {
                family: HopFamily::V2,
                pool_address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::ZERO,
                zfo: true,
                v4_pool_id: None,
            }],
        };
        let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
        let bytes = strat.compose_solution(&view).unwrap();
        assert_eq!(&bytes[..4], &simple_executor_selector());
        assert_eq!(bytes.len(), 4 + 4 * 32 + 2 * 32);
    }
}
