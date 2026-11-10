//! The fan-out trace row — one TRACE-level forensic line per simulated
//! candidate: the read set + cost facts a recorded-fan-out replay consumes.
//!
//! Transport is the log (`diag_trace!` on the `sim` domain, enabled by the
//! `degenbot=trace` sink); `grep 'fanout-trace '` of a run's log yields one
//! JSON object per line. Rows are emitted at the simulation seam for EVERY
//! candidate — including reverts and refusals — so a replay sees the real
//! read-walk length distribution, not only the survivors' (the capture that
//! reaches `SimResult` covers profitable rows only).
//!
//! # The row, field by field
//!
//! | Field | Meaning |
//! |---|---|
//! | `block` | the anchor the candidate was simulated at |
//! | `cand` | the candidate identity |
//! | `sim_ns` | the wall the seam measured for the candidate |
//! | `verdict` / `gas` | how it ended, and the gas it burned |
//! | `reads` | the POST-execution access list: `address|slot,slot` per entry |
//! | `relevance` | the PRE-simulation relevance set: bare `0x` addresses |
//! | `fanout` | the fan-out that simulated this candidate; ABSENT when a caller outside any fan-out emitted the row |
//!
//! `reads` is what the frame actually touched; `relevance` is what the
//! candidate's PATH and the deployment's settlement declaration made relevant
//! before it ran ([`degenbot_simulation::sim::path_relevance_addresses`]). The
//! two answer different questions — "what did this read?" versus "what must a
//! block-access-list snapshot carry for this candidate to execute at all?" — and
//! a snapshot replay needs the second. Deriving it from the first would replay an
//! oracle the live run never had: a captured read is only known after the read
//! happened, so a relevance set built from `reads` cannot filter the snapshot it
//! was supposed to filter.
//!
//! # Migration notes (the additive `relevance` field)
//!
//! Rows written before this field existed carry `block`, `cand`, `sim_ns`,
//! `verdict`, `gas` and `reads` only. Adding a key is backward compatible for a
//! reader that treats it as optional, and `recorded_corpus`'s parser does exactly
//! that: a legacy row parses unchanged with no relevance set, and a consumer that
//! needs one falls back to the row's recorded read ADDRESSES, labelled as the
//! ORACLE fallback it is
//! (`recorded_corpus::RelevanceSource::RecordedReadOracle`) — never reported as
//! production relevance. No existing field changed meaning, so an old reader of an
//! old corpus and an old reader of a new corpus both keep working; a new consumer
//! of an old corpus gets the labelled fallback.
//!
//! A later capture should read `relevance` for the candidate's address set and
//! treat the fallback as a measurement artifact: the production set is knowable
//! before simulation, so a snapshot scope built from it is what a live run would
//! actually have carried. Re-capturing a corpus with this field is therefore a
//! pure upgrade — the `reads` stream is unchanged, so every existing read-driven
//! benchmark keeps its numbers.
//!
//! # Migration notes (the additive `fanout` field)
//!
//! That field joins one candidate's row to the fan-out that simulated it:
//! `degenbot-arbitrage` mints one identifier per dispatch and echoes the same
//! value on the fan-out's storage-selection row, its prefetch row, its `[sim]`
//! delta summary, and every candidate's trace row
//! (`degenbot_simulation::sim::sim_metrics::next_fanout_id`).
//!
//! It is OPTIONAL in two directions:
//!
//! - a row emitted by a caller outside any fan-out — the inline hook re-quote
//!   arm (`degenbot-python`'s `simulation/inline_hook.rs`) — carries no `fanout`
//!   key at all, so it can never be attributed to a fan-out that did not
//!   simulate it, and its row shape is byte-identical to a pre-`fanout` row;
//! - rows written before this field existed also carry no key. A reader that
//!   treats it as optional parses both, and a consumer that needs attribution
//!   (the OFF/ON live report) reports those rows as UNATTRIBUTED rather than
//!   guessing from a block number.
//!
//! No existing field changed meaning: the key is appended last, so an old reader
//! of a new row keeps working and a new reader of an old row gets "unattributed"
//! instead of an error.

use alloy::primitives::Address;
use alloy::rpc::types::eth::AccessList;

use degenbot_core::diag_trace;

/// Build one trace row. The `reads` array holds one string per access-list
/// entry: `address|slot,slot` (an empty slot list is a trailing `|`), so the
/// row stays single-line JSON without nested quoting. Slots ride as minimal
/// word hex; addresses as full 0x-hex.
///
/// `relevance` is the candidate's PRE-simulation relevance set — the seam hands
/// over [`degenbot_simulation::sim::path_relevance_addresses`]'s output, never a
/// derivation from `access_list` (see the module's migration notes). It renders
/// as a bare `0x`-hex array in the order the derivation produced.
///
/// `fanout` is the fan-out that simulated the candidate, or `None` for a caller
/// outside any fan-out (the inline hook re-quote arm). `None` OMITS the key
/// rather than rendering a placeholder, so an unattributed row is
/// indistinguishable from a pre-`fanout` row and no reader has to know a sentinel
/// to avoid mis-attributing it.
#[must_use]
// A row is a flat record of the seam's own observations; grouping the fields into
// a struct would move the same values to a constructor at the same call sites.
#[expect(clippy::too_many_arguments)]
pub fn fanout_trace_row(
    block: u64,
    cand: &str,
    sim_ns: u64,
    verdict: &str,
    gas: u64,
    access_list: &AccessList,
    relevance: &[Address],
    fanout: Option<u64>,
) -> String {
    use std::fmt::Write;

    let mut reads = String::new();
    for (index, item) in access_list.0.iter().enumerate() {
        if index > 0 {
            reads.push(',');
        }
        let _ = write!(reads, "\"{:#x}|", item.address);
        for (slot_index, key) in item.storage_keys.iter().enumerate() {
            if slot_index > 0 {
                reads.push(',');
            }
            let _ = write!(
                reads,
                "{:#x}",
                alloy::primitives::U256::from_be_bytes(key.0)
            );
        }
        reads.push('"');
    }
    let mut relevant = String::new();
    for (index, address) in relevance.iter().enumerate() {
        if index > 0 {
            relevant.push(',');
        }
        let _ = write!(relevant, "\"{address:#x}\"");
    }
    let mut row = String::with_capacity(128 + reads.len() + relevant.len());
    let _ = write!(
        row,
        "fanout-trace {{\"block\":{block},\"cand\":\"{cand}\",\"sim_ns\":{sim_ns},\"verdict\":\"{verdict}\",\"gas\":{gas},\"reads\":[{reads}],\"relevance\":[{relevant}]"
    );
    if let Some(fanout) = fanout {
        let _ = write!(row, ",\"fanout\":{fanout}");
    }
    row.push('}');
    row
}

/// Emit one trace row at TRACE level for a finished candidate simulation.
///
/// `relevance` is the set the seam derived from the candidate's path and the
/// deployment's settlement facts BEFORE the simulation ran. `fanout` is the
/// fan-out that simulated the candidate, or `None` for a caller outside any
/// fan-out.
#[expect(clippy::too_many_arguments)]
pub fn record(
    block: u64,
    cand: &str,
    sim_ns: u64,
    verdict: &str,
    gas: u64,
    access_list: &AccessList,
    relevance: &[Address],
    fanout: Option<u64>,
) {
    let row = fanout_trace_row(
        block,
        cand,
        sim_ns,
        verdict,
        gas,
        access_list,
        relevance,
        fanout,
    );
    diag_trace!(domain = sim, "{}", row);
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use alloy::primitives::{Address, B256};
    use alloy::rpc::types::eth::{AccessList, AccessListItem};
    use degenbot_execution::{HopInfo, PathInfo, V2HopInfo, V4HopInfo};
    use degenbot_executor::compute_simulation_warmup_slots;
    use degenbot_simulation::sim::{PrefetchFacts, SettlementWarmup};

    use super::fanout_trace_row;

    const EXECUTOR: Address = Address::new([0xaa; 20]);
    const WETH: Address = Address::new([0xbb; 20]);
    const POOL_MANAGER: Address = Address::new([0xcc; 20]);
    const TOKEN: Address = Address::new([0xdd; 20]);
    const POOL: Address = Address::new([0x01; 20]);
    const HOOK: Address = Address::new([0x4c; 20]);
    /// An address only a POST-read walk can know: no path hop names it.
    const POST_READ_ONLY: Address = Address::new([0x99; 20]);

    fn key(byte: u8) -> B256 {
        let mut word = [0u8; 32];
        word[31] = byte;
        B256::from(word)
    }

    fn v2_hop() -> HopInfo {
        HopInfo::V2(V2HopInfo {
            pool_address: POOL,
            token0_address: WETH,
            token1_address: TOKEN,
            fee: 30,
            zfo: true,
        })
    }

    fn v4_hop() -> HopInfo {
        HopInfo::V4(V4HopInfo {
            pool_manager_address: POOL_MANAGER,
            pool_id_hex: format!("{}", B256::repeat_byte(0x42)),
            currency0_address: WETH,
            currency1_address: TOKEN,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

    #[test]
    fn the_row_is_one_greppable_json_line_shaped_like_the_replay_parser_expects() {
        let access_list = AccessList(vec![
            AccessListItem {
                address: Address::repeat_byte(0x11),
                storage_keys: vec![key(1), key(2)],
            },
            AccessListItem {
                address: Address::repeat_byte(0x22),
                storage_keys: Vec::new(),
            },
        ]);
        let relevance = [Address::repeat_byte(0x11), Address::repeat_byte(0x22)];
        let row = fanout_trace_row(7, "42-1", 9, "result", 3, &access_list, &relevance, None);
        let expected = concat!(
            "fanout-trace {\"block\":7,\"cand\":\"42-1\",\"sim_ns\":9,\"verdict\":\"result\",",
            "\"gas\":3,\"reads\":[\"0x1111111111111111111111111111111111111111|0x1,0x2\",",
            "\"0x2222222222222222222222222222222222222222|\"],",
            "\"relevance\":[\"0x1111111111111111111111111111111111111111\",",
            "\"0x2222222222222222222222222222222222222222\"]}"
        );
        assert_eq!(row, expected);
    }

    /// One fan-out's identifier rides the row as a last-key `fanout` field, and a
    /// caller OUTSIDE any fan-out (the inline hook re-quote arm) omits the key
    /// entirely: its row is byte-identical to a pre-`fanout` row, so no reader
    /// can mistake it for a dispatch fan-out's candidate.
    #[test]
    fn the_fanout_key_joins_a_row_to_its_dispatch_and_an_unattributed_caller_omits_it() {
        let access_list = AccessList(Vec::new());
        let relevance = [Address::repeat_byte(0x44)];
        let attributed =
            fanout_trace_row(9, "8-1", 3, "none", 0, &access_list, &relevance, Some(7));
        let unattributed = fanout_trace_row(9, "8-1", 3, "none", 0, &access_list, &relevance, None);

        assert!(
            attributed.ends_with(",\"fanout\":7}"),
            "the fan-out id is the row's last key: {attributed}"
        );
        assert!(
            !unattributed.contains("\"fanout\":"),
            "an unattributed row carries no key at all: {unattributed}"
        );
        let value: serde_json::Value = serde_json::from_str(
            attributed
                .strip_prefix("fanout-trace ")
                .expect("the row is tagged"),
        )
        .expect("the row is JSON");
        assert_eq!(value["fanout"], 7);
        let legacy: serde_json::Value = serde_json::from_str(
            unattributed
                .strip_prefix("fanout-trace ")
                .expect("the row is tagged"),
        )
        .expect("the row is JSON");
        assert!(
            legacy.get("fanout").is_none(),
            "an absent key, never a placeholder: {legacy}"
        );
    }

    #[test]
    fn an_empty_read_set_still_emits_a_parsable_row() {
        let row = fanout_trace_row(
            1,
            "7-2",
            0,
            "none",
            0,
            &AccessList::default(),
            &[Address::repeat_byte(0x33)],
            None,
        );
        assert_eq!(
            row,
            concat!(
                "fanout-trace {\"block\":1,\"cand\":\"7-2\",\"sim_ns\":0,\"verdict\":\"none\",",
                "\"gas\":0,\"reads\":[],",
                "\"relevance\":[\"0x3333333333333333333333333333333333333333\"]}"
            )
        );
    }

    /// The relevance field is the pre-simulation derivation's own output. An
    /// address that only the POST-execution access list knows — a read the path
    /// never named — stays in `reads` and never leaks into `relevance`, because a
    /// replay that treated the post-read walk as production relevance would be
    /// replaying an oracle the live run did not have.
    #[test]
    fn the_relevance_field_is_the_path_derivation_not_the_post_read_access_list() {
        let facts = PrefetchFacts {
            settlement: Some(SettlementWarmup {
                weth_address: WETH,
                pool_manager_address: Some(POOL_MANAGER),
                slots: compute_simulation_warmup_slots(EXECUTOR, WETH),
            }),
        };
        let mut v4 = v4_hop();
        if let HopInfo::V4(ref mut hop) = v4 {
            hop.hook_address = HOOK;
        }
        let info = PathInfo::new(vec![v2_hop(), v4]);
        let relevance: Vec<Address> =
            degenbot_simulation::sim::path_relevance_addresses([&info], &facts);

        // The post-read walk: every relevant address PLUS an address the path
        // never names (an arbitrary ERC20 `balanceOf` base, say).
        let mut access_list = AccessList(Vec::new());
        for address in &relevance {
            access_list.0.push(AccessListItem {
                address: *address,
                storage_keys: vec![key(9)],
            });
        }
        access_list.0.push(AccessListItem {
            address: POST_READ_ONLY,
            storage_keys: vec![key(4)],
        });

        let row = fanout_trace_row(11, "7-3", 5, "result", 8, &access_list, &relevance, None);
        let value: serde_json::Value = serde_json::from_str(
            row.strip_prefix("fanout-trace ")
                .expect("the row is tagged"),
        )
        .expect("the row is JSON");
        let emitted: Vec<Address> = value["relevance"]
            .as_array()
            .expect("the row carries a relevance array")
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .expect("addresses are strings")
                    .parse()
                    .expect("hex address")
            })
            .collect();
        assert_eq!(
            emitted, relevance,
            "the field is exactly the derivation, in its own order"
        );
        assert!(
            emitted.contains(&HOOK),
            "a hooked path makes the hook relevant: {emitted:?}"
        );
        assert!(
            !emitted.contains(&POST_READ_ONLY),
            "a post-read-only address must never be described as production relevance"
        );
        let reads = value["reads"].as_array().expect("reads stay recorded");
        assert_eq!(
            reads.len(),
            relevance.len() + 1,
            "the post-read walk still records the extra address in `reads`"
        );
    }
}
