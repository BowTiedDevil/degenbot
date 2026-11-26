use std::path::Path;

use alloy::primitives::U256;
use degenbot_pools::solve::fixtures::{write_fixture, FixtureWriteOutcome};
use degenbot_pools::solve::golden_gate::{classify, parse_golden, GateVerdict, ReplaySolution};
use serde_json::{json, Value};

#[derive(Debug)]
pub struct RegenError(String);

impl RegenError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for RegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegenError {}

#[derive(Clone, Debug)]
pub struct GoldenRefresh {
    line_index: usize,
    path_id: u64,
    old_input: U256,
    old_profit: U256,
    new_input: U256,
    new_profit: U256,
    old_hop_outputs: Vec<String>,
    hop_outputs: Vec<U256>,
    over_shoot: U256,
}

impl GoldenRefresh {
    pub fn try_new(
        line_index: usize,
        doc: &Value,
        replay: Option<&ReplaySolution>,
    ) -> Result<Self, RegenError> {
        let path_id = doc.get("path_id").and_then(Value::as_u64).unwrap_or(0);
        let golden = parse_golden(doc.get("golden"))
            .ok_or_else(|| RegenError::new(format!("path {path_id}: no recorded golden")))?;
        let GateVerdict::OverShoot { over, .. } = classify(Some(&golden), replay) else {
            return Err(RegenError::new(format!(
                "path {path_id}: only over-shoot verdicts are regeneration-eligible"
            )));
        };
        let replay = replay.ok_or_else(|| {
            RegenError::new(format!("path {path_id}: over-shoot has no replay solution"))
        })?;
        let old_hop_outputs = doc
            .get("golden")
            .and_then(|golden| golden.get("hop_outputs"))
            .and_then(Value::as_array)
            .map(|outputs| {
                outputs
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            line_index,
            path_id,
            old_input: golden.optimal_input,
            old_profit: golden.profit,
            new_input: replay.optimal_input,
            new_profit: replay.profit(),
            old_hop_outputs,
            hop_outputs: replay.hop_outputs.clone(),
            over_shoot: over,
        })
    }

    fn new_golden(&self) -> Value {
        json!({
            "optimal_input": self.new_input.to_string(),
            "profit": self.new_profit.to_string(),
            "hop_outputs": self
                .hop_outputs
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "path_id": self.path_id,
            "old_input": self.old_input.to_string(),
            "old_profit": self.old_profit.to_string(),
            "new_input": self.new_input.to_string(),
            "new_profit": self.new_profit.to_string(),
            "over_shoot": self.over_shoot.to_string(),
            "old_hop_outputs": self.old_hop_outputs,
            "new_hop_outputs": self
                .hop_outputs
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug)]
pub struct GoldenRegenReport {
    pub outcome: FixtureWriteOutcome,
    pub refreshes: Vec<GoldenRefresh>,
}

impl GoldenRegenReport {
    pub fn to_json(&self) -> Value {
        json!({
            "destination": self.outcome.destination.to_string_lossy(),
            "packed": self.outcome.packed,
            "removed_plain_shadow": self.outcome.removed_plain_shadow,
            "refreshes": self
                .refreshes
                .iter()
                .map(GoldenRefresh::to_json)
                .collect::<Vec<_>>(),
        })
    }

    pub fn human(&self) -> String {
        let mut out = String::new();
        for refresh in &self.refreshes {
            out.push_str(&format!(
                "path {}: optimal_input {} -> {} | profit {} -> {} | over_shoot {} | hop_outputs {:?} -> {:?}\n",
                refresh.path_id,
                refresh.old_input,
                refresh.new_input,
                refresh.old_profit,
                refresh.new_profit,
                refresh.over_shoot,
                refresh.old_hop_outputs,
                refresh.hop_outputs,
            ));
        }
        out
    }
}

pub fn apply_golden_refreshes(
    path: &Path,
    content: &str,
    refreshes: Vec<GoldenRefresh>,
) -> Result<GoldenRegenReport, RegenError> {
    let raw_lines: Vec<&str> = content.lines().collect();
    let mut rows = Vec::with_capacity(raw_lines.len());
    for (index, line) in raw_lines.iter().enumerate() {
        let row = serde_json::from_str::<Value>(line)
            .map_err(|error| RegenError::new(format!("capture line {}: {error}", index + 1)))?;
        if !row.is_object() {
            return Err(RegenError::new(format!(
                "capture line {}: row is not a JSON object",
                index + 1
            )));
        }
        rows.push(row);
    }
    let mut rewritten = std::collections::HashMap::new();
    for refresh in &refreshes {
        let Some(row) = rows.get(refresh.line_index).cloned() else {
            return Err(RegenError::new(format!(
                "path {}: refresh line {} is outside the capture",
                refresh.path_id, refresh.line_index
            )));
        };
        let mut after = row.clone();
        after["golden"] = refresh.new_golden();
        only_golden_changed(&row, &after)?;
        rewritten.insert(
            refresh.line_index,
            serde_json::to_string(&after).map_err(|error| RegenError::new(error.to_string()))?,
        );
    }
    let final_text = raw_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            rewritten
                .get(&index)
                .cloned()
                .unwrap_or_else(|| (*line).to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let outcome = write_fixture(path, &(final_text + "\n"))
        .map_err(|error| RegenError::new(error.to_string()))?;
    Ok(GoldenRegenReport { outcome, refreshes })
}

fn only_golden_changed(before: &Value, after: &Value) -> Result<(), RegenError> {
    let mut before = before.clone();
    let mut after = after.clone();
    before["golden"] = Value::Null;
    after["golden"] = Value::Null;
    if before != after {
        return Err(RegenError::new(
            "non-golden field changed during regeneration",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_pools::solve::fixtures::{read_fixture, write_plain_atomic};
    use tempfile::TempDir;

    fn doc() -> Value {
        json!({
            "path_id": 4,
            "hops": [],
            "golden": {
                "optimal_input": "10",
                "profit": "90",
                "hop_outputs": ["100"]
            }
        })
    }

    fn replay() -> ReplaySolution {
        ReplaySolution {
            optimal_input: U256::from(20u64),
            hop_outputs: vec![U256::from(200u64)],
        }
    }

    fn refresh() -> GoldenRefresh {
        GoldenRefresh::try_new(0, &doc(), Some(&replay())).unwrap()
    }

    #[test]
    fn over_shoot_regenerates_and_reports_old_and_new_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl");
        let content = format!("{}\n", serde_json::to_string(&doc()).unwrap());
        let report = apply_golden_refreshes(&path, &content, vec![refresh()]).unwrap();
        let updated = read_fixture(&path);
        assert!(updated.contains("\"optimal_input\":\"20\""));
        assert!(updated.contains("\"profit\":\"180\""));
        assert!(updated.contains("\"hops\":[]"));
        let human = report.human();
        assert!(human.contains("optimal_input 10 -> 20"));
        assert!(human.contains("profit 90 -> 180"));
        assert!(human.contains("over_shoot 90"));
        assert!(human.contains("hop_outputs [\"100\"] -> [200]"));
        let json = report.to_json();
        assert_eq!(json["refreshes"][0]["old_input"], "10");
        assert_eq!(json["refreshes"][0]["new_input"], "20");
        assert_eq!(json["refreshes"][0]["over_shoot"], "90");
        assert_eq!(json["refreshes"][0]["old_hop_outputs"][0], "100");
        assert_eq!(json["refreshes"][0]["new_hop_outputs"][0], "200");
    }

    #[test]
    fn under_shoot_is_not_regeneration_eligible() {
        let mut lower = doc();
        lower["golden"]["hop_outputs"] = json!(["1100"]);
        let error = GoldenRefresh::try_new(0, &lower, Some(&replay())).unwrap_err();
        assert!(error.to_string().contains("only over-shoot"));
    }

    #[test]
    fn non_golden_mutation_is_rejected() {
        let before = doc();
        let mut after = before.clone();
        after["hops"] = json!([1]);
        assert!(only_golden_changed(&before, &after).is_err());
    }

    #[test]
    fn malformed_rows_are_rejected_without_writing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl");
        assert!(apply_golden_refreshes(&path, "not-json\n", vec![refresh()]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn non_object_rows_are_rejected_without_writing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl");
        assert!(apply_golden_refreshes(&path, "1\n", vec![refresh()]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn explicit_zst_write_target_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl.zst");
        let content = format!("{}\n", serde_json::to_string(&doc()).unwrap());
        assert!(apply_golden_refreshes(&path, &content, vec![refresh()]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn packed_shadow_is_replaced_and_plain_shadow_removed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl");
        let packed = path.with_extension("jsonl.zst");
        let old = format!("{}\n", serde_json::to_string(&doc()).unwrap());
        write_plain_atomic(&path, &old).unwrap();
        let encoded = zstd::encode_all(old.as_bytes(), 3).unwrap();
        std::fs::write(&packed, encoded).unwrap();
        let report = apply_golden_refreshes(&path, &old, vec![refresh()]).unwrap();
        assert!(report.outcome.packed);
        assert!(report.outcome.removed_plain_shadow);
        assert!(!path.exists());
        let updated = read_fixture(&path);
        assert!(updated.contains("\"profit\":\"180\""));
    }

    #[test]
    fn divergent_shadow_is_refused_without_mutation() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("capture.jsonl");
        let packed = path.with_extension("jsonl.zst");
        write_plain_atomic(&path, "plain\n").unwrap();
        std::fs::write(&packed, zstd::encode_all(&b"packed\n"[..], 3).unwrap()).unwrap();
        let content = format!("{}\n", serde_json::to_string(&doc()).unwrap());
        assert!(apply_golden_refreshes(&path, &content, vec![refresh()]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "plain\n");
        assert_eq!(read_fixture(&packed), "packed\n");
    }
}
