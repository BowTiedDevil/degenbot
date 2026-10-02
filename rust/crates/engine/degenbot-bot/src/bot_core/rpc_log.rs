//! Reconstruct an [`alloy::rpc::types::Log`] from the WS-log shape
//! `(address, topics, data, block_number)` the Python seam passes.
//!
//! Moved from the PyO3 shell (`degenbot-python/bot/mod.rs`) so the
//! offline pump→dispatch→solve loop is drivable from pure Rust too
//! (ADR-006 D4): the standalone driver rebuilds the same
//! [`alloy::rpc::types::Log`] the `BlockPump` feeds
//! [`Bot::dispatch_log`](crate::bot_core::bot::Bot::dispatch_log), reusing
//! the pure-logic dispatcher untouched. Hex strings accept an optional
//! `0x` prefix.

/// Build an [`alloy::rpc::types::Log`] from the WS-log shape
/// `(address, topics, data, block_number)`. The `Err` carries the exact
/// refusal message the Python facade maps onto `ValueError`.
pub fn build_rpc_log(
    address: &str,
    topics: Vec<String>,
    data: &str,
    block_number: u64,
) -> Result<alloy::rpc::types::Log, String> {
    let addr: alloy::primitives::Address = address
        .parse()
        .map_err(|e| format!("Invalid address '{address}': {e}"))?;
    let mut topic_hashes = Vec::with_capacity(topics.len());
    for t in topics {
        let stripped = t.strip_prefix("0x").unwrap_or(&t);
        let b: alloy::primitives::B256 = stripped
            .parse()
            .map_err(|e| format!("Invalid topic '{t}': {e}"))?;
        topic_hashes.push(b);
    }
    let data_stripped = data.strip_prefix("0x").unwrap_or(data);
    let data_bytes =
        alloy::hex::decode(data_stripped).map_err(|e| format!("Invalid data hex '{data}': {e}"))?;
    let inner = alloy::primitives::Log::new_unchecked(
        addr,
        topic_hashes,
        alloy::primitives::Bytes::from(data_bytes),
    );
    Ok(alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    })
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::build_rpc_log;

    #[test]
    fn parses_with_and_without_0x_prefixes() {
        let log = build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec![
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef".to_string(),
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string(),
            ],
            "0xdeadbeef",
            7,
        )
        .expect("valid ws log shape");
        assert_eq!(log.block_number, Some(7));
        assert_eq!(
            log.inner.data.data,
            alloy::primitives::Bytes::from(vec![0xde, 0xad, 0xbe, 0xef])
        );
        assert!(!log.removed);
    }

    #[test]
    fn refusals_carry_the_mapped_messages() {
        let err = build_rpc_log("nothex", vec![], "0x", 1).unwrap_err();
        assert!(err.contains("Invalid address 'nothex'"), "{err}");
        let err = build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec!["zz".to_string()],
            "0x",
            1,
        )
        .unwrap_err();
        assert!(err.contains("Invalid topic 'zz'"), "{err}");
        let err = build_rpc_log(
            "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
            vec![],
            "zz",
            1,
        )
        .unwrap_err();
        assert!(err.contains("Invalid data hex 'zz'"), "{err}");
    }
}
