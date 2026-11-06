//! The evm2 port of `BotStateDb` (`sim::evm::bot_state_db`) — a thin
//! `Database` wrapper forwarding every read to the cold-miss fallback, with
//! the tracked-pool code-less tripwire layered on the membership oracle.
//!
//! Scope: the forwarding shape + the invariant tripwire (a tracked pool
//! resolving non-existent or code-less is an impossibility — fail loudly at
//! the value-origin point so the poisoned read can never be simulated or
//! cached) + the revm-side original's read-path diagnostics: the per-cycle
//! storage memo and the divergence observer (both engine-neutral — the memo
//! is `sim::evm::StorageMemo`, the probe `sim::evm::divergence_probe`). The
//! false-empty provenance probe stays cutover scope. The trait is evm2's
//! mutable `Database` for the same composition reason documented in
//! `warm_code_cache`.

use alloy::primitives::{Address, B256, KECCAK256_EMPTY};
use degenbot_substrate::SimAnchorOracle;

use crate::sim::divergence_probe::DivergenceTallyHandle;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, Database};
use ::evm2::interpreter::Word;
#[cfg(test)]
use degenbot_core::fee::fee_ratio;

/// A thin `Database` wrapper that forwards every read to the `fallback`,
/// with the tracked-pool code-less tripwire on `get_account`.
pub struct BotStateDb<'bot, ExtDb>
where
    ExtDb: Database,
{
    /// The typed oracle backing the code-less tripwire (membership). Same
    /// borrow shape as the revm-side original — the snapshot is owned by the
    /// caller and outlives the per-block wrapper.
    pub oracle: &'bot dyn SimAnchorOracle,
    /// The RPC cold-miss fallback (the `AlloyProvider` adapter in the target
    /// stack).
    pub fallback: ExtDb,
    /// The per-block storage memo (see `sim::evm::StorageMemo`): consulted
    /// before the fallback on `get_storage`, populated from the fallback's
    /// own answers. Recreated by the owner on every block advance.
    pub storage_memo: Option<std::sync::Arc<crate::sim::StorageMemo>>,
    /// The per-sim on-demand divergence-probe arm (VERIFY2 T2 — sim-failure
    /// re-verify or a random spot-check), skipping the env gate.
    pub divergence_probe: bool,
    /// The divergence tally the observations accumulate into. `None` — the
    /// production default — is the process-global tally; a caller that must
    /// read the counts of its own reads hands its own handle.
    pub divergence_tally: Option<DivergenceTallyHandle>,
}

impl<'bot, ExtDb> BotStateDb<'bot, ExtDb>
where
    ExtDb: Database,
{
    /// Wrap the cold-miss fallback `Database`. The `oracle` borrow backs the
    /// code-less tripwire.
    #[must_use]
    pub fn new(oracle: &'bot dyn SimAnchorOracle, fallback: ExtDb) -> Self {
        Self {
            oracle,
            fallback,
            storage_memo: None,
            divergence_probe: false,
            divergence_tally: None,
        }
    }

    /// Layer the per-block storage memo onto `get_storage` (mirrors the
    /// revm-side `with_storage_memo`).
    #[must_use]
    pub fn with_storage_memo(mut self, memo: std::sync::Arc<crate::sim::StorageMemo>) -> Self {
        self.storage_memo = Some(memo);
        self
    }

    /// The `Option`-carrying form (mirrors the revm-side
    /// `with_storage_memo_opt` — the handle passes its optional memo
    /// through).
    #[must_use]
    pub fn with_storage_memo_opt(
        mut self,
        memo: Option<&std::sync::Arc<crate::sim::StorageMemo>>,
    ) -> Self {
        self.storage_memo = memo.cloned();
        self
    }

    /// Arm (or disarm) the on-demand divergence probe for this wrapper
    /// (the armed form skips the env gate; mirrors the revm-side
    /// `with_divergence_probe(bool)`).
    #[must_use]
    pub fn with_divergence_probe(mut self, arm: bool) -> Self {
        self.divergence_probe = arm;
        self
    }

    /// Point this wrapper's divergence observations at `tally` instead of the
    /// process-global one (the same handle shape as `with_divergence_probe`;
    /// the process-global stays the production default).
    #[must_use]
    pub fn with_divergence_tally(mut self, tally: DivergenceTallyHandle) -> Self {
        self.divergence_tally = Some(tally);
        self
    }

    /// The `Option`-carrying form (mirrors `with_storage_memo_opt` — the
    /// handle passes its optional tally through).
    #[must_use]
    pub fn with_divergence_tally_opt(mut self, tally: Option<&DivergenceTallyHandle>) -> Self {
        self.divergence_tally = tally.cloned();
        self
    }
}

impl<ExtDb> Database for BotStateDb<'_, ExtDb>
where
    ExtDb: Database,
{
    type Error = ExtDb::Error;

    /// Forward to the fallback, with the LOUD invalidity tripwire for tracked
    /// pools: a registered pool resolving non-existent or code-less is an
    /// impossibility (every later sim would execute it as an EOA), so the
    /// read panics at the value-origin point.
    #[expect(clippy::panic_in_result_fn, clippy::panic)] // code-less tracked pool = invariant break (loud)
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        let info = self.fallback.get_account(address)?;
        if self.oracle.is_registered_pool(address) {
            let invalid = match &info {
                None => true,
                Some(acc) => acc.code_hash == KECCAK256_EMPTY,
            };
            if invalid {
                let state = if info.is_none() {
                    "non-existent (None)"
                } else {
                    "code-less (KECCAK256_EMPTY)"
                };
                panic!(
                    "Sim DB invariant: tracked pool {address} resolved as {state} by the \
                     RPC fallback — refusing to simulate a code-less pool"
                );
            }
        }
        Ok(info)
    }

    /// Forward to the fallback. Never invoked when the fallback's
    /// `get_account` eager-loads code (the RPC adapter's shape).
    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.fallback.get_code_by_hash(code_hash)
    }

    /// Forward to the fallback — the revm-side original's serving seam was
    /// retired (ADR-056); the fallback's value is the sim's value.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        // The per-block memo first — a hit is the same block's own earlier
        // fallback answer (the fallback remains the authority; the memo
        // never serves engine slots).
        if let Some(memo) = self.storage_memo.as_ref() {
            if let Some(value) = memo.get(*address, *key) {
                return Ok(value);
            }
        }
        let rpc_value = self.fallback.get_storage(address, key)?;
        if let Some(memo) = self.storage_memo.as_ref() {
            memo.put(*address, *key, rpc_value);
        }
        // Observation (compares engine vs RPC, never changes what the sim
        // reads); the armed arm is the on-demand one, and both accumulate into
        // this wrapper's tally (the process-global unless one was handed in).
        if self.divergence_probe {
            crate::sim::divergence_probe::observe_storage_read_forced(
                self.oracle,
                *address,
                *key,
                rpc_value,
                self.divergence_tally.as_ref(),
            );
        } else {
            crate::sim::divergence_probe::observe_storage_read(
                self.oracle,
                *address,
                *key,
                rpc_value,
                self.divergence_tally.as_ref(),
            );
        }
        Ok(rpc_value)
    }

    /// Block hashes are not in `Bot`'s domain — always fall through.
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.fallback.get_block_hash(number)
    }
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::aliases::U112;
    use alloy::primitives::{Bytes, U256};
    use degenbot_pools::RegisterV2PoolParams;
    use degenbot_substrate::{BotState, SimAnchorState};
    use degenbot_uniswap::dex_identity::DexVariant;
    use std::convert::Infallible;

    const POOL: Address = Address::new([0xaa; 20]);
    const EOA: Address = Address::new([0xee; 20]);

    fn bot_state_with_pool() -> BotState {
        let mut core = BotState::new();
        core.register_v2_pool(&RegisterV2PoolParams {
            address: POOL,
            token0: Address::from([0xbb; 20]),
            token1: Address::from([0xcc; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: fee_ratio(997, 1000),
            fee_token1: fee_ratio(997, 1000),
            factory: Address::from([0xdd; 20]),
            update_block: 0,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
        })
        .expect("test setup: V2 registration");
        core
    }

    struct ScriptedDb {
        result: Option<AccountInfo>,
    }

    /// A counting fallback whose storage answer the test scripts (the memo
    /// seam's forward counter).
    struct CountingDb {
        storage_value: Word,
        storage_calls: u64,
    }

    impl CountingDb {
        fn new(storage_value: u64) -> Self {
            Self {
                storage_value: Word::from(storage_value),
                storage_calls: 0,
            }
        }
    }

    impl Database for CountingDb {
        type Error = Infallible;

        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(code_info()))
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            self.storage_calls += 1;
            Ok(self.storage_value)
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    impl Database for ScriptedDb {
        type Error = Infallible;
        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(self.result.clone())
        }
        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }
        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            Ok(Word::ZERO)
        }
        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    fn code_info() -> AccountInfo {
        AccountInfo::default()
            .with_balance(U256::from(1u64))
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00])))
    }

    #[test]
    #[should_panic(expected = "Sim DB invariant")]
    fn tracked_pool_resolving_none_panics() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let mut db = BotStateDb::new(&anchor, ScriptedDb { result: None });
        let _ = db.get_account(&POOL).unwrap();
    }

    #[test]
    #[should_panic(expected = "Sim DB invariant")]
    fn tracked_pool_resolving_codeless_panics() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let mut db = BotStateDb::new(
            &anchor,
            ScriptedDb {
                result: Some(AccountInfo::default()),
            },
        );
        let _ = db.get_account(&POOL).unwrap();
    }

    #[test]
    fn tracked_pool_with_code_passes_through() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let mut db = BotStateDb::new(
            &anchor,
            ScriptedDb {
                result: Some(code_info()),
            },
        );
        let got = db.get_account(&POOL).unwrap();
        assert!(got.is_some());
        assert_ne!(got.unwrap().code_hash, KECCAK256_EMPTY);
    }

    #[test]
    fn non_pool_none_is_exempt() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let mut db = BotStateDb::new(&anchor, ScriptedDb { result: None });
        assert!(db.get_account(&EOA).unwrap().is_none());
    }

    /// A block's fan-outs share the anchor's memo: the second one pays no
    /// storage forward and reads the same value. Both wrappers are handed the
    /// same tally — a forward is what a divergence observation rides on, so the
    /// memo hit observes nothing.
    #[test]
    fn one_anchors_fan_outs_share_the_storage_memo() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let cache = crate::sim::StorageMemoCache::new();
        let tally = crate::sim::divergence_probe::DivergenceTallyHandle::new();
        let key = Word::from(8u64);

        let mut first = BotStateDb::new(&anchor, CountingDb::new(42))
            .with_storage_memo(cache.for_anchor(crate::sim::ReadAnchor::at(100)))
            .with_divergence_tally(tally.clone());
        assert_eq!(first.get_storage(&POOL, &key).unwrap(), Word::from(42u64));
        assert_eq!(first.fallback.storage_calls, 1, "the cold fan-out forwards");
        assert_eq!(
            tally.snapshot().slots_compared,
            1,
            "the forwarded read observes into the handed tally"
        );

        let mut second = BotStateDb::new(&anchor, CountingDb::new(42))
            .with_storage_memo(cache.for_anchor(crate::sim::ReadAnchor::at(100)))
            .with_divergence_tally(tally.clone());
        assert_eq!(
            second.get_storage(&POOL, &key).unwrap(),
            Word::from(42u64),
            "the shared memo serves the anchor's own earlier answer"
        );
        assert_eq!(
            second.fallback.storage_calls, 0,
            "the second fan-out at the same anchor pays no storage forward"
        );
        assert_eq!(
            tally.snapshot().slots_compared,
            1,
            "a memo hit forwards no RPC answer, so it observes nothing"
        );
    }

    /// The anchor moving retires the memo: the value the chain serves at the
    /// NEW anchor is never shadowed by the old memo's. Each wrapper's forward
    /// lands in the tally they share — one handle, two observations.
    #[test]
    fn an_anchor_move_starts_a_fresh_storage_memo() {
        let core = bot_state_with_pool();
        let anchor = SimAnchorState::snapshot(&core);
        let cache = crate::sim::StorageMemoCache::new();
        let tally = crate::sim::divergence_probe::DivergenceTallyHandle::new();
        let key = Word::from(8u64);

        let mut before = BotStateDb::new(&anchor, CountingDb::new(42))
            .with_storage_memo(cache.for_anchor(crate::sim::ReadAnchor::at(100)))
            .with_divergence_tally(tally.clone());
        assert_eq!(before.get_storage(&POOL, &key).unwrap(), Word::from(42u64));

        // The chain answers differently once the anchor moves (a reorg
        // replacement, or simply the next block).
        let mut after = BotStateDb::new(&anchor, CountingDb::new(7))
            .with_storage_memo(cache.for_anchor(crate::sim::ReadAnchor::at(101)))
            .with_divergence_tally(tally.clone());
        assert_eq!(
            after.get_storage(&POOL, &key).unwrap(),
            Word::from(7u64),
            "the moved anchor's own answer is served, not the retired memo's"
        );
        assert_eq!(
            after.fallback.storage_calls, 1,
            "the fresh memo re-fetches at the new anchor"
        );
        assert_eq!(
            tally.snapshot().slots_compared,
            2,
            "both wrappers observe into the one handle they were handed"
        );
    }
}
