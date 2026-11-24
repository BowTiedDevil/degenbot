//! The committed replay-surface authority: the one reviewed home of the
//! per-capture expectations the replay suites and the replay bench assert
//! against (ADR-068 D6).
//!
//! A golden capture pins one recorded span's RPC answers
//! (`tests/fixtures/cassettes/`) and its SQL product
//! (`tests/fixtures/sql_goldens/`). This authority pins the COUNTERS those
//! artifacts must produce when the REAL chunk loop replays them — RPC round
//! trips, response bytes, SQL statements, and the applied deltas/events — in
//! one committed, machine-readable file
//! (`tests/fixtures/replay_surfaces/manifest.json`). Replay suites and
//! `updater_replay_bench` read the same entry, so a bench row and a replay
//! assertion cannot drift into two independently maintained literals.
//!
//! The values are never derived from the run being measured: the manifest is
//! committed, reviewed data and every assertion compares an observed run
//! against it. Refreshing an entry (a legitimate re-record, or a chunk loop
//! whose RPC/SQL surface really changed) is a reviewed manifest diff — the
//! schema, the per-field meaning, and the regeneration/review process are
//! documented in `docs/updater-rpc-sql-survey.md`, "The replay-surface
//! authority".
//!
//! Parse-time validation keeps the authority self-consistent: the schema tag
//! must match, capture ids are unique, spans are forward, and an entry's
//! `kind` must agree with the applied counts it carries (a pool capture
//! states pools written + liquidity applies; an Aave capture states applied
//! events).

use std::path::Path;

use serde::Deserialize;

/// The only schema tag this reader accepts.
pub const SCHEMA: &str = "degenbot.replay-surface/v1";

/// The authority's committed home, relative to the repository root.
pub const MANIFEST_RELATIVE_PATH: &str = "tests/fixtures/replay_surfaces/manifest.json";

/// The primary pool-update capture (the seed corpus's chunk replay).
pub const POOL_UPDATE_CAPTURE: &str = "pool_update_chunk_26102622-26102626";

/// The primary Aave-update capture (the current-era chunk replay).
pub const AAVE_UPDATE_CAPTURE: &str = "aave_update_chunk_26130440-26130445";

/// The Aave-config capture (the GHO-discount-era chunk replay: the cross-tx
/// fact-dependence corpus).
pub const AAVE_CONFIG_CAPTURE: &str = "aave_config_chunk_19091050-19091059";

/// The parsed replay-surface authority.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaySurface {
    /// The schema tag; must equal [`SCHEMA`].
    pub schema: String,
    /// One entry per governed capture, in the manifest's order.
    pub captures: Vec<ReplayCapture>,
}

/// Which chunk loop an entry governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    /// `run_pool_update` / `run_pool_update_on_db`.
    PoolUpdate,
    /// `run_aave_update` / `run_aave_update_on_db`.
    AaveUpdate,
}

/// One capture's reviewed expectations.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayCapture {
    /// The capture id: the cassette's artifact name without its `.json.zst`
    /// suffix (and the replay suite's name for the workload).
    pub id: String,
    /// The chunk loop this entry governs.
    pub kind: CaptureKind,
    /// The committed cassette package, relative to the repository root.
    pub cassette: String,
    /// The recorded span the cassette was taken over.
    pub span: CaptureSpan,
    /// The RPC surface the replayed run must serve.
    pub rpc: RpcSurface,
    /// The SQL surface the replayed chunk apply must run.
    pub sql: SqlSurface,
    /// The deltas/events the replayed chunk apply must report; read it
    /// through [`Self::pool_applied`] / [`Self::aave_applied`], which enforce
    /// the kind's shape.
    applied: AppliedSurface,
}

/// The recorded span (the cassette's provenance span).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSpan {
    /// The span's first block.
    pub from_block: u64,
    /// The span's last block.
    pub to_block: u64,
}

/// The RPC counters a run over the cassette must serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcSurface {
    /// Ledger entries served (the run's round-trip count).
    pub round_trips: u64,
    /// The served answers' serialized payload bytes.
    pub response_bytes: u64,
}

/// The SQL counter a chunk apply must run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlSurface {
    /// Statement-ledger records per chunk apply.
    pub statements: usize,
}

/// The applied deltas/events a chunk apply must report. The `kind` decides
/// which family is present; the other family's fields must be absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedSurface {
    /// Pool rows the chunk wrote (`pool_update` only).
    #[serde(default)]
    pub pools_written: Option<usize>,
    /// In-scope liquidity applies the chunk committed (`pool_update` only).
    #[serde(default)]
    pub liquidity_applies: Option<usize>,
    /// Configuration effects the chunk applied (`aave_update` only).
    #[serde(default)]
    pub events_applied: Option<usize>,
}

/// A pool capture's applied expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolApplied {
    /// Pool rows the chunk wrote.
    pub pools_written: usize,
    /// In-scope liquidity applies the chunk committed.
    pub liquidity_applies: usize,
}

/// An Aave capture's applied expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveApplied {
    /// Configuration effects the chunk applied.
    pub events_applied: usize,
}

impl ReplaySurface {
    /// Parse and validate the authority from committed manifest bytes.
    ///
    /// # Errors
    ///
    /// [`ReplaySurfaceError::Parse`] when the bytes are not this schema's
    /// JSON, and the identity/shape variants when the parsed data contradicts
    /// itself.
    pub fn parse(bytes: &[u8]) -> Result<Self, ReplaySurfaceError> {
        let surface: Self = serde_json::from_slice(bytes).map_err(ReplaySurfaceError::Parse)?;
        surface.validate()?;
        Ok(surface)
    }

    /// Read, parse, and validate the committed authority under a repository
    /// root (the one call every replay consumer makes).
    ///
    /// # Errors
    ///
    /// Every error [`Self::load`] reports.
    pub fn load_committed(repo_root: &Path) -> Result<Self, ReplaySurfaceError> {
        Self::load(&repo_root.join(MANIFEST_RELATIVE_PATH))
    }

    /// Read, parse, and validate a committed manifest file.
    ///
    /// # Errors
    ///
    /// [`ReplaySurfaceError::Io`] when the file cannot be read, plus every
    /// error [`Self::parse`] reports.
    pub fn load(path: &Path) -> Result<Self, ReplaySurfaceError> {
        let bytes = std::fs::read(path).map_err(|source| ReplaySurfaceError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&bytes)
    }

    /// One capture's reviewed expectations.
    ///
    /// # Errors
    ///
    /// [`ReplaySurfaceError::UnknownCapture`] when the authority does not
    /// govern that id.
    pub fn capture(&self, id: &str) -> Result<&ReplayCapture, ReplaySurfaceError> {
        self.captures
            .iter()
            .find(|capture| capture.id == id)
            .ok_or_else(|| ReplaySurfaceError::UnknownCapture { id: id.to_string() })
    }

    fn validate(&self) -> Result<(), ReplaySurfaceError> {
        if self.schema != SCHEMA {
            return Err(ReplaySurfaceError::UnsupportedSchema {
                found: self.schema.clone(),
            });
        }
        for (index, capture) in self.captures.iter().enumerate() {
            if self.captures[index + 1..]
                .iter()
                .any(|other| other.id == capture.id)
            {
                return Err(ReplaySurfaceError::DuplicateCapture {
                    id: capture.id.clone(),
                });
            }
            capture.validate()?;
        }
        Ok(())
    }
}

impl ReplayCapture {
    /// The pool-update applied expectation, for a `pool_update` entry.
    #[must_use]
    pub fn pool_applied(&self) -> Option<PoolApplied> {
        match (
            self.kind,
            self.applied.pools_written,
            self.applied.liquidity_applies,
        ) {
            (CaptureKind::PoolUpdate, Some(pools_written), Some(liquidity_applies)) => {
                Some(PoolApplied {
                    pools_written,
                    liquidity_applies,
                })
            }
            _ => None,
        }
    }

    /// The Aave-update applied expectation, for an `aave_update` entry.
    #[must_use]
    pub fn aave_applied(&self) -> Option<AaveApplied> {
        match (self.kind, self.applied.events_applied) {
            (CaptureKind::AaveUpdate, Some(events_applied)) => Some(AaveApplied { events_applied }),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), ReplaySurfaceError> {
        if self.span.from_block > self.span.to_block {
            return Err(ReplaySurfaceError::BackwardsSpan {
                id: self.id.clone(),
            });
        }
        let shape_matches = match self.kind {
            CaptureKind::PoolUpdate => {
                self.pool_applied().is_some() && self.applied.events_applied.is_none()
            }
            CaptureKind::AaveUpdate => {
                self.aave_applied().is_some()
                    && self.applied.pools_written.is_none()
                    && self.applied.liquidity_applies.is_none()
            }
        };
        if !shape_matches {
            return Err(ReplaySurfaceError::AppliedShape {
                id: self.id.clone(),
            });
        }
        Ok(())
    }
}

/// The replay-surface authority's failure modes: every one is a loud refusal
/// of a manifest the committed corpus cannot back.
#[derive(Debug, thiserror::Error)]
pub enum ReplaySurfaceError {
    /// The manifest file could not be read.
    #[error("the replay-surface manifest {path} could not be read: {source}")]
    Io {
        /// The path the loader was handed.
        path: String,
        /// The underlying read error.
        source: std::io::Error,
    },
    /// The bytes are not this schema's JSON.
    #[error("the replay-surface manifest is not valid JSON for this schema: {0}")]
    Parse(serde_json::Error),
    /// A schema tag this reader does not implement.
    #[error("the replay-surface manifest declares schema {found}; this build reads {SCHEMA}")]
    UnsupportedSchema {
        /// The tag the manifest carried.
        found: String,
    },
    /// Two entries claim the same capture id.
    #[error("the replay-surface manifest lists capture {id} more than once")]
    DuplicateCapture {
        /// The repeated id.
        id: String,
    },
    /// A query for an id the manifest does not govern.
    #[error("the replay-surface manifest governs no capture {id}")]
    UnknownCapture {
        /// The id that was asked for.
        id: String,
    },
    /// An entry whose span ends before it starts.
    #[error("the replay-surface entry {id} records a backwards span")]
    BackwardsSpan {
        /// The offending capture.
        id: String,
    },
    /// An entry whose applied counts disagree with its kind.
    #[error(
        "the replay-surface entry {id} must state exactly the applied counts its kind carries (a pool capture: pools_written + liquidity_applies; an aave capture: events_applied)"
    )]
    AppliedShape {
        /// The offending capture.
        id: String,
    },
}

#[cfg(test)]
#[expect(clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cassette::{verify_artifact, Cassette};

    /// The workspace nests every crate manifest four levels under the
    /// repository root.
    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }

    fn committed_surface() -> ReplaySurface {
        ReplaySurface::load_committed(&repo_root())
            .unwrap_or_else(|e| panic!("the committed replay-surface authority must load: {e}"))
    }

    /// A one-entry manifest over an in-memory pool capture (never the
    /// committed corpus: the probes mutate a copy).
    fn manifest(entries: &[String]) -> String {
        format!(
            r#"{{"schema": "{SCHEMA}", "captures": [{}]}}"#,
            entries.join(",")
        )
    }

    fn pool_entry(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "kind": "pool_update",
                "cassette": "tests/fixtures/cassettes/{id}.json.zst",
                "span": {{"from_block": 1, "to_block": 2}},
                "rpc": {{"round_trips": 2, "response_bytes": 10}},
                "sql": {{"statements": 3}},
                "applied": {{"pools_written": 1, "liquidity_applies": 0}}
            }}"#
        )
    }

    /// The committed authority governs exactly the three primary captures and
    /// agrees with the committed cassettes it points at: the file exists, its
    /// bytes pass the cassette drift gate, the manifest's span is the
    /// cassette's provenance span, and the id is the cassette artifact's stem.
    #[test]
    fn the_committed_authority_matches_the_committed_cassettes() {
        let surface = committed_surface();
        assert_eq!(surface.schema, SCHEMA);

        for id in [
            POOL_UPDATE_CAPTURE,
            AAVE_UPDATE_CAPTURE,
            AAVE_CONFIG_CAPTURE,
        ] {
            let capture = surface
                .capture(id)
                .expect("the authority governs the primary captures");
            assert!(
                capture.cassette.ends_with(&format!("/{id}.json.zst")),
                "capture {id} must name its own cassette package, not {}",
                capture.cassette
            );
            let path = repo_root().join(&capture.cassette);
            verify_artifact(&path).expect("the committed cassette must pass its drift gate");
            let cassette = Cassette::from_artifact(&path).unwrap_or_else(|e| {
                panic!("capture {id}: cassette {} must decode: {e}", path.display())
            });
            assert_eq!(
                capture.span,
                CaptureSpan {
                    from_block: cassette.provenance.span.from_block,
                    to_block: cassette.provenance.span.to_block,
                },
                "capture {id}: the authority's span must be the cassette's provenance span"
            );
        }

        let pool = surface
            .capture(POOL_UPDATE_CAPTURE)
            .expect("the pool capture");
        assert!(
            pool.pool_applied().is_some(),
            "the pool capture carries pool counts"
        );
        assert!(
            pool.aave_applied().is_none(),
            "the pool capture carries no applied-event count"
        );
        for id in [AAVE_UPDATE_CAPTURE, AAVE_CONFIG_CAPTURE] {
            let aave = surface.capture(id).expect("the aave capture");
            assert_eq!(aave.kind, CaptureKind::AaveUpdate);
            assert!(
                aave.aave_applied().is_some(),
                "the aave capture carries applied events"
            );
            assert!(
                aave.pool_applied().is_none(),
                "an aave capture carries no pool counts"
            );
        }
    }

    /// The applied counts are the capture's own kind: a pool capture may not
    /// borrow the Aave event count (or the reverse) to make a run match.
    #[test]
    fn applied_counts_must_match_the_captures_kind() {
        let pool_with_events = pool_entry("a").replace(
            r#""applied": {"pools_written": 1, "liquidity_applies": 0}"#,
            r#""applied": {"events_applied": 20}"#,
        );
        assert!(matches!(
            ReplaySurface::parse(manifest(&[pool_with_events]).as_bytes()),
            Err(ReplaySurfaceError::AppliedShape { .. })
        ));

        let aave_with_pool_counts =
            pool_entry("b").replace(r#""kind": "pool_update""#, r#""kind": "aave_update""#);
        assert!(matches!(
            ReplaySurface::parse(manifest(&[aave_with_pool_counts]).as_bytes()),
            Err(ReplaySurfaceError::AppliedShape { .. })
        ));
    }

    #[test]
    fn a_repeated_capture_id_is_refused() {
        assert!(matches!(
            ReplaySurface::parse(manifest(&[pool_entry("a"), pool_entry("a")]).as_bytes()),
            Err(ReplaySurfaceError::DuplicateCapture { .. })
        ));
    }

    #[test]
    fn a_foreign_schema_tag_or_a_misspelled_field_is_refused() {
        let foreign = manifest(&[pool_entry("a")]).replace(SCHEMA, "degenbot.replay-surface/v2");
        assert!(matches!(
            ReplaySurface::parse(foreign.as_bytes()),
            Err(ReplaySurfaceError::UnsupportedSchema { .. })
        ));

        let misspelled =
            manifest(&[pool_entry("a")]).replace(r#""round_trips": 2"#, r#""round_trips2": 2"#);
        assert!(matches!(
            ReplaySurface::parse(misspelled.as_bytes()),
            Err(ReplaySurfaceError::Parse(_))
        ));
    }

    #[test]
    fn an_ungoverned_capture_id_is_loud() {
        let surface = committed_surface();
        assert!(matches!(
            surface.capture("no_such_capture"),
            Err(ReplaySurfaceError::UnknownCapture { .. })
        ));
    }
}
