//! The CRAWL adapter over Registration admission.
//!
//! The stage order itself — memo → construction → eligibility → verify →
//! registration — is [`RegistrationAdmission`]'s (one implementation, shared
//! with the direct-registration adapter). What this module adds is the shape the
//! crawl's loop submits: a [`CrawlUnit`] (the discovery edge's candidate, in
//! submission order) and the [`CrawlUnitHost`] a seat borrows for one unit.
//!
//! # What the type system already removed
//!
//! The Python unit carries "wire drift" arms that exist only because `PoolKind`,
//! the gate verdict and the unit kind cross a language boundary: an
//! unrecognized hop family, an unrecognized eligibility verdict, and a
//! caught-`Exception` net. Here the hop family is [`DiscoveredHop`]'s variant
//! set (closed, exhaustive), the gate verdict is an enum match, and every
//! fallible step is typed — so there is no arm left to guess a branch for.

use degenbot_db::snapshot::TickMapDb;

use crate::arb_engine::crawl::admission::{CrawlAbort, DiscoveredHop, RegistrationAdmission};
use crate::arb_engine::crawl::receipt::CrawlReceipt;

/// One candidate path: the crawl's unit of work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrawlUnit {
    /// FIFO submission order — the receipt stream's spine.
    pub seq: u64,
    /// The hops, in hop order.
    pub hops: Vec<DiscoveredHop>,
    /// Operator-pinned per-hop directions (`Some` = the operator's answer, which
    /// the gate must agree with rather than resolve).
    pub directions: Option<Vec<bool>>,
}

/// Everything one unit runs against: Registration admission, plus the per-use
/// tick-map handle the crawl's snapshot arm hands out.
///
/// The admission carries the construction state, the construction I/O, the
/// engine stage seam, the verify home, the ledger, the build flights, and the
/// policy values — so this host names no session object, and the crawl runs in a
/// process that never pumps.
pub struct CrawlUnitHost<'a> {
    /// The one admission implementation.
    pub admission: &'a RegistrationAdmission,
    /// The snapshot DB handle a head construction seeds its tick map from.
    pub db: Option<&'a dyn TickMapDb>,
}

/// Run one candidate path through admission, as the crawl's unit.
///
/// `Ok` carries the unit's receipt — exactly one terminal outcome, with the
/// stage split front-truncated at the stage that answered. `Err` is the loud
/// contract: the crawl stops.
///
/// # Errors
///
/// [`CrawlAbort`] — see its variants.
pub async fn run_crawl_unit(
    host: &CrawlUnitHost<'_>,
    unit: &CrawlUnit,
) -> Result<CrawlReceipt, CrawlAbort> {
    host.admission
        .admit(unit.seq, &unit.hops, unit.directions.as_deref(), host.db)
        .await
}

#[cfg(test)]
mod tests;
