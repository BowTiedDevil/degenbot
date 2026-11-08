//! The built-in `cmd_executor` release identity and its release manifest.
//!
//! ADR-098 D5 makes versioning an *optional built-in* concern: this module is
//! the on-chain/off-chain model the built-in adapter owns, and nothing here
//! reaches the generic execution seam. A foreign execution contract needs no
//! metadata getters, no manifest, and no release policy.
//!
//! # Two halves, two authorities
//!
//! - **On-chain**: the deployed contract answers four pure getters —
//!   `release_id()`, `protocol_major()`, `call_encoding_version()`, and
//!   `capabilities()`. They return compile-time constants, so a deployment
//!   declares its release without a proxy, an upgrade path, or an extra
//!   constructor argument. The decoders in this module are strict: an absent,
//!   short, malformed, or self-contradictory answer is an error, never a
//!   default.
//! - **Off-chain**: [`ReleaseManifest`] binds that declared identity to the
//!   *complete call encoding* — the command-stream grammar, the packed
//!   `execute(config)` semantics, the `execute` ABI (including the receipt
//!   shape), the capability names behind the on-chain IDs, and the runtime
//!   artifact location.
//!
//! [`CallEncodingVersion`] versions stream, config, and execute ABI *together*:
//! a change to any of the three is one release bump, never three independent
//! ones.
//!
//! # Release identity is not artifact integrity
//!
//! A release is named by its declared tag (`"cmd_executor/1"`), never by a
//! whole-bytecode hash: Vyper immutables (owner, WETH, PoolManager, USDC, USDT,
//! and the two precomputed V4 delta slots) differ per deployment, so two
//! faithful deployments of one release have different runtime code. Artifact
//! integrity and provenance stay with
//! `tier3-oracle/artifacts/executor/manifest.json`, which the release manifest
//! *references* (`artifact.provenance`) rather than absorbs.
//!
//! # Unknown capability IDs survive
//!
//! [`CapabilityId`] is an opaque `u32`. An ID this build does not know decodes
//! verbatim and stays in the [`CapabilitySet`]; only the manifest can name it.
//! Collapsing the set to a boolean, or to an enum that rejects future IDs,
//! would make every new capability a breaking change for every reader.
//!
//! # Getterless deployments
//!
//! A deployment that predates these getters answers nothing. That is *absence
//! of evidence*, not evidence of the latest release: [`classify_deployment`]
//! names such a deployment only through an operator-provided
//! [`LegacyReleaseMapping`], and otherwise reports [`DeploymentRelease::Unknown`].

use alloy::primitives::{keccak256, Address};
use serde::Deserialize;

/// The current release's ASCII tag (`cmd_executor/1`), as declared on-chain.
pub const RELEASE_ID_TAG: &str = "cmd_executor/1";

/// The committed release manifest for [`RELEASE_ID_TAG`].
pub const RELEASE_MANIFEST_V1: &str = include_str!("../releases/cmd_executor.v1.json");

/// The protocol majors this build can dispatch, in ascending order.
///
/// Bounded on purpose: an unknown major is a refusal, not an invitation to try
/// the newest encoder. Multi-major support grows this set and adds one manifest
/// per major — no generic crate changes (ADR-098 D4/D5).
pub const SUPPORTED_PROTOCOL_MAJORS: &[u32] = &[1];

/// The largest capability-ID list the contract can return
/// (`MAX_CAPABILITY_IDS` in `cmd_executor.vy`).
pub const MAX_CAPABILITY_IDS: usize = 32;

// ── Identity primitives ────────────────────────────────────────────────────

/// The release identity declared by an executor deployment.
///
/// A NUL-padded printable-ASCII tag, so a raw on-chain read is human-readable
/// without a lookup table, and so any future release tag decodes without a new
/// enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReleaseId {
    bytes: [u8; 32],
    tag_len: u8,
}

/// Why a byte string is not a release identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseIdError {
    /// An empty tag names nothing.
    #[error("a release id must carry a tag")]
    Empty,
    /// More than 32 non-NUL bytes cannot fit a `bytes32`.
    #[error("release id tag is {0} bytes, above the 32-byte bytes32 limit")]
    TooLong(usize),
    /// Printable ASCII keeps the tag readable and the padding unambiguous.
    #[error("release id tag is not printable ASCII")]
    NotAscii,
    /// A NUL before the end of the tag would make the padding ambiguous.
    #[error("release id tag has a NUL byte before its end")]
    InteriorNul,
}

impl ReleaseId {
    /// Build a release identity from its printable-ASCII tag.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseIdError`] for an empty tag, a tag longer than 32 bytes,
    /// a non-printable-ASCII byte, or an embedded NUL.
    pub fn from_ascii_tag(tag: &str) -> Result<Self, ReleaseIdError> {
        let raw = tag.as_bytes();
        if raw.is_empty() {
            return Err(ReleaseIdError::Empty);
        }
        if raw.len() > 32 {
            return Err(ReleaseIdError::TooLong(raw.len()));
        }
        if raw.contains(&0) {
            return Err(ReleaseIdError::InteriorNul);
        }
        if !raw.iter().all(|byte| (0x20..0x7f).contains(byte)) {
            return Err(ReleaseIdError::NotAscii);
        }
        let mut bytes = [0u8; 32];
        bytes[..raw.len()].copy_from_slice(raw);
        // `raw.len() <= 32` here, so the narrowing cannot fail; `unwrap_or`
        // keeps the function panic-free on a value that cannot occur.
        let tag_len = u8::try_from(raw.len()).unwrap_or(u8::MAX);
        Ok(Self { bytes, tag_len })
    }

    /// Read a release identity from a raw `bytes32` word, requiring the
    /// NUL-padded printable-ASCII tag shape.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseIdError`] when the word's prefix is not a printable-ASCII
    /// tag or a non-NUL byte follows the padding.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, ReleaseIdError> {
        let tag_len = bytes.iter().position(|byte| *byte == 0).unwrap_or(32);
        if bytes[tag_len..].iter().any(|byte| *byte != 0) {
            return Err(ReleaseIdError::NotAscii);
        }
        // `tag_len <= 32` (position over a 32-byte array), so the narrowing
        // cannot fail.
        let tag_len_u8 = u8::try_from(tag_len).unwrap_or(u8::MAX);
        let tag = Self {
            bytes,
            tag_len: tag_len_u8,
        };
        if tag.ascii_tag().is_empty() {
            return Err(ReleaseIdError::Empty);
        }
        if !tag.bytes[..tag_len]
            .iter()
            .all(|byte| (0x20..0x7f).contains(byte))
        {
            return Err(ReleaseIdError::NotAscii);
        }
        Ok(tag)
    }

    /// The raw `bytes32` word, exactly as the contract returns it.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// The printable-ASCII tag, without the NUL padding.
    #[must_use]
    pub fn ascii_tag(&self) -> &str {
        // Construction admits only printable ASCII, so this slice is valid UTF-8.
        std::str::from_utf8(&self.bytes[..usize::from(self.tag_len).min(self.bytes.len())])
            .unwrap_or("")
    }
}

impl std::fmt::Display for ReleaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ascii_tag())
    }
}

/// The bounded protocol major: the unit a future multi-major dispatcher selects
/// an encoder by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtocolMajor(u32);

impl ProtocolMajor {
    /// Wrap a raw protocol major.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw declared value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether this build can dispatch this protocol major.
    #[must_use]
    pub fn is_supported(self) -> bool {
        SUPPORTED_PROTOCOL_MAJORS.contains(&self.0)
    }
}

/// The version of the whole call encoding: command stream, packed config, and
/// execute ABI move together under this number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallEncodingVersion(u32);

impl CallEncodingVersion {
    /// Wrap a raw call-encoding version.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw declared value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An opaque capability identifier declared by a deployment.
///
/// Deliberately not an enum: an ID this build does not know must survive
/// decoding verbatim (it may name a capability added after this build), so the
/// type carries the raw value and nothing rejects it for being unfamiliar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityId(u32);

impl CapabilityId {
    /// Wrap a raw capability ID.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw declared value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "capability:{}", self.0)
    }
}

/// An ordered capability-ID list as the deployment declares it.
///
/// Order is preserved and duplicates are preserved by construction; the strict
/// decoder rejects duplicates and non-ascending order, so a set that came off
/// chain is a faithful copy of the declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    ids: Vec<CapabilityId>,
}

impl CapabilitySet {
    /// Build a set from declared IDs, preserving order and duplicates.
    #[must_use]
    pub fn from_ids(ids: &[CapabilityId]) -> Self {
        Self { ids: ids.to_vec() }
    }

    /// The declared IDs, in declaration order.
    #[must_use]
    pub fn ids(&self) -> &[CapabilityId] {
        &self.ids
    }

    /// Whether the deployment declared this ID.
    #[must_use]
    pub fn contains(&self, id: CapabilityId) -> bool {
        self.ids.contains(&id)
    }

    /// The declared IDs as raw values.
    pub fn raw_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.ids.iter().map(|id| id.0)
    }
}

// ── On-chain metadata ──────────────────────────────────────────────────────

/// What a deployment declared through its metadata getters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorMetadata {
    /// The declared release identity.
    pub release: ReleaseId,
    /// The declared protocol major.
    pub protocol_major: ProtocolMajor,
    /// The declared version of the whole call encoding.
    pub call_encoding_version: CallEncodingVersion,
    /// The declared capability IDs, including any this build cannot name.
    pub capabilities: CapabilitySet,
}

/// One metadata getter's raw `eth_call` outcome.
///
/// `Absent` is the getterless case: the call reverted or returned no data, so
/// the deployment declared nothing. It is never read as "the latest release".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetterOutcome<'a> {
    /// The deployment answered with return data.
    Answered(&'a [u8]),
    /// The deployment has no such getter.
    Absent,
}

impl GetterOutcome<'_> {
    /// The answered return data, if any.
    #[must_use]
    pub const fn data(&self) -> Option<&[u8]> {
        match self {
            Self::Answered(data) => Some(*data),
            Self::Absent => None,
        }
    }
}

/// The four getter outcomes for one deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetterObservations<'a> {
    /// `release_id()` return data.
    pub release_id: GetterOutcome<'a>,
    /// `protocol_major()` return data.
    pub protocol_major: GetterOutcome<'a>,
    /// `call_encoding_version()` return data.
    pub call_encoding_version: GetterOutcome<'a>,
    /// `capabilities()` return data.
    pub capabilities: GetterOutcome<'a>,
}

impl<'a> GetterObservations<'a> {
    /// A deployment that answered no metadata getter at all.
    #[must_use]
    pub const fn all_absent() -> Self {
        Self {
            release_id: GetterOutcome::Absent,
            protocol_major: GetterOutcome::Absent,
            call_encoding_version: GetterOutcome::Absent,
            capabilities: GetterOutcome::Absent,
        }
    }

    /// Build observations from optional return data (`None` = getter absent).
    #[must_use]
    pub const fn from_parts(
        release_id: Option<&'a [u8]>,
        protocol_major: Option<&'a [u8]>,
        call_encoding_version: Option<&'a [u8]>,
        capabilities: Option<&'a [u8]>,
    ) -> Self {
        Self {
            release_id: answered(release_id),
            protocol_major: answered(protocol_major),
            call_encoding_version: answered(call_encoding_version),
            capabilities: answered(capabilities),
        }
    }

    /// How many of the four getters answered.
    #[must_use]
    pub const fn answered_count(&self) -> usize {
        let mut answered = 0;
        if self.release_id.data().is_some() {
            answered += 1;
        }
        if self.protocol_major.data().is_some() {
            answered += 1;
        }
        if self.call_encoding_version.data().is_some() {
            answered += 1;
        }
        if self.capabilities.data().is_some() {
            answered += 1;
        }
        answered
    }
}

const fn answered(data: Option<&[u8]>) -> GetterOutcome<'_> {
    match data {
        Some(data) => GetterOutcome::Answered(data),
        None => GetterOutcome::Absent,
    }
}

/// Why a metadata answer is not a usable identity declaration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataDecodeError {
    /// The getter answered nothing.
    #[error("{getter}() answered no data; absence is not a release declaration")]
    Absent {
        /// The getter that stayed silent.
        getter: &'static str,
    },
    /// The return data is not exactly the declared ABI shape.
    #[error("{getter}() returned {actual} bytes, expected exactly {expected}")]
    Length {
        /// The getter that answered.
        getter: &'static str,
        /// The exact expected byte count.
        expected: usize,
        /// The observed byte count.
        actual: usize,
    },
    /// A value does not fit the declared type.
    #[error("{getter}() returned a value outside its declared range: {detail}")]
    OutOfRange {
        /// The getter that answered.
        getter: &'static str,
        /// What exactly is out of range.
        detail: String,
    },
    /// The release word is not a NUL-padded printable-ASCII tag.
    #[error("release_id() is not a NUL-padded printable-ASCII release tag")]
    MalformedReleaseId,
    /// A field that must name something declared zero.
    #[error("{getter}() declared zero, which names nothing")]
    Zero {
        /// The getter that declared zero.
        getter: &'static str,
    },
    /// More capability IDs than the contract can return.
    #[error("capabilities() declared {count} IDs, above the {max} the contract can return")]
    TooManyCapabilities {
        /// The declared count.
        count: usize,
        /// The contract's ABI bound.
        max: usize,
    },
    /// Duplicate or descending capability IDs.
    #[error(
        "capabilities() declared ID {id} after {previous}: the list must be ascending and unique"
    )]
    UnorderedCapabilities {
        /// The ID of the preceding element.
        previous: u32,
        /// The out-of-order ID.
        id: u32,
    },
    /// Some getters answered and some did not.
    #[error("only {answered} of 4 metadata getters answered; a deployment that declares a partial identity is neither declared nor getterless")]
    PartialAnswers {
        /// How many getters answered.
        answered: usize,
    },
}

/// Decode `release_id()` return data.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::Length`] unless the data is exactly one ABI
/// word, and [`MetadataDecodeError::MalformedReleaseId`] unless that word is a
/// NUL-padded printable-ASCII tag.
pub fn decode_release_id(data: &[u8]) -> Result<ReleaseId, MetadataDecodeError> {
    let word = abi_word("release_id", data)?;
    ReleaseId::from_bytes(word).map_err(|_| MetadataDecodeError::MalformedReleaseId)
}

/// Decode `protocol_major()` return data.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::Length`] unless the data is exactly one ABI
/// word, [`MetadataDecodeError::OutOfRange`] when the word's high bytes are
/// non-zero, and [`MetadataDecodeError::Zero`] for a zero major.
pub fn decode_protocol_major(data: &[u8]) -> Result<ProtocolMajor, MetadataDecodeError> {
    let raw = abi_u32("protocol_major", data)?;
    if raw == 0 {
        return Err(MetadataDecodeError::Zero {
            getter: "protocol_major",
        });
    }
    Ok(ProtocolMajor::new(raw))
}

/// Decode `call_encoding_version()` return data.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::Length`] unless the data is exactly one ABI
/// word, [`MetadataDecodeError::OutOfRange`] when the word's high bytes are
/// non-zero, and [`MetadataDecodeError::Zero`] for version zero.
pub fn decode_call_encoding_version(
    data: &[u8],
) -> Result<CallEncodingVersion, MetadataDecodeError> {
    let raw = abi_u32("call_encoding_version", data)?;
    if raw == 0 {
        return Err(MetadataDecodeError::Zero {
            getter: "call_encoding_version",
        });
    }
    Ok(CallEncodingVersion::new(raw))
}

/// Decode `capabilities()` return data: a canonical ABI `uint32[]`.
///
/// The decode is exact: one offset word of 32, one length word, `length`
/// element words, and nothing else. Unknown IDs are preserved verbatim.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::Length`] for short data, non-canonical
/// trailing data, or a length that disagrees with the payload;
/// [`MetadataDecodeError::OutOfRange`] for a non-canonical head or an element
/// above `u32`; [`MetadataDecodeError::TooManyCapabilities`] above the contract
/// bound; [`MetadataDecodeError::Zero`] for an empty list or a zero ID; and
/// [`MetadataDecodeError::UnorderedCapabilities`] for a duplicate or descending
/// ID.
pub fn decode_capabilities(data: &[u8]) -> Result<CapabilitySet, MetadataDecodeError> {
    const GETTER: &str = "capabilities";
    if data.len() < 64 {
        return Err(MetadataDecodeError::Length {
            getter: GETTER,
            expected: 64,
            actual: data.len(),
        });
    }
    let offset = element_u32(GETTER, &data[..32], "the dynamic-array offset")?;
    if offset != 32 {
        return Err(MetadataDecodeError::OutOfRange {
            getter: GETTER,
            detail: format!("the dynamic-array offset is {offset}, expected 32"),
        });
    }
    let count = element_u32(GETTER, &data[32..64], "the array length")? as usize;
    if count == 0 {
        return Err(MetadataDecodeError::Zero { getter: GETTER });
    }
    if count > MAX_CAPABILITY_IDS {
        return Err(MetadataDecodeError::TooManyCapabilities {
            count,
            max: MAX_CAPABILITY_IDS,
        });
    }
    let expected = 64 + 32 * count;
    if data.len() != expected {
        return Err(MetadataDecodeError::Length {
            getter: GETTER,
            expected,
            actual: data.len(),
        });
    }
    let mut ids = Vec::with_capacity(count);
    for element in data[64..].as_chunks::<32>().0 {
        let id = element_u32(GETTER, element, "a capability ID")?;
        if id == 0 {
            return Err(MetadataDecodeError::Zero { getter: GETTER });
        }
        if let Some(previous) = ids.last().copied() {
            if id <= previous {
                return Err(MetadataDecodeError::UnorderedCapabilities { previous, id });
            }
        }
        ids.push(id);
    }
    Ok(CapabilitySet::from_ids(
        &ids.into_iter().map(CapabilityId::new).collect::<Vec<_>>(),
    ))
}

/// Decode all four getter outcomes into one identity declaration.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::PartialAnswers`] unless all four getters
/// answered, and the per-getter [`MetadataDecodeError`] when one answered
/// malformed data.
pub fn decode_metadata(
    observations: GetterObservations<'_>,
) -> Result<ExecutorMetadata, MetadataDecodeError> {
    let answered = observations.answered_count();
    if answered != 4 {
        return Err(MetadataDecodeError::PartialAnswers { answered });
    }
    let (
        GetterOutcome::Answered(release_id),
        GetterOutcome::Answered(protocol_major),
        GetterOutcome::Answered(call_encoding_version),
        GetterOutcome::Answered(capabilities),
    ) = (
        observations.release_id,
        observations.protocol_major,
        observations.call_encoding_version,
        observations.capabilities,
    )
    else {
        return Err(MetadataDecodeError::PartialAnswers { answered });
    };
    Ok(ExecutorMetadata {
        release: decode_release_id(release_id)?,
        protocol_major: decode_protocol_major(protocol_major)?,
        call_encoding_version: decode_call_encoding_version(call_encoding_version)?,
        capabilities: decode_capabilities(capabilities)?,
    })
}

/// One ABI word holding a static value, with the exact-length rule.
fn abi_word(getter: &'static str, data: &[u8]) -> Result<[u8; 32], MetadataDecodeError> {
    if data.len() != 32 {
        return Err(MetadataDecodeError::Length {
            getter,
            expected: 32,
            actual: data.len(),
        });
    }
    let mut word = [0u8; 32];
    word.copy_from_slice(data);
    Ok(word)
}

/// One ABI word holding a `u32`, rejecting non-canonical high bytes.
fn abi_u32(getter: &'static str, data: &[u8]) -> Result<u32, MetadataDecodeError> {
    let word = abi_word(getter, data)?;
    element_u32(getter, &word, "the ABI word")
}

/// Read a `u32` from one ABI word, rejecting non-canonical high bytes.
fn element_u32(getter: &'static str, word: &[u8], what: &str) -> Result<u32, MetadataDecodeError> {
    if word.len() != 32 {
        return Err(MetadataDecodeError::Length {
            getter,
            expected: 32,
            actual: word.len(),
        });
    }
    if word[..28].iter().any(|byte| *byte != 0) {
        return Err(MetadataDecodeError::OutOfRange {
            getter,
            detail: format!("{what} carries non-zero high bytes"),
        });
    }
    Ok(u32::from_be_bytes([word[28], word[29], word[30], word[31]]))
}

// ── Legacy mapping ─────────────────────────────────────────────────────────

/// One operator-declared legacy deployment → release binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyReleaseEntry {
    /// The getterless deployment address.
    pub address: Address,
    /// The release the operator declares it runs.
    pub release: ReleaseId,
}

/// The operator-provided mapping that names getterless deployments.
///
/// A mapping entry is the *only* way a getterless deployment acquires a
/// release. Silence from the getters proves nothing about which release a
/// deployment runs, and guessing "latest" would hand a stale encoder a
/// deployment it cannot drive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyReleaseMapping {
    entries: Vec<LegacyReleaseEntry>,
}

impl LegacyReleaseMapping {
    /// Build a mapping from the operator's declared entries.
    #[must_use]
    pub fn new(entries: &[LegacyReleaseEntry]) -> Self {
        Self {
            entries: entries.to_vec(),
        }
    }

    /// The release the operator declared for `address`, if any.
    #[must_use]
    pub fn release_for(&self, address: Address) -> Option<ReleaseId> {
        self.entries
            .iter()
            .find(|entry| entry.address == address)
            .map(|entry| entry.release)
    }

    /// The declared entries, in declaration order.
    #[must_use]
    pub fn entries(&self) -> &[LegacyReleaseEntry] {
        &self.entries
    }
}

/// How a deployment's release is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentRelease {
    /// The deployment declared its own identity through the metadata getters.
    Declared(ExecutorMetadata),
    /// A getterless deployment the operator's [`LegacyReleaseMapping`] names.
    Legacy {
        /// The mapped release.
        release: ReleaseId,
    },
    /// A getterless deployment nobody named: unknown, and deliberately not
    /// "latest".
    Unknown,
}

/// Classify a deployment's release from its metadata observations.
///
/// - all four getters answered → [`DeploymentRelease::Declared`] (after a strict
///   decode, so malformed data is an error rather than a declaration);
/// - none answered → [`DeploymentRelease::Legacy`] when the operator mapping
///   names the address, otherwise [`DeploymentRelease::Unknown`];
/// - some answered → an error: a partially-declaring deployment is neither
///   declared nor getterless, and the operator must investigate rather than
///   fall back.
///
/// # Errors
///
/// Returns [`MetadataDecodeError::PartialAnswers`] for a mixed outcome set and
/// the per-getter [`MetadataDecodeError`] for malformed answered data.
pub fn classify_deployment(
    address: Address,
    observations: GetterObservations<'_>,
    legacy: Option<&LegacyReleaseMapping>,
) -> Result<DeploymentRelease, MetadataDecodeError> {
    match observations.answered_count() {
        4 => decode_metadata(observations).map(DeploymentRelease::Declared),
        0 => Ok(
            match legacy.and_then(|mapping| mapping.release_for(address)) {
                Some(release) => DeploymentRelease::Legacy { release },
                None => DeploymentRelease::Unknown,
            },
        ),
        answered => Err(MetadataDecodeError::PartialAnswers { answered }),
    }
}

// ── Release manifest ───────────────────────────────────────────────────────

/// The off-chain description of one built-in executor release.
///
/// Every section is `deny_unknown_fields`: a misspelled or invented key is a
/// hard error, so a manifest cannot silently drift into "mostly right". The
/// identity block deliberately has no bytecode hash — a whole-runtime hash is
/// deployment-specific (Vyper immutables) and would make two faithful
/// deployments of one release disagree; integrity lives in
/// `artifact.provenance`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    /// Manifest schema version (this build understands 1).
    pub schema_version: u32,
    /// The declared release tag, matching `release_id()`.
    pub release_id: String,
    /// The declared protocol major and informational minor.
    pub protocol: ProtocolDeclaration,
    /// The complete call encoding.
    pub call_encoding: CallEncoding,
    /// The capability names behind the on-chain IDs.
    pub capabilities: Vec<CapabilityDeclaration>,
    /// Where the runtime artifact and its provenance live.
    pub artifact: ArtifactDeclaration,
}

/// The protocol block of a release manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolDeclaration {
    /// The bounded dispatch unit declared by `protocol_major()`.
    pub major: u32,
    /// Informational minor version; never used for dispatch.
    pub minor: u32,
}

/// The complete call-encoding description of a release.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEncoding {
    /// The joint version declared by `call_encoding_version()`.
    pub version: u32,
    /// The `execute(commands)` ABI length bound.
    pub max_stream_length: u32,
    /// How stream bytes resolve to addresses.
    pub address_resolution: AddressResolution,
    /// The command grammar.
    pub command_stream: CommandStream,
    /// The packed `execute(config)` semantics.
    pub config: ConfigDeclaration,
    /// The `execute` ABI, including the receipt shape.
    pub execute_abi: ExecuteAbi,
}

/// How a stream's address operands resolve.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressResolution {
    /// The largest `SET_ADDRESS` table (`MAX_INDEXED_ADDRESSES`).
    pub max_table_entries: u32,
    /// The reserved sentinel index → role bindings.
    pub sentinels: Vec<SentinelDeclaration>,
}

/// One reserved sentinel index.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentinelDeclaration {
    /// The reserved index, as a hex byte (`"0xfc"`).
    pub id: String,
    /// The role it resolves to.
    pub role: String,
}

/// The command-stream grammar.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStream {
    /// Every opcode the release accepts, ascending.
    pub opcodes: Vec<OpcodeDeclaration>,
}

/// One opcode's framing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpcodeDeclaration {
    /// The opcode byte, as hex (`"0x20"`).
    pub opcode: String,
    /// The opcode's name, used by capability declarations.
    pub name: String,
    /// Preprocessing, execution, or the preprocessing separator.
    pub kind: OpcodeKind,
    /// The fixed fields, in wire order.
    pub fields: Vec<FieldDeclaration>,
    /// A count-prefixed repeated entry block, when the opcode has one.
    #[serde(default)]
    pub repeat: Option<RepeatDeclaration>,
    /// A length-prefixed variable tail, when the opcode has one.
    #[serde(default)]
    pub variable_tail: Option<VariableTailDeclaration>,
}

/// Where in the stream an opcode may appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpcodeKind {
    /// Preprocessing, before the separator.
    Preprocess,
    /// An execution command.
    Execute,
    /// The preprocessing/execution separator.
    Separator,
}

/// One fixed field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDeclaration {
    /// The field name.
    pub name: String,
    /// The field type (`uint8`, `uint16`, `int16`, `uint96`, `uint256`,
    /// `address`, or `bytes`).
    pub r#type: String,
}

/// A count-prefixed repeated block.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepeatDeclaration {
    /// The `uint8` field carrying the entry count.
    pub count_field: String,
    /// The contract's loop bound for the count.
    pub max_count: u32,
    /// The repeated entry.
    pub entry: EntryDeclaration,
}

/// One repeated entry's framing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryDeclaration {
    /// The entry's fixed fields, in wire order.
    pub fields: Vec<FieldDeclaration>,
}

/// A length-prefixed variable tail.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableTailDeclaration {
    /// The `uint8` field carrying the tail length.
    pub length_field: String,
    /// The `bytes` field carrying the tail.
    pub data_field: String,
}

/// The packed `execute(config)` semantics.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDeclaration {
    /// The ABI parameter type (`uint256`).
    pub r#type: String,
    /// The bit layout, ascending and gapless.
    pub fields: Vec<ConfigFieldDeclaration>,
}

/// One packed config field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFieldDeclaration {
    /// The field name.
    pub name: String,
    /// The inclusive bit range `[low, high]`.
    pub bits: [u32; 2],
    /// The exact set of accepted values, when the field is an enumeration.
    #[serde(default)]
    pub allowed: Option<Vec<u64>>,
    /// The inclusive value range, when the field is a number.
    #[serde(default)]
    pub range: Option<[u64; 2]>,
    /// What the field means to the contract.
    pub semantics: String,
}

/// The `execute` ABI.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteAbi {
    /// The function name (`execute`).
    pub function: String,
    /// The canonical signature.
    pub signature: String,
    /// The 4-byte selector, as hex.
    pub selector: String,
    /// The ABI state mutability.
    pub state_mutability: String,
    /// The inputs, in order.
    pub inputs: Vec<AbiInputDeclaration>,
    /// The return shape.
    pub returns: AbiReturnDeclaration,
}

/// One `execute` input.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbiInputDeclaration {
    /// The input name.
    pub name: String,
    /// The ABI type.
    pub r#type: String,
    /// The ABI length bound, for `bytes`.
    #[serde(default)]
    pub max_length: Option<u32>,
    /// The ABI default value, for a defaulted argument.
    #[serde(default)]
    pub default: Option<String>,
    /// What the input means.
    pub semantics: String,
}

/// The `execute` return shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbiReturnDeclaration {
    /// The struct (or scalar) type name.
    pub r#type: String,
    /// The tuple fields, in order.
    pub fields: Vec<AbiReturnFieldDeclaration>,
}

/// One receipt field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbiReturnFieldDeclaration {
    /// The field name.
    pub name: String,
    /// The ABI type.
    pub r#type: String,
    /// What the field means.
    pub semantics: String,
}

/// One capability's off-chain name.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDeclaration {
    /// The on-chain ID.
    pub id: u32,
    /// The stable machine name.
    pub name: String,
    /// What the capability lets an encoder emit.
    pub description: String,
    /// The opcodes and config fields it covers (empty for config/receipt
    /// capabilities).
    #[serde(default)]
    pub provides: Vec<String>,
}

/// Where a release's runtime artifact and provenance live.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDeclaration {
    /// The ABI artifact (repository-relative).
    pub abi: String,
    /// The creation-code artifact.
    pub creation_code: String,
    /// The runtime-code artifact.
    pub runtime_code: String,
    /// The immutable layout artifact.
    pub immutables: String,
    /// The error-map artifact.
    pub error_map: String,
    /// The method-identifier artifact.
    pub method_identifiers: String,
    /// The provenance/integrity manifest that pins artifacts to sources.
    pub provenance: String,
    /// How a deployment is assembled from the artifact.
    pub deployment_form: String,
}

/// Why a release manifest is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// The JSON does not match the schema (including unknown keys).
    #[error("release manifest is not valid against the schema: {0}")]
    Json(String),
    /// The schema version is not this build's.
    #[error("release manifest schema_version {0} is not the supported version 1")]
    UnsupportedSchemaVersion(u32),
    /// The release tag is not a NUL-padded printable-ASCII tag.
    #[error("release manifest release_id {0:?} is not a NUL-padded printable-ASCII tag")]
    MalformedReleaseId(String),
    /// The protocol major is outside this build's bounded set.
    #[error("protocol major {major} is not one of this build's bounded majors {supported:?}")]
    UnsupportedProtocolMajor {
        /// The declared major.
        major: u32,
        /// The bounded set this build dispatches.
        supported: &'static [u32],
    },
    /// Zero versions name nothing.
    #[error("call_encoding.version and protocol.major must be non-zero")]
    ZeroVersion,
    /// An opcode or sentinel byte is not valid hex.
    #[error("{what} {value:?} is not a single hex byte")]
    MalformedByte {
        /// Which byte field is malformed.
        what: &'static str,
        /// The offending value.
        value: String,
    },
    /// Two opcodes share a byte.
    #[error("opcode {opcode:#04x} ({name}) is declared twice")]
    DuplicateOpcode {
        /// The duplicated opcode.
        opcode: u8,
        /// The duplicated name.
        name: String,
    },
    /// The opcode table is not ascending.
    #[error("opcode {opcode:#04x} ({name}) breaks ascending opcode order")]
    UnsortedOpcodes {
        /// The out-of-order opcode.
        opcode: u8,
        /// Its name.
        name: String,
    },
    /// An unknown field type.
    #[error("{owner} declares unknown field type {field_type:?} for field {field:?}")]
    UnknownFieldType {
        /// The opcode or entry that declares it.
        owner: String,
        /// The offending field.
        field: String,
        /// The unknown type name.
        field_type: String,
    },
    /// Duplicate field or capability names.
    #[error("{owner} declares {name:?} twice")]
    DuplicateName {
        /// The declaring opcode, entry, or manifest section.
        owner: String,
        /// The duplicated name.
        name: String,
    },
    /// Variable framing that does not match the declared fields.
    #[error("{owner} frames variable data inconsistently: {detail}")]
    InconsistentFraming {
        /// The opcode that is framed wrongly.
        owner: String,
        /// What is inconsistent.
        detail: String,
    },
    /// The packed config word is not fully and uniquely described.
    #[error("config word is not fully described: {detail}")]
    ConfigLayout {
        /// What is wrong with the layout.
        detail: String,
    },
    /// A config field's allowed value or range cannot fit its bit width.
    #[error("config field {field:?} declares values outside its {width}-bit width")]
    ConfigValueOutOfRange {
        /// The offending field.
        field: String,
        /// Its declared width in bits.
        width: u32,
    },
    /// The declared selector does not match the declared signature.
    #[error("execute selector {declared:?} does not match keccak256({signature:?})")]
    SelectorMismatch {
        /// The declared selector.
        declared: String,
        /// The declared signature.
        signature: String,
    },
    /// Capability IDs are not unique and ascending.
    #[error("capability IDs must be unique and ascending; found {id} after {previous:?}")]
    UnorderedCapabilities {
        /// The preceding ID, if any.
        previous: Option<u32>,
        /// The offending ID.
        id: u32,
    },
    /// A capability names an opcode the grammar does not declare.
    #[error("capability {name:?} names {provided:?}, which the command stream does not declare")]
    UnknownCapabilityOpcode {
        /// The capability name.
        name: String,
        /// The unknown name.
        provided: String,
    },
    /// An artifact path is not repository-relative.
    #[error("artifact path {path:?} is not a plain repository-relative path")]
    ArtifactPath {
        /// The offending path.
        path: String,
    },
}

impl ReleaseManifest {
    /// Parse and validate a release manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Json`] when the JSON does not match the schema
    /// and the specific [`ManifestError`] when validation refuses it.
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let manifest: Self =
            serde_json::from_str(json).map_err(|error| ManifestError::Json(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate every cross-reference in the manifest.
    ///
    /// # Errors
    ///
    /// Returns the specific [`ManifestError`] of the first contradiction found.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != 1 {
            return Err(ManifestError::UnsupportedSchemaVersion(self.schema_version));
        }
        ReleaseId::from_ascii_tag(&self.release_id)
            .map_err(|_| ManifestError::MalformedReleaseId(self.release_id.clone()))?;
        if !SUPPORTED_PROTOCOL_MAJORS.contains(&self.protocol.major) {
            return Err(ManifestError::UnsupportedProtocolMajor {
                major: self.protocol.major,
                supported: SUPPORTED_PROTOCOL_MAJORS,
            });
        }
        if self.call_encoding.version == 0 {
            return Err(ManifestError::ZeroVersion);
        }
        self.validate_address_resolution()?;
        let names = self.validate_command_stream()?;
        self.validate_config()?;
        self.validate_execute_abi()?;
        self.validate_capabilities(&names)?;
        self.validate_artifact()
    }

    fn validate_address_resolution(&self) -> Result<(), ManifestError> {
        let resolution = &self.call_encoding.address_resolution;
        if resolution.max_table_entries == 0
            || resolution.max_table_entries > u32::from(u8::MAX) / 2
        {
            return Err(ManifestError::ConfigLayout {
                detail: format!(
                    "max_table_entries {} is outside 1..=127",
                    resolution.max_table_entries
                ),
            });
        }
        let mut previous: Option<u8> = None;
        for sentinel in &resolution.sentinels {
            let id = parse_hex_byte("sentinel", &sentinel.id)?;
            if let Some(previous) = previous {
                if id <= previous {
                    return Err(ManifestError::ConfigLayout {
                        detail: format!("sentinel {id:#04x} breaks ascending sentinel order"),
                    });
                }
            }
            previous = Some(id);
            if sentinel.role.is_empty() {
                return Err(ManifestError::ConfigLayout {
                    detail: format!("sentinel {id:#04x} declares an empty role"),
                });
            }
        }
        Ok(())
    }

    /// Validate the opcode table, returning the declared opcode names.
    fn validate_command_stream(&self) -> Result<Vec<&str>, ManifestError> {
        let stream = &self.call_encoding.command_stream;
        let mut previous: Option<u8> = None;
        let mut names: Vec<&str> = Vec::with_capacity(stream.opcodes.len());
        let mut separators = 0;
        for opcode in &stream.opcodes {
            let code = parse_hex_byte("opcode", &opcode.opcode)?;
            match previous {
                Some(previous) if code == previous => {
                    return Err(ManifestError::DuplicateOpcode {
                        opcode: code,
                        name: opcode.name.clone(),
                    });
                }
                Some(previous) if code < previous => {
                    return Err(ManifestError::UnsortedOpcodes {
                        opcode: code,
                        name: opcode.name.clone(),
                    });
                }
                _ => {}
            }
            previous = Some(code);
            if names.contains(&opcode.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    owner: format!("opcode {code:#04x}"),
                    name: opcode.name.clone(),
                });
            }
            names.push(&opcode.name);
            if opcode.kind == OpcodeKind::Separator {
                separators += 1;
            }
            validate_opcode(opcode)?;
        }
        if separators != 1 {
            return Err(ManifestError::InconsistentFraming {
                owner: "command_stream".to_owned(),
                detail: format!("declares {separators} separators, expected exactly 1"),
            });
        }
        Ok(names)
    }

    fn validate_config(&self) -> Result<(), ManifestError> {
        let config = &self.call_encoding.config;
        if config.r#type != "uint256" {
            return Err(ManifestError::ConfigLayout {
                detail: format!(
                    "the packed config word must be uint256, found {:?}",
                    config.r#type
                ),
            });
        }
        let mut next_bit = 0u32;
        let mut names: Vec<&str> = Vec::with_capacity(config.fields.len());
        for field in &config.fields {
            let [low, high] = field.bits;
            if low != next_bit || high < low {
                return Err(ManifestError::ConfigLayout {
                    detail: format!(
                        "field {:?} occupies bits {low}..={high}, expected to start at {next_bit}",
                        field.name
                    ),
                });
            }
            if names.contains(&field.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    owner: "config".to_owned(),
                    name: field.name.clone(),
                });
            }
            names.push(&field.name);
            let width = high - low + 1;
            let limit = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            if let Some(allowed) = &field.allowed {
                if allowed.is_empty() || allowed.iter().any(|value| *value > limit) {
                    return Err(ManifestError::ConfigValueOutOfRange {
                        field: field.name.clone(),
                        width,
                    });
                }
            }
            if let Some([low_value, high_value]) = field.range {
                if low_value > high_value || high_value > limit {
                    return Err(ManifestError::ConfigValueOutOfRange {
                        field: field.name.clone(),
                        width,
                    });
                }
            }
            next_bit = high + 1;
        }
        if next_bit != 256 {
            return Err(ManifestError::ConfigLayout {
                detail: format!(
                    "config fields cover bits 0..={}, expected 0..=255",
                    next_bit - 1
                ),
            });
        }
        Ok(())
    }

    fn validate_execute_abi(&self) -> Result<(), ManifestError> {
        let execute = &self.call_encoding.execute_abi;
        let arguments: Vec<&str> = execute
            .inputs
            .iter()
            .map(|input| input.r#type.as_str())
            .collect();
        let signature = format!("{}({})", execute.function, arguments.join(","));
        if execute.signature != signature {
            return Err(ManifestError::ConfigLayout {
                detail: format!(
                    "execute signature {:?} does not match its inputs {:?}",
                    execute.signature, signature
                ),
            });
        }
        let digest = keccak256(execute.signature.as_bytes());
        let expected = format!(
            "0x{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        );
        if execute.selector.to_ascii_lowercase() != expected {
            return Err(ManifestError::SelectorMismatch {
                declared: execute.selector.clone(),
                signature: execute.signature.clone(),
            });
        }
        if execute.returns.fields.is_empty() {
            return Err(ManifestError::ConfigLayout {
                detail: "execute returns no receipt fields".to_owned(),
            });
        }
        let mut names: Vec<&str> = Vec::with_capacity(execute.returns.fields.len());
        for field in &execute.returns.fields {
            if names.contains(&field.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    owner: "execute_abi.returns".to_owned(),
                    name: field.name.clone(),
                });
            }
            names.push(&field.name);
            known_field_width(&field.r#type).ok_or_else(|| ManifestError::UnknownFieldType {
                owner: "execute_abi.returns".to_owned(),
                field: field.name.clone(),
                field_type: field.r#type.clone(),
            })?;
        }
        Ok(())
    }

    fn validate_capabilities(&self, opcode_names: &[&str]) -> Result<(), ManifestError> {
        if self.capabilities.is_empty() {
            return Err(ManifestError::ConfigLayout {
                detail: "the manifest declares no capabilities".to_owned(),
            });
        }
        if self.capabilities.len() > MAX_CAPABILITY_IDS {
            return Err(ManifestError::ConfigLayout {
                detail: format!(
                    "{} capabilities declared, above the on-chain bound {MAX_CAPABILITY_IDS}",
                    self.capabilities.len()
                ),
            });
        }
        let mut previous: Option<u32> = None;
        let mut names: Vec<&str> = Vec::with_capacity(self.capabilities.len());
        for capability in &self.capabilities {
            if capability.id == 0 || previous.is_some_and(|previous| capability.id <= previous) {
                return Err(ManifestError::UnorderedCapabilities {
                    previous,
                    id: capability.id,
                });
            }
            previous = Some(capability.id);
            if capability.name.is_empty()
                || !capability
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(ManifestError::ConfigLayout {
                    detail: format!(
                        "capability name {:?} is not a lowercase identifier",
                        capability.name
                    ),
                });
            }
            if names.contains(&capability.name.as_str()) {
                return Err(ManifestError::DuplicateName {
                    owner: "capabilities".to_owned(),
                    name: capability.name.clone(),
                });
            }
            names.push(&capability.name);
            for provided in &capability.provides {
                if !opcode_names.contains(&provided.as_str()) {
                    return Err(ManifestError::UnknownCapabilityOpcode {
                        name: capability.name.clone(),
                        provided: provided.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_artifact(&self) -> Result<(), ManifestError> {
        let artifact = &self.artifact;
        for path in [
            &artifact.abi,
            &artifact.creation_code,
            &artifact.runtime_code,
            &artifact.immutables,
            &artifact.error_map,
            &artifact.method_identifiers,
            &artifact.provenance,
        ] {
            if path.is_empty()
                || path.starts_with('/')
                || path.starts_with('\\')
                || path.contains('\\')
                || path.split('/').any(|segment| segment == "..")
            {
                return Err(ManifestError::ArtifactPath { path: path.clone() });
            }
        }
        if artifact.provenance == artifact.runtime_code {
            return Err(ManifestError::ArtifactPath {
                path: artifact.provenance.clone(),
            });
        }
        Ok(())
    }

    /// The capability declaration for `id`, if this manifest names it.
    #[must_use]
    pub fn capability(&self, id: CapabilityId) -> Option<&CapabilityDeclaration> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id.get())
    }

    /// The declared opcode, if this manifest declares it.
    #[must_use]
    pub fn opcode(&self, opcode: u8) -> Option<&OpcodeDeclaration> {
        self.call_encoding
            .command_stream
            .opcodes
            .iter()
            .find(|declared| parse_hex_byte("opcode", &declared.opcode) == Ok(opcode))
    }

    /// The declared base byte size of `opcode`: the opcode byte plus its fixed
    /// fields, with a length-prefixed variable tail counted at its base (the
    /// length prefix is itself a declared field) and a repeated block counted as
    /// its count field only.
    ///
    /// Returns `None` when the manifest does not declare the opcode or when one
    /// of its field types is unknown.
    #[must_use]
    pub fn declared_command_size(&self, opcode: u8) -> Option<u32> {
        let declared = self.opcode(opcode)?;
        fixed_size(&declared.fields).ok()
    }

    /// The declared byte size of one repeated entry, when `opcode` repeats one.
    #[must_use]
    pub fn declared_entry_size(&self, opcode: u8) -> Option<u32> {
        let repeat = self.opcode(opcode)?.repeat.as_ref()?;
        fields_width(&repeat.entry.fields).ok()
    }

    /// The on-chain metadata this manifest declares.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::MalformedReleaseId`] when the tag is not a valid
    /// release identity.
    pub fn declared_metadata(&self) -> Result<ExecutorMetadata, ManifestError> {
        Ok(ExecutorMetadata {
            release: ReleaseId::from_ascii_tag(&self.release_id)
                .map_err(|_| ManifestError::MalformedReleaseId(self.release_id.clone()))?,
            protocol_major: ProtocolMajor::new(self.protocol.major),
            call_encoding_version: CallEncodingVersion::new(self.call_encoding.version),
            capabilities: CapabilitySet::from_ids(
                &self
                    .capabilities
                    .iter()
                    .map(|capability| CapabilityId::new(capability.id))
                    .collect::<Vec<_>>(),
            ),
        })
    }

    /// Check an observed metadata declaration against this manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseMismatch`] on the first disagreement, including an
    /// observed protocol major outside this build's bounded set — an unknown
    /// major is a refusal, never "current".
    pub fn check_metadata(&self, observed: &ExecutorMetadata) -> Result<(), ReleaseMismatch> {
        if !observed.protocol_major.is_supported() {
            return Err(ReleaseMismatch::UnsupportedProtocolMajor {
                observed: observed.protocol_major.get(),
                supported: SUPPORTED_PROTOCOL_MAJORS,
            });
        }
        if observed.release.ascii_tag() != self.release_id {
            return Err(ReleaseMismatch::ReleaseId {
                declared: self.release_id.clone(),
                observed: observed.release.ascii_tag().to_owned(),
            });
        }
        if observed.protocol_major.get() != self.protocol.major {
            return Err(ReleaseMismatch::ProtocolMajor {
                declared: self.protocol.major,
                observed: observed.protocol_major.get(),
            });
        }
        if observed.call_encoding_version.get() != self.call_encoding.version {
            return Err(ReleaseMismatch::CallEncodingVersion {
                declared: self.call_encoding.version,
                observed: observed.call_encoding_version.get(),
            });
        }
        let declared: Vec<CapabilityId> = self
            .capabilities
            .iter()
            .map(|capability| CapabilityId::new(capability.id))
            .collect();
        if observed.capabilities.ids() != declared.as_slice() {
            return Err(ReleaseMismatch::Capabilities {
                declared,
                observed: observed.capabilities.ids().to_vec(),
            });
        }
        Ok(())
    }
}

/// Why an observed deployment does not match a release manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseMismatch {
    /// The release tags differ.
    #[error("deployment declares release {observed:?}, manifest describes {declared:?}")]
    ReleaseId {
        /// The manifest's release tag.
        declared: String,
        /// The deployment's release tag.
        observed: String,
    },
    /// The protocol majors differ.
    #[error("deployment declares protocol major {observed}, manifest describes {declared}")]
    ProtocolMajor {
        /// The manifest's major.
        declared: u32,
        /// The deployment's major.
        observed: u32,
    },
    /// The observed protocol major is outside this build's bounded set.
    #[error("protocol major {observed} is not one of this build's bounded majors {supported:?}")]
    UnsupportedProtocolMajor {
        /// The observed major.
        observed: u32,
        /// The bounded set this build dispatches.
        supported: &'static [u32],
    },
    /// The call-encoding versions differ.
    #[error("deployment declares call encoding version {observed}, manifest describes {declared}")]
    CallEncodingVersion {
        /// The manifest's version.
        declared: u32,
        /// The deployment's version.
        observed: u32,
    },
    /// The capability sets differ.
    #[error("deployment declares capabilities {observed:?}, manifest describes {declared:?}")]
    Capabilities {
        /// The manifest's IDs.
        declared: Vec<CapabilityId>,
        /// The deployment's IDs.
        observed: Vec<CapabilityId>,
    },
}

/// Every built-in release manifest source, in declaration order.
pub fn built_in_release_sources() -> impl Iterator<Item = &'static str> {
    [RELEASE_MANIFEST_V1].into_iter()
}

/// Look up a built-in release manifest by its declared tag.
///
/// # Errors
///
/// Returns [`ManifestError`] when an embedded manifest no longer validates —
/// a build defect, surfaced rather than swallowed.
pub fn built_in_release(tag: &str) -> Result<Option<ReleaseManifest>, ManifestError> {
    for source in built_in_release_sources() {
        let manifest = ReleaseManifest::parse(source)?;
        if manifest.release_id == tag {
            return Ok(Some(manifest));
        }
    }
    Ok(None)
}

/// The byte width of a declared field type.
#[must_use]
pub fn known_field_width(field_type: &str) -> Option<u32> {
    match field_type {
        "uint8" | "bool" => Some(1),
        "uint16" | "int16" => Some(2),
        "uint96" => Some(12),
        "uint256" => Some(32),
        "address" => Some(20),
        "bytes" => Some(0),
        _ => None,
    }
}

fn parse_hex_byte(what: &'static str, value: &str) -> Result<u8, ManifestError> {
    let body = value.strip_prefix("0x").unwrap_or(value);
    if body.len() != 2 {
        return Err(ManifestError::MalformedByte {
            what,
            value: value.to_owned(),
        });
    }
    u8::from_str_radix(body, 16).map_err(|_| ManifestError::MalformedByte {
        what,
        value: value.to_owned(),
    })
}

/// The sum of a field list's declared widths (`bytes` contributes 0: its length
/// prefix is a separate declared field).
fn fields_width(fields: &[FieldDeclaration]) -> Result<u32, ManifestError> {
    let mut width = 0u32;
    for field in fields {
        width +=
            known_field_width(&field.r#type).ok_or_else(|| ManifestError::UnknownFieldType {
                owner: "opcode".to_owned(),
                field: field.name.clone(),
                field_type: field.r#type.clone(),
            })?;
    }
    Ok(width)
}

/// The fixed byte size of a command: its opcode byte plus its fields.
fn fixed_size(fields: &[FieldDeclaration]) -> Result<u32, ManifestError> {
    Ok(1 + fields_width(fields)?)
}

fn validate_opcode(opcode: &OpcodeDeclaration) -> Result<(), ManifestError> {
    let owner = format!("opcode {} ({})", opcode.opcode, opcode.name);
    validate_field_list(&owner, &opcode.fields)?;
    validate_framing(&owner, opcode)?;
    if let Some(repeat) = &opcode.repeat {
        validate_repeat(&owner, opcode, repeat)?;
    }
    Ok(())
}

/// Every field names a known type and appears once.
fn validate_field_list(owner: &str, fields: &[FieldDeclaration]) -> Result<(), ManifestError> {
    let mut names: Vec<&str> = Vec::with_capacity(fields.len());
    for field in fields {
        if known_field_width(&field.r#type).is_none() {
            return Err(ManifestError::UnknownFieldType {
                owner: owner.to_owned(),
                field: field.name.clone(),
                field_type: field.r#type.clone(),
            });
        }
        if names.contains(&field.name.as_str()) {
            return Err(ManifestError::DuplicateName {
                owner: owner.to_owned(),
                name: field.name.clone(),
            });
        }
        names.push(&field.name);
    }
    Ok(())
}

/// A length-prefixed tail names a declared `uint8` length and a declared `bytes`
/// payload, and a `bytes` field never appears without one.
fn validate_framing(owner: &str, opcode: &OpcodeDeclaration) -> Result<(), ManifestError> {
    let variable_fields = opcode
        .fields
        .iter()
        .filter(|field| field.r#type == "bytes")
        .count();
    let Some(tail) = &opcode.variable_tail else {
        return if variable_fields == 0 {
            Ok(())
        } else {
            Err(ManifestError::InconsistentFraming {
                owner: owner.to_owned(),
                detail: "declares a bytes field without a length-prefixed tail".to_owned(),
            })
        };
    };
    if variable_fields != 1 {
        return Err(ManifestError::InconsistentFraming {
            owner: owner.to_owned(),
            detail: format!(
                "declares variable tail {:?} without exactly one bytes field",
                tail.data_field
            ),
        });
    }
    if !opcode
        .fields
        .iter()
        .any(|field| field.name == tail.length_field && field.r#type == "uint8")
    {
        return Err(ManifestError::InconsistentFraming {
            owner: owner.to_owned(),
            detail: format!(
                "length field {:?} is not a declared uint8",
                tail.length_field
            ),
        });
    }
    if !opcode
        .fields
        .iter()
        .any(|field| field.name == tail.data_field && field.r#type == "bytes")
    {
        return Err(ManifestError::InconsistentFraming {
            owner: owner.to_owned(),
            detail: format!(
                "data field {:?} is not a declared bytes field",
                tail.data_field
            ),
        });
    }
    Ok(())
}

/// A repeated entry names a declared `uint8` count field with a non-zero bound,
/// and its entry fields are known and unique.
fn validate_repeat(
    owner: &str,
    opcode: &OpcodeDeclaration,
    repeat: &RepeatDeclaration,
) -> Result<(), ManifestError> {
    if !opcode
        .fields
        .iter()
        .any(|field| field.name == repeat.count_field && field.r#type == "uint8")
    {
        return Err(ManifestError::InconsistentFraming {
            owner: owner.to_owned(),
            detail: format!(
                "count field {:?} is not a declared uint8",
                repeat.count_field
            ),
        });
    }
    if repeat.max_count == 0 {
        return Err(ManifestError::InconsistentFraming {
            owner: owner.to_owned(),
            detail: "declares a zero repeat bound".to_owned(),
        });
    }
    validate_field_list(owner, &repeat.entry.fields)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::pack_config;
    use crate::encoders::{
        enc_erc20_transfer, enc_erc20_xfer_balance, enc_preamble, enc_send_eth, enc_send_eth_all,
        enc_set_address, enc_v2_swap_calc, enc_v2_swap_compact, enc_v2_swap_direct,
        enc_v3_swap_compact, enc_v3_swap_delta, enc_v4_batch, enc_v4_batch_open_weth,
        enc_v4_burn_compact, enc_v4_mint_compact, enc_v4_settle, enc_v4_settle_all,
        enc_v4_settle_delta, enc_v4_swap_compact, enc_v4_swap_dynamic, enc_v4_sync, enc_v4_take,
        enc_v4_take_compact, enc_v4_take_delta, enc_v4_unlock, enc_weth_deposit,
        enc_weth_deposit_all, enc_weth_withdraw, enc_weth_withdraw_all, AddressTable, V4BatchEntry,
    };
    use alloy::primitives::{address, Address, U256};
    use serde_json::{json, Value};

    /// Opcodes the contract decodes but the Rust encoder surface does not emit.
    ///
    /// They are hand-written streams (the direction-baked V3 variants), so the
    /// encoder cross-check below cannot size them; the Python encoding helpers
    /// in `tests/executor/test_release_metadata.py` cross-check instead.
    const CONTRACT_ONLY_OPCODES: &[&str] = &["V3_SWAP_ONE_FOR_ZERO", "V3_SWAP_ZERO_FOR_ONE"];

    fn current_manifest() -> ReleaseManifest {
        ReleaseManifest::parse(RELEASE_MANIFEST_V1).expect("committed release manifest validates")
    }

    /// One ABI word holding `value`.
    fn word(value: u64) -> Vec<u8> {
        U256::from(value).to_be_bytes::<32>().to_vec()
    }

    fn tag_word(tag: &str) -> Vec<u8> {
        ReleaseId::from_ascii_tag(tag).unwrap().as_bytes().to_vec()
    }

    fn capabilities_array(ids: &[u32]) -> Vec<u8> {
        let mut out = word(32);
        out.extend(word(u64::try_from(ids.len()).unwrap()));
        for id in ids {
            out.extend(word(u64::from(*id)));
        }
        out
    }

    /// The four getter answers a faithful deployment of the current release
    /// gives.
    struct Answers {
        release_id: Vec<u8>,
        protocol_major: Vec<u8>,
        call_encoding_version: Vec<u8>,
        capabilities: Vec<u8>,
    }

    impl Answers {
        fn declared() -> Self {
            let manifest = current_manifest();
            Self {
                release_id: tag_word(&manifest.release_id),
                protocol_major: word(u64::from(manifest.protocol.major)),
                call_encoding_version: word(u64::from(manifest.call_encoding.version)),
                capabilities: capabilities_array(
                    &manifest
                        .capabilities
                        .iter()
                        .map(|capability| capability.id)
                        .collect::<Vec<_>>(),
                ),
            }
        }

        fn observations(&self) -> GetterObservations<'_> {
            GetterObservations::from_parts(
                Some(&self.release_id),
                Some(&self.protocol_major),
                Some(&self.call_encoding_version),
                Some(&self.capabilities),
            )
        }
    }

    // ── On-chain: declared values ──────────────────────────────────────────

    #[test]
    fn declared_metadata_decodes_from_exact_abi_data() {
        let manifest = current_manifest();
        let answers = Answers::declared();

        let decoded = decode_metadata(answers.observations()).unwrap();

        assert_eq!(decoded.release.ascii_tag(), RELEASE_ID_TAG);
        assert_eq!(decoded.protocol_major.get(), 1);
        assert_eq!(decoded.call_encoding_version.get(), 1);
        assert_eq!(
            decoded.capabilities.raw_ids().collect::<Vec<_>>(),
            (1..=15).collect::<Vec<_>>()
        );
        // The manifest describes exactly what the getters declared.
        manifest.check_metadata(&decoded).unwrap();
        assert_eq!(manifest.declared_metadata().unwrap(), decoded);
    }

    #[test]
    fn a_release_tag_round_trips_through_bytes32_padding() {
        for tag in ["cmd_executor/1", "cmd_executor/17", "x"] {
            let id = ReleaseId::from_ascii_tag(tag).unwrap();
            let decoded = ReleaseId::from_bytes(*id.as_bytes()).unwrap();
            assert_eq!(decoded, id);
            assert_eq!(decoded.ascii_tag(), tag);
            assert_eq!(format!("{decoded}"), tag);
            assert_eq!(id.as_bytes()[tag.len()], 0);
        }
        assert_eq!(ReleaseId::from_ascii_tag(""), Err(ReleaseIdError::Empty));
        assert_eq!(
            ReleaseId::from_ascii_tag(&"a".repeat(33)),
            Err(ReleaseIdError::TooLong(33))
        );
        assert_eq!(
            ReleaseId::from_ascii_tag("cmd\0executor"),
            Err(ReleaseIdError::InteriorNul)
        );
        assert_eq!(
            ReleaseId::from_ascii_tag("cmd\u{7f}executor"),
            Err(ReleaseIdError::NotAscii)
        );
        let padded = *ReleaseId::from_ascii_tag("cmd_executor/1")
            .unwrap()
            .as_bytes();
        assert!(ReleaseId::from_bytes(padded).is_ok());
        let mut trailing_nonzero = padded;
        trailing_nonzero[31] = 0xff;
        assert_eq!(
            ReleaseId::from_bytes(trailing_nonzero),
            Err(ReleaseIdError::NotAscii)
        );
    }

    // ── On-chain: strict rejection ─────────────────────────────────────────

    #[test]
    fn absent_getters_are_never_a_declaration() {
        assert_eq!(
            decode_metadata(GetterObservations::all_absent()).unwrap_err(),
            MetadataDecodeError::PartialAnswers { answered: 0 }
        );
        let answers = Answers::declared();
        let partial =
            GetterObservations::from_parts(None, Some(&answers.protocol_major), None, None);
        assert_eq!(
            decode_metadata(partial).unwrap_err(),
            MetadataDecodeError::PartialAnswers { answered: 1 }
        );
    }

    #[test]
    fn short_and_trailing_data_are_rejected() {
        assert_eq!(
            decode_release_id(&[0u8; 31]).unwrap_err(),
            MetadataDecodeError::Length {
                getter: "release_id",
                expected: 32,
                actual: 31,
            }
        );
        let mut trailing = tag_word(RELEASE_ID_TAG);
        trailing.push(0);
        assert_eq!(
            decode_release_id(&trailing).unwrap_err(),
            MetadataDecodeError::Length {
                getter: "release_id",
                expected: 32,
                actual: 33,
            }
        );
        assert_eq!(
            decode_protocol_major(&[]).unwrap_err(),
            MetadataDecodeError::Length {
                getter: "protocol_major",
                expected: 32,
                actual: 0,
            }
        );
        assert_eq!(
            decode_capabilities(&word(32)).unwrap_err(),
            MetadataDecodeError::Length {
                getter: "capabilities",
                expected: 64,
                actual: 32,
            }
        );
        let mut trailing = capabilities_array(&[1, 2]);
        trailing.push(0);
        assert_eq!(
            decode_capabilities(&trailing).unwrap_err(),
            MetadataDecodeError::Length {
                getter: "capabilities",
                expected: 128,
                actual: 129,
            }
        );
    }

    #[test]
    fn non_canonical_words_are_rejected() {
        let mut high_bytes = word(1);
        high_bytes[0] = 1;
        assert!(matches!(
            decode_protocol_major(&high_bytes),
            Err(MetadataDecodeError::OutOfRange { .. })
        ));
        assert_eq!(
            decode_protocol_major(&word(0)).unwrap_err(),
            MetadataDecodeError::Zero {
                getter: "protocol_major"
            }
        );
        assert_eq!(
            decode_call_encoding_version(&word(0)).unwrap_err(),
            MetadataDecodeError::Zero {
                getter: "call_encoding_version"
            }
        );
        assert_eq!(
            decode_release_id(&[0xff; 32]).unwrap_err(),
            MetadataDecodeError::MalformedReleaseId
        );
        assert_eq!(
            decode_release_id(&word(0)).unwrap_err(),
            MetadataDecodeError::MalformedReleaseId
        );
    }

    #[test]
    fn capability_array_shape_is_strict() {
        // Non-canonical head offset.
        let mut shifted = capabilities_array(&[1]);
        shifted[..32].copy_from_slice(&word(64));
        assert!(matches!(
            decode_capabilities(&shifted),
            Err(MetadataDecodeError::OutOfRange { .. })
        ));

        // Above the contract's ABI bound.
        let over_bound = capabilities_array(&(1..=33).collect::<Vec<_>>());
        assert_eq!(
            decode_capabilities(&over_bound).unwrap_err(),
            MetadataDecodeError::TooManyCapabilities {
                count: 33,
                max: MAX_CAPABILITY_IDS,
            }
        );

        // An empty declaration names nothing.
        assert_eq!(
            decode_capabilities(&capabilities_array(&[])).unwrap_err(),
            MetadataDecodeError::Zero {
                getter: "capabilities"
            }
        );

        // Duplicate and descending IDs both break the declared order.
        assert_eq!(
            decode_capabilities(&capabilities_array(&[1, 1])).unwrap_err(),
            MetadataDecodeError::UnorderedCapabilities { previous: 1, id: 1 }
        );
        assert_eq!(
            decode_capabilities(&capabilities_array(&[2, 1])).unwrap_err(),
            MetadataDecodeError::UnorderedCapabilities { previous: 2, id: 1 }
        );

        // A zero ID names no capability.
        assert_eq!(
            decode_capabilities(&capabilities_array(&[0])).unwrap_err(),
            MetadataDecodeError::Zero {
                getter: "capabilities"
            }
        );

        // A non-canonical element word.
        let mut dirty = capabilities_array(&[1]);
        dirty[64] = 1;
        assert!(matches!(
            decode_capabilities(&dirty),
            Err(MetadataDecodeError::OutOfRange { .. })
        ));
    }

    #[test]
    fn unknown_capability_ids_survive_verbatim() {
        let declared = [1, 4242, u32::MAX];
        let set = decode_capabilities(&capabilities_array(&declared)).unwrap();

        assert_eq!(set.raw_ids().collect::<Vec<_>>(), declared);
        assert!(set.contains(CapabilityId::new(4242)));
        assert_eq!(CapabilityId::new(u32::MAX).get(), u32::MAX);

        // The manifest names a capability only when it knows the ID; an unknown
        // ID is preserved, not refused.
        let manifest = current_manifest();
        assert!(manifest.capability(CapabilityId::new(4242)).is_none());
        assert_eq!(
            manifest.capability(CapabilityId::new(1)).unwrap().name,
            "address_table"
        );
    }

    // ── Legacy mapping semantics ───────────────────────────────────────────

    #[test]
    fn getterless_deployments_need_an_explicit_legacy_mapping() {
        let deployment = address!("1111111111111111111111111111111111111111");
        let elsewhere = address!("2222222222222222222222222222222222222222");
        let legacy_release = ReleaseId::from_ascii_tag("cmd_executor/0").unwrap();
        let mapping = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
            address: deployment,
            release: legacy_release,
        }]);

        // Silence from every getter names nothing: not "latest", not "release
        // 1", just unknown.
        assert_eq!(
            classify_deployment(deployment, GetterObservations::all_absent(), None).unwrap(),
            DeploymentRelease::Unknown
        );

        // The operator's mapping is the only way that deployment acquires a name.
        assert_eq!(
            classify_deployment(deployment, GetterObservations::all_absent(), Some(&mapping))
                .unwrap(),
            DeploymentRelease::Legacy {
                release: legacy_release
            }
        );

        // A mapping entry for another address names nothing here.
        assert_eq!(
            classify_deployment(elsewhere, GetterObservations::all_absent(), Some(&mapping))
                .unwrap(),
            DeploymentRelease::Unknown
        );

        // A declaring deployment is Declared, and the mapping cannot relabel it.
        let answers = Answers::declared();
        assert!(matches!(
            classify_deployment(deployment, answers.observations(), Some(&mapping)).unwrap(),
            DeploymentRelease::Declared(_)
        ));

        // Partial answers are neither declared nor getterless; the mapping is
        // not a rescue for a half-declaring deployment.
        let partial = GetterObservations::from_parts(
            Some(&answers.release_id),
            Some(&answers.protocol_major),
            Some(&answers.call_encoding_version),
            None,
        );
        assert_eq!(
            classify_deployment(deployment, partial, Some(&mapping)).unwrap_err(),
            MetadataDecodeError::PartialAnswers { answered: 3 }
        );
    }

    // ── Manifest: the complete encoding description ────────────────────────

    #[test]
    fn manifest_validates_the_complete_call_encoding() {
        let manifest = current_manifest();

        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.release_id, RELEASE_ID_TAG);
        assert_eq!(manifest.protocol.major, 1);
        assert_eq!(manifest.call_encoding.version, 1);
        assert_eq!(manifest.call_encoding.max_stream_length, 288);
        assert_eq!(
            manifest.call_encoding.address_resolution.max_table_entries,
            32
        );

        // Every opcode the contract dispatches is described exactly once, and
        // all three families are present.
        let opcodes = &manifest.call_encoding.command_stream.opcodes;
        assert_eq!(opcodes.len(), 31);
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| opcode.kind == OpcodeKind::Separator)
                .count(),
            1
        );
        assert!(
            opcodes
                .iter()
                .filter(|opcode| opcode.kind == OpcodeKind::Execute)
                .count()
                > 25
        );

        // The config word is fully described, with the declared enumerations.
        let fields = &manifest.call_encoding.config.fields;
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].name, "profit_mode");
        assert_eq!(fields[0].allowed.as_deref(), Some(&[1, 2][..]));
        assert_eq!(fields[3].bits, [32, 255]);

        // The execute ABI is the declared one, selector included.
        let execute = &manifest.call_encoding.execute_abi;
        assert_eq!(execute.signature, "execute(bytes,uint256)");
        assert_eq!(execute.selector, "0xab5898e8");
        assert_eq!(execute.state_mutability, "payable");
        assert_eq!(
            execute
                .returns
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["gross_profit", "bribe_paid"]
        );

        // Artifact location is named; integrity lives in the provenance file.
        assert!(manifest
            .artifact
            .runtime_code
            .ends_with("cmd_executor.runtime.hex"));
        assert_ne!(manifest.artifact.provenance, manifest.artifact.runtime_code);
    }

    #[test]
    fn manifest_rejects_contradictory_declarations() {
        fn revalidate(mutate: impl FnOnce(&mut Value)) -> ManifestError {
            let mut value: Value = serde_json::from_str(RELEASE_MANIFEST_V1).unwrap();
            mutate(&mut value);
            ReleaseManifest::parse(&value.to_string()).unwrap_err()
        }

        // Unknown keys are hard errors, in any section.
        assert!(matches!(
            revalidate(|manifest| manifest["unexpected"] = json!(1)),
            ManifestError::Json(_)
        ));
        // ... including a whole-bytecode hash masquerading as release identity.
        assert!(matches!(
            revalidate(|manifest| manifest["artifact"]["runtime_sha256"] = json!("00")),
            ManifestError::Json(_)
        ));
        // An unknown major is refused, never assumed current.
        assert_eq!(
            revalidate(|manifest| manifest["protocol"]["major"] = json!(7)),
            ManifestError::UnsupportedProtocolMajor {
                major: 7,
                supported: SUPPORTED_PROTOCOL_MAJORS,
            }
        );
        assert_eq!(
            revalidate(|manifest| manifest["call_encoding"]["version"] = json!(0)),
            ManifestError::ZeroVersion
        );
        assert_eq!(
            revalidate(|manifest| manifest["release_id"] = json!("cmd\u{7f}executor")),
            ManifestError::MalformedReleaseId("cmd\u{7f}executor".to_owned())
        );
        // Duplicate and out-of-order opcodes.
        assert!(matches!(
            revalidate(|manifest| {
                let opcode = manifest["call_encoding"]["command_stream"]["opcodes"][1].clone();
                manifest["call_encoding"]["command_stream"]["opcodes"][2] = opcode;
            }),
            ManifestError::DuplicateOpcode { opcode: 0x10, .. }
        ));
        assert!(matches!(
            revalidate(|manifest| {
                let opcodes = &mut manifest["call_encoding"]["command_stream"]["opcodes"];
                let first = opcodes[1].clone();
                opcodes[1] = opcodes[2].clone();
                opcodes[2] = first;
            }),
            ManifestError::UnsortedOpcodes { .. }
        ));
        // A config field that leaves a hole in the word.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["call_encoding"]["config"]["fields"][3]["bits"] = json!([33, 255]);
            }),
            ManifestError::ConfigLayout { .. }
        ));
        // A config enumeration value that cannot fit its width.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["call_encoding"]["config"]["fields"][1]["range"] = json!([0, 70_000]);
            }),
            ManifestError::ConfigValueOutOfRange { .. }
        ));
        // A selector that disagrees with the signature.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["call_encoding"]["execute_abi"]["selector"] = json!("0x00000000");
            }),
            ManifestError::SelectorMismatch { .. }
        ));
        // A capability claiming an opcode the grammar does not declare.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["capabilities"][0]["provides"] = json!(["NOT_AN_OPCODE"]);
            }),
            ManifestError::UnknownCapabilityOpcode { .. }
        ));
        // A capability ID that breaks the ascending, unique order.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["capabilities"][1]["id"] = json!(1);
            }),
            ManifestError::UnorderedCapabilities { id: 1, .. }
        ));
        // A variable tail that does not reference a declared field.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["call_encoding"]["command_stream"]["opcodes"][9]["variable_tail"]
                    ["length_field"] = json!("nope");
            }),
            ManifestError::InconsistentFraming { .. }
        ));
        // An artifact path that escapes the repository.
        assert!(matches!(
            revalidate(|manifest| {
                manifest["artifact"]["runtime_code"] = json!("/etc/passwd");
            }),
            ManifestError::ArtifactPath { .. }
        ));
    }

    #[test]
    fn check_metadata_refuses_unknown_and_contradictory_deployments() {
        let manifest = current_manifest();
        let declared = manifest.declared_metadata().unwrap();

        // An observed major this build does not dispatch is a refusal.
        let mut unknown_major = declared.clone();
        unknown_major.protocol_major = ProtocolMajor::new(2);
        assert_eq!(
            manifest.check_metadata(&unknown_major).unwrap_err(),
            ReleaseMismatch::UnsupportedProtocolMajor {
                observed: 2,
                supported: SUPPORTED_PROTOCOL_MAJORS,
            }
        );

        let mut other_release = declared.clone();
        other_release.release = ReleaseId::from_ascii_tag("cmd_executor/2").unwrap();
        assert!(matches!(
            manifest.check_metadata(&other_release),
            Err(ReleaseMismatch::ReleaseId { .. })
        ));

        let mut other_encoding = declared.clone();
        other_encoding.call_encoding_version = CallEncodingVersion::new(2);
        assert!(matches!(
            manifest.check_metadata(&other_encoding),
            Err(ReleaseMismatch::CallEncodingVersion { .. })
        ));

        let mut other_capabilities = declared.clone();
        other_capabilities.capabilities = CapabilitySet::from_ids(&[CapabilityId::new(1)]);
        assert!(matches!(
            manifest.check_metadata(&other_capabilities),
            Err(ReleaseMismatch::Capabilities { .. })
        ));

        manifest.check_metadata(&declared).unwrap();
    }

    #[test]
    fn built_in_release_lookup_is_by_declared_tag() {
        assert!(built_in_release(RELEASE_ID_TAG).unwrap().is_some());
        assert!(built_in_release("cmd_executor/99").unwrap().is_none());
        assert_eq!(built_in_release_sources().count(), 1);
    }

    // ── Command bytes stay pinned to the declared grammar ──────────────────

    /// One sample encoding per opcode the Rust surface emits, with an empty
    /// variable tail so the length is the declared base size.
    fn encoder_sample(name: &str) -> Option<Vec<u8>> {
        let entry = V4BatchEntry {
            c0_idx: 1,
            c1_idx: 2,
            fee: 3000,
            tick_spacing: 60,
            hooks_idx: 0xff,
            zfo: true,
            amount_u96: 1_000,
        };
        Some(match name {
            "SET_ADDRESS" => enc_set_address(Address::repeat_byte(0x11)),
            "ERC20_TRANSFER" => enc_erc20_transfer(1, 2, 1_000).unwrap(),
            "ERC20_XFER_BALANCE" => enc_erc20_xfer_balance(1, 2),
            "WETH_DEPOSIT" => enc_weth_deposit(U256::from(1u64)),
            "WETH_WITHDRAW" => enc_weth_withdraw(U256::from(1u64)),
            "WETH_DEPOSIT_ALL" => enc_weth_deposit_all(),
            "WETH_WITHDRAW_ALL" => enc_weth_withdraw_all(),
            "SEND_ETH" => enc_send_eth(1, 1_000).unwrap(),
            "SEND_ETH_ALL" => enc_send_eth_all(1),
            "V2_SWAP_COMPACT" => enc_v2_swap_compact(1, true, 1_000, 2, 30, &[]).unwrap(),
            "V2_SWAP_CALC" => enc_v2_swap_calc(1, true, 2, 30),
            "V2_SWAP_DIRECT" => enc_v2_swap_direct(1, true, 1_000, 2).unwrap(),
            "V3_SWAP_COMPACT" => enc_v3_swap_compact(1, true, 1_000, 2, &[]).unwrap(),
            "V3_SWAP_DELTA" => enc_v3_swap_delta(1, true, 2),
            "V4_SWAP_COMPACT" => enc_v4_swap_compact(1, 2, 3000, 60, 0xff, true, 1_000).unwrap(),
            "V4_SWAP_DYNAMIC" => enc_v4_swap_dynamic(1, 2, 3000, 60, 0xff, true),
            "V4_BATCH" => enc_v4_batch(&[entry]).unwrap(),
            "V4_BATCH_OPEN_WETH" => enc_v4_batch_open_weth(&[entry]).unwrap(),
            "V4_UNLOCK" => enc_v4_unlock(&[]).unwrap(),
            "V4_TAKE" => enc_v4_take(1, 2, U256::from(1u64)),
            "V4_TAKE_COMPACT" => enc_v4_take_compact(1, 2, 1_000).unwrap(),
            "V4_TAKE_DELTA" => enc_v4_take_delta(1, 2),
            "V4_SYNC" => enc_v4_sync(1),
            "V4_SETTLE" => enc_v4_settle(),
            "V4_SETTLE_DELTA" => enc_v4_settle_delta(1),
            "V4_SETTLE_ALL" => enc_v4_settle_all(),
            "V4_MINT_COMPACT" => enc_v4_mint_compact(1, 2, 1_000).unwrap(),
            "V4_BURN_COMPACT" => enc_v4_burn_compact(1, 1_000).unwrap(),
            "BEGIN_EXECUTION" => vec![crate::encoders::BEGIN_EXECUTION],
            _ => return None,
        })
    }

    #[test]
    fn declared_opcode_sizes_match_the_encoders() {
        let manifest = current_manifest();
        let mut cross_checked = 0usize;

        for declared in &manifest.call_encoding.command_stream.opcodes {
            let Some(bytes) = encoder_sample(&declared.name) else {
                assert!(
                    CONTRACT_ONLY_OPCODES.contains(&declared.name.as_str()),
                    "{} declares no Rust encoder and is not a known contract-only opcode",
                    declared.name
                );
                continue;
            };

            let opcode = bytes[0];
            let described = manifest.opcode(opcode).unwrap_or_else(|| {
                panic!("{} emitted undeclared opcode {opcode:#04x}", declared.name)
            });
            assert_eq!(
                described.name, declared.name,
                "opcode {opcode:#04x} name mismatch"
            );
            // The sample carries one repeated entry (for a repeating opcode)
            // and an empty variable tail (for a tailed one).
            let expected = manifest.declared_command_size(opcode).unwrap()
                + manifest.declared_entry_size(opcode).unwrap_or(0);
            assert_eq!(
                u32::try_from(bytes.len()).unwrap(),
                expected,
                "{} declared size disagrees with the encoder",
                declared.name
            );
            cross_checked += 1;
        }

        assert_eq!(
            cross_checked,
            manifest.call_encoding.command_stream.opcodes.len() - CONTRACT_ONLY_OPCODES.len()
        );
    }

    #[test]
    fn current_command_bytes_stay_pinned() {
        let mut table = AddressTable::new();
        let index = table
            .add(address!("1111111111111111111111111111111111111111"))
            .unwrap();
        assert_eq!(index, 0);

        // [SET_ADDRESS(0x11…11)][0xFF][V4_SETTLE_ALL]
        let mut stream = enc_preamble(&table);
        assert_eq!(
            alloy::hex::encode(&stream),
            "001111111111111111111111111111111111111111ff"
        );
        stream.extend(enc_v4_settle_all());
        assert_eq!(
            alloy::hex::encode(&stream),
            "001111111111111111111111111111111111111111ff57"
        );

        // The packed config word is the release's, field for field.
        let config = pack_config(2, U256::from(1_000u64), 500, 1).unwrap();
        assert_eq!(format!("{config:#x}"), "0x3e80101f402");
    }
}
