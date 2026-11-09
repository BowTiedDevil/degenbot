//! The fan-out trace row — one TRACE-level forensic line per simulated
//! candidate: the read set + cost facts a recorded-fan-out replay consumes.
//!
//! Transport is the log (`diag_trace!` on the `sim` domain, enabled by the
//! `degenbot=trace` sink); `grep 'fanout-trace '` of a run's log yields one
//! JSON object per line. Rows are emitted at the simulation seam for EVERY
//! candidate — including reverts and refusals — so a replay sees the real
//! read-walk length distribution, not only the survivors' (the capture that
//! reaches `SimResult` covers profitable rows only).

use alloy::rpc::types::eth::AccessList;

use degenbot_core::diag_trace;

/// Build one trace row. The `reads` array holds one string per access-list
/// entry: `address|slot,slot` (an empty slot list is a trailing `|`), so the
/// row stays single-line JSON without nested quoting. Slots ride as minimal
/// word hex; addresses as full 0x-hex.
#[must_use]
pub fn fanout_trace_row(
    block: u64,
    cand: &str,
    sim_ns: u64,
    verdict: &str,
    gas: u64,
    access_list: &AccessList,
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
    let mut row = String::with_capacity(96 + reads.len());
    let _ = write!(
        row,
        "fanout-trace {{\"block\":{block},\"cand\":\"{cand}\",\"sim_ns\":{sim_ns},\"verdict\":\"{verdict}\",\"gas\":{gas},\"reads\":[{reads}]}}"
    );
    row
}

/// Emit one trace row at TRACE level for a finished candidate simulation.
pub fn record(
    block: u64,
    cand: &str,
    sim_ns: u64,
    verdict: &str,
    gas: u64,
    access_list: &AccessList,
) {
    let row = fanout_trace_row(block, cand, sim_ns, verdict, gas, access_list);
    diag_trace!(domain = sim, "{}", row);
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Address, B256};
    use alloy::rpc::types::eth::{AccessList, AccessListItem};

    use super::fanout_trace_row;

    fn key(byte: u8) -> B256 {
        let mut word = [0u8; 32];
        word[31] = byte;
        B256::from(word)
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
        let row = fanout_trace_row(7, "42-1", 9, "result", 3, &access_list);
        let expected = concat!(
            "fanout-trace {\"block\":7,\"cand\":\"42-1\",\"sim_ns\":9,\"verdict\":\"result\",",
            "\"gas\":3,\"reads\":[\"0x1111111111111111111111111111111111111111|0x1,0x2\",",
            "\"0x2222222222222222222222222222222222222222|\"]}"
        );
        assert_eq!(row, expected);
    }

    #[test]
    fn an_empty_read_set_still_emits_a_parsable_row() {
        let row = fanout_trace_row(1, "7-2", 0, "none", 0, &AccessList::default());
        assert_eq!(
            row,
            "fanout-trace {\"block\":1,\"cand\":\"7-2\",\"sim_ns\":0,\"verdict\":\"none\",\"gas\":0,\"reads\":[]}"
        );
    }
}
