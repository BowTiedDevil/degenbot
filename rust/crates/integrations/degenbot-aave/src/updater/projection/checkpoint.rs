//! The projection checkpoint's payload: what a resume needs besides the exact
//! cursor, stated in the delta vocabulary's own terms (ADR-105 D8, D9).
//!
//! `degenbot-db` owns the checkpoint's schema, its constraints, and the one
//! atomic write; this module owns what the payload MEANS and which version it
//! states. It reads the two state families the reducing driver holds at a
//! projection unit's boundary:
//!
//! - the forward-only [`ProjectionView`] (ADR-105 D3): the exact cursor, the
//!   positions it has advanced, and the generations it reads under;
//! - the [`ChunkSubstrate`] (the resolution read cache): the reserve identities
//!   (address mappings + revisions), the contract endpoints and revisions, and
//!   the chain's GHO row (its vToken link and discount columns).
//!
//! Those are the reads resolution makes at the boundary, so a restore classifies
//! GHO initialization, the discount pre-pass, and same-block replacement
//! generations from the checkpoint alone — never from a row that may sit on an
//! abandoned branch.
//!
//! The per-user family is the one family the payload does not state: a checkpoint
//! must carry the market's COMPLETE user set, which only the market's own rows
//! enumerate (the substrate cache holds the chunk's candidate addresses). The
//! unit writer reads it inside the same transaction, exactly as it states the
//! settled position rows.
//!
//! The checkpoint is a storage and recovery optimization, never a correctness
//! dependency: a market whose units were never checkpointed still journals every
//! delta and advances its cursor exactly the same way (ADR-105 D9).

use degenbot_core::canonical_head::{LogRef, TxRef};
use degenbot_db::{
    CheckpointAsset, CheckpointContract, CheckpointGho, CheckpointPositionState, GenerationRecord,
    ProjectionCheckpoint, ProjectionCheckpointRow,
};

use super::journal::generation_record;
use super::{
    GenerationActivation, GenerationEntity, GenerationRef, ProjectionError, ProjectionView,
};
use crate::updater::run::substrate::ChunkSubstrate;

/// The encoding version every checkpoint states. A change to what
/// [`CheckpointState::capture`] records advances this tag, so a recovery can tell
/// two checkpoint payloads apart without guessing (ADR-105 D7's versioning rule,
/// applied to the checkpoint family).
pub const CHECKPOINT_ENCODING_VERSION: &str = "aave.projection-checkpoint/v4";

/// Whether the projection unit that journaled `journaled_entries` deltas writes a
/// checkpoint with its cursor.
///
/// The initial cadence is the simplest one that bounds recovery: every unit that
/// advances the market's exact cursor is checkpointed, so a restore replays no
/// delta at all and the checkpoint set is exactly the cursor history — a
/// checkpoint cannot exist at a position the journal did not commit. The cost is
/// one copy of the market's settled position, user, and lineage state per unit;
/// the cadence is a storage/recovery trade-off (ADR-105 D9) measured before any
/// tuning, and changing it is this function alone.
#[must_use]
pub fn checkpoint_due(journaled_entries: usize) -> bool {
    journaled_entries > 0
}

/// The resume state the reducing driver carries at a projection unit's boundary.
///
/// Deterministically ordered, because the unit's checkpoint rows are a golden
/// surface: iteration order over the view's and the substrate's maps must never
/// reach storage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckpointState {
    generations: Vec<GenerationRecord>,
    positions: Vec<CheckpointPositionState>,
    assets: Vec<CheckpointAsset>,
    asset_configs: Vec<degenbot_db::CheckpointAssetConfig>,
    contracts: Vec<CheckpointContract>,
    gho: Option<CheckpointGho>,
    users: Vec<degenbot_db::CheckpointUser>,
    user_collaterals: Vec<degenbot_db::CheckpointUserCollateral>,
    emode_categories: Vec<degenbot_db::CheckpointEModeCategory>,
    cursor: Option<degenbot_db::LogCoordinate>,
}

impl CheckpointState {
    /// Project the resume state of one projection unit's boundary.
    #[must_use]
    pub fn capture(view: &ProjectionView, substrate: &ChunkSubstrate) -> Self {
        // The order IS the current-vs-sealed statement: for every entity its
        // sealed generations come first, in a deterministic order, and the
        // generation it reads under comes last. A restore replays the list in
        // order and takes the last record per entity as current, so the
        // selection never consults a revision or an activation kind — a
        // non-monotonic replacement cannot make it pick the sealed one.
        let mut per_entity: std::collections::BTreeMap<
            String,
            (Vec<GenerationRecord>, Option<GenerationRecord>),
        > = std::collections::BTreeMap::new();
        for generation in view.sealed_generations() {
            let record = generation_record(generation);
            per_entity
                .entry(record.entity.clone())
                .or_default()
                .0
                .push(record);
        }
        for (_, generation) in view.generations() {
            let record = generation_record(generation);
            let entity = record.entity.clone();
            per_entity.entry(entity).or_default().1 = Some(record);
        }
        let mut generations: Vec<GenerationRecord> = Vec::new();
        for (_, (mut sealed, current)) in per_entity {
            sealed.sort_by_key(sealed_order);
            sealed.dedup();
            if let Some(current) = &current {
                sealed.retain(|record| record != current);
            }
            generations.extend(sealed);
            generations.extend(current);
        }
        let mut positions: Vec<CheckpointPositionState> = view
            .positions()
            .map(|(subject, (balance, index))| CheckpointPositionState {
                position: subject.position,
                position_id: subject.position_id,
                balance,
                index,
            })
            .collect();
        positions.sort_by_key(|state| (state.position, state.position_id));
        Self {
            generations,
            positions,
            assets: Vec::new(),
            asset_configs: Vec::new(),
            contracts: substrate.capture_contracts(),
            gho: substrate.capture_gho(),
            users: Vec::new(),
            user_collaterals: Vec::new(),
            emode_categories: Vec::new(),
            cursor: None,
        }
    }

    /// The checkpoint a unit writer commits, at the cursor the unit advances to.
    #[must_use]
    pub fn wire(&self) -> ProjectionCheckpoint<'_> {
        ProjectionCheckpoint {
            encoding_version: CHECKPOINT_ENCODING_VERSION,
            generations: &self.generations,
            positions: &self.positions,
            contracts: &self.contracts,
            gho: self.gho.as_ref(),
        }
    }

    /// The state a committed checkpoint states, read back without consulting a
    /// single market row: the resume entry point (ADR-105 D9).
    #[must_use]
    pub fn restore(row: &ProjectionCheckpointRow) -> Self {
        Self {
            generations: row.generations.clone(),
            positions: row.positions.clone(),
            assets: row.assets.clone(),
            asset_configs: row.asset_configs.clone(),
            contracts: row.contracts.clone(),
            gho: row.gho.clone(),
            users: row.users.clone(),
            user_collaterals: row.user_collaterals.clone(),
            emode_categories: row.emode_categories.clone(),
            cursor: Some(row.position),
        }
    }

    /// The exact position the checkpoint restores to.
    #[must_use]
    pub const fn cursor(&self) -> Option<degenbot_db::LogCoordinate> {
        self.cursor
    }

    /// The transaction-start GHO discount inputs a resume derives from this
    /// checkpoint alone: the linked vToken (if the reserve is initialized), the
    /// revision that selects the discount rule, and the user's settled discount.
    ///
    /// These are exactly the three values the live pre-pass is handed
    /// ([`crate::updater::config_dispatch::discount`]), so a restored checkpoint
    /// classifies a user's discount path without reading a market row.
    #[must_use]
    pub fn discount_prepass_inputs(&self, user_address: &str) -> DiscountPrepassInputs {
        let vtoken = self
            .gho
            .as_ref()
            .and_then(|gho| gho.v_token_address.as_deref())
            .and_then(|address| address.parse().ok());
        let revision = self
            .gho
            .as_ref()
            .and_then(|gho| gho.v_token_address.as_deref())
            .and_then(|address| self.asset_by_address("v_token", address))
            .map(|asset| asset.v_token_revision);
        let settled = self
            .users
            .iter()
            .find(|user| user.address == user_address)
            .map(|user| user.gho_discount);
        DiscountPrepassInputs {
            vtoken,
            revision,
            settled,
        }
    }

    /// The complete user set the checkpoint states — the market's own enumeration
    /// at the checkpointed cursor.
    #[must_use]
    pub fn users(&self) -> &[degenbot_db::CheckpointUser] {
        &self.users
    }

    /// The user state the checkpoint states, by address.
    #[must_use]
    pub fn user(&self, address: &str) -> Option<&degenbot_db::CheckpointUser> {
        self.users.iter().find(|user| user.address == address)
    }

    /// The generation relationships this checkpoint states.
    #[must_use]
    pub fn generations(&self) -> &[GenerationRecord] {
        &self.generations
    }

    /// The position state the reducing view held when the checkpoint was taken.
    #[must_use]
    pub fn positions(&self) -> &[CheckpointPositionState] {
        &self.positions
    }

    /// The reserves' identities, revisions, and settled configuration.
    #[must_use]
    pub fn assets(&self) -> &[CheckpointAsset] {
        &self.assets
    }

    /// The reserves' settled collateral configurations.
    #[must_use]
    pub fn asset_configs(&self) -> &[degenbot_db::CheckpointAssetConfig] {
        &self.asset_configs
    }

    /// The users' settled collateral flags — the `ReserveUsedAsCollateral`
    /// resolution state a rewind restores.
    #[must_use]
    pub fn user_collaterals(&self) -> &[degenbot_db::CheckpointUserCollateral] {
        &self.user_collaterals
    }

    /// The e-mode category definitions in force — the rows an
    /// `EModeCategoryAdded` delta writes.
    #[must_use]
    pub fn emode_categories(&self) -> &[degenbot_db::CheckpointEModeCategory] {
        &self.emode_categories
    }

    /// The contract endpoints and revisions resolution targets.
    #[must_use]
    pub fn contracts(&self) -> &[CheckpointContract] {
        &self.contracts
    }

    /// The chain's GHO row, when the chain has one.
    #[must_use]
    pub fn gho(&self) -> Option<&CheckpointGho> {
        self.gho.as_ref()
    }

    /// The reserve an address maps to, by the kind resolution asks for
    /// (`"underlying"`, `"a_token"`, `"v_token"`). A restored state answers this
    /// exactly as the resolution cache does.
    #[must_use]
    pub fn asset_by_address(&self, kind: &str, address: &str) -> Option<&CheckpointAsset> {
        self.assets.iter().find(|asset| match kind {
            "underlying" => asset.underlying_address == address,
            "a_token" => asset.a_token_address == address,
            "v_token" => asset.v_token_address == address,
            _ => false,
        })
    }

    /// The contract endpoint a role resolves to.
    #[must_use]
    pub fn contract(&self, name: &str) -> Option<&CheckpointContract> {
        self.contracts.iter().find(|contract| contract.name == name)
    }
}

/// A restored view's generation: the entity and revision a checkpoint names,
/// with the activation position a journaled delta carries.
/// The deterministic order an entity's sealed generations are stated in: by
/// revision, seed before an event of the same revision, then by position. It
/// orders only the sealed run; which record is current is stated by position
/// within the run (last), never by this key.
fn sealed_order(
    generation: &GenerationRecord,
) -> (Option<u32>, u8, alloy::primitives::B256, u64, u64) {
    let (rank, hash, block, log_index) = match generation.activation {
        degenbot_db::GenerationActivation::Seed => (0, alloy::primitives::B256::ZERO, 0, 0),
        degenbot_db::GenerationActivation::Event(at) => {
            (1, at.tx_hash(), at.anchor().number(), at.log_index())
        }
    };
    (generation.revision, rank, hash, block, log_index)
}

fn restored_generation(record: &GenerationRecord) -> Result<GenerationRef, super::ProjectionError> {
    let entity = GenerationEntity::decode(&record.entity)?;
    let activation = match record.activation {
        degenbot_db::GenerationActivation::Seed => GenerationActivation::Seed,
        degenbot_db::GenerationActivation::Event(at) => GenerationActivation::Event(log_ref(at)?),
    };
    Ok(GenerationRef::restored(entity, activation, record.revision))
}

/// The exact chain position a stored coordinate names.
pub(super) fn log_ref(at: degenbot_db::LogCoordinate) -> Result<LogRef, super::ProjectionError> {
    let anchor = at.anchor();
    let tx = TxRef::new(anchor, at.tx_hash(), at.tx_index()).map_err(|error| {
        ProjectionError::EntityKey {
            key: error.to_string(),
        }
    })?;
    Ok(LogRef::new(tx, at.log_index()))
}

/// Which path of the GHO discount pre-pass answers a user, given the three
/// inputs a boundary states (ADR-105 D8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscountPath {
    /// No GHO vToken is linked yet: the pre-pass has nothing to read.
    NoGhoToken,
    /// The vToken's revision is unresolved: the pre-pass has nothing to read.
    UnresolvedRevision,
    /// Path #1: the user's settled row answers the transaction-start discount.
    Settled(i64),
    /// Path #3: the vToken's revision is at or past the deprecation revision.
    Deprecated,
    /// Path #2: the transaction-start discount must be read from the chain.
    ChainRead,
}

/// The one classification the GHO discount pre-pass makes per user: which of its
/// supported paths answers this user's transaction-start discount. The live
/// pre-pass and a restored checkpoint both call THIS, so a checkpoint cannot
/// classify a discount path the running driver would not.
#[must_use]
pub fn classify_discount_prepass(inputs: DiscountPrepassInputs) -> DiscountPath {
    if inputs.vtoken.is_none() {
        return DiscountPath::NoGhoToken;
    }
    let Some(revision) = inputs.revision else {
        return DiscountPath::UnresolvedRevision;
    };
    if let Some(settled) = inputs.settled {
        return DiscountPath::Settled(settled);
    }
    if revision < crate::updater::config_dispatch::GHO_DISCOUNT_DEPRECATION_REVISION {
        DiscountPath::ChainRead
    } else {
        DiscountPath::Deprecated
    }
}

/// The three values the live GHO discount pre-pass is handed at transaction
/// start, as a checkpoint restates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscountPrepassInputs {
    /// The linked GHO vToken address, once the reserve is initialized.
    pub vtoken: Option<alloy::primitives::Address>,
    /// The vToken's revision (the discount rule selector).
    pub revision: Option<u32>,
    /// The user's settled transaction-start discount.
    pub settled: Option<i64>,
}

impl CheckpointState {
    /// Rebuild the forward-only read position this checkpoint states: its cursor,
    /// the position state it settled, and the generations it read under. Applying
    /// retained journal deltas to this view is recovery's whole forward step, and
    /// it needs no market row to get here.
    ///
    /// # Errors
    ///
    /// [`super::ProjectionError::EntityKey`] for a generation entity key outside
    /// the vocabulary, or a stored coordinate that cannot name a real position.
    pub fn restore_view(&self) -> Result<ProjectionView, super::ProjectionError> {
        let mut view = ProjectionView::new();
        if let Some(cursor) = self.cursor {
            view.seed_cursor(log_ref(cursor)?);
        }
        for state in &self.positions {
            let subject = super::PositionSubject {
                position: state.position,
                position_id: state.position_id,
            };
            view.seed_position(subject, (state.balance, state.index));
        }
        for (index, record) in self.generations.iter().enumerate() {
            let generation = restored_generation(record)?;
            let is_current = self.generations[index + 1..]
                .iter()
                .all(|later| later.entity != record.entity);
            if is_current {
                view.observe_generation(generation);
            } else {
                view.seal_generation(generation);
            }
        }
        Ok(view)
    }
}

/// The state a resume reads back from a committed checkpoint must classify the
/// GHO discount pre-pass exactly as the live pre-pass does, before and after the
/// reserve's vToken link, and from the checkpoint alone (ADR-105 D8, D9).
#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod restore_tests {
    use alloy::primitives::{Address, B256, U256};
    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_db::{
        CheckpointAsset, CheckpointContract, CheckpointGho, CheckpointPositionState,
        CheckpointUser, GenerationActivation as PersistedActivation, GenerationRecord,
        LogCoordinate, ProjectionCheckpointRow, ScaledTokenPosition,
    };

    use super::{classify_discount_prepass, CheckpointState, DiscountPath, DiscountPrepassInputs};
    use crate::projection::{
        CanonicalDelta, ContractRole, DeltaEffect, GenerationActivation, GenerationEntity,
        GenerationRef, ProjectionError, ProjectionView,
    };

    const USER: &str = "0x00000000000000000000000000000000000000AA";
    const GHO_VTOKEN: &str = "0x00000000000000000000000000000000000000BB";
    const POOL: &str = "0x00000000000000000000000000000000000000CC";
    const NEW_POOL: &str = "0x00000000000000000000000000000000000000DD";

    fn coordinate(number: u64, log_index: u64) -> LogCoordinate {
        let anchor =
            ChainAnchor::new(1, number, B256::repeat_byte(0x11), B256::repeat_byte(0x10)).unwrap();
        LogCoordinate::new(anchor, B256::repeat_byte(0x22), 0, log_index)
    }

    fn asset(v_token_revision: u32) -> CheckpointAsset {
        CheckpointAsset {
            asset_id: 1,
            underlying_address: "0x00000000000000000000000000000000000000Ee".to_string(),
            a_token_address: "0x00000000000000000000000000000000000000Ef".to_string(),
            v_token_address: GHO_VTOKEN.to_string(),
            a_token_revision: 1,
            v_token_revision,
            e_mode_category_id: None,
            price_source: None,
            last_update_block: None,
            liquidity_index: U256::ZERO,
            liquidity_rate: U256::ZERO,
            borrow_index: U256::ZERO,
            borrow_rate: U256::ZERO,
        }
    }

    fn user(discount: i64, balance: Option<U256>) -> CheckpointUser {
        CheckpointUser {
            user_id: 1,
            address: USER.to_string(),
            gho_discount: discount,
            stk_aave_balance: balance,
            e_mode: 0,
            isolation_mode_collateral_asset_id: None,
            isolation_mode_debt: U256::ZERO,
        }
    }

    fn row(
        position: LogCoordinate,
        generations: Vec<GenerationRecord>,
        contracts: Vec<CheckpointContract>,
        gho: Option<CheckpointGho>,
        assets: Vec<CheckpointAsset>,
    ) -> ProjectionCheckpointRow {
        ProjectionCheckpointRow {
            market_id: 1,
            journal_sequence: 1,
            position,
            range_end_block: position.anchor().number(),
            encoding_version: super::CHECKPOINT_ENCODING_VERSION.to_string(),
            generations,
            positions: vec![CheckpointPositionState {
                position: ScaledTokenPosition::Debt,
                position_id: 1,
                balance: U256::from(7_u64),
                index: Some(U256::from(2_u64)),
            }],
            assets,
            contracts,
            gho,
            users: vec![user(42, None)],
            asset_configs: Vec::new(),
            user_collaterals: Vec::new(),
            emode_categories: Vec::new(),
        }
    }

    fn unlinked_gho() -> CheckpointGho {
        CheckpointGho {
            gho_id: 1,
            token_id: 9,
            v_token_id: None,
            gho_token_address: "0x00000000000000000000000000000000000000Ff".to_string(),
            v_token_address: None,
            discount_rate_strategy: None,
            discount_token: None,
        }
    }

    fn linked_gho() -> CheckpointGho {
        CheckpointGho {
            v_token_id: Some(10),
            v_token_address: Some(GHO_VTOKEN.to_string()),
            discount_token: Some("0x00000000000000000000000000000000000000E0".to_string()),
            ..unlinked_gho()
        }
    }

    fn class(state: &CheckpointState) -> DiscountPath {
        classify_discount_prepass(state.discount_prepass_inputs(USER))
    }

    /// The two resolution families a rewind restores arrive from the checkpoint
    /// row alone: the users' collateral flags and the e-mode category
    /// definitions travel with the state, and a restored state exposes them for
    /// the recovery writer (payload v4).
    #[test]
    fn a_restored_checkpoint_states_the_collateral_flags_and_emode_categories() {
        let mut stored = row(
            coordinate(100, 7),
            Vec::new(),
            Vec::new(),
            None,
            vec![asset(1)],
        );
        stored.user_collaterals = vec![degenbot_db::CheckpointUserCollateral {
            user_id: 42,
            asset_id: 1,
            enabled: true,
        }];
        stored.emode_categories = vec![degenbot_db::CheckpointEModeCategory {
            category_id: 4,
            label: Some("stablecoins".to_string()),
            ltv: 900,
            liquidation_threshold: 950,
            liquidation_bonus: 1010,
            price_source: None,
        }];

        let state = CheckpointState::restore(&stored);
        assert_eq!(state.user_collaterals(), stored.user_collaterals);
        assert_eq!(state.emode_categories(), stored.emode_categories);
        assert_eq!(
            state.wire().encoding_version,
            super::CHECKPOINT_ENCODING_VERSION
        );
        assert!(
            super::CHECKPOINT_ENCODING_VERSION.ends_with("/v4"),
            "the payload states the version its families belong to"
        );
    }

    /// (a) The transaction-start discount classification before and after the
    /// reserve's vToken link, read from a checkpoint alone.
    #[test]
    fn a_restored_checkpoint_classifies_the_discount_prepass_across_the_gho_link() {
        let pre_link = CheckpointState::restore(&row(
            coordinate(100, 7),
            Vec::new(),
            Vec::new(),
            Some(unlinked_gho()),
            vec![asset(1)],
        ));
        assert_eq!(
            class(&pre_link),
            DiscountPath::NoGhoToken,
            "an unlinked reserve has no discount path at all"
        );

        let post_link = CheckpointState::restore(&row(
            coordinate(100, 9),
            Vec::new(),
            Vec::new(),
            Some(linked_gho()),
            vec![asset(1)],
        ));
        assert_eq!(
            class(&post_link),
            DiscountPath::Settled(42),
            "once the vToken is linked the settled row answers the pre-pass"
        );
        assert_eq!(
            post_link.user(USER).map(|user| user.gho_discount),
            Some(42),
            "the checkpoint states the transaction-start discount it classifies"
        );

        let deprecated = CheckpointState::restore(&row(
            coordinate(100, 9),
            Vec::new(),
            Vec::new(),
            Some(linked_gho()),
            vec![asset(4)],
        ));
        let unknown_user = CheckpointState::restore(&ProjectionCheckpointRow {
            users: Vec::new(),
            ..row(
                coordinate(100, 9),
                Vec::new(),
                Vec::new(),
                Some(linked_gho()),
                vec![asset(4)],
            )
        });
        assert_eq!(
            classify_discount_prepass(unknown_user.discount_prepass_inputs(USER)),
            DiscountPath::Deprecated,
            "a v4+ vToken is the deprecation path, not a chain read"
        );
        assert_eq!(
            deprecated
                .asset_by_address("v_token", GHO_VTOKEN)
                .map(|asset| asset.v_token_revision),
            Some(4),
            "the revision that selects the rule is the checkpoint's asset state"
        );
    }

    /// (b) A same-block endpoint/revision replacement: the restored checkpoint
    /// states the pre-activation generation and endpoint, and the retained
    /// journal delta activates the replacement — with the production generation
    /// rule refusing a stale reduction before it and accepting it at its own
    /// position.
    #[test]
    fn a_restored_checkpoint_states_the_generation_a_same_block_replacement_activates() {
        let entity = GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        };
        let activation = coordinate(100, 9);
        let seed = GenerationRecord {
            entity: entity.encode(),
            activation: PersistedActivation::Seed,
            revision: Some(1),
        };
        let restored = CheckpointState::restore(&row(
            coordinate(100, 8),
            vec![seed.clone()],
            vec![CheckpointContract {
                name: "POOL".to_string(),
                address: POOL.to_string(),
                revision: Some(1),
            }],
            None,
            Vec::new(),
        ));
        assert_eq!(
            restored
                .contract("POOL")
                .map(|contract| contract.address.as_str()),
            Some(POOL),
            "the checkpoint states the endpoint before the replacement"
        );
        assert_eq!(
            restored
                .contract("POOL")
                .and_then(|contract| contract.revision),
            Some(1)
        );

        let view: ProjectionView = restored.restore_view().expect("the view restores");
        let mut view = view;
        let source = degenbot_core::canonical_head::LogRef::new(
            degenbot_core::canonical_head::TxRef::new(
                activation.anchor(),
                activation.tx_hash(),
                activation.tx_index(),
            )
            .unwrap(),
            activation.log_index(),
        );
        // The activating delta carries the replacement generation at its own
        // position, so the view accepts and installs it.
        let revision_two = GenerationRef::activated_at(entity, Some(2), source);
        let activating = CanonicalDelta::new(
            source,
            Some(revision_two),
            DeltaEffect::GhoRefreshDiscount { position_id: 1 },
        );
        view.apply(&activating).expect("the activation is accepted");
        assert_eq!(
            view.generation(entity).and_then(|g| g.revision()),
            Some(2),
            "the replacement is the generation the view reads after activation"
        );
        assert_eq!(
            view.generation(entity).map(|g| g.activation()),
            Some(GenerationActivation::Event(source))
        );

        // A delta reduced under the PRE-activation generation, arriving after it,
        // is the stale reduction the rule refuses.
        let stale = CanonicalDelta::new(
            source,
            Some(GenerationRef::new(entity, 1)),
            DeltaEffect::GhoRefreshDiscount { position_id: 1 },
        );
        assert!(matches!(
            view.apply(&stale),
            Err(ProjectionError::StaleGeneration { .. })
        ));
    }

    /// (b, continued) The post-activation checkpoint restates the replacement
    /// endpoint and generation, so the resume continues under the new version.
    #[test]
    fn a_restored_checkpoint_states_the_generation_after_a_replacement() {
        let entity = GenerationEntity::Contract {
            market_id: 1,
            role: ContractRole::Pool,
        };
        let activation = coordinate(100, 9);
        // The post-activation checkpoint restates the replacement endpoint and
        // generation, so the resume continues under the new version.
        let after = CheckpointState::restore(&row(
            coordinate(100, 9),
            vec![GenerationRecord {
                entity: entity.encode(),
                activation: PersistedActivation::Event(activation),
                revision: Some(2),
            }],
            vec![CheckpointContract {
                name: "POOL".to_string(),
                address: NEW_POOL.to_string(),
                revision: Some(2),
            }],
            None,
            Vec::new(),
        ));
        assert_eq!(
            after
                .contract("POOL")
                .map(|contract| contract.address.as_str()),
            Some(NEW_POOL)
        );
        assert_eq!(
            after.generations()[0].activation,
            PersistedActivation::Event(activation),
            "the generation keeps its activation position across the restore"
        );
        assert_eq!(
            Address::from_slice(&[0_u8; 20]),
            Address::ZERO,
            "the fixture addresses are the zero-slice shape"
        );
        let inputs = DiscountPrepassInputs {
            vtoken: None,
            revision: None,
            settled: None,
        };
        assert_eq!(classify_discount_prepass(inputs), DiscountPath::NoGhoToken);
        assert_eq!(ScaledTokenPosition::Debt.code(), "debt");
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use alloy::primitives::{I256, U256};
    use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};

    use super::*;
    use crate::projection::{
        CanonicalDelta, DeltaEffect, GenerationEntity, GenerationRef, PositionSubject,
        ProjectionView, ScaledTokenPosition,
    };

    fn at(log_index: u64) -> LogRef {
        let anchor = ChainAnchor::new(
            1,
            26_130_441,
            alloy::primitives::B256::repeat_byte(0xaa),
            alloy::primitives::B256::repeat_byte(0x99),
        )
        .unwrap();
        let tx = TxRef::new(anchor, alloy::primitives::B256::repeat_byte(0xbb), 3).unwrap();
        LogRef::new(tx, log_index)
    }

    #[test]
    fn the_cadence_checkpoints_every_unit_that_journals_a_delta() {
        assert!(checkpoint_due(1));
        assert!(checkpoint_due(32));
        assert!(
            !checkpoint_due(0),
            "a unit with no delta has no cursor to capture"
        );
    }

    #[test]
    fn a_checkpoint_captures_the_generations_the_view_reads_under() {
        let substrate = ChunkSubstrate::lazy(1);
        let empty = ProjectionView::new();
        assert_eq!(
            CheckpointState::capture(&empty, &substrate),
            CheckpointState::default()
        );

        let mut view = ProjectionView::new();
        view.observe_generation(GenerationRef::new(
            GenerationEntity::ScaledToken {
                asset_id: 2,
                position: ScaledTokenPosition::Collateral,
            },
            1,
        ));
        view.observe_generation(GenerationRef::seed(GenerationEntity::Contract {
            market_id: 1,
            role: crate::projection::ContractRole::Pool,
        }));

        let state = CheckpointState::capture(&view, &substrate);
        let entities: Vec<&str> = state
            .generations()
            .iter()
            .map(|generation| generation.entity.as_str())
            .collect();
        assert_eq!(
            entities,
            vec![
                "contract{market=1,role=pool}",
                "scaled-token{asset=2,leg=collateral}"
            ],
            "the checkpoint states the view's generations in entity order"
        );
        assert_eq!(state.wire().encoding_version, CHECKPOINT_ENCODING_VERSION);
        assert_eq!(
            state.wire().generations.len(),
            state.generations().len(),
            "the wire form borrows the captured state"
        );
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn a_checkpoint_records_the_generation_a_replacement_sealed() {
        let substrate = ChunkSubstrate::lazy(1);
        let entity = GenerationEntity::ScaledToken {
            asset_id: 2,
            position: ScaledTokenPosition::Collateral,
        };
        let mut view = ProjectionView::new();
        view.observe_generation(GenerationRef::new(entity, 3));
        view.seal_generation(GenerationRef::new(entity, 1));
        assert_eq!(
            view.generation(entity),
            Some(GenerationRef::new(entity, 3)),
            "a sealed generation never displaces the generation the view reads under"
        );

        let state = CheckpointState::capture(&view, &substrate);
        let stated: Vec<Option<u32>> = state
            .generations()
            .iter()
            .map(|generation| generation.revision)
            .collect();
        assert_eq!(
            stated,
            vec![Some(1), Some(3)],
            "the sealed generation the chunk began under is recorded beside the replacement"
        );

        let restored = state.restore_view().expect("the checkpoint restores");
        assert_eq!(
            restored.generation(entity),
            Some(GenerationRef::new(entity, 3)),
            "a restore reads under the replacement, not the sealed generation"
        );
    }

    #[test]
    fn a_checkpoint_captures_every_position_state_the_view_holds() {
        let substrate = ChunkSubstrate::lazy(1);
        let mut view = ProjectionView::new();
        let debt = PositionSubject {
            position: ScaledTokenPosition::Debt,
            position_id: 9,
        };
        view.seed_position(debt, (U256::from(1_000_u64), Some(U256::from(2_u64))));
        view.apply(&CanonicalDelta::new(
            at(7),
            None,
            DeltaEffect::ScaledBalance {
                subject: debt,
                balance_delta: I256::try_from(5_i64).unwrap(),
                index: U256::from(3_u64),
            },
        ))
        .unwrap();

        let state = CheckpointState::capture(&view, &substrate);
        assert_eq!(
            state.positions(),
            &[CheckpointPositionState {
                position: ScaledTokenPosition::Debt,
                position_id: 9,
                balance: U256::from(1_005_u64),
                index: Some(U256::from(3_u64)),
            }],
            "the checkpoint states the advanced balance and the max-with-prev index"
        );
        assert_eq!(state.positions().len(), 1);
        assert!(state.generations().is_empty());
        assert!(state.assets().is_empty());
        assert!(state.contracts().is_empty());
        assert!(state.gho().is_none());
    }

    /// The generation a checkpoint states as current is positional, not
    /// revision-ordered: a replacement that moves the revision BACKWARD (or an
    /// activation kind that sorts first) must not make a restore adopt the
    /// sealed generation, and a recapture of the restored view must keep the
    /// sealed lineage.
    #[test]
    #[expect(clippy::expect_used)]
    fn a_checkpoint_restores_the_current_generation_and_keeps_the_sealed_lineage() {
        let substrate = ChunkSubstrate::lazy(1);
        let entity = GenerationEntity::ScaledToken {
            asset_id: 2,
            position: ScaledTokenPosition::Collateral,
        };
        let current = GenerationRef::new(entity, 1);
        let mut view = ProjectionView::new();
        view.observe_generation(current);
        view.seal_generation(GenerationRef::new(entity, 5));
        view.seal_generation(GenerationRef::activated_at(entity, Some(3), at(7)));

        let state = CheckpointState::capture(&view, &substrate);
        let stated: Vec<Option<u32>> = state
            .generations()
            .iter()
            .map(|generation| generation.revision)
            .collect();
        assert_eq!(
            stated,
            vec![Some(3), Some(5), Some(1)],
            "the sealed generations precede the current one, whose lower revision sorts last"
        );

        let restored = state.restore_view().expect("the checkpoint restores");
        assert_eq!(
            restored.generation(entity),
            Some(current),
            "a restore reads under the generation the checkpoint stated as current"
        );
        let mut sealed: Vec<Option<u32>> = restored
            .sealed_generations()
            .map(|generation| generation.revision())
            .collect();
        sealed.sort_unstable();
        assert_eq!(
            sealed,
            vec![Some(3), Some(5)],
            "the restored view keeps the sealed lineage"
        );

        let recaptured = CheckpointState::capture(&restored, &substrate);
        assert_eq!(
            recaptured, state,
            "a recapture after restore states the same current-vs-sealed lineage"
        );
    }
}
