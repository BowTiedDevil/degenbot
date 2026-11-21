use super::util::{clone_scaled_event, EventDisposition, EventOwnership, LiquidationWindow};
use super::{
    aave_event_decoder, log_idx_value, Address, HashMap, HashSet, Log, Operation, OperationType,
    ParseError, ScaledTokenEvent, ScaledTokenEventType, TransactionOperationsParser, U256,
};

// ── the collateral scan's diagnosis ───────────────────────────────────────

/// How many rejected in-window log indices a refusal names before it summarises
/// the rest as a count: the payload is one log line, not a candidate dump.
const REFUSAL_EXEMPLAR_LIMIT: usize = 8;

/// The slot a liquidation's collateral events fill: the user's debit, or the
/// scaled report of a movement to the liquidator / treasury.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollateralSlot {
    Debit,
    Transfer,
}

/// Why the collateral scan skipped one scaled candidate, in the scan's own
/// filter order. A candidate is counted under the first reason that refuses it,
/// so one refusal's tallies sum to the events the scan walked.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollateralSkip {
    /// An earlier operation claimed a log emitted inside this window.
    AlreadyClaimed = 0,
    /// Emitted outside this liquidation's own sub-call.
    OutsideWindow = 1,
    /// Not this liquidation's collateral aToken.
    WrongToken = 2,
    /// The log's ownership role is neither a collateral debit nor a scaled
    /// movement report (a companion report, an index-less mirror, another role).
    WrongDisposition = 3,
    /// The event kind does not fill the disposition's slot.
    WrongKind = 4,
    /// Another user's event.
    WrongUser = 5,
    /// A `CollateralMint` that is not the interest-exceeds debit: that slot
    /// needs `amount < balance_increase`.
    NotNetDebit = 6,
}

impl CollateralSkip {
    /// Every reason, in discriminant order — the order a refusal renders.
    const ALL: [Self; 7] = [
        Self::AlreadyClaimed,
        Self::OutsideWindow,
        Self::WrongToken,
        Self::WrongDisposition,
        Self::WrongKind,
        Self::WrongUser,
        Self::NotNetDebit,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::AlreadyClaimed => "already claimed",
            Self::OutsideWindow => "outside window",
            Self::WrongToken => "wrong token",
            Self::WrongDisposition => "wrong disposition",
            Self::WrongKind => "wrong kind",
            Self::WrongUser => "wrong user",
            Self::NotNetDebit => "not a net debit",
        }
    }

    const fn slot(self) -> usize {
        self as usize
    }
}

/// One liquidation's collateral scan: the events it collected, and the reason it
/// skipped every other scaled candidate. On success the tally is a few integer
/// increments; on refusal it is the whole diagnostic payload.
#[derive(Debug, Default)]
pub(super) struct CollateralScanTally {
    matched: usize,
    skipped: [usize; CollateralSkip::ALL.len()],
    rejected_in_window: Vec<u64>,
}

impl CollateralScanTally {
    fn record(&mut self, reason: CollateralSkip, log_index: u64) {
        self.skipped[reason.slot()] += 1;
        if reason != CollateralSkip::OutsideWindow {
            self.rejected_in_window.push(log_index);
        }
    }

    /// Every scaled event the scan walked: matched or skipped exactly once.
    fn scanned(&self) -> usize {
        self.matched + self.skipped.iter().sum::<usize>()
    }

    fn in_window(&self) -> usize {
        self.scanned() - self.skipped[CollateralSkip::OutsideWindow.slot()]
    }

    /// The refusal payload. Its consumer writes it into one `AAVE-PARSE-FAIL`
    /// log line, so the candidates appear as counts plus at most
    /// [`REFUSAL_EXEMPLAR_LIMIT`] rejected in-window log indices.
    pub(super) fn refusal(&self, user: Address, anchor: u64, window: LiquidationWindow) -> String {
        let (start, end) = window.bounds();
        let skipped = CollateralSkip::ALL
            .iter()
            .map(|reason| format!("{}: {}", reason.label(), self.skipped[reason.slot()]))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Expected at least 1 collateral event (burn or transfer) for liquidation. \
             User: {user}, anchor: LiquidationCall log {anchor}, window: {start}..{end}, \
             scaled events: {}, in-window: {}, matched: {}, skipped: {{{skipped}}}, \
             rejected in-window logs: {}",
            self.scanned(),
            self.in_window(),
            self.matched,
            self.exemplars(),
        )
    }

    fn exemplars(&self) -> String {
        if self.rejected_in_window.is_empty() {
            return "none".to_string();
        }
        let shown = self
            .rejected_in_window
            .iter()
            .take(REFUSAL_EXEMPLAR_LIMIT)
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let hidden = self
            .rejected_in_window
            .len()
            .saturating_sub(REFUSAL_EXEMPLAR_LIMIT);
        if hidden == 0 {
            shown
        } else {
            format!("{shown} (+{hidden} more)")
        }
    }
}

/// The collateral events one liquidation owns, with the tally that explains
/// every candidate it did not collect.
pub(super) struct CollateralScan<'b, 'a> {
    pub(super) burn: Option<&'b ScaledTokenEvent<'a>>,
    pub(super) transfers: Vec<&'b ScaledTokenEvent<'a>>,
    pub(super) tally: CollateralScanTally,
}

impl CollateralScan<'_, '_> {
    /// `true` when the liquidation owns no collateral event at all.
    pub(super) fn is_empty(&self) -> bool {
        self.burn.is_none() && self.transfers.is_empty()
    }
}

impl<'a> TransactionOperationsParser<'a, '_> {
    // ── the liquidation engine ────────────────────────────────────────────

    // ── the liquidation engine fns ──────────────────────────────────

    /// pre-analyze
    /// liquidations to detect multi-liquidation scenarios. Returns a mapping
    /// of `(user, debt_v_token_address)` → `liquidation_count`. This allows
    /// proper disambiguation when the same user is liquidated multiple times
    /// with the same debt asset in one transaction.
    ///
    /// Parity-gate note: the Python reads `decode_address(ev["topics"][3])`
    /// (user) + `decode_address(ev["topics"][2])` (`debt_asset`) per
    /// `LiquidationCall` event → resolves `debt_asset` → vToken via
    /// `_get_v_token_for_asset`; we mirror the same per-event vToken resolution.
    fn analyze_liquidation_scenarios(
        &mut self,
        all_events: &[&Log],
    ) -> Result<HashMap<(Address, Address), usize>, ParseError> {
        let mut counts: HashMap<(Address, Address), usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            let debt_asset = Address::from_word(topics[2]);
            let v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no vToken for debt_asset {debt_asset} in market {}",
                    self.market_id
                ))
            })?;
            *counts.entry((user, v_token_address)).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// count total
    /// liquidations per user (not per user+asset pair). Returns `user` →
    /// `total_liquidations`.
    ///
    /// When a user has exactly 1 liquidation, ALL debt burns for that user
    /// belong to that single liquidation. This handles bad-debt liquidations
    /// where the protocol burns multiple debt positions via `_burnBadDebt()`.
    /// When a user has multiple liquidations, use asset-specific matching to
    /// disambiguate which burns belong to which liquidation.
    pub(super) fn analyze_user_liquidation_count(all_events: &[&Log]) -> HashMap<Address, usize> {
        let mut counts: HashMap<Address, usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            *counts.entry(user).or_insert(0) += 1;
        }
        counts
    }

    /// the parity-critical
    /// `SINGLE/COMBINED_BURN/SEPARATE_BURNS` pattern-detection debt-burn
    /// collector.
    ///
    /// Collection strategy (port-EXACT):
    /// - Single liquidation per user: collect ALL debt burns (no asset filter)
    ///   → handles bad-debt liquidations where `_burnBadDebt()` burns all debt positions.
    /// - Multiple liquidations per user: use asset filter + sequential matching
    ///   to disambiguate which burns belong to which liquidation.
    ///   - **`SEPARATE_BURNS`** (`liquidation_count_for_asset == total_burn_count`):
    ///     burn `i` belongs to liquidation `i`; return the burn at
    ///     `liquidation_position`.
    ///   - **`COMBINED_BURN`** (`liquidation_count_for_asset > total_burn_count` +
    ///     `liquidation_position == 0`): all burns go to the first liquidation.
    ///
    /// # Parity-gate note (the `assigned_indices.add(ev.index)` quirk)
    ///
    /// The Python also calls `assigned_indices.add(ev.index)` to store the
    /// Burn event's `index` field (a ray-scale u256 liquidity-index value) in
    /// `assigned_indices`. This storage is **dead-code** — every
    /// `in assigned_indices` query is `ev.event["logIndex"] in assigned_indices`
    /// (small-int logIndex), never `ev.index`. The `add(ev.index)` stores
    /// ray-scale values that are never matched; we omit the assignment (a u64
    /// set can't hold ray values anyway) but preserve the assertions
    /// `assert ev.index is not None; assert ev.index > 0` (the runtime panic on
    /// ill-formed events) as `debug_assert!`. Output parity verified: the
    /// `assigned_indices` state never affects the parsed Operation output.
    pub(super) fn collect_debt_burns<'b>(
        user: Address,
        debt_v_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        liquidation_analysis: &HashMap<(Address, Address), usize>,
        user_liquidation_count: usize,
        liquidation_position: usize,
    ) -> Vec<&'b ScaledTokenEvent<'a>>
    where
        'a: 'b,
    {
        let mut burns: Vec<&ScaledTokenEvent<'a>> = Vec::new();
        if user_liquidation_count == 1 {
            // Single liquidation per user: collect ALL debt burns (no asset filter).
            let mut candidate_burns: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    !assigned_indices.contains(&ev.log_index)
                        && ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                })
                .collect();
            candidate_burns.sort_by_key(|e| e.log_index);
            for ev in candidate_burns {
                burns.push(ev);
                assigned_indices.insert(ev.log_index);
                let idx = ev.index.unwrap_or(U256::ZERO);
                debug_assert!(
                    !idx.is_zero(),
                    "DebtBurn index must be > 0 (parity: assert ev.index > 0)"
                );
                // NB: the Python's `assigned_indices.add(ev.index)` here is
                // dead-code (no later `in assigned_indices` query hits the
                // ray-scale `index` field); omitted (u64 set can't hold it).
            }
        } else {
            #[expect(clippy::expect_used)] // required on the multi-liquidation path (documented)
            let debt_v =
                debt_v_token_address.expect("multi-liquidation requires debt_v_token_address");
            let liquidation_count_for_asset = liquidation_analysis
                .get(&(user, debt_v))
                .copied()
                .unwrap_or(1);
            let is_multi_liquidation = liquidation_count_for_asset > 1;
            // Get ALL burns for this (user, debt_asset) to determine pattern.
            // Don't filter by assigned_indices yet — we need total count for
            // pattern detection (parity: Python lines 1580-1586).
            let mut all_burns_for_asset: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                        && ev.token_address == debt_v
                })
                .collect();
            all_burns_for_asset.sort_by_key(|e| e.log_index);
            // Now get only unassigned burns for assignment.
            let candidate_burns: Vec<&ScaledTokenEvent<'a>> = all_burns_for_asset
                .iter()
                .copied()
                .filter(|ev| !assigned_indices.contains(&ev.log_index))
                .collect();
            // Determine pattern: COMBINED_BURN vs SEPARATE_BURNS.
            let total_burn_count = all_burns_for_asset.len();
            if is_multi_liquidation && !candidate_burns.is_empty() {
                debug_assert!(
                    liquidation_count_for_asset >= total_burn_count,
                    "parity: assert liquidation_count_for_asset >= total_burn_count"
                );
                if liquidation_count_for_asset == total_burn_count {
                    // SEPARATE_BURNS pattern: each liquidation gets exactly one burn.
                    debug_assert!(
                        liquidation_position < total_burn_count,
                        "parity: assert liquidation_position < total_burn_count"
                    );
                    let target_burn = all_burns_for_asset[liquidation_position];
                    debug_assert!(
                        !assigned_indices.contains(&target_burn.log_index),
                        "SEPARATE_BURNS target burn already assigned"
                    );
                    burns.push(target_burn);
                    assigned_indices.insert(target_burn.log_index);
                    let idx = target_burn.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                } else if liquidation_position == 0 {
                    // COMBINED_BURN pattern: more liquidations than burns.
                    // All burns go to the first liquidation.
                    for ev in candidate_burns {
                        burns.push(ev);
                        assigned_indices.insert(ev.log_index);
                        let idx = ev.index.unwrap_or(U256::ZERO);
                        debug_assert!(!idx.is_zero());
                    }
                }
            } else {
                // Single liquidation or no burns: collect all available burns.
                for ev in candidate_burns {
                    burns.push(ev);
                    assigned_indices.insert(ev.log_index);
                    let idx = ev.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                }
            }
        }
        burns
    }

    /// Collect the collateral events this liquidation owns: the semantic
    /// collateral `Burn` (the user's debit) and the `BalanceTransfer` reports for
    /// this liquidation's collateral asset (the movement to the liquidator, or
    /// the protocol-fee move to the treasury).
    ///
    /// Ownership is two filters, and both are required. [`LiquidationWindow`]
    /// bounds the scan to this liquidation's own sub-call, so two liquidations
    /// of the same user against the same collateral asset never see each
    /// other's legs. [`EventOwnership`] then excludes the zero-address
    /// companion reports (their delta belongs to the semantic event) and the
    /// index-less ERC20 reports (a liquidation movement is carried by the
    /// `Burn`/`Mint` and by the scaled `BalanceTransfer`). Nothing is written
    /// into the claim set: a report excluded here is excluded by its
    /// disposition, so this collector only *skips* logs an earlier operation
    /// already claimed.
    ///
    /// The debit slot is the `CollateralBurn` **or** the interest-exceeds
    /// `CollateralMint` (`value < balance_increase`): a liquidation whose
    /// seized collateral is smaller than the interest accrued since the
    /// position's last action is emitted as that net-burn `Mint`, never as a
    /// `Burn`.
    ///
    /// Returns the [`CollateralScan`]: the debit, the scaled transfer reports,
    /// and the tally that explains every candidate this liquidation did not
    /// collect. The tally rides the same walk, so a refusal can name the reason
    /// each in-window candidate was skipped without a second filter.
    pub(super) fn collect_collateral_events<'b>(
        user: Address,
        collateral_a_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        ownership: &EventOwnership,
        window: LiquidationWindow,
        assigned_indices: &HashSet<u64>,
    ) -> CollateralScan<'b, 'a>
    where
        'a: 'b,
    {
        let mut scan = CollateralScan {
            burn: None,
            transfers: Vec::new(),
            tally: CollateralScanTally::default(),
        };
        for ev in scaled_events {
            match Self::collateral_slot(
                ev,
                user,
                collateral_a_token_address,
                ownership,
                window.owns(ev.log_index),
                assigned_indices,
            ) {
                Ok(CollateralSlot::Debit) => {
                    scan.burn = Some(ev);
                    scan.tally.matched += 1;
                }
                Ok(CollateralSlot::Transfer) => {
                    scan.transfers.push(ev);
                    scan.tally.matched += 1;
                }
                Err(reason) => scan.tally.record(reason, ev.log_index),
            }
        }
        scan
    }

    /// The collateral slot one scaled event fills for this liquidation, or the
    /// one reason the scan skips it. The checks run in filter order — the
    /// window first, so `already claimed` only ever means claimed *inside* this
    /// window — and each candidate is refused, and counted, once.
    fn collateral_slot(
        ev: &ScaledTokenEvent<'_>,
        user: Address,
        collateral_a_token_address: Option<Address>,
        ownership: &EventOwnership,
        in_window: bool,
        assigned_indices: &HashSet<u64>,
    ) -> Result<CollateralSlot, CollateralSkip> {
        // The window is coarsest: an out-of-window log is another
        // liquidation's leg whether or not it is already claimed, and saying
        // "already claimed" here would read as a starvation inside THIS window.
        if !in_window {
            return Err(CollateralSkip::OutsideWindow);
        }
        if assigned_indices.contains(&ev.log_index) {
            return Err(CollateralSkip::AlreadyClaimed);
        }
        // Match collateral events only if they belong to this liquidation's
        // collateral asset (prevents incorrect matching when a user is
        // liquidated multiple times with different collateral assets).
        if collateral_a_token_address.is_some_and(|expected| ev.token_address != expected) {
            return Err(CollateralSkip::WrongToken);
        }
        match ownership
            .disposition(ev.log_index)
            .ok_or(CollateralSkip::WrongDisposition)?
        {
            EventDisposition::SemanticBurn => {
                if ev.event_type != ScaledTokenEventType::CollateralBurn {
                    return Err(CollateralSkip::WrongKind);
                }
                if ev.user_address != user {
                    return Err(CollateralSkip::WrongUser);
                }
                Ok(CollateralSlot::Debit)
            }
            EventDisposition::SemanticMint => {
                if ev.event_type != ScaledTokenEventType::CollateralMint {
                    return Err(CollateralSkip::WrongKind);
                }
                if ev.user_address != user {
                    return Err(CollateralSkip::WrongUser);
                }
                if ev.amount >= ev.balance_increase.unwrap_or_default() {
                    return Err(CollateralSkip::NotNetDebit);
                }
                Ok(CollateralSlot::Debit)
            }
            // The scaled report of a collateral movement is the only transfer
            // report a liquidation applies: the index-less ERC20 report of the
            // same movement is its mirror, and a companion report belongs to its
            // semantic event.
            EventDisposition::BalanceTransfer { .. } => {
                if ev.event_type != ScaledTokenEventType::CollateralTransfer {
                    return Err(CollateralSkip::WrongKind);
                }
                if ev.user_address != user {
                    return Err(CollateralSkip::WrongUser);
                }
                Ok(CollateralSlot::Transfer)
            }
            EventDisposition::StandaloneTransfer { .. }
            | EventDisposition::DuplicateCarrier { .. }
            | EventDisposition::Companion { .. } => Err(CollateralSkip::WrongDisposition),
        }
    }

    /// the gnarly ~160-LoC
    /// fn that builds an `Operation { operation_type: Liquidation, ... }` (or
    /// `GhoLiquidation` if the debt asset is GHO). Coordinates:
    /// - `analyze_liquidation_scenarios` + `analyze_user_liquidation_count` (the
    ///   pre-analysis dicts over `all_events`).
    /// - `collect_debt_burns` (the `SINGLE/COMBINED_BURN/SEPARATE_BURNS` detection).
    /// - `collect_collateral_events` (the `collateral_burn` vs `collateral_transfers` split).
    /// - the `debt_mint` net-debt-increase branch (accrued interest > repayment).
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Substrate`] if either the aToken or vToken sibling
    /// lookup fails (parity: `assert debt_v_token_address is not None`).
    ///
    /// Returns [`ParseError::NoMatch`] when the liquidation owns no collateral
    /// event; its payload names the `LiquidationCall` anchor, the liquidation
    /// window, and the collateral scan's skip tally, so the refusal states its
    /// own cause.
    #[expect(clippy::too_many_lines)] // 5-nested-branch fn — intrinsic
    pub(super) fn create_liquidation_operation(
        &mut self,
        operation_id: u32,
        liquidation_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        ownership: &EventOwnership,
        all_events: &[&'a Log],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let topics = liquidation_event.topics();
        let collateral_asset = Address::from_word(topics[1]);
        let debt_asset = Address::from_word(topics[2]);
        let user = Address::from_word(topics[3]);
        // Extract debtToCover from LiquidationCall event data (word 0).
        let data = liquidation_event.data().data.as_ref();
        let mut buf = [0u8; 32];
        if data.len() >= 32 {
            buf.copy_from_slice(&data[0..32]);
        }
        let debt_to_cover = U256::from_be_bytes::<32>(buf);
        let is_gho = self.gho_token_address == Some(debt_asset);
        let collateral_a_token_address = self.get_a_token_for_asset(collateral_asset)?;
        let debt_v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "no vToken for debt_asset {debt_asset} in market {}",
                self.market_id
            ))
        })?;
        // Pre-analyze liquidations to detect multi-liquidation scenarios.
        let liquidation_analysis = self.analyze_liquidation_scenarios(all_events)?;
        let user_liquidation_analysis = Self::analyze_user_liquidation_count(all_events);
        let user_liquidation_count = user_liquidation_analysis.get(&user).copied().unwrap_or(1);
        // Calculate this liquidation's position among all liquidations for
        // this (user, debt_asset) — sequential matching; burn[i] belongs to
        // liquidation[i]. Count LiquidationCall events before this one with
        // the same (user, debt_asset).
        let this_log_index = log_idx_value(liquidation_event);
        let mut liquidation_position = 0usize;
        for ev in all_events {
            if ev.topics().first() != Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if ev.topics().len() < 4 {
                continue;
            }
            if Address::from_word(ev.topics()[3]) != user {
                continue;
            }
            if Address::from_word(ev.topics()[2]) != debt_asset {
                continue;
            }
            if log_idx_value(ev) < this_log_index {
                liquidation_position += 1;
            }
        }
        // Collect debt burns (SINGLE/COMBINED_BURN/SEPARATE_BURNS detection).
        let debt_burns = Self::collect_debt_burns(
            user,
            Some(debt_v_token_address),
            scaled_events,
            assigned_indices,
            &liquidation_analysis,
            user_liquidation_count,
            liquidation_position,
        );
        // Collect collateral events (burns + transfers) from this
        // liquidation's own sub-call.
        let window = LiquidationWindow::ending_at(all_events, this_log_index);
        let scan = Self::collect_collateral_events(
            user,
            collateral_a_token_address,
            scaled_events,
            ownership,
            window,
            assigned_indices,
        );
        // A liquidation requires at least one collateral event.
        if scan.is_empty() {
            return Err(ParseError::NoMatch(scan.tally.refusal(
                user,
                this_log_index,
                window,
            )));
        }
        let CollateralScan {
            burn: collateral_burn,
            transfers: collateral_transfers,
            ..
        } = scan;
        // Find debt mint events that represent net debt increase during
        // liquidation (when accrued interest > debt repayment:
        // balance_increase > amount).
        let mut debt_mint: Option<&ScaledTokenEvent<'a>> = None;
        for scaled_event in scaled_events {
            if assigned_indices.contains(&scaled_event.log_index) {
                continue;
            }
            if scaled_event.user_address != user {
                continue;
            }
            if is_gho && scaled_event.event_type != ScaledTokenEventType::GhoDebtMint {
                continue;
            }
            if !is_gho && scaled_event.event_type != ScaledTokenEventType::DebtMint {
                continue;
            }
            // Match debt mint events only if they belong to this liquidation's
            // debt asset (parity: assert event_token_address == debt_v_token_address).
            if scaled_event.token_address != debt_v_token_address {
                continue;
            }
            let bal_inc = scaled_event.balance_increase.unwrap_or(U256::ZERO);
            debug_assert!(
                bal_inc > scaled_event.amount,
                "parity: balance_increase > amount for net-debt-increase mint"
            );
            debt_mint = Some(scaled_event);
            break;
        }
        // Assemble scaled_token_events + balance_transfer_events.
        let mut scaled_token_events: Vec<ScaledTokenEvent<'a>> = Vec::new();
        let mut balance_transfer_events: Vec<&'a Log> = Vec::new();
        // Note: debt_burns may be empty for flash loan liquidations or when
        // interest > repayment; debt_mint is set when interest > repayment;
        // collateral_burn may be None when collateral is transferred to treasury.
        if let Some(burn) = collateral_burn {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        // Add all debt burns (primary and secondary).
        for burn in &debt_burns {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        if let Some(mint) = debt_mint {
            scaled_token_events.push(clone_scaled_event(mint));
        }
        if !collateral_transfers.is_empty() {
            // Add all collateral transfers to scaled_token_events. Both
            // ERC20 Transfers (index=0) and BalanceTransfer events (index>0)
            // are collateral events that should be validated together.
            for transfer in &collateral_transfers {
                scaled_token_events.push(clone_scaled_event(transfer));
                // Track BalanceTransfer events separately so ERC20 Transfers
                // can use them for proper scaling during processing.
                let idx = transfer.index.unwrap_or(U256::ZERO);
                if !idx.is_zero() {
                    balance_transfer_events.push(transfer.log);
                }
            }
        }
        let op_type = if is_gho {
            OperationType::GhoLiquidation
        } else {
            OperationType::Liquidation
        };
        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(liquidation_event),
            scaled_events: scaled_token_events,
            transfer_events: Vec::new(),
            balance_transfer_events,
            minted_to_treasury_amount: None,
            debt_to_cover: Some(debt_to_cover),
            validation_errors: Vec::new(),
        })
    }
}
