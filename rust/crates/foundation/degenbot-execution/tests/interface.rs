#![expect(clippy::expect_used, clippy::panic)]

//! The declared contract interface (ADR-100) — tier 1's neutral declaration and
//! its per-encode checks, the optional `ValidatedAdapter` wrapper, and tier 2's
//! explicit validation cases.
//!
//! This is a consumer test: it reaches every type off `degenbot_execution`
//! alone. The declaration is derived two ways — an ABI JSON array and a `sol!`
//! signature — from one `execute(uint256,uint256,uint256[])` declaration, so a
//! drift between the two routes is a test failure rather than a silent
//! divergence.

use std::sync::atomic::{AtomicUsize, Ordering};

use alloy::primitives::{address, keccak256, Address, Bytes, U256};
use alloy::sol_types::SolCall;

use degenbot_execution::{
    case_hop_family, run_cases, AssessError, AssessOptions, AssessRule, CaseAmounts, CaseFailure,
    ComposeError, ComposeOptions, ComposerInputs, ContractInterface, ContractViolation,
    DeclarationError, ExecutionAdapter, ExecutionObservation, ExecutionResult, ExecutionStatus,
    Expected, ExpectedCall, ExpectedTransaction, FeePolicy, HopInfo, PathInfo, PayloadAdapter,
    PayloadComposer, ProbeReading, ProbeSpec, ProbeSpecs, TransactionRequest, V2HopInfo,
    ValidatedAdapter, ValidationCase, ValuePolicy,
};

alloy::sol! {
    /// The sample contract's callable surface, declared once. Both declaration
    /// routes (`from_signatures` here, the ABI JSON below) describe THIS type.
    interface SimpleExecutor {
        function execute(uint256 optimal_input, uint256 final_output, uint256[] hop_outputs)
            external
            returns (bool);
        function cancel() external;
    }
}

const EXECUTOR: Address = address!("7777777777777777777777777777777777777777");
const ELSEWHERE: Address = address!("8888888888888888888888888888888888888888");
const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000_000;
const FINAL_OUTPUT: u128 = 1_210_000_000_000_000_000;

/// The recorded corpus the Rust sample's two-hop path produces. The declaration
/// is derived from the same `sol!` call type, so these bytes are the shared
/// oracle for the typed encoding.
const CORPUS: &str = "ead35cae\
    0000000000000000000000000000000000000000000000000de0b6b3a7640000\
    00000000000000000000000000000000000000000000000010cac896d2390000\
    0000000000000000000000000000000000000000000000000000000000000060\
    0000000000000000000000000000000000000000000000000000000000000002\
    0000000000000000000000000000000000000000000000000de0b6b3a7640000\
    00000000000000000000000000000000000000000000000010cac896d2390000";

const SAMPLE_ABI: &str = r#"[
  {"type": "constructor", "stateMutability": "nonpayable", "inputs": []},
  {"type": "event", "name": "Executed", "anonymous": false,
   "inputs": [{"name": "profit", "type": "uint256", "indexed": false}]},
  {"type": "error", "name": "NothingToDo", "inputs": []},
  {"type": "fallback", "stateMutability": "payable"},
  {"type": "function", "name": "execute", "stateMutability": "nonpayable",
   "inputs": [{"name": "optimal_input", "type": "uint256"},
              {"name": "final_output", "type": "uint256"},
              {"name": "hop_outputs", "type": "uint256[]"}],
   "outputs": [{"name": "", "type": "bool"}]},
  {"type": "function", "name": "cancel", "stateMutability": "nonpayable",
   "inputs": [], "outputs": []}
]"#;

const EXECUTE_SIGNATURE: &str = "execute(uint256,uint256,uint256[])";

/// The `sol!`-encoded sample call for the canonical two-hop path.
fn sample_calldata() -> Bytes {
    Bytes::from(
        SimpleExecutor::executeCall {
            optimal_input: U256::from(OPTIMAL_INPUT),
            final_output: U256::from(FINAL_OUTPUT),
            hop_outputs: vec![U256::from(OPTIMAL_INPUT), U256::from(FINAL_OUTPUT)],
        }
        .abi_encode(),
    )
}

fn sample_inputs<'a>(hop_outputs: &'a [u128], consumed_inputs: &'a [u128]) -> ComposerInputs<'a> {
    ComposerInputs {
        optimal_input: OPTIMAL_INPUT,
        hop_outputs,
        consumed_inputs,
        opts: ComposeOptions,
    }
}

fn recorded_corpus() -> Bytes {
    Bytes::from(alloy::hex::decode(format!("0x{CORPUS}")).expect("the corpus is hex"))
}

/// The interface derived from the `sol!` declaration — one route of the two the
/// ADR allows. No ABI JSON is involved.
fn declared_interface() -> ContractInterface {
    ContractInterface::from_signatures([SimpleExecutor::executeCall::SIGNATURE, "cancel()"])
        .expect("the sol! signatures declare a usable interface")
}

fn declared_interface_for(target: Address) -> ContractInterface {
    declared_interface()
        .with_target(target)
        .expect("a nonzero declared target")
}

fn abi_interface() -> ContractInterface {
    ContractInterface::from_abi_json(SAMPLE_ABI).expect("the ABI JSON declares a usable interface")
}

/// An encoding-only composer whose bytes come from the `sol!` call type — the
/// foreign-adapter shape this crate's sample ships.
struct SolComposer;

impl PayloadComposer for SolComposer {
    fn compose(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<Bytes, ComposeError> {
        let final_output = inputs
            .hop_outputs
            .last()
            .copied()
            .ok_or_else(|| ComposeError::encode("path has no hops"))?;
        Ok(Bytes::from(
            SimpleExecutor::executeCall {
                optimal_input: U256::from(inputs.optimal_input),
                final_output: U256::from(final_output),
                hop_outputs: inputs.hop_outputs.iter().copied().map(U256::from).collect(),
            }
            .abi_encode(),
        ))
    }
}

fn bound_adapter() -> PayloadAdapter<SolComposer> {
    PayloadAdapter::new(SolComposer, EXECUTOR)
}

fn encode(adapter: &dyn ExecutionAdapter) -> Result<TransactionRequest, ComposeError> {
    let hop_outputs = [OPTIMAL_INPUT, FINAL_OUTPUT];
    let consumed_inputs = [OPTIMAL_INPUT, FINAL_OUTPUT];
    adapter.encode(
        &PathInfo::new(Vec::new()),
        &sample_inputs(&hop_outputs, &consumed_inputs),
    )
}

/// The two-hop V2 path the case corpus pins.
fn two_v2_hops() -> Vec<HopInfo> {
    vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("1111111111111111111111111111111111111111"),
            token0_address: WETH,
            token1_address: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: address!("2222222222222222222222222222222222222222"),
            token0_address: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            token1_address: WETH,
            fee: 30,
            zfo: false,
        }),
    ]
}

// ════════════════════════════════════════════════════════════════════════════
// Tier 0 — the default pays nothing and changes nothing
// ════════════════════════════════════════════════════════════════════════════

/// An adapter that declares no interface — `PayloadAdapter` over a composer —
/// produces the pinned transaction constants, byte for byte, and never consults
/// a declaration. A declaration is an ADDITION: the unwrapped adapter's bytes
/// and value are exactly what the pre-ADR suites pin.
#[test]
fn an_adapter_without_a_declaration_encodes_byte_identically() {
    let adapter = bound_adapter();
    let transaction = encode(&adapter).expect("the tier-0 adapter encodes");
    assert_eq!(transaction.to, EXECUTOR);
    assert_eq!(transaction.value, U256::ZERO);
    assert_eq!(transaction.data, recorded_corpus());
    assert_eq!(transaction.data, sample_calldata());

    // The same bytes pass the declaration when one is attached: tier 0's output
    // is what tier 1 accepts, so attaching a declaration changes no encoding.
    let validated = ValidatedAdapter::new(adapter, declared_interface_for(EXECUTOR))
        .expect("the declarations agree");
    assert_eq!(
        encode(&validated).expect("the wrapped adapter encodes"),
        transaction
    );
}

/// A `simulate`-shaped custom assesment an adapter written before this ADR would
/// already ship: it implements Encode + Probe and overrides `assess`, and it
/// still compiles and runs against the unchanged trait — no wrapper, no new
/// required method, no declaration.
#[test]
fn an_adapter_written_before_this_adr_meets_the_unchanged_trait() {
    struct LegacyAdapter;

    impl ExecutionAdapter for LegacyAdapter {
        fn encode(
            &self,
            _path: &PathInfo,
            _inputs: &ComposerInputs<'_>,
        ) -> Result<TransactionRequest, ComposeError> {
            Ok(TransactionRequest::call(EXECUTOR, sample_calldata()))
        }

        fn probe_spec(&self) -> &ProbeSpecs {
            static NO_PROBES: ProbeSpecs = Vec::new();
            &NO_PROBES
        }

        fn assess(
            &self,
            observation: &ExecutionObservation,
        ) -> Result<ExecutionResult, AssessError> {
            Ok(ExecutionResult::price(
                U256::from(17u64),
                Vec::new(),
                observation,
                U256::ZERO,
                &self.fee_policy(),
            ))
        }
    }

    let adapter: &dyn ExecutionAdapter = &LegacyAdapter;
    let hop_outputs = [OPTIMAL_INPUT, FINAL_OUTPUT];
    let transaction = adapter
        .encode(
            &PathInfo::new(Vec::new()),
            &sample_inputs(&hop_outputs, &hop_outputs),
        )
        .expect("the legacy adapter encodes");
    assert_eq!(transaction.data, recorded_corpus());

    // The defaulted halves are still the trait's defaults.
    assert_eq!(adapter.assess_options(), AssessOptions::default());
    assert_eq!(adapter.fee_policy(), FeePolicy::default());

    let observation =
        ExecutionObservation::new(transaction, ExecutionStatus::Success, 50_000, 1_000_000_000);
    assert_eq!(
        adapter
            .assess(&observation)
            .expect("the custom assessment runs")
            .gross_profit,
        U256::from(17u64)
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Tier 1 — the declared interface, checked per encode
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn a_conforming_transaction_passes_its_declared_interface() {
    let interface = declared_interface_for(EXECUTOR).with_value_policy(ValuePolicy::Zero);
    assert_eq!(interface.target(), Some(EXECUTOR));
    assert_eq!(interface.value_policy(), ValuePolicy::Zero);
    assert_eq!(
        interface.selectors(),
        vec!["cancel()", EXECUTE_SIGNATURE],
        "no allow-list means every declared function is callable"
    );

    let transaction = TransactionRequest::call(EXECUTOR, sample_calldata());
    assert_eq!(interface.validate(&transaction), Ok(()));

    // The wrapped adapter validates the same transaction it forwards.
    let validated = ValidatedAdapter::new(bound_adapter(), interface).expect("declarations agree");
    assert_eq!(
        encode(&validated).expect("the conforming encode passes"),
        transaction
    );
}

#[test]
fn an_undeclared_selector_is_a_validation_violation() {
    let interface = declared_interface_for(EXECUTOR);
    let mut data = sample_calldata().to_vec();
    data[..4].copy_from_slice(&keccak256(b"notDeclared(uint256)")[..4]);
    let transaction = TransactionRequest::call(EXECUTOR, Bytes::from(data));

    let violation = interface
        .validate(&transaction)
        .expect_err("the selector is not declared");
    let violated: [u8; 4] = keccak256(b"notDeclared(uint256)")[..4]
        .try_into()
        .expect("four bytes");
    assert_eq!(
        violation,
        ContractViolation::Selector { selector: violated }
    );
    assert!(violation.to_string().contains("0x"), "{violation}");

    // An allow-list narrows the declared surface: `cancel()` is declared but not
    // allowed, so its own selector is refused the same way.
    let narrowed = ContractInterface::from_signatures([EXECUTE_SIGNATURE])
        .expect("a usable interface")
        .with_selectors(["cancel()"])
        .expect_err("cancel() is not in that ABI");
    assert_eq!(
        narrowed,
        DeclarationError::UnknownSelector {
            signature: "cancel()".to_string()
        }
    );

    let allow_listed = declared_interface_for(EXECUTOR)
        .with_selectors([EXECUTE_SIGNATURE])
        .expect("the signature is declared");
    assert_eq!(allow_listed.selectors(), vec![EXECUTE_SIGNATURE]);
    let cancel = TransactionRequest::call(
        EXECUTOR,
        Bytes::from(SimpleExecutor::cancelCall {}.abi_encode()),
    );
    assert!(
        matches!(
            allow_listed.validate(&cancel),
            Err(ContractViolation::Selector { .. })
        ),
        "a declared-but-not-allowed selector is refused"
    );
}

#[test]
fn argument_bytes_that_do_not_decode_against_the_declared_types_are_refused() {
    let interface = declared_interface_for(EXECUTOR);
    let corpus = recorded_corpus();

    // A truncated dynamic array: the length word survives, its payload does not.
    let truncated =
        TransactionRequest::call(EXECUTOR, Bytes::from(corpus[..corpus.len() - 32].to_vec()));
    let violation = interface
        .validate(&truncated)
        .expect_err("the array payload is missing");
    assert!(
        matches!(&violation, ContractViolation::Arguments { signature, .. } if signature == EXECUTE_SIGNATURE),
        "{violation:?}"
    );
    assert!(violation.to_string().contains("arguments"), "{violation}");

    // Trailing data is refused: the payload is not the canonical encoding of the
    // declared call, so it is not the call the adapter declares.
    let mut trailing = corpus.to_vec();
    trailing.extend_from_slice(&[0u8; 32]);
    let violation = interface
        .validate(&TransactionRequest::call(EXECUTOR, Bytes::from(trailing)))
        .expect_err("trailing bytes are not part of the declared call");
    assert!(
        matches!(&violation, ContractViolation::Arguments { signature, .. } if signature == EXECUTE_SIGNATURE),
        "{violation:?}"
    );

    // A truncated static head is refused too.
    let short = TransactionRequest::call(EXECUTOR, Bytes::from(corpus[..4 + 32].to_vec()));
    assert!(matches!(
        interface.validate(&short),
        Err(ContractViolation::Arguments { .. })
    ));

    // A payload shorter than its selector cannot name a function at all.
    let bare = TransactionRequest::call(EXECUTOR, Bytes::from(vec![0xea, 0xd3]));
    assert!(matches!(
        interface.validate(&bare),
        Err(ContractViolation::Calldata { .. })
    ));
}

#[test]
fn a_target_policy_naming_another_contract_is_refused() {
    let interface = declared_interface_for(EXECUTOR);
    let transaction = TransactionRequest::call(ELSEWHERE, sample_calldata());
    assert_eq!(
        interface.validate(&transaction),
        Err(ContractViolation::Target {
            declared: EXECUTOR,
            encoded: ELSEWHERE,
        })
    );

    // An interface that declares no target is address-agnostic: one declaration
    // may describe several deployments of one contract.
    assert_eq!(declared_interface().validate(&transaction), Ok(()));
    assert_eq!(declared_interface().target(), None);
}

#[test]
fn the_native_value_policy_refuses_zero_max_and_exact_differently() {
    let interface = declared_interface_for(EXECUTOR);
    let funded = |value: u64| {
        TransactionRequest::call(EXECUTOR, sample_calldata()).with_value(U256::from(value))
    };

    // `any` is the default and checks nothing.
    assert_eq!(interface.value_policy(), ValuePolicy::Any);
    assert_eq!(interface.validate(&funded(5)), Ok(()));
    assert_eq!(
        declared_interface_for(EXECUTOR)
            .with_value_policy(ValuePolicy::Any)
            .validate(&funded(5)),
        Ok(())
    );

    // `zero` is a flash-funded contract.
    let zero = declared_interface_for(EXECUTOR).with_value_policy(ValuePolicy::Zero);
    assert_eq!(zero.validate(&funded(0)), Ok(()));
    assert_eq!(
        zero.validate(&funded(1)),
        Err(ContractViolation::NativeValue {
            policy: ValuePolicy::Zero,
            encoded: U256::from(1u64),
        })
    );

    // `max` bounds the value.
    let max = declared_interface_for(EXECUTOR)
        .with_value_policy(ValuePolicy::parse("max", Some("100")).expect("a valid policy"));
    assert_eq!(max.value_policy(), ValuePolicy::Max(U256::from(100u64)));
    assert_eq!(max.validate(&funded(100)), Ok(()));
    assert!(matches!(
        max.validate(&funded(101)),
        Err(ContractViolation::NativeValue { .. })
    ));

    // `exact` pins it.
    let exact = declared_interface_for(EXECUTOR)
        .with_value_policy(ValuePolicy::parse("exact", Some("5")).expect("a valid policy"));
    assert_eq!(exact.validate(&funded(5)), Ok(()));
    assert!(matches!(
        exact.validate(&funded(6)),
        Err(ContractViolation::NativeValue { .. })
    ));

    // A policy document that cannot mean anything is refused, not defaulted.
    for (kind, amount) in [
        ("sometimes", None),
        ("max", None),
        ("max", Some("not-a-number")),
        ("exact", None),
        (
            "max",
            Some("115792089237316195423570985008687907853269984665640564039457584007913129639936"),
        ),
    ] {
        assert!(
            matches!(
                ValuePolicy::parse(kind, amount),
                Err(DeclarationError::InvalidValuePolicy { .. })
            ),
            "({kind}, {amount:?}) must be refused"
        );
    }
}

#[test]
fn the_compiled_selector_table_refuses_a_duplicate_selector() {
    let duplicate = ContractInterface::from_signatures([EXECUTE_SIGNATURE, EXECUTE_SIGNATURE])
        .expect_err("two functions cannot share a selector");
    match &duplicate {
        DeclarationError::DuplicateSelector {
            selector,
            first,
            second,
        } => {
            assert_eq!(first, EXECUTE_SIGNATURE);
            assert_eq!(second, EXECUTE_SIGNATURE);
            assert_eq!(&selector[..], &keccak256(EXECUTE_SIGNATURE.as_bytes())[..4]);
        }
        other => panic!("{other:?} is not a duplicate-selector refusal"),
    }
}

#[test]
fn an_interface_declared_from_sol_signatures_matches_its_abi_json_declaration() {
    let from_signatures = declared_interface();
    let from_json = abi_interface();

    assert_eq!(from_signatures.selectors(), from_json.selectors());
    for signature in from_signatures.selectors() {
        assert_eq!(
            from_signatures.parameter_types(signature),
            from_json.parameter_types(signature),
            "`{signature}` resolves to the same parameter types by both routes"
        );
    }
    assert_eq!(
        from_signatures.parameter_types(EXECUTE_SIGNATURE),
        Some(
            [
                alloy::dyn_abi::DynSolType::Uint(256),
                alloy::dyn_abi::DynSolType::Uint(256),
                alloy::dyn_abi::DynSolType::Array(Box::new(alloy::dyn_abi::DynSolType::Uint(256))),
            ]
            .as_slice()
        )
    );
    assert_eq!(
        from_signatures.parameter_types("cancel()"),
        Some([].as_slice())
    );

    // A `sol!`-derived selector and the ABI's own selector agree, so the two
    // routes cannot disagree about what a signature means.
    assert_eq!(
        &SimpleExecutor::executeCall::SELECTOR[..],
        &keccak256(EXECUTE_SIGNATURE.as_bytes())[..4]
    );

    // Both routes accept exactly the same transaction.
    let transaction = TransactionRequest::call(EXECUTOR, sample_calldata());
    assert_eq!(from_signatures.validate(&transaction), Ok(()));
    assert_eq!(from_json.validate(&transaction), Ok(()));
}

#[test]
fn a_declared_probe_to_the_adapters_own_contract_must_be_declared() {
    // A probe to a DIFFERENT contract is not this interface's business: WETH is
    // untouched even though its selector is not declared here.
    let weth_probe = ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31]);
    let outside = PayloadAdapter::new(SolComposer, EXECUTOR).with_probes(vec![weth_probe.clone()]);
    ValidatedAdapter::new(outside, declared_interface_for(EXECUTOR))
        .expect("a probe to another contract is left alone");

    // A probe to the interface's OWN contract must name a declared selector.
    let undeclared = PayloadAdapter::new(SolComposer, EXECUTOR).with_probes(vec![ProbeSpec::call(
        "PROFIT",
        EXECUTOR,
        [0xde, 0xad, 0xbe, 0xef],
    )]);
    match ValidatedAdapter::new(undeclared, declared_interface_for(EXECUTOR)) {
        Err(DeclarationError::Probe { label, .. }) => assert_eq!(label, "PROFIT"),
        Ok(_) => panic!("the probe's selector is not declared, so this must be refused"),
        Err(other) => panic!("{other:?} is not a probe refusal"),
    }

    // Declared selector, undeclared argument shape: the probe's own calldata
    // must decode against the declaration.
    let mut mangled = SimpleExecutor::executeCall {
        optimal_input: U256::from(OPTIMAL_INPUT),
        final_output: U256::from(FINAL_OUTPUT),
        hop_outputs: vec![U256::from(OPTIMAL_INPUT)],
    }
    .abi_encode();
    mangled.truncate(mangled.len() - 32);
    let mangled_probe = PayloadAdapter::new(SolComposer, EXECUTOR)
        .with_probes(vec![ProbeSpec::call("EXECUTE", EXECUTOR, mangled)]);
    assert!(matches!(
        ValidatedAdapter::new(mangled_probe, declared_interface_for(EXECUTOR)),
        Err(DeclarationError::Probe { .. })
    ));

    // A conforming probe to the interface's own contract is accepted.
    let conforming = PayloadAdapter::new(SolComposer, EXECUTOR).with_probes(vec![ProbeSpec::call(
        "EXECUTE",
        EXECUTOR,
        sample_calldata(),
    )]);
    ValidatedAdapter::new(conforming, declared_interface_for(EXECUTOR))
        .expect("a declared, decodable probe is accepted");
}

#[test]
fn a_declaration_that_cannot_validate_is_refused() {
    // Not a standard ABI JSON array.
    assert!(matches!(
        ContractInterface::from_abi_json(r#"{"abi": []}"#),
        Err(DeclarationError::AbiJson { .. })
    ));
    assert!(matches!(
        ContractInterface::from_abi_json("[]"),
        Err(DeclarationError::EmptyInterface)
    ));
    assert!(matches!(
        ContractInterface::from_abi_json(
            r#"[{"type": "event", "name": "E", "anonymous": false, "inputs": []}]"#
        ),
        Err(DeclarationError::EmptyInterface)
    ));

    // A `sol!`-route signature string that does not parse.
    assert!(matches!(
        ContractInterface::from_signatures(["execute(uint256,"]),
        Err(DeclarationError::UnparsableSignature { signature }) if signature == "execute(uint256,"
    ));
    assert!(matches!(
        ContractInterface::from_signatures(std::iter::empty::<&str>()),
        Err(DeclarationError::EmptyInterface)
    ));

    // A zero declared target is a typo, not an address.
    assert!(matches!(
        declared_interface().with_target(Address::ZERO),
        Err(DeclarationError::ZeroTarget)
    ));
}

// ════════════════════════════════════════════════════════════════════════════
// The wrapper — forwarding, never repairing
// ════════════════════════════════════════════════════════════════════════════

/// An adapter with every assessment half overridden, so a wrapper that
/// re-derived any of them instead of forwarding is visible.
struct AssessedAdapter {
    probes: ProbeSpecs,
    options: AssessOptions,
    fee: FeePolicy,
}

impl ExecutionAdapter for AssessedAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::call(EXECUTOR, sample_calldata()))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess_options(&self) -> AssessOptions {
        self.options.clone()
    }

    fn fee_policy(&self) -> FeePolicy {
        self.fee
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        // A custom assessment with its own gross figure: no probe delta and no
        // return data is consulted, so only the inner's override can produce it.
        Ok(ExecutionResult::price(
            U256::from(4_242u64),
            Vec::new(),
            observation,
            self.options.min_net_profit,
            &self.fee,
        ))
    }
}

#[test]
fn the_wrapper_forwards_every_assessment_half_unchanged() {
    let inner = AssessedAdapter {
        probes: vec![ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31])],
        options: AssessOptions {
            rule: AssessRule::ReturnData,
            min_net_profit: U256::from(1_000u64),
        },
        fee: FeePolicy {
            market_percentile: false,
            override_priority_fee: 7_777,
        },
    };
    let validated = ValidatedAdapter::new(inner, declared_interface_for(EXECUTOR))
        .expect("the declarations agree");

    assert_eq!(
        validated.probe_spec(),
        &vec![ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31])],
        "the inner's declared probes are forwarded unchanged"
    );
    assert_eq!(
        validated.assess_options(),
        AssessOptions {
            rule: AssessRule::ReturnData,
            min_net_profit: U256::from(1_000u64),
        }
    );
    assert_eq!(
        validated.fee_policy(),
        FeePolicy {
            market_percentile: false,
            override_priority_fee: 7_777,
        }
    );

    let observation = ExecutionObservation::new(
        TransactionRequest::call(EXECUTOR, sample_calldata()),
        ExecutionStatus::Success,
        50_000,
        1_000_000_000,
    )
    .with_probe_reads(vec![ProbeReading::new(
        ProbeSpec::call("WETH", WETH, [0x70, 0xa0, 0x82, 0x31]),
        Some(U256::ZERO),
        Some(U256::from(1u64)),
    )]);
    let wrapped = validated
        .assess(&observation)
        .expect("the custom assessment forwards");
    assert_eq!(
        wrapped.gross_profit,
        U256::from(4_242u64),
        "the inner's custom assess ran, not the default rule"
    );
    assert_eq!(wrapped.priority_fee, 7_777);

    // Unwrapping returns the inner adapter untouched: the declaration is the
    // only thing the wrapper owns.
    let inner = validated.into_inner();
    assert_eq!(inner.probe_spec()[0].label(), "WETH");
    assert_eq!(inner.fee_policy().override_priority_fee, 7_777);
}

/// An adapter that overrides the sealed-view projection, so a wrapper that
/// re-derived `compose_view` from `encode` would be caught.
struct ProjectingAdapter {
    /// A distinct final output, so the projected calldata is identifiable.
    final_output: u64,
}

impl ExecutionAdapter for ProjectingAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::call(EXECUTOR, sample_calldata()))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn compose_view(
        &self,
        _path: &PathInfo,
        _result: &degenbot_execution::SolveResult,
    ) -> Result<TransactionRequest, ComposeError> {
        Ok(TransactionRequest::call(
            EXECUTOR,
            Bytes::from(
                SimpleExecutor::executeCall {
                    optimal_input: U256::from(self.final_output),
                    final_output: U256::from(self.final_output),
                    hop_outputs: vec![U256::from(self.final_output)],
                }
                .abi_encode(),
            ),
        ))
    }
}

#[test]
fn the_wrapper_forwards_compose_view_so_an_inner_override_is_not_bypassed() {
    let inner = ProjectingAdapter { final_output: 99 };
    let projected = inner
        .compose_view(
            &PathInfo::new(Vec::new()),
            &degenbot_execution::SolveResult {
                path_id: 0,
                hop_count: 0,
                optimal_input: U256::ZERO,
                hop_outputs: Vec::new(),
                consumed_inputs: Vec::new(),
                net_profit: U256::ZERO,
                hop_descriptors: Vec::new(),
            },
        )
        .expect("the inner projects");

    let validated = ValidatedAdapter::new(inner, declared_interface_for(EXECUTOR))
        .expect("the declarations agree");
    let through_wrapper = validated
        .compose_view(
            &PathInfo::new(Vec::new()),
            &degenbot_execution::SolveResult {
                path_id: 0,
                hop_count: 0,
                optimal_input: U256::ZERO,
                hop_outputs: Vec::new(),
                consumed_inputs: Vec::new(),
                net_profit: U256::ZERO,
                hop_descriptors: Vec::new(),
            },
        )
        .expect("the inner's projection is forwarded");
    assert_eq!(
        through_wrapper, projected,
        "the inner's own compose_view output, not a re-derivation"
    );

    // The same forwarding validates: an inner projection that violates the
    // declaration is refused, never repaired.
    let elsewhere = ContractInterface::from_signatures([EXECUTE_SIGNATURE])
        .expect("a usable interface")
        .with_target(ELSEWHERE)
        .expect("a nonzero declared target");
    let violating = ValidatedAdapter::new(ProjectingAdapter { final_output: 99 }, elsewhere)
        .expect("declarations agree");
    let error = violating
        .compose_view(
            &PathInfo::new(Vec::new()),
            &degenbot_execution::SolveResult {
                path_id: 0,
                hop_count: 0,
                optimal_input: U256::ZERO,
                hop_outputs: Vec::new(),
                consumed_inputs: Vec::new(),
                net_profit: U256::ZERO,
                hop_descriptors: Vec::new(),
            },
        )
        .expect_err("the declared target is not the encoded one");
    assert_eq!(
        error,
        ComposeError::Validation(ContractViolation::Target {
            declared: ELSEWHERE,
            encoded: EXECUTOR,
        })
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Tier 2 — validation cases over a real PathInfo and real amounts
// ════════════════════════════════════════════════════════════════════════════

/// What one `encode` call was handed — recorded, then asserted by the test.
#[derive(Clone, Debug)]
struct Seen {
    hops: Vec<(u16, bool)>,
    hops_typed: Vec<HopInfo>,
    optimal_input: u128,
    hop_outputs: Vec<u128>,
    consumed_inputs: Vec<u128>,
}

/// An adapter that records the intake it was handed and encodes the corpus.
struct RecordingAdapter {
    calls: AtomicUsize,
    seen: std::sync::Mutex<Vec<Seen>>,
}

impl RecordingAdapter {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the fixture's lock").clone()
    }
}

impl ExecutionAdapter for RecordingAdapter {
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let hops = path
            .hops
            .iter()
            .map(|hop| match hop {
                HopInfo::V2(hop) => (hop.fee, hop.zfo),
                HopInfo::V3(hop) => (u16::try_from(hop.fee).unwrap_or_default(), hop.zfo),
                HopInfo::V4(hop) => (u16::try_from(hop.fee).unwrap_or_default(), hop.zfo),
            })
            .collect();
        self.seen.lock().expect("the fixture's lock").push(Seen {
            hops,
            hops_typed: path.hops.clone(),
            optimal_input: inputs.optimal_input,
            hop_outputs: inputs.hop_outputs.to_vec(),
            consumed_inputs: inputs.consumed_inputs.to_vec(),
        });
        Ok(TransactionRequest::call(EXECUTOR, sample_calldata()))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }
}

fn transaction_case(name: &str) -> ValidationCase {
    ValidationCase::new(
        name,
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Transaction(ExpectedTransaction {
            selector: EXECUTE_SIGNATURE.to_string(),
            target: Some(EXECUTOR),
            value: Some(U256::ZERO),
            call: Some(ExpectedCall::Arguments(vec![
                alloy::dyn_abi::DynSolValue::Uint(U256::from(OPTIMAL_INPUT), 256),
                alloy::dyn_abi::DynSolValue::Uint(U256::from(FINAL_OUTPUT), 256),
                alloy::dyn_abi::DynSolValue::Array(vec![
                    alloy::dyn_abi::DynSolValue::Uint(U256::from(OPTIMAL_INPUT), 256),
                    alloy::dyn_abi::DynSolValue::Uint(U256::from(FINAL_OUTPUT), 256),
                ]),
            ])),
        }),
    )
    .expect("the case document is well formed")
}

#[test]
fn a_case_hands_the_encoder_a_real_path_info_and_real_amounts() {
    let adapter = RecordingAdapter::new();
    let interface = declared_interface_for(EXECUTOR);
    let reports = run_cases(
        &adapter,
        &interface,
        &[transaction_case("two_hop_v2_execute")],
    );

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].name, "two_hop_v2_execute");
    assert!(reports[0].passed(), "{:?}", reports[0].result);
    assert_eq!(adapter.calls(), 1, "one case is one encode");

    // The case's hops arrived as real V2 hop descriptors, per-family fields
    // included, in path order — and its amounts as the seam's borrowed bundle.
    let seen = adapter.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].hops, vec![(30, true), (30, false)]);
    assert_eq!(seen[0].optimal_input, OPTIMAL_INPUT);
    assert_eq!(seen[0].hop_outputs, vec![OPTIMAL_INPUT, FINAL_OUTPUT]);
    assert_eq!(seen[0].consumed_inputs, vec![OPTIMAL_INPUT, FINAL_OUTPUT]);

    // The case reproduces the sample's recorded corpus exactly.
    let corpus_case = ValidationCase::new(
        "corpus",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Transaction(ExpectedTransaction {
            selector: EXECUTE_SIGNATURE.to_string(),
            target: None,
            value: None,
            call: Some(ExpectedCall::Calldata(recorded_corpus())),
        }),
    )
    .expect("the case document is well formed");
    let reports = run_cases(&adapter, &interface, &[corpus_case]);
    assert!(reports[0].passed(), "{:?}", reports[0].result);
}

/// A case's hops arrive as the real per-family descriptors, every field
/// included — the full `HopInfo` shape, not the `SolveResult` report shape.
#[test]
fn a_case_hands_the_encoder_every_per_family_hop_field() {
    let v3 = degenbot_execution::V3HopInfo {
        pool_address: address!("3333333333333333333333333333333333333333"),
        token0_address: WETH,
        token1_address: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        fee: 3_000,
        zfo: false,
    };
    let v4 = degenbot_execution::V4HopInfo {
        pool_manager_address: address!("000000000004444c5dc75cb358380d2e3de08a90"),
        pool_id_hex: format!("0x{}", "ab".repeat(32)),
        currency0_address: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        currency1_address: WETH,
        fee: 500,
        tick_spacing: 10,
        hook_address: address!("00000000000000000000000000000000000000ff"),
        zfo: true,
    };
    let case = ValidationCase::new(
        "v3_then_v4",
        vec![HopInfo::V3(v3.clone()), HopInfo::V4(v4.clone())],
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Transaction(ExpectedTransaction {
            selector: EXECUTE_SIGNATURE.to_string(),
            target: None,
            value: None,
            call: None,
        }),
    )
    .expect("the case document is well formed");

    let adapter = RecordingAdapter::new();
    let reports = run_cases(&adapter, &declared_interface_for(EXECUTOR), &[case]);
    assert!(reports[0].passed(), "{:?}", reports[0].result);

    let seen = adapter.seen();
    assert_eq!(seen[0].hops_typed.len(), 2);
    match &seen[0].hops_typed[0] {
        HopInfo::V3(hop) => {
            assert_eq!(hop.pool_address, v3.pool_address);
            assert_eq!(hop.token0_address, v3.token0_address);
            assert_eq!(hop.token1_address, v3.token1_address);
            assert_eq!(hop.fee, v3.fee);
            assert!(!hop.zfo);
        }
        other => panic!("{other:?} is not the case's V3 hop"),
    }
    match &seen[0].hops_typed[1] {
        HopInfo::V4(hop) => {
            assert_eq!(hop.pool_manager_address, v4.pool_manager_address);
            assert_eq!(hop.pool_id_hex, v4.pool_id_hex);
            assert_eq!(hop.currency0_address, v4.currency0_address);
            assert_eq!(hop.currency1_address, v4.currency1_address);
            assert_eq!(hop.fee, v4.fee);
            assert_eq!(hop.tick_spacing, v4.tick_spacing);
            assert_eq!(hop.hook_address, v4.hook_address);
            assert!(hop.zfo);
        }
        other => panic!("{other:?} is not the case's V4 hop"),
    }
}

/// An adapter that declines every path for want of capability.
struct DecliningAdapter;

impl ExecutionAdapter for DecliningAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        _inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        Err(ComposeError::unsupported(
            "this contract has no producer for this path",
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }
}

#[test]
fn a_case_whose_adapter_declines_the_path_reports_the_decline() {
    let interface = declared_interface_for(EXECUTOR);
    let decline = ValidationCase::new(
        "v4_path_outside_capability",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Decline,
    )
    .expect("the case document is well formed");

    let reports = run_cases(
        &DecliningAdapter,
        &interface,
        std::slice::from_ref(&decline),
    );
    assert!(reports[0].passed(), "{:?}", reports[0].result);

    // A produced transaction where a decline was expected is a failure, and the
    // message names the case.
    let reports = run_cases(&RecordingAdapter::new(), &interface, &[decline]);
    assert!(!reports[0].passed());
    match &reports[0].result {
        Err(CaseFailure::Refused(ComposeError::Encode(detail))) => {
            assert!(detail.contains("v4_path_outside_capability"), "{detail}");
        }
        other => panic!("{other:?} is not a reported refusal"),
    }

    // A transaction case whose adapter declines reports the decline, unchanged.
    let reports = run_cases(
        &DecliningAdapter,
        &interface,
        &[transaction_case("two_hop_v2_execute")],
    );
    assert!(matches!(
        &reports[0].result,
        Err(CaseFailure::Refused(ComposeError::Unsupported(_)))
    ));
}

#[test]
fn a_case_with_a_mismatched_calldata_golden_names_the_case_and_the_diff() {
    let interface = declared_interface_for(EXECUTOR);
    let mut wrong = recorded_corpus().to_vec();
    wrong[4 + 31] ^= 0x01;
    let case = ValidationCase::new(
        "two_hop_v2_execute",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Transaction(ExpectedTransaction {
            selector: EXECUTE_SIGNATURE.to_string(),
            target: None,
            value: None,
            call: Some(ExpectedCall::Calldata(Bytes::from(wrong))),
        }),
    )
    .expect("the case document is well formed");

    let reports = run_cases(&RecordingAdapter::new(), &interface, &[case]);
    match &reports[0].result {
        Err(CaseFailure::Mismatch(ContractViolation::CaseExpectation {
            case,
            field,
            expected,
            actual,
        })) => {
            assert_eq!(case, "two_hop_v2_execute");
            assert_eq!(field, "calldata");
            assert!(expected.starts_with("0xead35cae"), "{expected}");
            assert_ne!(expected, actual);
        }
        other => panic!("{other:?} is not a case-expectation mismatch"),
    }

    // A mismatched DECODED argument names the argument position.
    let mut wrong_argument = transaction_case("two_hop_v2_execute");
    if let Expected::Transaction(expected) = &mut wrong_argument.expect {
        expected.call = Some(ExpectedCall::Arguments(vec![
            alloy::dyn_abi::DynSolValue::Uint(U256::from(1u64), 256),
            alloy::dyn_abi::DynSolValue::Uint(U256::from(FINAL_OUTPUT), 256),
            alloy::dyn_abi::DynSolValue::Array(vec![
                alloy::dyn_abi::DynSolValue::Uint(U256::from(OPTIMAL_INPUT), 256),
                alloy::dyn_abi::DynSolValue::Uint(U256::from(FINAL_OUTPUT), 256),
            ]),
        ]));
    }
    let reports = run_cases(&RecordingAdapter::new(), &interface, &[wrong_argument]);
    match &reports[0].result {
        Err(CaseFailure::Mismatch(ContractViolation::CaseExpectation { field, .. })) => {
            assert_eq!(field, "arguments[0]");
        }
        other => panic!("{other:?} is not an argument mismatch"),
    }
}

#[test]
fn a_malformed_case_is_refused_before_any_encode_runs() {
    let adapter = RecordingAdapter::new();

    // Wrong arity: two hops, one hop output.
    let arity = ValidationCase::new(
        "short_hop_outputs",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Decline,
    )
    .expect_err("the amounts do not match the hop count");
    assert!(
        matches!(&arity, DeclarationError::CaseArity { case, .. } if case == "short_hop_outputs"),
        "{arity:?}"
    );

    // An amount outside the seam's int128 bound.
    let over = ValidationCase::new(
        "over_bound",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: u128::MAX,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Decline,
    )
    .expect_err("the amount is above i128::MAX");
    assert!(
        matches!(&over, DeclarationError::CaseAmount { case, field } if case == "over_bound" && field == "optimal_input"),
        "{over:?}"
    );

    // A hop family tag that names no family.
    let unknown = case_hop_family("unknown_family", "v5").expect_err("v5 is not a family");
    assert!(
        matches!(&unknown, DeclarationError::CaseDocument { case, .. } if case == "unknown_family"),
        "{unknown:?}"
    );
    assert!(case_hop_family("v2_case", "v2").is_ok());
    assert!(case_hop_family("v3_case", "v3").is_ok());
    assert!(case_hop_family("v4_case", "v4").is_ok());

    // An expectation whose selector is not a function signature.
    let unparsable = ValidationCase::new(
        "unparsable_selector",
        two_v2_hops(),
        CaseAmounts {
            optimal_input: OPTIMAL_INPUT,
            hop_outputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
            consumed_inputs: vec![OPTIMAL_INPUT, FINAL_OUTPUT],
        },
        Expected::Transaction(ExpectedTransaction {
            selector: "execute(uint256,".to_string(),
            target: None,
            value: None,
            call: None,
        }),
    )
    .expect_err("the expectation names no function");
    assert!(matches!(&unparsable, DeclarationError::CaseDocument { .. }));

    assert_eq!(
        adapter.calls(),
        0,
        "a malformed case is refused before any encode runs"
    );
}
