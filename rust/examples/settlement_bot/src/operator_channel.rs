//! Operator command channel: JSON-lines over a Unix domain socket — parity
//! ledger row 20 (Gap G5).
//!
//! Mirrors `src/degenbot/operator/operator_channel.py` (the `OperatorServer`
//! wire guard + `step_from_wire` + `handle_fleet_posture_op`), driven by the
//! same four ops the Python example's `operator_handler` serves:
//! `add_path` / `discover` / `set_fleet_posture` / `get_fleet_posture`.
//!
//! Wire protocol (one JSON object per line, newline-terminated). Request
//! lines carry the op at the top level and the payload nested — the exact
//! shape `send_command` and `_decode_request` speak:
//!
//!     {"op": "add_path", "payload": {"steps": [...], "directions": [true, false]}}
//!     {"op": "discover", "payload": {"bound": 5}}
//!     {"op": "set_fleet_posture", "payload": {"cordon_enter_events": 2}}
//!     {"op": "get_fleet_posture", "payload": {}}
//!
//! Response lines (byte-for-byte `json.dumps` spelling: `": "` / `", "`):
//!
//!     {"ok": true, "detail": "..."}
//!     {"ok": true, "detail": "", "effective": {...}}
//!     {"ok": false, "error": "..."}
//!
//! The fleet-posture ops route through the `degenbot::workers::posture`
//! umbrella surface (`process()` / `PosturePolicyPatch` / `PosturePolicy`) —
//! the same process owner the Python `degenbot._ffi.fleet` mirror mints, so
//! the reach claim is proven by construction (row 20 is DRIVER-POLICY, not a
//! new gap). The host is the driver-local transport: `add_path`/`discover`
//! delegate to a [`PathOps`] sink (the driver's registration pipeline), and
//! the wire guard never crashes on a malformed command.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use degenbot::db::discovery_read::DiscoveryPoolRow;
use degenbot::pathfinding::PoolKind;
use degenbot::workers::posture::{
    FleetPosture, PosturePolicy, PosturePolicyPatch, PostureRetuneError,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::discovery::{build_graph, BatchedPathFinder, BuiltGraph, DiscoveryParams, PoolNode};
use crate::pipeline::RegistrationPipeline;
use crate::policy::PathPolicy;
use crate::retry::RetryPolicy;

/// Default per-request read timeout (mirrors `OperatorServer`'s
/// `request_timeout` default).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The six fleet-posture threshold key names `set_fleet_posture` accepts
/// (mirrors `FLEET_POSTURE_THRESHOLD_KEYS`).
const FLEET_POSTURE_THRESHOLD_KEYS: [&str; 6] = [
    "cordon_enter_events",
    "cordon_enter_window_ms",
    "cordon_duty_percent",
    "cordon_duty_window_ms",
    "cordon_exit_clean_ms",
    "cordon_sim_intake_floor",
];

/// A hop descriptor in the wire `steps` shape (mirrors `StepSpec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireStep {
    /// The pool family (`StepSpec.type`).
    pub family: PoolFamily,
    /// The pool address (`0x` + 40 hex).
    pub address: String,
    /// The V4 pool id (`0x` + 64 hex) when `family` is V4.
    pub hash: Option<String>,
}

/// The wire `family` string translated to the pool family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolFamily {
    /// `"V2"`.
    V2,
    /// `"V3"`.
    V3,
    /// `"V4"`.
    V4,
}

impl PoolFamily {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V2 => "V2",
            Self::V3 => "V3",
            Self::V4 => "V4",
        }
    }

    /// The pathfinding pool kind.
    #[must_use]
    pub const fn pool_kind(self) -> PoolKind {
        match self {
            Self::V2 => PoolKind::V2,
            Self::V3 => PoolKind::V3,
            Self::V4 => PoolKind::V4,
        }
    }
}

/// Translate a wire `steps` entry into a [`WireStep`] (mirrors
/// `step_from_wire`).
///
/// # Errors
///
/// Returns the Python `ValueError` message text when `family` is not
/// V2/V3/V4 or `address` is absent/empty.
pub fn step_from_wire(step: &Value) -> Result<WireStep, String> {
    let family_value = step.get("family");
    let family = match family_value.and_then(Value::as_str) {
        Some("V2") => PoolFamily::V2,
        Some("V3") => PoolFamily::V3,
        Some("V4") => PoolFamily::V4,
        _ => {
            let shown = family_value.map_or_else(|| "None".to_string(), python_repr);
            return Err(format!("unknown pool family {shown} (expected V2|V3|V4)"));
        }
    };
    let address = step
        .get("address")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{} step is missing an address", family.as_str()))?;
    Ok(WireStep {
        family,
        address: address.to_string(),
        hash: step
            .get("hash")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

/// The driver-side sink the `add_path` / `discover` ops delegate to.
pub trait PathOps: Send + Sync {
    /// Enqueue one operator path; return the hop count for the detail text.
    fn enqueue_path(
        &self,
        steps: &[WireStep],
        directions: Option<&[bool]>,
    ) -> Result<usize, String>;

    /// Run a bounded discovery sweep; return the number of paths processed.
    fn trigger_discovery(&self, bound: Option<usize>) -> Result<usize, String>;
}

/// The production [`PathOps`]: enqueue/discover through a driver
/// [`RegistrationPipeline`] over a graph built from the same discovery rows.
pub struct PipelinePathOps {
    graph: BuiltGraph,
    params: DiscoveryParams,
    pipeline: Mutex<RegistrationPipeline>,
    input_token: String,
    weth: String,
}

impl PipelinePathOps {
    /// Build the sink from the boot discovery rows + driver policy.
    #[expect(
        clippy::too_many_arguments,
        reason = "linear projection of the boot discovery/policy state into the operator sink"
    )]
    #[must_use]
    pub fn new(
        rows: &[DiscoveryPoolRow],
        requested_kinds: &[PoolKind],
        allowed: &std::collections::BTreeSet<String>,
        params: &DiscoveryParams,
        policy: PathPolicy,
        retry_policy: RetryPolicy,
        input_token: String,
        weth: String,
    ) -> Self {
        Self {
            graph: build_graph(rows, requested_kinds, Some(allowed)),
            params: params.clone(),
            pipeline: Mutex::new(RegistrationPipeline::new(policy, retry_policy)),
            input_token,
            weth,
        }
    }
}

fn resolve_node<'a>(graph: &'a BuiltGraph, step: &WireStep) -> Option<&'a PoolNode> {
    let address = step.address.to_lowercase();
    let hash = step.hash.as_ref().map(|h| h.to_lowercase());
    graph.nodes.iter().find(|node| match step.family {
        PoolFamily::V2 => {
            node.kind == PoolKind::V2 && node.address.as_deref() == Some(address.as_str())
        }
        PoolFamily::V3 => {
            node.kind == PoolKind::V3 && node.address.as_deref() == Some(address.as_str())
        }
        PoolFamily::V4 => node.kind == PoolKind::V4 && node.pool_hash.as_deref() == hash.as_deref(),
    })
}

impl PathOps for PipelinePathOps {
    fn enqueue_path(
        &self,
        steps: &[WireStep],
        _directions: Option<&[bool]>,
    ) -> Result<usize, String> {
        let mut path: Vec<(u64, PoolKind)> = Vec::with_capacity(steps.len());
        let mut resolved = true;
        for step in steps {
            if let Some(node) = resolve_node(&self.graph, step) {
                path.push((node.graph_id, node.kind));
            } else {
                resolved = false;
                break;
            }
        }
        if resolved {
            let mut pipeline = match self.pipeline.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let _ = pipeline.prepare_candidate(&self.graph, &path, &self.input_token, &self.weth);
        }
        Ok(steps.len())
    }

    fn trigger_discovery(&self, bound: Option<usize>) -> Result<usize, String> {
        let mut finder = BatchedPathFinder::new(&self.graph.graph, &self.params);
        let mut pipeline = match self.pipeline.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut count = 0_usize;
        while let Some(batch) = finder.next_batch() {
            for path in &batch {
                if bound.is_some_and(|limit| count >= limit) {
                    return Ok(count);
                }
                let _ =
                    pipeline.prepare_candidate(&self.graph, path, &self.input_token, &self.weth);
                count += 1;
            }
        }
        Ok(count)
    }
}

/// A decoded handler success: `detail` plus an optional raw effective-policy
/// JSON object string (already in Python `json.dumps` spelling).
#[derive(Debug)]
struct HandlerResponse {
    detail: String,
    effective: Option<String>,
}

/// `json.dumps`'s default `ensure_ascii` escaping for the ASCII subset the
/// wire uses.
fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            other if (other as u32) < 0x20 => {
                let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out
}

/// The `{"ok": true, ...}` response string (`json.dumps` spelling).
fn wire_ok(detail: &str, effective: Option<&str>) -> String {
    match effective {
        Some(effective) => format!(
            "{{\"ok\": true, \"detail\": \"{}\", \"effective\": {effective}}}",
            json_escape(detail)
        ),
        None => format!("{{\"ok\": true, \"detail\": \"{}\"}}", json_escape(detail)),
    }
}

/// The `{"ok": false, "error": ...}` response string (`json.dumps` spelling).
fn wire_err(error: &str) -> String {
    format!("{{\"ok\": false, \"error\": \"{}\"}}", json_escape(error))
}

/// A Python `repr()` spelling for the unknown-op / unknown-family messages.
fn python_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// A Python type name for a rejection message.
fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn usize_threshold(key: &str, value: &Value) -> Result<usize, String> {
    if value.is_boolean() {
        return Err(format!(
            "PostureRetuneError: {key}: expected a number, got bool (bool)"
        ));
    }
    match value.as_u64().and_then(|v| usize::try_from(v).ok()) {
        Some(v) => Ok(v),
        None => Err(format!(
            "PostureRetuneError: {key}: expected an int, got {}",
            python_type_name(value)
        )),
    }
}

fn u64_threshold(key: &str, value: &Value) -> Result<u64, String> {
    if value.is_boolean() {
        return Err(format!(
            "PostureRetuneError: {key}: expected a number, got bool (bool)"
        ));
    }
    value.as_u64().ok_or_else(|| {
        format!(
            "PostureRetuneError: {key}: expected an int, got {}",
            python_type_name(value)
        )
    })
}

fn f64_threshold(key: &str, value: &Value) -> Result<f64, String> {
    if value.is_boolean() {
        return Err(format!(
            "PostureRetuneError: {key}: expected a number, got bool (bool)"
        ));
    }
    value.as_f64().ok_or_else(|| {
        format!(
            "PostureRetuneError: {key}: expected a number, got {}",
            python_type_name(value)
        )
    })
}

fn patch_from_json(obj: &serde_json::Map<String, Value>) -> Result<PosturePolicyPatch, String> {
    let mut patch = PosturePolicyPatch::default();
    for (key, value) in obj {
        match key.as_str() {
            "cordon_enter_events" => patch.enter_events = Some(usize_threshold(key, value)?),
            "cordon_enter_window_ms" => patch.enter_window_ms = Some(u64_threshold(key, value)?),
            "cordon_duty_percent" => patch.duty_percent = Some(f64_threshold(key, value)?),
            "cordon_duty_window_ms" => patch.duty_window_ms = Some(u64_threshold(key, value)?),
            "cordon_exit_clean_ms" => patch.exit_clean_ms = Some(u64_threshold(key, value)?),
            "cordon_sim_intake_floor" => {
                patch.sim_intake_floor_override = if value.is_null() {
                    Some(None)
                } else {
                    Some(Some(usize_threshold(key, value)?))
                };
            }
            _ => {
                return Err(format!(
                    "PostureRetuneError: {key}: unknown fleet-posture threshold key \
                     (expected one of the six cordon_* keys)"
                ));
            }
        }
    }
    Ok(patch)
}

/// The posture enum's wire spelling (mirrors `posture_name`).
fn posture_name(posture: FleetPosture) -> &'static str {
    match posture {
        FleetPosture::Nominal => "Nominal",
        FleetPosture::Cordoned => "Cordoned",
    }
}

/// The effective-policy JSON object in the FFI's insertion order (all six
/// fields + `posture`) — the ONE echo shape both fleet verbs return.
fn effective_json(policy: PosturePolicy, posture: FleetPosture) -> String {
    let floor = match policy.sim_intake_floor_override {
        Some(floor) => floor.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"cordon_enter_events\": {}, \"cordon_enter_window_ms\": {}, \
         \"cordon_duty_percent\": {:?}, \"cordon_duty_window_ms\": {}, \
         \"cordon_exit_clean_ms\": {}, \"cordon_sim_intake_floor\": {floor}, \
         \"posture\": \"{}\"}}",
        policy.enter_events,
        policy.enter_window_ms,
        policy.duty_percent,
        policy.duty_window_ms,
        policy.exit_clean_ms,
        posture_name(posture),
    )
}

/// Handle the two fleet-posture ops through `degenbot::workers::posture`
/// (mirrors `handle_fleet_posture_op`).
fn handle_fleet_posture(op: &str, payload: &Value) -> Result<HandlerResponse, String> {
    let owner = degenbot::workers::posture::process();
    if op == "get_fleet_posture" {
        return Ok(HandlerResponse {
            detail: String::new(),
            effective: Some(effective_json(owner.policy(), owner.current())),
        });
    }
    let obj = payload
        .as_object()
        .ok_or_else(|| "'payload' must be a dict".to_string())?;
    let mut unknown: Vec<String> = obj
        .keys()
        .filter(|key| !FLEET_POSTURE_THRESHOLD_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown fleet-posture threshold key(s): {}",
            unknown.join(", ")
        ));
    }
    if obj.is_empty() {
        return Err("set_fleet_posture needs at least one threshold key (empty patch)".to_string());
    }
    let patch = patch_from_json(obj)?;
    patch
        .validate()
        .map_err(|err: PostureRetuneError| format!("PostureRetuneError: {err}"))?;
    let effective = owner.policy().patched_with(patch);
    owner.retune(effective);
    Ok(HandlerResponse {
        detail: String::new(),
        effective: Some(effective_json(owner.policy(), owner.current())),
    })
}

fn dispatch_add_path(payload: &Value, ops: &dyn PathOps) -> String {
    let Some(steps_value) = payload.get("steps") else {
        return wire_err("KeyError: 'steps'");
    };
    let Some(steps_array) = steps_value.as_array() else {
        return wire_err("TypeError: 'steps' must be a list");
    };
    let mut steps: Vec<WireStep> = Vec::with_capacity(steps_array.len());
    for step in steps_array {
        if !step.is_object() {
            return wire_err("ValueError: step must be an object");
        }
        match step_from_wire(step) {
            Ok(parsed) => steps.push(parsed),
            Err(message) => return wire_err(&format!("ValueError: {message}")),
        }
    }
    let directions: Option<Vec<bool>> = payload
        .get("directions")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_bool).collect());
    match ops.enqueue_path(&steps, directions.as_deref()) {
        Ok(count) => wire_ok(&format!("enqueued {count}-hop path"), None),
        Err(error) => wire_err(&error),
    }
}

fn dispatch_discover(payload: &Value, ops: &dyn PathOps) -> String {
    let bound = payload
        .get("bound")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    match ops.trigger_discovery(bound) {
        Ok(count) => wire_ok(&format!("discovery processed {count} paths"), None),
        Err(error) => wire_err(&error),
    }
}

/// Decode + dispatch one request line into the final wire response string.
fn dispatch_request(line: &[u8], ops: &dyn PathOps) -> String {
    let request: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(error) => return wire_err(&format!("invalid request JSON: {error}")),
    };
    let Some(obj) = request.as_object() else {
        return wire_err("invalid request JSON: request must be a JSON object");
    };
    let Some(op_value) = obj.get("op") else {
        return wire_err("request is missing 'op'");
    };
    let payload = obj
        .get("payload")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if !payload.is_object() {
        return wire_err("'payload' must be a dict");
    }
    let Some(op) = op_value.as_str() else {
        return wire_err(&format!("unknown op {}", python_repr(op_value)));
    };
    match op {
        "add_path" => dispatch_add_path(&payload, ops),
        "discover" => dispatch_discover(&payload, ops),
        "set_fleet_posture" | "get_fleet_posture" => match handle_fleet_posture(op, &payload) {
            Ok(response) => wire_ok(&response.detail, response.effective.as_deref()),
            Err(error) => wire_err(&error),
        },
        _ => wire_err(&format!("unknown op {}", python_repr(op_value))),
    }
}

/// The accept loop behind [`start_operator_server`].
async fn serve(listener: UnixListener, ops: Arc<dyn PathOps>, mut shutdown: oneshot::Receiver<()>) {
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let ops = Arc::clone(&ops);
                        tokio::spawn(handle_client(stream, ops, DEFAULT_REQUEST_TIMEOUT));
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// One client connection: one request line in, one response line out.
async fn handle_client(stream: UnixStream, ops: Arc<dyn PathOps>, request_timeout: Duration) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let read = tokio::time::timeout(request_timeout, reader.read_until(b'\n', &mut line)).await;
    let response = match read {
        // Request timeout: drop the connection with no response (mirrors the
        // Python warning-and-return path).
        Err(_) | Ok(Err(_)) => return,
        Ok(Ok(0)) => wire_err("empty request"),
        Ok(Ok(_)) => dispatch_request(&line, &*ops),
    };
    let mut bytes = response.into_bytes();
    bytes.push(b'\n');
    let _ = writer.write_all(&bytes).await;
    let _ = writer.shutdown().await;
}

/// A running operator server. [`Self::close`] mirrors Python
/// `OperatorServer.close`: stop accepting, then unlink the socket file.
#[derive(Debug)]
pub struct RunningOperator {
    handle: JoinHandle<()>,
    shutdown: Option<oneshot::Sender<()>>,
    path: PathBuf,
}

impl RunningOperator {
    /// Gracefully stop the accept loop and remove the socket file.
    pub async fn close(mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        let _ = self.handle.await;
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind `path` and spawn the operator server (the CLI `--operator-socket`
/// surface).
///
/// # Errors
///
/// Returns a message when the socket cannot be bound.
pub fn start_operator_server(
    path: &Path,
    ops: Arc<dyn PathOps>,
) -> Result<RunningOperator, String> {
    // The example's socket is ephemeral; clear a stale path before binding.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .map_err(|error| format!("bind operator socket {}: {error}", path.display()))?;
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let handle = tokio::spawn(serve(listener, ops, shutdown_receiver));
    Ok(RunningOperator {
        handle,
        shutdown: Some(shutdown_sender),
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeOps {
        discovered: usize,
    }

    impl PathOps for FakeOps {
        fn enqueue_path(
            &self,
            steps: &[WireStep],
            _directions: Option<&[bool]>,
        ) -> Result<usize, String> {
            Ok(steps.len())
        }

        fn trigger_discovery(&self, _bound: Option<usize>) -> Result<usize, String> {
            Ok(self.discovered)
        }
    }

    fn req(json: &str) -> String {
        dispatch_request(json.as_bytes(), &FakeOps::default())
    }

    #[test]
    fn add_path_response_bytes_match_python() {
        let response = req(
            r#"{"op":"add_path","payload":{"steps":[{"family":"V2","address":"0x1111111111111111111111111111111111111111"},{"family":"V3","address":"0x2222222222222222222222222222222222222222"}]}}"#,
        );
        assert_eq!(response, r#"{"ok": true, "detail": "enqueued 2-hop path"}"#);
    }

    #[test]
    fn unknown_op_error_bytes_match_python() {
        assert_eq!(
            req(r#"{"op":"bogus","payload":{}}"#),
            r#"{"ok": false, "error": "unknown op 'bogus'"}"#
        );
    }

    #[test]
    fn discover_response_bytes_match_python() {
        let ops = FakeOps { discovered: 5 };
        let response = dispatch_request(br#"{"op":"discover","payload":{"bound":5}}"#, &ops);
        assert_eq!(
            response,
            r#"{"ok": true, "detail": "discovery processed 5 paths"}"#
        );
    }

    #[test]
    fn get_fleet_posture_response_shape_matches_python() {
        let response = req(r#"{"op":"get_fleet_posture","payload":{}}"#);
        assert!(
            response
                .starts_with(r#"{"ok": true, "detail": "", "effective": {"cordon_enter_events":"#),
            "got {response}"
        );
        assert!(
            response.contains("\"posture\": \"Nominal\""),
            "got {response}"
        );
    }

    #[test]
    fn decode_and_shape_errors_match_python() {
        assert_eq!(
            req("not json"),
            r#"{"ok": false, "error": "invalid request JSON: expected ident at line 1 column 2"}"#
        );
        assert_eq!(
            req(r#"{"payload":{}}"#),
            r#"{"ok": false, "error": "request is missing 'op'"}"#
        );
        assert_eq!(
            req(r#"{"op":"add_path","payload":{}}"#),
            r#"{"ok": false, "error": "KeyError: 'steps'"}"#
        );
        assert_eq!(
            req(r#"{"op":"add_path","payload":{"steps":[{"family":"V9","address":"0x11"}]}}"#),
            r#"{"ok": false, "error": "ValueError: unknown pool family 'V9' (expected V2|V3|V4)"}"#
        );
    }

    #[test]
    fn step_from_wire_rejects_a_missing_address() {
        let step = serde_json::json!({"family": "V2"});
        assert_eq!(
            step_from_wire(&step).unwrap_err(),
            "V2 step is missing an address"
        );
    }

    #[test]
    fn empty_patch_error_matches_python() {
        assert_eq!(
            req(r#"{"op":"set_fleet_posture","payload":{}}"#),
            r#"{"ok": false, "error": "set_fleet_posture needs at least one threshold key (empty patch)"}"#
        );
    }

    #[test]
    fn unknown_fleet_key_error_matches_python() {
        assert_eq!(
            req(r#"{"op":"set_fleet_posture","payload":{"nope":1}}"#),
            r#"{"ok": false, "error": "unknown fleet-posture threshold key(s): nope"}"#
        );
    }
}
