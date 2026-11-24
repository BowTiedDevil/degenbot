//! Golden-capture cassettes and the recording transport (ADR-068 D1/D2).
//!
//! A **cassette** is the wire-form half of a golden capture: the ordered RPC
//! `(method, canonical params) → raw JSON response` ledger for a pinned block
//! span, replayable in place of a live node. Schema v1 is additive to the
//! [`crate::offline`] recorded JSON — it carries a per-capture `chain_id`,
//! provenance (`source`, `recorded_at`, the pinned `span`), and the entry
//! ledger — while [`crate::offline`] parsing stays untouched.
//!
//! # Canonical entries (the drift-gate precondition)
//!
//! Entry keys canonicalize params so regeneration is **byte-identical**:
//!
//! - object keys are sorted recursively;
//! - hex quantity strings are normalized to decimal under a precision rule
//!   ("decimal block keys"): ONLY minimal-form quantities decimalize — a
//!   `0x` prefix followed by 1..=16 hex digits with no leading zero digit,
//!   with `0x0` the canonical zero — and that decimalization is exact
//!   (replay re-hexes the decimal to the same minimal wire form). A
//!   leading-zero digit marks a NON-minimal string — byte-hex, not a
//!   quantity (`0x00`, `0x00000000`, `0x06fdde03`) — which decimalizing
//!   would mangle (`0x00000000` → `0` → replayed `0x0`: different bytes,
//!   in an odd-length form `Bytes` decoding rejects), so non-minimal hex
//!   is preserved verbatim;
//! - everything else recorded on the wire stays verbatim: addresses and data
//!   hex are NOT rewritten (the wire form is already canonical for them),
//!   and ≥17-digit hex never parsed as a quantity anyway;
//! - entries live in a `BTreeMap`, so the ledger serializes sorted by
//!   `(method, canonical params)`.
//!
//! Replay restores the wire form byte-exactly for both classes
//! (`cassette_replay::wire_value`). The residual — deciding
//! quantity-vs-data by FIELD instead of by shape — is the v2
//! field-aware-canonicalization note (see
//! `docs/updater-rpc-sql-survey.md`, "Cassette hex canonicalization").
//!
//! Responses are canonicalized the same way, so any byte-level mutation of a
//! recorded response changes the canonical bytes — the drift gate
//! ([`verify_cassette_bytes`]) goes red.
//!
//! The pipeline that writes a capture ([`RecordingTransport`] flushed through
//! [`Cassette::canonical_bytes`]) is the same pipeline the gate runs.
//!
//! # Artifacts
//!
//! A committed cassette is a **package**: the canonical bytes above, framed
//! with zstd as `<name>.json.zst`. `write_artifact` is the only writer and
//! `read_artifact` the only reader, so every recorder and every replay suite
//! inherits the packaging instead of choosing it — recorded receipt and trace
//! payloads are highly redundant (the largest committed capture measures
//! 107 MB decoded), which is what keeps the corpus below a git host's
//! per-file limit. The package is transport, not format: `verify_artifact`
//! gates the DECODED canonical bytes, so a libzstd that frames the same
//! content differently never reddens a gate. A plain `.json` beside a package
//! wins, because that is the artifact a recorder just produced.
//!
//! The codec lives behind the non-default `cassette-artifacts` feature, the
//! same lane shape `degenbot-pools` gives its capture fixtures (ADR-084 D3):
//! the pure-Rust tree compiles no zstd, and the recording, replay, and
//! drift-gate lanes opt in.
//!
//! A capture's `eth_getBlockByNumber` answers are chain evidence rather than
//! payload: a recorded run resolves its blocks through exactly the reads a live
//! run does, and [`Cassette::recorded_headers`] reads them back as the linked
//! [`ChainAnchor`] chain the capture proves (ADR-105 D1/D2).
//!
//! The recorder that drives it is the non-publishable fixture tool
//! `record_updater_cassette` in `degenbot-fixture-tools`
//! (`rust/tools/fixture-tools/src/bin/record_updater_cassette/main.rs`); it needs
//! no feature lane, and its `--check <cassette.json[.zst]>...` arm is the offline gate that
//! runs [`verify_cassette_bytes`] over the committed corpus.

use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(feature = "cassette-artifacts")]
use std::path::{Path, PathBuf};

use alloy::primitives::B256;
use alloy::providers::{Provider, RootProvider};
use alloy::pubsub::PubSubConnect as _;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{BoxTransport, Transport, TransportFut};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::AlloyProvider;
use degenbot_core::canonical_head::ChainAnchor;

/// The schema marker written into every v1 cassette.
pub const CASSETTE_SCHEMA_V1: &str = "degenbot.cassette/v1";

/// A golden-capture cassette (schema v1).
///
/// Parse with [`Cassette::from_json_bytes`]; the committed artifact must
/// equal [`Cassette::canonical_bytes`] (see [`verify_cassette_bytes`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    /// The schema marker — [`CASSETTE_SCHEMA_V1`] on any current capture.
    pub schema: String,
    /// The chain the capture was taken against (`eth_chainId`).
    pub chain_id: u64,
    /// Where and when the capture was taken, and the pinned block span.
    pub provenance: CassetteProvenance,
    /// The `(method, canonical params) → response` ledger. Ordered by the
    /// canonical key, so regeneration is byte-identical (the drift-gate
    /// precondition).
    pub entries: BTreeMap<String, CassetteEntry>,
}

/// Where a cassette's answers came from, and which blocks they pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CassetteProvenance {
    /// Identity of the recording node (the `web3_clientVersion` string, e.g.
    /// `reth/v2.7.0-3d592ec/x86_64-unknown-linux-gnu`).
    pub source: String,
    /// RFC 3339 UTC wall-clock time the capture was taken
    /// (`YYYY-MM-DDTHH:MM:SSZ`).
    pub recorded_at: String,
    /// The pinned block span the capture covers.
    pub span: CassetteSpan,
}

/// Why a cassette's recorded header evidence is not usable as chain evidence.
///
/// Header evidence is the block identities a recorded run resolved, so it has to
/// be MORE than parseable bytes: a header outside the capture's own span, a
/// header that names no parent, or two headers that name different branches are
/// all ways a capture stops being evidence of the span it claims.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderEvidenceError {
    /// A ledger entry key is not the `[method, params]` pair the recorder
    /// writes, so the cassette cannot be read as evidence at all.
    #[error("cassette ledger key {0} is not a recorded request")]
    MalformedEntry(String),
    /// The recorded header read names no block height (a tag such as `latest`):
    /// a height-addressed capture cannot state which block it pinned.
    #[error("cassette records a header read for {0}, which names no block height")]
    HeaderWithoutHeight(String),
    /// The recorded answer is not a complete header: a failure answer, a null
    /// block, or a header naming no block hash or parent.
    #[error("the cassette's recorded header at block {number} is not a complete header")]
    IncompleteHeader {
        /// The height the recorded request named.
        number: u64,
    },
    /// The recorded header's own `number` disagrees with the request that
    /// recorded it.
    #[error("the cassette's header for block {number} states block {served}")]
    HeaderNumberMismatch {
        /// The height the recorded request named.
        number: u64,
        /// The height the recorded answer states.
        served: u64,
    },
    /// The recorded header names a block outside the capture's pinned span.
    #[error("the cassette records a header at block {number}, outside its span {from}-{to}")]
    HeaderOutsideSpan {
        /// The height the recorded answer names.
        number: u64,
        /// The capture's first height.
        from: u64,
        /// The capture's last height.
        to: u64,
    },
    /// Two recorded headers at consecutive heights name different branches.
    #[error(
        "the cassette's header at block {number} names parent {actual}, but the recorded block \
         before it hashes to {expected}"
    )]
    BrokenParentLink {
        /// The child's height.
        number: u64,
        /// The preceding recorded header's own hash.
        expected: B256,
        /// The parent hash the child names.
        actual: B256,
    },
    /// Two recorded header reads name ONE height with different blocks: a
    /// same-height substitution recorded into the ledger, where which block the
    /// capture names would depend on which request shape a run issued.
    #[error("the cassette records two different blocks at height {number}: {first} and {second}")]
    SameHeightSubstitution {
        /// The height both recorded blocks claim.
        number: u64,
        /// The first recorded block's hash.
        first: B256,
        /// The competing recorded block's hash.
        second: B256,
    },
    /// Two recorded header reads name ONE height with the same block: no
    /// substitution, but a capture that answers a height twice states no single
    /// record of it.
    #[error("the cassette records block {number} twice, so it states no single record of it")]
    DuplicateHeaderHeight {
        /// The height both recorded reads name.
        number: u64,
    },
}

/// A pinned block span (inclusive on both ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteSpan {
    /// First recorded block (decimal).
    pub from_block: u64,
    /// Last recorded block (decimal).
    pub to_block: u64,
}

/// One ledger entry: the recorded answer plus the recorder's write digest.
///
/// The digest ([`entry_digest`]) pins the entry's canonical response bytes.
/// Without it, a single-byte flip inside a *value* of an already-canonical
/// cassette would re-canonicalize to itself and the formatting gate alone
/// would stay inertly green; the recomputed digest is what makes any
/// response mutation red (the negative-probe discipline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CassetteEntry {
    /// The recorded answer.
    pub response: CassetteResponse,
    /// `0x`-prefixed keccak256 of the canonical compact JSON of
    /// [`CassetteEntry::response`] — written by the recorder, recomputed by
    /// the drift gate.
    pub digest: String,
}

/// The recorder's write digest for an entry: keccak256 over the canonical
/// compact JSON serialization of the response.
#[must_use]
pub fn entry_digest(response: &CassetteResponse) -> String {
    let canonical =
        serde_json::to_string(response).unwrap_or_else(|_| "unserializable".to_string());
    alloy::primitives::keccak256(canonical.as_bytes()).to_string()
}

/// A recorded RPC answer: the JSON-RPC `result` or the `error` payload.
///
/// Stored as canonical [`Value`]s (not raw wire strings) so the committed
/// cassette is fully canonical — a mutated response byte changes the
/// canonical bytes and fails [`verify_cassette_bytes`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CassetteResponse {
    /// A successful JSON-RPC answer (`result` — may be JSON `null`).
    Success {
        /// The canonicalized `result` value.
        result: Value,
    },
    /// A JSON-RPC error answer (revert, method-not-found, …).
    Failure {
        /// The canonicalized error object (`code`/`message`/optional `data`).
        error: Value,
    },
}

impl Cassette {
    /// Parse a cassette from a JSON byte slice.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the bytes are not a v1 cassette.
    pub fn from_json_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    /// Parse a cassette from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the string is not a v1 cassette.
    pub fn from_json_str(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// The canonical serialization — the exact bytes a recorder writes and
    /// the only byte-form the drift gate accepts for a committed cassette.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails (cannot happen
    /// for values parsed from JSON or recorded from JSON-RPC).
    pub fn canonical_bytes(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
    }

    /// The exact anchors this capture's recorded header reads state, ascending
    /// by height — the block identities a recorded run resolved (ADR-105 D1/D2).
    ///
    /// The ledger is the one source: every `eth_getBlockByNumber` entry is read
    /// back as a header answer, its height comes from the request that produced
    /// it, and consecutive records must name one branch. A capture that recorded
    /// no header read has no header evidence (an empty list) — that is a capture
    /// which resolved no block identity, not a broken one.
    ///
    /// # Errors
    ///
    /// [`HeaderEvidenceError`] for a recorded read that names no height, an
    /// answer that is not a complete header, a header outside the pinned span, a
    /// header whose own number disagrees with its request, two reads that name
    /// one height (whether they agree or substitute a block), or consecutive
    /// headers that name different branches.
    pub fn recorded_headers(&self) -> Result<Vec<ChainAnchor>, HeaderEvidenceError> {
        let (from, to) = (
            self.provenance.span.from_block,
            self.provenance.span.to_block,
        );
        let mut anchors: Vec<ChainAnchor> = Vec::new();
        for (key, entry) in &self.entries {
            let parsed: Vec<Value> = serde_json::from_str(key)
                .map_err(|_| HeaderEvidenceError::MalformedEntry(key.clone()))?;
            if parsed.first().and_then(Value::as_str) != Some("eth_getBlockByNumber") {
                continue;
            }
            let number = parsed
                .get(1)
                .and_then(|params| params.get(0))
                .and_then(Value::as_str)
                .and_then(|height| height.parse::<u64>().ok())
                .ok_or_else(|| HeaderEvidenceError::HeaderWithoutHeight(key.clone()))?;
            // ONE block below the span is admitted: it is the parent link of the
            // range's first block, which an intentional pre-block read (the
            // stkAAVE balance seed) resolves before any read of the range itself.
            // It states no completeness claim over that block's logs, so the
            // span's own meaning is unchanged.
            if !(from.saturating_sub(1)..=to).contains(&number) {
                return Err(HeaderEvidenceError::HeaderOutsideSpan { number, from, to });
            }
            let CassetteResponse::Success { result } = &entry.response else {
                return Err(HeaderEvidenceError::IncompleteHeader { number });
            };
            let field = |name: &str| {
                result
                    .get(name)
                    .and_then(Value::as_str)
                    .and_then(|raw| raw.parse::<B256>().ok())
            };
            let (Some(hash), Some(parent_hash)) = (field("hash"), field("parentHash")) else {
                return Err(HeaderEvidenceError::IncompleteHeader { number });
            };
            if let Some(served) = result
                .get("number")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse::<u64>().ok())
            {
                if served != number {
                    return Err(HeaderEvidenceError::HeaderNumberMismatch { number, served });
                }
            }
            let anchor = ChainAnchor::new(self.chain_id, number, hash, parent_hash)
                .map_err(|_| HeaderEvidenceError::IncompleteHeader { number })?;
            anchors.push(anchor);
        }
        anchors.sort_by_key(ChainAnchor::number);
        for pair in anchors.windows(2) {
            let [earlier, later] = pair else { continue };
            if later.number() == earlier.number() {
                if later.hash() == earlier.hash() {
                    return Err(HeaderEvidenceError::DuplicateHeaderHeight {
                        number: later.number(),
                    });
                }
                return Err(HeaderEvidenceError::SameHeightSubstitution {
                    number: later.number(),
                    first: earlier.hash(),
                    second: later.hash(),
                });
            }
            if later.number() == earlier.number() + 1 && later.parent_hash() != earlier.hash() {
                return Err(HeaderEvidenceError::BrokenParentLink {
                    number: later.number(),
                    expected: earlier.hash(),
                    actual: later.parent_hash(),
                });
            }
        }
        Ok(anchors)
    }
}

/// The drift gate for a committed cassette (GLOSSARY "drift gate" +
/// "negative probe"): parse, re-serialize through the recorder's own
/// canonical writer, require byte-identity, and require the recorded header
/// evidence to state a usable chain. Any hand edit or recorded byte mutation
/// diverges the canonical form and fails; a mutation that keeps the bytes
/// canonical (a re-digested parent hash, say) is what the header check
/// refuses.
///
/// # Errors
///
/// Returns the reason the cassette failed the gate.
pub fn verify_cassette_bytes(bytes: &[u8]) -> Result<(), String> {
    let cassette =
        Cassette::from_json_bytes(bytes).map_err(|e| format!("cassette parse failed: {e}"))?;
    if cassette.schema != CASSETTE_SCHEMA_V1 {
        return Err(format!(
            "cassette schema {:?} is not {}",
            cassette.schema, CASSETTE_SCHEMA_V1
        ));
    }
    let canonical = cassette
        .canonical_bytes()
        .map_err(|e| format!("cassette re-serialization failed: {e}"))?;
    if canonical != bytes {
        return Err(format!(
            "cassette is not canonical ({} bytes on file, {} canonical): regenerate with \
             the recorder (`record_updater_cassette`) — the drift gate requires byte-identity",
            bytes.len(),
            canonical.len()
        ));
    }
    for (key, entry) in &cassette.entries {
        let recomputed = entry_digest(&entry.response);
        if recomputed != entry.digest {
            return Err(format!(
                "cassette entry {key} fails its write digest (recorded {}, recomputed {recomputed}): \
                 the recorded response was mutated — regenerate with the recorder",
                entry.digest
            ));
        }
    }
    cassette
        .recorded_headers()
        .map_err(|error| format!("cassette header evidence fails: {error}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Artifact packaging
// ---------------------------------------------------------------------------

/// The suffix of a committed cassette package: `<name>.json` ships as
/// `<name>.json.zst`.
#[cfg(feature = "cassette-artifacts")]
pub const CASSETTE_ARTIFACT_SUFFIX: &str = ".zst";

/// The zstd level every cassette package is written with. Receipt and trace
/// payloads are highly redundant (the largest committed capture measures
/// 107 MB decoded -> 19 MB here); the level is a write-side choice and never a
/// gate input, because the gate compares decoded bytes.
#[cfg(feature = "cassette-artifacts")]
const CASSETTE_PACK_LEVEL: i32 = 19;

#[cfg(feature = "cassette-artifacts")]
impl Cassette {
    /// Parse a cassette from its artifact: `logical_path` itself, or its
    /// `.zst` package.
    ///
    /// # Errors
    ///
    /// When the artifact is missing, its package cannot be decoded, or the
    /// decoded bytes are not a v1 cassette.
    pub fn from_artifact<P: AsRef<Path>>(logical_path: P) -> std::io::Result<Self> {
        let bytes = read_artifact(logical_path)?;
        Self::from_json_bytes(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// The committed artifact path for `logical_path`: the `.json` name plus
/// [`CASSETTE_ARTIFACT_SUFFIX`]. The one place the suffix is spelled.
#[cfg(feature = "cassette-artifacts")]
#[must_use]
pub fn artifact_path<P: AsRef<Path>>(logical_path: P) -> PathBuf {
    packaged_path(logical_path.as_ref())
}

/// Write `canonical_bytes` as the package for `logical_path`, returning the
/// path the package was written to.
///
/// # Errors
///
/// When the package cannot be encoded or written.
#[cfg(feature = "cassette-artifacts")]
pub fn write_artifact<P: AsRef<Path>>(
    logical_path: P,
    canonical_bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let package = artifact_path(logical_path);
    std::fs::write(
        &package,
        zstd::encode_all(canonical_bytes, CASSETTE_PACK_LEVEL)?,
    )?;
    Ok(package)
}

/// The canonical bytes of the cassette artifact at `logical_path`.
///
/// A plain file wins over its package: an unpackaged recording is exactly what
/// a recorder just produced. An explicit `.zst` path decodes directly.
///
/// # Errors
///
/// When neither path is present, or the package cannot be decoded.
#[cfg(feature = "cassette-artifacts")]
pub fn read_artifact<P: AsRef<Path>>(logical_path: P) -> std::io::Result<Vec<u8>> {
    let logical = logical_path.as_ref();
    if is_package(logical) {
        return decode_package(logical);
    }
    match std::fs::read(logical) {
        Ok(bytes) => Ok(bytes),
        Err(plain_error) if plain_error.kind() == std::io::ErrorKind::NotFound => {
            let package = packaged_path(logical);
            if package.try_exists()? {
                decode_package(&package)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "cassette artifact missing: neither {} nor {} is present; regenerate it \
                         with its recorder",
                        logical.display(),
                        package.display()
                    ),
                ))
            }
        }
        Err(error) => Err(error),
    }
}

/// The drift gate over the committed artifact at `logical_path`: read it
/// (decoding the package), then run [`verify_cassette_bytes`].
///
/// # Errors
///
/// The reason the artifact is unreadable or failed the gate.
#[cfg(feature = "cassette-artifacts")]
pub fn verify_artifact<P: AsRef<Path>>(logical_path: P) -> Result<(), String> {
    let path = logical_path.as_ref();
    let bytes = read_artifact(path)
        .map_err(|e| format!("cassette artifact {} is unreadable: {e}", path.display()))?;
    verify_cassette_bytes(&bytes)
}

#[cfg(feature = "cassette-artifacts")]
fn is_package(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "zst")
}

#[cfg(feature = "cassette-artifacts")]
fn packaged_path(logical: &Path) -> PathBuf {
    if is_package(logical) {
        return logical.to_path_buf();
    }
    let mut package = logical.as_os_str().to_owned();
    package.push(CASSETTE_ARTIFACT_SUFFIX);
    PathBuf::from(package)
}

#[cfg(feature = "cassette-artifacts")]
fn decode_package(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;

    let mut decoded = Vec::new();
    zstd::Decoder::new(std::fs::File::open(path)?)?.read_to_end(&mut decoded)?;
    Ok(decoded)
}

/// Canonicalize a JSON value: sort object keys recursively and normalize hex
/// quantity strings to decimal. Addresses and data hex stay as recorded on
/// the wire (the wire form is already canonical for them).
#[must_use]
pub fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            // Rebuild with sorted keys (robust even under a
            // serde_json `preserve_order` build, where Map is an IndexMap).
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(keys.len());
            for key in keys {
                out.insert(key.clone(), canonicalize_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        Value::String(s) => match canonical_hex_quantity(s) {
            Some(decimal) => Value::String(decimal),
            None => Value::String(s.clone()),
        },
        other => other.clone(),
    }
}

/// Decimalize a minimal-form hex *quantity* string: `0x` + 1..=16 hex
/// digits with no leading zero digit (`0x0` is the canonical zero) — a
/// `u64`.
///
/// The precision rule behind this shape: decimalization must be exact,
/// because replay re-hexes the decimal and the wire form has to come back
/// byte-identical. A minimal form does (`0x6fdde03` → `117300739` →
/// `0x6fdde03`; `0xf30dba93` → `4077763219` → `0xf30dba93`). A
/// leading-zero digit marks a NON-minimal string — byte-hex, not a
/// quantity (`0x00`, `0x00000000`, `0x06fdde03`) — and decimalizing it is
/// lossy: `0x00000000` → `0` → replayed `0x0`, different bytes in an
/// odd-length form `Bytes` decoding rejects. Non-minimal forms therefore
/// stay verbatim. Long hex — addresses (40 digits), data words, topics
/// (≥17 digits) — and `0x` itself stay verbatim too: their wire form is
/// already canonical.
fn canonical_hex_quantity(s: &str) -> Option<String> {
    let hex = s.strip_prefix("0x")?;
    if hex.is_empty() || hex.len() > 16 {
        return None;
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // The precision rule: a leading zero digit is a non-minimal form —
    // byte-hex, not a quantity — and stays verbatim (decimalizing it would
    // not re-hex back to the same wire string). The one-digit `0x0` is the
    // canonical zero and decimalizes.
    if hex.len() > 1 && hex.starts_with('0') {
        return None;
    }
    let n = u64::from_str_radix(hex, 16).ok()?;
    Some(n.to_string())
}

/// The ledger key for a request: a compact JSON pair
/// `[method, canonical params]`. Deterministic by construction.
///
/// Public because a test that must serve exactly ONE recorded answer builds a
/// single-entry cassette, and the entry's key has to come from this same
/// canonicalization — a hand-rolled key silently becomes a fixture gap (and a
/// retried, unbounded request stream) the moment the canonical form moves.
#[must_use]
pub fn entry_key(method: &str, params: &Value) -> String {
    let canonical = canonicalize_value(params);
    // The getLogs filter's OR-set option lists (`address`, each topic
    // position) are serialized by alloy through a `HashSet` (`FilterSet`),
    // so their wire order is per-process random. The ledger key must not
    // carry that order — the drift-gate precondition is that the same
    // request canonicalizes to the same key on every run — so the recorder
    // sorts the option sets here (semantically exact: an OR-set is
    // order-free). The replay transport's semantic-key fallback keeps
    // cassettes recorded before this normalization replayable.
    let normalized = match method {
        "eth_getLogs" => normalize_log_filter_or_sets(canonical),
        _ => canonical,
    };
    let key = Value::Array(vec![Value::String(method.to_string()), normalized]);
    serde_json::to_string(&key).unwrap_or_else(|_| format!("{method:?}"))
}

/// Sort the OR-set option lists of an `eth_getLogs` filter in place. No-op
/// for non-arrays (a bare-string topic position is a single option, already
/// order-free).
pub(crate) fn normalize_log_filter_or_sets(mut params: Value) -> Value {
    // The filter is the first (only) element of the params array.
    if let Some(Value::Object(filter)) = params.get_mut(0) {
        if let Some(address) = filter.get_mut("address") {
            sort_json_array(address);
        }
        if let Some(topics) = filter.get_mut("topics").and_then(Value::as_array_mut) {
            for position in topics {
                sort_json_array(position);
            }
        }
    }
    params
}

/// Sort a JSON array value in place by its string form — the OR-set-order
/// normalization shared by the ledger key and the replay semantic key.
pub(crate) fn sort_json_array(value: &mut Value) {
    if let Some(items) = value.as_array_mut() {
        items.sort_by_key(std::string::ToString::to_string);
    }
}

/// Format unix seconds as an RFC 3339 UTC timestamp
/// (`YYYY-MM-DDTHH:MM:SSZ`). Dependency-free (no `time`/`chrono` in this
/// crate); verified against fixed literals in the tests.
#[must_use]
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let secs_of_day = unix_secs % 86_400;
    // `days` is a quotient of non-negative integers, so the `i64` widening
    // `civil_from_days` expects cannot wrap.
    let (year, month, day) = civil_from_days(days.cast_signed());
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    // Hinnant-algorithm invariants: the day-of-month below lands in [1, 31]
    // and the month in [1, 12] (pinned by the `rfc3339_utc_known_instants`
    // test), so the u32 widenings never fail; the `unwrap_or` is unreachable.
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(0);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(0);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The recording transport: an alloy `Transport` decorator (sibling of the
/// offline transport) that forwards every JSON-RPC packet to a live inner
/// transport and records each `(method, canonical params) → response` pair
/// into the in-memory ledger.
///
/// Wrap as a real [`AlloyProvider`] via [`RecordingTransport::as_alloy_provider`]
/// and run the updaters unchanged; flush the capture with
/// [`RecordingTransport::cassette`] → [`Cassette::canonical_bytes`].
#[derive(Clone, Debug)]
pub struct RecordingTransport {
    inner: BoxTransport,
    entries: Arc<Mutex<BTreeMap<String, CassetteEntry>>>,
}

impl RecordingTransport {
    /// Decorate a live inner transport.
    #[must_use]
    pub fn new(inner: BoxTransport) -> Self {
        Self {
            inner,
            entries: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Decorate a live endpoint (HTTP, WS, or IPC — the same transports the
    /// live provider accepts), resolved through alloy's `TransportConnect`.
    ///
    /// Note: the wrapped WS/IPC transport serves *requests* only — the
    /// recorder never subscribes, so the pubsub frontend is left unwired.
    ///
    /// # Errors
    ///
    /// Returns a message when the URL scheme is unrecognized or the
    /// WS/IPC handshake fails.
    pub async fn connect(rpc_url: &str) -> Result<Self, String> {
        let transport = if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
            let url = rpc_url
                .parse()
                .map_err(|e| format!("invalid RPC URL {rpc_url:?}: {e}"))?;
            alloy::transports::http::Http::new(url).boxed()
        } else if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
            alloy::transports::ws::WsConnect::new(rpc_url)
                .into_service()
                .await
                .map_err(|e| format!("ws connect {rpc_url:?}: {e}"))?
                .boxed()
        } else if rpc_url.starts_with("ipc://")
            || rpc_url.starts_with('/')
            || rpc_url.starts_with("\\\\")
        {
            let path = rpc_url.strip_prefix("ipc://").unwrap_or(rpc_url);
            alloy::transports::ipc::IpcConnect::new(path.to_string())
                .into_service()
                .await
                .map_err(|e| format!("ipc connect {rpc_url:?}: {e}"))?
                .boxed()
        } else {
            let scheme = rpc_url.split("://").next().unwrap_or(rpc_url);
            return Err(format!(
                "unsupported transport scheme {scheme:?} for recording: {rpc_url:?}"
            ));
        };
        Ok(Self::new(transport))
    }

    /// Wrap this recording transport as a live [`AlloyProvider`] — the same
    /// wiring [`crate::offline::OfflineProvider::as_alloy_provider`] uses, so
    /// updaters run unchanged over the recording seam. The clone shares the
    /// in-memory ledger, so the caller keeps reading entries while the
    /// provider runs.
    #[must_use]
    pub fn as_alloy_provider(&self) -> AlloyProvider {
        let client = RpcClient::new(self.clone(), false);
        let root: RootProvider = RootProvider::new(client);
        let arc: Arc<dyn Provider<_>> = Arc::new(root);
        AlloyProvider::from_provider(arc)
    }

    /// Snapshot the ledger into a cassette with the caller-supplied
    /// provenance (the span the driver pinned, the recording node identity,
    /// and the capture wall-clock time).
    #[must_use]
    pub fn cassette(&self, chain_id: u64, provenance: CassetteProvenance) -> Cassette {
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id,
            provenance,
            entries: self.entries.lock().clone(),
        }
    }

    /// Number of distinct recorded entries so far.
    #[must_use]
    pub fn recorded_len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Record one aligned request/response pair (first write wins — a
    /// repeated request against a pinned span is the same answer).
    fn record_pair(&self, req: &SerializedRequest, resp: &Response) {
        let params = req
            .params()
            .and_then(|rv| serde_json::from_str::<Value>(rv.get()).ok())
            .unwrap_or(Value::Null);
        let key = entry_key(req.method(), &params);
        let response = match &resp.payload {
            ResponsePayload::Success(raw) => CassetteResponse::Success {
                result: serde_json::from_str::<Value>(raw.get())
                    .map_or_else(|_| Value::Null, |v| canonicalize_value(&v)),
            },
            ResponsePayload::Failure(err) => CassetteResponse::Failure {
                error: serde_json::to_value(err)
                    .map_or_else(|_| Value::Null, |v| canonicalize_value(&v)),
            },
        };
        let entry = CassetteEntry {
            digest: entry_digest(&response),
            response,
        };
        self.entries.lock().entry(key).or_insert(entry);
    }

    fn record_packet(&self, req: &RequestPacket, resp: &ResponsePacket) {
        match (req, resp) {
            (RequestPacket::Single(req), ResponsePacket::Single(resp)) => {
                self.record_pair(req, resp);
            }
            (RequestPacket::Batch(reqs), ResponsePacket::Batch(resps)) => {
                for (req, resp) in reqs.iter().zip(resps.iter()) {
                    self.record_pair(req, resp);
                }
            }
            // Mismatched shapes cannot come from a conforming transport;
            // nothing meaningful is recordable.
            _ => {}
        }
    }
}

impl RecordingTransport {
    /// Forward the packet to the inner transport and record the answers.
    /// Shared by the `Service` impls for `RecordingTransport` and
    /// `Arc<RecordingTransport>` (the Arc form is what `RpcClient` boxes).
    fn forward(&self, req: RequestPacket) -> TransportFut<'static> {
        let mut inner = self.inner.clone();
        let this = self.clone();
        Box::pin(async move {
            let resp = tower::Service::call(&mut inner, req.clone()).await?;
            this.record_packet(&req, &resp);
            Ok(resp)
        })
    }
}

impl tower::Service<RequestPacket> for RecordingTransport {
    type Response = ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        self.forward(req)
    }
}

#[cfg(test)]
// Test assertions fail by design through panic!/expect, so the panic-family
// deny is expected away for this module only — the same scoping as the
// unwrap/expect allowances beside it.
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cassette_replay::{wire_hex_quantity, wire_value};
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::mock::Asserter;
    use std::borrow::Cow;

    // ------------------------------------------------------------------
    // Canonical params — expected values are independent hand-written
    // literals, not recomputed through the code under test.
    // ------------------------------------------------------------------

    #[test]
    fn canonical_params_sort_keys_and_decimalize_block_tags() {
        // A wire-form eth_getLogs filter (alloy emits hex quantities): keys
        // out of order, hex block tags. The canonical form must sort the
        // keys and decimalize the quantities while leaving the 40-digit
        // address and the 64-digit topics verbatim.
        let wire: Value = serde_json::from_str(
            r#"{
                "toBlock": "0x20",
                "address": ["0x1f98431c8ad98523631ae4a59f267346ea31f984"],
                "fromBlock": "0x10",
                "topics": [[
                    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                ]]
            }"#,
        )
        .unwrap();
        let canonical = canonicalize_value(&wire);
        let expected: Value = serde_json::from_str(
            r#"{
                "address": ["0x1f98431c8ad98523631ae4a59f267346ea31f984"],
                "fromBlock": "16",
                "toBlock": "32",
                "topics": [[
                    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                ]]
            }"#,
        )
        .unwrap();
        assert_eq!(canonical, expected);
    }

    #[test]
    fn entry_key_is_order_insensitive_for_getlogs_or_sets() {
        // alloy serializes filter option sets through a HashSet — the same
        // request arrives with per-process-random OR-set order. The ledger
        // key must collide for both orders (the drift-gate precondition).
        let a: Value = serde_json::from_str(
            r#"[{"topics":[["0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde","0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"]],"fromBlock":"0x10","toBlock":"0x20"}]"#,
        )
        .unwrap();
        let b: Value = serde_json::from_str(
            r#"[{"topics":[["0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c","0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"]],"toBlock":"0x20","fromBlock":"0x10"}]"#,
        )
        .unwrap();
        assert_eq!(entry_key("eth_getLogs", &a), entry_key("eth_getLogs", &b));
        // Non-getLogs methods keep the plain canonical form (no OR-set rule).
        assert_eq!(entry_key("eth_call", &a), entry_key("eth_call", &a));
    }

    #[test]
    fn canonical_key_pairs_method_and_params() {
        let params: Value = serde_json::from_str(r#"{"toBlock":"0x10"}"#).unwrap();
        let key = entry_key("eth_getLogs", &params);
        // Independent literal: method first, canonical (sorted + decimalized)
        // params second, compact.
        assert_eq!(key, r#"["eth_getLogs",{"toBlock":"16"}]"#);
    }

    #[test]
    fn canonical_hex_quantity_leaves_addresses_and_data_alone() {
        // 40-digit address: verbatim.
        assert_eq!(
            canonical_hex_quantity("0x1f98431c8ad98523631ae4a59f267346ea31f984"),
            None
        );
        // 64-digit topic: verbatim.
        assert_eq!(
            canonical_hex_quantity(
                "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
            ),
            None
        );
        // Bare 0x: verbatim (call data can be empty).
        assert_eq!(canonical_hex_quantity("0x"), None);
        // 16-digit quantity (u64 max): decimalized.
        assert_eq!(
            canonical_hex_quantity("0xffffffffffffffff"),
            Some("18446744073709551615".to_string())
        );
        // The precision rule: minimal forms decimalize exactly — odd- and
        // even-length alike (independent decimal literals).
        assert_eq!(
            canonical_hex_quantity("0x6fdde03"),
            Some("117300739".to_string())
        );
        assert_eq!(
            canonical_hex_quantity("0xf30dba93"),
            Some("4077763219".to_string())
        );
        // `0x0` is the canonical zero.
        assert_eq!(canonical_hex_quantity("0x0"), Some("0".to_string()));
        // Leading-zero forms are NON-minimal — byte-hex, not quantities:
        // verbatim, not decimalized (decimalizing 0x0010 to 16 would
        // replay as 0x10, a different wire string; 0x00000000 would replay
        // as the odd-length 0x0, which Bytes decoding rejects).
        assert_eq!(canonical_hex_quantity("0x0010"), None);
        assert_eq!(canonical_hex_quantity("0x00000000"), None);
        assert_eq!(canonical_hex_quantity("0x06fdde03"), None);
        // Not hex.
        assert_eq!(canonical_hex_quantity("latest"), None);
    }

    #[test]
    fn rfc3339_utc_known_instants() {
        // Independent literals (well-known epoch facts).
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(rfc3339_utc(2_147_483_647), "2038-01-19T03:14:07Z");
    }

    // ------------------------------------------------------------------
    // RecordingTransport at the public seam: drive a real AlloyProvider
    // over a mock inner transport, inspect the flushed cassette.
    // ------------------------------------------------------------------

    fn provenance() -> CassetteProvenance {
        CassetteProvenance {
            source: "mock-node/1.0".to_string(),
            recorded_at: rfc3339_utc(1_770_000_000),
            span: CassetteSpan {
                from_block: 26_102_618,
                to_block: 26_102_619,
            },
        }
    }

    #[tokio::test]
    async fn recording_transport_records_forwarded_answers() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1234"); // eth_blockNumber → 4660
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();

        let n = provider.get_block_number().await.unwrap();
        assert_eq!(n, 4_660); // independent literal: 0x1234

        let cassette = recorder.cassette(1, provenance());
        assert_eq!(cassette.schema, CASSETTE_SCHEMA_V1);
        assert_eq!(cassette.entries.len(), 1);
        let (key, entry) = cassette.entries.iter().next().unwrap();
        assert!(key.starts_with("[\"eth_blockNumber\","), "key was {key}");
        // The recorded quantity result is canonicalized to decimal
        // (0x1234 = 4660, an independent literal).
        assert_eq!(
            entry.response,
            CassetteResponse::Success {
                result: Value::String("4660".to_string())
            }
        );
        // The write digest pins the canonical response bytes.
        assert_eq!(entry.digest, entry_digest(&entry.response));
    }

    #[tokio::test]
    async fn recording_transport_records_error_payloads() {
        let asserter = Asserter::new();
        // A non-revert RPC failure (a revert-classified message would
        // surface as ExecutionReverted at the provider layer).
        asserter.push_failure(ErrorPayload {
            code: -32005,
            message: Cow::Borrowed("node overloaded"),
            data: None,
        });
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();
        let err = provider.get_block_number().await.unwrap_err();
        assert!(matches!(
            err,
            degenbot_core::errors::ProviderError::RpcError { .. }
        ));

        let cassette = recorder.cassette(1, provenance());
        let (_, entry) = cassette.entries.iter().next().unwrap();
        let CassetteResponse::Failure { error } = &entry.response else {
            panic!("expected a Failure entry, got {:?}", entry.response)
        };
        assert_eq!(error["code"], -32005);
        assert_eq!(error["message"], "node overloaded");
    }

    #[test]
    fn cassette_round_trips_byte_identical_through_canonical_writer() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1");
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let cassette = recorder.cassette(1, provenance());
        let bytes = cassette.canonical_bytes().unwrap();

        // The gate: parse the written bytes, re-serialize with the same
        // writer, require byte-identity.
        verify_cassette_bytes(&bytes).unwrap();
    }

    /// Negative probe (unit level): flip one recorded response byte and the
    /// drift gate must go red. The demonstrated red-path transcript (the
    /// same check run through the driver's --check mode) is in the task
    /// result.
    #[tokio::test]
    async fn mutated_response_byte_fails_the_drift_gate() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1234");
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();
        provider.get_block_number().await.unwrap();
        let cassette = recorder.cassette(1, provenance());
        let bytes = cassette.canonical_bytes().unwrap();
        verify_cassette_bytes(&bytes).expect("unmutated cassette must pass");

        // Flip one byte inside the recorded response value: the recorded
        // eth_blockNumber answer "4660" becomes "4661" — a different
        // recorded answer.
        let mut mutated = bytes.clone();
        let marker = b"\"4660\"";
        let pos = mutated
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("recorded response literal present in canonical bytes");
        mutated[pos + 3] = b'1'; // "4660" → "4661"
        assert_ne!(mutated, bytes);

        let verdict = verify_cassette_bytes(&mutated);
        assert!(
            verdict.is_err(),
            "a mutated recorded response MUST fail the drift gate, got {verdict:?}"
        );
    }

    // ------------------------------------------------------------------
    // Header evidence: the recorder carries the block identities a run
    // resolved, and the drift gate proves they are chain evidence rather
    // than bytes that happen to parse.
    // ------------------------------------------------------------------

    /// The node a header recording talks to: one header answer per height,
    /// the endpoint's chain id, and an empty logs answer. Records the method
    /// of every request so a test can state the recorded request shape.
    #[derive(Clone)]
    struct HeaderNode {
        chain: u64,
        headers: BTreeMap<u64, Value>,
        methods: Arc<Mutex<Vec<String>>>,
    }

    impl HeaderNode {
        fn new() -> Self {
            Self {
                chain: 1,
                headers: BTreeMap::new(),
                methods: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn header(self, number: u64, hash_tag: u8, parent_tag: u8) -> Self {
            let header = serde_json::json!({
                "number": format!("0x{number:x}"),
                "hash": format!("{}", alloy::primitives::B256::new([hash_tag; 32])),
                "parentHash": format!("{}", alloy::primitives::B256::new([parent_tag; 32])),
            });
            self.header_value(number, header)
        }

        fn header_value(mut self, number: u64, header: Value) -> Self {
            self.headers.insert(number, header);
            self
        }

        fn handle(&self, packet: RequestPacket) -> ResponsePacket {
            match packet {
                RequestPacket::Single(req) => ResponsePacket::Single(self.respond(&req)),
                RequestPacket::Batch(reqs) => {
                    ResponsePacket::Batch(reqs.iter().map(|req| self.respond(req)).collect())
                }
            }
        }

        fn respond(&self, req: &SerializedRequest) -> Response {
            self.methods.lock().push(req.method().to_string());
            let params: Value = req
                .params()
                .and_then(|raw| serde_json::from_str(raw.get()).ok())
                .unwrap_or(Value::Null);
            let payload = match req.method() {
                "eth_chainId" => ok_answer(&Value::String(format!("0x{:x}", self.chain))),
                "eth_getBlockByNumber" => {
                    let number = params
                        .get(0)
                        .and_then(Value::as_str)
                        .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok());
                    match number.and_then(|number| self.headers.get(&number)) {
                        Some(header) => ok_answer(header),
                        None => ok_answer(&Value::Null),
                    }
                }
                "eth_getLogs" => ok_answer(&Value::Array(Vec::new())),
                other => ResponsePayload::Failure(ErrorPayload {
                    code: -32_601,
                    message: Cow::Owned(format!("header node does not serve {other}")),
                    data: None,
                }),
            };
            Response {
                id: req.id().clone(),
                payload,
            }
        }
    }

    impl tower::Service<RequestPacket> for HeaderNode {
        type Response = ResponsePacket;
        type Error = alloy::transports::TransportError;
        type Future = alloy::transports::TransportFut<'static>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: RequestPacket) -> Self::Future {
            let node = self.clone();
            Box::pin(std::future::ready(Ok(node.handle(req))))
        }
    }

    fn ok_answer(value: &Value) -> ResponsePayload {
        ResponsePayload::Success(
            serde_json::value::to_raw_value(value).expect("a fixture answer serializes"),
        )
    }

    fn provenance_over(from_block: u64, to_block: u64) -> CassetteProvenance {
        CassetteProvenance {
            source: "header-node/1.0".to_string(),
            recorded_at: rfc3339_utc(1_770_000_000),
            span: CassetteSpan {
                from_block,
                to_block,
            },
        }
    }

    /// Record one header read per height of `heights` through the recorder, the
    /// way a run records the block identities it resolved, and flush the
    /// cassette under the (possibly different) declared `span`.
    async fn header_capture(
        node: HeaderNode,
        heights: std::ops::RangeInclusive<u64>,
        span: CassetteSpan,
    ) -> (Cassette, Vec<u8>) {
        let recorder = RecordingTransport::new(node.boxed());
        let provider = recorder.as_alloy_provider();
        for number in heights {
            provider
                .get_block_lineage_at(number)
                .await
                .expect("the stub answers the header read");
        }
        let cassette = recorder.cassette(1, provenance_over(span.from_block, span.to_block));
        let bytes = cassette
            .canonical_bytes()
            .expect("a recorded cassette serializes");
        (cassette, bytes)
    }

    fn three_header_node() -> HeaderNode {
        HeaderNode::new()
            .header(100, 0xa1, 0xa0)
            .header(101, 0xa2, 0xa1)
            .header(102, 0xa3, 0xa2)
    }

    #[tokio::test]
    async fn a_recorded_header_chain_is_complete_evidence_and_regenerates_identically() {
        let (cassette, bytes) = header_capture(
            three_header_node(),
            100..=102,
            CassetteSpan {
                from_block: 100,
                to_block: 102,
            },
        )
        .await;

        verify_cassette_bytes(&bytes)
            .expect("a complete recorded header chain is valid chain evidence");
        assert_eq!(
            Cassette::from_json_bytes(&bytes).expect("the canonical bytes parse"),
            cassette,
            "regeneration through the canonical writer is byte-identical"
        );

        let recorded = cassette
            .recorded_headers()
            .expect("the recorded headers state a linked chain");
        assert_eq!(
            recorded
                .iter()
                .map(|anchor| (anchor.number(), anchor.hash()))
                .collect::<Vec<_>>(),
            vec![
                (100, alloy::primitives::B256::new([0xa1; 32])),
                (101, alloy::primitives::B256::new([0xa2; 32])),
                (102, alloy::primitives::B256::new([0xa3; 32])),
            ],
            "the evidence is the chain the run resolved, height by height"
        );
    }

    #[tokio::test]
    async fn a_recorded_header_outside_the_pinned_span_fails_the_drift_gate() {
        // The capture pinned 100..=101 but the recorder also carries block
        // 102's header: evidence the provenance span does not claim.
        let (_, bytes) = header_capture(
            three_header_node(),
            100..=102,
            CassetteSpan {
                from_block: 100,
                to_block: 101,
            },
        )
        .await;

        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            matches!(
                verdict,
                Err(ref reason) if reason.contains("outside its span 100-101")
            ),
            "a header the capture's span does not cover is not its evidence: {verdict:?}"
        );
    }

    /// The parent of the span's FIRST block is the one height below it that a
    /// capture legitimately carries: an intentional pre-block read (the stkAAVE
    /// seed) resolves that header before any read of the range itself.
    #[tokio::test]
    async fn the_parent_header_of_the_first_block_is_span_evidence() {
        let (_, bytes) = header_capture(
            three_header_node(),
            100..=101,
            CassetteSpan {
                from_block: 101,
                to_block: 101,
            },
        )
        .await;

        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            verdict.is_ok(),
            "the first block's parent link is evidence the span implies: {verdict:?}"
        );
    }

    /// Two blocks below the span is still outside it.
    #[tokio::test]
    async fn a_header_two_blocks_below_the_span_fails_the_drift_gate() {
        let (_, bytes) = header_capture(
            three_header_node(),
            100..=102,
            CassetteSpan {
                from_block: 102,
                to_block: 102,
            },
        )
        .await;

        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            matches!(
                verdict,
                Err(ref reason) if reason.contains("outside its span 102-102")
            ),
            "a header beyond the first block's parent is not span evidence: {verdict:?}"
        );
    }

    #[tokio::test]
    async fn a_self_consistent_header_mutation_fails_the_drift_gate() {
        // The negative probe for chain evidence: mutate a recorded header's
        // parent hash AND recompute that entry's write digest, so the byte and
        // digest gates are satisfied and only the parent link can catch it.
        let (cassette, _) = header_capture(
            three_header_node(),
            100..=102,
            CassetteSpan {
                from_block: 100,
                to_block: 102,
            },
        )
        .await;
        let mut mutated = cassette.clone();
        let key = mutated
            .entries
            .keys()
            .find(|key| key.contains("eth_getBlockByNumber") && key.contains("101"))
            .expect("the capture records block 101's header")
            .clone();
        let entry = mutated.entries.get_mut(&key).expect("the entry exists");
        let CassetteResponse::Success { result } = &mut entry.response else {
            panic!("a recorded header is a success answer");
        };
        result["parentHash"] =
            Value::String(format!("{}", alloy::primitives::B256::new([0xb0; 32])));
        entry.digest = entry_digest(&entry.response);

        let bytes = mutated
            .canonical_bytes()
            .expect("the mutated cassette serializes");
        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            matches!(
                verdict,
                Err(ref reason) if reason.contains("names parent")
            ),
            "a self-consistent header mutation must fail the chain gate: {verdict:?}"
        );
    }

    #[tokio::test]
    async fn a_recorded_header_without_its_parent_fails_the_drift_gate() {
        let node = HeaderNode::new()
            .header(100, 0xa1, 0xa0)
            .header_value(
                101,
                serde_json::json!({ "number": "0x65", "hash": format!("{}", alloy::primitives::B256::new([0xa2; 32])) }),
            );
        let (_, bytes) = header_capture(
            node,
            100..=101,
            CassetteSpan {
                from_block: 100,
                to_block: 101,
            },
        )
        .await;

        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            matches!(
                verdict,
                Err(ref reason) if reason.contains("is not a complete header")
            ),
            "a header that names no parent is not chain evidence: {verdict:?}"
        );
    }

    /// One recorded header answer under an explicit request-param shape, so a
    /// test can state two canonical keys for one height.
    fn header_entry_with_params(
        params: &Value,
        hash_tag: u8,
        parent_tag: u8,
    ) -> (String, CassetteEntry) {
        let response = CassetteResponse::Success {
            result: serde_json::json!({
                "number": "100",
                "hash": format!("{}", alloy::primitives::B256::new([hash_tag; 32])),
                "parentHash": format!("{}", alloy::primitives::B256::new([parent_tag; 32])),
            }),
        };
        (
            entry_key("eth_getBlockByNumber", params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        )
    }

    /// A capture whose ledger carries two header reads for ONE height under two
    /// distinct canonical request keys (the `false`/`true` full-transactions
    /// shapes).
    fn two_headers_at_one_height(first: (u8, u8), second: (u8, u8)) -> Cassette {
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: provenance_over(100, 100),
            entries: [
                header_entry_with_params(&serde_json::json!(["0x64", false]), first.0, first.1),
                header_entry_with_params(&serde_json::json!(["0x64", true]), second.0, second.1),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn a_same_height_substitution_in_the_ledger_fails_the_drift_gate() {
        // Two canonical keys, one height, two DIFFERENT blocks: whichever
        // request shape a replayed run issued would decide which block the
        // capture named, so the capture names no single block at that height.
        let cassette = two_headers_at_one_height((0xa1, 0xa0), (0xb1, 0xa0));

        let refusal = cassette
            .recorded_headers()
            .expect_err("two blocks at one height are not one chain");
        assert!(
            matches!(
                refusal,
                HeaderEvidenceError::SameHeightSubstitution { number: 100, .. }
            ),
            "got {refusal:?}"
        );

        let bytes = cassette.canonical_bytes().expect("the cassette serializes");
        let verdict = verify_cassette_bytes(&bytes);
        assert!(
            matches!(
                verdict,
                Err(ref reason) if reason.contains("two different blocks at height 100")
            ),
            "the drift gate must refuse a substituted height: {verdict:?}"
        );
    }

    #[test]
    fn a_duplicate_header_height_is_refused_as_ambiguous_evidence() {
        // The same block recorded under two request shapes: no substitution,
        // but a capture that answers one height twice states no single record.
        let cassette = two_headers_at_one_height((0xa1, 0xa0), (0xa1, 0xa0));

        let refusal = cassette
            .recorded_headers()
            .expect_err("one height recorded twice is not a single record");
        assert!(
            matches!(
                refusal,
                HeaderEvidenceError::DuplicateHeaderHeight { number: 100 }
            ),
            "got {refusal:?}"
        );
    }

    #[tokio::test]
    async fn a_capture_without_recorded_headers_states_no_header_evidence() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1234");
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        recorder
            .as_alloy_provider()
            .get_block_number()
            .await
            .unwrap();
        let cassette = recorder.cassette(1, provenance());

        assert!(
            cassette
                .recorded_headers()
                .expect("no header evidence is not broken evidence")
                .is_empty(),
            "a capture that resolved no header claims no header evidence"
        );
        verify_cassette_bytes(&cassette.canonical_bytes().unwrap())
            .expect("the pool-updater captures stay valid captures");
    }

    // ------------------------------------------------------------------
    // Committed corpus: the drift gate runs over the real fixtures.
    // ------------------------------------------------------------------

    /// The seed-key retirement guard (the wave-3 seed-key adjudication): the
    /// two legacy aave seed cassettes' `eth_getLogs` ledger keys were
    /// regenerated through the deterministic writer (`entry_key`), so the
    /// replay transport's OR-set semantic-key fallback is no longer
    /// load-bearing for them - the exact canonical key hits. The guard
    /// re-derives every ledger key through `entry_key` and requires
    /// identity: a future edit that re-introduces an un-normalized key
    /// (or a filter whose OR-set order drifts) goes red here.
    ///
    /// live-invariant gate (ADR-087 D2): watches the committed seed cassettes against the live `entry_key` writer; not tripwire precedent.
    #[test]
    fn regenerated_seed_cassettes_ledger_keys_are_deterministically_normalized() {
        for file in [
            "aave_update_chunk_26130440-26130445.json",
            "aave_config_chunk_19091050-19091059.json",
        ] {
            let bytes = read_artifact(corpus_dir().join(file)).unwrap();
            verify_cassette_bytes(&bytes).unwrap_or_else(|e| panic!("{file}: drift gate RED: {e}"));
            let cassette = Cassette::from_json_bytes(&bytes).unwrap();
            for key in cassette.entries.keys() {
                let parsed: Vec<Value> = serde_json::from_str(key).unwrap();
                let method = parsed[0].as_str().unwrap();
                let derived = entry_key(method, &parsed[1]);
                assert_eq!(
                    &derived, key,
                    "{file}: ledger key is not the deterministic writer's \
                     canonical form (the semantic fallback would be \
                     load-bearing for it)"
                );
            }
        }
    }

    fn corpus_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/fixtures/cassettes")
    }

    /// Drift gate over the committed seed corpus: every `.json.zst` package
    /// must decode to bytes byte-identical to the canonical regeneration
    /// through the recorder's writer, and the corpus must carry at least the
    /// two pinned spans (one pool-updater chunk, one Aave market chunk).
    #[test]
    fn committed_cassette_corpus_is_byte_identical() {
        // The corpus home carries the seed recordings at the top level and
        // per-generator scenario subdirectories beneath it (the wave-2
        // EVM-oracle captures live in `wave2/`) — the guard walks the whole
        // home so a subdirectory can never hide a drifted cassette.
        fn collect(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("cassette corpus missing at {}: {e}", dir.display()))
            {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "zst") {
                    files.push(path);
                }
            }
        }
        let dir = corpus_dir();
        let mut files = Vec::new();
        collect(&dir, &mut files);
        files.sort();
        assert!(
            files.len() >= 2,
            "seed corpus must carry at least the pool + Aave cassettes, found {}",
            files.len()
        );
        for path in &files {
            verify_artifact(path)
                .unwrap_or_else(|e| panic!("drift gate RED for {}: {e}", path.display()));
        }
    }

    // ------------------------------------------------------------------
    // Short-hex precision rule: the corpus guard.
    //
    // The recorder decimalizes minimal-form quantities at record time and
    // preserves every other 0x form verbatim, so a committed cassette can
    // only carry hex the rule leaves untouched — and replay must restore
    // every token byte-exactly. These tests pin both halves.
    // ------------------------------------------------------------------

    /// The precision-rule class of one `0x`-hex token. Total and explicit:
    /// every token lands in exactly one named class, and
    /// [`assert_token_matches_rule`] asserts each class's canonicalization
    /// outcome — there is no catch-all arm.
    #[derive(Debug, PartialEq, Eq)]
    enum HexTokenClass {
        /// Minimal form (no leading-zero digit, ≤16 digits): decimalized
        /// at record time; replay re-hexes to the same wire string.
        MinimalQuantity,
        /// ≥17 digits: verbatim on both sides (never parsed as a quantity).
        LongHex,
        /// Leading-zero digit, ≤16 digits: byte-hex, not a quantity —
        /// preserved verbatim by the precision rule.
        NonMinimalHex,
        /// `0x` with no digits or a non-hex tail: not a quantity — verbatim.
        NotAQuantity,
    }

    /// Classify one `0x`-hex token exactly as [`canonical_hex_quantity`]
    /// dispatches it (the two must never diverge — the guard asserts it).
    fn hex_token_class(token: &str) -> HexTokenClass {
        let hex = token
            .strip_prefix("0x")
            .expect("the scanner yields 0x-prefixed tokens");
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return HexTokenClass::NotAQuantity;
        }
        if hex.len() > 16 {
            return HexTokenClass::LongHex;
        }
        if hex.len() > 1 && hex.starts_with('0') {
            return HexTokenClass::NonMinimalHex;
        }
        HexTokenClass::MinimalQuantity
    }

    /// The guard's per-token dispatch: each class's canonicalization
    /// outcome, asserted against the real rule function.
    fn assert_token_matches_rule(token: &str) {
        let decimalized = canonical_hex_quantity(token);
        match hex_token_class(token) {
            HexTokenClass::MinimalQuantity => {
                assert!(decimalized.is_some(), "{token:?} must decimalize");
            }
            HexTokenClass::LongHex | HexTokenClass::NonMinimalHex | HexTokenClass::NotAQuantity => {
                assert!(
                    decimalized.is_none(),
                    "{token:?} must stay verbatim (untouched-by-construction)"
                );
            }
        }
    }

    /// Scan one raw string (a serialized ledger key or a string value) for
    /// `0x` + hex-digit runs. Ledger keys embed hex inside JSON string
    /// literals; the quote characters are not hex digits and end a run.
    fn push_hex_tokens(s: &str, tokens: &mut Vec<String>) {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'0' && bytes[i + 1] == b'x' {
                let mut end = i + 2;
                while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                    end += 1;
                }
                tokens.push(s[i..end].to_string());
                i = end;
            } else {
                i += 1;
            }
        }
    }

    /// Walk a parsed cassette JSON and collect every `0x`-hex token from
    /// object keys (the serialized ledger keys) and string values.
    fn collect_hex_tokens(value: &Value, tokens: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    push_hex_tokens(key, tokens);
                    collect_hex_tokens(item, tokens);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_hex_tokens(item, tokens);
                }
            }
            Value::String(s) => push_hex_tokens(s, tokens),
            _ => {}
        }
    }

    /// Corpus guard (green path): every `0x`-hex token in the committed
    /// corpus — ledger keys AND response values — must be
    /// untouched-by-construction (≥17 digits or non-minimal, now preserved
    /// verbatim), and every entry must be a byte-exact fixed point of
    /// canonicalization ∘ wire restoration. Together with the drift gate
    /// this proves the corpus replays byte-identically under the rule.
    #[test]
    fn committed_corpus_hex_tokens_round_trip_byte_exactly() {
        let dir = corpus_dir();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cassette corpus missing at {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "zst"))
            .collect();
        files.sort();

        let mut total_tokens = 0usize;
        let mut total_entries = 0usize;
        for path in &files {
            let bytes = read_artifact(path).unwrap();
            verify_cassette_bytes(&bytes)
                .unwrap_or_else(|e| panic!("drift gate RED for {}: {e}", path.display()));
            let cassette = Cassette::from_json_bytes(&bytes).unwrap();
            let parsed = serde_json::to_value(&cassette).unwrap();

            let mut tokens = Vec::new();
            collect_hex_tokens(&parsed, &mut tokens);
            for token in &tokens {
                // The class dispatch agrees with the rule function...
                assert_token_matches_rule(token);
                // ...and a committed cassette carries NO minimal-form
                // quantity: the recorder decimalizes it at record time, so
                // one in a committed file means a hand edit or a bypassed
                // recorder (the drift gate would also fail the mutated
                // canonical bytes).
                assert!(
                    !matches!(hex_token_class(token), HexTokenClass::MinimalQuantity),
                    "{token:?} in {}: a minimal-form quantity cannot survive \
                     recording — regenerate through the recorder",
                    path.display()
                );
            }
            total_tokens += tokens.len();

            // Entry-level byte-exactness: the ledger key re-derives from
            // its parsed (method, params), and the canonical response is a
            // fixed point of canonicalize ∘ wire — replay restores the
            // recorded wire form and re-recording it reproduces the
            // committed bytes, for both classes.
            for (key, entry) in &cassette.entries {
                let pair: Value = serde_json::from_str(key).unwrap();
                let items = pair.as_array().expect("ledger key is [method, params]");
                let method = items[0].as_str().expect("method is a string");
                // The ledger key re-derives from its parsed (method, params).
                // Compared SEMANTICALLY (the OR-set normalization applied to
                // both sides): keys recorded before the recorder began
                // normalizing getLogs OR-set order carry alloy's per-process
                // random FilterSet order, and the replay transport's semantic
                // fallback is what keeps them replayable — the guard pins
                // that both eras collide with their re-derivation under that
                // same order-free comparison.
                let rederived = entry_key(method, &items[1]);
                let semantic_form = |key: &str| -> String {
                    let pair: Value = serde_json::from_str(key).unwrap();
                    let mut params = pair.as_array().expect("key is [method, params]")[1].clone();
                    if method == "eth_getLogs" {
                        params = crate::cassette::normalize_log_filter_or_sets(params);
                    }
                    serde_json::to_string(&Value::Array(vec![
                        Value::String(method.to_string()),
                        params,
                    ]))
                    .unwrap()
                };
                assert_eq!(
                    semantic_form(&rederived),
                    semantic_form(key),
                    "ledger key does not re-derive semantically in {}",
                    path.display()
                );
                let response_value = serde_json::to_value(&entry.response).unwrap();
                let restored = canonicalize_value(&wire_value(&response_value));
                assert_eq!(
                    restored,
                    response_value,
                    "canonicalize ∘ wire is not the identity on a response in {}",
                    path.display()
                );
                total_entries += 1;
            }
        }
        assert!(
            total_tokens > 0,
            "the corpus guard scanned zero 0x-hex tokens — vacuous; the corpus changed shape"
        );
        assert!(
            total_entries > 0,
            "the corpus guard scanned zero ledger entries — vacuous; the corpus changed shape"
        );
    }

    /// Red path for the precision rule, demonstrated on an in-memory copy:
    /// a committed cassette's copy carrying a short NON-minimal value —
    /// `0x00000000`, the exact class the earlier rule decimalized — is
    /// flagged, and both halves of the rule's necessity are asserted. Under
    /// the implemented rule the token is preserved verbatim
    /// (`canonical_hex_quantity` refuses it; the canonicalize ∘ wire round
    /// trip is byte-exact). Under a decimalizing rule the same token
    /// decimalizes to `0` and replay re-hexes it to `0x0` — different
    /// bytes, in an odd-length form `Bytes` decoding rejects; that lossy
    /// decimalize-then-restore is asserted here as the counterfactual the
    /// rule exists to prevent.
    #[test]
    fn corpus_guard_flags_nonminimal_short_hex_on_in_memory_copy() {
        // In-memory copy (never written to disk): the pool cassette's
        // recorded eth_blockNumber answer becomes the non-minimal token.
        let path = corpus_dir().join("pool_v3_created_26102622-26102626.json");
        let bytes = read_artifact(&path).unwrap();
        let mut cassette = Cassette::from_json_bytes(&bytes).unwrap();
        let block_key = cassette
            .entries
            .keys()
            .find(|k| k.starts_with("[\"eth_blockNumber\""))
            .expect("the pool cassette records eth_blockNumber")
            .clone();
        // Mutate the recorded answer inside a scoped borrow; the mutated
        // response's canonical JSON form is what the fixed-point check
        // replays below.
        let response_value = {
            let entry = cassette.entries.get_mut(&block_key).expect("entry present");
            entry.response = CassetteResponse::Success {
                result: Value::String("0x00000000".to_string()),
            };
            serde_json::to_value(&entry.response).unwrap()
        };

        // The guard's scan flags the injected token: it classes as
        // NonMinimalHex — the protected byte-hex class — and the rule
        // function refuses to decimalize it (preserved verbatim).
        let copy = serde_json::to_value(&cassette).unwrap();
        let mut tokens = Vec::new();
        collect_hex_tokens(&copy, &mut tokens);
        assert!(
            tokens.contains(&"0x00000000".to_string()),
            "the scanner must see the injected non-minimal token"
        );
        assert_token_matches_rule("0x00000000");
        assert_eq!(hex_token_class("0x00000000"), HexTokenClass::NonMinimalHex);
        assert_eq!(canonical_hex_quantity("0x00000000"), None);

        // The mutated entry round-trips byte-exactly: replay restores the
        // recorded wire form and re-recording it reproduces the canonical
        // bytes — the rule working as designed on this input class.
        assert_eq!(
            canonicalize_value(&wire_value(&response_value)),
            response_value
        );

        // The counterfactual (what a decimalizing rule does to this token): the
        // lossy decimalize-then-restore that mangled such values —
        // 0x00000000 → "0" → replayed "0x0".
        let pre_fix_decimal = u64::from_str_radix("00000000", 16)
            .expect("8 hex digits fit a u64")
            .to_string();
        assert_eq!(pre_fix_decimal, "0");
        let pre_fix_restored = wire_hex_quantity(&pre_fix_decimal).expect("decimal re-hexes");
        assert_eq!(pre_fix_restored, "0x0");
        assert_ne!(
            pre_fix_restored, "0x00000000",
            "the decimalize-then-restore mangles this value class; \
             the precision rule must keep preserving it verbatim"
        );
    }
}

/// The artifact codec: the package is transport, the decoded bytes are the
/// format.
#[cfg(all(test, feature = "cassette-artifacts"))]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod artifact_tests {
    use super::*;

    /// One throwaway tree under the system temp directory, removed on drop.
    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "degenbot-cassette-artifact-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// A one-entry cassette whose recorded answer is `result`.
    fn cassette(result: Value) -> Cassette {
        let response = CassetteResponse::Success { result };
        let digest = entry_digest(&response);
        let mut entries = BTreeMap::new();
        entries.insert(
            entry_key("eth_blockNumber", &serde_json::json!([])),
            CassetteEntry { response, digest },
        );
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: CassetteProvenance {
                source: "mock-node/1.0".to_string(),
                recorded_at: rfc3339_utc(1_770_000_000),
                span: CassetteSpan {
                    from_block: 1,
                    to_block: 1,
                },
            },
            entries,
        }
    }

    #[test]
    fn a_package_round_trips_to_its_canonical_bytes() {
        let dir = ScratchDir::new("round-trip");
        let logical = dir.path().join("capture.json");
        let cassette = cassette(Value::String("4660".to_string()));
        let canonical = cassette.canonical_bytes().unwrap();

        let package = write_artifact(&logical, &canonical).unwrap();

        assert_eq!(package, dir.path().join("capture.json.zst"));
        assert!(
            !logical.exists(),
            "the plain path is never written: the package is the artifact"
        );
        assert_eq!(read_artifact(&logical).unwrap(), canonical);
        assert_eq!(
            read_artifact(&package).unwrap(),
            canonical,
            "an explicit .zst path decodes directly"
        );
        verify_artifact(&logical).expect("a packaged cassette passes the drift gate");
        assert_eq!(Cassette::from_artifact(&logical).unwrap(), cassette);
    }

    #[test]
    fn a_plain_recording_wins_over_its_package() {
        let dir = ScratchDir::new("plain-wins");
        let logical = dir.path().join("fresh.json");
        write_artifact(
            &logical,
            &cassette(Value::String("4660".to_string()))
                .canonical_bytes()
                .unwrap(),
        )
        .unwrap();
        let plain = cassette(Value::String("4661".to_string()))
            .canonical_bytes()
            .unwrap();
        std::fs::write(&logical, &plain).unwrap();

        assert_eq!(
            read_artifact(&logical).unwrap(),
            plain,
            "the unpackaged recording beside the package is the newest artifact"
        );
    }

    #[test]
    fn a_missing_artifact_names_both_paths() {
        let dir = ScratchDir::new("missing");

        let error = read_artifact(dir.path().join("absent.json"))
            .expect_err("an absent artifact is an error");
        let message = error.to_string();
        assert!(message.contains("absent.json"), "{message}");
        assert!(message.contains("absent.json.zst"), "{message}");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_corrupt_package_is_refused() {
        let dir = ScratchDir::new("corrupt");
        std::fs::write(dir.path().join("broken.json.zst"), b"not a zstd frame").unwrap();

        assert!(read_artifact(dir.path().join("broken.json")).is_err());
        assert!(verify_artifact(dir.path().join("broken.json")).is_err());
    }

    /// Negative probe (ADR-068 D4): the gate has to stay red through the
    /// package, or compression would launder a mutated recording.
    #[test]
    fn a_mutated_answer_fails_the_gate_through_the_package() {
        let dir = ScratchDir::new("mutated");
        let logical = dir.path().join("mutated.json");
        let mut mutated = cassette(Value::String("4660".to_string()));
        mutated.entries.values_mut().next().unwrap().response = CassetteResponse::Success {
            result: Value::String("4661".to_string()),
        };
        write_artifact(&logical, &mutated.canonical_bytes().unwrap()).unwrap();

        let verdict = verify_artifact(&logical).expect_err("the mutated answer must be red");
        assert!(verdict.contains("write digest"), "{verdict}");
    }

    #[test]
    fn packaging_shrinks_a_redundant_artifact() {
        let dir = ScratchDir::new("shrink");
        let logical = dir.path().join("big.json");
        let traced = Value::Array(vec![
            serde_json::json!({
                "topic": "0x0000000000000000000000000000000000000000000000000000000000000000"
            });
            4_000
        ]);
        let canonical = cassette(traced).canonical_bytes().unwrap();

        let package = write_artifact(&logical, &canonical).unwrap();
        let packed = std::fs::metadata(&package).unwrap().len();

        assert!(
            packed * 10 < canonical.len() as u64,
            "packed {packed} bytes against {} canonical: the package is the size win",
            canonical.len()
        );
    }
}
