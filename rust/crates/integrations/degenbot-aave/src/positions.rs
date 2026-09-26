//! The Aave side of the position seam: a [`PositionObserver`] that reads one
//! account's position in one market out of the state this crate's updater
//! maintains.
//!
//! # The source is the updater's own state
//!
//! The rows are the ones [`crate::updater`] applies transactionally at chunk
//! boundaries, so a reading is only as fresh as the market's `last_update_block`
//! — which is exactly what the reading reports, and exactly what the caller's
//! [`Freshness`] requirement is checked against. A market the updater has never
//! advanced has no observation at all, so the read is refused as stale rather
//! than reported with a block it does not have.
//!
//! # Every fault is a refusal, never a value
//!
//! An unserved market, an account with no position, a contended read, and a row
//! that cannot be decoded are four different refusals: the first two are facts
//! about the request, the third is worth retrying, and the fourth reproduces
//! unchanged. None of them produces a reading, because a caller about to act on
//! a position cannot tell a fabricated one from a real one.

use std::sync::Arc;

use alloy::primitives::U256;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_core::session_positions::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
};
use degenbot_db::{DbError, DegenbotDb};

use crate::analysis;

/// The Aave V3 position reader.
///
/// Holds the read substrate and nothing else: no cache, no default, and no
/// market table of its own — the market a position names is resolved from the
/// database on every read, so a market the updater activates mid-session is
/// served without this observer learning anything about it.
#[derive(Clone)]
pub struct AavePositionObserver {
    db: Arc<DegenbotDb>,
}

impl AavePositionObserver {
    /// The observer over `db`'s Aave V3 state.
    #[must_use]
    pub const fn new(db: Arc<DegenbotDb>) -> Self {
        Self { db }
    }
}

impl PositionObserver for AavePositionObserver {
    fn read_position(
        &self,
        identity: &PositionIdentity,
        freshness: &Freshness,
    ) -> Result<PositionReading, PositionRefusal> {
        // The chain is part of the key and the row's own chain is part of the
        // lookup, so a position cannot be answered for by another chain's rows.
        let Some(chain_id) = i64::try_from(identity.chain_id()).ok() else {
            return Err(PositionRefusal::MarketNotServed {
                identity: *identity,
            });
        };
        let market = self
            .db
            .fetch_aave_market_by_pool_address(
                chain_id,
                &address_to_checksum_string(&identity.market()),
            )
            .map_err(|error| classify(&error, *identity))?
            .ok_or(PositionRefusal::MarketNotServed {
                identity: *identity,
            })?;

        let observed_block = market
            .last_update_block
            .and_then(|block| u64::try_from(block).ok())
            .ok_or(PositionRefusal::StaleObservation {
                identity: *identity,
                observed_block: None,
                required: *freshness,
            })?;
        // Refuse BEFORE the per-user reads: a caller that cannot use this
        // observation should not cost three more queries to find out.
        if !freshness.accepts(Some(observed_block)) {
            return Err(PositionRefusal::StaleObservation {
                identity: *identity,
                observed_block: Some(observed_block),
                required: *freshness,
            });
        }

        let user = self
            .db
            .fetch_aave_user_record_by_address(
                market.id,
                &address_to_checksum_string(&identity.account()),
            )
            .map_err(|error| classify(&error, *identity))?
            .ok_or(PositionRefusal::UnknownPosition {
                identity: *identity,
            })?;
        let collateral = self
            .db
            .fetch_aave_collateral_positions(user.id)
            .map_err(|error| classify(&error, *identity))?;
        let debt = self
            .db
            .fetch_aave_debt_positions(user.id)
            .map_err(|error| classify(&error, *identity))?;
        let collateral_config = self
            .db
            .fetch_aave_collateral_config_map(user.id)
            .map_err(|error| classify(&error, *identity))?;

        // The same pure analysis the batch path runs, so a per-position reading
        // and the market-wide report cannot disagree about one account.
        let summary =
            analysis::analyze_user_position(&user, &collateral, &debt, &collateral_config, None)
                .map_err(|error| PositionRefusal::UnreadablePosition {
                    identity: *identity,
                    reason: error.to_string(),
                })?;

        Ok(PositionReading::new(
            *identity,
            observed_block,
            health_factor(summary.health_factor, *identity)?,
        ))
    }
}

/// Whether a `SQLite` failure is one a retry may clear.
///
/// A contended, interrupted, or I/O-bound read is transient; a schema or
/// statement fault reproduces unchanged. Reporting the second as retryable
/// would send a caller into a retry loop that can only end the same way.
fn is_retryable_sqlite(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return false;
    };
    // A busy handler reports an EXTENDED code; the primary code is the low byte.
    let primary = failure.extended_code & 0xFF;
    matches!(primary, code if code == rusqlite::ffi::SQLITE_BUSY
        || code == rusqlite::ffi::SQLITE_LOCKED
        || code == rusqlite::ffi::SQLITE_IOERR
        || code == rusqlite::ffi::SQLITE_INTERRUPT
        || code == rusqlite::ffi::SQLITE_CANTOPEN)
}

/// The typed refusal a substrate failure turns into.
fn classify(error: &DbError, identity: PositionIdentity) -> PositionRefusal {
    let reason = error.to_string();
    match &error {
        DbError::Sqlite(sqlite) if is_retryable_sqlite(sqlite) => {
            PositionRefusal::TransientRead { identity, reason }
        }
        DbError::Io(_) => PositionRefusal::TransientRead { identity, reason },
        _ => PositionRefusal::UnreadablePosition { identity, reason },
    }
}

/// The fixed-point risk ratio the analysis reports as a float.
///
/// [`HealthFactor::NoDebt`] is its own variant rather than a zero: the analysis
/// reports no health factor for an account with no debt, and a zero ratio means
/// LIQUIDATABLE, so collapsing the two would fabricate a liquidation out of an
/// empty position. A non-finite or negative ratio is refused rather than
/// rounded, because there is no honest fixed-point value to report for it.
fn health_factor(
    health_factor: Option<f64>,
    identity: PositionIdentity,
) -> Result<HealthFactor, PositionRefusal> {
    let Some(health_factor) = health_factor else {
        return Ok(HealthFactor::NoDebt);
    };
    let scaled = health_factor * 1e18;
    if !scaled.is_finite() || scaled < 0.0 {
        return Err(PositionRefusal::UnreadablePosition {
            identity,
            reason: format!("non-finite health factor {health_factor}"),
        });
    }
    U256::from_str_radix(&format!("{scaled:.0}"), 10)
        .map(HealthFactor::Ratio)
        .map_err(|error| PositionRefusal::UnreadablePosition {
            identity,
            reason: format!("health factor {health_factor} is not a representable ratio: {error}"),
        })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used)]

    use super::is_retryable_sqlite;
    use alloy::primitives::{Address, U256};
    use degenbot_core::session_positions::{HealthFactor, PositionIdentity, PositionRefusal};
    use degenbot_db::DbError;

    fn identity() -> PositionIdentity {
        PositionIdentity::new(1, Address::ZERO, Address::ZERO)
    }

    fn sqlite(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    /// A contended read is the fault a retry clears, and it is reported with its
    /// EXTENDED code (a busy handler that timed out on a snapshot reads
    /// `SQLITE_BUSY_SNAPSHOT`), so the classification takes the primary code out
    /// of the low byte rather than comparing the whole number. A statement or
    /// constraint fault reproduces unchanged and must not be reported as
    /// retryable.
    #[test]
    fn a_contended_sqlite_fault_is_retryable_and_a_statement_fault_is_not() {
        assert!(is_retryable_sqlite(&sqlite(rusqlite::ffi::SQLITE_BUSY)));
        assert!(is_retryable_sqlite(&sqlite(
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
        )));
        assert!(is_retryable_sqlite(&sqlite(rusqlite::ffi::SQLITE_IOERR)));
        assert!(!is_retryable_sqlite(&sqlite(rusqlite::ffi::SQLITE_ERROR)));
        assert!(!is_retryable_sqlite(&sqlite(
            rusqlite::ffi::SQLITE_CONSTRAINT
        )));
        assert!(!is_retryable_sqlite(&rusqlite::Error::QueryReturnedNoRows));
    }

    /// The refusal a substrate fault maps into, and the one property a caller
    /// acts on: which of them a retry can fix.
    #[test]
    fn a_substrate_fault_maps_to_a_typed_refusal() {
        let identity = identity();
        let transient = super::classify(
            &DbError::Sqlite(sqlite(rusqlite::ffi::SQLITE_BUSY)),
            identity,
        );
        assert!(transient.is_retryable());
        assert!(matches!(transient, PositionRefusal::TransientRead { .. }));

        let unreadable = super::classify(
            &DbError::Sqlite(sqlite(rusqlite::ffi::SQLITE_ERROR)),
            identity,
        );
        assert!(!unreadable.is_retryable());
        assert!(matches!(
            unreadable,
            PositionRefusal::UnreadablePosition { .. }
        ));
    }

    /// A risk ratio the analysis could not state as a real number is refused, and
    /// an account with no debt is `NoDebt` rather than a zero ratio that would
    /// read as liquidatable.
    #[test]
    fn a_health_factor_is_scaled_or_refused_and_never_invented() {
        let identity = identity();
        assert_eq!(
            super::health_factor(None, identity).expect("no debt is a position"),
            HealthFactor::NoDebt
        );
        assert_eq!(
            super::health_factor(Some(1.5), identity).expect("a finite ratio scales"),
            HealthFactor::Ratio(U256::from(1_500_000_000_000_000_000u64))
        );
        for unrepresentable in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(
                matches!(
                    super::health_factor(Some(unrepresentable), identity),
                    Err(PositionRefusal::UnreadablePosition { .. })
                ),
                "a non-finite or negative health factor has no honest fixed-point value"
            );
        }
    }
}
