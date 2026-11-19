//! The host-scoped registration of a strategy's nonce lane.
//!
//! A [`NonceLane`] is the seam one strategy stamps through, but the lane alone
//! does not say which host filed it or whose account it may sign for. Two
//! engines that sign with the same operator account must not each keep a
//! private occupancy map for it, and two engines that sign different accounts
//! must not reach each other's mutable nonce state.
//!
//! [`LaneRegistry`] is that declaration. A lane is filed under a [`LaneScope`]
//! — the host namespace that owns it and the strategy it signs for — and the
//! [`LaneStake`] the filing returns *is* the host's handle on it: a lookup names
//! the stake, so a peer host's lane is never a fallback. The registry enforces
//! the account rule across every filing, so one operator account has one
//! authority however many hosts file their own lanes for it, and a lane itself
//! binds to the one account its submit seam proves.

use std::sync::{Arc, Weak};

use alloy::primitives::Address;
use parking_lot::Mutex;

use crate::driver_spawn::LaneNamespace;
use crate::nonce::{NonceLane, NonceLedger, StrategyId};

/// The host scope a lane is filed under: the namespace that owns the lane's
/// run artifacts and the strategy it stamps for.
///
/// The namespace is `None` for a host with no state root installed: that host
/// still owns its lane, it simply has no artifact root to scope it to, so its
/// scope is named by the strategy alone. Two hosts in one process may name the
/// same scope (they share the process's state root); their filings stay
/// distinct because the [`LaneStake`], not the scope, is the lookup handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneScope {
    namespace: Option<LaneNamespace>,
    strategy: StrategyId,
}

impl LaneScope {
    /// Name the scope of `strategy` under `namespace`.
    #[must_use]
    pub fn new(namespace: Option<LaneNamespace>, strategy: impl Into<StrategyId>) -> Self {
        Self {
            namespace,
            strategy: strategy.into(),
        }
    }

    /// The host namespace this scope belongs to, when the host installed a
    /// state root.
    #[must_use]
    pub fn namespace(&self) -> Option<&LaneNamespace> {
        self.namespace.as_ref()
    }

    /// The strategy the scope files a lane for.
    #[must_use]
    pub fn strategy(&self) -> &StrategyId {
        &self.strategy
    }
}

/// Why a lane lookup was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaneRefusal {
    /// The named filing is not in this registry: the host that filed it is not
    /// booted here, or its teardown already removed the registration. Another
    /// host's filing of the same account is never a fallback.
    #[error("no nonce lane is registered under this host filing")]
    NotRegistered,
    /// Another lane already stamps this operator account: the account's nonces
    /// have one authority, and a second one would issue the same nonce twice.
    #[error("operator account {account} already signs through another host's lane")]
    AccountOwned {
        /// The account both lanes would stamp.
        account: Address,
    },
    /// This lane is bound to a different operator account.
    #[error("this nonce lane is registered to operator account {bound}")]
    AccountBound {
        /// The account the lane was bound to.
        bound: Address,
    },
}

/// The lane one host filed, with the account its seam bound it to.
#[derive(Debug)]
struct Entry<L: NonceLedger> {
    scope: LaneScope,
    lane: Arc<NonceLane<L>>,
    /// The operator account the filing signs for; `None` until the first
    /// submission names one.
    account: Option<Address>,
    /// How many stakes hold this filing open.
    stakes: usize,
}

/// The nonce-lane filings one process composes.
///
/// The composition root hands the same registry to every engine it boots,
/// because the operator account is process-scoped: a second engine that mints
/// its own lane for the same account is refused rather than silently given a
/// second occupancy map. Reaching a lane still requires the filing's stake, so
/// the shared registry is no lookup shortcut.
#[derive(Debug)]
pub struct LaneRegistry<L: NonceLedger> {
    entries: Mutex<Vec<Entry<L>>>,
}

impl<L: NonceLedger> Default for LaneRegistry<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: NonceLedger> LaneRegistry<L> {
    /// An empty registry: no host has filed a lane yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// File `lane` under `scope`, returning the stake that names and owns the
    /// registration.
    ///
    /// Filing the same lane for one scope twice is the same declaration made
    /// twice (a re-entrant boot): it is accepted, and each stake owns a share,
    /// so the registration survives until the last share is dropped. Filing a
    /// *different* lane under a scope another host already used is accepted
    /// too — two hosts on one state root name one scope but own separate lanes,
    /// and the account rule is what keeps their nonce spaces apart.
    pub fn install(self: &Arc<Self>, scope: LaneScope, lane: Arc<NonceLane<L>>) -> LaneStake<L> {
        let mut entries = self.entries.lock();
        let filed = entries
            .iter()
            .position(|entry| entry.scope == scope && Arc::ptr_eq(&entry.lane, &lane));
        match filed {
            Some(index) => entries[index].stakes += 1,
            None => entries.push(Entry {
                scope: scope.clone(),
                lane: Arc::clone(&lane),
                account: None,
                stakes: 1,
            }),
        }
        LaneStake {
            registry: Arc::downgrade(self),
            scope,
            lane,
        }
    }

    /// The lane this filing holds, without naming an operator account.
    ///
    /// The registration's existence is checked; nothing claims the account's
    /// authority. A host that may issue a nonce through the lane takes the
    /// claiming [`Self::resolve`] instead — a host that never issues (a dry
    /// run) never claims, so it neither contends for nor blocks the account.
    ///
    /// # Errors
    ///
    /// [`LaneRefusal::NotRegistered`] when the filing is gone.
    pub fn lane(&self, stake: &LaneStake<L>) -> Result<Arc<NonceLane<L>>, LaneRefusal> {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|entry| entry.scope == stake.scope && Arc::ptr_eq(&entry.lane, &stake.lane))
            .map(|entry| Arc::clone(&entry.lane))
            .ok_or(LaneRefusal::NotRegistered)
    }

    /// The lane this filing holds, bound to the operator account the submission
    /// names.
    ///
    /// The lookup names the stake, so it reaches exactly the registration that
    /// host filed. The first lookup binds the *lane* to `account`, and the
    /// binding belongs to the lane rather than to the one filing: every scope
    /// that filed this lane must name the same account. A different lane already
    /// bound to this account — in any scope, in any host of this process — is
    /// refused as [`LaneRefusal::AccountOwned`], because two occupancy maps over
    /// one account would issue the same nonce twice.
    ///
    /// # Errors
    ///
    /// [`LaneRefusal::NotRegistered`] when the filing is gone,
    /// [`LaneRefusal::AccountBound`] when the lane is bound to another account,
    /// and [`LaneRefusal::AccountOwned`] when another lane already stamps this
    /// account.
    pub fn resolve(
        &self,
        stake: &LaneStake<L>,
        account: Address,
    ) -> Result<Arc<NonceLane<L>>, LaneRefusal> {
        let mut entries = self.entries.lock();
        let index = entries
            .iter()
            .position(|entry| entry.scope == stake.scope && Arc::ptr_eq(&entry.lane, &stake.lane))
            .ok_or(LaneRefusal::NotRegistered)?;
        let lane = Arc::clone(&entries[index].lane);
        let bound = entries.iter().find_map(|entry| {
            Arc::ptr_eq(&entry.lane, &lane)
                .then_some(entry.account)
                .flatten()
        });
        let account = match bound {
            Some(bound) if bound != account => return Err(LaneRefusal::AccountBound { bound }),
            Some(bound) => bound,
            None => {
                let owned_elsewhere = entries.iter().any(|entry| {
                    entry.account == Some(account) && !Arc::ptr_eq(&entry.lane, &lane)
                });
                if owned_elsewhere {
                    return Err(LaneRefusal::AccountOwned { account });
                }
                account
            }
        };
        for entry in entries
            .iter_mut()
            .filter(|entry| Arc::ptr_eq(&entry.lane, &lane))
        {
            entry.account = Some(account);
        }
        Ok(lane)
    }

    /// The scopes currently filed, in filing order.
    #[must_use]
    pub fn scopes(&self) -> Vec<LaneScope> {
        self.entries
            .lock()
            .iter()
            .map(|entry| entry.scope.clone())
            .collect()
    }

    /// The operator account a scope's filing signs for, if the seam named one.
    ///
    /// A scope two hosts filed resolves to the first filing's binding; the
    /// stake is the precise handle, this is a reporting read.
    #[must_use]
    pub fn account_of(&self, scope: &LaneScope) -> Option<Address> {
        self.entries
            .lock()
            .iter()
            .find(|entry| entry.scope == *scope)
            .and_then(|entry| entry.account)
    }

    /// Drop one share of the filing, removing it with the last.
    fn release(&self, scope: &LaneScope, lane: &Arc<NonceLane<L>>) {
        let mut entries = self.entries.lock();
        let Some(index) = entries
            .iter()
            .position(|entry| entry.scope == *scope && Arc::ptr_eq(&entry.lane, lane))
        else {
            return;
        };
        match entries[index].stakes.checked_sub(1) {
            Some(remaining) if remaining > 0 => entries[index].stakes = remaining,
            _ => {
                entries.remove(index);
            }
        }
    }
}

/// The registration one [`LaneRegistry::install`] filed: the host's handle on
/// its lane, and the owner of the filing.
///
/// Dropping the stake removes exactly this registration: a peer host's filing
/// (a different scope, or the same scope with its own lane) is untouched, and a
/// torn-down registration is not resolvable.
#[derive(Debug)]
pub struct LaneStake<L: NonceLedger> {
    registry: Weak<LaneRegistry<L>>,
    scope: LaneScope,
    lane: Arc<NonceLane<L>>,
}

impl<L: NonceLedger> LaneStake<L> {
    /// The scope this stake filed.
    #[must_use]
    pub fn scope(&self) -> &LaneScope {
        &self.scope
    }

    /// The lane this stake filed.
    #[must_use]
    pub fn lane(&self) -> &Arc<NonceLane<L>> {
        &self.lane
    }
}

impl<L: NonceLedger> Drop for LaneStake<L> {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.release(&self.scope, &self.lane);
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "unit tests: a malformed fixture must fail the test loudly"
)]
mod tests {
    use alloy::primitives::B256;

    use super::*;
    use crate::nonce::{HoldWindow, LanePolicy, NonceAuthority};

    #[derive(Debug)]
    struct NoopLedger;

    impl NonceLedger for NoopLedger {
        type Decline = &'static str;

        fn record_signed(
            &self,
            _strategy: &StrategyId,
            _nonce: u64,
            _target: B256,
            _bundle_hash: B256,
            _built_at_head: u64,
            _replaces: Option<B256>,
        ) -> Result<(), Self::Decline> {
            Ok(())
        }

        fn record_broadcast(
            &self,
            _strategy: &StrategyId,
            _nonce: u64,
        ) -> Result<(), Self::Decline> {
            Ok(())
        }
    }

    fn lane(authority: &Arc<NonceAuthority>) -> Arc<NonceLane<NoopLedger>> {
        Arc::new(authority.mint_lane(
            "settlement",
            Arc::new(NoopLedger),
            LanePolicy::new(HoldWindow::DEFAULT, "settlement"),
        ))
    }

    fn scope(root: &str) -> LaneScope {
        LaneScope::new(
            Some(LaneNamespace::under(root, &StrategyId::new("settlement"))),
            "settlement",
        )
    }

    fn account(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    /// A filing is reachable only through its own stake: a peer's lane is never
    /// a fallback, and a torn-down filing resolves nothing.
    #[test]
    fn a_host_resolves_only_the_filing_it_owns() {
        let registry = Arc::new(LaneRegistry::new());
        let host_a = Arc::new(NonceAuthority::new(0));
        let host_b = Arc::new(NonceAuthority::new(0));
        let filed_a = lane(&host_a);
        let filed_b = lane(&host_b);
        let stake_a = registry.install(scope("/root-a"), Arc::clone(&filed_a));
        let stake_b = registry.install(scope("/root-b"), Arc::clone(&filed_b));

        let resolved_a = registry
            .resolve(&stake_a, account(0xa1))
            .expect("host A resolves");
        let resolved_b = registry
            .resolve(&stake_b, account(0xb2))
            .expect("host B resolves");
        assert!(Arc::ptr_eq(&resolved_a, &filed_a));
        assert!(Arc::ptr_eq(&resolved_b, &filed_b));
        assert!(
            !Arc::ptr_eq(&resolved_a, &resolved_b),
            "each filing resolves its own lane, never its peer's"
        );
        assert_eq!(
            registry.resolve(&stake_b, account(0xa1)).err(),
            Some(LaneRefusal::AccountBound {
                bound: account(0xb2)
            }),
            "host B's lane signs for the one account it bound"
        );

        drop(stake_a);
        assert_eq!(
            registry.scopes(),
            vec![scope("/root-b")],
            "the torn-down filing is gone; the peer's survives"
        );
        registry
            .resolve(&stake_b, account(0xb2))
            .expect("the peer's filing still resolves");
    }

    /// Two hosts on one registry — the production shape, since the operator
    /// account is process-scoped — sign distinct accounts through their own
    /// lanes and separate authorities.
    #[test]
    fn two_hosts_on_one_registry_keep_distinct_accounts_isolated() {
        let registry = Arc::new(LaneRegistry::new());
        let host_a = Arc::new(NonceAuthority::new(0));
        let host_b = Arc::new(NonceAuthority::new(0));
        let stake_a = registry.install(scope("/state"), lane(&host_a));
        let stake_b = registry.install(scope("/state"), lane(&host_b));

        let lane_a = registry
            .resolve(&stake_a, account(0x0a))
            .expect("A binds its account");
        let lane_b = registry
            .resolve(&stake_b, account(0x0b))
            .expect("B binds its account");
        assert!(!Arc::ptr_eq(&lane_a, &lane_b));
        assert!(
            !Arc::ptr_eq(lane_a.authority(), lane_b.authority()),
            "distinct accounts keep distinct authorities"
        );
        lane_a.stamp().expect("A stamps");
        assert!(
            lane_b.authority().outstanding_nonces().is_empty(),
            "A's stamp never reaches B's authority"
        );
    }

    /// Two separately minted lanes cannot claim one account: the second lookup
    /// is refused instead of silently minting a second authority.
    #[test]
    fn a_second_lane_cannot_claim_a_bound_account() {
        let registry = Arc::new(LaneRegistry::new());
        let authority = Arc::new(NonceAuthority::new(0));
        let stake_a = registry.install(scope("/root-a"), lane(&authority));
        let stake_b = registry.install(scope("/root-b"), lane(&authority));

        registry
            .resolve(&stake_a, account(0x42))
            .expect("A binds the account");
        assert_eq!(
            registry.resolve(&stake_b, account(0x42)).err(),
            Some(LaneRefusal::AccountOwned {
                account: account(0x42)
            }),
            "the account's nonces have exactly one authority"
        );
    }

    /// A lane signs for the one account its seam named first; a later
    /// submission naming another account is refused.
    #[test]
    fn a_registration_is_bound_to_one_operator_account() {
        let registry = Arc::new(LaneRegistry::new());
        let authority = Arc::new(NonceAuthority::new(0));
        let stake = registry.install(scope("/root"), lane(&authority));

        registry
            .resolve(&stake, account(0x01))
            .expect("the first submission binds the account");
        assert_eq!(
            registry.resolve(&stake, account(0x02)).err(),
            Some(LaneRefusal::AccountBound {
                bound: account(0x01)
            })
        );
        assert_eq!(registry.account_of(&scope("/root")), Some(account(0x01)));
    }

    /// Two scopes that filed the same lane are the explicit declaration of one
    /// shared authority: both resolve to the one lane.
    #[test]
    fn scopes_that_file_one_lane_share_that_lane_deliberately() {
        let registry = Arc::new(LaneRegistry::new());
        let shared_authority = Arc::new(NonceAuthority::new(7));
        let shared = lane(&shared_authority);
        let stake_a = registry.install(scope("/root-a"), Arc::clone(&shared));
        let stake_b = registry.install(scope("/root-b"), Arc::clone(&shared));

        let resolved_a = registry
            .resolve(&stake_a, account(0x11))
            .expect("A resolves");
        let resolved_b = registry
            .resolve(&stake_b, account(0x11))
            .expect("B resolves");
        assert!(Arc::ptr_eq(&resolved_a, &shared));
        assert!(
            Arc::ptr_eq(&resolved_a, &resolved_b),
            "the declared sharing resolves to the one lane"
        );
    }

    /// A shared lane still signs for the account it bound, however many scopes
    /// filed it: a second account would share one nonce sequence.
    #[test]
    fn a_shared_lane_still_signs_for_one_operator_account() {
        let registry = Arc::new(LaneRegistry::new());
        let authority = Arc::new(NonceAuthority::new(0));
        let shared = lane(&authority);
        let stake_a = registry.install(scope("/root-a"), Arc::clone(&shared));
        let stake_b = registry.install(scope("/root-b"), Arc::clone(&shared));

        registry
            .resolve(&stake_a, account(0x01))
            .expect("A binds the first account");
        assert_eq!(
            registry.resolve(&stake_b, account(0x02)).err(),
            Some(LaneRefusal::AccountBound {
                bound: account(0x01)
            }),
            "one lane signs for one account, however many scopes filed it"
        );
        registry
            .resolve(&stake_b, account(0x01))
            .expect("the same account shares the lane");
    }

    /// A teardown removes exactly the registration its stake filed: a peer
    /// host's filing over the same account space survives it.
    #[test]
    fn teardown_removes_only_the_owned_registration() {
        let registry = Arc::new(LaneRegistry::new());
        let authority = Arc::new(NonceAuthority::new(0));
        let mine = registry.install(scope("/root-a"), lane(&authority));
        let peer = registry.install(scope("/root-b"), lane(&authority));
        registry.resolve(&mine, account(0x01)).expect("A binds");
        registry.resolve(&peer, account(0x02)).expect("B binds");

        assert_eq!(
            registry.scopes(),
            vec![scope("/root-a"), scope("/root-b")],
            "both hosts filed"
        );

        drop(mine);

        assert_eq!(
            registry.scopes(),
            vec![scope("/root-b")],
            "the teardown removed only the filing its stake owned"
        );
        assert_eq!(registry.account_of(&scope("/root-a")), None);
        registry
            .resolve(&peer, account(0x02))
            .expect("the peer's registration survives");
    }

    /// A host that never issues a nonce never claims the account: a dry-run
    /// host reads its lane, and a live host may still claim that account.
    #[test]
    fn a_dry_run_filing_claims_nothing() {
        let registry = Arc::new(LaneRegistry::new());
        let dry_authority = Arc::new(NonceAuthority::new(0));
        let live_authority = Arc::new(NonceAuthority::new(0));
        let dry = registry.install(scope("/state"), lane(&dry_authority));
        let live = registry.install(scope("/state"), lane(&live_authority));

        registry
            .lane(&dry)
            .expect("the dry-run filing resolves its lane");
        assert_eq!(
            registry.account_of(&scope("/state")),
            None,
            "a host that never issues claims no account"
        );
        let claimed = registry
            .resolve(&live, account(0x01))
            .expect("the live host claims the account");
        assert!(Arc::ptr_eq(&claimed, live.lane()));
        registry
            .lane(&dry)
            .expect("the dry-run filing still resolves");
    }

    /// A filing composed by another registry (another process's composition
    /// root) is not reachable here: the shared process registry is still not a
    /// process-wide lookup.
    #[test]
    fn a_foreign_filing_is_not_registered_here() {
        let registry = Arc::new(LaneRegistry::new());
        let other = Arc::new(LaneRegistry::new());
        let authority = Arc::new(NonceAuthority::new(0));
        let foreign = other.install(scope("/state"), lane(&authority));

        assert_eq!(
            registry.resolve(&foreign, account(0x01)).err(),
            Some(LaneRefusal::NotRegistered)
        );
    }
}
