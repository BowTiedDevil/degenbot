//! Live end-to-end probe: one real span through the production provider
//! constructor (`provider_from_endpoint`) into a REAL Jaeger, on both driver
//! runtime flavors, with receipt verified through Jaeger's query API.
//!
//! Opt-in: needs `DEGENBOT_JAEGER_E2E=1`, an OTLP/HTTP collector
//! (`DEGENBOT_JAEGER_ENDPOINT`, default `http://127.0.0.1:4318`) and Jaeger's
//! query API (`DEGENBOT_JAEGER_QUERY_ENDPOINT`, default
//! `http://127.0.0.1:16686`).
#![cfg(feature = "otel")]
#![expect(clippy::expect_used, clippy::print_stderr)] // live probe: `.expect` with context is the failure report; eprintln is the report channel

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::SubscriberExt;

/// Operation name only the probes use, so the Jaeger query filters to them.
/// Must equal the `info_span!` name in [`probe_through_jaeger`].
const PROBE_OPERATION: &str = "degenbot.jaeger.e2e.probe";

/// Both live endpoints, or `None` when the gate is closed.
fn live_endpoints() -> Option<(String, String)> {
    if std::env::var("DEGENBOT_JAEGER_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skipped: set DEGENBOT_JAEGER_E2E=1 with an OTLP collector at \
             DEGENBOT_JAEGER_ENDPOINT and Jaeger's query API at \
             DEGENBOT_JAEGER_QUERY_ENDPOINT"
        );
        return None;
    }
    let otlp = std::env::var("DEGENBOT_JAEGER_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:4318".into());
    let query = std::env::var("DEGENBOT_JAEGER_QUERY_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:16686".into());
    Some((otlp, query))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs()
}

/// Emission time → UTC date-time. Jaeger's query API takes RFC 3339 bounds;
/// the probe needs nothing beyond a civil date, so the conversion stays local
/// (days-from-civil, Howard Hinnant's algorithm).
fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400).cast_signed();
    let seconds_of_day = unix_secs % 86_400;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

/// Minimal blocking HTTP GET (the probe binary has no HTTP client dependency):
/// returns the response body.
fn http_get(url: &str) -> String {
    let rest = url.strip_prefix("http://").expect("plain-http query URL");
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let mut stream = TcpStream::connect(authority).expect("connect Jaeger query API");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .expect("write query request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read query response");
    response
        .split_once("\r\n\r\n")
        .map_or(response.clone(), |(_, body)| body.to_string())
}

/// Emit one span on the given runtime, flush it, then assert Jaeger returns it.
fn probe_through_jaeger(runtime: &tokio::runtime::Runtime, flavor: &str) {
    let Some((otlp, query)) = live_endpoints() else {
        return;
    };
    let started_at = unix_seconds();
    // Unique per run, so the query proves THIS run's span arrived rather than a
    // previous one with the same operation name.
    let probe_id = format!("{flavor}.{}-{}", std::process::id(), started_at);
    let (provider, tracer) =
        degenbot_bot::otel::provider_from_endpoint(&otlp).expect("exporter build");

    runtime.block_on(async {
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(degenbot_bot::otel::layer(tracer)),
            || {
                let span = tracing::info_span!(
                    "degenbot.jaeger.e2e.probe",
                    probe.id = probe_id.as_str(),
                    flavor = flavor
                );
                let _entered = span.enter();
                tracing::info!("e2e probe event");
            },
        );
    });
    provider.force_flush().expect("flush to the OTLP collector");

    // Jaeger indexes by operation name; a bogus name answers 404, so the
    // filtered result set is this probe family only.
    let window = format!(
        "query.start_time_min={}&query.start_time_max={}",
        // The collector stamps its own start time; a minute of slack covers the
        // SDK's clock read and any collector-side rounding.
        rfc3339_utc(started_at.saturating_sub(60)),
        rfc3339_utc(started_at + 3_600)
    );
    let traces_url = format!(
        "{query}/api/v3/traces?query.service_name=degenbot-bot&query.operation_name={PROBE_OPERATION}&{window}&query.search_depth=20"
    );
    // The collector stores the batch asynchronously; poll until the probe is
    // queryable rather than racing its ingest.
    let mut query_body = String::new();
    for attempt in 0..40 {
        query_body = http_get(&traces_url);
        if query_body.contains(&probe_id) {
            eprintln!(
                "probe {probe_id} flushed to {otlp}; Jaeger returned it after {attempt} poll(s)"
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        query_body.contains(&probe_id),
        "Jaeger must return the probe span tagged probe.id={probe_id}; response was: {query_body}"
    );
}

#[test]
fn span_reaches_real_jaeger_from_a_current_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    probe_through_jaeger(&runtime, "current-thread");
}

#[test]
fn span_reaches_real_jaeger_from_a_multi_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    probe_through_jaeger(&runtime, "multi-thread");
}

#[test]
fn rfc3339_utc_formats_known_instants() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    // Leap day: the civil conversion must not drift on a 400-year era boundary.
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
}
