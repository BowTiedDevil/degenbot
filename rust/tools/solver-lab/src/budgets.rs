//! Deterministic concentrated-liquidity walk-count budgets.
//!
//! The artifact owns exact per-solve counters for the two golden-bearing
//! offline paths. Timing fields stay out of the artifact; every repetition
//! includes prepared-hop derivation and drains telemetry back to a per-solve
//! delta before the next repetition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use alloy::primitives::U256;
use degenbot_pools::solve::concentrated_liquidity::{
    solve_concentrated_liquidity_piecewise, PreparedConcentratedLiquidityHop, WalkNsCensus,
    WalkStats, WalkTelemetry,
};
use degenbot_pools::solve::fixtures::{write_plain_atomic_checked, FixtureWriteError};
use degenbot_pools::solve::golden_gate::ReplaySolution;
use degenbot_pools::solve::runtime::SolveRuntimeConfig;
use degenbot_pools::state::int_v3_hop::IntV3TickRangeSequence;
use serde::{Deserialize, Serialize};

pub const BUDGET_SCHEMA: u32 = 1;
pub const BUDGET_WORKFLOW: &str = "derivation-inclusive-prepared-entry";
pub const UNCUT: &str = "uncut";
pub const REQUIRED_NONZERO: [&str; 5] = [
    "stats.pieces",
    "stats.sims",
    "stats.refine_sims",
    "ns.refine_sims",
    "ns.dir_sims",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetArtifact {
    pub schema: u32,
    pub workflow: String,
    pub entries: Vec<BudgetEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub corpus: String,
    pub path_id: u64,
    pub variant: String,
    pub range_fingerprint: Vec<usize>,
    pub required_nonzero: Vec<String>,
    pub counts: BTreeMap<String, u64>,
}

impl BudgetEntry {
    #[must_use]
    pub fn from_measurements(
        corpus: &str,
        path_id: u64,
        range_fingerprint: Vec<usize>,
        stats: &WalkStats,
        ns: &WalkNsCensus,
    ) -> Self {
        let count = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        let counts = BTreeMap::from([
            ("stats.pieces".to_string(), count(stats.pieces)),
            ("stats.sims".to_string(), count(stats.sims)),
            ("stats.refine_sims".to_string(), count(stats.refine_sims)),
            ("stats.ternary_sims".to_string(), count(stats.ternary_sims)),
            ("stats.grid_sims".to_string(), count(stats.grid_sims)),
            ("stats.anchor_sims".to_string(), count(stats.anchor_sims)),
            ("ns.dir_sims".to_string(), ns.dir_sims),
            ("ns.refine_sims".to_string(), ns.refine_sims),
        ]);
        Self {
            corpus: corpus.to_string(),
            path_id,
            variant: UNCUT.to_string(),
            range_fingerprint,
            required_nonzero: REQUIRED_NONZERO.map(str::to_string).to_vec(),
            counts,
        }
    }

    fn identity(&self) -> (&str, u64, &str) {
        (&self.corpus, self.path_id, &self.variant)
    }

    fn owned_identity(&self) -> (String, u64, String) {
        (self.corpus.clone(), self.path_id, self.variant.clone())
    }

    fn label(&self) -> String {
        format!("{} path {} {}", self.corpus, self.path_id, self.variant)
    }
}

impl BudgetArtifact {
    pub fn from_actuals(actuals: &[BudgetEntry]) -> Result<Self, String> {
        let mut entries = actuals.to_vec();
        entries.sort_by_key(BudgetEntry::owned_identity);
        let artifact = Self {
            schema: BUDGET_SCHEMA,
            workflow: BUDGET_WORKFLOW.to_string(),
            entries,
        };
        let report = compare_budgets(actuals, &artifact);
        if report.ok() {
            Ok(artifact)
        } else {
            Err(report.issues.join("\n"))
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BudgetReport {
    pub compared: usize,
    pub budget_rows: usize,
    pub issues: Vec<String>,
}

impl BudgetReport {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.issues.is_empty() && self.compared > 0 && self.compared == self.budget_rows
    }

    #[must_use]
    pub fn summary(&self) -> String {
        if self.ok() {
            format!(
                "BUDGET: compared {} row(s); all required measurements present and counts match",
                self.compared
            )
        } else {
            format!(
                "BUDGET: compared {}/{} row(s); {} issue(s)",
                self.compared,
                self.budget_rows,
                self.issues.len()
            )
        }
    }
}

#[must_use]
pub fn compare_budgets(actuals: &[BudgetEntry], budget: &BudgetArtifact) -> BudgetReport {
    let mut report = BudgetReport {
        budget_rows: budget.entries.len(),
        ..BudgetReport::default()
    };
    if budget.schema != BUDGET_SCHEMA {
        report.issues.push(format!(
            "DRIFT: budget schema {} differs from supported {}",
            budget.schema, BUDGET_SCHEMA
        ));
    }
    if budget.workflow != BUDGET_WORKFLOW {
        report.issues.push(format!(
            "DRIFT: budget workflow {:?} differs from {:?}",
            budget.workflow, BUDGET_WORKFLOW
        ));
    }
    if actuals.is_empty() {
        report
            .issues
            .push("UNMEASURED: zero compared rows".to_string());
        return report;
    }
    let mut actual_identities = BTreeSet::new();
    for actual in actuals {
        if !actual_identities.insert(actual.owned_identity()) {
            report.issues.push(format!(
                "DRIFT: duplicate actual row for {}",
                actual.label()
            ));
        }
    }
    let mut budget_identities = BTreeSet::new();
    for entry in &budget.entries {
        if !budget_identities.insert(entry.owned_identity()) {
            report
                .issues
                .push(format!("DRIFT: duplicate budget row for {}", entry.label()));
        }
        let Some(actual) = actuals
            .iter()
            .find(|actual| actual.identity() == entry.identity())
        else {
            report
                .issues
                .push(format!("DRIFT: missing actual row for {}", entry.label()));
            continue;
        };
        report.compared += 1;
        compare_entry(actual, entry, &mut report);
    }
    for actual in actuals {
        if !budget
            .entries
            .iter()
            .any(|entry| entry.identity() == actual.identity())
        {
            report.issues.push(format!(
                "DRIFT: unmatched actual row for {}",
                actual.label()
            ));
        }
    }
    if report.compared < budget.entries.len() {
        report.issues.push(format!(
            "DRIFT: compared {} rows, fewer than {} artifact entries",
            report.compared,
            budget.entries.len()
        ));
    }
    report
}

fn compare_entry(actual: &BudgetEntry, budget: &BudgetEntry, report: &mut BudgetReport) {
    if actual.range_fingerprint != budget.range_fingerprint {
        report.issues.push(format!(
            "DRIFT: {} range fingerprint actual {:?} budget {:?}",
            budget.label(),
            actual.range_fingerprint,
            budget.range_fingerprint
        ));
    }
    let declared: BTreeSet<&str> = budget.required_nonzero.iter().map(String::as_str).collect();
    for required in REQUIRED_NONZERO {
        if !declared.contains(required) {
            report.issues.push(format!(
                "DRIFT: {} does not declare required-nonzero {required}",
                budget.label()
            ));
        }
    }
    for required in &budget.required_nonzero {
        match actual.counts.get(required) {
            None => report.issues.push(format!(
                "UNMEASURED: {} missing required counter {required}",
                budget.label()
            )),
            Some(0) => report.issues.push(format!(
                "UNMEASURED: {} required counter {required} is zero",
                budget.label()
            )),
            Some(_) => {}
        }
    }
    let keys: BTreeSet<&String> = actual.counts.keys().chain(budget.counts.keys()).collect();
    for key in keys {
        match (actual.counts.get(key), budget.counts.get(key)) {
            (Some(actual), Some(expected)) if actual != expected => {
                report.issues.push(format!(
                    "DRIFT: {} {key} actual {actual} budget {expected}",
                    budget.label()
                ));
            }
            (Some(actual), None) => report.issues.push(format!(
                "DRIFT: {} {key} actual {actual} has no budget value",
                budget.label()
            )),
            (None, Some(_)) => report.issues.push(format!(
                "UNMEASURED: {} missing measured counter {key}",
                budget.label()
            )),
            _ => {}
        }
    }
}

pub fn load_budget(path: &Path) -> Result<BudgetArtifact, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read budget {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse budget {}: {error}", path.display()))
}

pub fn write_budget(path: &Path, artifact: &BudgetArtifact) -> Result<(), String> {
    write_budget_with(path, artifact, || Ok(()))
}

fn write_budget_with(
    path: &Path,
    artifact: &BudgetArtifact,
    after_parse: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(artifact)
        .map_err(|error| format!("serialize budget: {error}"))?;
    let text = text + "\n";
    write_plain_atomic_checked(path, &text, |temp| {
        let result = (|| {
            let written = std::fs::read_to_string(temp)
                .map_err(|error| format!("read temporary budget: {error}"))?;
            let parsed: BudgetArtifact = serde_json::from_str(&written)
                .map_err(|error| format!("parse temporary budget: {error}"))?;
            if &parsed != artifact {
                return Err("temporary budget differs from generated artifact".to_string());
            }
            after_parse()
        })();
        result.map_err(|reason| FixtureWriteError::Validation {
            path: temp.to_path_buf(),
            reason,
        })
    })
    .map_err(|error| error.to_string())
}

#[derive(Debug)]
pub struct ReplayRepetitions {
    pub times: Vec<u128>,
    pub runs: Vec<Option<ReplaySolution>>,
    pub last_stats: WalkStats,
    pub last_ns: WalkNsCensus,
    pub census_pieces: Vec<(Vec<usize>, U256)>,
}

#[must_use]
pub fn measure_repetitions(refs: &[&IntV3TickRangeSequence], iters: usize) -> ReplayRepetitions {
    let iters = iters.max(1);
    let telemetry = WalkTelemetry::new();
    let cfg = SolveRuntimeConfig::default();
    let mut times = Vec::with_capacity(iters);
    let mut runs = Vec::with_capacity(iters);
    let mut last_stats = WalkStats::default();
    let mut last_ns = WalkNsCensus::default();
    let mut census_pieces = Vec::new();
    for _ in 0..iters {
        let t0 = std::time::Instant::now();
        let prepared: Vec<PreparedConcentratedLiquidityHop> = refs
            .iter()
            .map(|seq| {
                PreparedConcentratedLiquidityHop::derive(Arc::new(IntV3TickRangeSequence::clone(
                    seq,
                )))
            })
            .collect();
        let out =
            solve_concentrated_liquidity_piecewise(&prepared, None, Some(&telemetry), &cfg, None);
        times.push(t0.elapsed().as_micros());
        last_stats = out.stats;
        census_pieces = out.census_pieces;
        last_ns = telemetry.drain_ns();
        runs.push(out.result.map(|solution| ReplaySolution {
            optimal_input: solution.optimal_input,
            hop_outputs: solution.hop_outputs,
        }));
    }
    ReplayRepetitions {
        times,
        runs,
        last_stats,
        last_ns,
        census_pieces,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::concentrated_liquidity_rows::parse_concentrated_liquidity_hops;
    use degenbot_pools::solve::fixtures::{fixture_path, read_fixture, rows};
    use tempfile::TempDir;

    fn entry(path_id: u64) -> BudgetEntry {
        let counts = if path_id == 4 {
            [38, 1188, 366, 300, 66, 180, 455, 402]
        } else {
            [191, 3430, 123, 90, 33, 955, 1379, 123]
        };
        let keys = [
            "stats.pieces",
            "stats.sims",
            "stats.refine_sims",
            "stats.ternary_sims",
            "stats.grid_sims",
            "stats.anchor_sims",
            "ns.dir_sims",
            "ns.refine_sims",
        ];
        BudgetEntry {
            corpus: "concentrated_liquidity_capture_offline.jsonl".to_string(),
            path_id,
            variant: UNCUT.to_string(),
            range_fingerprint: vec![2, 3],
            required_nonzero: REQUIRED_NONZERO.map(str::to_string).to_vec(),
            counts: keys.into_iter().map(str::to_string).zip(counts).collect(),
        }
    }

    fn artifact() -> BudgetArtifact {
        BudgetArtifact::from_actuals(&[entry(4), entry(105)]).unwrap()
    }

    #[test]
    fn increased_count_is_a_readable_drift() {
        let mut actual = entry(4);
        *actual.counts.get_mut("stats.sims").unwrap() += 1;
        let report = compare_budgets(&[actual, entry(105)], &artifact());
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("DRIFT:") && issue.contains("stats.sims")));
    }

    #[test]
    fn zero_required_counter_is_unmeasured() {
        let mut actual = entry(4);
        *actual.counts.get_mut("ns.dir_sims").unwrap() = 0;
        let report = compare_budgets(&[actual, entry(105)], &artifact());
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("UNMEASURED:") && issue.contains("ns.dir_sims")));
    }

    #[test]
    fn zero_matched_rows_fails_closed() {
        let report = compare_budgets(&[], &artifact());
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue == "UNMEASURED: zero compared rows"));
    }

    #[test]
    fn missing_artifact_row_fails_closed() {
        let mut budget = artifact();
        budget.entries.retain(|entry| entry.path_id != 105);
        let report = compare_budgets(&[entry(4), entry(105)], &budget);
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("unmatched actual") && issue.contains("105")));
    }

    #[test]
    fn fewer_actual_rows_than_artifact_entries_fails_closed() {
        let report = compare_budgets(&[entry(4)], &artifact());
        assert_eq!(report.compared, 1);
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("missing actual") && issue.contains("105")));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("fewer than 2 artifact entries")));
    }

    #[test]
    fn range_fingerprint_mismatch_fails_closed() {
        let mut budget = artifact();
        budget.entries[0].range_fingerprint[0] += 1;
        let report = compare_budgets(&[entry(4), entry(105)], &budget);
        assert!(!report.ok());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("range fingerprint")));
    }

    #[test]
    fn legitimate_zero_counters_are_compared_exactly() {
        let mut first = entry(4);
        first.counts.insert("stats.ternary_sims".to_string(), 0);
        let mut second = entry(105);
        second.counts.insert("stats.ternary_sims".to_string(), 0);
        let budget = BudgetArtifact::from_actuals(&[first.clone(), second]).unwrap();
        assert!(!compare_budgets(&[first.clone(), entry(105)], &budget).ok());
        let mut second = entry(105);
        second.counts.insert("stats.ternary_sims".to_string(), 0);
        assert!(compare_budgets(&[first.clone(), second], &budget).ok());
        first.counts.insert("stats.ternary_sims".to_string(), 1);
        assert!(!compare_budgets(&[first, entry(105)], &budget).ok());
    }

    #[test]
    fn interrupted_generation_preserves_previous_artifact() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("budget.json");
        std::fs::write(&path, "previous\n").unwrap();
        let error =
            write_budget_with(&path, &artifact(), || Err("interrupted".to_string())).unwrap_err();
        assert!(error.contains("interrupted"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "previous\n");
        assert!(!dir
            .path()
            .join(format!(".budget.json.regen.{}.tmp", std::process::id()))
            .exists());
    }

    #[test]
    fn generation_read_back_round_trips_the_artifact() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("budget.json");
        let budget = artifact();
        write_budget(&path, &budget).unwrap();
        assert_eq!(load_budget(&path).unwrap(), budget);
    }

    fn captured(path_id: u64) -> Vec<IntV3TickRangeSequence> {
        let path = fixture_path("concentrated_liquidity_capture_offline.jsonl");
        let content = read_fixture(&path);
        let row = rows(&content)
            .into_iter()
            .find(|row| row["path_id"].as_u64() == Some(path_id))
            .unwrap();
        parse_concentrated_liquidity_hops(row["hops"].as_array().unwrap()).unwrap()
    }

    #[test]
    fn per_solve_ns_counts_and_verdicts_are_invariant_for_one_and_nine_reps() {
        for path_id in [4, 105] {
            let seqs = captured(path_id);
            let refs: Vec<&IntV3TickRangeSequence> = seqs.iter().collect();
            let one = measure_repetitions(&refs, 1);
            let nine = measure_repetitions(&refs, 9);
            assert_eq!(one.runs[0], nine.runs[8]);
            let fingerprint: Vec<usize> = seqs.iter().map(|seq| seq.ranges.len()).collect();
            let one_entry = BudgetEntry::from_measurements(
                "concentrated_liquidity_capture_offline.jsonl",
                path_id,
                fingerprint.clone(),
                &one.last_stats,
                &one.last_ns,
            );
            let nine_entry = BudgetEntry::from_measurements(
                "concentrated_liquidity_capture_offline.jsonl",
                path_id,
                fingerprint,
                &nine.last_stats,
                &nine.last_ns,
            );
            assert_eq!(one_entry.counts, nine_entry.counts);
            let budget = BudgetArtifact::from_actuals(std::slice::from_ref(&one_entry)).unwrap();
            let one_report = compare_budgets(std::slice::from_ref(&one_entry), &budget);
            let nine_report = compare_budgets(std::slice::from_ref(&nine_entry), &budget);
            assert_eq!(one_report, nine_report);
            assert!(one_report.ok());
        }
    }
}
