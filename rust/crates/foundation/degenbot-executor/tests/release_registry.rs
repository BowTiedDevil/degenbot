#![expect(
    clippy::expect_used,
    reason = "the fixture drives known-valid values and asserts on the typed product"
)]

//! The bounded release registry, the capability catalog, and the discovery
//! classification (ADR-098 D5).
//!
//! These are the pure halves of the built-in binding: which releases this build
//! dispatches, what each release declares, and what a failed metadata read
//! means. The RPC half and the per-release goldens live beside the adapter
//! (`degenbot-arbitrage/tests/release_binding.rs`), where a registry binds real
//! deployments.

use std::sync::Arc;

use alloy::primitives::{Bytes, U256};

use degenbot_execution::{ComposeError, PathInfo, V2HopInfo, V4HopInfo};
use degenbot_executor::composers::{resolve_axes, EncodeContext, EncodeOptions, EncodeRequest};
use degenbot_executor::discovery::{classify_getter, GetterAnswers, GetterCall, METADATA_GETTERS};
use degenbot_executor::receipt::ExecutionReceipt;
use degenbot_executor::release::{
    capability, CapabilityId, GetterOutcome, ManifestError, ReleaseManifest, METADATA_GETTER_NAMES,
    RELEASE_ID_TAG, RELEASE_MANIFEST_V2, SUPPORTED_PROTOCOL_MAJORS,
};
use degenbot_executor::release_registry::{
    required_capabilities, CmdStreamV1Encoding, Release, ReleaseCall, ReleaseEncoding,
    ReleaseError, ReleaseRegistry,
};

fn committed_manifest() -> ReleaseManifest {
    ReleaseManifest::parse(RELEASE_MANIFEST_V2).expect("the committed manifest validates")
}

fn production_release() -> Release {
    Release::new(
        committed_manifest(),
        SUPPORTED_PROTOCOL_MAJORS,
        Arc::new(CmdStreamV1Encoding),
        None,
    )
    .expect("the committed release pairs with the production encoding")
}

const ALTERNATE_TAG: &str = "test_executor/2";
const ALTERNATE_MAX_STREAM_LENGTH: u32 = 4;

#[derive(Debug)]
struct AlternateGrammarEncoding;

fn alternate_release() -> Release {
    let mut manifest = committed_manifest();
    ALTERNATE_TAG.clone_into(&mut manifest.release_id);
    manifest.protocol.major = 2;
    manifest.call_encoding.version = 2;
    manifest.call_encoding.max_stream_length = ALTERNATE_MAX_STREAM_LENGTH;
    manifest
        .validate_within(&[1, 2])
        .expect("the alternate grammar manifest validates");
    Release::new(manifest, &[1, 2], Arc::new(AlternateGrammarEncoding), None)
        .expect("the alternate grammar pairs with its manifest")
}

impl ReleaseEncoding for AlternateGrammarEncoding {
    fn release_id(&self) -> &str {
        ALTERNATE_TAG
    }

    fn encode(
        &self,
        _context: &EncodeContext,
        request: &EncodeRequest,
    ) -> Result<ReleaseCall, ComposeError> {
        let mut commands = Vec::with_capacity(request.path.hops.len() * 3);
        for (index, _) in request.path.hops.iter().enumerate() {
            let raw = if index == 0 {
                request.optimal_input
            } else {
                request.hop_outputs[index]
            };
            let amount = u16::try_from(raw).map_err(|_| {
                ComposeError::Encode(format!("alternate u16 operand overflows: {raw}"))
            })?;
            commands.push(0xa5);
            commands.extend_from_slice(&amount.to_le_bytes());
        }
        let length = u32::try_from(commands.len())
            .map_err(|error| ComposeError::Encode(format!("alternate stream length: {error}")))?;
        if length > ALTERNATE_MAX_STREAM_LENGTH {
            return Err(ComposeError::Unsupported(format!(
                "alternate command stream length {length} exceeds 4 bytes"
            )));
        }
        Ok(ReleaseCall {
            data: Bytes::from(commands),
            value: U256::ZERO,
            requires: Vec::new(),
        })
    }

    fn decode_receipt(
        &self,
        return_data: Option<&Bytes>,
    ) -> Result<ExecutionReceipt, degenbot_execution::AssessError> {
        ExecutionReceipt::decode(return_data)
    }
}

#[test]
fn production_ships_exactly_the_validated_release() {
    let registry = ReleaseRegistry::production().expect("production builds");
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.tags().collect::<Vec<_>>(), vec![RELEASE_ID_TAG]);
    assert_eq!(registry.majors(), vec![1]);

    let release = registry
        .get(RELEASE_ID_TAG)
        .expect("the current release is registered");
    assert_eq!(release.protocol_major(), 1);
    assert_eq!(release.call_encoding_version(), 1);
    assert_eq!(release.encoding().release_id(), RELEASE_ID_TAG);
    assert!(release.development_runtime().is_none());
}

#[test]
fn the_capability_catalog_names_every_declared_capability() {
    // The catalog is release-independent (an ID never changes meaning), so the
    // committed manifest's names must agree with it or diagnostics about a
    // MISSING capability would disagree with the release that declares it.
    let manifest = committed_manifest();
    for declared in &manifest.capabilities.ids {
        assert_eq!(
            capability::name(CapabilityId::new(declared.id)),
            declared.name,
            "capability {} drifted from the stable catalog",
            declared.id
        );
    }
    assert_eq!(capability::name(CapabilityId::new(4242)), "unknown");
}

#[test]
fn an_injected_major_enters_only_a_registry_that_declares_it() {
    // A manifest declaring a major production does not dispatch is refused by
    // the production bound... (the tag still matches the production encoding, so
    // the ONLY difference is the major)
    let mut manifest = committed_manifest();
    manifest.protocol.major = 2;
    let unvalidated = manifest.validate();
    assert_eq!(
        unvalidated.expect_err("production is bounded to its own majors"),
        ManifestError::UnsupportedProtocolMajor {
            major: 2,
            supported: SUPPORTED_PROTOCOL_MAJORS.to_vec()
        }
    );

    // ... and accepted by the explicit bound of a registry that dispatches it.
    manifest
        .validate_within(&[1, 2])
        .expect("an explicit bound admits the injected major");
    let injected = Release::new(
        manifest,
        &[1, 2],
        Arc::new(CmdStreamV1Encoding),
        Some(Bytes::from_static(&[0x60, 0x00, 0x60, 0x00, 0xf3])),
    )
    .expect("an injected release validates against its own bound");
    assert_eq!(injected.protocol_major(), 2);
    assert!(injected.development_runtime().is_some());
}

#[test]
fn one_tag_names_one_release() {
    let registry = ReleaseRegistry::with_releases(vec![production_release()]);
    assert_eq!(registry.expect("one release").len(), 1);

    let mut duplicate = committed_manifest();
    duplicate.call_encoding.version = 2;
    let second = Release::new(
        duplicate,
        SUPPORTED_PROTOCOL_MAJORS,
        Arc::new(CmdStreamV1Encoding),
        None,
    )
    .expect("a structurally valid release");
    assert_eq!(
        ReleaseRegistry::with_releases(vec![production_release(), second])
            .expect_err("two claimants make every binding ambiguous"),
        ReleaseError::DuplicateRelease(RELEASE_ID_TAG.to_owned())
    );
}

#[test]
fn the_production_encoding_names_its_own_release() {
    let encoding = CmdStreamV1Encoding;
    assert_eq!(encoding.release_id(), RELEASE_ID_TAG);

    // The receipt shape is the release's, and the strict decode is untouched.
    let two_words = Bytes::from(vec![0u8; 64]);
    assert!(encoding
        .decode_receipt(Some(&two_words))
        .expect("two words decode")
        .retained()
        .is_ok());
    assert!(encoding.decode_receipt(None).is_err());
    assert!(encoding
        .decode_receipt(Some(&Bytes::from(vec![0u8; 32])))
        .is_err());
}

fn v2_hop() -> degenbot_execution::HopInfo {
    v2_hop_at(alloy::primitives::Address::repeat_byte(0x11))
}

fn v2_hop_at(pool_address: alloy::primitives::Address) -> degenbot_execution::HopInfo {
    degenbot_execution::HopInfo::V2(V2HopInfo {
        pool_address,
        token0_address: alloy::primitives::Address::repeat_byte(0x22),
        token1_address: alloy::primitives::Address::repeat_byte(0x33),
        fee: 30,
        zfo: true,
    })
}

fn v4_hop() -> degenbot_execution::HopInfo {
    degenbot_execution::HopInfo::V4(V4HopInfo {
        pool_manager_address: alloy::primitives::Address::repeat_byte(0x44),
        pool_id_hex: "0x0".to_owned(),
        currency0_address: alloy::primitives::Address::repeat_byte(0x22),
        currency1_address: alloy::primitives::Address::repeat_byte(0x33),
        fee: 3000,
        tick_spacing: 60,
        hook_address: alloy::primitives::Address::ZERO,
        zfo: true,
    })
}

#[test]
fn primitive_emission_overflow_is_a_typed_encode_error() {
    let path = PathInfo::new(vec![v4_hop(), v4_hop()]);
    let request = EncodeRequest::new(
        path,
        1u128 << 96,
        vec![1u128 << 96, 1u128 << 96],
        vec![1u128 << 96, 1u128 << 96],
        EncodeOptions::default(),
    );
    let context = EncodeContext::new(
        alloy::primitives::Address::repeat_byte(0x99),
        alloy::primitives::Address::repeat_byte(0x44),
        alloy::primitives::Address::repeat_byte(0x22),
    );

    let error = CmdStreamV1Encoding
        .encode(&context, &request)
        .expect_err("uint96 overflow must be returned, not panic");
    assert!(
        matches!(&error, ComposeError::Encode(message) if message.contains("uint96")),
        "unexpected error: {error}"
    );
}

#[test]
fn a_stream_above_the_release_bound_is_a_per_candidate_decline() {
    let path = PathInfo::new(
        (1..=9)
            .map(|index| v2_hop_at(alloy::primitives::Address::repeat_byte(index)))
            .collect(),
    );
    let request = EncodeRequest::new(
        path,
        1_000,
        vec![1_000; 9],
        vec![1_000; 9],
        EncodeOptions::default(),
    );
    let context = EncodeContext::new(
        alloy::primitives::Address::repeat_byte(0x99),
        alloy::primitives::Address::repeat_byte(0x44),
        alloy::primitives::Address::repeat_byte(0x22),
    );

    let error = CmdStreamV1Encoding
        .encode(&context, &request)
        .expect_err("a 308-byte stream exceeds the release's 288-byte bound");
    assert!(
        matches!(&error, ComposeError::Unsupported(message) if message.contains("288") && message.contains("308")),
        "unexpected error: {error}"
    );
}

#[test]
fn a_capability_the_release_does_not_declare_is_missing() {
    let release = production_release();
    let declared = release.declared_capabilities();
    assert_eq!(
        declared,
        (1..=15).map(CapabilityId::new).collect::<Vec<_>>(),
        "production declares every catalogued capability"
    );
    assert!(release
        .missing_capabilities(&[capability::V4_SWAPS, capability::ERC6909_CLAIMS])
        .is_empty());

    // An ID this build cannot name is still compared: it is preserved, and it
    // is NOT support.
    let unknown = CapabilityId::new(4242);
    assert_eq!(release.missing_capabilities(&[unknown]), vec![unknown]);
    assert_eq!(
        release.describe_capabilities(&[unknown]),
        "unknown (capability:4242)"
    );
    assert_eq!(
        release.describe_capabilities(&[capability::ERC6909_CLAIMS]),
        "erc6909_claims (capability:10)"
    );
}

#[test]
fn the_requirement_set_follows_the_path_and_the_axes() {
    let v2_path = PathInfo::new(vec![v2_hop(), v2_hop()]);
    assert_eq!(
        required_capabilities(&v2_path, EncodeOptions::default()),
        vec![
            capability::ADDRESS_TABLE,
            capability::V2_SWAPS,
            capability::EXECUTION_RECEIPT
        ]
    );

    let v4_path = PathInfo::new(vec![v4_hop(), v4_hop()]);
    let batched = EncodeOptions {
        use_v4_batch: true,
        erc6909_profit: true,
        ..EncodeOptions::default()
    };
    let required = required_capabilities(&v4_path, batched);
    for id in [
        capability::ADDRESS_TABLE,
        capability::V4_SWAPS,
        capability::V4_BATCH,
        capability::ERC6909_CLAIMS,
        capability::EXECUTION_RECEIPT,
    ] {
        assert!(required.contains(&id), "{id} must be required");
    }

    // A mixed path never claims the pure-V4 batch capability.
    let mixed = PathInfo::new(vec![v2_hop(), v4_hop()]);
    assert!(!required_capabilities(&mixed, batched).contains(&capability::V4_BATCH));

    // Resolved axes: the legacy bool forces the ERC6909 capture.
    let (_, capture, _) = resolve_axes(batched);
    assert_eq!(
        capture,
        degenbot_executor::grammar_ledger::ProfitCapture::Erc6909
    );
}

#[test]
fn the_metadata_getter_selectors_match_their_signatures() {
    // The table is what an `eth_call` carries, so each selector must be the
    // first four bytes of keccak256 over the canonical signature — including
    // the cutover's `capabilities_hash()`.
    let declared: Vec<&str> = METADATA_GETTERS.iter().map(|getter| getter.name).collect();
    assert_eq!(declared, METADATA_GETTER_NAMES.to_vec());

    for getter in &METADATA_GETTERS {
        let digest = alloy::primitives::keccak256(getter.signature.as_bytes());
        assert_eq!(
            getter.selector,
            [digest[0], digest[1], digest[2], digest[3]],
            "{} selector",
            getter.name
        );
        assert_eq!(
            getter.signature,
            format!("{}()", getter.name),
            "a getter's signature is its zero-argument form"
        );
    }

    let capability_getter = METADATA_GETTERS
        .iter()
        .find(|getter| getter.name == "capabilities_hash")
        .expect("the capability getter is declared");
    assert_eq!(capability_getter.selector, [0xb8, 0xca, 0x45, 0x9e]);
}

#[test]
fn the_getter_classification_keeps_absence_and_failure_apart() {
    for getter in &METADATA_GETTERS {
        // Data is an answer.
        assert_eq!(
            classify_getter(getter.name, GetterCall::Returned(&[1, 2, 3])).expect("answered"),
            GetterOutcome::Answered(&[1, 2, 3])
        );
        // Empty data and a revert are absence (no code, or no such getter).
        assert_eq!(
            classify_getter(getter.name, GetterCall::Returned(&[])).expect("absent"),
            GetterOutcome::Absent
        );
        assert_eq!(
            classify_getter(getter.name, GetterCall::Reverted).expect("absent"),
            GetterOutcome::Absent
        );
        // A node failure is an error, never an absence.
        assert!(classify_getter(
            getter.name,
            GetterCall::Failed {
                detail: "connection reset"
            }
        )
        .is_err());
    }
}

#[test]
fn getter_answers_preserve_what_the_transport_returned() {
    let answers = GetterAnswers::all_absent();
    assert_eq!(answers.answered_count(), 0);
    assert_eq!(
        degenbot_executor::release::classify_deployment(
            alloy::primitives::Address::repeat_byte(0x11),
            answers.observations(),
            None,
        )
        .expect("a getterless deployment classifies"),
        degenbot_executor::release::DeploymentRelease::Unknown
    );

    let one = GetterAnswers::from_parts(Some(Bytes::from_static(&[7])), None, None, None);
    assert_eq!(one.answered_count(), 1);
    assert_eq!(one.observations().release_id, GetterOutcome::Answered(&[7]));
    assert_eq!(one.observations().capabilities_hash, GetterOutcome::Absent);
}

#[test]
fn release_selection_supports_a_genuinely_different_command_grammar() {
    let registry = ReleaseRegistry::with_releases(vec![production_release(), alternate_release()])
        .expect("the test grammar coexists with production");
    let selected = registry
        .get(ALTERNATE_TAG)
        .expect("the alternate grammar is selected by its release tag");
    assert_eq!(selected.manifest().call_encoding.max_stream_length, 4);

    let context = EncodeContext::new(
        alloy::primitives::Address::repeat_byte(0x99),
        alloy::primitives::Address::repeat_byte(0x44),
        alloy::primitives::Address::repeat_byte(0x22),
    );
    let one_hop = EncodeRequest::new(
        PathInfo::new(vec![v2_hop()]),
        1_000,
        vec![1_000],
        vec![999],
        EncodeOptions::default(),
    );
    let call = selected
        .encoding()
        .encode(&context, &one_hop)
        .expect("one alternate-grammar command fits exactly");
    assert_eq!(call.data.to_vec(), vec![0xa5, 0xe8, 0x03]);

    let two_hops = EncodeRequest::new(
        PathInfo::new(vec![v2_hop(), v2_hop()]),
        1_000,
        vec![1_000, 1_000],
        vec![999, 999],
        EncodeOptions::default(),
    );
    let oversize = selected
        .encoding()
        .encode(&context, &two_hops)
        .expect_err("two three-byte commands exceed the alternate four-byte bound");
    assert!(
        matches!(&oversize, ComposeError::Unsupported(message) if message.contains("4 bytes")),
        "unexpected error: {oversize}"
    );

    let overflow = EncodeRequest::new(
        PathInfo::new(vec![v2_hop()]),
        u128::from(u16::MAX) + 1,
        vec![u128::from(u16::MAX) + 1],
        vec![999],
        EncodeOptions::default(),
    );
    let primitive = selected
        .encoding()
        .encode(&context, &overflow)
        .expect_err("the alternate operand is a checked u16");
    assert!(
        matches!(&primitive, ComposeError::Encode(message) if message.contains("u16")),
        "unexpected error: {primitive}"
    );
}
