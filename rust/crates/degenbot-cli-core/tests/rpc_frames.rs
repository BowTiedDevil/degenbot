//! Frame-level + wire-hygiene tests for the operator command-channel client
//! (ADR-051 D6; ergo 6RNZDT).
//!
//! The client is exercised two ways:
//!
//! - **pure frame tests** over [`decode_response`]/[`render_json_sorted`]:
//!   happy `ok`, structured `{"ok": false}`, the host's unknown-op reply,
//!   and malformed JSON (which must never panic);
//! - **loopback socket tests** against an in-test std UDS harness that writes
//!   fixture response frames (not a live Python host), proving the transport
//!   reads one request line and one response line;
//! - **wire-hygiene refusals**: an unknown `cordon_*` key, an empty posture
//!   patch, and an unknown hop-family string are refused BEFORE any socket is
//!   touched (the socket paths in these tests do not exist, so an attempted
//!   connect would surface as a protocol error instead).
#![expect(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::path::Path;

use degenbot_cli_core::operator::{
    decode_response, parse_hop_token, parse_sim_intake_floor, render_json_sorted, resolve_socket,
    send_request, validate_posture_patch, PosturePatchEntry, PosturePatchValue, WireRequest,
    WireResponse,
};
use degenbot_cli_core::{
    run, CliContext, CliError, Command, ExitCode, FleetCommand, PathCommand, Prompter,
};
// The report/decoded-frame types the Unix loopback transports assert over.
#[cfg(unix)]
use degenbot_cli_core::{CommandReport, FleetReport, PathDirection, PathReport};
use degenbot_config::MapEnv;
use serde_json::json;
#[cfg(unix)]
use serde_json::Value;
use tempfile::TempDir;

/// A prompter that never confirms (no arm here prompts).
struct NoPrompt;

impl Prompter for NoPrompt {
    fn confirm(&self, _message: &str, _default: bool) -> bool {
        false
    }
}

fn empty_env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}

/// Bind `socket`, accept ONE connection, return the request line, and write
/// the newline-terminated `body` back. Uses std blocking UDS so the test needs
/// no runtime of its own.
#[cfg(unix)]
fn serve_once(socket: &Path, body: &'static str) -> std::thread::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        stream.write_all(&[10u8]).unwrap();
        stream.flush().unwrap();
        line
    })
}

fn closed_port_socket(dir: &TempDir) -> String {
    // Never bound: reaching the transport would produce OperatorProtocol, so a
    // wire-hygiene error proves the refusal happened before talk.
    dir.path().join("no-listener.sock").display().to_string()
}

// -- pure frame decoding ----------------------------------------------------

#[test]
fn decode_response_accepts_a_happy_ok_frame() {
    let decoded = decode_response("{\"ok\": true, \"detail\": \"path enqueued\"}").unwrap();
    assert!(decoded.is_ok());
    assert_eq!(decoded.detail(), "path enqueued");
    assert_eq!(decoded.effective(), None);
    assert_eq!(decoded.error(), None);
}

#[test]
fn decode_response_accepts_an_effective_echo() {
    let decoded = decode_response(
        "{\"ok\": true, \"detail\": \"\", \"effective\": {\"posture\": \"Nominal\"}}",
    )
    .unwrap();
    assert_eq!(decoded.effective(), Some(&json!({ "posture": "Nominal" })));
}

#[test]
fn decode_response_maps_a_structured_not_ok_frame_to_a_refusal() {
    let decoded = decode_response("{\"ok\": false, \"error\": \"patch refused\"}").unwrap();
    assert_eq!(
        decoded,
        WireResponse::Err {
            error: "patch refused".to_string()
        }
    );
    assert!(!decoded.is_ok());
    assert_eq!(decoded.error(), Some("patch refused"));
}

#[test]
fn unknown_op_server_response_is_a_refusal_never_a_panic() {
    let decoded = decode_response("{\"ok\": false, \"error\": \"unknown op 'bogus'\"}").unwrap();
    assert_eq!(decoded.error(), Some("unknown op 'bogus'"));
}

#[test]
fn decode_response_rejects_malformed_and_schema_less_frames() {
    for raw in [
        "",
        "not json",
        "[1, 2]",
        "{\"detail\": \"missing ok\"}",
        "{\"ok\": \"yes\"}",
    ] {
        let err = decode_response(raw).unwrap_err();
        assert!(
            matches!(err, CliError::OperatorProtocol(_)),
            "raw {raw:?} must be a protocol error, got {err:?}"
        );
    }
}

#[test]
fn render_json_sorted_matches_python_sort_keys() {
    let effective = json!({
        "posture": "Cordoned",
        "cordon_enter_events": 2,
        "cordon_duty_percent": 2.5
    });
    assert_eq!(
        render_json_sorted(Some(&effective)),
        "{\"cordon_duty_percent\": 2.5, \"cordon_enter_events\": 2, \"posture\": \"Cordoned\"}"
    );
    assert_eq!(render_json_sorted(None), "{}");
}

// -- loopback transport -----------------------------------------------------

#[cfg(unix)]
#[test]
fn happy_ok_frame_round_trips_over_a_loopback_socket() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("operator.sock");
    let handle = serve_once(
        &socket,
        "{\"ok\": true, \"detail\": \"enqueued 2-hop path\"}",
    );
    let request = WireRequest::AddPath {
        steps: vec![
            parse_hop_token("v3:0x1111111111111111111111111111111111111111").unwrap(),
            parse_hop_token("v4:0x2222222222222222222222222222222222222222:0xaa").unwrap(),
        ],
        directions: Some(vec![true, false]),
    };
    let response = send_request(&socket, &request).unwrap();
    assert_eq!(response.detail(), "enqueued 2-hop path");

    let line = handle.join().unwrap();
    assert_eq!(
        line.as_bytes().last().copied(),
        Some(10u8),
        "line is newline-terminated"
    );
    let decoded: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(decoded["op"], "add_path");
    assert_eq!(decoded["payload"]["directions"], json!([true, false]));
    assert_eq!(decoded["payload"]["steps"][0]["family"], "V3");
    assert_eq!(
        decoded["payload"]["steps"][1]["hash"], "0xaa",
        "the V4 pool id is carried"
    );
    assert!(decoded["payload"]["steps"][0].get("hash").is_none());
}

#[cfg(unix)]
#[test]
fn malformed_json_from_the_server_is_a_protocol_error_over_the_socket() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("operator.sock");
    let handle = serve_once(&socket, "definitely not json");
    let err = send_request(&socket, &WireRequest::GetFleetPosture).unwrap_err();
    assert!(matches!(err, CliError::OperatorProtocol(_)), "got {err:?}");
    let _ = handle.join();
}

#[test]
fn unreachable_socket_is_a_protocol_error() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("no-listener.sock");
    let err = send_request(&socket, &WireRequest::GetFleetPosture).unwrap_err();
    assert!(matches!(err, CliError::OperatorProtocol(_)), "got {err:?}");
}

/// Off Unix there is no transport at all: the one transport site returns the
/// typed protocol refusal instead of the crate failing to build (the Windows
/// wheel regression). The message names the missing Unix domain socket.
#[cfg(not(unix))]
#[test]
fn no_unix_domain_socket_is_a_protocol_refusal() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("no-listener.sock");
    let err = send_request(&socket, &WireRequest::GetFleetPosture).unwrap_err();
    assert!(matches!(err, CliError::OperatorProtocol(_)), "got {err:?}");
    assert_eq!(ExitCode::from(&err), ExitCode::Failure);
    assert!(
        err.message().contains("requires a Unix domain socket"),
        "got {err:?}"
    );
}

// -- arm-level happy path ---------------------------------------------------

#[cfg(unix)]
#[test]
fn path_add_over_the_wire_returns_the_host_detail() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("operator.sock");
    let handle = serve_once(
        &socket,
        "{\"ok\": true, \"detail\": \"enqueued 2-hop path\"}",
    );
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Path(PathCommand::Add {
            socket: Some(socket.display().to_string()),
            hops: vec![
                "v2:0x1111111111111111111111111111111111111111".to_string(),
                "v4:0x2222222222222222222222222222222222222222:0xaa".to_string(),
            ],
            direction: Some(PathDirection::Zfo),
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(CommandReport::Path(PathReport::Added { detail })) = outcome.report() else {
        panic!("expected Added, got {:?}", outcome.report());
    };
    assert_eq!(detail, "enqueued 2-hop path");
    let line = handle.join().unwrap();
    let decoded: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(decoded["payload"]["directions"], json!([true, true]));
}

#[cfg(unix)]
#[test]
fn fleet_posture_show_renders_the_sorted_effective_policy() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("operator.sock");
    let handle = serve_once(
        &socket,
        "{\"ok\": true, \"detail\": \"\", \"effective\": {\"posture\": \"Nominal\",          \"cordon_enter_events\": 2}}",
    );
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Fleet(FleetCommand::PostureShow {
            socket: Some(socket.display().to_string()),
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Success);
    let Some(CommandReport::Fleet(FleetReport::Posture { effective })) = outcome.report() else {
        panic!("expected Posture, got {:?}", outcome.report());
    };
    assert_eq!(
        effective,
        "{\"cordon_enter_events\": 2, \"posture\": \"Nominal\"}"
    );
    let line = handle.join().unwrap();
    let decoded: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(decoded["op"], "get_fleet_posture");
}

#[cfg(unix)]
#[test]
fn fleet_posture_show_maps_a_host_refusal_to_exit_one() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("operator.sock");
    let handle = serve_once(
        &socket,
        "{\"ok\": false, \"error\": \"unknown op 'bogus'\"}",
    );
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Fleet(FleetCommand::PostureShow {
            socket: Some(socket.display().to_string()),
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(matches!(
        outcome.error(),
        Some(CliError::OperatorRefused(_))
    ));
    assert_eq!(outcome.error().unwrap().message(), "unknown op 'bogus'");
    let _ = handle.join();
}

// -- wire hygiene (refused before the socket is touched) --------------------

#[test]
fn unknown_posture_key_is_refused_before_talk() {
    let dir = TempDir::new().unwrap();
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Fleet(FleetCommand::PostureSet {
            socket: Some(closed_port_socket(&dir)),
            patch: vec![PosturePatchEntry::int("not_a_cordon_key", 2)],
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    let err = outcome.error().unwrap();
    assert!(matches!(err, CliError::OperatorHygiene(_)), "got {err:?}");
    assert_eq!(
        err.message(),
        "unknown fleet-posture threshold key(s): not_a_cordon_key"
    );
    assert!(validate_posture_patch(&[PosturePatchEntry::int("nope", 1)]).is_err());
}

#[test]
fn empty_posture_patch_is_refused_before_talk() {
    let dir = TempDir::new().unwrap();
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Fleet(FleetCommand::PostureSet {
            socket: Some(closed_port_socket(&dir)),
            patch: Vec::new(),
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    let err = outcome.error().unwrap();
    assert!(matches!(err, CliError::OperatorHygiene(_)), "got {err:?}");
    assert_eq!(
        err.message(),
        "set_fleet_posture needs at least one threshold key (empty patch)"
    );
}

#[test]
fn unknown_hop_family_is_refused_before_talk() {
    let dir = TempDir::new().unwrap();
    let env = empty_env();
    let ctx = CliContext::new(&env);
    let outcome = run(
        &Command::Path(PathCommand::Add {
            socket: Some(closed_port_socket(&dir)),
            hops: vec!["v5:0x1111111111111111111111111111111111111111".to_string()],
            direction: None,
        }),
        &ctx,
        &NoPrompt,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(
        matches!(outcome.error(), Some(CliError::OperatorHygiene(_))),
        "got {:?}",
        outcome.error()
    );
    let direct = parse_hop_token("v5:0x1111").unwrap_err();
    assert!(
        matches!(direct, CliError::OperatorHygiene(_)),
        "got {direct:?}"
    );
    assert!(
        parse_hop_token("v3").is_err(),
        "a hop without an address is refused"
    );
}

// -- posture value + socket resolution --------------------------------------

#[test]
fn posture_patch_accepts_a_known_subset_and_the_null_restore_sentinel() {
    let patch = vec![
        PosturePatchEntry::int("cordon_enter_events", 2),
        PosturePatchEntry::float("cordon_duty_percent", 2.5),
        PosturePatchEntry::new(
            "cordon_sim_intake_floor",
            parse_sim_intake_floor("null").unwrap(),
        ),
    ];
    assert!(validate_posture_patch(&patch).is_ok());
    assert_eq!(
        parse_sim_intake_floor("null").unwrap(),
        PosturePatchValue::Null
    );
    assert_eq!(
        parse_sim_intake_floor("2").unwrap(),
        PosturePatchValue::Int(2)
    );
    assert!(parse_sim_intake_floor("half").is_err());
}

#[test]
fn socket_resolution_prefers_cli_then_env_then_the_expanded_default() {
    let env = MapEnv::new(BTreeMap::from([
        ("HOME".to_string(), "/home/tester".to_string()),
        (
            "DEGENBOT_OPERATOR_SOCKET".to_string(),
            "/tmp/from-env.sock".to_string(),
        ),
    ]));
    assert_eq!(
        resolve_socket(&env, Some("/tmp/from-cli.sock")),
        std::path::PathBuf::from("/tmp/from-cli.sock")
    );
    assert_eq!(
        resolve_socket(&env, None),
        std::path::PathBuf::from("/tmp/from-env.sock")
    );
    let bare = MapEnv::new(BTreeMap::from([(
        "HOME".to_string(),
        "/home/tester".to_string(),
    )]));
    assert_eq!(
        resolve_socket(&bare, None),
        std::path::PathBuf::from("/home/tester/.config/degenbot/operator.sock")
    );
}
