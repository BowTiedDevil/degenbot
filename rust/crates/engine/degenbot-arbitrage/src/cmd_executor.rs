//! The built-in `cmd_executor` simulation binding: the settlement deployment's
//! Encode, Probe, and Assess parts over the generic engine leaf (ADR-098 D3).
//!
//! Everything the production settlement path knows about ITS execution contract
//! lives here, on the built-in side of the seam:
//!
//! - **Encode** — the command stream + packed `execute(bytes,uint256)` call, from
//!   the session's [`degenbot_executor::composers::EncodeOptions`] (the cmd
//!   config axis) and the deployment's executor / PoolManager / WETH identities.
//! - **Probe** — nothing. The contract measures its own profit (ADR-098 D7), so
//!   the built-in declares no pre/post balance reads: the [`ExecutionReceipt`]
//!   in the `execute` call's return data is the evidence.
//! - **Assess** — decode that receipt and price the RETAINED profit
//!   (`gross_profit − bribe_paid`, before gas). The operator's gas floor and
//!   market-aware priority-fee sizing are applied ONCE above this seam by the
//!   settlement strategy ([`crate::simulator`]), which is where ADR-098 D7 puts
//!   gas profitability and operator margins.
//!
//! # The accounting domain is the contract's, the pricing policy is ours
//!
//! `profit_mode` selects what the receipt's `gross_profit` measures: physical
//! WETH + native ETH (1) or PoolManager ERC6909 WETH claims (2). In mode 2 the
//! bribe is still native ETH leaving the executor's physical balance, so
//! `gross_profit − bribe_paid` intentionally spans the two ledgers — it is the
//! value the operator retained, which is what the public `gross_profit` has
//! always meant here.
//!
//! The warmup-slot math, the runtime-artifact selection, and the packed cmd
//! config are this binding's too: a deployment built here carries its own
//! declaration, so a foreign deployment never inherits any of it (ADR-098 D6).

use std::path::Path;
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};

use degenbot_core::op_info;
use degenbot_execution::{
    AssessError, ComposeError, ComposerInputs, ExecutionAdapter, ExecutionObservation,
    ExecutionResult, FeePolicy, PathInfo, ProbeSpecs, TransactionRequest,
};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions,
    EncodeRequest,
};
use degenbot_executor::compute_simulation_warmup_slots;
use degenbot_simulation::{DeclaredCode, SettlementWarmup};

use crate::deployment::SimulationDeployment;
use crate::executor_runtime::{load_executor_runtime, ExecutorRuntimeError};

/// The session-scoped built-in `cmd_executor` facts: the deployment's identities
/// plus the session's encode policy.
///
/// Construct once per session and hand it to
/// [`CmdExecutorBinding::deployment`] (the already-deployed contract) or
/// [`CmdExecutorBinding::injected`] (an explicit declared-code override).
#[derive(Clone, Copy, Debug)]
pub struct CmdExecutorBinding {
    owner: Address,
    executor: Address,
    weth: Address,
    pool_manager: Address,
    opts: EncodeOptions,
}

impl CmdExecutorBinding {
    /// Capture the deployment identities and the session's encode policy.
    #[must_use]
    pub const fn new(
        owner: Address,
        executor: Address,
        weth: Address,
        pool_manager: Address,
        opts: EncodeOptions,
    ) -> Self {
        Self {
            owner,
            executor,
            weth,
            pool_manager,
            opts,
        }
    }

    /// The operator key's address (the opportunity's `from`, and the account the
    /// engine funds in the simulated block).
    #[must_use]
    pub const fn owner(&self) -> Address {
        self.owner
    }

    /// The `cmd_executor` contract address.
    #[must_use]
    pub const fn executor(&self) -> Address {
        self.executor
    }

    /// The session's encode policy (the cmd config axes).
    #[must_use]
    pub const fn opts(&self) -> EncodeOptions {
        self.opts
    }

    /// Re-bind the encode policy (a consumer's own axis set, e.g. the inline
    /// sim's `erc6909_profit`).
    #[must_use]
    pub const fn with_opts(mut self, opts: EncodeOptions) -> Self {
        self.opts = opts;
        self
    }

    /// The built-in adapter: this deployment's Encode / Probe / Assess.
    #[must_use]
    pub const fn adapter(&self) -> CmdExecutorSimAdapter {
        CmdExecutorSimAdapter { binding: *self }
    }

    /// The deployment of the ACTUAL deployed contract: production sims the code
    /// the chain holds at [`Self::executor`], and the state overrides declare no
    /// code.
    #[must_use]
    pub fn deployment(&self) -> SimulationDeployment {
        self.build_deployment(None, None)
    }

    /// The deployment of a DECLARED build: `runtime` is installed at `address`
    /// in the simulated block. Explicit development data — never a silent
    /// substitution for the deployed implementation.
    ///
    /// The built-in adapter encodes its transaction to [`Self::executor`], so
    /// `address` must BE the executor address: injecting the runtime anywhere
    /// else would simulate code the transaction never runs. A mismatch is
    /// refused rather than simulated.
    ///
    /// # Errors
    ///
    /// [`InjectionAddressMismatch`] when `address` is not [`Self::executor`].
    pub fn injected(
        &self,
        address: Address,
        runtime: Bytes,
    ) -> Result<SimulationDeployment, InjectionAddressMismatch> {
        if address != self.executor {
            return Err(InjectionAddressMismatch {
                address,
                executor: self.executor,
            });
        }
        Ok(self.build_deployment(Some(address), Some(runtime)))
    }

    /// Resolve + load the executor runtime artifact for a declared build.
    ///
    /// The artifact selection (one precedence chain, one validated read) is the
    /// binding's, so no driver shell resolves, stats, or decodes it.
    ///
    /// # Errors
    ///
    /// [`ExecutorRuntimeError`] when the artifact is absent, unreadable, or not
    /// `0x`-hex bytecode; the error names the file and the knob that would fix
    /// it.
    pub fn load_runtime(
        explicit: Option<&Path>,
        contracts_dir: Option<&Path>,
    ) -> Result<Bytes, ExecutorRuntimeError> {
        let runtime = load_executor_runtime(explicit, contracts_dir)?;
        op_info!(
            domain = sim,
            bytes = runtime.bytecode.len(),
            path = %runtime.path.display(),
            "executor runtime bytecode loaded"
        );
        Ok(runtime.bytecode)
    }

    fn build_deployment(
        &self,
        injected: Option<Address>,
        runtime: Option<Bytes>,
    ) -> SimulationDeployment {
        // The warmup slots key on the account whose ledger the contract touches:
        // the injected address for a declared build, else the owner (the
        // session's funding account).
        let warmup_subject = injected.unwrap_or(self.owner);
        let settlement = SettlementWarmup {
            weth_address: self.weth,
            pool_manager_address: Some(self.pool_manager),
            slots: compute_simulation_warmup_slots(warmup_subject, self.weth),
        };
        let code = match (injected, runtime) {
            (Some(address), Some(runtime)) => Some(DeclaredCode { address, runtime }),
            _ => None,
        };
        SimulationDeployment::new(
            Arc::new(self.adapter()),
            self.owner,
            crate::simulator::execute_gas_limit(),
            code,
            Some(settlement),
        )
        // The built-in settlement path is the one deployment that opts into the
        // strategy's market-aware fee sizing (ADR-098 D7): its Assess reports the
        // gross evidence and the operator's gas/priority policy lands once, above
        // the seam. Every other deployment keeps its adapter's own Fee decision.
        .with_fee_sizing(crate::deployment::FeeSizing::MarketAware)
    }
}

/// A declared-code override named an address other than the deployment's own
/// executor: the transaction the adapter encodes targets the executor, so the
/// injected runtime would never run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectionAddressMismatch {
    /// The address the caller declared for the injected runtime.
    pub address: Address,
    /// The executor the built-in adapter encodes its transaction to.
    pub executor: Address,
}

impl std::fmt::Display for InjectionAddressMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "declared code address {} is not the deployment's executor {}: the adapter              encodes its transaction to {}, so injecting elsewhere would simulate code              the transaction never runs",
            self.address, self.executor, self.executor
        )
    }
}

impl std::error::Error for InjectionAddressMismatch {}

/// The built-in contract's own profit measurement (ADR-098 D7).
///
/// `execute(bytes,uint256)` returns this struct as TWO ABI words, in order:
/// `gross_profit` — the selected WETH-equivalent balance-domain increase after
/// the command stream and BEFORE the bribe — and `bribe_paid` — the native ETH
/// the executor actually transferred to the bribe recipient, after any
/// available-balance cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionReceipt {
    /// The selected-domain increase after the commands, before the bribe.
    pub gross_profit: U256,
    /// The native ETH actually paid to the bribe recipient.
    pub bribe_paid: U256,
}

/// The receipt's ABI size: two `uint256` words and nothing else.
pub const EXECUTION_RECEIPT_LEN: usize = 64;

impl ExecutionReceipt {
    /// Decode the receipt from a completed `execute` call's return data.
    ///
    /// The encoding is exactly two ABI words. Anything else — absent data, a
    /// short payload, or trailing bytes — is a decode failure rather than a
    /// padded or partially-read receipt: a fabricated figure is
    /// indistinguishable from a measured one.
    ///
    /// # Errors
    ///
    /// [`AssessError::Evidence`] when the engine captured no return data;
    /// [`AssessError::Decode`] when the payload is not exactly
    /// [`EXECUTION_RECEIPT_LEN`] bytes.
    pub fn decode(return_data: Option<&Bytes>) -> Result<Self, AssessError> {
        let data = return_data.ok_or_else(|| {
            AssessError::evidence(
                "the execute call's return data was not captured: the receipt is unmeasured",
            )
        })?;
        let words: [u8; EXECUTION_RECEIPT_LEN] = data.as_ref().try_into().map_err(|_| {
            AssessError::decode(format!(
                "the execute receipt is two ABI words ({EXECUTION_RECEIPT_LEN} bytes); \
                 the call returned {}",
                data.len()
            ))
        })?;
        let mut gross = [0u8; 32];
        gross.copy_from_slice(&words[..32]);
        let mut bribe = [0u8; 32];
        bribe.copy_from_slice(&words[32..]);
        Ok(Self {
            gross_profit: U256::from_be_bytes(gross),
            bribe_paid: U256::from_be_bytes(bribe),
        })
    }

    /// The retained profit before gas: the measured increase minus the bribe
    /// actually paid.
    ///
    /// # Errors
    ///
    /// [`AssessError::Evidence`] when the bribe exceeds the measured gross. The
    /// contract REQUESTS `gross_profit * bribe_bips / 10000` with
    /// `bribe_bips <= 10000`, and its available-balance cap can only reduce what
    /// it sends (pre-existing funds raise the cap, never the requested share),
    /// so `bribe_paid <= gross_profit` holds for every receipt the contract can
    /// produce. A receipt that breaks it is inconsistent evidence; saturating
    /// the subtraction would report a figure the transaction never produced.
    pub fn retained(&self) -> Result<U256, AssessError> {
        self.gross_profit
            .checked_sub(self.bribe_paid)
            .ok_or_else(|| {
                AssessError::evidence(format!(
                    "the receipt reports bribe_paid {} above gross_profit {} — \
                     inconsistent evidence",
                    self.bribe_paid, self.gross_profit
                ))
            })
    }
}

/// The built-in `cmd_executor` [`ExecutionAdapter`]: one complete
/// `execute(bytes,uint256)` transaction for the deployment, no declared probes,
/// and its receipt-based assessment.
#[derive(Clone, Copy, Debug)]
pub struct CmdExecutorSimAdapter {
    binding: CmdExecutorBinding,
}

impl CmdExecutorSimAdapter {
    /// The deployment facts this adapter encodes against.
    #[must_use]
    pub const fn binding(&self) -> &CmdExecutorBinding {
        &self.binding
    }

    /// The built-in adapter's own pricing floor: base fee, no priority fee.
    ///
    /// The operator's market-aware priority fee is applied once by the strategy
    /// (`crate::simulator::price_gross`) — reached because this binding's
    /// deployment declares `FeeSizing::MarketAware` — never re-derived here.
    const fn pricing_floor() -> FeePolicy {
        FeePolicy {
            market_percentile: false,
            override_priority_fee: 0,
        }
    }
}

impl ExecutionAdapter for CmdExecutorSimAdapter {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let request = EncodeRequest::new(
            path.clone(),
            inputs.optimal_input,
            inputs.hop_outputs.to_vec(),
            inputs.consumed_inputs.to_vec(),
            self.binding.opts,
        );
        let context = EncodeContext::new(
            self.binding.executor,
            self.binding.pool_manager,
            self.binding.weth,
        );
        let Some(commands) = encode_cmd_stream(&context, &request) else {
            return Err(ComposeError::unsupported(
                "the command grammar has no producer for this path",
            ));
        };
        let config = config_for_options(self.binding.opts, U256::ZERO)
            .map_err(|error| ComposeError::encode(format!("config_for_options failed: {error}")))?;
        let call = encode_execute_call(self.binding.executor, &commands, config);
        Ok(TransactionRequest::new(
            self.binding.executor,
            Bytes::from(call.data),
            U256::ZERO,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        // The receipt is the evidence (ADR-098 D7): the built-in assesses the
        // `execute` return data, so it declares no pre/post reads and spends no
        // sequence steps on balances nobody interprets.
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        let policy = Self::pricing_floor();
        if !observation.succeeded() {
            // A revert moved no value, and its payload is a revert reason rather
            // than a receipt: the shared pricing helper declines it with zero
            // gross instead of decoding bytes the contract never measured.
            return Ok(ExecutionResult::price(
                U256::ZERO,
                Vec::new(),
                observation,
                U256::ZERO,
                &policy,
            ));
        }

        // The contract measured its own profit: the receipt's retained figure
        // (`gross_profit − bribe_paid`, before gas) is what the public
        // `gross_profit` means, so gas and the fee policy apply exactly once
        // here and once more only if the deployment opts into market-aware
        // sizing above the seam.
        let retained = ExecutionReceipt::decode(observation.return_data.as_ref())?.retained()?;
        Ok(ExecutionResult::price(
            retained,
            Vec::new(),
            observation,
            U256::ZERO,
            &policy,
        ))
    }
}
