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

#[cfg(test)]
pub(crate) use crate::release_package::v2_encoding::enforce_stream_length;
pub use crate::release_package::v2_encoding::{
    enforce_cmd_stream_v1_length, required_capabilities, CmdStreamV1Encoding,
};
use crate::release_package::ProductionRelease;
use std::sync::Arc;

use alloy::primitives::{Bytes, U256};

use degenbot_execution::ComposeError;

use crate::composers::{EncodeContext, EncodeRequest};
use crate::receipt::ExecutionReceipt;
use crate::release::{capability, CapabilityId, ManifestError, ReleaseManifest};

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
        let releases = ProductionRelease::ALL
            .iter()
            .map(|package| package.build().map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { releases })
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
