//! The process-global metrics lifecycle, in its own test binary.
//!
//! The phase is process-wide, so this binary walks it end to end (the same
//! reason `otel_init.rs` is its own file). The transition table itself is
//! unit-tested against owned instances inside `metrics.rs`; what this file
//! proves is the real global wiring: the one scrape endpoint, the second-init
//! report, and the terminal stop.

#![cfg(feature = "otel")]
#![expect(clippy::expect_used, clippy::panic)]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use degenbot_bot::metrics::{self, Endpoint, InitDecision, MetricsInit, MetricsInitError, Phase};

fn ephemeral() -> SocketAddr {
    "127.0.0.1:0".parse().expect("literal socket address")
}

/// One scrape of a running endpoint. The connection alone proves the serve
/// thread is up: `listen` ran inside the init transition, so the kernel queues
/// this connect and the thread's own response is the ordering edge.
fn scrape(addr: SocketAddr) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect to the scrape endpoint");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the response");
    response
}

#[test]
fn the_global_pipeline_serves_once_then_stops_for_good() {
    assert_eq!(
        metrics::phase(),
        Phase::NotStarted,
        "a fresh process has no pipeline"
    );
    assert!(metrics::try_global_meter().is_none());
    assert!(!metrics::is_running());

    let started = metrics::init_global_metrics_with_addr(ephemeral()).expect("first init");
    let MetricsInit::Started {
        endpoint: Endpoint::Serving(bound),
    } = started
    else {
        panic!("the first init serves, got {started:?}");
    };
    assert_ne!(bound.port(), 0, "a `:0` bind reports the resolved port");
    assert!(metrics::is_running());
    assert!(metrics::try_global_meter().is_some());

    // The serve thread answers on the bound socket with the live registry.
    let response = scrape(bound);
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
    assert!(
        response.contains("degenbot_metric_series"),
        "the registry self-metric must render: {response}"
    );

    // A second init reports the address already bound — it does not bind a
    // second endpoint, and it does not lose the resolved port.
    let second = metrics::init_global_metrics_with_addr(ephemeral()).expect("idempotent init");
    assert_eq!(
        second,
        MetricsInit::AlreadyRunning {
            endpoint: Endpoint::Serving(bound)
        },
        "the second init reports the bound address"
    );

    metrics::flush_global_metrics();

    metrics::shutdown_global_metrics();
    assert_eq!(
        metrics::phase(),
        Phase::Stopped {
            reason: metrics::StopReason::Shutdown
        }
    );
    assert!(
        metrics::try_global_meter().is_none(),
        "a stopped pipeline must not hand out a meter"
    );
    assert!(!metrics::is_running());

    // A flush and a second stop after the stop are no-ops, and the record sites
    // stay silent rather than collecting into the shut-down provider.
    metrics::flush_global_metrics();
    metrics::shutdown_global_metrics();
    metrics::record_log_drop("stdout");

    let refused = metrics::init_global_metrics_with_addr(ephemeral());
    assert!(
        matches!(
            refused,
            Err(MetricsInitError::Stopped {
                reason: metrics::StopReason::Shutdown
            })
        ),
        "init after a stop is refused, got {refused:?}"
    );

    // The retry sanity check rides the phase machine directly: the same request
    // that a fresh phase would start is refused from the stopped one.
    assert_eq!(
        metrics::phase().decide_init(ephemeral()),
        InitDecision::Refused {
            reason: metrics::StopReason::Shutdown
        }
    );
}
