#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the fixture drives known-valid values and asserts on the typed product"
)]

//! Validating built-in target binding and bounded multi-release selection
//! (ADR-098 D4/D5).
//!
//! The acceptance proof for independent versioning is two DEFINED
//! `cmd_executor` releases selecting different complete encodings beside a
//! metadata-free foreign contract, through the one generic target interface:
//!
//! | release | major | selector | config (defaults) | receipt |
//! |---------|-------|----------|-------------------|---------|
//! | `cmd_executor/2` (production) | 1 | `0xab5898e8` | `0x1` | 2 words |
//! | `test_executor/2` (injected) | 2 | `0x368c4e9c` | `0x01…` (mode at bit 248) | 1 word |
//! | `test_executor/3` (injected) | 3 | `execute3(bytes,uint256)` | `0x1…` (mode at bit 24) | 3 words |
//!
//! Everything below is fail-closed evidence: an unknown release, an unknown
//! major, a getterless deployment without a mapping, a partial declaration, a
//! node failure, a declaration that disagrees with its manifest, and an
//! artifact that is not the release's own all refuse to bind.

use std::sync::{Arc, Mutex};

use alloy::primitives::{address, keccak256, Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};

use degenbot_arbitrage::{
    dispatch_across_deployments, BindingError, BoundCmdExecutor, CmdExecutorBinding,
    DeploymentRegistry, FeeOnTransferRegistry, PoolDivergence, ReleaseSource, SimulationBlock,
    SimulationDeployment, TargetSelection,
};
use degenbot_execution::{
    AssessError, CandidateId, ComposeError, ComposerInputs, DeploymentId, ExecutionAdapter,
    ExecutionObservation, ExecutionResult, ExecutionStatus, FeePolicy, HopInfo, PathInfo,
    ProbeSpecs, TransactionRequest, V4HopInfo,
};
use degenbot_executor::composers::{
    config_for_options, encode_cmd_stream, encode_execute_call, resolve_axes, EncodeContext,
    EncodeOptions, EncodeRequest,
};
use degenbot_executor::discovery::{GetterAnswers, GetterCall, METADATA_GETTERS};
use degenbot_executor::grammar_ledger::{Bribe, ProfitCapture};
use degenbot_executor::receipt::ExecutionReceipt;
use degenbot_executor::release::GetterOutcome;
use degenbot_executor::release::{
    capability, AbiReturnFieldDeclaration, CapabilityCommitment, ConfigFieldDeclaration,
    LegacyReleaseEntry, LegacyReleaseMapping, ManifestError, MetadataDecodeError, ReleaseId,
    ReleaseManifest, ReleaseMismatch, RELEASE_ID_TAG, RELEASE_MANIFEST_V2,
};
use degenbot_executor::release_registry::{
    required_capabilities, CmdStreamV1Encoding, Release, ReleaseEncoding, ReleaseError,
    ReleaseRegistry,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use serde_json::value::{to_raw_value, RawValue};

const OWNER: Address = address!("0x1111111111111111111111111111111111111111");
const EXECUTOR_V1: Address = address!("0x000000000000000000000000000000000000e001");
const EXECUTOR_2: Address = address!("0x000000000000000000000000000000000000e002");
const EXECUTOR_3: Address = address!("0x000000000000000000000000000000000000e003");
const FOREIGN: Address = address!("0x00000000000000000000000000000000000000f0");
const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const USDC: Address = address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 1_000_000;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000_000;

/// The exact `execute(bytes,uint256)` calldata the production release encodes
/// for `two_v2_hops()` with default options: `0xab5898e8` (selector), `0x1`
/// (packed config), and the v1 command stream. Pinned so the metadata cutover
/// cannot move an encoder byte.
const CUTOVER_CALL_DATA: &str = "0xab5898e800000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000007700bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb00cccccccccccccccccccccccccccccccccccccccc00a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48ff200001000000000de0b6b3a7640000fd001e24100201000000000de0b6b3a7640000210101fd001e10fe00000000000de0b6b3a7640000000000000000000000";

const GROSS_2: u64 = 2_000_000_000_000_000;
const GROSS_3: u64 = 3_000_000_000_000_000;
const GROSS_FOREIGN: u64 = 4_000_000_000_000_000;

const TAG_2: &str = "test_executor/2";
const TAG_3: &str = "test_executor/3";

/// The majors this fixture's registry declares: production v1 plus the two
/// injected test releases. A deployment outside this set is refused.
const FIXTURE_MAJORS: &[u32] = &[1, 2, 3];

// ── identities ───────────────────────────────────────────────────────────

fn deployment_id_v1() -> DeploymentId {
    DeploymentId::new(1)
}

fn deployment_id_2() -> DeploymentId {
    DeploymentId::new(2)
}

fn deployment_id_3() -> DeploymentId {
    DeploymentId::new(3)
}

fn deployment_id_foreign() -> DeploymentId {
    DeploymentId::new(4)
}

fn binding(executor: Address) -> CmdExecutorBinding {
    CmdExecutorBinding::new(
        OWNER,
        executor,
        WETH,
        POOL_MANAGER,
        EncodeOptions::default(),
    )
}

fn binding_with(executor: Address, opts: EncodeOptions) -> CmdExecutorBinding {
    CmdExecutorBinding::new(OWNER, executor, WETH, POOL_MANAGER, opts)
}

// ── metadata answers ─────────────────────────────────────────────────────

fn word(value: u64) -> Bytes {
    Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec())
}

fn tag_word(tag: &str) -> Bytes {
    Bytes::from(
        ReleaseId::from_ascii_tag(tag)
            .expect("the fixture's tags are valid release identities")
            .as_bytes()
            .to_vec(),
    )
}

fn commitment_word(commitment: CapabilityCommitment) -> Bytes {
    Bytes::copy_from_slice(commitment.as_bytes())
}

fn declared_answers(manifest: &ReleaseManifest) -> GetterAnswers {
    GetterAnswers::from_parts(
        Some(tag_word(&manifest.release_id)),
        Some(word(u64::from(manifest.protocol.major))),
        Some(word(u64::from(manifest.call_encoding.version))),
        Some(commitment_word(manifest.capability_commitment())),
    )
}

/// The ID list a deployment declaring `observed` IDs would commit to.
fn commitment_of(ids: &[u32]) -> Bytes {
    commitment_word(CapabilityCommitment::derive(ids))
}

// ── manifests and releases ───────────────────────────────────────────────

fn committed_manifest() -> ReleaseManifest {
    ReleaseManifest::parse(RELEASE_MANIFEST_V2).expect("the committed manifest validates")
}

/// The injected releases' config layout: `(name, bits, allowed, range)`.
type Layout = &'static [(
    &'static str,
    [u32; 2],
    Option<&'static [u64]>,
    Option<[u64; 2]>,
)];

/// `test_executor/2`'s layout: the profit mode sits in the HIGH byte, so the
/// config word's value differs from v1's for identical options.
const LAYOUT_2: Layout = &[
    ("min_profit", [0, 223], None, None),
    ("bribe_bips", [224, 239], None, Some([0, 10_000])),
    ("bribe_recipient_idx", [240, 247], None, Some([0, 31])),
    ("profit_mode", [248, 255], Some(&[1, 2]), None),
];

/// `test_executor/3`'s layout: the mode sits at bit 24 instead.
const LAYOUT_3: Layout = &[
    ("bribe_bips", [0, 15], None, Some([0, 10_000])),
    ("bribe_recipient_idx", [16, 23], None, Some([0, 31])),
    ("profit_mode", [24, 31], Some(&[1, 2]), None),
    ("min_profit", [32, 255], None, None),
];

fn config_fields(layout: Layout) -> Vec<ConfigFieldDeclaration> {
    layout
        .iter()
        .map(|(name, bits, allowed, range)| ConfigFieldDeclaration {
            name: (*name).to_owned(),
            bits: *bits,
            allowed: allowed.map(<[u64]>::to_vec),
            range: *range,
            semantics: format!("{name} ({bits:?})"),
        })
        .collect()
}

/// The two `execute` return shapes the injected releases declare.
fn receipt_fields(words: &[&str]) -> Vec<AbiReturnFieldDeclaration> {
    words
        .iter()
        .map(|name| AbiReturnFieldDeclaration {
            name: (*name).to_owned(),
            r#type: "uint256".to_owned(),
            semantics: (*name).to_owned(),
        })
        .collect()
}

/// Derive an injected release manifest from the committed one.
///
/// Every difference is meaningful: a new tag, a new protocol major, a new
/// joint call-encoding version, a different `execute` selector and return
/// shape, a different packed-config layout, the release's own artifact paths,
/// and (for `test_executor/2`) one capability deliberately ABSENT so a path
/// that needs it must decline.
fn alternate_manifest(
    tag: &str,
    major: u32,
    function: &str,
    layout: Layout,
    receipt: &[&str],
    drop_capability: Option<u32>,
) -> ReleaseManifest {
    let mut manifest = committed_manifest();
    let signature = format!("{function}(bytes,uint256)");
    let digest = keccak256(signature.as_bytes());
    let selector = format!(
        "0x{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );

    tag.clone_into(&mut manifest.release_id);
    manifest.protocol.major = major;
    manifest.protocol.minor = 0;
    manifest.call_encoding.version = major;
    function.clone_into(&mut manifest.call_encoding.execute_abi.function);
    manifest.call_encoding.execute_abi.signature = signature;
    manifest.call_encoding.execute_abi.selector = selector;
    manifest.call_encoding.execute_abi.returns.fields = receipt_fields(receipt);
    manifest.call_encoding.config.fields = config_fields(layout);
    manifest.artifact.runtime_code = format!("tests/fixtures/{function}.runtime.hex");
    manifest.artifact.provenance = format!("tests/fixtures/{function}.manifest.json");
    if let Some(id) = drop_capability {
        manifest
            .capabilities
            .ids
            .retain(|declared| declared.id != id);
        // Dropping an ID changes the declared set, so the release's commitment
        // must change with it: a manifest whose word disagreed with its own list
        // is refused at validation.
        let ids: Vec<u32> = manifest.capability_ids().collect();
        manifest.capabilities.commitment = CapabilityCommitment::derive(&ids);
    }
    // The manifest must validate against the fixture's bounded major set: an
    // injected release is a decision, not an escape from validation.
    manifest
        .validate_within(FIXTURE_MAJORS)
        .expect("the derived manifest stays self-consistent");
    manifest
}

/// One injected release's encoding: a DIFFERENT selector, a DIFFERENT config
/// layout, a length-prefixed command stream, and its own receipt width.
#[derive(Clone, Copy, Debug)]
struct TestEncoding {
    tag: &'static str,
    selector: [u8; 4],
    layout: Layout,
    receipt_words: usize,
}

impl TestEncoding {
    fn config(&self, opts: EncodeOptions) -> U256 {
        let (_, capture, bribe) = resolve_axes(opts);
        let profit_mode = u64::from(capture == ProfitCapture::Erc6909) + 1;
        let (bips, recipient) = match bribe {
            Bribe::None => (0u64, 0u64),
            Bribe::Some {
                bips,
                recipient_idx,
            } => (u64::from(bips), u64::from(recipient_idx)),
        };
        let mut shifts = std::collections::HashMap::new();
        for (name, bits, _, _) in self.layout {
            shifts.insert(*name, bits[0]);
        }
        let field = |name: &str, value: u64| U256::from(value) << shifts[name];
        field("profit_mode", profit_mode)
            | field("bribe_bips", bips)
            | field("bribe_recipient_idx", recipient)
    }
}

impl ReleaseEncoding for TestEncoding {
    fn release_id(&self) -> &str {
        self.tag
    }

    fn encode(
        &self,
        context: &EncodeContext,
        request: &EncodeRequest,
    ) -> Result<degenbot_executor::release_registry::ReleaseCall, ComposeError> {
        let Some(commands) = encode_cmd_stream(context, request) else {
            return Err(ComposeError::unsupported(
                "the command grammar has no producer for this path",
            ));
        };
        let mut data = Vec::with_capacity(4 + 64 + commands.len());
        data.extend_from_slice(&self.selector);
        data.extend_from_slice(&self.config(request.opts).to_be_bytes::<32>());
        data.extend_from_slice(
            &U256::from(u64::try_from(commands.len()).expect("fits")).to_be_bytes::<32>(),
        );
        data.extend_from_slice(&commands);
        Ok(degenbot_executor::release_registry::ReleaseCall {
            data: Bytes::from(data),
            value: U256::ZERO,
            requires: required_capabilities(&request.path, request.opts),
        })
    }

    fn decode_receipt(&self, return_data: Option<&Bytes>) -> Result<ExecutionReceipt, AssessError> {
        let data = return_data.ok_or_else(|| {
            AssessError::evidence("the execute call's return data was not captured")
        })?;
        let expected = 32 * self.receipt_words;
        if data.len() != expected {
            return Err(AssessError::decode(format!(
                "release {} declares {expected} receipt bytes, the call returned {}",
                self.tag,
                data.len()
            )));
        }
        let mut gross = [0u8; 32];
        gross.copy_from_slice(&data[..32]);
        let bribe = if self.receipt_words >= 2 {
            let mut bribe = [0u8; 32];
            bribe.copy_from_slice(&data[32..64]);
            U256::from_be_bytes(bribe)
        } else {
            U256::ZERO
        };
        Ok(ExecutionReceipt {
            gross_profit: U256::from_be_bytes(gross),
            bribe_paid: bribe,
        })
    }
}

/// Consecutive ABI words — a receipt's RETURN DATA (not its code).
fn abi_words(words: &[U256]) -> Bytes {
    let mut out = Vec::with_capacity(32 * words.len());
    for value in words {
        out.extend_from_slice(&value.to_be_bytes::<32>());
    }
    Bytes::from(out)
}

/// A runtime stub that RETURNS `words` as consecutive ABI words.
fn receipt_stub(words: &[U256]) -> Bytes {
    let mut code = Vec::with_capacity(6 + 35 * words.len());
    for (index, value) in words.iter().enumerate() {
        code.push(0x7f);
        code.extend_from_slice(&value.to_be_bytes::<32>());
        code.push(0x60);
        code.push(u8::try_from(index * 32).expect("fits"));
        code.push(0x52);
    }
    code.push(0x60);
    code.push(u8::try_from(words.len() * 32).expect("fits"));
    code.push(0x60);
    code.push(0x00);
    code.push(0xf3);
    Bytes::from(code)
}

fn production_release() -> Release {
    Release::new(
        committed_manifest(),
        FIXTURE_MAJORS,
        Arc::new(CmdStreamV1Encoding),
        None,
    )
    .expect("the committed release pairs with the production encoding")
}

fn release_2(runtime: Bytes) -> Release {
    let encoding = Arc::new(TestEncoding {
        tag: TAG_2,
        selector: [0x36, 0x8c, 0x4e, 0x9c],
        layout: LAYOUT_2,
        receipt_words: 1,
    });
    Release::new(
        alternate_manifest(
            TAG_2,
            2,
            "execute2",
            LAYOUT_2,
            &["profit"],
            Some(capability::ERC6909_CLAIMS.get()),
        ),
        FIXTURE_MAJORS,
        encoding,
        Some(runtime),
    )
    .expect("the injected major-2 release validates")
}

fn release_3(runtime: Bytes) -> Release {
    let digest = keccak256(b"execute3(bytes,uint256)");
    let encoding = Arc::new(TestEncoding {
        tag: TAG_3,
        selector: [digest[0], digest[1], digest[2], digest[3]],
        layout: LAYOUT_3,
        receipt_words: 3,
    });
    Release::new(
        alternate_manifest(
            TAG_3,
            3,
            "execute3",
            LAYOUT_3,
            &["gross_profit", "bribe_paid", "fee_paid"],
            None,
        ),
        FIXTURE_MAJORS,
        encoding,
        Some(runtime),
    )
    .expect("the injected major-3 release validates")
}

fn registry_all() -> ReleaseRegistry {
    ReleaseRegistry::with_releases(vec![
        production_release(),
        release_2(receipt_stub(&[U256::from(GROSS_2)])),
        release_3(receipt_stub(&[U256::from(GROSS_3), U256::ZERO, U256::ZERO])),
    ])
    .expect("three distinct release tags compose a valid registry")
}

fn registry_production() -> ReleaseRegistry {
    ReleaseRegistry::production().expect("production ships the validated v1 release")
}

// ── paths and candidates ─────────────────────────────────────────────────

fn two_v2_hops() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(degenbot_execution::V2HopInfo {
            pool_address: address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token0_address: WETH,
            token1_address: USDC,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(degenbot_execution::V2HopInfo {
            pool_address: address!("0xcccccccccccccccccccccccccccccccccccccccc"),
            token0_address: USDC,
            token1_address: WETH,
            fee: 30,
            zfo: true,
        }),
    ])
}

fn v4_hop(currency0: Address, currency1: Address) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address: POOL_MANAGER,
        pool_id_hex: "0x0".to_owned(),
        currency0_address: currency0,
        currency1_address: currency1,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true,
    })
}

/// A WETH-terminal pure-V4 path — the shape whose ERC6909 capture needs the
/// `erc6909_claims` capability.
fn v4v4_weth_terminal() -> PathInfo {
    PathInfo::new(vec![v4_hop(WETH, USDC), v4_hop(USDC, WETH)])
}

fn combo_inputs<'a>(outputs: &'a [u128], consumed: &'a [u128]) -> ComposerInputs<'a> {
    ComposerInputs {
        optimal_input: OPTIMAL_INPUT,
        hop_outputs: outputs,
        consumed_inputs: consumed,
        opts: degenbot_execution::ComposeOptions,
    }
}

/// The exact call an injected release encodes:
/// `selector || config word || commands length word || commands`.
fn test_release_call(selector: [u8; 4], config: U256, commands: &[u8]) -> Vec<u8> {
    let mut data = selector.to_vec();
    data.extend_from_slice(&config.to_be_bytes::<32>());
    data.extend_from_slice(
        &U256::from(u64::try_from(commands.len()).expect("fits")).to_be_bytes::<32>(),
    );
    data.extend_from_slice(commands);
    data
}

/// The command stream the shared grammar produces for one executor's context.
fn commands_for(executor: Address, path: &PathInfo, outputs: &[u128]) -> Vec<u8> {
    let request = EncodeRequest::new(
        path.clone(),
        OPTIMAL_INPUT,
        outputs.to_vec(),
        outputs.iter().map(|output| output - 1).collect(),
        EncodeOptions::default(),
    );
    encode_cmd_stream(&EncodeContext::new(executor, POOL_MANAGER, WETH), &request)
        .expect("the fixture's paths encode")
}

fn candidate(
    path: PathInfo,
    deployment: DeploymentId,
    outputs: &[u128],
) -> degenbot_arbitrage::DispatchCandidate {
    degenbot_arbitrage::DispatchCandidate {
        candidate_id: CandidateId::new(7, deployment),
        optimal_input: OPTIMAL_INPUT,
        engine_profit: 1,
        steps: outputs
            .iter()
            .map(|output| degenbot_arbitrage::SolveStep {
                output: *output,
                consumed_input: *output - 1,
                state_nonce: 0,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        solve_block: BLOCK,
        path_info: path,
        prepared: None,
    }
}

// ── binding: the validated path ───────────────────────────────────────────

#[test]
fn a_declaring_deployment_binds_to_the_release_the_registry_validates() {
    let registry = registry_production();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("a faithful v1 deployment binds");

    assert_eq!(bound.release().release_id(), RELEASE_ID_TAG);
    assert_eq!(bound.release().protocol_major(), 1);
    assert_eq!(bound.release().call_encoding_version(), 1);
    assert!(matches!(bound.source(), ReleaseSource::Declared(_)));
    assert_eq!(bound.binding().executor(), EXECUTOR_V1);
}

#[test]
fn an_unknown_release_tag_is_refused_not_defaulted_to_the_newest_encoder() {
    let registry = registry_production();
    let answers = GetterAnswers::from_parts(
        Some(tag_word("cmd_executor/7")),
        Some(word(1)),
        Some(word(1)),
        Some(commitment_word(
            committed_manifest().capability_commitment(),
        )),
    );

    let error = BoundCmdExecutor::bind(binding(EXECUTOR_V1), &answers, None, &registry)
        .expect_err("an unregistered tag cannot bind");
    match error {
        BindingError::UnknownRelease { tag, registered } => {
            assert_eq!(tag, "cmd_executor/7");
            assert_eq!(registered, vec![RELEASE_ID_TAG.to_owned()]);
        }
        other => panic!("expected UnknownRelease, got {other:?}"),
    }
}

#[test]
fn a_getterless_deployment_needs_the_operators_legacy_mapping() {
    let registry = registry_production();
    let silent = GetterAnswers::all_absent();

    assert_eq!(
        BoundCmdExecutor::bind(binding(EXECUTOR_V1), &silent, None, &registry)
            .expect_err("silence names nothing"),
        BindingError::Unknown {
            address: EXECUTOR_V1
        }
    );

    let mapped = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
        address: EXECUTOR_V1,
        release: ReleaseId::from_ascii_tag(RELEASE_ID_TAG).expect("valid tag"),
    }]);
    let bound = BoundCmdExecutor::bind(binding(EXECUTOR_V1), &silent, Some(&mapped), &registry)
        .expect("the operator's mapping names the getterless deployment");
    assert_eq!(
        bound.source(),
        &ReleaseSource::Legacy {
            release: ReleaseId::from_ascii_tag(RELEASE_ID_TAG).expect("valid tag")
        }
    );

    // A mapping naming a release this build does not dispatch is still refused.
    let unknown = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
        address: EXECUTOR_V1,
        release: ReleaseId::from_ascii_tag("cmd_executor/0").expect("valid tag"),
    }]);
    assert!(matches!(
        BoundCmdExecutor::bind(binding(EXECUTOR_V1), &silent, Some(&unknown), &registry),
        Err(BindingError::UnknownRelease { .. })
    ));
}

#[test]
fn partial_metadata_is_neither_declared_nor_getterless() {
    let registry = registry_production();
    let full = declared_answers(&committed_manifest());
    // Drop the capability getter's answer: the pre-cutover deployment's shape.
    let partial = GetterAnswers::from_parts(
        Some(tag_word(RELEASE_ID_TAG)),
        Some(word(1)),
        Some(word(1)),
        None,
    );
    assert_eq!(partial.answered_count(), 3);
    let mapped = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
        address: EXECUTOR_V1,
        release: ReleaseId::from_ascii_tag(RELEASE_ID_TAG).expect("valid tag"),
    }]);

    assert_eq!(
        BoundCmdExecutor::bind(binding(EXECUTOR_V1), &partial, Some(&mapped), &registry)
            .expect_err("a half-declaring deployment cannot bind"),
        BindingError::Decode(MetadataDecodeError::PartialAnswers {
            answered: 3,
            absent: vec!["capabilities_hash"],
        })
    );
    assert!(BoundCmdExecutor::bind(binding(EXECUTOR_V1), &full, None, &registry).is_ok());
}

#[test]
fn a_declaration_that_disagrees_with_its_manifest_is_refused() {
    let registry = registry_all();

    // The current tag claiming an encoding version it does not have.
    let wrong_version = GetterAnswers::from_parts(
        Some(tag_word(RELEASE_ID_TAG)),
        Some(word(1)),
        Some(word(9)),
        Some(commitment_word(
            committed_manifest().capability_commitment(),
        )),
    );
    assert_eq!(
        BoundCmdExecutor::bind(binding(EXECUTOR_V1), &wrong_version, None, &registry)
            .expect_err("the encoding version is part of the declaration"),
        BindingError::Mismatch {
            address: EXECUTOR_V1,
            release: RELEASE_ID_TAG.to_owned(),
            mismatch: Box::new(ReleaseMismatch::CallEncodingVersion {
                declared: 1,
                observed: 9
            })
        }
    );

    // A superset declaration — the release's own list plus one extra ID — is a
    // commitment the manifest does not derive.
    let mut superset: Vec<u32> = committed_manifest().capability_ids().collect();
    superset.push(4242);
    let mut subset: Vec<u32> = committed_manifest().capability_ids().collect();
    subset.pop();
    for ids in [superset, subset] {
        let answers = GetterAnswers::from_parts(
            Some(tag_word(RELEASE_ID_TAG)),
            Some(word(1)),
            Some(word(1)),
            Some(commitment_of(&ids)),
        );
        match BoundCmdExecutor::bind(binding(EXECUTOR_V1), &answers, None, &registry)
            .expect_err("a disagreeing ID list cannot bind")
        {
            BindingError::Mismatch {
                mismatch,
                address,
                release,
            } => {
                assert_eq!(address, EXECUTOR_V1);
                assert_eq!(release, RELEASE_ID_TAG);
                match *mismatch {
                    ReleaseMismatch::CapabilityCommitment {
                        release,
                        declared,
                        observed,
                    } => {
                        assert_eq!(release, RELEASE_ID_TAG);
                        assert_eq!(declared, committed_manifest().capability_commitment());
                        assert_eq!(observed, CapabilityCommitment::derive(&ids));
                        assert_ne!(declared, observed);
                    }
                    other => panic!("expected a capability commitment mismatch, got {other:?}"),
                }
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }
}

#[test]
fn a_major_outside_the_registrys_bounded_set_is_refused() {
    // A registry that dispatches majors 2 and 3 only.
    let registry =
        ReleaseRegistry::with_releases(vec![release_2(Bytes::new()), release_3(Bytes::new())])
            .expect("two distinct tags compose a registry");
    assert_eq!(registry.majors(), vec![2, 3]);

    // The deployment names a REGISTERED tag, but a major nobody dispatches.
    let answers = GetterAnswers::from_parts(
        Some(tag_word(TAG_2)),
        Some(word(4)),
        Some(word(2)),
        Some(commitment_word(
            release_2(Bytes::new()).manifest().capability_commitment(),
        )),
    );
    assert_eq!(
        BoundCmdExecutor::bind(binding(EXECUTOR_2), &answers, None, &registry)
            .expect_err("an undispatchable major cannot bind"),
        BindingError::Mismatch {
            address: EXECUTOR_2,
            release: TAG_2.to_owned(),
            mismatch: Box::new(ReleaseMismatch::UnsupportedProtocolMajor {
                observed: 4,
                supported: vec![2, 3]
            })
        }
    );

    // The same registry also refuses a manifest that claims a major it does
    // not dispatch: an injected release cannot escape the bound.
    let mut manifest = alternate_manifest(TAG_2, 2, "execute2", LAYOUT_2, &["profit"], None);
    manifest.protocol.major = 9;
    assert_eq!(
        manifest
            .validate_within(FIXTURE_MAJORS)
            .expect_err("major 9 is outside the fixture's bound"),
        ManifestError::UnsupportedProtocolMajor {
            major: 9,
            supported: FIXTURE_MAJORS.to_vec()
        }
    );
}

#[test]
fn a_registry_refuses_two_releases_that_claim_one_tag_or_a_mismatched_encoding() {
    let duplicated =
        ReleaseRegistry::with_releases(vec![release_2(Bytes::new()), release_2(Bytes::new())]);
    assert_eq!(
        duplicated.expect_err("one tag names one encoding"),
        ReleaseError::DuplicateRelease(TAG_2.to_owned())
    );

    // An encoding that names a different release than its manifest is refused:
    // the two halves are one release.
    let mismatched = Release::new(
        alternate_manifest(TAG_2, 2, "execute2", LAYOUT_2, &["profit"], None),
        FIXTURE_MAJORS,
        Arc::new(CmdStreamV1Encoding),
        None,
    );
    assert_eq!(
        mismatched.expect_err("the encoding implements another tag"),
        ReleaseError::EncodingReleaseId {
            declared: TAG_2.to_owned(),
            encoding: RELEASE_ID_TAG.to_owned()
        }
    );
}

// ── per-release goldens ───────────────────────────────────────────────────

#[test]
fn the_metadata_cutover_moved_no_encoder_byte() {
    // ADR-099 moved the metadata ABI (a new tag, a new getter) and nothing
    // else: the call encoding version, the command stream, the packed config,
    // and the `execute` ABI — receipt included — are byte-identical.
    let registry = registry_production();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("the cutover deployment binds");

    assert_eq!(bound.release().release_id(), RELEASE_ID_TAG);
    assert_eq!(bound.release().protocol_major(), 1, "the wire did not move");
    assert_eq!(
        bound.release().call_encoding_version(),
        1,
        "the wire did not move"
    );

    let path = two_v2_hops();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let consumed = [OPTIMAL_INPUT - 1, OPTIMAL_INPUT - 1];
    let inputs = combo_inputs(&outputs, &consumed);
    let tx = bound
        .adapter()
        .encode(&path, &inputs)
        .expect("the cutover release encodes the path");

    assert_eq!(tx.to, EXECUTOR_V1);
    assert_eq!(tx.value, U256::ZERO);
    assert_eq!(
        format!("0x{}", alloy::hex::encode(&tx.data)),
        CUTOVER_CALL_DATA,
        "the metadata cutover changed no calldata byte"
    );

    // The receipt shape is still the two-word `ExecutionReceipt`.
    let receipt = registry
        .get(RELEASE_ID_TAG)
        .expect("the current release is registered")
        .encoding()
        .decode_receipt(Some(&abi_words(&[
            U256::from(1_000u64),
            U256::from(400u64),
        ])))
        .expect("two words decode");
    assert_eq!(receipt.gross_profit, U256::from(1_000u64));
    assert_eq!(receipt.bribe_paid, U256::from(400u64));
    assert!(registry
        .get(RELEASE_ID_TAG)
        .expect("the current release is registered")
        .encoding()
        .decode_receipt(Some(&abi_words(&[U256::from(1_000u64)])))
        .is_err());
}

#[test]
fn each_release_encodes_its_own_exact_call() {
    let registry = registry_all();
    let bound_1 = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("v1 binds");
    let bound_2 = BoundCmdExecutor::bind(
        binding(EXECUTOR_2),
        &declared_answers(&release_2(Bytes::new()).manifest().clone()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");

    let path = two_v2_hops();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let consumed = [OPTIMAL_INPUT - 1, OPTIMAL_INPUT - 1];
    let inputs = combo_inputs(&outputs, &consumed);

    let tx_1 = bound_1
        .adapter()
        .encode(&path, &inputs)
        .expect("v1 encodes the path");
    let tx_2 = bound_2
        .adapter()
        .encode(&path, &inputs)
        .expect("the major-2 release encodes the path");

    // v1: the exact `execute(bytes,uint256)` call, selector pinned.
    let request = EncodeRequest::new(
        path.clone(),
        OPTIMAL_INPUT,
        outputs.to_vec(),
        consumed.to_vec(),
        EncodeOptions::default(),
    );
    let context = EncodeContext::new(EXECUTOR_V1, POOL_MANAGER, WETH);
    let commands = encode_cmd_stream(&context, &request).expect("v1 encodes the stream");
    let config = config_for_options(EncodeOptions::default(), U256::ZERO).expect("config packs");
    let expected_1 = encode_execute_call(EXECUTOR_V1, &commands, config);
    assert_eq!(tx_1.to, EXECUTOR_V1);
    assert_eq!(tx_1.data.to_vec(), expected_1.data);
    assert_eq!(&tx_1.data[..4], &[0xab, 0x58, 0x98, 0xe8], "v1 selector");
    assert_eq!(tx_1.value, U256::ZERO);

    // The major-2 release: its OWN selector, its OWN config word, its OWN
    // length-prefixed stream.
    let expected_2 = test_release_call(
        [0x36, 0x8c, 0x4e, 0x9c],
        U256::from(1u64) << 248u32,
        &commands_for(EXECUTOR_2, &path, &outputs),
    );
    assert_eq!(tx_2.to, EXECUTOR_2);
    assert_eq!(tx_2.data.to_vec(), expected_2);
    assert_eq!(tx_2.value, U256::ZERO);

    assert_ne!(
        tx_1.data, tx_2.data,
        "two releases produce two different complete calls"
    );
    // The config words differ for identical options: the layouts are not the
    // same semantics under a renamed tag.
    assert_ne!(config, U256::from(1u64) << 248);
}

#[test]
fn each_release_decodes_only_its_own_receipt_shape() {
    let registry = registry_all();
    let v1 = registry.get(RELEASE_ID_TAG).expect("v1 is registered");
    let two = registry.get(TAG_2).expect("test release 2 is registered");
    let three = registry.get(TAG_3).expect("test release 3 is registered");

    let two_words = abi_words(&[U256::from(1_000u64), U256::from(400u64)]);
    let one_word = abi_words(&[U256::from(2_000u64)]);
    let three_words = abi_words(&[U256::from(3_000u64), U256::from(100u64), U256::from(7u64)]);

    // v1 reads two words and nothing else.
    let receipt = v1
        .encoding()
        .decode_receipt(Some(&two_words))
        .expect("v1 decodes its two-word receipt");
    assert_eq!(receipt.gross_profit, U256::from(1_000u64));
    assert_eq!(receipt.bribe_paid, U256::from(400u64));
    assert!(v1.encoding().decode_receipt(Some(&one_word)).is_err());
    assert!(v1.encoding().decode_receipt(Some(&three_words)).is_err());

    // Each injected release reads ITS OWN declared shape.
    let receipt = two
        .encoding()
        .decode_receipt(Some(&one_word))
        .expect("release 2 decodes its single-word receipt");
    assert_eq!(receipt.gross_profit, U256::from(2_000u64));
    assert_eq!(receipt.bribe_paid, U256::ZERO);
    assert!(two.encoding().decode_receipt(Some(&two_words)).is_err());

    let receipt = three
        .encoding()
        .decode_receipt(Some(&three_words))
        .expect("release 3 decodes its three-word receipt");
    assert_eq!(receipt.gross_profit, U256::from(3_000u64));
    assert_eq!(receipt.bribe_paid, U256::from(100u64));
    assert!(three.encoding().decode_receipt(Some(&two_words)).is_err());
}

#[test]
fn the_adapter_assesses_with_the_releases_own_receipt_decoder() {
    let registry = registry_all();
    let bound_2 = BoundCmdExecutor::bind(
        binding(EXECUTOR_2),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");

    let observation = ExecutionObservation {
        transaction: TransactionRequest::new(EXECUTOR_2, Bytes::new(), U256::ZERO),
        status: ExecutionStatus::Success,
        gas_used: 100_000,
        base_fee_next: BASE_FEE,
        return_data: Some(abi_words(&[U256::from(GROSS_2)])),
        probe_reads: Vec::new(),
        events: Vec::new(),
    };
    let result = bound_2
        .adapter()
        .assess(&observation)
        .expect("release 2's own receipt decodes");
    assert_eq!(result.gross_profit, U256::from(GROSS_2));

    // The same observation under v1's decoder is a decode failure: the receipt
    // shape is part of the release, not an adapter-wide convention.
    let bound_1 = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("v1 binds");
    assert!(bound_1.adapter().assess(&observation).is_err());
}

// ── artifacts ────────────────────────────────────────────────────────────

#[test]
fn a_declared_build_installs_the_releases_own_artifact() {
    let registry = registry_all();
    let runtime_2 = receipt_stub(&[U256::from(GROSS_2)]);
    let bound_2 = BoundCmdExecutor::bind(
        binding(EXECUTOR_2),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");

    let deployment = bound_2
        .injected(deployment_id_2())
        .expect("the release's own artifact installs");
    let code = deployment.code().expect("a declared build declares code");
    assert_eq!(code.address, EXECUTOR_2, "at the deployment's own executor");
    assert_eq!(code.runtime, runtime_2);
    assert_eq!(deployment.target(), EXECUTOR_2);
    assert_eq!(
        deployment.fee_sizing(),
        degenbot_arbitrage::FeeSizing::MarketAware
    );

    // An explicit runtime that is another release's artifact is refused.
    let other = receipt_stub(&[U256::from(GROSS_3), U256::ZERO, U256::ZERO]);
    match bound_2
        .injected_with_runtime(deployment_id_2(), other)
        .expect_err("a foreign artifact cannot stand in for the release's own")
    {
        degenbot_arbitrage::BoundRuntimeError::Mismatch {
            release,
            provided_len,
            expected_len,
            ..
        } => {
            assert_eq!(release, TAG_2);
            assert_ne!(provided_len, expected_len);
        }
        other @ degenbot_arbitrage::BoundRuntimeError::Artifact(_) => {
            panic!("expected an artifact mismatch, got {other}")
        }
    }

    // The release's own bytes are accepted explicitly.
    assert!(bound_2
        .injected_with_runtime(deployment_id_2(), runtime_2)
        .is_ok());
}

#[test]
fn an_artifact_from_another_release_is_refused_for_injection() {
    // A matching metadata declaration must never smuggle different code in for
    // simulation: the injected build is the release's OWN artifact or nothing.
    let registry = registry_all();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("the current release binds");

    let other_release_runtime = receipt_stub(&[U256::from(GROSS_2)]);
    match bound
        .injected_with_runtime(deployment_id_v1(), other_release_runtime)
        .expect_err("another release's artifact cannot stand in")
    {
        degenbot_arbitrage::BoundRuntimeError::Mismatch { release, .. } => {
            assert_eq!(release, RELEASE_ID_TAG);
        }
        other @ degenbot_arbitrage::BoundRuntimeError::Artifact(_) => {
            panic!("expected an artifact mismatch, got {other}")
        }
    }
}

#[test]
fn the_production_release_loads_its_committed_artifact() {
    let registry = registry_production();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("v1 binds");

    let runtime = bound
        .release_runtime()
        .expect("the committed v1 runtime artifact loads from the checkout");
    assert!(
        runtime.len() > 1_000,
        "the deployed runtime is a real contract ({} bytes)",
        runtime.len()
    );
    assert_eq!(
        bound.release().runtime_artifact_path(),
        "tier3-oracle/artifacts/executor/cmd_executor.runtime.hex",
        "the manifest names the compiler artifact the tier3 gate verifies"
    );
    // Any other bytes are refused, including a valid contract's.
    assert!(bound
        .injected_with_runtime(deployment_id_v1(), Bytes::from_static(&[0x60, 0x00]))
        .is_err());
}

// ── capability planning ──────────────────────────────────────────────────

#[test]
fn a_missing_capability_declines_only_the_release_that_lacks_it() {
    let registry = registry_all();
    let opts = EncodeOptions {
        erc6909_profit: true,
        ..EncodeOptions::default()
    };

    // A pure-V4 path whose capture needs `erc6909_claims`.
    let path = v4v4_weth_terminal();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT + OPTIMAL_INPUT / 5];
    let consumed = [OPTIMAL_INPUT - 1, OPTIMAL_INPUT - 1];
    let inputs = combo_inputs(&outputs, &consumed);

    // Release 2 does not declare `erc6909_claims` at all.
    let bound_2 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_2, opts),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("binding does not need the capability; planning does");
    assert_eq!(
        bound_2
            .release()
            .missing_capabilities(&[capability::ERC6909_CLAIMS]),
        vec![capability::ERC6909_CLAIMS]
    );
    match bound_2.adapter().encode(&path, &inputs) {
        Err(ComposeError::Unsupported(detail)) => {
            assert!(detail.contains("erc6909_claims"), "{detail}");
            assert!(detail.contains(TAG_2), "{detail}");
        }
        other => panic!("expected a capability decline, got {other:?}"),
    }

    // The major-3 release declares it, so the same path encodes there.
    let bound_3 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_3, opts),
        &declared_answers(release_3(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-3 deployment binds");
    assert!(bound_3
        .release()
        .missing_capabilities(&[capability::ERC6909_CLAIMS])
        .is_empty());
    let tx = bound_3
        .adapter()
        .encode(&path, &inputs)
        .expect("release 3 declares the capability the path needs");
    assert_eq!(tx.to, EXECUTOR_3);

    // And production v1 declares it too (the current release loses nothing).
    let bound_1 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_V1, opts),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("v1 binds");
    assert!(bound_1.adapter().encode(&path, &inputs).is_ok());
}

#[test]
fn required_capabilities_follow_the_encoder_axes() {
    let v2_path = two_v2_hops();
    assert_eq!(
        required_capabilities(&v2_path, EncodeOptions::default()),
        vec![
            capability::ADDRESS_TABLE,
            capability::V2_SWAPS,
            capability::EXECUTION_RECEIPT
        ]
    );

    let v4_path = v4v4_weth_terminal();
    let batched = EncodeOptions {
        use_v4_batch: true,
        erc6909_profit: true,
        ..EncodeOptions::default()
    };
    assert_eq!(
        required_capabilities(&v4_path, batched),
        vec![
            capability::ADDRESS_TABLE,
            capability::V4_SWAPS,
            capability::V4_BATCH,
            capability::ERC6909_CLAIMS,
            capability::EXECUTION_RECEIPT
        ]
    );

    // A mixed path never claims the pure-V4 batch capability.
    let mixed = PathInfo::new(vec![two_v2_hops().hops[0].clone(), v4_hop(USDC, WETH)]);
    assert!(!required_capabilities(&mixed, batched).contains(&capability::V4_BATCH));

    // A bribe needs the config capability.
    let bribed = EncodeOptions {
        bribe: Bribe::Some {
            bips: 500,
            recipient_idx: 0,
        },
        ..EncodeOptions::default()
    };
    assert!(required_capabilities(&v2_path, bribed).contains(&capability::CONFIG_BRIBE));
}

// ── discovery over RPC: absence vs node failure ───────────────────────────

/// One configurable `eth_call` behaviour for the metadata getters.
#[derive(Clone, Copy)]
enum CallBehaviour {
    /// Answer every getter with `data`.
    Answer(&'static [u8]),
    /// Revert every getter (`execution reverted`).
    Revert,
    /// Fail every getter with a node-level error.
    NodeFailure,
}

#[derive(Clone, Copy)]
struct MetadataTransport(CallBehaviour);

fn success(value: &str) -> ResponsePayload {
    let raw: Box<RawValue> =
        to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
    ResponsePayload::Success(raw)
}

impl tower::Service<RequestPacket> for MetadataTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let behaviour = self.0;
        Box::pin(async move {
            let respond = |req: &SerializedRequest| -> Response {
                let payload = match req.method() {
                    "eth_call" => match behaviour {
                        CallBehaviour::Answer(data) => {
                            success(&format!("0x{}", alloy::hex::encode(data)))
                        }
                        CallBehaviour::Revert => ResponsePayload::Failure(ErrorPayload {
                            code: -32000,
                            message: "execution reverted".into(),
                            data: None,
                        }),
                        CallBehaviour::NodeFailure => ResponsePayload::Failure(ErrorPayload {
                            code: -32601,
                            message: "method not found: eth_call".into(),
                            data: None,
                        }),
                    },
                    "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                    "eth_getCode" => success("0x"),
                    "eth_getStorageAt" => success(&format!("0x{:064x}", 0)),
                    _ => ResponsePayload::Failure(ErrorPayload {
                        code: -32601,
                        message: format!("method not found: {}", req.method()).into(),
                        data: None,
                    }),
                };
                Response {
                    id: req.id().clone(),
                    payload,
                }
            };
            Ok(match req {
                RequestPacket::Single(r) => ResponsePacket::Single(respond(&r)),
                RequestPacket::Batch(rs) => ResponsePacket::Batch(rs.iter().map(respond).collect()),
            })
        })
    }
}

fn provider(behaviour: CallBehaviour) -> AlloyProvider {
    let client = RpcClient::new(MetadataTransport(behaviour), true);
    let root: RootProvider = RootProvider::new(client);
    AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    )
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    runtime.block_on(future)
}

#[test]
fn a_reverting_getter_is_an_absent_getter_and_a_node_failure_is_not() {
    let registry = registry_production();

    // Every getter reverts: a getterless deployment, which nobody named.
    let reverting = provider(CallBehaviour::Revert);
    assert_eq!(
        block_on(BoundCmdExecutor::discover(
            binding(EXECUTOR_V1),
            &reverting,
            None,
            &registry
        ))
        .expect_err("silence names nothing"),
        BindingError::Unknown {
            address: EXECUTOR_V1
        }
    );

    // Every getter returns no data: the same classification.
    let empty = provider(CallBehaviour::Answer(&[]));
    assert!(matches!(
        block_on(BoundCmdExecutor::discover(
            binding(EXECUTOR_V1),
            &empty,
            None,
            &registry
        )),
        Err(BindingError::Unknown { .. })
    ));

    // A node-level failure proves nothing about the deployment: it is an
    // error, never an absent getter — otherwise a broken endpoint would push
    // a declaring deployment into the legacy path.
    let failing = provider(CallBehaviour::NodeFailure);
    match block_on(BoundCmdExecutor::discover(
        binding(EXECUTOR_V1),
        &failing,
        None,
        &registry,
    ))
    .expect_err("a node failure is not an absence")
    {
        BindingError::Node { getter, detail } => {
            assert_eq!(getter, METADATA_GETTERS[0].name);
            assert!(detail.contains("-32601"), "{detail}");
        }
        other => panic!("expected a node failure, got {other}"),
    }
}

#[test]
fn the_getter_table_carries_the_declared_selectors() {
    for getter in &METADATA_GETTERS {
        let digest = keccak256(getter.signature.as_bytes());
        assert_eq!(
            getter.selector,
            [digest[0], digest[1], digest[2], digest[3]],
            "{} selector",
            getter.name
        );
    }
    assert_eq!(
        classify(getter_name("release_id"), GetterCall::Returned(&[])),
        None
    );
}

/// `classify_getter`, flattened for assertions: `None` is an absent getter.
fn classify<'a>(getter: &'static str, call: GetterCall<'a>) -> Option<&'a [u8]> {
    match degenbot_executor::discovery::classify_getter(getter, call)
        .expect("no node failure in this case")
    {
        GetterOutcome::Answered(data) => Some(data),
        GetterOutcome::Absent => None,
    }
}

fn getter_name(name: &str) -> &'static str {
    METADATA_GETTERS
        .iter()
        .find(|getter| getter.name == name)
        .map(|getter| getter.name)
        .expect("the getter table declares every getter")
}

// ── fan-out certification through the generic target interface ────────────

/// The metadata-free foreign contract's adapter: its own ABI, its own gross,
/// and no release, version, or capability metadata anywhere.
#[derive(Clone, Copy)]
struct ForeignAdapter;

impl ExecutionAdapter for ForeignAdapter {
    fn encode(
        &self,
        _path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(&[0xf0, 0x0d, 0xba, 0xbe]);
        data.extend_from_slice(&U256::from(inputs.optimal_input).to_be_bytes::<32>());
        Ok(TransactionRequest::new(
            FOREIGN,
            Bytes::from(data),
            U256::ZERO,
        ))
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        static NO_PROBES: ProbeSpecs = Vec::new();
        &NO_PROBES
    }

    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        Ok(ExecutionResult::price(
            U256::from(GROSS_FOREIGN),
            Vec::new(),
            observation,
            U256::ZERO,
            &FeePolicy {
                market_percentile: false,
                override_priority_fee: 0,
            },
        ))
    }
}

fn foreign_deployment() -> Arc<SimulationDeployment> {
    Arc::new(SimulationDeployment::new(
        deployment_id_foreign(),
        Arc::new(ForeignAdapter),
        OWNER,
        FOREIGN,
        GAS_LIMIT,
        None,
        None,
    ))
}

fn simulation_block(provider: &AlloyProvider) -> SimulationBlock<'_> {
    SimulationBlock {
        provider,
        base_fee_next: BASE_FEE,
        current_block: BLOCK,
        block_timestamp: 0,
        block_priority_fees: None,
    }
}

fn run_fanout(
    registry: &DeploymentRegistry,
    selection: TargetSelection,
    candidates: Vec<degenbot_arbitrage::DispatchCandidate>,
) -> degenbot_arbitrage::DeploymentFanOutOutcome {
    let provider = provider(CallBehaviour::Answer(&[]));
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let block = simulation_block(&provider);
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
                block_on(async {
                    dispatch_across_deployments(
                        candidates,
                        &block,
                        registry,
                        selection,
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        Some(&bot_state),
                        Some(&warm_cache),
                    )
                })
            })
            .join()
            .expect("fan-out thread")
    })
    .expect("every candidate names a registered deployment")
}

#[test]
fn a_metadata_free_foreign_contract_still_binds_and_encodes() {
    // The generic-seam exclusion proof: a foreign contract exposes no
    // `release_id()`, `protocol_major()`, `call_encoding_version()`,
    // `capabilities_hash()`, WETH, or PoolManager, takes part in no manifest,
    // no commitment, and no release registry, and still encodes through the
    // same production path it always did.
    let deployment = foreign_deployment();
    let path = two_v2_hops();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let consumed = [OPTIMAL_INPUT - 1, OPTIMAL_INPUT - 1];
    let inputs = combo_inputs(&outputs, &consumed);

    let tx = deployment
        .adapter()
        .encode(&path, &inputs)
        .expect("the foreign contract encodes its own call");
    assert_eq!(tx.to, FOREIGN);
    assert_eq!(&tx.data[..4], &[0xf0, 0x0d, 0xba, 0xbe]);
    assert_eq!(
        tx.data.len(),
        4 + 32,
        "the foreign call is not a cmd_executor call"
    );
    assert_eq!(tx.value, U256::ZERO);
    assert_eq!(deployment.target(), FOREIGN);

    // Its silence is a classification, not a release: no registry entry names
    // it, and no capability policy reaches it.
    assert_eq!(
        degenbot_executor::release::classify_deployment(
            FOREIGN,
            GetterAnswers::all_absent().observations(),
            None,
        )
        .expect("a getterless contract classifies"),
        degenbot_executor::release::DeploymentRelease::Unknown
    );
}

/// Two `cmd_executor` releases and one metadata-free foreign contract, one
/// solved path: each target selects its OWN encoder, artifact, and receipt
/// through the same generic interface, and no target's outcome touches
/// another's.
#[test]
fn heterogeneous_releases_select_their_own_encodings_beside_a_foreign_target() {
    let registry = registry_all();
    let bound_2 = BoundCmdExecutor::bind(
        binding(EXECUTOR_2),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");
    let bound_3 = BoundCmdExecutor::bind(
        binding(EXECUTOR_3),
        &declared_answers(release_3(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-3 deployment binds");

    // Two deployments on two releases keep distinct identities even though both
    // are `cmd_executor` contracts.
    let deployments = DeploymentRegistry::new(vec![
        Arc::new(
            bound_2
                .injected(deployment_id_2())
                .expect("release 2 artifact"),
        ),
        Arc::new(
            bound_3
                .injected(deployment_id_3())
                .expect("release 3 artifact"),
        ),
        foreign_deployment(),
    ])
    .expect("three distinct identities compose a valid registry");

    let path = two_v2_hops();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let candidates = vec![
        candidate(path.clone(), deployment_id_2(), &outputs),
        candidate(path.clone(), deployment_id_3(), &outputs),
        candidate(path.clone(), deployment_id_foreign(), &outputs),
    ];
    let outcome = run_fanout(&deployments, TargetSelection::AllTargets, candidates);

    assert_eq!(outcome.lanes.len(), 3, "one lane per registered target");

    let lane_2 = outcome.lane(deployment_id_2()).expect("release 2 lane");
    let lane_3 = outcome.lane(deployment_id_3()).expect("release 3 lane");
    let lane_foreign = outcome.lane(deployment_id_foreign()).expect("foreign lane");

    assert_eq!(lane_2.gas_profitable.len(), 1, "release 2 executed");
    assert_eq!(lane_3.gas_profitable.len(), 1, "release 3 executed");
    assert_eq!(
        lane_foreign.gas_profitable.len(),
        1,
        "the foreign target executed"
    );

    let row_2 = &lane_2.gas_profitable[0];
    let row_3 = &lane_3.gas_profitable[0];
    let row_foreign = &lane_foreign.gas_profitable[0];

    // Each lane's gross is ITS release's receipt shape, decoded by ITS release.
    assert_eq!(row_2.gross_profit, U256::from(GROSS_2));
    assert_eq!(row_3.gross_profit, U256::from(GROSS_3));
    assert_eq!(row_foreign.gross_profit, U256::from(GROSS_FOREIGN));

    // Each lane's calldata is its own release's complete call.
    let digest = keccak256(b"execute3(bytes,uint256)");
    let expected_2 = test_release_call(
        [0x36, 0x8c, 0x4e, 0x9c],
        U256::from(1u64) << 248u32,
        &commands_for(EXECUTOR_2, &path, &outputs),
    );
    let expected_3 = test_release_call(
        [digest[0], digest[1], digest[2], digest[3]],
        U256::from(1u64) << 24u32,
        &commands_for(EXECUTOR_3, &path, &outputs),
    );

    assert_eq!(row_2.execute_calldata.to_vec(), expected_2);
    assert_eq!(row_3.execute_calldata.to_vec(), expected_3);
    assert_eq!(
        &row_foreign.execute_calldata[..4],
        &[0xf0, 0x0d, 0xba, 0xbe]
    );
    assert_eq!(row_2.value, U256::ZERO);
    assert_eq!(row_3.value, U256::ZERO);

    // Three targets, three transactions, one generic interface.
    assert_ne!(row_2.execute_calldata, row_3.execute_calldata);
    assert_ne!(row_2.candidate_id, row_3.candidate_id);
    assert_eq!(row_2.candidate_id, CandidateId::new(7, deployment_id_2()));

    // Each deployment's target is its own contract.
    assert_eq!(
        deployments
            .get(deployment_id_2())
            .expect("registered")
            .target(),
        EXECUTOR_2
    );
    assert_eq!(
        deployments
            .get(deployment_id_3())
            .expect("registered")
            .target(),
        EXECUTOR_3
    );
    assert_eq!(
        deployments
            .get(deployment_id_foreign())
            .expect("registered")
            .target(),
        FOREIGN
    );
}

/// A capability refusal at one release is a routine decline for THAT
/// candidate: under the first-compatible walk the path still executes at the
/// next release that declares the capability.
#[test]
fn a_capability_refusal_at_one_release_does_not_suppress_another() {
    let registry = registry_all();
    let opts = EncodeOptions {
        erc6909_profit: true,
        ..EncodeOptions::default()
    };

    // Release 2 leads the registry and cannot run an ERC6909 capture.
    let bound_2 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_2, opts),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");
    let bound_3 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_3, opts),
        &declared_answers(release_3(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-3 deployment binds");

    let deployments = DeploymentRegistry::new(vec![
        Arc::new(
            bound_2
                .injected(deployment_id_2())
                .expect("release 2 artifact"),
        ),
        Arc::new(
            bound_3
                .injected(deployment_id_3())
                .expect("release 3 artifact"),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let path = v4v4_weth_terminal();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT + OPTIMAL_INPUT / 5];
    let candidates = vec![
        candidate(path.clone(), deployment_id_2(), &outputs),
        candidate(path.clone(), deployment_id_3(), &outputs),
    ];

    // The first-compatible walk declines at release 2 and executes at release 3.
    let outcome = run_fanout(&deployments, TargetSelection::FirstCompatible, candidates);
    assert_eq!(
        outcome
            .lane(deployment_id_2())
            .expect("release 2 lane")
            .gas_profitable
            .len(),
        0,
        "the release that lacks the capability produced nothing"
    );
    assert_eq!(
        outcome
            .lane(deployment_id_3())
            .expect("release 3 lane")
            .gas_profitable
            .len(),
        1,
        "the release that declares the capability executed the path"
    );
}

/// The `AllTargets` fan-out keeps BOTH lanes: the declining release records its
/// own decline, and the accepting one still executes.
#[test]
fn an_all_targets_fanout_keeps_the_declining_lane_isolated() {
    let registry = registry_all();
    let opts = EncodeOptions {
        erc6909_profit: true,
        ..EncodeOptions::default()
    };
    let bound_2 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_2, opts),
        &declared_answers(release_2(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-2 deployment binds");
    let bound_3 = BoundCmdExecutor::bind(
        binding_with(EXECUTOR_3, opts),
        &declared_answers(release_3(Bytes::new()).manifest()),
        None,
        &registry,
    )
    .expect("the major-3 deployment binds");

    let deployments = DeploymentRegistry::new(vec![
        Arc::new(
            bound_2
                .injected(deployment_id_2())
                .expect("release 2 artifact"),
        ),
        Arc::new(
            bound_3
                .injected(deployment_id_3())
                .expect("release 3 artifact"),
        ),
    ])
    .expect("two distinct identities compose a valid registry");

    let path = v4v4_weth_terminal();
    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT + OPTIMAL_INPUT / 5];
    let outcome = run_fanout(
        &deployments,
        TargetSelection::AllTargets,
        vec![
            candidate(path.clone(), deployment_id_2(), &outputs),
            candidate(path.clone(), deployment_id_3(), &outputs),
        ],
    );

    let lane_2 = outcome.lane(deployment_id_2()).expect("release 2 lane");
    let lane_3 = outcome.lane(deployment_id_3()).expect("release 3 lane");
    assert_eq!(
        lane_2.fail_buckets.buckets().get("encode-failed").copied(),
        Some(1),
        "the capability decline is release 2's own encode-failed bucket"
    );
    assert!(
        lane_3.fail_buckets.buckets().is_empty(),
        "release 3's lane carries no failure: {:?}",
        lane_3.fail_buckets.buckets()
    );
    assert_eq!(lane_3.gas_profitable.len(), 1);
}

/// The single-target path is unchanged: one production deployment, one
/// candidate, exactly as before the registry bound it to a validated release.
#[test]
fn a_single_bound_deployment_keeps_the_single_target_behavior() {
    let registry = registry_production();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("v1 binds");
    let deployment = Arc::new(
        bound
            .injected_with_runtime(
                deployment_id_v1(),
                bound.release_runtime().expect("artifact"),
            )
            .expect("the release's own artifact installs"),
    );
    let deployments = match DeploymentRegistry::new(vec![deployment]) {
        Ok(registry) => registry,
        Err(error) => panic!("one deployment is a valid registry: {error}"),
    };

    let path = two_v2_hops();
    let inputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let outcome = run_fanout(
        &deployments,
        TargetSelection::FirstCompatible,
        vec![candidate(path, deployment_id_v1(), &inputs)],
    );

    assert_eq!(outcome.lanes.len(), 1);
    assert!(outcome.retired.is_empty());
    // The production release's real runtime executes the fixture's command
    // stream against an empty block: it does not revert the fan-out, and the
    // lane reports its own verdict (a real contract with no balances measured a
    // zero increase, so its receipt declines the opportunity).
    let lane = &outcome.lanes[0].outcome;
    assert_eq!(lane.candidate_count, 1);
    assert_eq!(lane.exception_count, 0);
    assert!(lane.gas_profitable.is_empty());
}
