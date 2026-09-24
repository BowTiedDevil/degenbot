use super::{
    aave_event_decoder, address_to_checksum_string, Address, HashSet, Log, ScaledTokenEvent,
    ScaledTokenEventType, U256,
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
/// duplicate (the RCDJPH §4.2 byte-divergence — duplicate lowercase users).
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
/// A Transfer to ZERO that is part of some burn.
pub(super) fn is_part_of_burn(
    ev: &ScaledTokenEvent<'_>,
    scaled_events: &[ScaledTokenEvent<'_>],
    local_assigned: &mut HashSet<u64>,
) -> bool {
    let ev_token = ev.token_address;
    for other in scaled_events {
        if other.event_type != ScaledTokenEventType::CollateralBurn
            && other.event_type != ScaledTokenEventType::DebtBurn
            && other.event_type != ScaledTokenEventType::GhoDebtBurn
        {
            continue;
        }
        if other.user_address != ev.from_address.unwrap_or(Address::ZERO) {
            continue;
        }
        if other.token_address != ev_token {
            continue;
        }
        local_assigned.insert(ev.log_index);
        return true;
    }
    false
}

/// A Transfer from ZERO that is part of some mint.
pub(super) fn is_part_of_mint(
    ev: &ScaledTokenEvent<'_>,
    scaled_events: &[ScaledTokenEvent<'_>],
    local_assigned: &mut HashSet<u64>,
) -> bool {
    let ev_token = ev.token_address;
    for other in scaled_events {
        if other.event_type != ScaledTokenEventType::CollateralMint
            && other.event_type != ScaledTokenEventType::DebtMint
            && other.event_type != ScaledTokenEventType::GhoDebtMint
        {
            continue;
        }
        if other.user_address != ev.target_address.unwrap_or(Address::ZERO) {
            continue;
        }
        if other.token_address != ev_token {
            continue;
        }
        local_assigned.insert(ev.log_index);
        return true;
    }
    false
}
