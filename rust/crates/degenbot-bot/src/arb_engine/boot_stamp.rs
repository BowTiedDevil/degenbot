//! THE construction-stamped fleet boot carrier (YI5NGB, epic 64ZQLA).
//!
//! `ArbitrageEngine::with_core_cfg` derives its `FleetBoot` from the
//! CALLER's own config (the KAHU5W trajectory completed for the boot path)
//! and packs it as a `BootStamp`: the boot value PLUS the constructing
//! engine's identity (`engine_id`) and a deterministic hash of the boot
//! (`cfg_hash`). The per-role `OnceLock<BootStamp>` statics in the three
//! fleet executors are now identified COURIERS, not anonymous stance
//! carriers: the first construction wins the fleet materialization, every
//! later construction RIDES, and a divergent-cfg ride is counted in prod
//! (warn log per winner/rider pair) and ILLEGAL in tests (a `panic!`).
//!
//! The `cfg_hash` recipe is FNV-1a over the boot's fields with the f64
//! quota folded in BIT-EXACTLY via `f64::to_bits` — never a
//! `format!("{:?}")`/Debug byte stream (float Debug padding is not a
//! contract and would make equal boots hash differently across rustc
//! versions).
//!
//! R3 (the design's accepted floor): an FNV-1a collision between two
//! DIFFERENT boots would misclassify a divergent rider as identical
//! (silencing a warn, never firing a false positive); the probability is
//! about 2^-64 per pair — documented here as the accepted floor.
use degenbot_core::op_warn;
use degenbot_workers::dispatcher::FleetBoot;
use std::sync::atomic::{AtomicU64, Ordering};
/// The fleet roles a boot stamps (one ledger row per role, first writer
/// wins). The census/thread-names identity stays KEYED BY ROLE (V5) —
/// the ledger's rows are keyed the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootRole {
    /// The Solver-role fleet (solve bins).
    Solve,
    /// The SimDriver-role fleet (inline sims).
    Sim,
    /// The PoolStateUpdater-role fleet (registration intake).
    Registration,
}
impl BootRole {
    /// Ledger label (the census resource string verbatim, for grep-ability).
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Solve => "fleet_solver_slots",
            Self::Sim => "fleet_simdriver_slots",
            Self::Registration => "fleet_pool_state_updater_slots",
        }
    }
}
/// A construction-stamped fleet boot: the boot value PLUS who constructed
/// it (`engine_id`) and what it hashed to (`cfg_hash`). NOT `Copy` — the
/// ledger's rows own a `Vec` of rides, so the stamp moves once and the
/// three `install_boot` call sites clone explicitly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BootStamp {
    /// The boot the constructing engine derived from ITS OWN cfg.
    pub(crate) boot: FleetBoot,
    /// Monotonic per-process engine construction counter (NEVER used for
    /// ordering decisions — the ledger only ever compares cfg hashes).
    pub(crate) engine_id: u64,
    /// FNV-1a over the boot fields — deterministic across hosts and
    /// builds; equal boots hash equal.
    pub(crate) cfg_hash: u64,
}
/// Monotonic engine construction counter (the stamp's identity half).
static ENGINE_SEQ: AtomicU64 = AtomicU64::new(0);
/// Fold one byte buffer into the FNV-1a state.
fn fnv_mix(state: u64, bytes: &[u8]) -> u64 {
    let mut state = state;
    let mut i = 0;
    while i < bytes.len() {
        state ^= u64::from(bytes[i]);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    state
}
/// FNV-1a 64-bit over the boot's fields, the f64 quota folded in
/// BIT-EXACTLY via `f64::to_bits` (never a Debug-format byte stream).
fn fnv1a_boot(boot: &FleetBoot) -> u64 {
    let mut state = fnv_mix(
        0xcbf2_9ce4_8422_2325,
        &boot.quota_cpus.to_bits().to_le_bytes(),
    );
    // overrides: the Option<u64> tags fold the discriminant in (None -> 0,
    // Some -> 1) so a None never collides with a zero value.
    let overrides_64 = [
        boot.overrides.reserve_cpus,
        boot.overrides.ambient_io_workers,
        boot.overrides.solver_cpus,
    ];
    let overrides_usize = [
        boot.overrides.sim_slot_cap,
        boot.overrides.pool_state_updater_slots,
        boot.overrides.solve_headroom,
    ];
    let mut i = 0;
    while i < overrides_64.len() {
        state = fnv_mix(state, &u64::from(overrides_64[i].is_some()).to_le_bytes());
        state = fnv_mix(state, &overrides_64[i].unwrap_or(0).to_le_bytes());
        i += 1;
    }
    i = 0;
    while i < overrides_usize.len() {
        state = fnv_mix(
            state,
            &u64::from(overrides_usize[i].is_some()).to_le_bytes(),
        );
        state = fnv_mix(state, &overrides_usize[i].unwrap_or(0).to_le_bytes());
        i += 1;
    }
    // posture: every threshold, the f64 duty percent bit-exact.
    state = fnv_mix(state, &(boot.posture.enter_events as u64).to_le_bytes());
    state = fnv_mix(state, &boot.posture.enter_window_ms.to_le_bytes());
    state = fnv_mix(state, &boot.posture.duty_percent.to_bits().to_le_bytes());
    state = fnv_mix(state, &boot.posture.duty_window_ms.to_le_bytes());
    state = fnv_mix(state, &boot.posture.exit_clean_ms.to_le_bytes());
    state = fnv_mix(
        state,
        &u64::from(boot.posture.sim_intake_floor_override.is_some()).to_le_bytes(),
    );
    state = fnv_mix(
        state,
        &(boot.posture.sim_intake_floor_override.unwrap_or(0) as u64).to_le_bytes(),
    );
    // FF-T2 (MEBF4V): the fleet profile folds in as the enum discriminant
    // (auto | pinned | serial) — a forced profile is a DIFFERENT boot.
    state = fnv_mix(
        state,
        &[match boot.profile {
            degenbot_config::FleetProfile::Auto => 0_u8,
            degenbot_config::FleetProfile::Pinned => 1,
            degenbot_config::FleetProfile::Serial => 2,
        }],
    );
    state
}
impl BootStamp {
    /// Stamp a construction boot: the NEXT engine id + the deterministic
    /// boot hash. Called by `with_core_cfg` (the packer) only.
    #[must_use]
    pub(crate) fn of(boot: FleetBoot) -> Self {
        Self {
            engine_id: ENGINE_SEQ.fetch_add(1, Ordering::Relaxed),
            cfg_hash: fnv1a_boot(&boot),
            boot,
        }
    }
    /// The stamp's boot value (`install_boot` hands this to the executor).
    #[must_use]
    pub(crate) fn boot(&self) -> FleetBoot {
        self.boot
    }
    /// Identical-boot check: the ledger's ONLY divergence test (R2/R3 —
    /// `engine_id` is identity, never a comparison key). Read by the
    /// F-suite's white-box probes (test builds).
    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub(crate) fn same_cfg(&self, other: &Self) -> bool {
        self.cfg_hash == other.cfg_hash
    }
}
/// One fleet-boot ledger row: the role label, the winning stamp's engine
/// id + cfg hash, and the identical-cfg riders that later installed the
/// same boot.
type Ride = (u64, u64);
type LedgerRow = (&'static str, u64, u64, Vec<Ride>);
/// The boot-audit ledger (the honesty molecule): first writer per role
/// records the BOOT row; every later construction records a RIDE —
/// silently when byte-identical (a legal rider), counted+warned in prod
/// and `panic!`-illegal in tests when the cfg hash DIVERGES.
static LEDGER: parking_lot::Mutex<Vec<LedgerRow>> = parking_lot::Mutex::new(Vec::new());
/// Record a construction's fleet-boot install for `role` (called at each
/// `install_boot`'s head). Divergent-cfg rides: prod = count + one warn;
/// test = ILLEGAL (a `panic!` naming both engine ids + cfg hashes).
pub(crate) fn record_ride(role: BootRole, stamp: &BootStamp) {
    let mut rows = LEDGER.lock();
    match rows.iter_mut().find(|(r, ..)| *r == role.label()) {
        Some((_, _, winner_cfg, rides)) if *winner_cfg == stamp.cfg_hash => {
            rides.push((stamp.engine_id, stamp.cfg_hash)); // identical-boot rider: legal, silent
        }
        Some((_, winner_engine, winner_cfg, rides)) => {
            // DIFFERENT-cfg rider: prod = count + one warn per pair; test =
            // ILLEGAL by construction (the F2 fire drill).
            rides.push((stamp.engine_id, stamp.cfg_hash));
            op_warn!(
                domain = solver,
                role = role.label(),
                winner_engine = winner_engine,
                winner_cfg = format_args!("{winner_cfg:016x}"),
                rider_engine = stamp.engine_id,
                rider_cfg = format_args!("{:016x}", stamp.cfg_hash),
                "mixed-boot fleet ride (YI5NGB): first-fleet-wins, rider cfg diverges"
            );
            #[cfg(test)]
            panic_mixed_boot_ride_illegal_in_tests(*winner_cfg, stamp);
        }
        None => rows.push((role.label(), stamp.engine_id, stamp.cfg_hash, Vec::new())),
    }
}
/// The test-build arm of the ledger (YI5NGB): a divergent-cfg ride is
/// ILLEGAL in tests by construction — the F2 fire drill. Prod builds never
/// compile this (the warn + the ledger row carry the whole story there).
#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "in test builds a mixed-boot ride is ILLEGAL by construction (the YI5NGB design's F2 fire drill)"
)]
fn panic_mixed_boot_ride_illegal_in_tests(winner_cfg: u64, stamp: &BootStamp) {
    panic!(
        "(YI5NGB) mixed-boot rides are ILLEGAL in tests: engine {} (cfg_hash {winner_cfg:016x} won) rode a fleet booted for a different cfg (rider cfg_hash {:016x})",
        stamp.engine_id,
        stamp.cfg_hash
    );
}
#[cfg(all(test, not(miri)))]
#[expect(clippy::expect_used)]
mod tests {
    use super::{fnv1a_boot, BootRole, BootStamp, LEDGER};
    use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
    use alloy::primitives::Address;
    use degenbot_config::BotConfigLoader;
    use degenbot_workers::budget::BudgetOverrides;
    use degenbot_workers::dispatcher::FleetBoot;
    use degenbot_workers::posture::{PostureOwner, PosturePolicy};
    use hashbrown::HashSet;
    const GAMMA_03_TEST: u64 = 997;
    const FEE_DENOM_03_TEST: u64 = 1_000;
    fn usdc_test(amount: u64) -> alloy::primitives::aliases::U112 {
        use alloy::primitives::{aliases::U112, U256};
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }
    fn weth_test(amount: u64) -> alloy::primitives::aliases::U112 {
        use alloy::primitives::{aliases::U112, U256};
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }
    fn hermetic_boot() -> FleetBoot {
        FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 8.0,
            overrides: BudgetOverrides {
                sim_slot_cap: Some(4),
                ..BudgetOverrides::default()
            },
            posture: PosturePolicy::doc_defaults(),
            // A fresh hermetic owner per boot — never the process global
            // (7KAPBB isolation; these tests never boot a host anyway).
            owner: Some(std::boxed::Box::leak(std::boxed::Box::new(
                PostureOwner::new(PosturePolicy::doc_defaults()),
            ))),
        }
    }
    /// The hash is deterministic for byte-equal boots and diverges for the
    /// divergence knob — the F2/F3 premise (R3's accepted 2^-64 floor).
    #[test]
    fn cfg_hash_is_deterministic_and_diverges_on_quota() {
        let base = hermetic_boot();
        assert_eq!(
            fnv1a_boot(&base),
            fnv1a_boot(&base),
            "byte-equal boots hash equal"
        );
        let mut other = base;
        other.quota_cpus = 4.5;
        let base_hash = fnv1a_boot(&hermetic_boot());
        assert_ne!(base_hash, fnv1a_boot(&other), "a quota change must re-hash");
        let stamp = BootStamp::of(hermetic_boot());
        assert_eq!(
            fnv1a_boot(&hermetic_boot()),
            fnv1a_boot(&stamp.boot()),
            "the stamp carries the value it hashed"
        );
    }
    /// Equal-boot stamps are a legal ride (`same_cfg`); divergent boots
    /// are not — the F2/F3 discriminator is the HASH, never the engine id.
    #[test]
    fn same_cfg_follows_the_hash_not_the_engine() {
        let a = BootStamp::of(hermetic_boot());
        let b = BootStamp::of(hermetic_boot());
        assert!(a.same_cfg(&b), "byte-equal boots ride legal");
        assert_ne!(a.engine_id, b.engine_id, "distinct constructions");
        let mut other = hermetic_boot();
        other.quota_cpus = 2.0;
        let c = BootStamp::of(other);
        assert!(!a.same_cfg(&c), "a divergent cfg is NOT a legal ride");
    }
    /// F2 (YI5NGB): the mixed-cfg fire drill — a SECOND engine's
    /// construction with a DIVERGENT cfg must make the ride ILLEGAL in
    /// tests.
    ///
    /// The divergence knob must diverge on EVERY host tier: a pure
    /// `fleet.quota_cpus` override can hash byte-equal to the default
    /// construction on a host whose detected quota IS that value (a
    /// 4-vCPU CI runner detects exactly 4.0, making the quota-only ride
    /// legal and the fire drill mute). The `runtime.fleet_profile=pinned`
    /// fold is tier-proof — a schema-default boot is always `Auto` — and
    /// `fleet.quota_cpus=4.0` rides along for the original intent.
    ///
    /// Route (the REV 2 contract): engine B goes through
    /// `ArbitrageEngine::with_core_cfg` DIRECTLY with a LOCALLY
    /// loader-built config (`BotConfigLoader::new().without_env()` + the
    /// CLI overrides) — never through the config holder (no holder- or
    /// stance-side `install` call of any name) and no default-installing
    /// `with_core` construction anywhere between the snapshots: the
    /// divergent value stays a LOCAL construction fact and cannot leak
    /// into any parallel or later construction (R10).
    #[test]
    fn mixed_cfg_ride_is_illegal_in_tests() {
        use crate::bot_core::state_lock::StateLock;
        use crate::bot_core::BotState;
        use std::sync::Arc;
        // Non-leak snapshot #1: the process holder is (still) untouched.
        let installed_before = degenbot_config::holder::installed();
        // Engine A: the default-cfg construction (leases its cfg from the
        // holder's schema DEFAULTS; never installs anything).
        let engine_a = ArbitrageEngine::new();
        assert!(
            !degenbot_config::holder::installed(),
            "an engine construction must never install the process holder"
        );
        // The DIVERGENT cfg, built from EXPLICIT LOCAL sources only (no
        // env layer, no file layer, no process-global read anywhere in
        // the chain).
        let loaded = BotConfigLoader::new()
            .without_env()
            .with_cli("fleet.quota_cpus", "4.0")
            // The tier-proof divergence knob (see the F2 doc): a default
            // boot is profile-Auto on every host, so profile=Pinned cannot
            // byte-match the winner's hash anywhere.
            .with_cli("runtime.fleet_profile", "pinned")
            .load()
            .expect("the divergent cfg must load from explicit sources");
        let cfg_b = Arc::new(loaded.config);
        // Engine B: constructed DIRECTLY with the local cfg (its
        // construction's install_boot records a DIVERGENT-cfg ride — in
        // test builds that is a panic).
        let cfg_ref = Arc::clone(&cfg_b);
        let ride = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let core = Arc::new(StateLock::new(BotState::new()));
            let _engine_b = ArbitrageEngine::with_core_cfg(core, &cfg_ref);
        }));
        // Non-leak snapshot #2, AFTER the ride.
        let installed_after = degenbot_config::holder::installed();
        let err = ride.expect_err("a divergent-cfg ride must be ILLEGAL in tests");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&str>().copied())
            .expect("panic payload is the ledger panic");
        assert!(
            msg.contains("mixed-boot rides are ILLEGAL in tests") && msg.contains("(YI5NGB)"),
            "the panic must name the audit + the task: {msg}"
        );
        assert_eq!(
            installed_before, installed_after,
            "the divergent cfg must NEVER cross the process-global holder boundary"
        );
        // White-box (order-independent): ledger rows exist whose recorded
        // RIDE diverges from their BOOT winner — B's divergent-cfg install
        // was both recorded AND loud before the panic caught it.
        let rows = LEDGER.lock();
        let divergent_rides: usize = rows
            .iter()
            .map(|(_, _, winner_cfg, rides)| {
                rides.iter().filter(|&&(_, h)| h != *winner_cfg).count()
            })
            .sum();
        assert!(
            divergent_rides >= 1,
            "the divergent-cfg ride must be recorded in the ledger before the panic"
        );
        assert_eq!(
            engine_a.fleet_boot_stamp().boot(),
            degenbot_workers::dispatcher::FleetBoot::from_config(
                &<::degenbot_config::BotConfig as Default>::default()
            ),
            "engine A's construction boot is the schema-default boot (the divergent cfg stayed local to B)"
        );
    }
    /// F3 (YI5NGB): the positive control — a second engine with a
    /// byte-IDENTICAL boot is a legal, silent rider: no panic, the shared
    /// fleet materializes exactly ONCE (`std::ptr::eq` on the `&'static`
    /// handles across A's and B's submits), and the ledger holds ONE boot
    /// row whose rides are all identical-cfg.
    #[expect(
        clippy::too_many_lines,
        reason = "the F3 positive control carries the whole legal-ride story in one deterministic body (twin stamp asserts + materialization identity + ledger probe)"
    )]
    #[test]
    fn same_cfg_ride_stays_legal() {
        use crate::bot_core::state_lock::StateLock;
        use crate::bot_core::BotState;
        use degenbot_solvers::mixed::PoolHop;
        use std::sync::Arc;
        let installed_before = degenbot_config::holder::installed();
        // Both engines: byte-identical default cfgs (the holder's schema
        // defaults) — the prod-twin shape.
        let mut engine_a = ArbitrageEngine::new();
        let core_b = Arc::new(StateLock::new(BotState::new()));
        let mut engine_b = ArbitrageEngine::with_core_cfg(
            core_b,
            &Arc::new(<::degenbot_config::BotConfig as Default>::default()),
        );
        // White-box: the twins' stamps hash EQUAL (a legal ride by
        // construction) while the engine identities stay distinct.
        assert!(
            engine_a
                .fleet_boot_stamp()
                .same_cfg(engine_b.fleet_boot_stamp()),
            "byte-identical twin constructions must produce identical cfg hashes"
        );
        assert_ne!(
            engine_a.fleet_boot_stamp().engine_id,
            engine_b.fleet_boot_stamp().engine_id,
            "two distinct construction identities over one shared boot value"
        );
        let hub_a = engine_a.register_v2_pool(
            Address::from([0xAA_u8; 20]),
            usdc_test(1_000_000),
            weth_test(700),
            GAMMA_03_TEST,
            FEE_DENOM_03_TEST,
        );
        let hub_b = engine_a.register_v2_pool(
            Address::from([0xBB_u8; 20]),
            weth_test(900),
            usdc_test(1_200_000),
            GAMMA_03_TEST,
            FEE_DENOM_03_TEST,
        );
        // Two hops: the registration gate refuses structurally unroutable
        // single-hop paths (the A/B fixture's shape).
        let path_a = engine_a
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: hub_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: hub_b,
                    zero_for_one: false,
                },
            ])
            .expect("two-hop path registers over the default-cfg core");
        let b_hub_a = engine_b.register_v2_pool(
            Address::from([0xAA_u8; 20]),
            usdc_test(1_000_000),
            weth_test(700),
            GAMMA_03_TEST,
            FEE_DENOM_03_TEST,
        );
        let b_hub_b = engine_b.register_v2_pool(
            Address::from([0xBB_u8; 20]),
            weth_test(900),
            usdc_test(1_200_000),
            GAMMA_03_TEST,
            FEE_DENOM_03_TEST,
        );
        let path_b = engine_b
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: b_hub_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: b_hub_b,
                    zero_for_one: false,
                },
            ])
            .expect("two-hop path registers over the twin core");
        // Both engines drive a solve: A's first solve MATERIALIZES the
        // fleet; B's rides it. The `&'static` handle identity across the
        // two submits PROVES the single shared fleet.
        engine_a.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &HashSet::from([hub_a, hub_b]),
                &HashSet::new(),
                &HashSet::new(),
            ),
            500,
            &BlockMetadata::default(),
            &engine_a.registry,
            &mut engine_a.delivery,
        );
        let handle_after_a = crate::arb_engine::fleet_solve_executor::global_fleet_solve_executor();
        engine_b.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &HashSet::from([b_hub_a, b_hub_b]),
                &HashSet::new(),
                &HashSet::new(),
            ),
            501,
            &BlockMetadata::default(),
            &engine_b.registry,
            &mut engine_b.delivery,
        );
        let handle_after_b = crate::arb_engine::fleet_solve_executor::global_fleet_solve_executor();
        assert!(
            std::ptr::eq(handle_after_a, handle_after_b),
            "the fleet must materialize EXACTLY ONCE: engine B must ride the shared executor"
        );
        let installed_after = degenbot_config::holder::installed();
        assert_eq!(
            installed_before, installed_after,
            "the legal twin must NEVER touch the process holder either"
        );
        // White-box (scoped to OUR twin: the ledger is process-shared and a
        // parallel F-suite stranger may co-record, so the row-level purity
        // claim is made for THIS test's engine B only — its (engine_id,
        // cfg_hash) pair is unique to F3's construction).
        let my_stamp = engine_b.fleet_boot_stamp();
        let rows = LEDGER.lock();
        let solve_row = rows
            .iter()
            .find(|(r, ..)| *r == BootRole::Solve.label())
            .expect("the Solve role boot row exists (engine A materialized)");
        assert!(
            solve_row.3
                .iter()
                .any(|&(id, h)| id == my_stamp.engine_id && h == my_stamp.cfg_hash),
            "engine B's construction rode the Solve static as an IDENTICAL-cfg rider (silent-legal): {my_stamp:?}",
        );
        let _ = (path_a, path_b);
    }
}
