//! Fractional cgroup-quota detection for the fleet budget authority.
//!
//! `degenbot_core::cpu_budget` owns runtime CPU sizing and keeps its ceil'd
//! integer budget (worker-existence sizing: a 4.5-core quota still buys a
//! 5th worker). The fleet's ALLOCATION arithmetic (ADR-042 §5) needs the
//! EXACT pre-ceil ratio instead — integer shares sum against `floor(Q)` and
//! the fractional remainder is spendable only by I/O-dominant consumers —
//! and the reviewed scope keeps that addition out of `cpu_budget.rs`. This
//! module is therefore the fleet-local fractional detector: the same
//! upward cgroup walk, `min`'d with affinity, floored at 1.0. If a second
//! consumer ever needs the fractional surface, promoting this into
//! `degenbot-core` is the follow-up.

use std::path::{Path, PathBuf};

/// Fractional (pre-ceil) cgroup v2 quota in `dir`: the exact
/// `quota / period` ratio, or `None` when unbounded (`max`) or the file is
/// missing/unreadable.
fn v2_quota_fractional(dir: &Path) -> Option<f64> {
    let raw = std::fs::read_to_string(dir.join("cpu.max")).ok()?;
    let mut parts = raw.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota: f64 = quota.parse().ok()?;
    let period: f64 = parts
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(100_000.0);
    if period == 0.0 || quota <= 0.0 {
        return None;
    }
    Some(quota / period)
}

/// Fractional (pre-ceil) cgroup v1 quota in `dir`: the exact
/// `cpu.cfs_quota_us / cpu.cfs_period_us` ratio, or `None` when unbounded
/// (`-1`) or the files are missing/unreadable.
fn v1_quota_fractional(dir: &Path) -> Option<f64> {
    let quota: f64 = std::fs::read_to_string(dir.join("cpu.cfs_quota_us"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if quota < 0.0 {
        return None; // -1 = unlimited
    }
    let period: f64 = std::fs::read_to_string(dir.join("cpu.cfs_period_us"))
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|p: &f64| *p > 0.0)
        .unwrap_or(100_000.0);
    if period == 0.0 {
        return None;
    }
    Some(quota / period)
}

/// Tightest fractional cgroup quota found walking `start` (or its nearest
/// existing ancestor chain up to `root`) upward, via `probe`. Missing
/// levels are skipped, not fatal; the tightest readable limit wins.
fn walk_quota_fractional(
    root: &Path,
    start: &Path,
    probe: fn(&Path) -> Option<f64>,
) -> Option<f64> {
    let mut min: Option<f64> = None;
    let mut cur: Option<&Path> = Some(start);
    while let Some(p) = cur {
        if let Some(q) = probe(p) {
            min = Some(match min {
                Some(existing) if existing <= q => existing,
                _ => q,
            });
        }
        if p == root || !p.starts_with(root) {
            break;
        }
        cur = p.parent();
    }
    min
}

/// `rel` may be absolute ("/a/b"), relative ("a/b"), or the root itself —
/// always join onto `root`, never replace it.
fn join_under_root(root: &Path, rel: &Path) -> PathBuf {
    let normalized = rel.strip_prefix(Path::new("/")).unwrap_or(rel);
    if normalized.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(normalized)
    }
}

/// Relative cgroup paths per `cgroup_text` (a `/proc/self/cgroup` body):
/// `(v2 unified path, v1 cpu-hierarchy path)`.
fn cgroup_relative_paths(cgroup_text: &str) -> (Option<String>, Option<String>) {
    let mut v2 = None;
    let mut v1: Option<String> = None;
    for line in cgroup_text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("0::") {
            if v2.is_none() {
                v2 = Some(path.to_string());
            }
            continue;
        }
        // v1: "<hierarchy-id>:<controllers>:<path>" — only a hierarchy
        // whose controller set contains the exact `cpu` token hosts cfs
        // quota files.
        let Some((controllers, path)) = line
            .split_once(':')
            .and_then(|(_, rest)| rest.split_once(':'))
        else {
            continue;
        };
        if controllers.split(',').any(|c| c == "cpu") && v1.is_none() {
            v1 = Some(path.to_string());
        }
    }
    (v2, v1)
}

/// `(cgroup2 mount root, cgroup v1 cpu mount root)` from `mounts_text` (a
/// `/proc/self/mounts` body). For v1 the cpu controller must be mounted; a
/// cpuacct-only mount has no cfs files.
fn cgroup_roots(mounts_text: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    for line in mounts_text.lines() {
        // <device> <mountpoint> <fstype> <options> ...
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let (Some(mountpoint), Some(fstype)) = (fields.next(), fields.next()) else {
            continue;
        };
        let options = fields.next().unwrap_or("");
        if fstype == "cgroup2" {
            return (Some(PathBuf::from(mountpoint)), None);
        }
        if fstype == "cgroup" && options.split(',').any(|t| t == "cpu") {
            return (None, Some(PathBuf::from(mountpoint)));
        }
    }
    (None, None)
}

/// Fractional effective budget from explicit inputs: the tightest cgroup
/// quota as an exact ratio (v2 or v1), min'd with `affinity`, floored at
/// 1.0. `cgroup_text`/`mounts_text` are `/proc/self/{cgroup,mounts}`
/// bodies; `fixture_root` lets tests inject a synthetic cgroup tree in
/// place of the real mount roots. Share arithmetic (ADR-042 §5) floors
/// against THIS value; worker-existence sizing keeps the ceil'd integer
/// budget in `degenbot_core::cpu_budget`.
fn fractional_budget_from(
    cgroup_text: &str,
    mounts_text: &str,
    affinity: f64,
    fixture_root: Option<&Path>,
) -> f64 {
    let (rel_v2, rel_v1) = cgroup_relative_paths(cgroup_text);
    let (root_v2, root_v1) = if let Some(root) = fixture_root {
        (Some(root.to_path_buf()), Some(root.to_path_buf()))
    } else {
        cgroup_roots(mounts_text)
    };
    let walk = |root: Option<PathBuf>, rel: Option<String>, probe: fn(&Path) -> Option<f64>| match (
        root, rel,
    ) {
        (Some(r), Some(p)) => {
            let start = join_under_root(&r, Path::new(&p));
            walk_quota_fractional(&r, &start, probe)
        }
        _ => None,
    };
    [
        walk(root_v2, rel_v2, v2_quota_fractional),
        walk(root_v1, rel_v1, v1_quota_fractional),
        Some(affinity),
    ]
    .into_iter()
    .flatten()
    .fold(None, |acc: Option<f64>, q| match acc {
        Some(existing) if existing <= q => Some(existing),
        _ => Some(q),
    })
    .unwrap_or(1.0)
    .max(1.0)
}

/// Fractional quota `Q` (cores) of the running process: the exact pre-ceil
/// cgroup ratio from the real `/proc/self/{cgroup,mounts}`, min'd with the
/// affinity count, floored at 1.0. This is what [`crate::budget`]
/// floor-allocation sums against (ADR-042 §5).
#[must_use]
pub fn fractional_cpu_budget() -> f64 {
    let cgroup_text = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let mounts_text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    #[expect(
        clippy::cast_precision_loss,
        reason = "affinity is a small platform CPU count; f64 is exact here"
    )]
    let affinity = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
    fractional_budget_from(&cgroup_text, &mounts_text, affinity, None)
}

#[cfg(all(test, unix))]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    fn fixture_dir(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("degenbot-workers-quota-{name}"))
            .join("cgroup")
    }

    fn write_cpu_max(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let d: PathBuf = if rel.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(rel)
        };
        std::fs::create_dir_all(&d).expect("fixture dir");
        std::fs::write(d.join("cpu.max"), body).expect("cpu.max");
        d
    }

    fn write_v1(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("v1 fixture dir");
        std::fs::write(dir.join(name), body).expect("v1 file");
        dir.to_path_buf()
    }

    #[test]
    fn v2_fractional_quota_is_the_exact_ratio() {
        let d = fixture_dir("v2-frac-exact");
        write_cpu_max(&d, "x", "450000 100000\n");
        let q = v2_quota_fractional(&d.join("x")).expect("4.5-core quota parses");
        assert!((q - 4.5).abs() < 1e-9, "exact ratio, got {q}");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn v1_fractional_quota_is_the_exact_ratio() {
        let root = fixture_dir("v1-frac-exact");
        let dir = write_v1(&root, "cpu.cfs_quota_us", "450000\n");
        write_v1(&root, "cpu.cfs_period_us", "100000\n");
        let q = v1_quota_fractional(&dir).expect("4.5-core v1 quota parses");
        assert!((q - 4.5).abs() < 1e-9, "exact ratio, got {q}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fractional_budget_walks_the_tightest_level() {
        let root = fixture_dir("walk-frac-tight");
        write_cpu_max(&root, "svc", "1600000 100000\n");
        write_cpu_max(&root, "svc/task", "620000 100000\n"); // 6.2 cores
        let text = "0::/svc/task\n";
        let mounts = "cgroup2 /sys/fs/cgroup cgroup2 rw\n";
        let q = fractional_budget_from(text, mounts, 24.0, Some(&root));
        assert!((q - 6.2).abs() < 1e-9, "tightest exact ratio, got {q}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fractional_budget_is_min_of_quota_affinity_and_floors_at_one() {
        let text = "0::/\n";
        let mounts = "cgroup2 /sys/fs/cgroup cgroup2 rw\n";
        let root = fixture_dir("budget-frac");
        write_cpu_max(&root, "", "450000 100000\n");
        let q = fractional_budget_from(text, mounts, 24.0, Some(&root));
        assert!(
            (q - 4.5).abs() < 1e-9,
            "quota tighter than affinity, got {q}"
        );
        let q = fractional_budget_from(text, mounts, 3.0, Some(&root));
        assert!(
            (q - 3.0).abs() < 1e-9,
            "affinity tighter than quota, got {q}"
        );
        write_cpu_max(&root, "", "900 100000\n");
        let q = fractional_budget_from(text, mounts, 24.0, Some(&root));
        assert!((q - 1.0).abs() < 1e-9, "floored at one, got {q}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fractional_budget_unlimited_falls_back_to_affinity() {
        let text = "0::/\n";
        let mounts = "proc /proc proc rw\n";
        let q = fractional_budget_from(text, mounts, 16.0, None);
        assert!(
            (q - 16.0).abs() < 1e-9,
            "no cgroup limit -> affinity, got {q}"
        );
    }
}
