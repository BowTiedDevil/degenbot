//! The recorded hook charge (F2) — the search-time fee of a pool whose charge
//! its HOOK carries.
//!
//! A V4 pool keyed at the `DYNAMIC_FEE_FLAG` sentinel, or keyed at zero with an
//! amount-modifying hook, charges whatever its hook charges: the key's `fee`
//! field is not the pool's price. The white-box concentrated-liquidity walk
//! needs a fee number, so the search tier uses the last measured charge per
//! (pool, direction) — `1 − out(as traded) / out(same walk, nothing charged)` at
//! a solve anchor, taken by the black-box twin the finalist re-quote already
//! runs (the derivation the census validated against static-fee controls;
//! `docs/findings/2026-11-04-dynamic-fee-census.md`).
//!
//! Three states, and the ABSENT one is load-bearing:
//!
//! - [`HookFeeRecord::Measured`] — the walk projects with the recorded pips (the
//!   V4 pips scale, `1_000_000` = 100%), composed with the pool's protocol fee
//!   exactly as a static key fee is;
//! - [`HookFeeRecord::Refused`] — the chain refuses that direction outright (44%
//!   of the census's pool-direction walks), so the pool can never be a hop in
//!   it, whatever its fee;
//! - **absent** — never measured, so the pool is NOT SEARCHABLE in that
//!   direction. Searching it at fee 0 is the systematic overestimate the census
//!   measured at 1.0–1.5% for the four most-populous hooks (44.7% of the
//!   population) and 0–42% across the long tail.
//!
//! The charge is a pool CONFIGURATION, not a per-block oracle — the census's
//! five anchors over an hour, one pool per top hook, both directions: 5 of 5
//! series flat to the hundredth of a bip. A record therefore does not rot with
//! time; it is superseded only when a later measurement disagrees
//! ([`HookFeeRecord::agrees`]) or once it is older than
//! [`HOOK_FEE_REFRESH_BLOCKS`] ([`HookFeeRecord::is_due`]), so repeated
//! measurements of the same pool at different notional widths do not churn the
//! owner's state nonce every cycle.
//!
//! # Why a side record, not a `V4PoolState` field
//!
//! The record is a per-pool, per-direction FACT refreshed by MEASUREMENT, not by
//! state drift: a reorg cannot un-measure it, and the pool's journals, snapshot
//! seeds, and reorg restore deliberately never carry it. It lives beside the
//! pool entry (the registration-gate precedent) and its WRITE bumps the pool's
//! `state_nonce`, which is what invalidates the hop-projection memo — a
//! projection built while the direction was unmeasured must not survive the
//! measurement that made it searchable.

use crate::state::v4_state::{has_amount_modifying_hook, V4PoolKey, V4_DYNAMIC_FEE_FLAG};

/// How many blocks a measured charge stands before a fresh measurement
/// supersedes it.
///
/// The census priced five anchors across a 3,612-second hour on one pool per
/// top hook: every reading repeated to the hundredth of a bip (0/5 series
/// moved). A day is far past the point the data says anything moves, and still
/// short enough that a hook reconfiguration reaches the search tier on its own.
pub const HOOK_FEE_REFRESH_BLOCKS: u64 = 7_200;

/// The highest charge the integer walk can price: a 100% charge leaves the walk
/// a zero `gamma`, and a pool that eats the whole input cannot be a hop anyway.
/// Ranking such a pool one pip under 100% is the conservative end of the truth —
/// no candidate clears a bar at that charge.
pub const MAX_RANKABLE_HOOK_FEE_PIPS: u32 = 999_999;

/// The direction slot a reading is keyed by: `0` is `zero_for_one` (currency0
/// in), `1` is `one_for_zero`.
#[must_use]
pub const fn direction_slot(zero_for_one: bool) -> usize {
    if zero_for_one {
        0
    } else {
        1
    }
}

/// One measurement of a (pool, direction)'s charge — what a black-box twin
/// learned, with no anchor attached (the recorder stamps the block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookFeeReading {
    /// The hook's charge in pips of `1_000_000` — `pips` of the walk's input,
    /// as the quote's output against the same walk with the hook neutered.
    /// A rebate reads as `0` (a negative charge is not a fee; searching at 0
    /// understates such a pool, which is the conservative direction).
    Measured {
        /// The charge in pips of 1e6 (1 bp = 100 pips).
        pips: u32,
    },
    /// The chain REFUSES this direction: the quote frame reverted in the hook
    /// or the manager. No output exists, so the direction can never be a hop —
    /// a stronger admission constraint than the charge itself.
    Refused,
}

/// The recorded charge of one (pool, direction), with the anchor it was
/// measured at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookFeeRecord {
    /// The measured charge, in pips of 1e6.
    Measured {
        /// The charge in pips of 1e6 (1 bp = 100 pips).
        pips: u32,
        /// The block the twin measured it at.
        at_block: u64,
    },
    /// The direction the chain refused at `at_block`.
    Refused {
        /// The block the refusal was observed at.
        at_block: u64,
    },
}

impl HookFeeRecord {
    /// Stamp a reading with the anchor it was taken at.
    #[must_use]
    pub const fn from_reading(reading: HookFeeReading, at_block: u64) -> Self {
        match reading {
            HookFeeReading::Measured { pips } => Self::Measured { pips, at_block },
            HookFeeReading::Refused => Self::Refused { at_block },
        }
    }

    /// The reading without its anchor.
    #[must_use]
    pub const fn reading(self) -> HookFeeReading {
        match self {
            Self::Measured { pips, .. } => HookFeeReading::Measured { pips },
            Self::Refused { .. } => HookFeeReading::Refused,
        }
    }

    /// The measured charge in pips, `None` for a refused direction.
    #[must_use]
    pub const fn pips(self) -> Option<u32> {
        match self {
            Self::Measured { pips, .. } => Some(pips),
            Self::Refused { .. } => None,
        }
    }

    /// The charge the white-box walk applies: the measured pips bounded by
    /// [`MAX_RANKABLE_HOOK_FEE_PIPS`], `None` for a refused direction.
    #[must_use]
    pub const fn rankable_pips(self) -> Option<u32> {
        match self.pips() {
            Some(pips) if pips > MAX_RANKABLE_HOOK_FEE_PIPS => Some(MAX_RANKABLE_HOOK_FEE_PIPS),
            other => other,
        }
    }

    /// The block this record was measured at.
    #[must_use]
    pub const fn at_block(self) -> u64 {
        match self {
            Self::Measured { at_block, .. } | Self::Refused { at_block } => at_block,
        }
    }

    /// Whether `reading` is the same fact this record already holds — the write
    /// path's "nothing changed, do not touch the pool's nonce" test.
    #[must_use]
    pub const fn agrees(self, reading: HookFeeReading) -> bool {
        match (self, reading) {
            (Self::Measured { pips, .. }, HookFeeReading::Measured { pips: new }) => pips == new,
            (Self::Refused { .. }, HookFeeReading::Refused) => true,
            _ => false,
        }
    }

    /// Whether a measurement at `anchor_block` is past the refresh horizon and
    /// must supersede this record.
    #[must_use]
    pub const fn is_due(self, anchor_block: u64) -> bool {
        anchor_block.saturating_sub(self.at_block()) >= HOOK_FEE_REFRESH_BLOCKS
    }
}

/// Whether a pool's charge is carried by its HOOK rather than its key's `fee`
/// field — the population this record governs.
///
/// Two disjuncts, both measured by the census:
///
/// - the key holds the `DYNAMIC_FEE_FLAG` sentinel: the hook supplies the fee
///   (`slot0.lpFee`), and the sentinel is not a price the walk can use;
/// - the key holds fee `0` AND the hook carries an amount-modifying permission
///   bit: there is no LP fee for the hook to override, so the hook IS the whole
///   charge, carried as a swap delta (every pool in the census's 32,358-pool
///   population has `hooks & 0xCC != 0`).
///
/// A hookless zero-fee pool is NOT in this population: its key fee 0 is the
/// truth, and the white-box walk prices it exactly (330 such pools in the
/// census's database, they keep working unchanged).
#[must_use]
pub fn charge_is_hook_carried(pool_key: &V4PoolKey) -> bool {
    pool_key.fee == V4_DYNAMIC_FEE_FLAG
        || (pool_key.fee == 0 && has_amount_modifying_hook(pool_key.hooks))
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;

    use super::*;

    fn key(fee: u32, hooks: u16) -> V4PoolKey {
        let mut raw = [0u8; 20];
        raw[18] = (hooks >> 8) as u8;
        raw[19] = (hooks & 0xFF) as u8;
        V4PoolKey {
            currency0: Address::from([0x30u8; 20]),
            currency1: Address::from([0x31u8; 20]),
            fee,
            tick_spacing: 10,
            hooks: Address::new(raw),
        }
    }

    /// The census population (hooked, key fee 0) is hook-carried; the sentinel
    /// population is too; the 330 hookless zero-fee pools and every static-fee
    /// hooked pool are not.
    #[test]
    fn the_hook_carried_population_is_the_dynamic_and_hooked_zero_fee_pools() {
        assert!(
            charge_is_hook_carried(&key(0, 0x80)),
            "hooked 0-fee (census)"
        );
        assert!(
            charge_is_hook_carried(&key(V4_DYNAMIC_FEE_FLAG, 0)),
            "sentinel"
        );
        assert!(charge_is_hook_carried(&key(V4_DYNAMIC_FEE_FLAG, 0x80)));
        assert!(
            !charge_is_hook_carried(&key(0, 0)),
            "a hookless zero-fee pool's key fee IS its charge"
        );
        assert!(
            !charge_is_hook_carried(&key(500, 0x80)),
            "a hooked static-fee pool is priced by its key fee"
        );
        assert!(
            !charge_is_hook_carried(&key(3_000, 0)),
            "a hookless static-fee pool is priced by its key fee"
        );
    }

    /// A record answers its own reading, its pips, and its anchor.
    #[test]
    fn a_record_round_trips_its_reading_and_anchor() {
        let measured = HookFeeRecord::from_reading(HookFeeReading::Measured { pips: 14_922 }, 42);
        assert_eq!(
            measured.reading(),
            HookFeeReading::Measured { pips: 14_922 }
        );
        assert_eq!(measured.pips(), Some(14_922));
        assert_eq!(measured.at_block(), 42);

        let refused = HookFeeRecord::from_reading(HookFeeReading::Refused, 42);
        assert_eq!(refused.reading(), HookFeeReading::Refused);
        assert_eq!(refused.pips(), None);
        assert_eq!(refused.at_block(), 42);
    }

    /// `agrees` is the no-churn test: same direction kind and same pips.
    #[test]
    fn agreeing_readings_do_not_supersede_a_record() {
        let measured = HookFeeRecord::Measured {
            pips: 14_922,
            at_block: 1,
        };
        assert!(measured.agrees(HookFeeReading::Measured { pips: 14_922 }));
        assert!(!measured.agrees(HookFeeReading::Measured { pips: 14_923 }));
        assert!(!measured.agrees(HookFeeReading::Refused));

        let refused = HookFeeRecord::Refused { at_block: 1 };
        assert!(refused.agrees(HookFeeReading::Refused));
        assert!(!refused.agrees(HookFeeReading::Measured { pips: 0 }));
    }

    /// The refresh horizon: inside the day the record stands, past it the next
    /// measurement supersedes it; an anchor behind the record never reads due.
    #[test]
    fn the_refresh_horizon_is_a_day_of_blocks() {
        let record = HookFeeRecord::Measured {
            pips: 100,
            at_block: 1_000,
        };
        assert!(!record.is_due(1_000 + HOOK_FEE_REFRESH_BLOCKS - 1));
        assert!(record.is_due(1_000 + HOOK_FEE_REFRESH_BLOCKS));
        assert!(!record.is_due(999), "a stale anchor never expires a record");
    }

    /// The direction slot mapping is the record's key: 0 = zfo.
    #[test]
    fn zero_for_one_is_the_first_slot() {
        assert_eq!(direction_slot(true), 0);
        assert_eq!(direction_slot(false), 1);
    }
}
