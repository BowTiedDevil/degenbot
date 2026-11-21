use super::{
    aave_event_decoder, address_to_checksum_string, Address, HashMap, HashSet, Log,
    ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType, U256,
};

/// `true` if the log is a `MintedToTreasury` event.
pub(super) fn is_minted_to_treasury_log(log: &Log) -> bool {
    log.topics().first() == Some(&aave_event_decoder::AAVE_MINTED_TO_TREASURY_TOPIC)
}

/// `true` if the log is a plain ERC20 `Transfer` event.
pub(super) fn is_erc20_transfer_log(log: &Log) -> bool {
    log.topics().first() == Some(&aave_event_decoder::ERC20_TRANSFER_TOPIC)
}

/// Read the `logIndex` of a `Log` as a `u64` (`0` if absent — every RPC-fetched
/// receipt has one).
pub(crate) fn log_idx_value(log: &Log) -> u64 {
    log.log_index.unwrap_or(0)
}

/// Convert an alloy `Address` to the EIP-55 checksummed hex string the
/// `erc20_tokens.address` / `aave_v3_users.address` VARCHAR(42) columns
/// store, matching the config path's `checksum()` (`config_dispatch.rs`).
/// MUST be consistent across both
/// paths: the `SQLite` columns use BINARY collation (case-sensitive), so a
/// lowercase lookup against a checksummed-seeded row would miss + create a
/// duplicate (the parity-gate byte-divergence — duplicate lowercase users).
pub(crate) fn addr_to_hex(addr: Address) -> String {
    address_to_checksum_string(&addr)
}

/// Parse a hex string (with or without `0x` prefix) to an alloy `Address`.
/// Returns `None` on malformed input.
pub(crate) fn parse_address(s: &str) -> Option<Address> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.len() != 40 {
        return None;
    }
    Address::parse_checksummed(s, None).ok().or_else(|| {
        // Fall back to case-insensitive parse (the DB-stored address may not
        // be EIP-55 checksum-encoded — the Python stores lowercase, so the
        // `parse_checksummed` strict check would fail; alloy's
        // `Address::from_str` accepts the lowercase form).
        let bytes = alloy::hex::decode(trimmed).ok()?;
        if bytes.len() != 20 {
            return None;
        }
        let arr: [u8; 20] = bytes.try_into().ok()?;
        Some(Address::from(arr))
    })
}

/// Mark an `event`'s `log_index` assigned (helper used by the supply/withdraw/
/// borrow/repay builders).
pub(super) fn assigned_log_idx_for_event(assigned: &mut HashSet<u64>, ev: &ScaledTokenEvent<'_>) {
    assigned.insert(ev.log_index);
}

/// Clone a `ScaledTokenEvent` reference into an owned `ScaledTokenEvent` (the
/// `Operation.scaled_events` vec owns by-value; the source lifetime still
/// ties to the input `&[&Log]` slice — the `log: &'a Log` field is
/// preserved across the clone).
pub(super) fn clone_scaled_event<'a>(ev: &ScaledTokenEvent<'a>) -> ScaledTokenEvent<'a> {
    ScaledTokenEvent {
        log: ev.log,
        decoded: ev.decoded.clone(),
        event_type: ev.event_type,
        token_address: ev.token_address,
        user_address: ev.user_address,
        caller_address: ev.caller_address,
        from_address: ev.from_address,
        target_address: ev.target_address,
        amount: ev.amount,
        balance_increase: ev.balance_increase,
        index: ev.index,
        log_index: ev.log_index,
    }
}

/// Same as `clone_scaled_event`, taking a reference (used to alias-borrow).
pub(super) fn clone_scaled_event_from_ref<'a>(ev: &ScaledTokenEvent<'a>) -> ScaledTokenEvent<'a> {
    clone_scaled_event(ev)
}

/// The amount-match tolerance applied to a `u64`-magnitude comparison.
#[must_use]
pub fn amounts_match_with_tolerance(calculated: U256, expected: U256, tolerance: u64) -> bool {
    if calculated == expected {
        return true;
    }
    let tol = U256::from(tolerance);
    if calculated > expected {
        calculated - expected <= tol
    } else {
        expected - calculated <= tol
    }
}

/// Bare-bytes comparison helper — repeated in a handful of test expressions;
/// kept here for parity (the Python field-tests on a similar helper).
fn _amounts_match_eq(a: U256, b: U256) -> bool {
    a == b
}
/// Whether two transfer-report types describe the same balance movement: the
/// index-less ERC20 report of an aToken/vToken transfer and the index-bearing
/// `BalanceTransfer` report of the same movement.
#[must_use]
pub(super) fn are_compatible_transfer_types(
    ev1: ScaledTokenEventType,
    ev2: ScaledTokenEventType,
) -> bool {
    use ScaledTokenEventType::{
        CollateralTransfer, DebtTransfer, Erc20CollateralTransfer, Erc20DebtTransfer,
    };
    let collateral_pair = (CollateralTransfer, Erc20CollateralTransfer);
    let debt_pair = (DebtTransfer, Erc20DebtTransfer);
    (ev1, ev2) == collateral_pair
        || (ev1, ev2) == debt_pair
        || (ev2, ev1) == collateral_pair
        || (ev2, ev1) == debt_pair
}

/// Which semantic event a [`EventDisposition::Companion`] report mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompanionSide {
    Mint,
    Burn,
}

/// The single ownership role of one decoded scaled-token log.
///
/// Aave reports one balance movement as up to two logs — the index-less ERC20
/// `Transfer` (underlying units) and the index-bearing `BalanceTransfer` (scaled
/// units) — and emits a zero-address `Transfer` companion for every semantic
/// `Mint`/`Burn`. [`EventOwnership::classify`] resolves those relations once,
/// before any `Operation` exists, so operation builders consume a decision
/// instead of re-deriving pairing rules at each call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EventDisposition {
    /// A `Mint`: the event itself carries its credit delta.
    SemanticMint,
    /// A `Burn`: the event itself carries its debit delta.
    SemanticBurn,
    /// A movement's index-less ERC20 `Transfer` report (underlying units), with
    /// the scaled `BalanceTransfer` report of the same movement when the token
    /// emitted one. This report is what a standalone
    /// `BALANCE_TRANSFER`/`STKAAVE_TRANSFER` operation carries.
    StandaloneTransfer { scaled_partner: Option<u64> },
    /// A movement's index-bearing `BalanceTransfer` report (scaled units), with
    /// the ERC20 `Transfer` report of the same movement when one was decoded.
    /// Never a standalone operation: the operation that owns the movement
    /// consumes this report (a liquidation fee transfer, or the scaling partner
    /// of the ERC20 carrier).
    BalanceTransfer { underpinned_by: Option<u64> },
    /// A repeated underlying-unit report of a movement whose scaled report is
    /// already owned by the named carrier. Typed benign ignore: only the first
    /// carrier can emit a standalone operation, and a duplicate never emits —
    /// regardless of whether the scaled report has been claimed yet.
    DuplicateCarrier { owner: u64 },
    /// A zero-address report whose delta is already carried by the named
    /// semantic event. Typed benign ignore: the log never becomes an operation
    /// and never becomes a transfer-operation companion.
    Companion { owner: u64, side: CompanionSide },
}

/// One transaction's per-log ownership decisions, keyed by log index.
#[derive(Debug, Default)]
pub(super) struct EventOwnership {
    dispositions: HashMap<u64, EventDisposition>,
}

impl EventOwnership {
    /// Classify every decoded log once. `events` is ordered by log index (the
    /// parser sorts before classifying), so companion and pairing resolution is
    /// deterministic and stable:
    ///
    /// - companion assignment is injective: each zero-address report claims a
    ///   distinct semantic `Mint`/`Burn`, so two companions can never name the
    ///   same owner;
    /// - one scaled report pairs with exactly one carrier, and a repeated
    ///   underlying-unit report of an already-carried movement is a typed
    ///   duplicate, never a second carrier.
    ///
    /// Companion lookup scans the whole slice, not just earlier logs: Aave
    /// emits the companion `Transfer` before the `Mint`/`Burn` it reports.
    pub(super) fn classify(events: &[ScaledTokenEvent<'_>]) -> Self {
        let mut dispositions: HashMap<u64, EventDisposition> = HashMap::with_capacity(events.len());
        // Semantic events already named as a report's owner — companion
        // assignment is injective, so a second companion needs its own owner.
        let mut claimed_semantic: HashSet<u64> = HashSet::new();
        // Non-companion transfer reports, in log order, awaiting movement pairing.
        let mut reports: Vec<&ScaledTokenEvent<'_>> = Vec::new();
        for ev in events {
            match &ev.decoded {
                ScaledTokenEventData::Mint { .. } => {
                    dispositions.insert(ev.log_index, EventDisposition::SemanticMint);
                }
                ScaledTokenEventData::Burn { .. } => {
                    dispositions.insert(ev.log_index, EventDisposition::SemanticBurn);
                }
                ScaledTokenEventData::BalanceTransfer { .. }
                | ScaledTokenEventData::Transfer { .. } => {
                    if let Some((owner, side)) =
                        semantic_companion(ev, events, &mut claimed_semantic)
                    {
                        dispositions
                            .insert(ev.log_index, EventDisposition::Companion { owner, side });
                    } else {
                        reports.push(ev);
                    }
                }
            }
        }
        // Pair each movement's underlying-unit report with its scaled report.
        // A scaled report belongs to exactly one carrier (first in log order),
        // so no movement is owned twice.
        let mut paired: HashSet<u64> = HashSet::new();
        // First carrier that owns a scaled report, keyed by the movement's
        // underlying-unit identity: a repeated report of the same movement is a
        // duplicate of that carrier, never a second operation.
        let mut carried_movements: HashMap<MovementIdentity, u64> = HashMap::new();
        for report in &reports {
            if !is_underlying_report(report) {
                continue;
            }
            let partner = reports.iter().copied().find(|other| {
                is_scaled_report(other)
                    && !paired.contains(&other.log_index)
                    && other.token_address == report.token_address
                    && other.from_address == report.from_address
                    && other.target_address == report.target_address
                    && are_compatible_transfer_types(report.event_type, other.event_type)
            });
            let Some(partner) = partner else {
                continue;
            };
            paired.insert(partner.log_index);
            dispositions.insert(
                report.log_index,
                EventDisposition::StandaloneTransfer {
                    scaled_partner: Some(partner.log_index),
                },
            );
            dispositions.insert(
                partner.log_index,
                EventDisposition::BalanceTransfer {
                    underpinned_by: Some(report.log_index),
                },
            );
            carried_movements
                .entry(movement_identity(report))
                .or_insert(report.log_index);
        }
        for report in reports {
            if dispositions.contains_key(&report.log_index) {
                continue;
            }
            let disposition = if is_scaled_report(report) {
                EventDisposition::BalanceTransfer {
                    underpinned_by: None,
                }
            } else if let Some(owner) = carried_movements.get(&movement_identity(report)) {
                EventDisposition::DuplicateCarrier { owner: *owner }
            } else {
                EventDisposition::StandaloneTransfer {
                    scaled_partner: None,
                }
            };
            dispositions.insert(report.log_index, disposition);
        }
        Self { dispositions }
    }

    /// The log's disposition; `None` for a log outside the decoded set.
    pub(super) fn disposition(&self, log_index: u64) -> Option<EventDisposition> {
        self.dispositions.get(&log_index).copied()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.dispositions.len()
    }
}

/// One liquidation's own slice of a transaction: the logs emitted after the
/// previous `LiquidationCall` and before this one.
///
/// Aave emits a `liquidationCall`'s legs (the seized-collateral `Burn` or
/// interest-exceeds `Mint`, the movement and protocol-fee `BalanceTransfer`
/// reports, the debt `Burn`) before the `LiquidationCall` event that closes the
/// sub-call, and never interleaves two sub-calls. A transaction that liquidates
/// the same user twice against the same collateral asset therefore carries two
/// disjoint legs, and only the window says which is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LiquidationWindow {
    start: u64,
    end: u64,
}

impl LiquidationWindow {
    /// The window closed by `anchor`, opened after the previous
    /// `LiquidationCall` in the transaction (none for the first).
    pub(super) fn ending_at(all_events: &[&Log], anchor: u64) -> Self {
        let previous = all_events
            .iter()
            .filter(|log| {
                log.topics().first() == Some(&aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC)
            })
            .map(|log| log_idx_value(log))
            .filter(|index| *index < anchor)
            .max();
        Self {
            start: previous.map_or(0, |index| index + 1),
            end: anchor,
        }
    }

    /// `true` when the log was emitted inside this liquidation's sub-call.
    pub(super) fn owns(self, log_index: u64) -> bool {
        (self.start..self.end).contains(&log_index)
    }

    /// The half-open `(start, end)` bounds the window owns, for a refusal that
    /// must name the range it looked in.
    pub(super) fn bounds(self) -> (u64, u64) {
        (self.start, self.end)
    }
}

/// The index-less ERC20 `Transfer` report of a balance movement.
fn is_underlying_report(ev: &ScaledTokenEvent<'_>) -> bool {
    matches!(ev.decoded, ScaledTokenEventData::Transfer { .. })
}

/// The index-bearing `BalanceTransfer` report of a balance movement.
fn is_scaled_report(ev: &ScaledTokenEvent<'_>) -> bool {
    matches!(ev.decoded, ScaledTokenEventData::BalanceTransfer { .. })
}

/// A balance movement's identity in underlying units: two index-less reports
/// with the same identity report the same movement.
type MovementIdentity = (Address, Address, Address, U256);

/// The underlying-unit identity of a transfer report.
fn movement_identity(ev: &ScaledTokenEvent<'_>) -> MovementIdentity {
    (
        ev.token_address,
        ev.from_address.unwrap_or(Address::ZERO),
        ev.target_address.unwrap_or(Address::ZERO),
        ev.amount,
    )
}

/// The semantic `Mint`/`Burn` whose delta a zero-address report duplicates.
/// Each semantic event owns at most one companion — the claim set makes the
/// assignment injective — and match keys are the movement's token and user,
/// never its amount, so two same-valued movements stay independently owned.
fn semantic_companion(
    ev: &ScaledTokenEvent<'_>,
    events: &[ScaledTokenEvent<'_>],
    claimed: &mut HashSet<u64>,
) -> Option<(u64, CompanionSide)> {
    if ev.target_address == Some(Address::ZERO) {
        let from = ev.from_address.unwrap_or(Address::ZERO);
        let burn = events.iter().find(|other| {
            matches!(other.decoded, ScaledTokenEventData::Burn { .. })
                && !claimed.contains(&other.log_index)
                && other.user_address == from
                && other.token_address == ev.token_address
        })?;
        claimed.insert(burn.log_index);
        return Some((burn.log_index, CompanionSide::Burn));
    }
    if ev.from_address == Some(Address::ZERO) {
        let target = ev.target_address.unwrap_or(Address::ZERO);
        let mint = events.iter().find(|other| {
            matches!(other.decoded, ScaledTokenEventData::Mint { .. })
                && !claimed.contains(&other.log_index)
                && other.user_address == target
                && other.token_address == ev.token_address
        })?;
        claimed.insert(mint.log_index);
        return Some((mint.log_index, CompanionSide::Mint));
    }
    None
}
