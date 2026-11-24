//! ADR-107 D10 build/verify leg — the matched pre-cutover/post-cutover crawl
//! admission replay.
//!
//! Same candidates, same offline fixture, two binaries. Per candidate it prints
//! the receipt's answering stage, its stage-split length, the `VerifyLifecycle`
//! claim / completion counts, and the registered-path count, so the old
//! (build + verify + register) and new (refuse before verify, then answer from
//! the pool fact) behaviours are numbers rather than prose.
//!
//! Every fixture is defined here from helpers present at both commits, so the
//! same source runs on both binaries.
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use super::*;

fn camelot_rpc() -> FakeRpc {
    let mut rpc = FakeRpc::new();
    let dex = dex_identity::CAMELOT_V2_STABLE;
    let pool = v2_address(&dex, USDC, WETH);
    rpc.set(pool, b"factory()", addr_word(dex.factory));
    rpc.set(pool, b"token0()", addr_word(USDC));
    rpc.set(pool, b"token1()", addr_word(WETH));
    rpc.set(pool, b"stableSwap()", enc(DynSolValue::Bool(true)));
    rpc.set(pool, b"FEE_DENOMINATOR()", uint_word(100_000, 256));
    rpc.set(pool, b"token0FeePercent()", uint_word(300, 256));
    rpc.set(pool, b"token1FeePercent()", uint_word(300, 256));
    rpc.set(
        pool,
        b"getReserves()",
        reserves_word(1_500_000_000_000, 800_000_000_000_000_000),
    );
    rpc.set(
        v2_address(&dex_identity::UNISWAP_V2, USDC, WETH),
        b"factory()",
        addr_word(dex_identity::UNISWAP_V2.factory),
    );
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    rpc
}

fn camelot_hop() -> DiscoveredHop {
    DiscoveredHop::V2 {
        address: v2_address(&dex_identity::CAMELOT_V2_STABLE, USDC, WETH),
    }
}

fn line(out: &mut Vec<String>, harness: &Harness, case: &str, seq: u64, receipt: &CrawlReceipt) {
    let claims = harness.verify().live_claim_count();
    let completed = harness.verify().verified_pool_count();
    out.push(format!(
        "case={case} seq={seq} stages={} outcome={:?} verify_claims={claims} verify_completed={completed} registered_paths={}",
        receipt.split.len(),
        receipt.outcome,
        harness.stages.path_count(),
    ));
}

fn default_expectation(key: &str) -> &'static str {
    match key {
        "DEGENBOT_CRAWL_EXPECT_FIRST_VERIFY"
        | "DEGENBOT_CRAWL_EXPECT_RESIGHT_VERIFY"
        | "DEGENBOT_CRAWL_EXPECT_FIRST_REGISTERED"
        | "DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED_ANY"
        | "DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED" => "0",
        "DEGENBOT_CRAWL_EXPECT_RESIGHT_STAGES"
        | "DEGENBOT_CRAWL_EXPECT_BUILD_ENTRIES"
        | "DEGENBOT_CRAWL_EXPECT_CONTROL_VERIFY"
        | "DEGENBOT_CRAWL_EXPECT_CONTROL_REGISTERED" => "1",
        "DEGENBOT_CRAWL_EXPECT_FIRST_STAGES" => "3",
        "DEGENBOT_CRAWL_EXPECT_CONTROL_STAGES" => "5",
        other => panic!("{other} has no default expectation"),
    }
}

fn env_usize(key: &str) -> usize {
    let raw = std::env::var(key).unwrap_or_else(|_| default_expectation(key).to_owned());
    raw.parse()
        .unwrap_or_else(|_| panic!("{key} must parse as an integer"))
}

fn build_entries(stages: [usize; 2]) -> usize {
    stages.iter().filter(|&&len| len >= 2).count()
}

fn report(tag: &str, out: &[String]) {
    let text = out.join("\n");
    if let Ok(path) = std::env::var("DEGENBOT_ADMISSION_REPLAY_OUT") {
        let path = format!("{path}.{tag}");
        std::fs::write(&path, format!("{text}\n")).expect("crawl replay report writes");
    }
    println!("{text}");
}

#[tokio::test]
async fn crawl_admission_replay_solve_only_family() {
    let harness = Harness::new(camelot_rpc());
    let mut out: Vec<String> = Vec::new();

    let first = harness
        .run(&CrawlUnit {
            seq: 0,
            hops: vec![camelot_hop(), v3_hop()],
            directions: None,
        })
        .await
        .unwrap();
    line(
        &mut out,
        &harness,
        "solidly_family_first_sighting",
        0,
        &first,
    );
    assert_eq!(
        first.split.len(),
        env_usize("DEGENBOT_CRAWL_EXPECT_FIRST_STAGES"),
        "first sighting stage count"
    );
    assert_eq!(
        harness.verify().verified_pool_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_FIRST_VERIFY"),
        "first sighting completed verifications"
    );
    assert_eq!(
        harness.stages.path_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_FIRST_REGISTERED"),
        "first sighting registered paths"
    );

    let resight = harness
        .run(&CrawlUnit {
            seq: 1,
            hops: vec![v3_hop(), camelot_hop()],
            directions: None,
        })
        .await
        .unwrap();
    line(&mut out, &harness, "solidly_family_resight", 1, &resight);
    assert_eq!(
        resight.split.len(),
        env_usize("DEGENBOT_CRAWL_EXPECT_RESIGHT_STAGES"),
        "re-sighting stage count (1 = answered by the pool fact)"
    );
    assert_eq!(
        usize::from(format!("{:?}", resight.outcome).starts_with("Registered")),
        env_usize("DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED_ANY"),
        "re-sighting registration"
    );
    assert_eq!(
        harness.verify().verified_pool_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_RESIGHT_VERIFY"),
        "re-sighting completed verifications"
    );
    assert_eq!(
        harness.stages.path_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_RESIGHT_REGISTERED"),
        "re-sighting registered paths"
    );
    assert_eq!(
        build_entries([first.split.len(), resight.split.len()]),
        env_usize("DEGENBOT_CRAWL_EXPECT_BUILD_ENTRIES"),
        "build-stage entries over the two solve-only candidates"
    );

    report("family", &out);
}

/// The supported control: the same admission over a route every hop of which has
/// a command producer, so a refused-everything harness cannot pass unnoticed.
#[tokio::test]
async fn crawl_admission_replay_supported_control() {
    let harness = Harness::new(cycle_rpc());
    let mut out: Vec<String> = Vec::new();
    let supported = harness.run(&cycle_unit(0)).await.unwrap();
    line(&mut out, &harness, "supported_cycle_control", 0, &supported);
    assert_eq!(
        supported.split.len(),
        env_usize("DEGENBOT_CRAWL_EXPECT_CONTROL_STAGES"),
        "supported control stage count"
    );
    assert_eq!(
        harness.verify().verified_pool_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_CONTROL_VERIFY"),
        "supported control completed verifications"
    );
    assert_eq!(
        harness.stages.path_count(),
        env_usize("DEGENBOT_CRAWL_EXPECT_CONTROL_REGISTERED"),
        "supported control registered paths"
    );
    report("control", &out);
}
