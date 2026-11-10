//! The analytical candidate-scaling model for BAL carry-forward: the
//! calibrated logical cost model that extrapolates candidate count and
//! read-set novelty to unique fetches, logical rounds, the storage-wait
//! ceiling, and snapshot working-set size WITHOUT executing a single
//! candidate — every result here is a formula over aggregates, never a replay
//! and never a sleep.
//!
//! # What the model is
//!
//! One calibration basis per recorded corpus (the committed shape and the
//! real-coverage accounting `bal_carry_probe` measured live and the offline
//! replay reproduced), plus shared rates calibrated jointly against BOTH
//! corpora. The pieces:
//!
//! - **carry-hit accounting** ([`carry_accounting`]): the anchored keys
//!   partition by precedence — an anchor's own final writes win first, a key
//!   the snapshot already held is carried, the rest are the dynamic fetch
//!   members — over the key-universe growth the scenario's novelty drives;
//! - **logical rounds**: read-dispatch rounds from two coalescing widths — the
//!   eliminated keys' width (per corpus: each corpus's key-overlap structure
//!   batches differently) and the residual stream's width (shared, calibrated
//!   against both corpora);
//! - **storage-wait estimate**: stream rounds times the price point's storage
//!   price times the stream's partition survival (shared; how much of the
//!   gross round-trip span survives the wall partition into its bucket);
//! - **the ceiling**: the eliminated stream's storage wait over the modeled
//!   cold wall (recorded execution + modeled storage wait + the per-candidate
//!   account/other rates measured on the corpora).
//!
//! The calibration gate ([`calibrate`]) closes this composition against the
//! measured real-coverage results: the carry-hit members and unique fetches
//! reproduce EXACTLY, and the four measured storage-wait ceilings (11.7%/26.9%
//! original corpus, 8.9%/22.0% broader corpus) reproduce within the documented
//! [`CEILING_TOLERANCE_PM`] relative tolerance — or the gate fails.
//!
//! # What the model does NOT claim
//!
//! - **actual p95 wall** — no wall-clock or completion-percentile parity; the
//!   scheduler's occupancy and overlap are only in the calibrated survival
//!   rates;
//! - **lock contention** — unmodeled and unmeasured here; nothing in this
//!   module speaks to scheduler lock behavior;
//! - **allocation cost** — snapshot memory is an entry COUNT (plus one linear
//!   clone-cost extrapolation of the measured 100,000-entry leg), never an
//!   allocator measurement;
//! - **economic outcomes** — no candidate verdicts, no value capture.
//!
//! The W/F axis beyond the measured W=2/F=8 is a documented sensitivity rule
//! (coalescing width scales with the lane-slot count), not a measurement.
//!
//! # Invocation and migration
//!
//! `just bal-candidate-scaling-probe` prints the calibration closure and the
//! scenario matrix; `BAL_SCALING_PLANT_MISMATCH=1` takes the red calibration
//! path (nonzero exit). The pins run under
//! `just test-rust-nextest -p degenbot-simulation`. Migration: none for
//! consumers — example/test surface only, no production `src`, no wire/ABI/FFI
//! contract.

/// The negative-probe knob: set to `1` to plant a calibration mismatch and
/// watch the gate fail loud (the probe exits nonzero).
pub const PLANT_ENV: &str = "BAL_SCALING_PLANT_MISMATCH";

/// The documented relative tolerance on the ceiling closure, in per-mille of
/// the measured ceiling (100pm = 10%).
pub const CEILING_TOLERANCE_PM: u64 = 100;

/// The documented relative tolerance on the logical-round closure, in
/// per-mille of the measured round count.
pub const ROUNDS_TOLERANCE_PM: u64 = 100;

/// The measured session shape every calibration point was recorded at: two
/// lanes, eight slots.
pub const MEASURED_LANES: u64 = 2;

/// The measured session shape's slot count (see [`MEASURED_LANES`]).
pub const MEASURED_SLOTS: u64 = 8;

/// Which calibrated price point a report runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PricePoint {
    /// The optimistic modeled prices: storage 200us, account 400us.
    Optimistic,
    /// The pessimistic modeled prices: storage 700us, account 1200us.
    Pessimistic,
}

impl PricePoint {
    /// Both calibrated points, in the report's order.
    pub const ALL: [Self; 2] = [Self::Optimistic, Self::Pessimistic];

    /// The storage round trip's modeled price, in microseconds.
    #[must_use]
    pub const fn storage_us(self) -> u64 {
        match self {
            Self::Optimistic => 200,
            Self::Pessimistic => 700,
        }
    }

    /// The multi-account round trip's modeled price, in microseconds.
    #[must_use]
    pub const fn account_us(self) -> u64 {
        match self {
            Self::Optimistic => 400,
            Self::Pessimistic => 1_200,
        }
    }

    /// The point's index into per-point arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Optimistic => 0,
            Self::Pessimistic => 1,
        }
    }

    /// The short label the report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Optimistic => "200/400us",
            Self::Pessimistic => "700/1200us",
        }
    }
}

/// One latency point's committed measured facts for one corpus, all from the
/// real-coverage replay (`logs/bal_real_coverage_result.md`,
/// `logs/bal_broader_corpus_result.md`) at W=2/F=8. Microseconds throughout.
#[derive(Clone, Copy, Debug)]
pub struct PointFacts {
    /// The cold leg's measured wall.
    pub cold_wall_us: u64,
    /// The cold leg's recorded execution wall.
    pub exec_us: u64,
    /// The cold leg's partitioned storage wait.
    pub storage_us: u64,
    /// The cold leg's partitioned account wait.
    pub account_us: u64,
    /// The cold leg's scheduler/other residual.
    pub other_us: u64,
    /// The carry leg's measured wall.
    pub carry_wall_us: u64,
    /// The carry leg's partitioned storage wait.
    pub carry_storage_us: u64,
    /// The eliminated storage wait's gross span sum (lane-time freed).
    pub eliminated_gross_us: u64,
    /// The cold leg's drained rounds, when the result table recorded them.
    pub cold_rounds: Option<u64>,
    /// The carry leg's drained rounds.
    pub carry_rounds: u64,
    /// The ceiling the result log published (per-mille of the cold wall).
    pub log_ceiling_pm: u64,
}

impl PointFacts {
    /// The eliminable storage-wait ceiling the measured buckets carry, in
    /// per-mille of the cold wall.
    #[must_use]
    pub const fn eliminated_us(self) -> u64 {
        self.storage_us.saturating_sub(self.carry_storage_us)
    }
}

/// The committed shape and real-coverage accounting of one recorded corpus:
/// the set analyses `bal_carry_probe` enforces, the live carry accounting
/// `bal_wall_ceiling_probe` cross-checks, and the coverage-independent read
/// structure (account members come from the read walks, so the synthetic
/// tables' count stands for both coverages).
#[derive(Clone, Copy, Debug)]
pub struct CorpusFacts {
    /// The corpus label the report prints.
    pub label: &'static str,
    /// The stretch's first canonical block.
    pub first_block: u64,
    /// The stretch's last canonical block.
    pub last_block: u64,
    /// Recorded candidates.
    pub candidates: u64,
    /// Candidate anchors.
    pub anchors: u64,
    /// Recorded read calls, repeats included.
    pub read_calls: u64,
    /// Per-anchor distinct keys, summed (the cold leg's members).
    pub anchored_keys: u64,
    /// Distinct keys over the whole corpus.
    pub global_keys: u64,
    /// Distinct contracts the corpus reads.
    pub contracts: u64,
    /// Keys the carried snapshot answered.
    pub carried: u64,
    /// Keys the anchor's own final writes answered.
    pub final_write: u64,
    /// Keys neither class explains: the dynamic fetch members.
    pub dynamic: u64,
    /// Account batch members dispatched over the run.
    pub account_members: u64,
    /// The optimistic point's facts.
    pub optimistic: PointFacts,
    /// The pessimistic point's facts.
    pub pessimistic: PointFacts,
    /// The carry snapshot's final entry count, when the live probe recorded it.
    pub snapshot_entries: Option<u64>,
    /// Entries the snapshot carried that no recorded key reads (BAL-only), when
    /// recorded.
    pub write_only_entries: Option<u64>,
}

impl CorpusFacts {
    /// The point's committed facts.
    #[must_use]
    pub const fn point(self, point: PricePoint) -> PointFacts {
        match point {
            PricePoint::Optimistic => self.optimistic,
            PricePoint::Pessimistic => self.pessimistic,
        }
    }

    /// The eliminated keys: the carried plus the anchor-final-write hits.
    #[must_use]
    pub const fn eliminated_keys(self) -> u64 {
        self.carried.saturating_add(self.final_write)
    }
}

/// The original 21-block corpus (26375033..=26375053): 444 candidates at 14
/// anchors — `bal_carry_probe`'s set analysis and live accounting, and the
/// real-coverage replay's measured tables.
pub const ORIGINAL: CorpusFacts = CorpusFacts {
    label: "original",
    first_block: 26_375_033,
    last_block: 26_375_053,
    candidates: 444,
    anchors: 14,
    read_calls: 26_099,
    anchored_keys: 5_938,
    global_keys: 2_543,
    contracts: 78,
    carried: 3_261,
    final_write: 136,
    dynamic: 2_541,
    account_members: 260,
    optimistic: PointFacts {
        cold_wall_us: 3_474_810,
        exec_us: 2_653_098,
        storage_us: 602_167,
        account_us: 25_590,
        other_us: 193_953,
        carry_wall_us: 2_909_836,
        carry_storage_us: 194_484,
        eliminated_gross_us: 488_400,
        cold_rounds: Some(4_560),
        carry_rounds: 2_183,
        log_ceiling_pm: 117,
    },
    pessimistic: PointFacts {
        cold_wall_us: 5_727_671,
        exec_us: 2_705_944,
        storage_us: 2_683_980,
        account_us: 78_048,
        other_us: 259_697,
        carry_wall_us: 3_995_686,
        carry_storage_us: 1_140_458,
        eliminated_gross_us: 1_684_900,
        cold_rounds: None,
        carry_rounds: 2_178,
        log_ceiling_pm: 269,
    },
    snapshot_entries: None,
    write_only_entries: None,
};

/// The broader 27-block corpus (26380992..=26381018): 631 candidates at 19
/// anchors — `bal_carry_probe`'s set analysis and live accounting, and the
/// real-coverage replay's measured tables (its snapshot split is the live
/// probe's 4,322 = 2,611 working set + 1,711 BAL-only entries).
pub const BROADER: CorpusFacts = CorpusFacts {
    label: "broader",
    first_block: 26_380_992,
    last_block: 26_381_018,
    candidates: 631,
    anchors: 19,
    read_calls: 33_479,
    anchored_keys: 6_478,
    global_keys: 2_611,
    contracts: 89,
    carried: 3_711,
    final_write: 164,
    dynamic: 2_603,
    account_members: 368,
    optimistic: PointFacts {
        cold_wall_us: 3_478_592,
        exec_us: 2_809_347,
        storage_us: 466_598,
        account_us: 34_095,
        other_us: 168_550,
        carry_wall_us: 3_007_257,
        carry_storage_us: 156_381,
        eliminated_gross_us: 431_600,
        cold_rounds: Some(4_286),
        carry_rounds: 2_229,
        log_ceiling_pm: 89,
    },
    pessimistic: PointFacts {
        cold_wall_us: 5_575_570,
        exec_us: 2_915_059,
        storage_us: 2_314_867,
        account_us: 105_185,
        other_us: 240_457,
        carry_wall_us: 4_099_581,
        carry_storage_us: 1_086_986,
        eliminated_gross_us: 1_503_600,
        cold_rounds: Some(4_278),
        carry_rounds: 2_227,
        log_ceiling_pm: 220,
    },
    snapshot_entries: Some(4_322),
    write_only_entries: Some(1_711),
};

/// Both calibration corpora, in the report's order.
pub const CORPORA: [CorpusFacts; 2] = [ORIGINAL, BROADER];

/// One carry accounting in the live probe's distinct-key shape: the anchor's
/// distinct keys split by where their answer came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CarryAccounting {
    /// The anchored distinct keys.
    pub keys: u64,
    /// Keys the carried snapshot answered.
    pub carried: u64,
    /// Keys the anchor's own final writes answered.
    pub final_write: u64,
    /// Keys neither class explains: the dynamic fetch members.
    pub dynamic: u64,
}

/// The numerator/denominator of one rate, in f64 — zero over zero is zero,
/// never a NaN into the model.
#[must_use]
pub fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    to_f64(numerator) / to_f64(denominator)
}

/// A u64 as f64 without a cast lint at every call site.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "aggregate counts stay far below 2^53"
)]
pub fn to_f64(value: u64) -> f64 {
    value as f64
}

/// A rounded aggregate count: negative, non-finite, and sub-half inputs floor
/// to the count they approximate.
#[must_use]
pub fn nearest(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is finite, positive, and pre-rounded"
    )]
    let rounded = value.round() as u64;
    rounded
}

/// The signed closure error of a modeled aggregate against its measured
/// value, in per-mille of the measured value.
#[must_use]
pub fn relative_error_pm(modeled: u64, measured: u64) -> i64 {
    if measured == 0 {
        return if modeled == 0 { 0 } else { i64::MAX };
    }
    let modeled = i64::try_from(modeled).unwrap_or(i64::MAX);
    let measured = i64::try_from(measured).unwrap_or(i64::MAX);
    (modeled - measured) * 1_000 / measured
}

/// Partition the anchored keys the way the live probe counts them: the
/// anchor's own final writes win first (even when the snapshot also held the
/// key), a key the snapshot already held is carried, and the rest are the
/// dynamic fetch members. `final_write_rate` is the final-write share of the
/// anchored keys; `novel_final_write_rate` is the share of the key universe
/// whose first appearance coincides with its anchor's own final writes (the
/// only novel keys that are NOT dynamic members).
#[must_use]
pub fn carry_accounting(
    anchored_keys: u64,
    universe_keys: u64,
    final_write_rate: f64,
    novel_final_write_rate: f64,
) -> CarryAccounting {
    let keys = anchored_keys;
    let final_write = nearest(to_f64(keys) * final_write_rate);
    let dynamic_cap = keys.saturating_sub(final_write);
    let novel = nearest(to_f64(universe_keys) * (1.0 - novel_final_write_rate));
    let dynamic = novel.min(dynamic_cap);
    CarryAccounting {
        keys,
        carried: keys.saturating_sub(final_write + dynamic),
        final_write,
        dynamic,
    }
}

/// The measured eliminable storage-wait ceiling of one corpus at one point,
/// in per-mille of the cold wall — the number the calibration closes against.
#[must_use]
pub const fn measured_ceiling_pm(facts: &CorpusFacts, point: PricePoint) -> u64 {
    let measured = facts.point(point);
    measured.eliminated_us() * 1_000 / measured.cold_wall_us
}

/// One corpus's derived calibration coefficients, and the shared rates they
/// combine with.
#[derive(Clone, Copy, Debug)]
pub struct Coefficients {
    /// The eliminated stream's coalescing width (keys per round), original
    /// corpus.
    pub eliminated_width_original: f64,
    /// The eliminated stream's coalescing width, broader corpus.
    pub eliminated_width_broader: f64,
    /// The pooled eliminated width the scenario model extrapolates with.
    pub eliminated_width_pooled: f64,
    /// The residual stream's coalescing width (members per round), shared.
    pub residual_width: f64,
    /// The eliminated stream's partition survival per price point, shared.
    pub eliminated_survival: [f64; 2],
    /// The residual stream's partition survival per price point, shared.
    pub residual_survival: [f64; 2],
}

/// The eliminated stream's measured rounds at one point: the gross span sum
/// over the storage round trip's price.
#[must_use]
pub fn eliminated_rounds_measured(facts: &CorpusFacts, point: PricePoint) -> f64 {
    to_f64(facts.point(point).eliminated_gross_us) / to_f64(point.storage_us())
}

/// Derive the model's coefficients from the calibration corpora: the two
/// coalescing widths and the two partition survivals per price point. The
/// widths come from the measured round structure (eliminated gross spans and
/// the carry leg's drained rounds); the survivals are the pooled means of the
/// per-corpus bucket-over-span ratios — calibrated against BOTH corpora, never
/// fitted per data point.
#[must_use]
pub fn coefficients(facts: &[CorpusFacts]) -> Coefficients {
    let mut eliminated_widths = [0.0; 2];
    let mut eliminated_rounds_sum = 0.0;
    let mut eliminated_keys_sum = 0.0;
    let mut residual_members_sum = 0.0;
    let mut residual_rounds_sum = 0.0;
    for (index, corpus) in facts.iter().enumerate() {
        let rounds = f64::midpoint(
            eliminated_rounds_measured(corpus, PricePoint::Optimistic),
            eliminated_rounds_measured(corpus, PricePoint::Pessimistic),
        );
        let eliminated = to_f64(corpus.eliminated_keys());
        eliminated_widths[index] = if rounds > 0.0 {
            eliminated / rounds
        } else {
            0.0
        };
        eliminated_rounds_sum += rounds;
        eliminated_keys_sum += eliminated;
        residual_members_sum += to_f64(corpus.dynamic + corpus.account_members);
        residual_rounds_sum += f64::midpoint(
            to_f64(corpus.optimistic.carry_rounds),
            to_f64(corpus.pessimistic.carry_rounds),
        );
    }
    let residual_width = if residual_rounds_sum > 0.0 {
        residual_members_sum / residual_rounds_sum
    } else {
        0.0
    };
    let mut eliminated_survival = [0.0; 2];
    let mut residual_survival = [0.0; 2];
    for point in PricePoint::ALL {
        let mut eliminated_sum = 0.0;
        let mut residual_sum = 0.0;
        for corpus in facts {
            let measured = corpus.point(point);
            eliminated_sum += rate(measured.eliminated_us(), measured.eliminated_gross_us);
            let residual_storage_rounds = to_f64(corpus.dynamic) / residual_width;
            let residual_gross_us = residual_storage_rounds * to_f64(point.storage_us());
            residual_sum += if residual_gross_us > 0.0 {
                to_f64(measured.carry_storage_us) / residual_gross_us
            } else {
                0.0
            };
        }
        let count = to_f64(u64::try_from(facts.len().max(1)).unwrap_or(u64::MAX));
        eliminated_survival[point.index()] = eliminated_sum / count;
        residual_survival[point.index()] = residual_sum / count;
    }
    Coefficients {
        eliminated_width_original: eliminated_widths[0],
        eliminated_width_broader: eliminated_widths[1],
        eliminated_width_pooled: if eliminated_rounds_sum > 0.0 {
            eliminated_keys_sum / eliminated_rounds_sum
        } else {
            0.0
        },
        residual_width,
        eliminated_survival,
        residual_survival,
    }
}

/// One corpus's modeled round/wait structure at one price point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeledPoint {
    /// The eliminated stream's read-dispatch rounds.
    pub eliminated_rounds: u64,
    /// The carry leg's rounds (the residual stream).
    pub carry_rounds: u64,
    /// The cold leg's rounds (eliminated stream plus residual stream).
    pub cold_rounds: u64,
    /// The eliminated stream's partitioned storage wait.
    pub eliminated_wait_us: u64,
    /// The carry leg's partitioned storage wait.
    pub carry_wait_us: u64,
    /// The composed cold wall (exec + modeled storage wait + account + other).
    pub cold_wall_us: u64,
    /// The modeled ceiling, in per-mille of the modeled cold wall.
    pub ceiling_pm: u64,
}

/// Model one corpus at one point from its facts, the derived coefficients,
/// and the corpus's own eliminated-stream width — the composition the
/// calibration gate closes.
#[must_use]
pub fn model_corpus(
    facts: &CorpusFacts,
    point: PricePoint,
    rates: &Coefficients,
    eliminated_width: f64,
) -> ModeledPoint {
    let measured = facts.point(point);
    model_streams(
        facts.eliminated_keys(),
        facts.dynamic,
        facts.account_members,
        eliminated_width,
        rates.residual_width,
        rates.eliminated_survival[point.index()],
        rates.residual_survival[point.index()],
        point.storage_us(),
        measured.exec_us,
        measured.account_us,
        measured.other_us,
    )
}

/// The stream composition itself: eliminated and residual rounds from the two
/// coalescing widths, storage waits from the streams' partition survivals, and
/// the composed cold wall and ceiling. Shared by the calibration closure and
/// the scenario extrapolation.
#[expect(
    clippy::too_many_arguments,
    reason = "the stream model's inputs are its documented factors"
)]
#[must_use]
pub fn model_streams(
    eliminated_keys: u64,
    dynamic_keys: u64,
    account_members: u64,
    eliminated_width: f64,
    residual_width: f64,
    eliminated_survival: f64,
    residual_survival: f64,
    storage_price_us: u64,
    exec_us: u64,
    account_us: u64,
    other_us: u64,
) -> ModeledPoint {
    let eliminated_rounds = if eliminated_width > 0.0 {
        to_f64(eliminated_keys) / eliminated_width
    } else {
        0.0
    };
    let residual_rounds = if residual_width > 0.0 {
        to_f64(dynamic_keys + account_members) / residual_width
    } else {
        0.0
    };
    let residual_storage_rounds = if residual_width > 0.0 {
        to_f64(dynamic_keys) / residual_width
    } else {
        0.0
    };
    let price = to_f64(storage_price_us);
    let eliminated_wait = eliminated_rounds * price * eliminated_survival;
    let carry_wait = residual_storage_rounds * price * residual_survival;
    let cold_wall =
        to_f64(exec_us) + eliminated_wait + carry_wait + to_f64(account_us) + to_f64(other_us);
    ModeledPoint {
        eliminated_rounds: nearest(eliminated_rounds),
        carry_rounds: nearest(residual_rounds),
        cold_rounds: nearest(eliminated_rounds + residual_rounds),
        eliminated_wait_us: nearest(eliminated_wait),
        carry_wait_us: nearest(carry_wait),
        cold_wall_us: nearest(cold_wall),
        ceiling_pm: if cold_wall > 0.0 {
            nearest(eliminated_wait * 1_000.0 / cold_wall)
        } else {
            0
        },
    }
}

/// One calibration row: the modeled composition beside the measured
/// real-coverage result it must reproduce.
#[derive(Clone, Copy, Debug)]
pub struct ClosureRow {
    /// The corpus the row closes.
    pub corpus: &'static str,
    /// The price point the row closes.
    pub point: PricePoint,
    /// The ceiling the result log published (per-mille).
    pub log_ceiling_pm: u64,
    /// The ceiling the committed buckets compute (per-mille).
    pub measured_ceiling_pm: u64,
    /// The ceiling the model composes (per-mille).
    pub modeled_ceiling_pm: u64,
    /// The modeled ceiling's signed error against the measured one (per-mille
    /// of the measured ceiling).
    pub error_pm: i64,
    /// The eliminated wait the measured buckets carry.
    pub measured_eliminated_us: u64,
    /// The eliminated wait the model composes.
    pub modeled_eliminated_us: u64,
    /// The cold wall the replay measured.
    pub measured_cold_wall_us: u64,
    /// The cold wall the model composes.
    pub modeled_cold_wall_us: u64,
    /// The eliminated rounds the gross spans imply.
    pub measured_eliminated_rounds: u64,
    /// The eliminated rounds the coalescing width models.
    pub modeled_eliminated_rounds: u64,
    /// The modeled eliminated rounds' signed error (per-mille).
    pub eliminated_rounds_error_pm: i64,
    /// The carry leg's measured rounds.
    pub measured_carry_rounds: u64,
    /// The carry leg's modeled rounds.
    pub modeled_carry_rounds: u64,
    /// The modeled carry rounds' signed error (per-mille).
    pub carry_rounds_error_pm: i64,
    /// The cold leg's measured rounds, when the result table recorded them.
    pub measured_cold_rounds: Option<u64>,
    /// The cold leg's modeled rounds.
    pub modeled_cold_rounds: u64,
    /// The modeled cold rounds' signed error (per-mille), when measurable.
    pub cold_rounds_error_pm: Option<i64>,
}

/// One corpus's aggregate reproduction row: the modeled carry accounting
/// beside the committed real accounting it must reproduce exactly.
#[derive(Clone, Copy, Debug)]
pub struct AggregateRow {
    /// The corpus the row reproduces.
    pub corpus: &'static str,
    /// The accounting the model computes.
    pub modeled: CarryAccounting,
    /// The committed real-coverage accounting.
    pub committed: CarryAccounting,
}

/// The calibration report: the exact aggregate reproductions, the ceiling and
/// round closures, and the coefficients the closure ran with.
#[derive(Clone, Debug)]
pub struct CalibrationReport {
    /// One row per corpus.
    pub aggregates: Vec<AggregateRow>,
    /// One row per corpus and price point.
    pub rows: Vec<ClosureRow>,
    /// The coefficients the closure composed.
    pub coefficients: Coefficients,
}

/// The per-corpus calibration rates the aggregate reproduction derives from
/// the corpus's own committed accounting.
#[must_use]
pub fn corpus_rates(facts: &CorpusFacts) -> (f64, f64) {
    (
        rate(facts.final_write, facts.anchored_keys),
        rate(
            facts.global_keys.saturating_sub(facts.dynamic),
            facts.global_keys,
        ),
    )
}

/// Check one corpus's committed accounting for internal consistency — a
/// broken fact set is a calibration failure, never a silently odd model.
fn check_facts(facts: &CorpusFacts) -> Result<(), String> {
    if facts.anchors == 0 || facts.candidates == 0 {
        return Err(format!(
            "{}: a corpus carries anchors and candidates",
            facts.label
        ));
    }
    let partitioned = facts.carried + facts.final_write + facts.dynamic;
    if partitioned != facts.anchored_keys {
        return Err(format!(
            "{}: the real accounting partitions the anchored keys ({} + {} + {} != {})",
            facts.label, facts.carried, facts.final_write, facts.dynamic, facts.anchored_keys
        ));
    }
    if facts.dynamic > facts.global_keys {
        return Err(format!(
            "{}: the dynamic members ({}) cannot exceed the key universe ({})",
            facts.label, facts.dynamic, facts.global_keys
        ));
    }
    if facts.global_keys > facts.anchored_keys {
        return Err(format!(
            "{}: the key universe ({}) cannot exceed the anchored pairs ({})",
            facts.label, facts.global_keys, facts.anchored_keys
        ));
    }
    Ok(())
}

/// Run the calibration gate: reproduce each corpus's unique members and
/// carry-hit structure exactly, and close the four measured storage-wait
/// ceilings and the logical round structure within the documented tolerances.
///
/// # Errors
///
/// An inconsistent committed fact set, a carry-hit structure the model does
/// not reproduce to the count, or a ceiling/round closure outside its
/// tolerance fails the gate by name — the calibration claim is checkable, not
/// asserted.
pub fn calibrate(facts: &[CorpusFacts]) -> Result<CalibrationReport, String> {
    let rates = coefficients(facts);
    if !(rates.eliminated_width_original > 0.0
        && rates.eliminated_width_broader > 0.0
        && rates.residual_width > 0.0)
    {
        return Err("the coalescing widths must be positive".to_string());
    }
    let mut aggregates = Vec::new();
    let mut rows = Vec::new();
    for (index, corpus) in facts.iter().enumerate() {
        let eliminated_width = if index == 0 {
            rates.eliminated_width_original
        } else {
            rates.eliminated_width_broader
        };
        check_facts(corpus)?;
        let (final_write_rate, novel_final_write_rate) = corpus_rates(corpus);
        let modeled = carry_accounting(
            corpus.anchored_keys,
            corpus.global_keys,
            final_write_rate,
            novel_final_write_rate,
        );
        let committed = CarryAccounting {
            keys: corpus.anchored_keys,
            carried: corpus.carried,
            final_write: corpus.final_write,
            dynamic: corpus.dynamic,
        };
        if modeled != committed {
            return Err(format!(
                "{}: the modeled carry accounting ({modeled:?}) does not reproduce the committed real accounting ({committed:?})",
                corpus.label
            ));
        }
        aggregates.push(AggregateRow {
            corpus: corpus.label,
            modeled,
            committed,
        });
        for point in PricePoint::ALL {
            rows.push(close_point(corpus, point, &rates, eliminated_width)?);
        }
    }
    Ok(CalibrationReport {
        aggregates,
        rows,
        coefficients: rates,
    })
}

/// Build and enforce one closure row: the modeled composition beside the
/// measured real-coverage result, with the ceiling and round tolerances the
/// calibration gate closes.
///
/// # Errors
///
/// A ceiling outside [`CEILING_TOLERANCE_PM`] or a round structure outside
/// [`ROUNDS_TOLERANCE_PM`] fails the gate by name.
fn close_point(
    corpus: &CorpusFacts,
    point: PricePoint,
    rates: &Coefficients,
    eliminated_width: f64,
) -> Result<ClosureRow, String> {
    let measured = corpus.point(point);
    let modeled = model_corpus(corpus, point, rates, eliminated_width);
    let measured_ceiling = measured_ceiling_pm(corpus, point);
    let error_pm = relative_error_pm(modeled.ceiling_pm, measured_ceiling);
    let measured_eliminated_rounds = nearest(eliminated_rounds_measured(corpus, point));
    let eliminated_rounds_error_pm =
        relative_error_pm(modeled.eliminated_rounds, measured_eliminated_rounds);
    let carry_rounds_error_pm = relative_error_pm(modeled.carry_rounds, measured.carry_rounds);
    let cold_rounds_error_pm = measured
        .cold_rounds
        .map(|rounds| relative_error_pm(modeled.cold_rounds, rounds));
    let row = ClosureRow {
        corpus: corpus.label,
        point,
        log_ceiling_pm: measured.log_ceiling_pm,
        measured_ceiling_pm: measured_ceiling,
        modeled_ceiling_pm: modeled.ceiling_pm,
        error_pm,
        measured_eliminated_us: measured.eliminated_us(),
        modeled_eliminated_us: modeled.eliminated_wait_us,
        measured_cold_wall_us: measured.cold_wall_us,
        modeled_cold_wall_us: modeled.cold_wall_us,
        measured_eliminated_rounds,
        modeled_eliminated_rounds: modeled.eliminated_rounds,
        eliminated_rounds_error_pm,
        measured_carry_rounds: measured.carry_rounds,
        modeled_carry_rounds: modeled.carry_rounds,
        carry_rounds_error_pm,
        measured_cold_rounds: measured.cold_rounds,
        modeled_cold_rounds: modeled.cold_rounds,
        cold_rounds_error_pm,
    };
    if error_pm.unsigned_abs() > CEILING_TOLERANCE_PM {
        return Err(format!(
            "{} at {}: the modeled ceiling {}pm lands {}pm off the measured {}pm — outside the {CEILING_TOLERANCE_PM}pm tolerance",
            row.corpus, point.label(), row.modeled_ceiling_pm, error_pm, row.measured_ceiling_pm
        ));
    }
    if eliminated_rounds_error_pm.unsigned_abs() > ROUNDS_TOLERANCE_PM
        || carry_rounds_error_pm.unsigned_abs() > ROUNDS_TOLERANCE_PM
        || cold_rounds_error_pm.is_some_and(|error| error.unsigned_abs() > ROUNDS_TOLERANCE_PM)
    {
        return Err(format!(
            "{} at {}: the modeled round structure (eliminated {} vs {}, carry {} vs {}, cold {} vs {:?}) lands outside the {ROUNDS_TOLERANCE_PM}pm tolerance",
            row.corpus,
            point.label(),
            row.modeled_eliminated_rounds,
            row.measured_eliminated_rounds,
            row.modeled_carry_rounds,
            row.measured_carry_rounds,
            row.modeled_cold_rounds,
            row.measured_cold_rounds
        ));
    }
    Ok(row)
}

/// The planted mismatch: the original corpus's committed optimistic
/// execution wall scaled by five quarters — a calibration input the model
/// composes into its cold wall, so the closure must go red.
#[must_use]
pub fn planted_facts() -> [CorpusFacts; 2] {
    let mut facts = CORPORA;
    facts[0].optimistic.exec_us = facts[0].optimistic.exec_us * 5 / 4;
    facts
}

/// The red calibration path: the gate over the planted fact set, with the
/// failure naming the plant.
///
/// # Errors
///
/// The planted mismatch must fail the gate; a green run over planted facts is
/// a toothless probe and is itself the error.
pub fn calibrate_planted() -> Result<CalibrationReport, String> {
    calibrate(&planted_facts()).map_err(|error| format!("planted calibration mismatch: {error}"))
}

/// The probe's process disposition: a green calibration is exit 0, a failed
/// gate is exit nonzero.
#[must_use]
pub fn probe_exit_code(report: &Result<CalibrationReport, String>) -> i32 {
    match report {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// A scenario family: the read-set shape and novelty regime the extrapolation
/// runs under. The shapes and novelties derive from the recorded corpora's
/// measured structure — never from the candidate count being modeled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioFamily {
    /// Exact repeated read sets: every candidate reads the same per-anchor key
    /// set, so the universe is one anchor's keys at any candidate count.
    Repeated,
    /// Steady-state age/survival: the steady segments' key recurrence (age
    /// 11-13 blocks, survival 5.6-6.8 anchors per key) over the pooled corpora
    /// anchor shape.
    SteadyState,
    /// Discovery-heavy novelty: the broader burst's read-set structure (294
    /// candidates at 2 anchors over 2,446 anchored pairs, 86% of the key
    /// material new at the dominant anchor, keys 0.31 blocks old).
    Discovery,
}

impl ScenarioFamily {
    /// The short label the report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Repeated => "repeated",
            Self::SteadyState => "steady-state",
            Self::Discovery => "discovery",
        }
    }

    /// The family's anchor shape source, for the report's provenance column.
    #[must_use]
    pub const fn shape_source(self) -> &'static str {
        match self {
            Self::Repeated | Self::SteadyState => {
                "pooled corpora (1,075 candidates / 33 anchors, 12,416 anchored keys)"
            }
            Self::Discovery => "broader burst segment (294 candidates / 2 anchors, 2,446 pairs)",
        }
    }

    /// The family's novelty source, for the report's provenance column.
    #[must_use]
    pub const fn novelty_source(self) -> &'static str {
        match self {
            Self::Repeated => "exact repetition (novelty zero by definition)",
            Self::SteadyState => {
                "steady segments' first-appearance share (528 new of 4,032 anchored pairs)"
            }
            Self::Discovery => "burst segment's first-appearance share (2,083 new of 2,446 pairs)",
        }
    }

    /// Candidates per anchor.
    #[must_use]
    pub fn fan_out(self) -> f64 {
        match self {
            Self::Repeated | Self::SteadyState => 1_075.0 / 33.0,
            Self::Discovery => 294.0 / 2.0,
        }
    }

    /// Distinct keys per anchor (the cold leg's per-anchor members).
    #[must_use]
    pub fn keys_per_anchor(self) -> f64 {
        match self {
            Self::Repeated | Self::SteadyState => 12_416.0 / 33.0,
            Self::Discovery => 2_446.0 / 2.0,
        }
    }

    /// The family's default novelty: the share of a later anchor's keys whose
    /// corpus-first appearance is that anchor.
    #[must_use]
    pub fn novelty(self) -> f64 {
        match self {
            Self::Repeated => 0.0,
            Self::SteadyState => 528.0 / 4_032.0,
            Self::Discovery => 2_083.0 / 2_446.0,
        }
    }
}

/// The committed synthetic snapshot leg the clone-cost extrapolation is
/// linear over: 100,000 entries cloned in 22.97ms.
pub const CLONE_LEG_ENTRIES: u64 = 100_000;

/// The committed synthetic snapshot leg's measured clone wall, in
/// microseconds (see [`CLONE_LEG_ENTRIES`]).
pub const CLONE_LEG_US: u64 = 22_970;

/// One extrapolation scenario: candidate count, family shape, an INDEPENDENT
/// novelty, one price point, and the lane/slot bound to model at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scenario {
    /// The candidate count to extrapolate to.
    pub candidates: u64,
    /// The read-set shape family.
    pub family: ScenarioFamily,
    /// The per-anchor first-appearance share, varied independently of the
    /// candidate count.
    pub novelty: f64,
    /// The price point to model.
    pub point: PricePoint,
    /// The scheduler's lane count (W).
    pub lanes: u64,
    /// The scheduler's slot count (F).
    pub slots: u64,
}

/// One scenario's aggregate report: what the carry-forward economy and the
/// snapshot look like at that candidate count and novelty — every number a
/// formula over aggregates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScenarioReport {
    /// The anchors the candidates land on.
    pub anchors: u64,
    /// The key universe after the last anchor (the snapshot's working set).
    pub universe_keys: u64,
    /// The cold leg's anchored fetches.
    pub cold_fetches: u64,
    /// Keys the carried snapshot answers.
    pub carried: u64,
    /// Keys the anchor's own final writes answer.
    pub final_write: u64,
    /// The carry leg's unique fetches: the dynamic members.
    pub dynamic: u64,
    /// The eliminated stream's read-dispatch rounds.
    pub eliminated_rounds: u64,
    /// The carry leg's modeled rounds.
    pub carry_rounds: u64,
    /// The cold leg's modeled rounds.
    pub cold_rounds: u64,
    /// The eliminated stream's modeled storage wait.
    pub eliminated_wait_us: u64,
    /// The carry leg's modeled storage wait.
    pub carry_wait_us: u64,
    /// The modeled cold wall (exec + storage wait + account + other rates).
    pub cold_wall_us: u64,
    /// The estimated eliminable storage-wait ceiling, per-mille of the modeled
    /// cold wall.
    pub ceiling_pm: u64,
    /// The carry snapshot's entry count (working set + write-only entries).
    pub snapshot_entries: u64,
    /// The snapshot clone cost's linear extrapolation of the measured
    /// 100,000-entry leg — an allocation-cost surrogate, NOT an allocation
    /// measurement.
    pub clone_estimate_us: u64,
}

/// The scenario extrapolation's rates: pooled coalescing widths and partition
/// survivals, the pooled carry-hit rates, and the per-candidate wall rates the
/// corpora measured.
#[derive(Clone, Copy, Debug)]
pub struct ScenarioRates {
    /// The eliminated stream's pooled coalescing width (keys per round).
    pub eliminated_width: f64,
    /// The residual stream's coalescing width (members per round).
    pub residual_width: f64,
    /// The eliminated stream's partition survival per price point.
    pub eliminated_survival: [f64; 2],
    /// The residual stream's partition survival per price point.
    pub residual_survival: [f64; 2],
    /// The final-write share of the anchored keys.
    pub final_write_rate: f64,
    /// The novel-key share whose first appearance is its anchor's final write.
    pub novel_final_write_rate: f64,
    /// The recorded execution wall per candidate.
    pub exec_us_per_candidate: f64,
    /// The account wait per candidate per price point.
    pub account_us_per_candidate: [f64; 2],
    /// The scheduler/other residual per candidate per price point.
    pub other_us_per_candidate: [f64; 2],
    /// The account batch members per candidate.
    pub account_members_per_candidate: f64,
    /// The snapshot's write-only entries per working-set key.
    pub write_only_rate: f64,
    /// The snapshot clone cost per entry, linear over the measured
    /// [`CLONE_LEG_ENTRIES`]-entry leg.
    pub clone_us_per_entry: f64,
}

/// Derive the extrapolation rates from the calibration corpora: the shared
/// coefficients plus the pooled per-candidate wall rates and carry-hit rates.
#[must_use]
pub fn scenario_rates(facts: &[CorpusFacts]) -> ScenarioRates {
    let coefficients = coefficients(facts);
    let mut candidates = 0u64;
    let mut final_write = 0u64;
    let mut anchored_keys = 0u64;
    let mut novel_final_write = 0u64;
    let mut global_keys = 0u64;
    let mut account_members = 0u64;
    let mut exec_us = 0u64;
    let mut account_us = [0u64; 2];
    let mut other_us = [0u64; 2];
    let mut write_only = (0u64, 0u64);
    for corpus in facts {
        candidates += corpus.candidates;
        final_write += corpus.final_write;
        anchored_keys += corpus.anchored_keys;
        global_keys += corpus.global_keys;
        novel_final_write += corpus.global_keys.saturating_sub(corpus.dynamic);
        account_members += corpus.account_members;
        for point in PricePoint::ALL {
            let measured = corpus.point(point);
            exec_us += measured.exec_us;
            account_us[point.index()] += measured.account_us;
            other_us[point.index()] += measured.other_us;
        }
        if let (Some(entries), Some(only)) = (corpus.snapshot_entries, corpus.write_only_entries) {
            write_only = (only, entries - only);
        }
    }
    let points = to_f64(u64::try_from(PricePoint::ALL.len()).unwrap_or(1));
    ScenarioRates {
        eliminated_width: coefficients.eliminated_width_pooled,
        residual_width: coefficients.residual_width,
        eliminated_survival: coefficients.eliminated_survival,
        residual_survival: coefficients.residual_survival,
        final_write_rate: rate(final_write, anchored_keys),
        novel_final_write_rate: rate(novel_final_write, global_keys),
        exec_us_per_candidate: to_f64(exec_us) / points / to_f64(candidates.max(1)),
        account_us_per_candidate: [
            rate(account_us[0], candidates),
            rate(account_us[1], candidates),
        ],
        other_us_per_candidate: [rate(other_us[0], candidates), rate(other_us[1], candidates)],
        account_members_per_candidate: rate(account_members, candidates),
        write_only_rate: rate(write_only.0, write_only.1),
        clone_us_per_entry: rate(CLONE_LEG_US, CLONE_LEG_ENTRIES),
    }
}

/// The rounded-up aggregate count: a partial fan-out still lands on its own
/// anchor.
#[must_use]
pub fn ceiling_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is finite, positive, and pre-rounded"
    )]
    let rounded = value.ceil() as u64;
    rounded
}

/// Model one scenario over the calibration corpora's pooled rates: the
/// key-universe growth from the independent novelty, the carry-hit partition,
/// the logical rounds from the two coalescing widths (scaled by the W/F bound
/// against the measured W=2/F=8 — a documented sensitivity rule, not a
/// measurement), the storage-wait estimate and ceiling, and the snapshot
/// working-set growth.
///
/// # Errors
///
/// A novelty outside `[0, 1]` or a lane/slot bound of zero is outside the
/// model's declared domain and fails loud instead of modeling nonsense.
pub fn scenario_report(scenario: &Scenario) -> Result<ScenarioReport, String> {
    if !scenario.novelty.is_finite() || !(0.0..=1.0).contains(&scenario.novelty) {
        return Err(format!(
            "novelty {} is outside the model's domain [0, 1]",
            scenario.novelty
        ));
    }
    if scenario.lanes == 0 || scenario.slots == 0 {
        return Err("a scenario needs at least one lane and one slot".to_string());
    }
    let rates = scenario_rates(&CORPORA);
    let anchors = ceiling_u64(to_f64(scenario.candidates) / scenario.family.fan_out());
    let keys_per_anchor = scenario.family.keys_per_anchor();
    let cold_fetches = nearest(to_f64(anchors) * keys_per_anchor);
    let universe_keys =
        nearest(keys_per_anchor * (1.0 + to_f64(anchors.saturating_sub(1)) * scenario.novelty))
            .min(cold_fetches);
    let accounting = carry_accounting(
        cold_fetches,
        universe_keys,
        rates.final_write_rate,
        rates.novel_final_write_rate,
    );
    // The W/F sensitivity rule: the coalescing width scales with the lane-slot
    // count against the measured W=2/F=8, capped at one anchor's keys.
    let bound_scale =
        to_f64(scenario.lanes * scenario.slots) / to_f64(MEASURED_LANES * MEASURED_SLOTS);
    let eliminated_width = (rates.eliminated_width * bound_scale).min(keys_per_anchor);
    let residual_width = (rates.residual_width * bound_scale).min(keys_per_anchor);
    let account_members =
        nearest(to_f64(scenario.candidates) * rates.account_members_per_candidate);
    let point = scenario.point;
    let modeled = model_streams(
        accounting.carried + accounting.final_write,
        accounting.dynamic,
        account_members,
        eliminated_width,
        residual_width,
        rates.eliminated_survival[point.index()],
        rates.residual_survival[point.index()],
        point.storage_us(),
        nearest(to_f64(scenario.candidates) * rates.exec_us_per_candidate),
        nearest(to_f64(scenario.candidates) * rates.account_us_per_candidate[point.index()]),
        nearest(to_f64(scenario.candidates) * rates.other_us_per_candidate[point.index()]),
    );
    let snapshot_entries = nearest(to_f64(universe_keys) * (1.0 + rates.write_only_rate));
    Ok(ScenarioReport {
        anchors,
        universe_keys,
        cold_fetches,
        carried: accounting.carried,
        final_write: accounting.final_write,
        dynamic: accounting.dynamic,
        eliminated_rounds: modeled.eliminated_rounds,
        carry_rounds: modeled.carry_rounds,
        cold_rounds: modeled.cold_rounds,
        eliminated_wait_us: modeled.eliminated_wait_us,
        carry_wait_us: modeled.carry_wait_us,
        cold_wall_us: modeled.cold_wall_us,
        ceiling_pm: modeled.ceiling_pm,
        snapshot_entries,
        clone_estimate_us: nearest(to_f64(snapshot_entries) * rates.clone_us_per_entry),
    })
}
