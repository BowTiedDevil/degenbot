//! Heavy-path capture diagnostics (from the retired grab-file dissolution,
//! ergo epic `5WCRWZ` task 1): the one-shot capture of heavy solver inputs
//! so the offline replay harnesses can be optimized against real captured
//! pool state without a full bot run.
//!
//! Extracted from the retired grab file so the diagnostic corpus stops riding the hot solve-path file:
//! its ~500 lines (writer + variants + tests) churn independently of the
//! lane walk.
use ::degenbot_solvers::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
/// Degenerate-path capture config parse (M6776W) — the owner side of the
/// `capture` config section (the gate itself reads no env). KAHU5W:
/// `gate_capture` is a typed bool (the presence-gated
/// `DEGENBOT_GATE_CAPTURE` legacy is retired; `0`/false disables).
#[must_use]
pub(crate) fn gate_capture_from_cfg(
    cfg: &::degenbot_config::BotConfig,
) -> Option<::degenbot_solvers::profit_envelope::GateCaptureCfg> {
    cfg.capture
        .gate_capture
        .then(|| ::degenbot_solvers::profit_envelope::GateCaptureCfg {
            out_path: cfg.capture.gate_capture_out.clone(),
            max_paths: u64::try_from(cfg.capture.gate_capture_cap).unwrap_or(u64::MAX),
        })
}
/// One-shot capture of heavy solver inputs, so the offline replay harnesses
/// (`int_solve_cl_path` for all-CL, `examples/mixed_solve_replay.rs` for mixed
/// V2+CL) can be optimized against real captured pool state without a full bot
/// run.
///
/// One writer owns the shared capture plumbing: the heavy gate
/// (`time_us >= MIN_US` OR `sims >= MIN_SIMS`), the 2-hop floor, path-id dedup,
/// the `DEGENBOT_SOLVER_CAPTURE_CAP` cap, parent-dir creation, and the append
/// (see [`Self::maybe_capture`]). The two variants differ only in DATA via
/// [`CaptureVariant`]: [`CaptureVariant::HeavyCl`] keeps all-CL paths and emits
/// the `IntV3TickRangeSequence` range body (default
/// `.../cl_heavy_paths.jsonl`), while [`CaptureVariant::HeavyMixed`] keeps
/// paths with >=1 V2 and >=1 CL hop and emits a `kind`-discriminated V2/CL body
/// plus `hop_order` (default `.../cl_mixed_paths.jsonl`; a
/// `DEGENBOT_SOLVER_CAPTURE_OUT` override gets a `_mixed` filename suffix so it
/// cannot clobber the all-CL fixture).
///
/// Gated by `DEGENBOT_SOLVER_CAPTURE=1` (`from_capture` yields `None`
/// otherwise).
pub(crate) struct HeavyPathCapture {
    variant: CaptureVariant,
    min_us: u64,
    min_sims: u64,
    max_captures: u64,
    out_path: std::path::PathBuf,
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
    count: std::sync::atomic::AtomicU64,
}
/// Per-variant capture facts — the ONLY thing that differs between the all-CL
/// and mixed V2+CL writers (shape filter, default/override OUT path, JSON body
/// serializer). The shared writer in [`HeavyPathCapture`] owns everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureVariant {
    /// Pure-CL paths (every hop resolves to an int sequence).
    HeavyCl,
    /// Mixed paths (>=1 V2 hop AND >=1 CL hop).
    HeavyMixed,
}
impl CaptureVariant {
    /// The production default OUT path (loop-18: working rows, never the
    /// exact-wei fixtures dir).
    fn default_out_path(self) -> std::path::PathBuf {
        let file = match self {
            Self::HeavyCl => "cl_heavy_paths.jsonl",
            Self::HeavyMixed => "cl_mixed_paths.jsonl",
        };
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../logs/solver_capture")
            .join(file)
    }
    /// Disambiguate a caller-supplied OUT path: the mixed corpus is written as
    /// a `_mixed` sibling rather than overwriting the all-CL fixture.
    fn transform_override(self, p: std::path::PathBuf) -> std::path::PathBuf {
        match self {
            Self::HeavyCl => p,
            Self::HeavyMixed => {
                let mut pb = p;
                if pb.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Some(stem) = pb.file_stem().and_then(|s| s.to_str()) {
                        pb.set_file_name(format!("{stem}_mixed.jsonl"));
                    }
                }
                pb
            }
        }
    }
    /// The path-shape filter: at least 2 hops, then the variant's hop mix.
    fn shape_matches(self, resolved: &ResolvedMixedPath) -> bool {
        if resolved.hops.len() < 2 {
            return false;
        }
        match self {
            Self::HeavyCl => resolved.hops.iter().all(|h| h.as_int_sequence().is_some()),
            Self::HeavyMixed => {
                // >=1 V2 hop AND >=1 CL hop. The all-CL capture owns pure-CL;
                // all-V2 dispatches to the closed-form Mobius solver.
                let has_v2 = resolved
                    .hops
                    .iter()
                    .any(|h| matches!(h, ResolvedHop::V2 { .. }));
                let has_cl = resolved.hops.iter().any(|h| h.as_int_sequence().is_some());
                has_v2 && has_cl
            }
        }
    }
    /// Serialize the per-hop body as `(hop_order, hops)`. `hop_order` is only
    /// emitted by the mixed variant (it drives the replay's reconstruction of
    /// the V2/CL interleave).
    fn serialize_hops(
        self,
        resolved: &ResolvedMixedPath,
    ) -> (Option<Vec<bool>>, Vec<serde_json::Value>) {
        match self {
            Self::HeavyCl => {
                // Every hop carries an int sequence (shape filter), so the `?`
                // never drops an element — it just satisfies the type checker
                // without an `unwrap`.
                let hops = resolved
                    .hops
                    .iter()
                    .filter_map(|h| Some(cl_ranges_json(h.as_int_sequence()?)))
                    .collect();
                (None, hops)
            }
            Self::HeavyMixed => {
                // Per-hop serialization: a `kind` discriminant + the hop's raw
                // fields. V2 -> reserve_in/out + gamma + fee_denom (decimal
                // strings, no alloy serde). CL -> the same
                // `IntV3TickRangeSequence.ranges` shape the all-CL fixture
                // uses, so the replay harness shares a CL-range parser.
                let hop_order = resolved
                    .hops
                    .iter()
                    .map(|h| matches!(h, ResolvedHop::V2 { .. }))
                    .collect();
                let hops = resolved
                    .hops
                    .iter()
                    .map(|h| match h {
                        ResolvedHop::V2 { state } => serde_json::json!({
                            "kind": "V2",
                            "reserve_in": state.reserve_in.to_string(),
                            "reserve_out": state.reserve_out.to_string(),
                            "gamma_numer": state.gamma_numer.to_string(),
                            "fee_denom": state.fee_denom.to_string(),
                        }),
                        ResolvedHop::V3 { int_seq, .. } | ResolvedHop::V4 { int_seq, .. } => {
                            serde_json::json!({
                                "kind": "CL",
                                "ranges": cl_ranges_json(int_seq),
                            })
                        }
                        _ => serde_json::Value::Null,
                    })
                    .collect();
                (Some(hop_order), hops)
            }
        }
    }
}
/// The 8 primitive fields of one CL tick range (big ints as decimal strings, so
/// no alloy serde) — shared by both capture variants and matching the all-CL
/// JSONL schema.
fn cl_ranges_json(seq: &::degenbot_pools::int_v3_hop::IntV3TickRangeSequence) -> serde_json::Value {
    serde_json::Value::Array(
        seq.ranges
            .iter()
            .map(|r| {
                serde_json::json!({
                    "liquidity": r.liquidity.to_string(),
                    "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                    "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                    "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                    "gamma_numer": r.gamma_numer,
                    "fee_denom": r.fee_denom,
                    "zero_for_one": r.zero_for_one,
                    "word_boundary_prices": r.word_boundary_prices
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}
impl HeavyPathCapture {
    pub(crate) fn from_capture(
        capture: &::degenbot_config::schema::CaptureConfig,
        variant: CaptureVariant,
    ) -> Option<Self> {
        capture.solver_capture.then_some(()).map(|()| Self {
            variant,
            min_us: capture.solver_capture_min_us,
            min_sims: capture.solver_capture_min_sims,
            max_captures: u64::try_from(capture.solver_capture_cap).unwrap_or(u64::MAX),
            out_path: match capture.solver_capture_out.clone() {
                Some(p) => variant.transform_override(p),
                None => variant.default_out_path(),
            },
            seen: std::sync::Mutex::new(std::collections::HashSet::new()),
            count: std::sync::atomic::AtomicU64::new(0),
        })
    }
    /// Append the captured solver input for a resolved heavy path, if it is a
    /// heavy, correctly-shaped, not-yet-captured path. The preamble — cap check,
    /// heavy gate, shape filter, dedup by path id, count, parent-dir creation,
    /// append — is ONE code path for both variants; only [`CaptureVariant`]
    /// shapes the filter and body.
    // The capture record is a flat diagnostic tuple; a params struct would
    // obscure the field-for-field mapping to the JSONL schema.
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn maybe_capture(
        &self,
        pid: u64,
        block: u64,
        micros_us: u64,
        sims: u64,
        pieces: u64,
        golden: Option<&SolvePathResult>,
        resolved: &ResolvedMixedPath,
    ) {
        if self.count.load(std::sync::atomic::Ordering::Relaxed) >= self.max_captures {
            return;
        }
        if micros_us < self.min_us && sims < self.min_sims {
            return;
        }
        if !self.variant.shape_matches(resolved) {
            return;
        }
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        if !seen.insert(pid) {
            return;
        }
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (hop_order, hops) = self.variant.serialize_hops(resolved);
        let golden_json = golden.map_or(serde_json::Value::Null, |g| {
            serde_json::json!({
                "optimal_input": g.optimal_input.to_string(),
                "profit": g.profit.to_string(),
                "hop_outputs": g.hop_outputs.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
            })
        });
        // Insertion order mirrors the two former hand-built docs (`hop_order`
        // is mixed-only); serde_json preserves it only under `preserve_order`,
        // but the map is built identically either way.
        let mut doc = serde_json::Map::new();
        doc.insert("path_id".into(), serde_json::json!(pid));
        doc.insert("block".into(), serde_json::json!(block));
        doc.insert("n_hops".into(), serde_json::json!(resolved.hops.len()));
        if let Some(order) = &hop_order {
            doc.insert("hop_order".into(), serde_json::json!(order));
        }
        doc.insert("hops".into(), serde_json::json!(hops));
        doc.insert(
            "measured".into(),
            serde_json::json!({ "time_us": micros_us, "sims": sims, "pieces": pieces }),
        );
        doc.insert("golden".into(), golden_json);
        let doc = serde_json::Value::Object(doc);
        if let Some(parent) = self.out_path.parent() {
            // The default OUT path lives under logs/solver_capture/ — a
            // directory that only exists if someone created it. A missing
            // parent previously failed every append SILENTLY (the in-process
            // `captured` counter kept advancing), losing the whole run's
            // corpus. Shared by both variants (the mixed writer's missing
            // create_dir_all was the same latent bug).
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.out_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{doc}");
        }
    }
}
#[cfg(test)]
mod heavy_path_capture_tests {
    #![expect(clippy::expect_used)] // tests assert capture gate/dedup/cap invariants
    #![expect(clippy::unwrap_used)] // tests build known-good fixtures
    use super::{CaptureVariant, HeavyPathCapture};
    use ::degenbot_config::schema::CaptureConfig;
    use ::degenbot_math::v2::IntHopState;
    use ::degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
    use ::degenbot_solvers::mixed::{ResolvedHop, ResolvedMixedPath};
    use alloy::primitives::U256;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    fn unique_stem() -> String {
        format!(
            "degenbot_capture_pin_{}_{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        )
    }
    fn temp_out() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{}.jsonl", unique_stem()))
    }
    fn cl_hop() -> ResolvedHop {
        let seq = IntV3TickRangeSequence::new(vec![IntV3TickRangeHop {
            liquidity: 1_000_000,
            sqrt_price_x96: U256::from(1u64 << 40),
            sqrt_price_lower_x96: U256::from(1u64 << 39),
            sqrt_price_upper_x96: U256::from(1u64 << 41),
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
            word_boundary_prices: Vec::new(),
        }])
        .expect("valid tick range sequence");
        ResolvedHop::V3 {
            int_seq: std::sync::Arc::new(seq),
            word_profiles: std::sync::Arc::new(Vec::new()),
            crossing_table: std::sync::Arc::new(Vec::new()),
        }
    }
    fn v2_hop() -> ResolvedHop {
        ResolvedHop::V2 {
            state: IntHopState::new(
                U256::from(1_000_000u64),
                U256::from(1_000_000u64),
                997,
                1000,
            ),
        }
    }
    fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
        ResolvedMixedPath {
            hops,
            valid: true,
            ..Default::default()
        }
    }
    fn shapes() -> [(CaptureVariant, Vec<ResolvedHop>); 2] {
        [
            (CaptureVariant::HeavyCl, vec![cl_hop(), cl_hop()]),
            (CaptureVariant::HeavyMixed, vec![v2_hop(), cl_hop()]),
        ]
    }
    fn cfg(out: std::path::PathBuf) -> CaptureConfig {
        CaptureConfig {
            solver_capture: true,
            solver_capture_out: Some(out),
            solver_capture_min_us: 50_000,
            solver_capture_min_sims: 2_000,
            solver_capture_cap: 16,
            ..Default::default()
        }
    }
    fn lines(out: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(out)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("valid JSONL row"))
            .collect()
    }
    /// The heavy gate (`time_us >= MIN_US` OR `sims >= MIN_SIMS`) is ONE code
    /// path for both variants: below BOTH thresholds nothing is written; at or
    /// above EITHER threshold the record lands.
    #[test]
    fn heavy_gate_is_identical_across_variants() {
        for (variant, hops) in shapes() {
            let out = temp_out();
            let cap = HeavyPathCapture::from_capture(&cfg(out.clone()), variant).expect("gated on");
            let out = cap.out_path.clone();
            let resolved = path(hops);
            cap.maybe_capture(1, 10, 0, 0, 0, None, &resolved);
            assert!(
                lines(&out).is_empty(),
                "{variant:?}: below-gate row was not refused"
            );
            cap.maybe_capture(2, 10, 0, 2_000, 0, None, &resolved); // sims gate
            cap.maybe_capture(3, 10, 50_000, 0, 0, None, &resolved); // us gate
            assert_eq!(
                lines(&out).len(),
                2,
                "{variant:?}: heavy gate drifted between variants"
            );
            let _ = std::fs::remove_file(&out);
        }
    }
    /// The shape filter is variant DATA fed to the shared writer: each variant
    /// refuses the other's shape and keeps its own, and the mixed body carries
    /// `hop_order` while the all-CL body does not.
    #[test]
    fn shape_filter_is_variant_data() {
        let all_cl = path(vec![cl_hop(), cl_hop()]);
        let mixed = path(vec![v2_hop(), cl_hop()]);
        let out = temp_out();
        let cap =
            HeavyPathCapture::from_capture(&cfg(out.clone()), CaptureVariant::HeavyCl).unwrap();
        cap.maybe_capture(1, 1, 60_000, 0, 0, None, &mixed);
        cap.maybe_capture(2, 1, 60_000, 0, 0, None, &all_cl);
        let l = lines(&cap.out_path);
        assert_eq!(l.len(), 1, "all-CL writer must keep only the all-CL shape");
        assert_eq!(l[0]["path_id"], serde_json::json!(2));
        assert!(
            l[0].get("hop_order").is_none(),
            "all-CL body has no hop_order"
        );
        let out2 = temp_out();
        let cap2 =
            HeavyPathCapture::from_capture(&cfg(out2.clone()), CaptureVariant::HeavyMixed).unwrap();
        cap2.maybe_capture(1, 1, 60_000, 0, 0, None, &all_cl);
        cap2.maybe_capture(2, 1, 60_000, 0, 0, None, &mixed);
        let l2 = lines(&cap2.out_path);
        assert_eq!(l2.len(), 1, "mixed writer must keep only the mixed shape");
        assert_eq!(l2[0]["path_id"], serde_json::json!(2));
        assert_eq!(l2[0]["hop_order"], serde_json::json!([true, false]));
        let _ = std::fs::remove_file(&cap.out_path);
        let _ = std::fs::remove_file(&cap2.out_path);
    }
    /// Dedup-by-path-id and the `MAX_CAPTURES` cap are the shared writer's
    /// preamble: identical semantics for both variants.
    #[test]
    fn dedup_and_cap_are_identical_across_variants() {
        for (variant, hops) in shapes() {
            let out = temp_out();
            let mut c = cfg(out.clone());
            c.solver_capture_cap = 1;
            let cap = HeavyPathCapture::from_capture(&c, variant).expect("gated on");
            let out = cap.out_path.clone();
            let resolved = path(hops);
            cap.maybe_capture(7, 1, 60_000, 0, 0, None, &resolved);
            cap.maybe_capture(7, 1, 60_000, 0, 0, None, &resolved); // dedup
            cap.maybe_capture(8, 1, 60_000, 0, 0, None, &resolved); // cap
            assert_eq!(
                lines(&out).len(),
                1,
                "{variant:?}: dedup/cap drifted between variants"
            );
            let _ = std::fs::remove_file(&out);
        }
    }
    /// OUT-path handling is variant data: all-CL honors the override verbatim,
    /// mixed writes a _mixed sibling so it cannot clobber the all-CL fixture.
    #[test]
    fn out_path_override_is_variant_data() {
        let base = temp_out();
        let cl = HeavyPathCapture::from_capture(&cfg(base.clone()), CaptureVariant::HeavyCl)
            .expect("gated on");
        assert_eq!(cl.out_path, base);
        let mixed = HeavyPathCapture::from_capture(&cfg(base.clone()), CaptureVariant::HeavyMixed)
            .expect("gated on");
        let mut expected = base.clone();
        expected.set_file_name(format!(
            "{}_mixed.jsonl",
            base.file_stem().and_then(|s| s.to_str()).unwrap()
        ));
        assert_eq!(mixed.out_path, expected);
    }
    /// The shared append preamble creates a missing parent dir for BOTH
    /// variants (the mixed writer previously lacked this and dropped rows
    /// silently on a missing parent).
    #[test]
    fn shared_writer_creates_missing_parent_for_both_variants() {
        for (variant, hops) in shapes() {
            let dir = std::env::temp_dir().join(format!("{}_nested", unique_stem()));
            let out = dir.join("deep").join("out.jsonl");
            let cap = HeavyPathCapture::from_capture(&cfg(out.clone()), variant).expect("gated on");
            let out = cap.out_path.clone();
            cap.maybe_capture(1, 1, 60_000, 0, 0, None, &path(hops));
            assert!(
                out.exists(),
                "{variant:?}: shared writer did not create the parent dir"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
