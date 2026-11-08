//! On-chain release discovery: the four metadata getters, and what a failed
//! read means.
//!
//! ADR-098 D5 makes discovery an optional built-in policy: a `cmd_executor`
//! deployment declares its own release identity through four pure getters, and
//! the built-in adapter validates that declaration against a release manifest
//! before it encodes anything. Nothing here reaches the generic execution seam
//! — a foreign execution contract exposes no getters and takes part in none of
//! this.
//!
//! # Absence is a classification, not an inference
//!
//! A getter that answers nothing is *absence of evidence*: a deployment
//! compiled before the getters existed has no such function, so an `eth_call`
//! to its selector reverts or returns no data. That is a fact about the
//! deployment, and [`classify_getter`] records it as
//! [`GetterOutcome::Absent`].
//!
//! A node failure is **not** that fact. A timeout, a dropped connection, a
//! rate-limit response, or an unresolvable host proves nothing about the
//! deployment; turning any of them into [`GetterOutcome::Absent`] would let a
//! broken RPC endpoint silently push a metadata-declaring deployment into the
//! legacy path, where an operator mapping — or a refusal — is all that is left.
//! [`DiscoveryError::Node`] is therefore a hard error, and the classification
//! is deliberate:
//!
//! | `eth_call` outcome | classification | why |
//! |--------------------|----------------|-----|
//! | return data, non-empty | [`GetterOutcome::Answered`] | the deployment declared something |
//! | return data, empty (`0x`) | [`GetterOutcome::Absent`] | no code / a fallback that returns nothing declares nothing |
//! | EVM revert (`execution reverted`) | [`GetterOutcome::Absent`] | an unknown selector reverts: the deployment has no such getter |
//! | transport / node failure | [`DiscoveryError::Node`] | proves nothing about the deployment |

use alloy::primitives::Bytes;

use crate::release::{GetterObservations, GetterOutcome};

/// One metadata getter as the deployment declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataGetter {
    /// The getter's name (the manifest and the error messages use it).
    pub name: &'static str,
    /// Its canonical signature, whose keccak selector an `eth_call` carries.
    pub signature: &'static str,
    /// The 4-byte selector, `keccak256(signature)[0..4]`.
    pub selector: [u8; 4],
}

/// The four metadata getters, in declaration order.
pub const METADATA_GETTERS: [MetadataGetter; 4] = [
    MetadataGetter {
        name: "release_id",
        signature: "release_id()",
        selector: [0x43, 0x64, 0x36, 0x76],
    },
    MetadataGetter {
        name: "protocol_major",
        signature: "protocol_major()",
        selector: [0x07, 0x41, 0x73, 0x56],
    },
    MetadataGetter {
        name: "call_encoding_version",
        signature: "call_encoding_version()",
        selector: [0x76, 0xc4, 0x34, 0x5b],
    },
    MetadataGetter {
        name: "capabilities",
        signature: "capabilities()",
        selector: [0x34, 0xa1, 0x8f, 0xc3],
    },
];

/// One getter's `eth_call` result, before classification.
///
/// A transport carries its own error type; the caller maps that to one of these
/// three arms *deliberately*, which is where "the node failed" stays distinct
/// from "the deployment does not have this getter".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetterCall<'a> {
    /// The call succeeded and returned `data` (possibly empty).
    Returned(&'a [u8]),
    /// The EVM reverted the call: an unknown selector on a contract without a
    /// fallback answers exactly this way.
    Reverted,
    /// The call never reached the EVM — a transport, node, or provider failure.
    Failed {
        /// What failed, in the transport's own words.
        detail: &'a str,
    },
}

/// Why a deployment's metadata could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// The getter could not be read at all: a node failure, never an absence.
    #[error("{getter}() could not be read ({detail}): a node failure is not an absent getter")]
    Node {
        /// The getter whose read failed.
        getter: &'static str,
        /// The transport's own account of the failure.
        detail: String,
    },
}

/// Classify ONE getter's call into a declaration outcome.
///
/// # Errors
///
/// [`DiscoveryError::Node`] when the transport failed — the one arm that must
/// never become [`GetterOutcome::Absent`].
pub fn classify_getter<'a>(
    getter: &'static str,
    call: GetterCall<'a>,
) -> Result<GetterOutcome<'a>, DiscoveryError> {
    match call {
        GetterCall::Returned([]) | GetterCall::Reverted => Ok(GetterOutcome::Absent),
        GetterCall::Returned(data) => Ok(GetterOutcome::Answered(data)),
        GetterCall::Failed { detail } => Err(DiscoveryError::Node {
            getter,
            detail: detail.to_owned(),
        }),
    }
}

/// The four getters' answers, as a transport returned them.
///
/// `None` is [`GetterOutcome::Absent`]; the borrowed
/// [`GetterObservations`](crate::release::GetterObservations) the strict
/// decoders consume is derived from this value rather than copied out of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GetterAnswers {
    release_id: Option<Bytes>,
    protocol_major: Option<Bytes>,
    call_encoding_version: Option<Bytes>,
    capabilities: Option<Bytes>,
}

impl GetterAnswers {
    /// A deployment that answered nothing (every getter absent).
    #[must_use]
    pub fn all_absent() -> Self {
        Self::default()
    }

    /// Build answers from optional return data (`None` = the getter is absent).
    #[must_use]
    pub fn from_parts(
        release_id: Option<Bytes>,
        protocol_major: Option<Bytes>,
        call_encoding_version: Option<Bytes>,
        capabilities: Option<Bytes>,
    ) -> Self {
        Self {
            release_id,
            protocol_major,
            call_encoding_version,
            capabilities,
        }
    }

    /// Record one getter's classified outcome.
    pub fn record(&mut self, getter: &MetadataGetter, outcome: GetterOutcome<'_>) {
        self.set(getter.name, outcome.data().map(Bytes::copy_from_slice));
    }

    fn set(&mut self, name: &'static str, data: Option<Bytes>) {
        match name {
            "release_id" => self.release_id = data,
            "protocol_major" => self.protocol_major = data,
            "call_encoding_version" => self.call_encoding_version = data,
            "capabilities" => self.capabilities = data,
            _ => {}
        }
    }

    /// The borrowed observations the strict decoders consume.
    #[must_use]
    pub fn observations(&self) -> GetterObservations<'_> {
        GetterObservations::from_parts(
            self.release_id.as_deref().map(|data| &**data),
            self.protocol_major.as_deref().map(|data| &**data),
            self.call_encoding_version.as_deref().map(|data| &**data),
            self.capabilities.as_deref().map(|data| &**data),
        )
    }

    /// How many of the four getters answered.
    #[must_use]
    pub fn answered_count(&self) -> usize {
        self.observations().answered_count()
    }
}
