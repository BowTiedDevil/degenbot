//! ADR-107 D10 all-targets-declined leg — the matched pre-cutover/post-cutover
//! first-compatible walk over the bound built-in adapter.
//!
//! Same corpus and measurement method in both binaries: bind the production
//! release, drive `TargetSelection::FirstCompatible` over one two-hop V4 route
//! whose every hop names a foreign `PoolManager`, and scrape the HTTP metrics
//! endpoint. The path-level outcome struct is deliberately not referenced — it
//! did not exist pre-cutover — so the walk is asserted through the candidate's
//! own lane record and the scrape.
#![cfg(feature = "otel-replay")]
#![expect(clippy::print_stdout, clippy::expect_used, clippy::unwrap_used)]

use super::*;

fn scrape(addr: std::net::SocketAddr) -> String {
    use std::io::{Read as _, Write as _};
    degenbot_bot::metrics::flush_global_metrics();
    let mut stream = std::net::TcpStream::connect(addr).expect("scrape endpoint connects");
    stream
        .write_all(b"GET /metrics HTTP/1.0\r\nHost: replay\r\n\r\n")
        .expect("scrape request writes");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("scrape reads");
    match text.split_once("\r\n\r\n") {
        Some((_, body)) => body.to_owned(),
        None => text,
    }
}

#[test]
fn all_targets_declined_replay() {
    let init = degenbot_bot::metrics::init_global_metrics_with_addr(
        "127.0.0.1:0".parse().expect("ephemeral endpoint"),
    )
    .expect("the replay owns the process metrics pipeline");
    let addr = match &init {
        degenbot_bot::metrics::MetricsInit::Started { endpoint }
        | degenbot_bot::metrics::MetricsInit::AlreadyRunning { endpoint } => endpoint.addr(),
    };

    let registry = registry_production();
    let bound = BoundCmdExecutor::bind(
        binding(EXECUTOR_V1),
        &declared_answers(&committed_manifest()),
        None,
        &registry,
    )
    .expect("the built-in deployment binds to the release this build ships");
    let deployments = DeploymentRegistry::new(vec![Arc::new(
        bound
            .injected(deployment_id_v1())
            .expect("the release's own artifact"),
    )])
    .expect("a one-entry registry");

    let foreign_manager = Address::repeat_byte(0x99);
    let mut path = v4v4_weth_terminal();
    for hop in &mut path.hops {
        let HopInfo::V4(v4) = hop else {
            panic!("the fixture route is pure V4");
        };
        v4.pool_manager_address = foreign_manager;
    }

    let outputs = [OPTIMAL_INPUT, OPTIMAL_INPUT];
    let consumed = [OPTIMAL_INPUT - 1, OPTIMAL_INPUT - 1];
    let answer = deployments
        .get(deployment_id_v1())
        .expect("registered")
        .adapter()
        .encode(&path, &combo_inputs(&outputs, &consumed));
    let mut out: Vec<String> = vec![format!(
        "built_in_encode_declined={} error={:?}",
        matches!(&answer, Err(ComposeError::Unsupported(_))),
        answer.as_ref().err()
    )];

    let candidate_row = candidate(path.clone(), deployment_id_v1(), &outputs);
    out.push(format!(
        "candidate_registered={:?} steps={} path_hops={} foreign_pool_manager={foreign_manager}",
        candidate_row.candidate_id,
        candidate_row.steps.len(),
        candidate_row.path_info.hops.len()
    ));

    let outcome = run_fanout(
        &deployments,
        TargetSelection::FirstCompatible,
        vec![candidate(path.clone(), deployment_id_v1(), &outputs)],
    );
    let lane = outcome.lane(deployment_id_v1()).expect("the built-in lane");
    out.push(format!(
        "walk retired={} gas_profitable={} gas_unprofitable={} failures={} encode_failed={:?}",
        outcome.retired.len(),
        lane.gas_profitable.len(),
        lane.gas_unprofitable.len(),
        lane.failures.len(),
        lane.fail_buckets.buckets().get("encode-failed").copied()
    ));

    let text = scrape(addr);
    let mut series = 0;
    for line in text
        .lines()
        .filter(|line| line.starts_with("degenbot_selection_all_targets_declined_total"))
    {
        series += 1;
        out.push(format!("metric {line}"));
    }
    out.push(format!("all_targets_declined_series_lines={series}"));

    let report = out.join("\n");
    if let Ok(path) = std::env::var("DEGENBOT_ALL_TARGETS_REPLAY_OUT") {
        std::fs::write(&path, format!("{report}\n--- scrape excerpt ---\n")).expect("writes");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("opens");
        use std::io::Write as _;
        for line in text
            .lines()
            .filter(|line| line.starts_with("degenbot_selection_all_targets_declined_total"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
    let expected_lines: usize = std::env::var("DEGENBOT_ALL_TARGETS_EXPECT_SERIES_LINES")
        .unwrap_or_else(|_| "1".to_owned())
        .parse()
        .expect("integer series-line expectation");
    assert_eq!(
        series, expected_lines,
        "all-targets-declined series lines in the scrape (0 = absent)"
    );
    assert!(matches!(&answer, Err(ComposeError::Unsupported(_))));
    assert_eq!(
        lane.fail_buckets.buckets().get("encode-failed").copied(),
        Some(1),
        "the candidate keeps its own routine refusal"
    );
    assert!(outcome.retired.is_empty(), "a decline retires no target");
    println!("{report}");
}
