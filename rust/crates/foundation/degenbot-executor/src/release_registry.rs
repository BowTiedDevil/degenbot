//! The bounded release registry: which complete call encodings this build
//! dispatches, and what each one needs from a deployment.
//!
//! ADR-098 D5 keeps versioning entirely inside the built-in adapter. This
//! module is the second half of that policy, after the manifest model in
//! [`crate::release`]: a [`Release`] pairs one validated manifest with the one
//! [`ReleaseEncoding`] that produces its bytes, and a [`ReleaseRegistry`] is
//! the *bounded* set this build will dispatch.
//!
//! # Complete encoding, selected by release
//!
//! A release selects the WHOLE call: the command stream, the packed config
//! semantics, and the `execute` ABI with its receipt together
//! ([`ReleaseEncoding::encode`]). A dispatcher that mixed one release's stream
//! with another release's config layout would produce a call no deployment can
//! execute, so the three never travel apart.
//!
//! [`CmdStreamV1Encoding`] is the production encoding — the current
//! `cmd_executor/2` release, whose command stream, packed config, and `execute`
//! ABI are the v1 encoding unchanged. A future major adds one manifest plus one
//! encoding and lands in [`ReleaseRegistry::with_releases`]; no generic crate
//! changes (ADR-098 D4/D5). A test may inject its own release for the same reason, and
//! the registry will not tell it apart from a shipped one: the registry is the
//! bound, not a flag.
//!
//! # Capability planning
//!
//! An encoding states which capability IDs its call needs
//! ([`ReleaseCall::requires`]); a release states which IDs it declares
//! ([`Release::declared_capabilities`]). Planning is the subtraction: a call
//! that needs a capability the selected release does not declare is declined
//! ([`Release::missing_capabilities`]) rather than encoded against a contract
//! that cannot run it. Declining is per-candidate, so one target's refusal
//! never suppresses another target's execution (ADR-098 D4).
//!
//! Capability IDs are opaque: an ID this build cannot name is still declared by
//! the manifest's list and still compared here, so an unknown ID can never be
//! mistaken for support. Planning reads only that list — the deployment's
//! capability commitment is a binding-time verification fact, never a planning
//! input.

use std::sync::{Arc, LazyLock};

use alloy::primitives::{Bytes, U256};

use degenbot_execution::{ComposeError, PathInfo};

use crate::composers::{
    config_for_options, encode_cmd_stream_detailed, encode_execute_call, resolve_axes,
    EncodeContext, EncodeOptions, EncodeRequest,
};
use crate::grammar_ledger::{Bribe, ProfitCapture};
use crate::grammar_shape::Derive;
use crate::receipt::ExecutionReceipt;
use crate::release::{
    capability, CapabilityId, ManifestError, ReleaseManifest, RELEASE_ID_TAG, RELEASE_MANIFEST_V2,
    SUPPORTED_PROTOCOL_MAJORS,
};

#[expect(
    clippy::expect_used,
    reason = "the embedded production manifest is a build-time invariant validated by ReleaseRegistry::production"
)]
static CMD_STREAM_V1_MANIFEST: LazyLock<ReleaseManifest> = LazyLock::new(|| {
    ReleaseManifest::parse(RELEASE_MANIFEST_V2)
        .expect("embedded cmd_executor/2 manifest must validate")
});

fn enforce_stream_length(commands: &[u8], max: u32) -> Result<(), ComposeError> {
    let max = usize::try_from(max)
        .map_err(|error| ComposeError::encode(format!("invalid stream-length bound: {error}")))?;
    if commands.len() > max {
        return Err(ComposeError::unsupported(format!(
            "command stream length {} exceeds the release maximum of {max} bytes",
            commands.len()
        )));
    }
    Ok(())
}

/// Enforce the command-stream bound declared by the embedded cmd_executor/2
/// manifest for callers that compose directly rather than through a bound release.
///
/// # Errors
///
/// [`ComposeError::Unsupported`] when the complete stream exceeds the declared bound.
pub fn enforce_cmd_stream_v1_length(commands: &[u8]) -> Result<(), ComposeError> {
    enforce_stream_length(
        commands,
        CMD_STREAM_V1_MANIFEST.call_encoding.max_stream_length,
    )
}

/// One release's complete encoding of a solved path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCall {
    /// The calldata, ABI-encoded as this release declares it.
    pub data: Bytes,
    /// The native value the call carries.
    pub value: U256,
    /// The capability IDs the release must declare for this call to run.
    pub requires: Vec<CapabilityId>,
}

impl ReleaseCall {
    /// A call that needs no declared capability.
    #[must_use]
    pub fn new(data: Bytes, value: U256) -> Self {
        Self {
            data,
            value,
            requires: Vec::new(),
        }
    }
}

/// The complete call encoding of one release: stream, config, and `execute`
/// ABI together.
///
/// The encoding is the release's own policy, above the generic
/// [`ExecutionAdapter`](degenbot_execution::ExecutionAdapter) seam: a foreign
/// contract never implements this trait and no generic crate names it.
pub trait ReleaseEncoding: Send + Sync + std::fmt::Debug {
    /// The release tag this encoding implements.
    ///
    /// A registry refuses an encoding whose tag disagrees with its manifest's
    /// ([`ReleaseError::EncodingReleaseId`]): the pair is one release, and a
    /// mismatched half is a definition defect, never a dispatch choice.
    fn release_id(&self) -> &str;

    /// Encode one solved path into the release's complete transaction.
    ///
    /// # Errors
    ///
    /// [`ComposeError::Unsupported`] when this release's grammar has no
    /// producer for the path; [`ComposeError::Encode`] when a primitive
    /// encoding step failed.
    fn encode(
        &self,
        context: &EncodeContext,
        request: &EncodeRequest,
    ) -> Result<ReleaseCall, ComposeError>;

    /// Decode this release's `execute` return data into its receipt.
    ///
    /// # Errors
    ///
    /// [`degenbot_execution::AssessError`] when the payload is not the shape
    /// this release's `execute_abi.returns` declares.
    fn decode_receipt(
        &self,
        return_data: Option<&Bytes>,
    ) -> Result<ExecutionReceipt, degenbot_execution::AssessError>;
}

/// The production encoding: the `cmd_executor` command stream.
///
/// The name keeps its v1 because the call encoding did not move in the ADR-099
/// cutover: `call_encoding_version()` is still 1 and `execute` produces the same
/// bytes for the same path.
#[derive(Debug, Clone, Copy, Default)]
pub struct CmdStreamV1Encoding;

impl ReleaseEncoding for CmdStreamV1Encoding {
    fn release_id(&self) -> &str {
        RELEASE_ID_TAG
    }

    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "ADR-030: a validator Reject is a fatal internal invariant, not a candidate decline"
    )]
    fn encode(
        &self,
        context: &EncodeContext,
        request: &EncodeRequest,
    ) -> Result<ReleaseCall, ComposeError> {
        let commands = match encode_cmd_stream_detailed(context, request) {
            Derive::Encoded(commands) => commands,
            Derive::Declined => {
                return Err(ComposeError::unsupported(
                    "the command grammar has no producer for this path",
                ));
            }
            Derive::EncodingFailed(error) => {
                return Err(ComposeError::encode(format!(
                    "command stream emission failed: {error}"
                )));
            }
            Derive::Rejected(error) => panic!(
                "executor derivation REJECT (a latent bug, ADR-030): the encoder wrote a Plan the ledger validator rejected: {error:?}"
            ),
        };
        enforce_cmd_stream_v1_length(&commands)?;
        let config = config_for_options(request.opts, U256::ZERO)
            .map_err(|error| ComposeError::encode(format!("config_for_options failed: {error}")))?;
        let call = encode_execute_call(context.executor, &commands, config);
        Ok(ReleaseCall {
            data: Bytes::from(call.data),
            value: call.value,
            requires: required_capabilities(&request.path, request.opts),
        })
    }

    fn decode_receipt(
        &self,
        return_data: Option<&Bytes>,
    ) -> Result<ExecutionReceipt, degenbot_execution::AssessError> {
        ExecutionReceipt::decode(return_data)
    }
}

/// The capability IDs a solved path's `cmd_executor` call needs.
///
/// The requirement set is derived from what the encoder will actually emit, so
/// a release that does not declare one of these declines the path instead of
/// producing bytes its contract would revert on:
///
/// - every stream opens with the address table and reports a receipt, so
///   [`capability::ADDRESS_TABLE`] and [`capability::EXECUTION_RECEIPT`] always
///   apply;
/// - one family capability per pool family present in the path;
/// - [`capability::V4_BATCH`] only for a pure-V4 path whose
///   [`EncodeOptions::use_v4_batch`] axis asks for the batched open;
/// - [`capability::ERC6909_CLAIMS`] for an ERC6909 profit capture;
/// - [`capability::CONFIG_BRIBE`] when the config carries a bribe.
///
/// The list is sorted and de-duplicated, so two requirement sources naming one
/// capability report it once.
#[must_use]
pub fn required_capabilities(path: &PathInfo, opts: EncodeOptions) -> Vec<CapabilityId> {
    let mut required = vec![capability::ADDRESS_TABLE, capability::EXECUTION_RECEIPT];

    let mut v2 = false;
    let mut v3 = false;
    let mut v4 = false;
    for hop in &path.hops {
        match hop {
            degenbot_execution::HopInfo::V2(_) => v2 = true,
            degenbot_execution::HopInfo::V3(_) => v3 = true,
            degenbot_execution::HopInfo::V4(_) => v4 = true,
        }
    }
    if v2 {
        required.push(capability::V2_SWAPS);
    }
    if v3 {
        required.push(capability::V3_SWAPS);
    }
    if v4 {
        required.push(capability::V4_SWAPS);
    }
    // The batched open exists only for an all-V4 path.
    if opts.use_v4_batch && v4 && !v2 && !v3 {
        required.push(capability::V4_BATCH);
    }

    let (_, capture, bribe) = resolve_axes(opts);
    if capture == ProfitCapture::Erc6909 {
        required.push(capability::ERC6909_CLAIMS);
    }
    if bribe != Bribe::None {
        required.push(capability::CONFIG_BRIBE);
    }

    required.sort_unstable();
    required.dedup();
    required
}

/// One validated release: a manifest plus the encoding it describes.
#[derive(Debug)]
pub struct Release {
    manifest: ReleaseManifest,
    encoding: Arc<dyn ReleaseEncoding>,
    development_runtime: Option<Bytes>,
}

/// Why a release definition was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseError {
    /// The manifest does not validate against the majors it claims.
    #[error("release manifest is not valid: {0}")]
    Manifest(#[from] ManifestError),
    /// The encoding's tag disagrees with the manifest's.
    #[error(
        "the encoding implements release {encoding:?} but its manifest describes {declared:?}: \
         a release's two halves name one tag"
    )]
    EncodingReleaseId {
        /// The manifest's tag.
        declared: String,
        /// The encoding's tag.
        encoding: String,
    },
    /// The registry already holds a release with this tag.
    #[error("release {0:?} is registered twice")]
    DuplicateRelease(String),
}

impl Release {
    /// Pair a validated manifest with its encoding.
    ///
    /// `supported_majors` is the bounded protocol-major set the caller is
    /// defining this release for: a manifest declaring a major outside it is
    /// refused here rather than at dispatch, so an unbounded release can never
    /// enter a registry.
    ///
    /// `development_runtime` is the release's own runtime artifact when the
    /// definition ships it in memory (a test-only release, an undeployed
    /// build). A production release leaves it `None` and its artifact is the
    /// committed file the manifest names.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::Manifest`] when the manifest does not validate against
    /// `supported_majors`; [`ReleaseError::EncodingReleaseId`] when the encoding
    /// names a different release than the manifest.
    pub fn new(
        manifest: ReleaseManifest,
        supported_majors: &[u32],
        encoding: Arc<dyn ReleaseEncoding>,
        development_runtime: Option<Bytes>,
    ) -> Result<Self, ReleaseError> {
        manifest.validate_within(supported_majors)?;
        if encoding.release_id() != manifest.release_id {
            return Err(ReleaseError::EncodingReleaseId {
                declared: manifest.release_id.clone(),
                encoding: encoding.release_id().to_owned(),
            });
        }
        Ok(Self {
            manifest,
            encoding,
            development_runtime,
        })
    }

    /// The release's validated manifest.
    #[must_use]
    pub const fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }

    /// The release's complete call encoding.
    #[must_use]
    pub fn encoding(&self) -> Arc<dyn ReleaseEncoding> {
        Arc::clone(&self.encoding)
    }

    /// The declared release tag.
    #[must_use]
    pub fn release_id(&self) -> &str {
        &self.manifest.release_id
    }

    /// The declared protocol major.
    #[must_use]
    pub const fn protocol_major(&self) -> u32 {
        self.manifest.protocol.major
    }

    /// The declared version of the whole call encoding.
    #[must_use]
    pub const fn call_encoding_version(&self) -> u32 {
        self.manifest.call_encoding.version
    }

    /// The repository-relative runtime artifact path the manifest names.
    #[must_use]
    pub fn runtime_artifact_path(&self) -> &str {
        &self.manifest.artifact.runtime_code
    }

    /// The in-memory runtime artifact this release definition ships, if any.
    ///
    /// A release whose artifact is a committed file answers `None`; its runtime
    /// is then read from the deployment artifact the repository ships for it.
    #[must_use]
    pub fn development_runtime(&self) -> Option<&Bytes> {
        self.development_runtime.as_ref()
    }

    /// The capability IDs this release declares.
    ///
    /// Planning reads the manifest's list, never the deployment's commitment:
    /// binding already proved the deployment's word names this list, so the
    /// commitment stays a verification fact rather than a planning input.
    #[must_use]
    pub fn declared_capabilities(&self) -> Vec<CapabilityId> {
        self.manifest
            .capability_ids()
            .map(CapabilityId::new)
            .collect()
    }

    /// The capability names this release declares, for diagnostics.
    #[must_use]
    pub fn capability_name(&self, id: CapabilityId) -> Option<&str> {
        self.manifest
            .capability(id)
            .map(|declared| declared.name.as_str())
    }

    /// The capability IDs `required` names that this release does NOT declare.
    ///
    /// Returned in the order they were required, so the first missing ID is the
    /// one a decline message names.
    #[must_use]
    pub fn missing_capabilities(&self, required: &[CapabilityId]) -> Vec<CapabilityId> {
        let declared = self.declared_capabilities();
        required
            .iter()
            .copied()
            .filter(|id| !declared.contains(id))
            .collect()
    }

    /// Render a missing-capability set for a decline message.
    ///
    /// The manifest names only the IDs the release DECLARES, so a missing one
    /// is named from this build's stable catalog
    /// ([`capability::name`]) — the ID's meaning never changes, which is why it
    /// can be named from outside the release that lacks it.
    #[must_use]
    pub fn describe_capabilities(&self, ids: &[CapabilityId]) -> String {
        ids.iter()
            .map(|id| format!("{} ({id})", capability::name(*id)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The bounded set of releases this build dispatches.
///
/// Production holds exactly the releases compiled into this build
/// ([`Self::production`]). Anything else — a test's alternate major, a future
/// release under development — is injected explicitly through
/// [`Self::with_releases`], which is what makes the bound a decision rather
/// than a convention.
#[derive(Debug, Default)]
pub struct ReleaseRegistry {
    releases: Vec<Arc<Release>>,
}

impl ReleaseRegistry {
    /// Every release this build ships: the validated production manifests.
    ///
    /// # Errors
    ///
    /// [`ReleaseError::Manifest`] when an embedded manifest no longer validates
    /// — a build defect, surfaced rather than swallowed.
    pub fn production() -> Result<Self, ReleaseError> {
        let manifest = ReleaseManifest::parse(RELEASE_MANIFEST_V2)?;
        let release = Release::new(
            manifest,
            SUPPORTED_PROTOCOL_MAJORS,
            Arc::new(CmdStreamV1Encoding),
            None,
        )?;
        Ok(Self {
            releases: vec![Arc::new(release)],
        })
    }

    /// A registry over an explicit release set (tests, a future major).
    ///
    /// # Errors
    ///
    /// [`ReleaseError::DuplicateRelease`] when two entries name one tag: a tag
    /// resolves to one encoding, so two claimants make every binding ambiguous.
    pub fn with_releases(releases: Vec<Release>) -> Result<Self, ReleaseError> {
        let mut entries: Vec<Arc<Release>> = Vec::with_capacity(releases.len());
        for release in releases {
            if entries
                .iter()
                .any(|entry| entry.release_id() == release.release_id())
            {
                return Err(ReleaseError::DuplicateRelease(
                    release.release_id().to_owned(),
                ));
            }
            entries.push(Arc::new(release));
        }
        Ok(Self { releases: entries })
    }

    /// The release declared under `tag`, if this build dispatches it.
    #[must_use]
    pub fn get(&self, tag: &str) -> Option<Arc<Release>> {
        self.releases
            .iter()
            .find(|release| release.release_id() == tag)
            .map(Arc::clone)
    }

    /// Every registered release, in declaration order.
    #[must_use]
    pub fn releases(&self) -> &[Arc<Release>] {
        &self.releases
    }

    /// How many releases this registry dispatches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.releases.len()
    }

    /// Whether the registry dispatches nothing (an empty set would accept no
    /// deployment at all, which is why [`Self::production`] never builds one).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }

    /// Every registered release tag, in declaration order.
    pub fn tags(&self) -> impl Iterator<Item = &str> + '_ {
        self.releases.iter().map(|release| release.release_id())
    }

    /// The bounded protocol-major set this registry can dispatch.
    ///
    /// A deployment declaring a major outside this set is refused at binding,
    /// even when its release tag is known: the tag names the encoding, and the
    /// major says whether this build may produce it at all.
    #[must_use]
    pub fn majors(&self) -> Vec<u32> {
        let mut majors: Vec<u32> = self
            .releases
            .iter()
            .map(|release| release.protocol_major())
            .collect();
        majors.sort_unstable();
        majors.dedup();
        majors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_bound_accepts_exact_and_rejects_one_over() {
        let commands = [0u8; 289];
        assert!(enforce_stream_length(&commands[..288], 288).is_ok());
        assert!(matches!(
            enforce_stream_length(&commands, 288),
            Err(ComposeError::Unsupported(_))
        ));
    }
}
