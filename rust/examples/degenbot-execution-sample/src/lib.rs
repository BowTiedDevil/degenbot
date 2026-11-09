//! `degenbot-execution-sample` — TLQ5VH: a **standalone-Rust user-defined
//! strategy** for a FOREIGN contract.
//!
//! This crate is a sample consumer of the [`degenbot_execution`] seam
//! (ADR-025). It implements the full four-part `ExecutionAdapter` for a
//! sample **`SimpleExecutor`** contract of our own design — deliberately NOT
//! `cmd_executor`:
//!
//! - **Encode** — [`SimpleExecutorComposer`] turns a solved path into the ABI
//!   `execute(uint256,uint256,uint256[])` calldata for the sample's own
//!   contract, and [`SimpleExecutorStrategy::encode`] returns the complete
//!   transaction around it: `to` = the sample contract, `value` = the
//!   strategy's configured native value. The payload shape (a plain ABI call
//!   with `(optimal_input, final_output, hop_outputs[])`) is **structurally
//!   distinct** from the default adapter's yul-command stream — the
//!   distinct-payload doctrine that proves a foreign strategy is a genuinely
//!   different path, never a re-derivation of `cmd_executor`.
//!
//!   The calldata is **typed**: [`SimpleExecutor`] is a `sol!` declaration, and
//!   `executeCall::abi_encode()` emits the canonical bytes no hand-packed word
//!   layout has to imitate. The same call type also declares the adapter's
//!   interface ([`SimpleExecutorComposer::interface`], ADR-100 D3), so an
//!   encoder and its declaration cannot drift — and the sample ships no ABI
//!   JSON at all.
//! - **Validate (optional)** — wrapping the strategy in
//!   [`degenbot_execution::ValidatedAdapter`] checks every encode against that
//!   declaration (ADR-100 D4). The sample's
//!   `examples/foreign_consumer.rs` shows the wrapped call site; the unwrapped
//!   strategy is tier 0 and pays nothing for the facility.
//! - **Probe** — [`SimpleExecutorStrategy::probes`]: declared pre/post
//!   read-calls (`WETH.balanceOf`) the engine snapshots (declared data).
//! - **Assess** — the built-in gate over the engine's typed
//!   [`ExecutionObservation`] (sum-of-deltas + `min_net_profit`), or the
//!   strategy's own override.
//! - **Fee** — the defaulted pricing half of Assess (built-in
//!   market-percentile policy).
//!
//! Guardrails observed:
//! - **No `degenbot-arbitrage` dependency** (this crate uses only the
//!   thin seam + its descriptor/solver input types).
//! - **No `degenbot-executor` dependency** — the hop descriptors are the
//!   seam's own (`degenbot_execution::{PathInfo, HopInfo, ...}`, ADR-098 D1),
//!   pinned by `architecture_gates::foreign_execution_sample_carries_no_concrete_executor_edge`.
//! - **No `pyo3`** — a pure-Rust consumer, per AGENTS.md consumer #1.
//! - **Encode is unconditional user code**; Probe/Assess/Fee are data/defaults
//!   the foreign searcher wires (ADR-025 D2/D5).

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolCall;
use degenbot_execution::solve_result::SolveResult;
use degenbot_execution::{
    AssessOptions, ComposeError, ComposeOptions, ComposerInputs, ContractInterface,
    DeclarationError, ExecutionAdapter, ExecutionObservation, ExecutionResult, FeePolicy, PathInfo,
    PayloadComposer, ProbeSpec, ProbeSpecs, TransactionRequest,
};

alloy::sol! {
    /// The sample's own execution contract, declared once. The generated
    /// `executeCall` carries the ABI's argument types, `SIGNATURE`, and
    /// `SELECTOR` — the encoder and the declared interface both derive from this
    /// one type (ADR-100 D3), so neither can disagree with the other.
    interface SimpleExecutor {
        function execute(uint256 optimal_input, uint256 final_output, uint256[] hop_outputs)
            external
            returns (bool);
    }
}

alloy::sol! {
    /// The read the sample's declared probe performs — the same typed-encoding
    /// rule, so its calldata is ABI-encoded rather than hand-packed.
    interface IWeth {
        function balanceOf(address owner) external view returns (uint256);
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Encode — the foreign `SimpleExecutor` composer (ADR-025 D2)
// ════════════════════════════════════════════════════════════════════════════

/// The `SimpleExecutor.execute` function selector, as Alloy computes it for the
/// `sol!` declaration: `keccak256("execute(uint256,uint256,uint256[])")[..4]`.
///
/// A foreign contract names its own selector. Distinct from `cmd_executor`'s
/// `execute(commands, config)` (which packs a YUL command stream) by both
/// selector and argument shape. Nothing in this crate hashes a signature string
/// itself: the selector is the declaration's own constant.
#[must_use]
pub fn simple_executor_selector() -> [u8; 4] {
    SimpleExecutor::executeCall::SELECTOR
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
    /// The bytes are the generated call struct's own `abi_encode()`, so the
    /// canonical ABI layout — the static head, the dynamic array's offset, its
    /// length prefix, and the element run — is Alloy's to emit, not this
    /// crate's to hand-pack. `hop_outputs` carries the per-hop outputs (`[i]` =
    /// after hop `i`), so the final element is the path's final output — the
    /// foreign contract's settlement figure.
    fn build_calldata(inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        let final_output = inputs
            .hop_outputs
            .last()
            .copied()
            .ok_or_else(|| ComposeError::encode("path has no hops"))?;
        let call = SimpleExecutor::executeCall {
            optimal_input: U256::from(inputs.optimal_input),
            final_output: U256::from(final_output),
            hop_outputs: inputs.hop_outputs.iter().copied().map(U256::from).collect(),
        };
        Ok(Bytes::from(call.abi_encode()))
    }

    /// The adapter's declared contract interface, derived from the SAME `sol!`
    /// call type that produces the calldata (ADR-100 D3).
    ///
    /// No ABI JSON is shipped: the signature, the selector, and the argument
    /// types are the declaration's own facts. The target is this composer's
    /// contract, so a transaction aimed anywhere else is a hard error; the
    /// sample declares no native-value policy because its funding model is the
    /// operator's choice — a flash-funded deployment adds
    /// [`degenbot_execution::ValuePolicy::Zero`].
    ///
    /// # Errors
    ///
    /// [`DeclarationError`] when the declaration cannot be built (the sample's
    /// own signature always parses, so this is the type's shape, not a runtime
    /// case).
    pub fn interface(&self) -> Result<ContractInterface, DeclarationError> {
        ContractInterface::from_signatures([SimpleExecutor::executeCall::SIGNATURE])?
            .with_target(self.executor)
    }
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
// ExecutionAdapter — the full four-part seam (ADR-025 D2)
// ════════════════════════════════════════════════════════════════════════════

/// The full foreign `ExecutionAdapter` — wires all four parts explicitly to
/// demonstrate a searcher owning its Encode + declared Probe + Assess + Fee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleExecutorStrategy {
    /// The Encode part.
    pub composer: SimpleExecutorComposer,
    /// Declared pre/post read-calls the engine snapshots (Probe — declared
    /// data, not code).
    pub probes: ProbeSpecs,
    /// The built-in assessment shape + min-net-profit threshold (Assess).
    pub assess_options: AssessOptions,
    /// The pricing policy (Fee — the defaulted half of Assess).
    pub fee_policy: FeePolicy,
    /// The native value (wei) the sample contract is called with. The sample
    /// executor is flash-funded, so this defaults to zero, but the value rides
    /// the transaction for a contract whose funding model needs it.
    pub value: U256,
}

impl Default for SimpleExecutorStrategy {
    fn default() -> Self {
        Self {
            composer: SimpleExecutorComposer {
                executor: Address::ZERO,
            },
            probes: Vec::new(),
            assess_options: AssessOptions::default(),
            fee_policy: FeePolicy::default(),
            value: U256::ZERO,
        }
    }
}

/// The WETH contract the sample's declared read observes.
const WETH: Address = alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

/// `WETH.balanceOf(executor)` as COMPLETE calldata: the selector plus the ABI
/// argument word, so the declared read observes the sample executor's own
/// balance rather than an argument-less call that could not be one.
#[must_use]
pub fn weth_balance_of_calldata(executor: Address) -> Bytes {
    Bytes::from(IWeth::balanceOfCall { owner: executor }.abi_encode())
}

impl SimpleExecutorStrategy {
    /// Construct for a sample contract with the canonical probe set — a
    /// `WETH.balanceOf(executor)` declared read carrying its complete calldata,
    /// exactly the `ProbeSpec` the engine executes.
    #[must_use]
    pub fn for_executor(executor: Address) -> Self {
        Self {
            composer: SimpleExecutorComposer { executor },
            probes: vec![ProbeSpec::call(
                "WETH",
                WETH,
                weth_balance_of_calldata(executor),
            )],
            ..Self::default()
        }
    }

    /// Convenience entry point: a sealed [`SolveResult`] -> ONE complete
    /// transaction for the foreign contract (the ADR-025 `compose_view` path
    /// most foreign strategies use), carrying the strategy's target + value.
    ///
    /// # Errors
    ///
    /// Returns [`ComposeError`] when the path/amounts cannot be encoded.
    pub fn encode_solution(
        &self,
        result: &SolveResult,
    ) -> Result<TransactionRequest, ComposeError> {
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
        let data = PayloadComposer::compose(&self.composer, &empty, &inputs)?;
        Ok(TransactionRequest::new(
            self.composer.executor,
            data,
            self.value,
        ))
    }

    /// Assess the engine's typed observation with the configured Assess/Fee —
    /// the seam's engine-free default gate (ADR-019): `gross = Σ deltas`,
    /// `net = gross − gas×(base_fee_next + priority_fee)`,
    /// `passed = net ≥ min_net_profit`.
    ///
    /// # Errors
    ///
    /// Returns [`degenbot_execution::AssessError`] when the observation lacks
    /// the evidence this assessment needs.
    pub fn assess(
        &self,
        observation: &ExecutionObservation,
    ) -> Result<ExecutionResult, degenbot_execution::AssessError> {
        ExecutionAdapter::assess(self, observation)
    }
}

impl ExecutionAdapter for SimpleExecutorStrategy {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let data = PayloadComposer::compose(&self.composer, path, inputs)?;
        Ok(TransactionRequest::new(
            self.composer.executor,
            data,
            self.value,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess_options(&self) -> AssessOptions {
        self.assess_options.clone()
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
    use alloy::primitives::keccak256;
    use degenbot_execution::solve_result::{HopDescriptor, HopFamily};
    use degenbot_execution::ProbeReading;

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
    /// doctrine the foreign corpus pins.
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

    /// **Own expected-bytes corpus**: the foreign `SimpleExecutor`
    /// payload for the canonical 2-hop sample path is a RECORDED constant —
    /// its own golden, distinct from the default adapter's. The Python mirror
    /// must reproduce these exact bytes (the cross-layer oracle), so
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

    /// The typed rewrite is a no-op for every committed golden: the `sol!` call
    /// type emits exactly the canonical bytes the retired hand-packing
    /// imitated, so the recorded corpus still pins the foreign payload (ADR-100
    /// D3's acceptance criterion for the rewrite).
    #[test]
    fn typed_encoding_reproduces_the_hand_packed_corpus() {
        const CORPUS: &str = "ead35cae\
            0000000000000000000000000000000000000000000000000de0b6b3a7640000\
            00000000000000000000000000000000000000000000000010cac896d2390000\
            0000000000000000000000000000000000000000000000000000000000000060\
            0000000000000000000000000000000000000000000000000000000000000002\
            0000000000000000000000000000000000000000000000000de0b6b3a7640000\
            00000000000000000000000000000000000000000000000010cac896d2390000";

        let (hop_outputs, _consumed) = sample_inputs();
        let typed = SimpleExecutor::executeCall {
            optimal_input: U256::from(hop_outputs[0]),
            final_output: U256::from(hop_outputs[1]),
            hop_outputs: hop_outputs.iter().copied().map(U256::from).collect(),
        }
        .abi_encode();

        assert_eq!(
            Bytes::from(typed),
            Bytes::from(alloy::hex::decode(format!("0x{CORPUS}")).unwrap()),
            "the typed encoding must reproduce the recorded corpus byte for byte"
        );
    }

    /// The declaration derived from the same `sol!` call type accepts the
    /// strategy's own encoding, and the wrapper forwards it unchanged — the
    /// tier-1 shape a foreign author opts into.
    #[test]
    fn the_sol_declaration_accepts_the_typed_encoding() {
        use degenbot_execution::{ContractViolation, ValidatedAdapter, ValuePolicy};

        let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
        let interface = strat
            .composer
            .interface()
            .unwrap()
            .with_value_policy(ValuePolicy::Zero);
        assert_eq!(interface.target(), Some(EXECUTOR));
        assert_eq!(
            interface.selectors(),
            vec![SimpleExecutor::executeCall::SIGNATURE],
            "the declaration is the call type's own signature"
        );

        let validated = ValidatedAdapter::new(strat, interface).unwrap();
        let (hop_outputs, consumed) = sample_inputs();
        let inputs = ComposerInputs {
            optimal_input: hop_outputs[0],
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts: ComposeOptions,
        };
        let transaction =
            ExecutionAdapter::encode(&validated, &PathInfo::new(Vec::new()), &inputs).unwrap();
        assert_eq!(transaction.to, EXECUTOR);
        assert_eq!(transaction.data[..4], simple_executor_selector());

        // A call the declaration does not describe is refused: the wrapper
        // reports it rather than repairing it.
        let mut elsewhere = transaction.data.to_vec();
        elsewhere[..4].copy_from_slice(&keccak256(b"steal(uint256)")[..4]);
        assert!(matches!(
            validated
                .interface()
                .validate(&TransactionRequest::call(EXECUTOR, Bytes::from(elsewhere))),
            Err(ContractViolation::Selector { .. })
        ));
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
    /// assessor resolves a typed observation's readings into gross → net →
    /// pass.
    #[test]
    fn four_part_wiring_and_builtin_assess() {
        let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
        // Probe — declared data (`WETH.balanceOf(executor)`, complete calldata).
        assert_eq!(strat.probes.len(), 1);
        assert_eq!(strat.probes[0].label(), "WETH");
        // The declared read's selector is Alloy's, checked against an
        // independent keccak of the signature.
        assert_eq!(
            &IWeth::balanceOfCall::SELECTOR[..],
            &keccak256(b"balanceOf(address)")[..4]
        );
        let read = weth_balance_of_calldata(EXECUTOR);
        assert_eq!(read.len(), 4 + 32, "the read carries its ABI argument");
        assert_eq!(
            &read[..4],
            &IWeth::balanceOfCall::SELECTOR[..],
            "balanceOf(address)"
        );
        assert_eq!(
            &read[4..],
            &U256::from_be_slice(EXECUTOR.as_slice()).to_be_bytes::<32>(),
            "the read observes the sample executor"
        );
        assert_eq!(
            strat.probes[0],
            ProbeSpec::call("WETH", WETH, read),
            "the declaration is exactly that complete call"
        );
        // Assess — built-in sum-of-deltas + zero min threshold.
        assert_eq!(strat.assess_options, AssessOptions::default());
        // Fee — built-in market-percentile default.
        assert!(strat.fee_policy.market_percentile);

        // Built-in assess: gross = Σ deltas, net = gross − gas×fee.
        // gas_cost = 100_000 × (1e9 + 1e9) = 2e14; gross 3e15 clears it.
        let spec = ProbeSpec::call("WETH", WETH, weth_balance_of_calldata(EXECUTOR));
        let observation = ExecutionObservation::new(
            TransactionRequest::new(EXECUTOR, Bytes::new(), U256::ZERO),
            degenbot_execution::ExecutionStatus::Success,
            100_000,
            1_000_000_000,
        )
        .with_probe_reads(vec![
            ProbeReading::new(
                spec.clone(),
                Some(U256::from(0u64)),
                Some(U256::from(4_000_000_000_000_000u64)),
            ),
            ProbeReading::new(
                spec,
                Some(U256::from(1_000_000_000_000_000u64)),
                Some(U256::from(0u64)),
            ),
        ]);
        let result = strat.assess(&observation).unwrap();
        assert_eq!(result.gross_profit, U256::from(3_000_000_000_000_000u64));
        assert!(result.passed);

        // A genuine loss (gross < gas cost) is a FAIL, not a zero pass.
        let loss = ExecutionObservation::new(
            TransactionRequest::new(EXECUTOR, Bytes::new(), U256::ZERO),
            degenbot_execution::ExecutionStatus::Success,
            100_000,
            1_000_000_000,
        )
        .with_probe_reads(vec![ProbeReading::new(
            ProbeSpec::call("WETH", WETH, weth_balance_of_calldata(EXECUTOR)),
            Some(U256::from(0u64)),
            Some(U256::from(10_000u64)),
        )]);
        assert!(!strat.assess(&loss).unwrap().passed);
    }

    /// Encode returns a complete transaction: the sample contract as the
    /// target, its ABI calldata, and the strategy's native value — zero for
    /// the flash-funded sample executor, preserved when configured.
    #[test]
    fn encode_returns_the_complete_transaction_for_the_sample_contract() {
        let strat = SimpleExecutorStrategy::for_executor(EXECUTOR);
        let (hop_outputs, consumed) = sample_inputs();
        let inputs = ComposerInputs {
            optimal_input: hop_outputs[0],
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts: ComposeOptions,
        };
        let path = PathInfo::new(Vec::new());

        let transaction = ExecutionAdapter::encode(&strat, &path, &inputs).unwrap();
        assert_eq!(transaction.to, EXECUTOR);
        assert_eq!(transaction.value, U256::ZERO);
        assert_eq!(&transaction.data[..4], &simple_executor_selector());

        let funded = SimpleExecutorStrategy {
            value: U256::from(5_000_000_000_000_000u64),
            ..SimpleExecutorStrategy::for_executor(EXECUTOR)
        };
        let transaction = ExecutionAdapter::encode(&funded, &path, &inputs).unwrap();
        assert_eq!(transaction.value, U256::from(5_000_000_000_000_000u64));
    }

    /// The sealed-view entry (`encode_solution`) projects a real
    /// `SolveResult` (via `from_solve_path`) and produces the same foreign
    /// transaction — the `(path, SolveResult) -> transaction` user path.
    #[test]
    fn encode_solution_projects_solve_result_view() {
        use degenbot_pools::solve::SolvePathResult;
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
        let transaction = strat.encode_solution(&view).unwrap();
        assert_eq!(transaction.to, EXECUTOR);
        assert_eq!(transaction.value, U256::ZERO);
        assert_eq!(&transaction.data[..4], &simple_executor_selector());
        assert_eq!(transaction.data.len(), 4 + 4 * 32 + 2 * 32);
    }
}
