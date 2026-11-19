//! The recorded fan-out corpus: the `fanout-trace` rows every recorded-read
//! benchmark replays, and the key sets the comparison report is computed over.
//!
//! The corpus is the bot's own `fanout-trace` stream: per candidate, the read
//! identities the simulation seam drained and the wall it measured for the
//! candidate (`sim_ns`). It carries no calldata and no chain snapshot, so a
//! replay drives the read schedule and the recorded execution — never an EVM
//! verdict.
//!
//! # One schema version per corpus, declared on every row
//!
//! Every row carries a `v` key naming the schema it was written under
//! ([`CorpusVersion`]), and one corpus pins exactly one version: [`load_corpus`]
//! refuses a file that mixes versions, a row with no `v` key, and a `v` this
//! reader does not know. The version is never inferred from which keys a row
//! happens to carry — it is what tells a reader which questions the row can
//! answer.
//!
//! | Version | Rows carry | Answers |
//! |---|---|---|
//! | [`CorpusVersion::V1`] | the read walk | read-driven replay only |
//! | [`CorpusVersion::V2`] | the read walk + `relevance` | read-driven replay, and relevance-scoped replay |
//!
//! `relevance` is the candidate's PRODUCTION relevance set, derived by the
//! simulation seam from the candidate's path and the deployment's settlement
//! facts BEFORE it ran (see `degenbot_arbitrage::fanout_trace`). Inside
//! [`CorpusVersion::V2`] the field is not optional: a version-2 row without it is
//! a malformed row, not an old one.
//!
//! # The consumer declares what it needs
//!
//! [`CorpusUse`] is the loader's argument, so a benchmark states its semantics
//! where it opens the corpus instead of discovering them when a set comes up
//! short:
//!
//! - [`CorpusUse::ReadDriven`] replays the read walk. A version-1 corpus
//!   satisfies it; its rows simply have no relevance to read.
//! - [`CorpusUse::RelevanceDependent`] scopes a snapshot by production relevance.
//!   Only a version-2 corpus satisfies it — a version-1 corpus is refused
//!   ([`CorpusError::ReadDrivenOnly`]) rather than served its reads.
//!
//! Recorded reads are what the run turned out to read: an oracle the live run did
//! not have when it had to decide what to snapshot. No door here substitutes them
//! for relevance — [`row_relevance`] and [`anchor_relevance`] answer
//! [`CorpusError::RelevanceNotRecorded`] for a version-1 row — so a
//! relevance-dependent benchmark over an old capture re-captures it, and a
//! read-driven benchmark says so at its own load call.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use alloy::primitives::{Address, U256};
use serde_json::Value;

/// The tag the bot's tracing formatter prefixes each recorded row with.
pub const TRACE_TAG: &str = "fanout-trace ";

/// The recorded corpus the benchmarks are defined over.
pub const DEFAULT_TRACE: &str = "/tmp/fanout_trace.log";

/// The key naming a row's schema version.
pub const VERSION_KEY: &str = "v";

/// The key carrying a row's production relevance set.
pub const RELEVANCE_KEY: &str = "relevance";

/// The fan-out trace row's schema version, declared on every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorpusVersion {
    /// The read walk only: no pre-simulation relevance exists in the wire, so a
    /// version-1 row answers a read-driven replay and nothing else.
    V1,
    /// The read walk beside the production `relevance` set the simulation seam
    /// derived from the candidate's path and the deployment's settlement facts
    /// before the candidate ran.
    V2,
}

impl CorpusVersion {
    /// The version every current capture writes.
    pub const CURRENT: Self = Self::V2;

    /// The integer the `v` key carries.
    #[must_use]
    pub const fn number(self) -> u64 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }

    /// The version a `v` value names, when this reader knows it.
    #[must_use]
    pub const fn from_number(number: u64) -> Option<Self> {
        match number {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }

    /// Whether the version records the production relevance set.
    #[must_use]
    pub const fn records_relevance(self) -> bool {
        matches!(self, Self::V2)
    }

    /// The label a report prints beside the scope its numbers were measured
    /// under.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V1 => "v1-reads-only",
            Self::V2 => "v2-production-relevance",
        }
    }
}

impl fmt::Display for CorpusVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.number())
    }
}

/// What a consumer asks of a recorded corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpusUse {
    /// The benchmark replays the recorded read walk. A version-1 corpus
    /// satisfies it: there is no relevance to read.
    ReadDriven,
    /// The benchmark scopes a snapshot by the production relevance set. Only a
    /// version-2 corpus satisfies it.
    RelevanceDependent,
}

impl CorpusUse {
    /// The label a report prints beside the semantics its numbers were measured
    /// under.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadDriven => "read-driven",
            Self::RelevanceDependent => "relevance-dependent",
        }
    }
}

/// One recorded row's read groups: an address and the slots the trace lists
/// beside it.
pub type ReadGroups = Vec<(Address, Vec<U256>)>;

/// One tracked read identity.
pub type Key = (Address, U256);

/// One row's recorded relevance, exactly as the row's schema version declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowRelevance {
    /// A version-1 row: the wire carries no pre-simulation set.
    NotRecorded,
    /// A version-2 row: the seam's pre-simulation production set.
    Production(Vec<Address>),
}

impl RowRelevance {
    /// The schema version this field IS — a row cannot claim a version its shape
    /// contradicts.
    #[must_use]
    pub const fn version(&self) -> CorpusVersion {
        match self {
            Self::NotRecorded => CorpusVersion::V1,
            Self::Production(_) => CorpusVersion::V2,
        }
    }
}

/// One recorded candidate: the read walk the simulation seam drained, the wall it
/// measured, and the relevance its schema version records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// The anchor the candidate was simulated at.
    pub block: u64,
    /// The wall the seam measured (encode + sequence + assess).
    pub sim_ns: u64,
    /// The candidate's recorded read walk, in recorded order.
    pub reads: ReadGroups,
    /// The relevance the row's schema version records: the seam's pre-simulation
    /// production set for a version-2 row, nothing for a version-1 row. Never a
    /// set derived from [`Self::reads`].
    pub relevance: RowRelevance,
}

impl Recorded {
    /// The schema version this row was written under.
    #[must_use]
    pub const fn version(&self) -> CorpusVersion {
        self.relevance.version()
    }

    /// The row's production relevance set.
    ///
    /// # Errors
    ///
    /// [`CorpusError::RelevanceNotRecorded`] for a version-1 row: its recorded
    /// read addresses are what the run turned out to read — an oracle the live
    /// run did not have when it had to decide what to snapshot — so they are
    /// never served in place of the set.
    pub fn production_relevance(&self) -> Result<&[Address], CorpusError> {
        match &self.relevance {
            RowRelevance::Production(addresses) => Ok(addresses),
            RowRelevance::NotRecorded => {
                Err(CorpusError::RelevanceNotRecorded { block: self.block })
            }
        }
    }
}

/// Why a recorded corpus cannot serve what its consumer asked of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusError {
    /// A row carries no `v` key at all.
    MissingVersion,
    /// A row's `v` names a schema this reader does not know.
    UnknownVersion {
        /// The version key's rendered value.
        found: String,
    },
    /// A row's keys contradict the version it declares.
    VersionShape {
        /// The version the row declares.
        version: CorpusVersion,
        /// How its keys contradict it.
        detail: &'static str,
    },
    /// A tagged row whose fields do not parse.
    Malformed {
        /// Which field, and what was wrong with it.
        detail: String,
    },
    /// A corpus spans more than one schema version.
    MixedVersions {
        /// The version the corpus started with.
        first: CorpusVersion,
        /// The version that appears later.
        found: CorpusVersion,
    },
    /// The corpus carries no trace row at all.
    NoRows,
    /// The corpus file could not be read.
    Unreadable {
        /// The transport error.
        detail: String,
    },
    /// A relevance question reached a version-1 row.
    RelevanceNotRecorded {
        /// The anchor the row was recorded at.
        block: u64,
    },
    /// A relevance-dependent consumer asked for a version-1 corpus.
    ReadDrivenOnly {
        /// The corpus's version.
        version: CorpusVersion,
    },
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVersion => f.write_str(
                "a trace row carries no `v` schema version — an unversioned capture cannot be \
                 replayed; re-capture it, or stamp each row with `\"v\":1` (read-driven) or \
                 `\"v\":2` (production relevance) once its shape is known",
            ),
            Self::UnknownVersion { found } => write!(
                f,
                "a trace row declares schema version {found}, which this reader does not know \
                 (it reads v1 and v2)"
            ),
            Self::VersionShape { version, detail } => write!(f, "a {version} row {detail}"),
            Self::Malformed { detail } => write!(f, "a trace row did not parse: {detail}"),
            Self::MixedVersions { first, found } => write!(
                f,
                "a corpus must pin one schema: {first} started it and {found} appears later"
            ),
            Self::NoRows => write!(f, "no `{TRACE_TAG}` row"),
            Self::Unreadable { detail } => write!(f, "read the recorded fan-out trace: {detail}"),
            Self::RelevanceNotRecorded { block } => write!(
                f,
                "block {block}: the row is v1 and records no production relevance — a \
                 relevance-dependent replay cannot substitute the reads the run turned out to make"
            ),
            Self::ReadDrivenOnly { version } => write!(
                f,
                "the corpus is {version} (reads only) and a relevance-dependent replay needs v2 \
                 rows that recorded the pre-simulation set"
            ),
        }
    }
}

impl std::error::Error for CorpusError {}

/// A corpus-level failure, with the file and line it was found at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpusLoadError {
    /// The corpus file.
    pub path: String,
    /// The one-based line, when the failure is a row rather than the file.
    pub line: Option<usize>,
    /// What was wrong.
    pub error: CorpusError,
}

impl fmt::Display for CorpusLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{} line {line}: {}", self.path, self.error),
            None => write!(f, "{}: {}", self.path, self.error),
        }
    }
}

impl std::error::Error for CorpusLoadError {}

/// A recorded corpus: rows that pin one schema version, checked against the
/// semantics its consumer declared.
#[derive(Clone, Debug)]
pub struct Corpus {
    version: CorpusVersion,
    rows: Vec<Recorded>,
}

impl Corpus {
    /// Pin a row slice to one schema version and to `used`'s semantics.
    ///
    /// # Errors
    ///
    /// [`CorpusError::MixedVersions`] when the rows disagree,
    /// [`CorpusError::NoRows`] for an empty slice, and
    /// [`CorpusError::ReadDrivenOnly`] when a relevance-dependent consumer asks
    /// for version-1 rows.
    pub fn from_rows(rows: Vec<Recorded>, used: CorpusUse) -> Result<Self, CorpusError> {
        let version = Self::check(&rows, used)?;
        Ok(Self { version, rows })
    }

    /// The version a row slice pins, gated against the semantics `used` declares —
    /// for a reader that keeps its own rows instead of handing them over.
    ///
    /// # Errors
    ///
    /// [`CorpusError::MixedVersions`] when the rows disagree,
    /// [`CorpusError::NoRows`] for an empty slice, and
    /// [`CorpusError::ReadDrivenOnly`] when a relevance-dependent consumer asks
    /// for version-1 rows.
    pub fn check<'a>(
        rows: impl IntoIterator<Item = &'a Recorded>,
        used: CorpusUse,
    ) -> Result<CorpusVersion, CorpusError> {
        let version = corpus_version(rows)?;
        if used == CorpusUse::RelevanceDependent && !version.records_relevance() {
            return Err(CorpusError::ReadDrivenOnly { version });
        }
        Ok(version)
    }

    /// The schema version every row pins.
    #[must_use]
    pub const fn version(&self) -> CorpusVersion {
        self.version
    }

    /// The corpus's rows, in file order.
    #[must_use]
    pub fn rows(&self) -> &[Recorded] {
        &self.rows
    }

    /// Take the rows, dropping the version they were already gated by.
    #[must_use]
    pub fn into_rows(self) -> Vec<Recorded> {
        self.rows
    }

    /// The label a report prints for the scope its numbers were measured under.
    #[must_use]
    pub const fn scope_label(&self) -> &'static str {
        self.version.label()
    }
}

/// The one version a row slice pins — for a caller holding rows rather than a
/// loaded [`Corpus`].
///
/// # Errors
///
/// [`CorpusError::MixedVersions`] when the rows disagree, and
/// [`CorpusError::NoRows`] for an empty slice.
pub fn corpus_version<'a>(
    rows: impl IntoIterator<Item = &'a Recorded>,
) -> Result<CorpusVersion, CorpusError> {
    pin(rows.into_iter().map(Recorded::version))
}

/// Pin one schema version across a stream of declared versions.
///
/// The single owner of "one corpus pins one schema": every reader, including one
/// that keeps its own row type, pins through here rather than re-deriving the
/// rule.
///
/// # Errors
///
/// [`CorpusError::MixedVersions`] when a later version differs from the first,
/// and [`CorpusError::NoRows`] for an empty stream.
pub fn pin(
    versions: impl IntoIterator<Item = CorpusVersion>,
) -> Result<CorpusVersion, CorpusError> {
    let mut pinned: Option<CorpusVersion> = None;
    for version in versions {
        match pinned {
            None => pinned = Some(version),
            Some(first) if first != version => {
                return Err(CorpusError::MixedVersions {
                    first,
                    found: version,
                });
            }
            Some(_) => {}
        }
    }
    pinned.ok_or(CorpusError::NoRows)
}

/// The schema version a parsed trace-row object declares, checked against the
/// semantics `used` declares — for a reader that parses its own fields out of the
/// same JSON object.
///
/// The version key, the version's relevance shape, and the consumer's
/// requirement are all this module's rules, so a private parser cannot drift from
/// the corpus reader: an unversioned object, an unknown version, keys that
/// contradict the version, and a v1 object under a relevance-dependent consumer
/// are each the same typed [`CorpusError`] here as at [`load_corpus`].
///
/// # Errors
///
/// [`CorpusError::MissingVersion`], [`CorpusError::UnknownVersion`],
/// [`CorpusError::VersionShape`], [`CorpusError::Malformed`], or
/// [`CorpusError::ReadDrivenOnly`].
pub fn check_row(value: &Value, used: CorpusUse) -> Result<CorpusVersion, CorpusError> {
    let version = declared_version(value)?;
    parse_relevance(version, value)?;
    if used == CorpusUse::RelevanceDependent && !version.records_relevance() {
        return Err(CorpusError::ReadDrivenOnly { version });
    }
    Ok(version)
}

/// Load a corpus, refusing any file whose rows do not pin one known schema
/// version and whose version cannot serve `used`.
///
/// The loader tracks the pinned version itself only to name the offending line;
/// the gate is [`Corpus::from_rows`]'s.
///
/// # Errors
///
/// A missing file, a row with no `v` key, a row naming a version this reader
/// does not know, a row whose keys contradict its version, mixed versions, a file
/// with no trace row, and a relevance-dependent request over a version-1 corpus —
/// each a typed [`CorpusError`] wrapped with the file and line.
pub fn load_corpus(path: &str, used: CorpusUse) -> Result<Corpus, CorpusLoadError> {
    let text = std::fs::read_to_string(path).map_err(|error| CorpusLoadError {
        path: path.to_string(),
        line: None,
        error: CorpusError::Unreadable {
            detail: error.to_string(),
        },
    })?;
    let mut rows: Vec<Recorded> = Vec::new();
    let mut pinned: Option<CorpusVersion> = None;
    for (index, line) in text.lines().enumerate() {
        let row = parse_trace_line(line).map_err(|error| CorpusLoadError {
            path: path.to_string(),
            line: Some(index + 1),
            error,
        })?;
        let Some(row) = row else { continue };
        match pinned {
            None => pinned = Some(row.version()),
            Some(first) if first != row.version() => {
                return Err(CorpusLoadError {
                    path: path.to_string(),
                    line: Some(index + 1),
                    error: CorpusError::MixedVersions {
                        first,
                        found: row.version(),
                    },
                });
            }
            Some(_) => {}
        }
        rows.push(row);
    }
    Corpus::from_rows(rows, used).map_err(|error| CorpusLoadError {
        path: path.to_string(),
        line: None,
        error,
    })
}

/// Parse one log line's trace row (the substring after `fanout-trace `).
///
/// A line with no trace tag is `Ok(None)`: a capture interleaves unrelated log
/// output, and only a tagged line makes a schema promise.
///
/// # Errors
///
/// A tagged row this reader cannot stand behind: no `v` key, an unknown `v`,
/// keys that contradict the declared version, or a field that does not parse.
pub fn parse_trace_line(line: &str) -> Result<Option<Recorded>, CorpusError> {
    let Some(start) = line.find(TRACE_TAG) else {
        return Ok(None);
    };
    let value: Value =
        serde_json::from_str(line[start + TRACE_TAG.len()..].trim()).map_err(|error| {
            CorpusError::Malformed {
                detail: error.to_string(),
            }
        })?;
    let version = declared_version(&value)?;
    Ok(Some(Recorded {
        block: integer(&value, "block")?,
        sim_ns: integer(&value, "sim_ns")?,
        reads: parse_reads(&value)?,
        relevance: parse_relevance(version, &value)?,
    }))
}

/// The version a row declares. An unversioned row is refused, never inferred
/// from the keys it happens to carry.
fn declared_version(value: &Value) -> Result<CorpusVersion, CorpusError> {
    let Some(raw) = value.get(VERSION_KEY) else {
        return Err(CorpusError::MissingVersion);
    };
    let Some(number) = raw.as_u64() else {
        return Err(CorpusError::UnknownVersion {
            found: raw.to_string(),
        });
    };
    CorpusVersion::from_number(number).ok_or_else(|| CorpusError::UnknownVersion {
        found: raw.to_string(),
    })
}

/// One integer field of a row.
fn integer(value: &Value, key: &str) -> Result<u64, CorpusError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| CorpusError::Malformed {
            detail: format!("no integer `{key}`"),
        })
}

/// A row's read walk: `address|slot,slot` per entry, an empty slot list a
/// trailing `|`.
fn parse_reads(value: &Value) -> Result<ReadGroups, CorpusError> {
    let entries = value
        .get("reads")
        .and_then(Value::as_array)
        .ok_or_else(|| CorpusError::Malformed {
            detail: "no `reads` array".to_string(),
        })?;
    entries
        .iter()
        .map(|entry| {
            let text = entry.as_str().ok_or_else(|| CorpusError::Malformed {
                detail: "a `reads` entry is not a string".to_string(),
            })?;
            let (address, slots) = text.split_once('|').ok_or_else(|| CorpusError::Malformed {
                detail: format!("a `reads` entry carries no `|`: {text}"),
            })?;
            let address = address
                .parse::<Address>()
                .map_err(|error| CorpusError::Malformed {
                    detail: format!("a `reads` address: {error}"),
                })?;
            let slots = slots
                .split(',')
                .filter(|slot| !slot.is_empty())
                .map(|slot| {
                    slot.parse::<U256>()
                        .map_err(|error| CorpusError::Malformed {
                            detail: format!("a `reads` slot: {error}"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((address, slots))
        })
        .collect()
}

/// The relevance the declared version requires, or the typed refusal.
fn parse_relevance(version: CorpusVersion, value: &Value) -> Result<RowRelevance, CorpusError> {
    let entries = value.get(RELEVANCE_KEY);
    match (version.records_relevance(), entries) {
        (false, None) => Ok(RowRelevance::NotRecorded),
        (false, Some(_)) => Err(CorpusError::VersionShape {
            version,
            detail: "carries a `relevance` key its schema does not define",
        }),
        (true, None) => Err(CorpusError::VersionShape {
            version,
            detail: "records no `relevance` set its schema requires",
        }),
        (true, Some(entries)) => {
            let entries = entries.as_array().ok_or_else(|| CorpusError::Malformed {
                detail: "`relevance` is not an array".to_string(),
            })?;
            let addresses = entries
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .ok_or_else(|| CorpusError::Malformed {
                            detail: "a `relevance` entry is not a string".to_string(),
                        })?
                        .parse::<Address>()
                        .map_err(|error| CorpusError::Malformed {
                            detail: format!("a `relevance` address: {error}"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(RowRelevance::Production(addresses))
        }
    }
}

/// Group the rows into fan-outs: one per recorded anchor, in file order.
#[must_use]
pub fn group_by_anchor(rows: Vec<Recorded>) -> Vec<(u64, Vec<Recorded>)> {
    let mut groups: Vec<(u64, Vec<Recorded>)> = Vec::new();
    for row in rows {
        match groups.last_mut() {
            Some((block, group)) if *block == row.block => group.push(row),
            _ => groups.push((row.block, vec![row])),
        }
    }
    groups
}

/// The rows' distinct read identities, sorted.
#[must_use]
pub fn anchor_keys(rows: &[Recorded]) -> BTreeSet<Key> {
    rows.iter()
        .flat_map(|row| {
            row.reads
                .iter()
                .flat_map(|(address, slots)| slots.iter().map(move |slot| (*address, *slot)))
        })
        .collect()
}

/// Every anchor's distinct recorded identities, sorted — the coalesced read set
/// one hub at that anchor would carry.
#[must_use]
pub fn anchors_keys(rows: &[Recorded]) -> BTreeMap<u64, Vec<Key>> {
    let mut anchors: BTreeMap<u64, BTreeSet<Key>> = BTreeMap::new();
    for row in rows {
        let keys = anchors.entry(row.block).or_default();
        for (address, slots) in &row.reads {
            for slot in slots {
                keys.insert((*address, *slot));
            }
        }
    }
    anchors
        .into_iter()
        .map(|(block, keys)| (block, keys.into_iter().collect()))
        .collect()
}

/// How many recorded read calls each identity carries — the denominator of the
/// snapshot-hit classification.
#[must_use]
pub fn read_call_counts(rows: &[Recorded]) -> BTreeMap<Key, u64> {
    let mut counts: BTreeMap<Key, u64> = BTreeMap::new();
    for row in rows {
        for (address, slots) in &row.reads {
            for slot in slots {
                *counts.entry((*address, *slot)).or_default() += 1;
            }
        }
    }
    counts
}

/// The rows' distinct recorded read addresses, in first-seen order.
///
/// This is what the run turned out to READ — the oracle leg of a paired
/// measurement (`relevance_pairs`), never production relevance: a replay that
/// scoped a snapshot with it would be filtering by an address it could only
/// know after the read.
#[must_use]
pub fn candidate_addresses(rows: &[Recorded]) -> Vec<Address> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for row in rows {
        for (address, _) in &row.reads {
            if seen.insert(*address) {
                ordered.push(*address);
            }
        }
    }
    ordered
}

/// One recorded candidate's production relevance set.
///
/// # Errors
///
/// [`CorpusError::RelevanceNotRecorded`] for a version-1 row — see
/// [`Recorded::production_relevance`].
pub fn row_relevance(row: &Recorded) -> Result<&[Address], CorpusError> {
    row.production_relevance()
}

/// An anchor's production relevance set: the deduped union of its rows' sets,
/// in row order.
///
/// # Errors
///
/// [`CorpusError::RelevanceNotRecorded`] naming the first version-1 row: a
/// partial union would scope a snapshot by a set the live run could not have
/// had.
pub fn anchor_relevance(rows: &[Recorded]) -> Result<Vec<Address>, CorpusError> {
    let mut seen = BTreeSet::new();
    let mut addresses = Vec::new();
    for row in rows {
        for address in row_relevance(row)? {
            if seen.insert(*address) {
                addresses.push(*address);
            }
        }
    }
    Ok(addresses)
}

/// The corpus's set analysis: the counts a corpus description pins, computed
/// from the parsed rows so a replay and the live probe compare the same
/// numbers instead of trusting a printed line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorpusShape {
    /// Recorded candidates (trace rows).
    pub candidates: usize,
    /// Candidate anchors: blocks the corpus recorded candidates at.
    pub candidate_anchors: usize,
    /// Recorded candidate-anchor keys: every recorded slot read, repeats
    /// included.
    pub candidate_anchor_keys: usize,
    /// Distinct identities per anchor, summed — the cold leg's anchored reads.
    pub anchored_keys: usize,
    /// Distinct identities over the whole corpus.
    pub global_keys: usize,
    /// Distinct addresses the corpus reads.
    pub contracts: usize,
}

/// Measure one corpus's shape.
#[must_use]
pub fn shape(rows: &[Recorded]) -> CorpusShape {
    let mut anchors: BTreeMap<u64, BTreeSet<Key>> = BTreeMap::new();
    let mut global: BTreeSet<Key> = BTreeSet::new();
    let mut contracts: BTreeSet<Address> = BTreeSet::new();
    let mut candidate_anchor_keys = 0usize;
    for row in rows {
        let anchor = anchors.entry(row.block).or_default();
        for (address, slots) in &row.reads {
            contracts.insert(*address);
            for slot in slots {
                candidate_anchor_keys += 1;
                anchor.insert((*address, *slot));
                global.insert((*address, *slot));
            }
        }
    }
    CorpusShape {
        candidates: rows.len(),
        candidate_anchors: anchors.len(),
        candidate_anchor_keys,
        anchored_keys: anchors.values().map(BTreeSet::len).sum(),
        global_keys: global.len(),
        contracts: contracts.len(),
    }
}

/// A candidate-density regime: the size class of an anchor's fan-out. The
/// thresholds are a declared taxonomy over fan-out size, not a fit to any
/// corpus, so segments from different corpora stay comparable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DensityRegime {
    /// A trickle anchor: at most [`SPARSE_MAX`] candidates.
    Sparse,
    /// An ordinary block: up to fifty candidates.
    Moderate,
    /// A burst block: at least [`BURST_MIN`] candidates.
    Burst,
}

/// The largest fan-out still [`DensityRegime::Sparse`].
pub const SPARSE_MAX: usize = 14;

/// The smallest fan-out already [`DensityRegime::Burst`].
pub const BURST_MIN: usize = 51;

impl DensityRegime {
    /// The short label the report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sparse => "sparse",
            Self::Moderate => "moderate",
            Self::Burst => "burst",
        }
    }
}

/// Classify one anchor's fan-out into the declared density regimes.
#[must_use]
pub const fn classify_density(candidates: usize) -> DensityRegime {
    if candidates <= SPARSE_MAX {
        DensityRegime::Sparse
    } else if candidates < BURST_MIN {
        DensityRegime::Moderate
    } else {
        DensityRegime::Burst
    }
}

/// One anchor's measured place in the corpus: how big its fan-out was and how
/// much of its read set the corpus had never seen before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorProfile {
    /// The anchor's block.
    pub block: u64,
    /// The density regime its fan-out lands in.
    pub regime: DensityRegime,
    /// Recorded candidates at the anchor.
    pub candidates: usize,
    /// Recorded reads at the anchor, repeats included.
    pub read_calls: usize,
    /// Distinct identities the anchor reads.
    pub keys: usize,
    /// Identities whose corpus-first appearance is this anchor.
    pub new_keys: usize,
    /// Distinct addresses the anchor reads.
    pub contracts: usize,
}

/// One density regime's segment: every anchor the corpus recorded in the
/// regime, with the read-set diversity measured over their reads. The anchors
/// need not be contiguous — the segment is the regime, not a phase — and a
/// regime the corpus never recorded produces no segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The density regime every anchor of the segment lands in.
    pub regime: DensityRegime,
    /// The segment's anchors' blocks, in block order.
    pub anchors: Vec<u64>,
    /// Recorded candidates over the segment's anchors.
    pub candidates: usize,
    /// Recorded reads over the segment's anchors, repeats included.
    pub read_calls: usize,
    /// Distinct identities the segment's anchors read.
    pub keys: BTreeSet<Key>,
    /// Keys whose corpus-first appearance is one of the segment's anchors.
    pub new_keys: usize,
    /// Keys the corpus read at an earlier anchor before this segment read them.
    pub repeated_keys: usize,
    /// Distinct addresses the segment's anchors read.
    pub contracts: usize,
    /// Blocks since the key's corpus-first appearance, summed over every
    /// distinct `(anchor, key)` read, and the count of those pairs.
    pub key_age_sum_blocks: u64,
    /// The count `key_age_sum_blocks` averages over.
    pub key_age_pairs: u64,
    /// The longest blocks-since-first-appearance any recorded read carried.
    pub key_age_max_blocks: u64,
    /// Anchors-that-read-the-key summed over the segment's distinct keys over
    /// the whole corpus: the carry-forward's reuse depth per key.
    pub survival_anchor_sum: u64,
    /// Segment keys more than one anchor reads.
    pub recurred_keys: usize,
    /// Segment keys some anchor past the segment's last anchor still reads.
    pub surviving_keys: usize,
}

/// The per-anchor read sets, and the per-key first appearance and anchor
/// membership, the segmentation measures over.
#[derive(Default)]
struct AnchorReads {
    candidates: usize,
    read_calls: usize,
    keys: BTreeSet<Key>,
    contracts: BTreeSet<Address>,
}

/// The segmentation's inputs: every anchor's reads beside the per-key history.
#[derive(Default)]
struct CorpusReads {
    anchors: BTreeMap<u64, AnchorReads>,
    first_seen: BTreeMap<Key, u64>,
    seen_at: BTreeMap<Key, BTreeSet<u64>>,
}

fn anchor_reads(rows: &[Recorded]) -> CorpusReads {
    let mut reads = CorpusReads::default();
    for row in rows {
        let anchor = reads.anchors.entry(row.block).or_default();
        anchor.candidates += 1;
        for (address, slots) in &row.reads {
            anchor.contracts.insert(*address);
            for slot in slots {
                anchor.read_calls += 1;
                let key = (*address, *slot);
                anchor.keys.insert(key);
                reads
                    .first_seen
                    .entry(key)
                    .and_modify(|first| *first = (*first).min(row.block))
                    .or_insert(row.block);
                reads.seen_at.entry(key).or_default().insert(row.block);
            }
        }
    }
    reads
}

/// Profile every anchor, in block order.
#[must_use]
pub fn anchor_profiles(rows: &[Recorded]) -> Vec<AnchorProfile> {
    let reads = anchor_reads(rows);
    reads
        .anchors
        .iter()
        .map(|(block, anchor)| AnchorProfile {
            block: *block,
            regime: classify_density(anchor.candidates),
            candidates: anchor.candidates,
            read_calls: anchor.read_calls,
            keys: anchor.keys.len(),
            new_keys: anchor
                .keys
                .iter()
                .filter(|key| reads.first_seen[*key] == *block)
                .count(),
            contracts: anchor.contracts.len(),
        })
        .collect()
}

/// Segment the anchors into the density regimes the corpus actually records,
/// one segment per regime in first-appearance order.
#[must_use]
pub fn segments_by_density(rows: &[Recorded]) -> Vec<Segment> {
    let reads = anchor_reads(rows);
    let mut order: Vec<DensityRegime> = Vec::new();
    let mut members: BTreeMap<DensityRegime, Vec<u64>> = BTreeMap::new();
    for (block, anchor) in &reads.anchors {
        let regime = classify_density(anchor.candidates);
        let entry = members.entry(regime).or_default();
        if entry.is_empty() {
            order.push(regime);
        }
        entry.push(*block);
    }
    order
        .into_iter()
        .map(|regime| segment(regime, &members[&regime], &reads))
        .collect()
}

/// Fold one regime's anchors into their segment.
fn segment(regime: DensityRegime, blocks: &[u64], reads: &CorpusReads) -> Segment {
    let mut keys: BTreeSet<Key> = BTreeSet::new();
    let mut contracts: BTreeSet<Address> = BTreeSet::new();
    let mut candidates = 0usize;
    let mut read_calls = 0usize;
    let mut age_sum = 0u64;
    let mut age_pairs = 0u64;
    let mut age_max = 0u64;
    let mut last = 0u64;
    for block in blocks {
        let anchor = &reads.anchors[block];
        last = last.max(*block);
        candidates += anchor.candidates;
        read_calls += anchor.read_calls;
        contracts.extend(anchor.contracts.iter().copied());
        for key in &anchor.keys {
            keys.insert(*key);
            let age = block.saturating_sub(reads.first_seen[key]);
            age_sum += age;
            age_pairs += 1;
            age_max = age_max.max(age);
        }
    }
    let new_keys = keys
        .iter()
        .filter(|key| blocks.contains(&reads.first_seen[*key]))
        .count();
    let mut survival_anchor_sum = 0u64;
    let mut recurred_keys = 0usize;
    let mut surviving_keys = 0usize;
    for key in &keys {
        let read_at = &reads.seen_at[key];
        survival_anchor_sum += u64::try_from(read_at.len()).unwrap_or(u64::MAX);
        recurred_keys += usize::from(read_at.len() > 1);
        surviving_keys += usize::from(read_at.iter().any(|block| *block > last));
    }
    Segment {
        regime,
        anchors: blocks.to_vec(),
        candidates,
        read_calls,
        keys: keys.clone(),
        new_keys,
        repeated_keys: keys.len() - new_keys,
        contracts: contracts.len(),
        key_age_sum_blocks: age_sum,
        key_age_pairs: age_pairs,
        key_age_max_blocks: age_max,
        survival_anchor_sum,
        recurred_keys,
        surviving_keys,
    }
}

/// The corpus's tracked identity universe and its block range, for the modeled
/// chain's write history.
#[must_use]
pub fn universe(rows: &[Recorded]) -> (BTreeSet<Key>, u64, u64) {
    let mut keys = BTreeSet::new();
    let mut first = u64::MAX;
    let mut last = u64::MIN;
    for row in rows {
        first = first.min(row.block);
        last = last.max(row.block);
        for (address, slots) in &row.reads {
            for slot in slots {
                keys.insert((*address, *slot));
            }
        }
    }
    (keys, first, last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version-2 row: the seam's pre-simulation set beside the read walk.
    const PRODUCTION_ROW: &str = r#"DEBUG [sim] fanout-trace {"v":2,"block":9,"cand":"1@1","sim_ns":4,"verdict":"result","gas":7,"reads":["0x1111111111111111111111111111111111111111|0x1"],"relevance":["0x1111111111111111111111111111111111111111","0x2222222222222222222222222222222222222222"]}"#;

    /// A version-1 row: the read walk only, which is all a read-driven replay may
    /// consume from it.
    const READS_ONLY_ROW: &str = r#"DEBUG [sim] fanout-trace {"v":1,"block":9,"cand":"1@1","sim_ns":4,"verdict":"result","gas":7,"reads":["0x1111111111111111111111111111111111111111|0x1","0x3333333333333333333333333333333333333333|"]}"#;

    /// A capture written before the version key existed: its schema is unknown,
    /// so it is refused rather than guessed at.
    const UNVERSIONED_ROW: &str = r#"DEBUG [sim] fanout-trace {"block":9,"cand":"1@1","sim_ns":4,"verdict":"result","gas":7,"reads":["0x1111111111111111111111111111111111111111|0x1"],"relevance":["0x1111111111111111111111111111111111111111"]}"#;

    /// A row claiming a schema this reader has never seen.
    const UNKNOWN_ROW: &str = r#"DEBUG [sim] fanout-trace {"v":3,"block":9,"cand":"1@1","sim_ns":4,"verdict":"result","gas":7,"reads":[]}"#;

    fn address(byte: u8) -> Address {
        Address::new([byte; 20])
    }

    /// Parse one fixture row, failing loudly when the fixture itself is wrong.
    fn parse(line: &str) -> Recorded {
        parse_trace_line(line)
            .expect("the fixture row parses")
            .expect("the fixture line carries a row")
    }

    /// The typed refusal one fixture row produces.
    fn refusal(line: &str) -> CorpusError {
        parse_trace_line(line).expect_err("the fixture row is refused")
    }

    /// Write one fixture corpus and return its path.
    fn corpus_file(name: &str, text: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("fanout_corpus_{}_{name}.log", std::process::id()));
        std::fs::write(&path, text).expect("the fixture corpus writes");
        path.to_str().expect("a utf-8 temp path").to_string()
    }

    /// A version-2 row parses its production set verbatim, and the recorded read
    /// walk is left exactly as it was.
    #[test]
    fn a_version_two_row_parses_its_production_relevance_set_beside_the_reads() {
        let row = parse(PRODUCTION_ROW);
        assert_eq!(row.version(), CorpusVersion::V2);
        assert_eq!(
            row.relevance,
            RowRelevance::Production(vec![address(0x11), address(0x22)]),
            "the production set is the row's own, in the seam's order"
        );
        assert_eq!(row.reads.len(), 1, "the read walk is unchanged: {row:?}");
        assert_eq!(
            row_relevance(&row).expect("a v2 row answers"),
            [address(0x11), address(0x22)],
            "the row answers with the set it recorded"
        );
        assert_eq!(CorpusVersion::CURRENT, CorpusVersion::V2);
        assert_eq!(
            CorpusVersion::CURRENT.number(),
            2,
            "the number is the wire contract"
        );
    }

    /// A version-1 row parses its read walk and refuses every relevance question:
    /// its reads are what the run turned out to read, not the set the live run had
    /// before it read.
    #[test]
    fn a_version_one_row_parses_its_reads_and_refuses_every_relevance_question() {
        let row = parse(READS_ONLY_ROW);
        assert_eq!(row.version(), CorpusVersion::V1);
        assert_eq!(
            row.relevance,
            RowRelevance::NotRecorded,
            "a version-1 row records no set"
        );
        assert_eq!(row.reads.len(), 2, "the read walk parses as before");
        assert_eq!(
            row_relevance(&row),
            Err(CorpusError::RelevanceNotRecorded { block: 9 }),
            "no door substitutes the recorded reads for the set"
        );
        assert_eq!(
            anchor_relevance(std::slice::from_ref(&row)),
            Err(CorpusError::RelevanceNotRecorded { block: 9 }),
            "an anchor built over one version-1 row is refused too"
        );
        assert!(
            !CorpusVersion::V1.records_relevance(),
            "the version itself declares what it cannot supply"
        );
    }

    /// A row with no `v` key is refused, and the refusal names both remedies.
    #[test]
    fn an_unversioned_row_is_refused_and_names_the_offline_migration() {
        let error = refusal(UNVERSIONED_ROW);
        assert_eq!(error, CorpusError::MissingVersion);
        let message = error.to_string();
        assert!(
            message.contains("`v`") && message.contains("re-capture"),
            "the refusal says what to do instead of guessing: {message}"
        );
        assert!(
            message.contains("\"v\":1") && message.contains("\"v\":2"),
            "both stampable versions are named: {message}"
        );
    }

    /// A row naming a schema this reader does not know is refused with the value
    /// it found, never parsed as the nearest version.
    #[test]
    fn an_unknown_version_is_refused_naming_the_version_it_found() {
        assert_eq!(
            refusal(UNKNOWN_ROW),
            CorpusError::UnknownVersion {
                found: "3".to_string()
            }
        );
        assert!(
            CorpusVersion::from_number(0).is_none(),
            "zero is not a version"
        );
    }

    /// The keys must match the declared version in both directions: a version-1
    /// row cannot smuggle a set its schema has no room for, and a version-2 row
    /// cannot omit the set its schema requires.
    #[test]
    fn a_row_whose_keys_contradict_its_version_is_refused() {
        let v1_with_relevance = r#"fanout-trace {"v":1,"block":3,"sim_ns":1,"reads":[],"relevance":["0x1111111111111111111111111111111111111111"]}"#;
        assert_eq!(
            refusal(v1_with_relevance),
            CorpusError::VersionShape {
                version: CorpusVersion::V1,
                detail: "carries a `relevance` key its schema does not define",
            }
        );
        let v2_without_relevance = r#"fanout-trace {"v":2,"block":3,"sim_ns":1,"reads":[]}"#;
        assert_eq!(
            refusal(v2_without_relevance),
            CorpusError::VersionShape {
                version: CorpusVersion::V2,
                detail: "records no `relevance` set its schema requires",
            }
        );
    }

    /// A malformed read entry is a typed row failure, never a silently dropped
    /// read that would shrink a benchmark's denominator.
    #[test]
    fn a_malformed_read_entry_is_refused_instead_of_dropped() {
        let line = r#"fanout-trace {"v":1,"block":3,"sim_ns":1,"reads":["0xnot-an-address|0x1"]}"#;
        assert!(matches!(refusal(line), CorpusError::Malformed { .. }));
        assert_eq!(
            parse_trace_line("a line without a trace row").expect("unrelated lines are skipped"),
            None,
            "an unrelated log line carries no candidate"
        );
    }

    /// The load's semantics decide which versions it accepts: a read-driven
    /// benchmark declares itself and takes either version, while a
    /// relevance-dependent one is refused a version-1 corpus outright.
    #[test]
    fn the_declared_use_decides_which_versions_a_corpus_may_carry() {
        let v2_file = corpus_file("v2", &format!("{PRODUCTION_ROW}\n"));
        let v1_file = corpus_file("v1", &format!("{READS_ONLY_ROW}\n{READS_ONLY_ROW}\n"));

        let read_driven_v1 = load_corpus(&v1_file, CorpusUse::ReadDriven)
            .expect("a read-driven benchmark takes a version-1 corpus");
        assert_eq!(read_driven_v1.version(), CorpusVersion::V1);
        assert_eq!(read_driven_v1.rows().len(), 2);
        assert_eq!(read_driven_v1.scope_label(), "v1-reads-only");

        let read_driven_v2 = load_corpus(&v2_file, CorpusUse::ReadDriven)
            .expect("a read-driven benchmark takes a version-2 corpus too");
        assert_eq!(read_driven_v2.version(), CorpusVersion::V2);

        let relevance = load_corpus(&v2_file, CorpusUse::RelevanceDependent)
            .expect("a version-2 corpus serves a relevance-dependent replay");
        assert_eq!(relevance.scope_label(), "v2-production-relevance");
        assert_eq!(
            anchor_relevance(relevance.rows()).expect("every row recorded its set"),
            vec![address(0x11), address(0x22)]
        );

        let error = load_corpus(&v1_file, CorpusUse::RelevanceDependent)
            .expect_err("a version-1 corpus cannot serve a relevance-dependent replay");
        assert_eq!(
            error.error,
            CorpusError::ReadDrivenOnly {
                version: CorpusVersion::V1
            }
        );
        assert!(
            error.path.contains("fanout_corpus_") && error.line.is_none(),
            "a corpus-level refusal names the file, not a line: {error}"
        );
        assert!(error.to_string().contains("v1"), "{error}");
    }

    /// One file pins one schema. A later row declaring another is refused with the
    /// line that breaks it.
    #[test]
    fn a_file_mixing_versions_is_refused_at_the_line_that_breaks_it() {
        let path = corpus_file("mixed", &format!("{PRODUCTION_ROW}\n{READS_ONLY_ROW}\n"));
        let error = load_corpus(&path, CorpusUse::ReadDriven)
            .expect_err("a mixed corpus is refused even by a read-driven load");
        assert_eq!(
            error.error,
            CorpusError::MixedVersions {
                first: CorpusVersion::V2,
                found: CorpusVersion::V1,
            }
        );
        assert_eq!(error.line, Some(2), "the breaking line is named: {error}");

        let rows = vec![parse(PRODUCTION_ROW), parse(READS_ONLY_ROW)];
        assert_eq!(
            Corpus::from_rows(rows, CorpusUse::ReadDriven).expect_err("mixed rows are refused"),
            CorpusError::MixedVersions {
                first: CorpusVersion::V2,
                found: CorpusVersion::V1,
            }
        );
    }

    /// A file with no row, or no file at all, is a corpus failure — never an empty
    /// replay that would report zero of everything.
    #[test]
    fn a_missing_corpus_or_a_corpus_with_no_row_is_a_failure() {
        let empty = corpus_file("empty", "no trace rows here\n");
        let error = load_corpus(&empty, CorpusUse::ReadDriven).expect_err("no row is a failure");
        assert_eq!(error.error, CorpusError::NoRows);
        let missing = load_corpus("/tmp/no_such_fanout_corpus.log", CorpusUse::ReadDriven)
            .expect_err("a missing file is a failure");
        assert!(matches!(missing.error, CorpusError::Unreadable { .. }));
        assert!(
            load_corpus(&empty, CorpusUse::RelevanceDependent).is_err(),
            "an empty corpus cannot serve either semantics"
        );
    }

    /// An anchor's production set is the deduped union of its rows' sets, in row
    /// order, and one version-1 row refuses the whole union rather than reporting
    /// a partial set as complete.
    #[test]
    fn an_anchor_unions_its_production_sets_and_refuses_one_version_one_row() {
        let first = parse(PRODUCTION_ROW);
        let mut second = first.clone();
        second.relevance = RowRelevance::Production(vec![address(0x22), address(0x44)]);
        assert_eq!(
            anchor_relevance(&[first.clone(), second]).expect("both rows carry their set"),
            vec![address(0x11), address(0x22), address(0x44)],
            "the anchor's set is the deduped union, in row order"
        );

        let reads_only = parse(READS_ONLY_ROW);
        assert_eq!(
            anchor_relevance(&[first, reads_only]),
            Err(CorpusError::RelevanceNotRecorded { block: 9 }),
            "one version-1 row means the anchor's production set is incomplete"
        );
    }

    /// A parsed row object is gated by the same rules as a loaded corpus, so a
    /// reader that parses its own fields cannot drift: read-driven use takes
    /// either version, relevance-dependent use refuses v1, and the version key
    /// and relevance shape are still enforced.
    #[test]
    fn a_parsed_row_object_is_gated_by_the_same_rules_as_a_loaded_corpus() {
        let object = |version: &str, relevance: &str| -> Value {
            serde_json::from_str(&format!(
                r#"{{"v":{version},"block":1,"sim_ns":1,"reads":[]{relevance}}}"#
            ))
            .expect("the fixture object parses")
        };
        let v2 = object("2", r#","relevance":[]"#);
        let v1 = object("1", "");
        let v1_with_relevance = object("1", r#","relevance":[]"#);
        let v2_without_relevance = object("2", "");
        let unversioned = serde_json::from_str::<Value>(r#"{"block":1,"sim_ns":1,"reads":[]}"#)
            .expect("the fixture object parses");
        let unknown = object("3", "");

        assert_eq!(
            check_row(&v2, CorpusUse::ReadDriven).expect("v2 serves a read-driven replay"),
            CorpusVersion::V2
        );
        assert_eq!(
            check_row(&v1, CorpusUse::ReadDriven).expect("v1 serves a read-driven replay"),
            CorpusVersion::V1
        );
        assert_eq!(
            check_row(&v1, CorpusUse::RelevanceDependent),
            Err(CorpusError::ReadDrivenOnly {
                version: CorpusVersion::V1
            }),
            "a v1 object cannot serve a relevance-dependent reader"
        );
        assert_eq!(
            check_row(&unversioned, CorpusUse::ReadDriven),
            Err(CorpusError::MissingVersion)
        );
        assert_eq!(
            check_row(&unknown, CorpusUse::ReadDriven),
            Err(CorpusError::UnknownVersion {
                found: "3".to_string()
            })
        );
        assert!(matches!(
            check_row(&v1_with_relevance, CorpusUse::ReadDriven),
            Err(CorpusError::VersionShape {
                version: CorpusVersion::V1,
                ..
            })
        ));
        assert!(matches!(
            check_row(&v2_without_relevance, CorpusUse::ReadDriven),
            Err(CorpusError::VersionShape {
                version: CorpusVersion::V2,
                ..
            })
        ));
    }

    /// The one-schema-per-corpus rule has a single owner, and a private reader's
    /// version stream pins through it: a mixed stream is the same typed refusal a
    /// loaded corpus gives.
    #[test]
    fn a_version_stream_pins_through_the_one_owner_and_refuses_a_mix() {
        assert_eq!(
            pin([CorpusVersion::V2, CorpusVersion::V2]).expect("one version pins"),
            CorpusVersion::V2
        );
        assert_eq!(
            corpus_version(&[parse(PRODUCTION_ROW), parse(PRODUCTION_ROW)]).expect("pins"),
            CorpusVersion::V2
        );
        assert_eq!(
            pin([CorpusVersion::V1, CorpusVersion::V2]),
            Err(CorpusError::MixedVersions {
                first: CorpusVersion::V1,
                found: CorpusVersion::V2,
            })
        );
        assert_eq!(
            pin(std::iter::empty::<CorpusVersion>()),
            Err(CorpusError::NoRows)
        );
    }

    /// The labels a report prints name the scope its numbers were measured under,
    /// and the two versions cannot be confused for one another.
    #[test]
    fn the_version_labels_name_the_scope_a_report_prints() {
        assert_eq!(CorpusVersion::V1.label(), "v1-reads-only");
        assert_eq!(CorpusVersion::V2.label(), "v2-production-relevance");
        assert_ne!(CorpusVersion::V1.label(), CorpusVersion::V2.label());
        assert_eq!(CorpusUse::ReadDriven.label(), "read-driven");
        assert_eq!(CorpusUse::RelevanceDependent.label(), "relevance-dependent");
        assert_eq!(CorpusVersion::V1.to_string(), "v1");
        assert_eq!(CorpusVersion::V2.to_string(), "v2");
    }
}
