
//! Parity: the feed crate's default searcher WS is the SAME constant the
//! strategy-readiness resolution hands an activated backrun facet.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

#[test]
fn default_stream_url_is_the_config_default() {
    assert_eq!(
        degenbot_rpc::backrun_feed::DEFAULT_STREAM_URL,
        degenbot_config::DEFAULT_BACKRUN_STREAM_URL,
        "the two default-WS homes must stay one constant"
    );
}
