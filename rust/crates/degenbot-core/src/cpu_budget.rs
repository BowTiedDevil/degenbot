//! Container-aware runtime CPU budget detection, shared by every consumer.
//!
//! `nproc`/`sched_getaffinity` lie inside containers: the devcontainer caps
//! the cgroup at `cpu.max = 800000 100000` (8 cores) while the affinity mask
//! can still show 24 host CPUs. The solve fan-out sized its 8 workers off
//! `rayon::current_num_threads()` and exhausted the quota
//! (`/sys/fs/cgroup/cpu.stat`: 967 throttle events / 260s frozen) - the
//! kernel froze the whole process on heavy solve bursts, the root cause of
//! the >10s solve p95.
//!
//! Detection walks UP the cgroup hierarchy (containers nest under slices
//! whose parents may be tighter) and takes the tightest limit found, min'd
//! with the affinity budget.
//!
//! Two-runtime sizing policy (SMTH6M: this module is the single sizing
//! authority for BOTH runtimes):
//! - solve bins: budget minus solve headroom (the main runtime, Python, the
//!   pump, and the `OTel` exporter share the quota and must not starve
//!   during bursts), overridable with `DEGENBOT_SOLVE_CPUS`;
//! - the ambient I/O runtime (`crate::runtime`): the leftover after the
//!   solve bins take theirs - i.e. the headroom - floored at 1, overridable
//!   with `DEGENBOT_IO_WORKERS`.
//!
//! Everything CPU-adjacent that is NOT a solve bin (inline-sim drivers
//! behind the fleet `SimDriver` seat cap, sim runtime sizing) derives from
//! the same leftover budget.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Cpus the cgroup v2 `cpu.max` in `dir` admits: `quota period`, or `None`
/// when unbounded (`max`) or the file is missing/unreadable. Fractional
/// ratios ceil (a 4.5-core quota still buys a 5th worker).
pub(crate) fn v2_quota_cpus(dir: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(dir.join("cpu.max")).ok()?;
    let mut parts = raw.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota: u64 = quota.parse().ok()?;
    let period: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(100_000);
    if period == 0 {
        return None;
    }
    Some(ceil_div_cpus(quota, period))
}

/// Cpus the cgroup v1 files (`cpu.cfs_quota_us`/`cpu.cfs_period_us`) in
/// `dir` admit. `-1` quota = unbounded -> `None`.
pub(crate) fn v1_quota_cpus(dir: &Path) -> Option<u64> {
    let quota: i64 = std::fs::read_to_string(dir.join("cpu.cfs_quota_us"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if quota < 0 {
        return None; // -1 = unlimited
    }
    #[expect(clippy::cast_sign_loss)] // negative rejected above
    let quota = quota as u64;
    let period: u64 = std::fs::read_to_string(dir.join("cpu.cfs_period_us"))
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|p: &u64| *p > 0)
        .unwrap_or(100_000);
    Some(ceil_div_cpus(quota as u64, period))
}

/// Tightest cgroup v2 quota found walking `<root>/<rel>` upward to
/// `<root>`. Missing levels are skipped, not fatal.
pub(crate) fn min_quota_v2(root: &Path, rel: &Path) -> Option<u64> {
    let start = join_under_root(root, rel);
    walk_quota(root, &start, v2_quota_cpus)
}

/// Tightest cgroup v1 quota found walking `<root>/<rel>` upward.
pub(crate) fn min_quota_v1(root: &Path, rel: &Path) -> Option<u64> {
    let start = join_under_root(root, rel);
    walk_quota(root, &start, v1_quota_cpus)
}

/// Relative cgroup paths per `cgroup_text` (a `/proc/self/cgroup` body):
/// `(v2 unified path, v1 cpu-hierarchy path)`.
pub(crate) fn cgroup_relative_paths(cgroup_text: &str) -> (Option<String>, Option<String>) {
    let mut v2 = None;
    let mut v1 = None;
    for line in cgroup_text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("0::") {
            if v2.is_none() {
                v2 = Some(path.to_string());
            }
            continue;
        }
        // v1: "<hierarchy-id>:<controllers>:<path>" - only a hierarchy whose
        // controller set contains the exact `cpu` token hosts cfs quota files.
        let Some((_, controllers, path)) = split_v1_cgroup_line(line) else {
            continue;
        };
        if controllers.split(',').any(|c| c == "cpu") && v1.is_none() {
            v1 = Some(path.to_string());
        }
    }
    (v2, v1)
}

fn split_v1_cgroup_line(line: &str) -> Option<(&str, &str, &str)> {
    let (hier, rest) = line.split_once(':')?;
    let (controllers, path) = rest.split_once(':')?;
    Some((hier, controllers, path))
}

/// `(cgroup2 mount root, cgroup v1 cpu mount root)` from a
/// `/proc/self/mounts` body.
pub(crate) fn cgroup_roots(mounts_text: &str) -> (Option<String>, Option<String>) {
    let mut v2 = None;
    let mut v1 = None;
    for line in mounts_text.lines() {
        // /proc/self/mounts: <device> <mountpoint> <fstype> <options> ... -
        // options are a single comma-joined field. For v1, the cpu quota
        // controller must be mounted (a cpuacct-only mount has no cfs files).
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let Some(mountpoint) = fields.next() else {
            continue;
        };
        let Some(fstype) = fields.next() else {
            continue;
        };
        let options = fields.next().unwrap_or("");
        if fstype == "cgroup2" && v2.is_none() {
            v2 = Some(mountpoint.to_string());
        } else if fstype == "cgroup" && v1.is_none() && options.split(',').any(|t| t == "cpu") {
            v1 = Some(mountpoint.to_string());
        }
    }
    (v2, v1)
}

/// Budget sizes are tiny; `u64` -> `usize` via `try_from` clamps to max.
fn budget_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// The effective CPU budget given explicit roots: tightest cgroup limit
/// (v2 or v1) min'd with the affinity budget, floored at 1. The `*_root`
/// parameters let tests inject fixture trees; `None` = unprobeable ->
/// falls back to affinity.
pub(crate) fn effective_budget_from_with_roots(
    cgroup_text: &str,
    mounts_text: &str,
    affinity: usize,
    fixture_root: Option<&Path>,
) -> usize {
    let (rel_v2, rel_v1) = cgroup_relative_paths(cgroup_text);
    let (root_v2, root_v1): (Option<PathBuf>, Option<PathBuf>) = if let Some(root) = fixture_root {
        (Some(root.to_path_buf()), Some(root.to_path_buf()))
    } else {
        let (v2, v1) = cgroup_roots(mounts_text);
        (v2.map(PathBuf::from), v1.map(PathBuf::from))
    };
    let v2_budget = root_v2
        .zip(rel_v2)
        .and_then(|(r, p)| min_quota_v2(&r, Path::new(&p)));
    let v1_budget = root_v1
        .zip(rel_v1)
        .and_then(|(r, p)| min_quota_v1(&r, Path::new(&p)));
    [
        v2_budget.map(budget_usize),
        v1_budget.map(budget_usize),
        Some(affinity),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(1)
    .max(1)
}

/// Solve-worker count from overrides: `override_cpu` (`DEGENBOT_SOLVE_CPUS`)
/// wins outright (headroom ignored); otherwise budget minus the headroom
/// (`override_headroom`, default [`DEFAULT_SOLVE_HEADROOM`]), floored at 1.
///
/// Public so the fleet budget authority can property-test the
/// cross-authority sizing contract `TTANQJ` (epic 64ZQLA), and so a
/// standalone (ADR-005) consumer can size solve bins from a budget it
/// derived itself.
#[must_use]
pub fn solve_worker_count_from(
    override_cpu: Option<&str>,
    override_headroom: Option<&str>,
    budget: usize,
) -> usize {
    // The explicit override is terminal: the operator knows the quota better
    // than a heuristic (e.g. "give the bot all cores, I tuned elsewhere").
    if let Some(raw) = override_cpu {
        if let Ok(n) = raw.trim().parse::<usize>() {
            return n.max(1);
        }
        // unparseable override falls through to the budget policy
    }
    let headroom = override_headroom
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SOLVE_HEADROOM);
    budget.saturating_sub(headroom).max(1)
}

pub const DEFAULT_SOLVE_HEADROOM: usize = 2;

/// Process-wide CPU budget: reads the real `/proc/self/{cgroup,mounts}` and
/// affinity once.
pub(crate) fn effective_cpu_budget() -> usize {
    let cgroup_text = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let mounts_text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let affinity = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    effective_budget_from_with_roots(&cgroup_text, &mounts_text, affinity, None)
}

/// Cached solve worker count; logs the detection verdict once via `tracing`.
/// The leftover of the effective CPU budget after the solve bins take
/// theirs is computed by [`leftover_worker_budget`].
pub fn solve_worker_count() -> usize {
    static SOLVE_WORKERS: OnceLock<usize> = OnceLock::new();
    *SOLVE_WORKERS.get_or_init(|| {
        let budget = effective_cpu_budget();
        // KAHU5W: typed schema overrides (`solve.solve_cpus` /
        // `solve.solve_headroom`); the loader owns the env read, and the
        // holder lives in degenbot-config (installed once at boot).
        let cfg = ::degenbot_config::holder::config().solve.clone();
        let override_cpu = cfg.solve_cpus.map(|v| v.to_string());
        let override_headroom = cfg.solve_headroom.map(|v| v.to_string());
        let workers = solve_worker_count_from(
            override_cpu.as_deref(),
            override_headroom.as_deref(),
            budget,
        );
        crate::op_info!(
            domain = solver,
            cpu_budget = budget,
            solve_workers = workers,
            solve_headroom = DEFAULT_SOLVE_HEADROOM,
            "solve worker count detected from cgroup + affinity"
        );
        workers
    })
}

/// Cached solve worker count's leftover: the ambient I/O budget everything
/// non-solve derives from. Floor 1: one worker always remains for I/O
/// latency even under a headroom-less operator override.
#[must_use]
pub fn leftover_worker_budget() -> usize {
    effective_cpu_budget()
        .saturating_sub(solve_worker_count())
        .max(1)
}

/// Ambient I/O runtime worker count from explicit inputs (SMTH6M): the
/// `runtime.io_workers` override (`DEGENBOT_IO_WORKERS`) wins, floored at
/// 1; otherwise the two-runtime contract — the ambient runtime takes the
/// headroom the solve bins leave behind (`budget - solve_workers`), so the
/// two runtimes tile the cgroup budget instead of each sizing from raw
/// core counts.
#[must_use]
pub(crate) fn ambient_io_worker_count_from(
    io_workers_override: Option<usize>,
    solve_workers: usize,
    budget: usize,
) -> usize {
    io_workers_override
        .unwrap_or_else(|| budget.saturating_sub(solve_workers))
        .max(1)
}

/// Cached ambient I/O runtime worker count consumed by
/// [`crate::runtime::build_runtime`]. Tagged onto the same `[cpu-budget]`
/// boot-log family as [`solve_worker_count`] so one grep shows the whole
/// two-runtime split: the derived budget, the solve bins, and the ambient
/// I/O workers (plus whether an explicit override pinned the latter).
pub(crate) fn ambient_io_worker_count() -> usize {
    static AMBIENT_WORKERS: OnceLock<usize> = OnceLock::new();
    *AMBIENT_WORKERS.get_or_init(|| {
        let budget = effective_cpu_budget();
        let io_override = ::degenbot_config::holder::config().runtime.io_workers;
        let solve = solve_worker_count();
        let workers = ambient_io_worker_count_from(io_override, solve, budget);
        crate::op_info!(domain = solver, cpu_budget = budget,
            solve_workers = solve,
            ambient_io_workers = workers,
            io_workers_override = ?io_override,
            "ambient I/O runtime sized from the cgroup + affinity budget minus the solve bins"
        );
        workers
    })
}

/// `rel` may be absolute ("/a/b"), relative ("a/b"), or the root itself
/// ("/", empty after strip) - always join onto `root`, never replace it.
fn join_under_root(root: &Path, rel: &Path) -> PathBuf {
    let normalized = rel.strip_prefix(Path::new("/")).unwrap_or(rel);
    if normalized.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(normalized)
    }
}

fn ceil_div_cpus(quota: u64, period: u64) -> u64 {
    quota.div_ceil(period).max(1)
}

/// Walk `start` (or its nearest existing ancestor chain up to `root`),
/// taking the tightest quota any level declares. Missing levels are
/// skipped: a read-only or partially-mounted tree still yields the limit
/// of whatever levels ARE readable.
fn walk_quota(root: &Path, start: &Path, probe: fn(&Path) -> Option<u64>) -> Option<u64> {
    let mut min: Option<u64> = None;
    let mut cur: Option<&Path> = Some(start);
    while let Some(p) = cur {
        if let Some(q) = probe(p) {
            min = Some(match min {
                Some(existing) => existing.min(q),
                None => q,
            });
        }
        if p == root || !p.starts_with(root) {
            break;
        }
        cur = p.parent();
    }
    min
}

/// Parsed throttle fields from a cgroup v2 `cpu.stat` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleStats {
    pub nr_throttled: u64,
    pub throttled_usec: u64,
}

/// Parse the throttle-relevant fields of a `cpu.stat` body (missing lines
/// are zeros; unreadable file => None).
pub(crate) fn parse_cpu_stat(text: &str) -> Option<ThrottleStats> {
    let mut stats = ThrottleStats {
        nr_throttled: 0,
        throttled_usec: 0,
    };
    let mut any_pair = false;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(' ') else {
            continue;
        };
        let Ok(v) = val.trim().parse::<u64>() else {
            continue;
        };
        any_pair = true;
        match key {
            "nr_throttled" => stats.nr_throttled = v,
            "throttled_usec" => stats.throttled_usec = v,
            _ => {}
        }
    }
    // A body with no parseable key-value pairs is not a cpu.stat; a body
    // with one (e.g. usage_usec alone) is legitimate - missing throttle
    // fields read as zeros.
    any_pair.then_some(stats)
}

/// Delta of throttle counters since the previous call, zero on the first
/// call. None when the cgroup file is unreadable (no counter tape here).
pub fn cgroup_throttle_delta() -> Option<ThrottleStats> {
    static LAST: OnceLock<parking_lot::Mutex<Option<(u64, u64)>>> = OnceLock::new();
    let last = LAST.get_or_init(|| parking_lot::Mutex::new(None));
    let text = std::fs::read_to_string(CGROUP_CPU_STAT).ok()?;
    let stats = parse_cpu_stat(&text)?;
    let cur = (stats.nr_throttled, stats.throttled_usec);
    let (events, usecs) = {
        let mut guard = last.lock();
        let d = delta_from(*guard, Some(cur));
        *guard = Some(cur);
        d
    };
    Some(ThrottleStats {
        nr_throttled: events,
        throttled_usec: usecs,
    })
}

/// cgroup v2 mount root per this container's usual layout; the cpu-budget
/// detector derives roots dynamically, but the throttle tape is read at a
/// fixed path (containers mount cgroup2 here; a custom root is exotic and
/// the read simply fails closed to None).
const CGROUP_CPU_STAT: &str = "/sys/fs/cgroup/cpu.stat";

/// Deltas with counter-reset clamping: a recreated cgroup slice restarts
/// its counters at zero, and a monotonic export must never jump backwards.
fn delta_from(prev: Option<(u64, u64)>, cur: Option<(u64, u64)>) -> (u64, u64) {
    let (c, p) = (cur.unwrap_or((0, 0)), prev.unwrap_or((0, 0)));
    (c.0.saturating_sub(p.0), c.1.saturating_sub(p.1))
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cpu-budget-{}-{:?}-{}",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before unix epoch"),
            name
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("fixture dir creation");
        d
    }

    /// Write `<root>/<rel>/cpu.max` (contents may carry a trailing newline).
    fn write_cpu_max(root: &Path, rel: &str, contents: &str) -> std::path::PathBuf {
        let dir = root.join(rel.trim_matches('/'));
        fs::create_dir_all(&dir).expect("fixture dir creation");
        fs::write(dir.join("cpu.max"), contents).expect("fixture file write");
        dir
    }

    /// Write `<root>/<rel>/<name>` (v1 cfs files).
    fn write_v1(root: &Path, rel: &str, name: &str, contents: &str) -> std::path::PathBuf {
        let dir = root.join(rel.trim_matches('/'));
        fs::create_dir_all(&dir).expect("fixture dir creation");
        fs::write(dir.join(name), contents).expect("fixture file write");
        dir
    }

    // ---- v2 parsing ----

    #[test]
    fn v2_quota_parses_quota_and_period() {
        let d = fixture_dir("v2-basic");
        write_cpu_max(&d, "x", "800000 100000\n");
        assert_eq!(v2_quota_cpus(&d.join("x")), Some(8));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn v2_unbounded_is_none() {
        let d = fixture_dir("v2-max");
        write_cpu_max(&d, "x", "max 100000\n");
        assert_eq!(v2_quota_cpus(&d.join("x")), None);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn v2_fractional_ceils() {
        let d = fixture_dir("v2-frac");
        write_cpu_max(&d, "x", "450000 100000\n");
        assert_eq!(v2_quota_cpus(&d.join("x")), Some(5));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn v2_below_one_quota_floors_at_one_cpu() {
        let d = fixture_dir("v2-tiny");
        write_cpu_max(&d, "x", "90000 100000\n");
        assert_eq!(v2_quota_cpus(&d.join("x")), Some(1));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn v2_missing_file_is_none() {
        let d = fixture_dir("v2-empty");
        assert_eq!(v2_quota_cpus(&d), None);
        fs::remove_dir_all(&d).ok();
    }

    // ---- walk-up ----

    #[test]
    fn walk_takes_child_tighter_than_parent() {
        let root = fixture_dir("walk-tight");
        write_cpu_max(&root, "svc", "1600000 100000\n");
        write_cpu_max(&root, "svc/task", "600000 100000\n");
        assert_eq!(min_quota_v2(&root, Path::new("svc/task")), Some(6));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn walk_takes_parent_tighter_when_child_missing() {
        let root = fixture_dir("walk-parent");
        write_cpu_max(&root, "svc", "250000 100000\n");
        assert_eq!(min_quota_v2(&root, Path::new("svc/task")), Some(3));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn walk_container_root_path_visits_mount_root() {
        let root = fixture_dir("walk-root");
        write_cpu_max(&root, "", "800000 100000\n");
        // container case: cgroup path is "/" -> only <root> itself applies
        assert_eq!(min_quota_v2(&root, Path::new("/")), Some(8));
        fs::remove_dir_all(&root).ok();
    }

    // ---- v1 parsing ----

    #[test]
    fn v1_quota_minus_one_is_unbounded() {
        let root = fixture_dir("v1-unbounded");
        let dir = write_v1(&root, "cpu", "cpu.cfs_quota_us", "-1\n");
        write_v1(&root, "cpu", "cpu.cfs_period_us", "100000\n");
        assert_eq!(v1_quota_cpus(&dir), None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn v1_quota_ratio_ceils() {
        let root = fixture_dir("v1-ratio");
        let dir = write_v1(&root, "cpu/cpuacct", "cpu.cfs_quota_us", "450000\n");
        write_v1(&root, "cpu/cpuacct", "cpu.cfs_period_us", "100000\n");
        assert_eq!(v1_quota_cpus(&dir), Some(5));
        fs::remove_dir_all(&root).ok();
    }

    // ---- /proc/self/cgroup path extraction + mounts ----

    #[test]
    fn v2_relative_path_from_cgroup_text() {
        let (v2, v1) = cgroup_relative_paths("12:pids:/x\n0::/system.slice/docker.scope\n");
        assert_eq!(v2.as_deref(), Some("/system.slice/docker.scope"));
        assert!(v1.is_none());
    }

    #[test]
    fn v1_cpu_hierarchy_path_extracted() {
        let (v2, v1) = cgroup_relative_paths(
            "12:pids:/x\n7:cpu,cpuacct:/lxc/payload\n3:memory:/lxc/payload\n",
        );
        assert!(v2.is_none());
        assert_eq!(v1.as_deref(), Some("/lxc/payload"));
    }

    #[test]
    fn cgroup_controller_token_must_be_exact() {
        // cpuacct alone does NOT host cfs quota; only a real `cpu` token does.
        let (_, v1) = cgroup_relative_paths("4:cpuacct:/acc\n");
        assert!(v1.is_none());
    }

    #[test]
    fn cgroup2_mount_point_found() {
        let (v2, _) = cgroup_roots("proc /proc proc rw\ncgroup2 /sys/fs/cgroup cgroup2 rw\n");
        assert_eq!(v2.as_deref(), Some("/sys/fs/cgroup"));
    }

    // ---- effective budget ----

    #[test]
    fn budget_is_min_of_cgroup_and_affinity() {
        // quota says 8, affinity says 24 -> 8
        let text = "0::/\n";
        let mounts = "cgroup2 /sys/fs/cgroup cgroup2 rw\n";
        let root = fixture_dir("budget-min");
        write_cpu_max(&root, "", "800000 100000\n");
        assert_eq!(
            effective_budget_from_with_roots(text, mounts, 24, Some(&root)),
            8
        );
        // affinity tighter than quota -> affinity wins
        assert_eq!(
            effective_budget_from_with_roots(text, mounts, 4, Some(&root)),
            4
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn budget_floors_at_one() {
        let text = "0::/\n";
        let mounts = "cgroup2 /sys/fs/cgroup cgroup2 rw\n";
        let root = fixture_dir("budget-floor");
        write_cpu_max(&root, "", "1000 100000\n");
        assert_eq!(
            effective_budget_from_with_roots(text, mounts, 1, Some(&root)),
            1
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_cgroup_limit_falls_back_to_affinity() {
        let text = "0::/\n";
        let mounts = "proc /proc proc rw\n";
        assert_eq!(effective_budget_from_with_roots(text, mounts, 24, None), 24);
    }

    // ---- solve worker-count policy ----

    #[test]
    fn worker_count_is_budget_minus_headroom() {
        assert_eq!(solve_worker_count_from(None, None, 8), 6);
        assert_eq!(solve_worker_count_from(None, None, 4), 2);
    }

    #[test]
    fn worker_count_floors_at_one_under_tiny_budgets() {
        assert_eq!(solve_worker_count_from(None, None, 2), 1);
        assert_eq!(solve_worker_count_from(None, Some("0"), 1), 1);
    }

    #[test]
    fn explicit_override_beats_headroom() {
        assert_eq!(solve_worker_count_from(Some("8"), None, 8), 8);
        assert_eq!(solve_worker_count_from(Some("3"), Some("9"), 8), 3);
    }

    #[test]
    fn invalid_override_is_ignored() {
        assert_eq!(solve_worker_count_from(Some("notanumber"), None, 8), 6);
    }

    // ---- ambient I/O worker policy (SMTH6M) ----

    #[test]
    fn ambient_workers_are_the_leftover_after_solve_bins() {
        // budget 8, solve 6 -> ambient keeps the 2 headroom cores
        assert_eq!(ambient_io_worker_count_from(None, 6, 8), 2);
        assert_eq!(ambient_io_worker_count_from(None, 0, 3), 3);
    }

    #[test]
    fn ambient_workers_floor_at_one() {
        // solution override consuming the whole budget leaves 1 I/O worker
        assert_eq!(ambient_io_worker_count_from(None, 8, 8), 1);
        assert_eq!(ambient_io_worker_count_from(None, 9, 8), 1);
        assert_eq!(ambient_io_worker_count_from(None, 0, 1), 1);
    }

    #[test]
    fn ambient_override_wins_and_floors_at_one() {
        assert_eq!(ambient_io_worker_count_from(Some(4), 6, 8), 4);
        assert_eq!(ambient_io_worker_count_from(Some(0), 6, 8), 1);
    }

    // ---- [cpu-budget] boot log (SMTH6M acceptance) ----

    /// Minimal recording subscriber so the boot-log contract is asserted
    /// without a tracing-subscriber dependency in this leaf crate.
    struct RecordingSubscriber(std::sync::Mutex<String>);

    /// Field visitor that collapses one event into a `k=v ` line.
    struct LogLineVisitor<'a>(&'a mut String);

    impl tracing_core::field::Visit for LogLineVisitor<'_> {
        fn record_debug(
            &mut self,
            field: &tracing_core::field::Field,
            value: &dyn std::fmt::Debug,
        ) {
            std::fmt::write(self.0, format_args!("{}={:?} ", field.name(), value)).ok();
        }
    }

    impl tracing_core::Subscriber for RecordingSubscriber {
        fn enabled(&self, _: &tracing_core::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing_core::span::Attributes<'_>) -> tracing_core::span::Id {
            tracing_core::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Record<'_>) {}
        fn enter(&self, _: &tracing_core::span::Id) {}
        fn exit(&self, _: &tracing_core::span::Id) {}
        fn record_follows_from(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Id) {}
        fn event(&self, event: &tracing_core::event::Event<'_>) {
            let mut buf = String::new();
            event.record(&mut LogLineVisitor(&mut buf));
            if let Ok(mut s) = self.0.lock() {
                s.push_str(&buf);
            }
        }
    }

    #[test]
    fn ambient_sizing_logs_the_cpu_budget_verdict_once() {
        // First sizing call runs initialization inside a recording subscriber;
        // the line must carry the ambient worker count AND the budget/solve
        // split it was derived from, The tag is not part of the message:
        // the console derives the area from the record target (ADR-043 s7).
        let rec = std::sync::Arc::new(RecordingSubscriber(std::sync::Mutex::new(String::new())));
        let _ = tracing::subscriber::with_default(rec.clone(), ambient_io_worker_count);
        let logged = rec.0.lock().expect("log buffer").clone();
        for needle in ["ambient_io_workers", "cpu_budget", "solve_workers"] {
            assert!(logged.contains(needle), "line missing {needle}: {logged}");
        }
        // Idempotent: the OnceLock must not re-log on later calls.
        let rec2 = std::sync::Arc::new(RecordingSubscriber(std::sync::Mutex::new(String::new())));
        let _ = tracing::subscriber::with_default(rec2.clone(), ambient_io_worker_count);
        assert_eq!(rec2.0.lock().expect("log buffer 2").clone(), "");
    }

    // ---- cgroup throttle counters ----

    #[test]
    fn cpu_stat_parses_throttle_fields() {
        let stats = parse_cpu_stat(
            "usage_usec 1451708989\nuser_usec 1222927453\nsystem_usec 228781536\n\
             nice_usec 1523606\ncore_sched.force_idle_usec 0\nnr_periods 12150\n\
             nr_throttled 967\nthrottled_usec 260082264\nnr_bursts 0\nburst_usec 0\n",
        )
        .expect("stat parses");
        assert_eq!(stats.nr_throttled, 967);
        assert_eq!(stats.throttled_usec, 260_082_264);
    }

    #[test]
    fn cpu_stat_missing_fields_are_zero() {
        let stats = parse_cpu_stat("usage_usec 1\n").expect("stat parses");
        assert_eq!(stats.nr_throttled, 0);
        assert_eq!(stats.throttled_usec, 0);
    }

    #[test]
    fn cpu_stat_garbage_is_none() {
        assert!(parse_cpu_stat("not a stat file").is_none());
    }

    #[test]
    fn throttle_delta_is_zero_then_deltas() {
        // The delta tape starts at process start: first read reports zero,
        // later reads report the since-last sample.
        let (first, second, third) = (
            delta_from(Some((10, 100)), None),
            delta_from(Some((10, 100)), Some((25, 140))),
            delta_from(Some((25, 140)), Some((25, 140))),
        );
        assert_eq!(first, (0, 0));
        assert_eq!(second, (15, 40));
        assert_eq!(third, (0, 0));
    }

    #[test]
    fn counter_wraparound_on_cgroup_reset_is_zero_not_huge() {
        // cgroup counters reset when the slice is recreated: clamp at zero
        // so the exported counter never jumps backwards.
        assert_eq!(delta_from(Some((100, 999)), Some((5, 50))), (0, 0));
    }

    mod derivation {
        //! CVURM7 (epic 64ZQLA): host-shape derivation properties. Any
        //! combination of cgroup quota text and affinity must produce the
        //! documented `min(ceil(cgroup quota), affinity)` floored at 1 —
        //! never zero, never a panic, fail-closed on malformed text.
        use super::*;
        use proptest::prelude::*;

        /// v2 `cpu.max` text for optional quota units (period `100_000`).
        fn cpu_max_text(quota_units: Option<u64>) -> String {
            match quota_units {
                Some(q) => format!("{q} 100000\n"),
                None => "max 100000\n".to_owned(),
            }
        }

        proptest! {
            #![proptest_config(proptest::test_runner::Config::with_cases(256))]
            #[test]
            fn budget_is_the_documented_min_of_ceiled_cgroup_and_affinity(
                quota_units in proptest::option::of(1u64..=64_000_000u64),
                affinity in 1u64..=1024u64,
            ) {
                let affinity_usize = usize::try_from(affinity).unwrap_or(usize::MAX);
                let root = fixture_dir("prop-budget");
                write_cpu_max(&root, "", &cpu_max_text(quota_units));
                let got = effective_budget_from_with_roots(
                    "0::/\n",
                    "cgroup2 /fixture cgroup2 rw\n",
                    affinity_usize,
                    Some(&root),
                );
                let expected = match quota_units {
                    Some(q) => q.div_ceil(100_000).min(affinity).max(1),
                    None => affinity,
                };
                prop_assert_eq!(
                    got,
                    usize::try_from(expected).unwrap_or(usize::MAX)
                );
                fs::remove_dir_all(&root).ok();
                let _ = affinity_usize;
            }

            #[test]
            fn malformed_or_absent_cgroup_falls_back_to_affinity(
                junk in "[a-z =]{0,40}",
                affinity in 1u64..=1024u64,
            ) {
                // Junk on the quota line parses fail-closed: affinity budget.
                let affinity_usize = usize::try_from(affinity).unwrap_or(usize::MAX);
                let root = fixture_dir("prop-junk");
                write_cpu_max(&root, "", &format!("{junk}\n"));
                let got = effective_budget_from_with_roots(
                    "0::/\n",
                    "cgroup2 /fixture cgroup2 rw\n",
                    affinity_usize,
                    Some(&root),
                );
                prop_assert_eq!(
                    got,
                    usize::try_from(affinity).unwrap_or(usize::MAX)
                );
                // No cgroup mount at all: affinity budget.
                prop_assert_eq!(
                    effective_budget_from_with_roots(
                        "0::/\n", "proc /proc proc rw\n", affinity_usize, None
                    ),
                    usize::try_from(affinity).unwrap_or(usize::MAX)
                );
                fs::remove_dir_all(&root).ok();
            }

            #[test]
            fn solve_worker_policy_over_arbitrary_override_text(
                budget in 0usize..=1024usize,
                cpu_override in proptest::option::of("[a-z0-9]{0,4}"),
                headroom_override in proptest::option::of("[a-z0-9]{0,2}"),
            ) {
                let solve = solve_worker_count_from(
                    cpu_override.as_deref(),
                    headroom_override.as_deref(),
                    budget,
                );
                let parsed_cpu = cpu_override
                    .as_deref()
                    .and_then(|s| s.trim().parse::<usize>().ok());
                if let Some(n) = parsed_cpu {
                    // The terminal override wins outright (floored at 1).
                    prop_assert_eq!(solve, n.max(1));
                } else {
                    // Unparseable/absent override: budget minus headroom,
                    // floored at 1.
                    let headroom = headroom_override
                        .as_deref()
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(DEFAULT_SOLVE_HEADROOM);
                    prop_assert_eq!(solve, budget.saturating_sub(headroom).max(1));
                }
            }
        }
    }
}
