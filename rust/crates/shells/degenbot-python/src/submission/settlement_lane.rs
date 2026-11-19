//! The settlement lane's host registration: the lane one engine boot minted,
//! filed under that host's own lane namespace in the process's lane registry.
//!
//! The settlement submit seam does not reach for a lane; it resolves the
//! registration its engine filed, and only for the operator account the signing
//! key proves. The registry is process-wide because the operator account is —
//! two engines signing one key must not each keep a private occupancy map for
//! it — while the filing's stake, not the registry, is what a lookup names, so
//! one host's lane is never a fallback for another's. The stake held here is
//! the filing's lifetime: dropping the engine removes exactly this host's
//! registration.

use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_submission::{LaneRefusal, LaneScope, LaneStake, SettlementLanes, SubmissionLedger};
use degenbot_substrate::driver_spawn::LaneNamespace;
use degenbot_substrate::nonce::NonceLane;

/// The process's one nonce-lane registry: the composition root every engine
/// boot files its lanes into.
///
/// The registry is process-scoped because the operator account is: two engines
/// that sign with the same key must not each keep a private occupancy map for
/// it. It is *not* a lookup shortcut — nothing resolves a lane through this
/// function. An engine asks for the registry at boot, files its lane under its
/// own [`LaneScope`], and hands every submission through that filing, so a lane
/// stays reachable only from the host that declared it.
pub(crate) fn process_lanes() -> Arc<SettlementLanes> {
    static LANES: std::sync::OnceLock<Arc<SettlementLanes>> = std::sync::OnceLock::new();
    Arc::clone(LANES.get_or_init(|| Arc::new(SettlementLanes::new())))
}

/// The settlement lane one host booted, filed under the host's lane namespace.
#[derive(Debug)]
pub(crate) struct SettlementLaneHome {
    lanes: Arc<SettlementLanes>,
    /// The filing: the lookup handle and the registration's lifetime. Dropped
    /// with the host, removing only this registration and never a peer's.
    stake: LaneStake<SubmissionLedger>,
}

impl SettlementLaneHome {
    /// File the boot's settlement `lane` into `lanes` — the process's lane
    /// registry — under `namespace`, the host's lane namespace for the
    /// settlement strategy (or `None` when the boot installed no state root).
    pub(crate) fn boot(
        lanes: Arc<SettlementLanes>,
        namespace: Option<LaneNamespace>,
        lane: Arc<NonceLane<SubmissionLedger>>,
    ) -> Self {
        let scope = LaneScope::new(namespace, lane.strategy().clone());
        let stake = lanes.install(scope, lane);
        Self { lanes, stake }
    }

    /// The settlement lane for one submission from `account`, seeded from the
    /// submission-time chain read so its first stamp never re-issues a nonce
    /// the chain has already consumed.
    ///
    /// `claims` states whether this host may issue a nonce through the lane: a
    /// submitting host claims the operator account (so one account never gets
    /// two authorities), while a dry-run host never issues and so never claims
    /// — it neither contends for the account nor blocks a live host.
    ///
    /// # Errors
    ///
    /// [`LaneRefusal::NotRegistered`] when this host's filing is gone (another
    /// host's filing is never a fallback), [`LaneRefusal::AccountBound`] when
    /// the lane signs for a different operator account, and
    /// [`LaneRefusal::AccountOwned`] when another lane in the process already
    /// stamps this account — one account has one authority.
    pub(crate) fn lane(
        &self,
        account: Address,
        operator_nonce: u64,
        claims: bool,
    ) -> Result<Arc<NonceLane<SubmissionLedger>>, LaneRefusal> {
        let lane = if claims {
            self.lanes.resolve(&self.stake, account)?
        } else {
            self.lanes.lane(&self.stake)?
        };
        lane.authority().observe_confirmed(operator_nonce);
        Ok(lane)
    }

    /// The host's lane registry (the boot's own tests read what it filed).
    #[cfg(test)]
    pub(crate) fn lanes(&self) -> &Arc<SettlementLanes> {
        &self.lanes
    }

    /// The scope this host filed its lane under.
    #[cfg(test)]
    pub(crate) fn scope(&self) -> &LaneScope {
        self.stake.scope()
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test assertions fail loudly")]
mod tests {
    use super::*;
    use degenbot_substrate::nonce::{HoldWindow, LanePolicy, NonceAuthority, StrategyId};

    fn lane() -> Arc<NonceLane<SubmissionLedger>> {
        let authority = Arc::new(NonceAuthority::new(0));
        Arc::new(authority.mint_lane(
            StrategyId::new("settlement"),
            Arc::new(SubmissionLedger::new()),
            LanePolicy::new(HoldWindow::DEFAULT, "settlement"),
        ))
    }

    fn namespace(root: &str) -> LaneNamespace {
        LaneNamespace::under(root, &StrategyId::new("settlement"))
    }

    /// The boot files the lane it minted under the host's namespace, and the
    /// seam resolves that very lane.
    #[test]
    fn a_booted_host_resolves_the_lane_it_filed() {
        let filed = lane();
        let home = SettlementLaneHome::boot(
            process_lanes(),
            Some(namespace("/state")),
            Arc::clone(&filed),
        );
        let resolved = home
            .lane(Address::repeat_byte(0x01), 11, true)
            .expect("the host resolves its own lane");
        assert!(
            Arc::ptr_eq(&resolved, &filed),
            "the boot's lane is the one the seam stamps through"
        );
        assert_eq!(
            resolved.authority().confirmed(),
            11,
            "the submission-time chain read seeds the authority"
        );
    }

    /// The submission-time chain read seeds the authority forward-only: a stale
    /// read never rewinds the confirmed nonce.
    #[test]
    fn a_stale_chain_read_never_rewinds_the_authority() {
        let lanes = Arc::new(SettlementLanes::new());
        let home = SettlementLaneHome::boot(lanes, Some(namespace("/state")), lane());
        let lane = home
            .lane(Address::repeat_byte(0x01), 11, true)
            .expect("resolve");
        home.lane(Address::repeat_byte(0x01), 10, true)
            .expect("a stale read still resolves");
        assert_eq!(lane.authority().confirmed(), 11);
    }

    /// Two engines booted over one registry — the production shape — sign
    /// distinct accounts through their own filings, and a second engine that
    /// filters its own lane for an account another engine already stamps is
    /// refused instead of silently minting a second authority.
    #[test]
    fn one_registry_keeps_distinct_accounts_apart_and_one_account_on_one_authority() {
        let lanes = Arc::new(SettlementLanes::new());
        let first = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());
        let second =
            SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());

        let third = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());

        first
            .lane(Address::repeat_byte(0x0a), 0, true)
            .expect("the first host binds its own account");
        second
            .lane(Address::repeat_byte(0x0b), 0, true)
            .expect("the second host's distinct account is untouched");
        assert_eq!(
            third.lane(Address::repeat_byte(0x0a), 0, true).err(),
            Some(LaneRefusal::AccountOwned {
                account: Address::repeat_byte(0x0a)
            }),
            "one account has one authority across the process"
        );
    }

    /// The filed lane signs for the one operator account the seam proved: a
    /// second account is refused rather than sharing one nonce space.
    #[test]
    fn a_lane_signs_for_one_operator_account() {
        let lanes = Arc::new(SettlementLanes::new());
        let home = SettlementLaneHome::boot(lanes, Some(namespace("/state")), lane());
        home.lane(Address::repeat_byte(0x01), 0, true)
            .expect("the first account binds");
        assert_eq!(
            home.lane(Address::repeat_byte(0x02), 0, true).err(),
            Some(LaneRefusal::AccountBound {
                bound: Address::repeat_byte(0x01)
            })
        );
    }

    /// Teardown removes the registration this host filed and leaves a peer
    /// engine's filing over the same registry intact.
    #[test]
    fn teardown_removes_only_the_hosts_own_filing() {
        let lanes = Arc::new(SettlementLanes::new());
        let home = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());
        let peer = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());
        let scope = home.scope().clone();
        assert_eq!(lanes.scopes(), vec![scope.clone(), scope.clone()]);

        drop(home);

        assert_eq!(
            lanes.scopes(),
            vec![scope],
            "the teardown removed only the filing its stake owned"
        );
        peer.lane(Address::repeat_byte(0x02), 0, true)
            .expect("the peer host's filing still resolves");
    }

    /// A dry-run host reads its lane without claiming the account, so a live
    /// host can still claim it, and the dry-run host keeps reading afterwards.
    #[test]
    fn a_dry_run_host_never_claims_the_account() {
        let lanes = Arc::new(SettlementLanes::new());
        let dry = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());
        let live = SettlementLaneHome::boot(Arc::clone(&lanes), Some(namespace("/state")), lane());

        dry.lane(Address::repeat_byte(0x01), 0, false)
            .expect("a dry run reads its own lane");
        live.lane(Address::repeat_byte(0x01), 0, true)
            .expect("the live host claims the account the dry run left free");
        dry.lane(Address::repeat_byte(0x01), 0, false)
            .expect("the dry run still reads its lane");
    }
}
