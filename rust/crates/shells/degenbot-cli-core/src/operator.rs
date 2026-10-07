//! The operator command-channel client: JSON-lines over a Unix domain socket
//! (ADR-051 D6;).
//!
//! `degenbot fleet posture [show|set]` and `degenbot path [add|discover]` are
//! clients of a LIVE bot's `OperatorServer`. The versioned wire protocol is
//! documented in `src/degenbot/operator/operator_channel.py`; that header +
//! the server's framing are read-only contract and this module must not drift
//! from them.
//!
//! # Wire shape (one JSON object per line, newline-terminated)
//!
//! Request lines nest the payload at the top level (the exact shape Python's
//! `send_command` writes and the server's `_decode_request` reads):
//!
//! ```text
//! {"op": "add_path", "payload": {"steps": [...], "directions": [true]}}
//! {"op": "discover", "payload": {"bound": 5}}
//! {"op": "set_fleet_posture", "payload": {"cordon_enter_events": 2}}
//! {"op": "get_fleet_posture", "payload": {}}
//! ```
//!
//! Response lines:
//!
//! ```text
//! {"ok": true, "detail": "..."}
//! {"ok": true, "detail": "", "effective": {...}}
//! {"ok": false, "error": "..."}
//! ```
//!
//! # Division of validation
//!
//! This client adds **wire hygiene only** - an unknown `cordon_*` key, an empty
//! posture patch, an unknown hop-family string - refused BEFORE the socket is
//! touched by [`validate_posture_patch`] / [`parse_hop_token`]. The six cordon
//! values are DOMAIN validation and stay the authority of the server
//! (`PosturePolicyPatch::validate` in the workers core); this crate deliberately
//! does not import or re-implement it, so a client built earlier never second-
//! guesses a host built later.
//!
//! # Socket resolution
//!
//! `--socket` > `DEGENBOT_OPERATOR_SOCKET` (the shell-environment layer, read
//! through the `degenbot-config` `EnvVars` seam) > `~/.config/degenbot/operator.sock`
//! (the example driver's plug-in default). No typed config-file key exists for
//! the operator socket, so the env layer is the only config surface; a future
//! typed key would slot in above the env layer at exactly one site.

#[cfg(unix)]
use std::future::Future;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use degenbot_config::EnvVars;
use serde_json::{json, Map, Value};
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::error::{CliError, UnknownVariant};

/// The environment variable naming the live bot's operator socket (the shell
/// layer of the cascade). Empty is treated exactly like unset.
pub const SOCKET_ENV: &str = "DEGENBOT_OPERATOR_SOCKET";

/// The plug-in default socket path (the settlement-bot example's
/// `--operator-socket` default family). The leading `~` expands against `HOME`.
pub const SOCKET_DEFAULT: &str = "~/.config/degenbot/operator.sock";

/// The `HOME` layer used to expand a leading `~` in the default.
const HOME_ENV: &str = "HOME";

/// Per-request connect/read timeout (mirrors the server's `request_timeout`
/// default of 60s).
#[cfg(unix)]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The refusal `send_request` returns on a host with no Unix domain socket.
/// The channel is a UDS protocol end to end (the Python `OperatorServer`
/// cannot bind one on Windows either), so off Unix the command arms stay
/// compiled and fail here, at the one transport site.
#[cfg(not(unix))]
const UDS_UNSUPPORTED: &str = "the operator command channel requires a Unix domain socket, \
     which this platform does not provide";

/// The six fleet-posture threshold key names `set_fleet_posture` accepts (the
/// typed `DEGENBOT_FLEET_CORDON_*` keys). Anything else is refused at the wire
/// before it reaches the host.
pub const FLEET_POSTURE_THRESHOLD_KEYS: [&str; 6] = [
    "cordon_enter_events",
    "cordon_enter_window_ms",
    "cordon_duty_percent",
    "cordon_duty_window_ms",
    "cordon_exit_clean_ms",
    "cordon_sim_intake_floor",
];

/// The documented sentinel for `--cordon-sim-intake-floor`: the literal
/// `null` (case-insensitive) restores half the slot cap; any other value is a
/// threshold. The host's `PosturePolicyPatch` carries this as the
/// `Some(None)` override.
pub const SIM_INTAKE_FLOOR_RESTORE: &str = "null";

/// A pool family on the operator wire (`V2` / `V3` / `V4`) — the canonical
/// pool-family taxonomy, re-exported under its wire name.
///
/// One taxonomy, one owner: the variant set, the discriminants, and the wire
/// tags live on [`degenbot_db::PoolKind`] (re-exported from the
/// `degenbot-pathfinding` graph vocabulary this crate's DB surface already
/// projects through). This shell adds only the operator channel's lenient
/// parse; a family added to the canonical enum compiles here without an edit
/// and inherits its tag, where the second enum this re-export replaced would
/// have drifted silently.
pub use degenbot_db::PoolKind as PathFamily;

/// Parse a case-insensitive operator-wire family token (`v2`/`V2`, ...
/// `v4`/`V4`) into the canonical [`PathFamily`].
///
/// The canonical enum's own `TryFrom<&str>` parses the *persisted* `kind`
/// strings (`uniswap_v2`, ...), so the wire spelling keeps its own entry
/// point rather than overloading that contract.
///
/// # Errors
///
/// [`UnknownVariant`] for a family outside the canonical taxonomy, naming the
/// raw input and the known wire tags: a hop naming no family on the wire is
/// refused before the socket is touched, never silently routed.
pub fn path_family_from_wire(raw: &str) -> Result<PathFamily, UnknownVariant> {
    let upper = raw.to_ascii_uppercase();
    PathFamily::ALL
        .iter()
        .find(|family| family.tag() == upper)
        .copied()
        .ok_or_else(|| UnknownVariant {
            raw: raw.to_owned(),
            known: PathFamily::ALL.iter().map(|family| family.tag()).collect(),
        })
}

/// One hop in an `add_path` `steps` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    /// The hop's pool family.
    pub family: PathFamily,
    /// The pool address (`0x` + 40 hex).
    pub address: String,
    /// The V4 pool id (`0x` + 64 hex); `None` for V2/V3 and for a V4 hop that
    /// omitted it.
    pub hash: Option<String>,
}

/// A `--direction` choice: one bit applied to every hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDirection {
    /// Zero-for-one: `true` for each hop.
    Zfo,
    /// One-for-zero: `false` for each hop.
    Ozf,
}

impl PathDirection {
    /// Every direction, in declaration order — the set an unrecognized argv
    /// spelling is judged against.
    pub const ALL: [Self; 2] = [Self::Zfo, Self::Ozf];

    /// The argv spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zfo => "zfo",
            Self::Ozf => "ozf",
        }
    }

    /// The per-hop wire bit.
    #[must_use]
    pub const fn is_zfo(self) -> bool {
        matches!(self, Self::Zfo)
    }
}

impl TryFrom<&str> for PathDirection {
    type Error = UnknownVariant;

    /// Parse the argv spelling (case-sensitive, mirroring click's Choice).
    ///
    /// # Errors
    ///
    /// [`UnknownVariant`] for a spelling outside the closed set, naming the
    /// raw input and the known spellings.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "zfo" => Ok(Self::Zfo),
            "ozf" => Ok(Self::Ozf),
            _ => Err(UnknownVariant {
                raw: raw.to_owned(),
                known: Self::ALL.iter().map(|d| d.as_str()).collect(),
            }),
        }
    }
}

/// Parse one `FAMILY:ADDRESS[:HASH]` hop token into a wire [`PathStep`]
/// (mirrors `_parse_hop`). Family is case-insensitive; the V4 hash is carried
/// only for V4 and only when non-empty.
///
/// # Errors
///
/// [`CliError::OperatorHygiene`] when the family is not V2/V3/V4 or the
/// address is absent/empty - refused before any socket work.
pub fn parse_hop_token(hop: &str) -> Result<PathStep, CliError> {
    let parts: Vec<&str> = hop.split(':').collect();
    let raw_family = parts.first().copied().unwrap_or_default();
    let family = path_family_from_wire(raw_family).map_err(|_| {
        CliError::OperatorHygiene(format!("--hop family must be V2|V3|V4, got {raw_family:?}"))
    })?;
    let address = parts
        .get(1)
        .copied()
        .filter(|address| !address.is_empty())
        .ok_or_else(|| CliError::OperatorHygiene(format!("--hop {hop:?} is missing an address")))?;
    let hash = if family == PathFamily::V4 {
        parts
            .get(2)
            .copied()
            .filter(|hash| !hash.is_empty())
            .map(ToString::to_string)
    } else {
        None
    };
    Ok(PathStep {
        family,
        address: address.to_string(),
        hash,
    })
}

/// A `set_fleet_posture` threshold value.
///
/// The wire carries integers for the five count/window keys and a float for
/// `cordon_duty_percent`; `Null` is the `cordon_sim_intake_floor` restore
/// sentinel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PosturePatchValue {
    /// An integer threshold.
    Int(u64),
    /// A float threshold (`cordon_duty_percent`).
    Float(f64),
    /// Restore half the slot cap (`cordon_sim_intake_floor` only).
    Null,
}

impl PosturePatchValue {
    /// The JSON value the wire carries. A non-finite float cannot cross JSON
    /// and degrades to `null` (the host then rejects it - domain validation
    /// stays server-side).
    #[must_use]
    pub fn json_value(self) -> Value {
        match self {
            Self::Int(value) => Value::from(value),
            Self::Float(value) => {
                serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
            }
            Self::Null => Value::Null,
        }
    }
}

/// One `cordon_*` entry in a partial posture patch.
#[derive(Debug, Clone, PartialEq)]
pub struct PosturePatchEntry {
    /// One of [`FLEET_POSTURE_THRESHOLD_KEYS`].
    pub key: String,
    /// The threshold value.
    pub value: PosturePatchValue,
}

impl PosturePatchEntry {
    /// Build an entry from a key + typed value.
    #[must_use]
    pub fn new(key: impl Into<String>, value: PosturePatchValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }

    /// Build an integer entry.
    #[must_use]
    pub fn int(key: impl Into<String>, value: u64) -> Self {
        Self::new(key, PosturePatchValue::Int(value))
    }

    /// Build a float entry.
    #[must_use]
    pub fn float(key: impl Into<String>, value: f64) -> Self {
        Self::new(key, PosturePatchValue::Float(value))
    }
}

/// Parse the `cordon_sim_intake_floor` flag text: the literal `null`
/// (case-insensitive) is the documented restore sentinel
/// ([`SIM_INTAKE_FLOOR_RESTORE`]), anything else must be an unsigned integer.
///
/// # Errors
///
/// [`CliError::OperatorHygiene`] when the text is neither the sentinel nor an
/// unsigned integer.
pub fn parse_sim_intake_floor(raw: &str) -> Result<PosturePatchValue, CliError> {
    if raw.eq_ignore_ascii_case(SIM_INTAKE_FLOOR_RESTORE) {
        return Ok(PosturePatchValue::Null);
    }
    raw.trim()
        .parse::<u64>()
        .map(PosturePatchValue::Int)
        .map_err(|_| {
            CliError::OperatorHygiene(format!(
                "cordon_sim_intake_floor must be an integer or {SIM_INTAKE_FLOOR_RESTORE:?} to                  restore half the slot cap, got {raw:?}"
            ))
        })
}

/// Client-side wire hygiene for a posture patch: no unknown `cordon_*` key and
/// no empty patch. Mirrors the host's `handle_fleet_posture_op` guard messages;
/// the host's typed `PosturePolicyPatch::validate` remains the value authority.
///
/// # Errors
///
/// [`CliError::OperatorHygiene`] with the refused key list or the empty-patch
/// message.
pub fn validate_posture_patch(patch: &[PosturePatchEntry]) -> Result<(), CliError> {
    let mut unknown: Vec<&str> = patch
        .iter()
        .map(|entry| entry.key.as_str())
        .filter(|key| !FLEET_POSTURE_THRESHOLD_KEYS.contains(key))
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    if !unknown.is_empty() {
        return Err(CliError::OperatorHygiene(format!(
            "unknown fleet-posture threshold key(s): {}",
            unknown.join(", ")
        )));
    }
    if patch.is_empty() {
        return Err(CliError::OperatorHygiene(
            "set_fleet_posture needs at least one threshold key (empty patch)".to_string(),
        ));
    }
    Ok(())
}

/// The four ops the live host serves, encoded as one request line.
#[derive(Debug, Clone, PartialEq)]
pub enum WireRequest {
    /// `add_path`: enqueue one specific path.
    AddPath {
        /// The hop steps, in path order.
        steps: Vec<PathStep>,
        /// Per-hop direction bits, or `None` to let the host auto-resolve.
        directions: Option<Vec<bool>>,
    },
    /// `discover`: one bounded on-demand discovery sweep.
    Discover {
        /// Maximum paths to process, or `None` for the host's default.
        bound: Option<u64>,
    },
    /// `set_fleet_posture`: a partial patch over the six `cordon_*` keys.
    SetFleetPosture {
        /// The patch entries (must already pass [`validate_posture_patch`]).
        patch: Vec<PosturePatchEntry>,
    },
    /// `get_fleet_posture`: read the live thresholds + posture.
    GetFleetPosture,
}

impl WireRequest {
    /// The wire op name.
    #[must_use]
    pub const fn op_name(&self) -> &'static str {
        match self {
            Self::AddPath { .. } => "add_path",
            Self::Discover { .. } => "discover",
            Self::SetFleetPosture { .. } => "set_fleet_posture",
            Self::GetFleetPosture => "get_fleet_posture",
        }
    }

    /// The request payload object.
    #[must_use]
    pub fn payload(&self) -> Value {
        match self {
            Self::AddPath { steps, directions } => {
                let steps: Vec<Value> = steps.iter().map(step_json).collect();
                let directions = directions.as_ref().map_or(Value::Null, |bits| json!(bits));
                json!({ "steps": steps, "directions": directions })
            }
            Self::Discover { bound } => json!({ "bound": bound }),
            Self::SetFleetPosture { patch } => {
                let mut payload = Map::new();
                for entry in patch {
                    payload.insert(entry.key.clone(), entry.value.json_value());
                }
                Value::Object(payload)
            }
            Self::GetFleetPosture => json!({}),
        }
    }

    /// Encode the one request line the server reads (JSON object + newline).
    #[must_use]
    pub fn encode_line(&self) -> String {
        let mut envelope = Map::new();
        envelope.insert("op".to_string(), Value::from(self.op_name()));
        envelope.insert("payload".to_string(), self.payload());
        let mut line =
            serde_json::to_string(&Value::Object(envelope)).unwrap_or_else(|_| "{}".to_string());
        line.push('\n');
        line
    }
}

/// One wire `steps` entry (only the V4 hash key is carried, and only when set).
fn step_json(step: &PathStep) -> Value {
    let mut object = Map::new();
    object.insert("family".to_string(), Value::from(step.family.tag()));
    object.insert("address".to_string(), Value::from(step.address.clone()));
    if let Some(hash) = &step.hash {
        object.insert("hash".to_string(), Value::from(hash.clone()));
    }
    Value::Object(object)
}

/// A decoded response frame.
#[derive(Debug, Clone, PartialEq)]
pub enum WireResponse {
    /// `{"ok": true, ...}`.
    Ok {
        /// The `detail` string (`""` when absent).
        detail: String,
        /// The `effective` policy object, when the op echoes one.
        effective: Option<Value>,
    },
    /// `{"ok": false, "error": "..."}` - including the host's unknown-op reply.
    Err {
        /// The `error` string.
        error: String,
    },
}

impl WireResponse {
    /// Whether the host accepted the command.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// The success detail (`""` for an error frame).
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Ok { detail, .. } => detail,
            Self::Err { .. } => "",
        }
    }

    /// The echoed effective policy, when present.
    #[must_use]
    pub fn effective(&self) -> Option<&Value> {
        match self {
            Self::Ok { effective, .. } => effective.as_ref(),
            Self::Err { .. } => None,
        }
    }

    /// The host's error string, when the frame was a refusal.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Err { error } => Some(error),
            Self::Ok { .. } => None,
        }
    }
}

/// Decode one response line. A malformed/non-object/missing-`ok` frame is a
/// protocol error; a well-formed `{"ok": false, ...}` frame (including the
/// host's unknown-op reply) is [`WireResponse::Err`], never a panic.
///
/// # Errors
///
/// [`CliError::OperatorProtocol`] for an empty line, invalid UTF-8/JSON, a
/// non-object frame, or an `ok` field that is missing/non-boolean.
pub fn decode_response(line: &str) -> Result<WireResponse, CliError> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Err(CliError::OperatorProtocol(
            "operator host sent an empty response line".to_string(),
        ));
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|err| {
        CliError::OperatorProtocol(format!("invalid operator response JSON: {err}"))
    })?;
    let Some(object) = value.as_object() else {
        return Err(CliError::OperatorProtocol(
            "operator response is not a JSON object".to_string(),
        ));
    };
    match object.get("ok") {
        Some(Value::Bool(true)) => Ok(WireResponse::Ok {
            detail: object
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            effective: object.get("effective").cloned(),
        }),
        Some(Value::Bool(false)) => Ok(WireResponse::Err {
            error: object
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("operator host reported failure without an error message")
                .to_string(),
        }),
        Some(_) => Err(CliError::OperatorProtocol(
            "operator response 'ok' is not a boolean".to_string(),
        )),
        None => Err(CliError::OperatorProtocol(
            "operator response is missing 'ok'".to_string(),
        )),
    }
}

/// Render an `effective` object as one compact JSON object with its top-level
/// keys sorted - the Python CLI's `json.dumps(effective, sort_keys=True)`.
/// `None` renders `{}` (the Python `response.get("effective", {})` default).
///
/// The effective policy is a flat object by protocol, so sorting the top level
/// is equivalent to Python's recursive `sort_keys`.
#[must_use]
pub fn render_json_sorted(value: Option<&Value>) -> String {
    match value {
        Some(Value::Object(map)) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let inner = entries
                .iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).unwrap_or_default();
                    let value = serde_json::to_string(value).unwrap_or_default();
                    format!("{key}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
        None => "{}".to_string(),
    }
}

/// Resolve the operator socket: `--socket` > `DEGENBOT_OPERATOR_SOCKET` >
/// `~/.config/degenbot/operator.sock`. The winning value has a leading `~`
/// expanded against `HOME` through the env seam.
#[must_use]
pub fn resolve_socket(env: &dyn EnvVars, cli_socket: Option<&str>) -> PathBuf {
    let env_value = env.get(SOCKET_ENV);
    let raw = match (non_empty(cli_socket), non_empty(env_value.as_deref())) {
        (Some(cli), _) => cli,
        (None, Some(value)) => value,
        (None, None) => SOCKET_DEFAULT,
    };
    expand_tilde(env, raw)
}

/// Treat an empty string exactly like an absent layer.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// Expand a leading `~` (or `~`/) against `HOME` from the env seam (mirrors
/// the `degenbot-config` resolver's expansion for the database-path default).
fn expand_tilde(env: &dyn EnvVars, raw: &str) -> PathBuf {
    let home = env.get(HOME_ENV).filter(|home| !home.is_empty());
    if raw == "~" {
        if let Some(home) = home {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Send one request to the operator host and decode its single response line.
///
/// The arms build a [`WireRequest`] and call this; the transport is a
/// short-lived `tokio` runtime over a Unix-domain socket (the arms are sync).
///
/// # Errors
///
/// [`CliError::OperatorProtocol`] when the socket is unreachable, the exchange
/// times out, or the response is malformed; [`CliError::RuntimeNested`] when
/// called from inside an existing `tokio` runtime.
pub fn send_request(socket: &Path, request: &WireRequest) -> Result<WireResponse, CliError> {
    let line = request.encode_line();
    exchange_blocking(socket, line)
}

/// Drive one encoded request line to its response line over the operator
/// socket.
#[cfg(unix)]
fn exchange_blocking(socket: &Path, line: String) -> Result<WireResponse, CliError> {
    block_on_operator(exchange(socket, line))?
}

/// Off Unix there is no Unix domain socket to reach: the same
/// [`CliError::OperatorProtocol`] the unreachable-socket arm raises reports it.
#[cfg(not(unix))]
fn exchange_blocking(socket: &Path, line: String) -> Result<WireResponse, CliError> {
    let _ = (socket, line);
    Err(CliError::OperatorProtocol(UDS_UNSUPPORTED.to_string()))
}

/// Drive the exchange future on a self-built runtime, reusing the console's one
/// runtime helper. Only `RuntimeNested` is reachable from
/// [`crate::block::block_on`]; any other mapping is a build failure and is
/// reported as the operator-protocol error it is.
#[cfg(unix)]
fn block_on_operator<F: Future>(future: F) -> Result<F::Output, CliError> {
    crate::block::block_on(future).map_err(|err| match err {
        CliError::RuntimeNested => err,
        other => CliError::OperatorProtocol(other.message()),
    })
}

/// One request line out, one response line in (the server closes after the
/// reply). Unix-only: the wire rides a Unix domain socket.
///
/// The production budget is the single [`REQUEST_TIMEOUT`]; the body takes it
/// as a parameter so tests can pin the deadline arithmetic with a short one.
#[cfg(unix)]
async fn exchange(socket: &Path, line: String) -> Result<WireResponse, CliError> {
    exchange_within(socket, line, REQUEST_TIMEOUT).await
}

/// The exchange body under ONE deadline shared by connect + write + flush +
/// read.
///
/// Every phase spends the *remaining* budget, never a fresh copy of it, so a
/// peer that stalls any single phase cannot stretch the whole exchange past
/// `request_timeout` (the old three-phase shape let a stalling peer spend the
/// bound three times over).
///
/// The read side is additionally **resumable**. `read_until` appends into the
/// CALLER-OWNED `buffer`, and a `timeout` that fires drops only the in-flight
/// future - never the bytes it already pushed into that buffer. So a timeout
/// need not abandon a partial line: the loop re-arms against the SAME buffer
/// under the SAME deadline, and the terminal timeout reports how many partial
/// bytes it gave up.
///
/// # Cancel safety
///
/// **Cancel-safe on the READ side only**: the `read_until` loop appends into
/// the CALLER-OWNED `buffer`, and a cancelled `timeout(.., read_until(..))`
/// future drops only the in-flight read — never the bytes it already pushed
/// into that buffer. So cancellation mid-read leaves the accumulated prefix
/// intact and a resumed loop re-arms against the same buffer under the same
/// deadline (this is exactly the resumability the phased-body doc documents).
/// The `connect`/`write_all`/`flush` arms are single `await`s that lose
/// nothing on drop; each surfaces a terminal `CliError::OperatorProtocol` on
/// timeout or I/O failure rather than a partial success.
///
/// **The unsafe remainder — the accumulated prefix is discarded with no
/// observer**: if the CALLER drops this future mid-exchange, the local
/// `buffer` is dropped with it. Any partial response bytes read so far vanish,
/// and because the drop is silent there is no telemetry, no error, and no
/// partial-line report — the caller just never sees a `WireResponse`. The
/// resumability above is resumability WITHIN one `exchange_within` call, not
/// across a cancellation: a dropped future does not hand its `buffer` back.
/// The caller's obligation is therefore to treat a cancelled exchange as a
/// total loss of the in-flight line (re-issue the request fresh), not as a
/// resumable partial — "resumable" describes the deadline lap, never the
/// cancelled future.
#[cfg(unix)]
async fn exchange_within(
    socket: &Path,
    line: String,
    request_timeout: Duration,
) -> Result<WireResponse, CliError> {
    let deadline = tokio::time::Instant::now() + request_timeout;

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let connect = tokio::time::timeout(remaining, UnixStream::connect(socket))
        .await
        .map_err(|_| {
            CliError::OperatorProtocol(format!(
                "timed out connecting to operator socket {}",
                socket.display()
            ))
        })?;
    let mut stream = connect.map_err(|err| {
        CliError::OperatorProtocol(format!(
            "cannot reach operator socket {}: {err}",
            socket.display()
        ))
    })?;

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    tokio::time::timeout(remaining, stream.write_all(line.as_bytes()))
        .await
        .map_err(|_| {
            CliError::OperatorProtocol(format!(
                "timed out writing to operator socket {}",
                socket.display()
            ))
        })?
        .map_err(|err| {
            CliError::OperatorProtocol(format!(
                "failed writing to operator socket {}: {err}",
                socket.display()
            ))
        })?;

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    tokio::time::timeout(remaining, stream.flush())
        .await
        .map_err(|_| {
            CliError::OperatorProtocol(format!(
                "timed out flushing operator socket {}",
                socket.display()
            ))
        })?
        .map_err(|err| {
            CliError::OperatorProtocol(format!(
                "failed flushing operator socket {}: {err}",
                socket.display()
            ))
        })?;

    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    // Resume against the one deadline: a dropped `read_until` future leaves the
    // bytes it already appended in `buffer` (the buffer is caller-owned), so a
    // slow-dribbling server keeps its accumulated prefix across laps rather
    // than losing everything to a single per-read timeout.
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(CliError::OperatorProtocol(format!(
                "timed out waiting for a response from operator socket {} (discarded {} partial bytes)",
                socket.display(),
                buffer.len()
            )));
        }
        match tokio::time::timeout(remaining, reader.read_until(b'\n', &mut buffer)).await {
            // A full line (or EOF-after-bytes, which the decode below rejects).
            Ok(Ok(read)) if read > 0 => break,
            Ok(Ok(_)) => {
                return Err(CliError::OperatorProtocol(format!(
                    "no response from operator server at {}",
                    socket.display()
                )));
            }
            Ok(Err(err)) => {
                return Err(CliError::OperatorProtocol(format!(
                    "failed reading from operator socket {}: {err}",
                    socket.display()
                )));
            }
            // Timed out mid-line: the partial prefix stays in `buffer`
            // (caller-owned), so the (empty) arm falls through to loop back
            // and resume the SAME buffer under the SAME deadline - the bytes
            // already received are never abandoned.
            Err(_) => {}
        }
    }
    let text = String::from_utf8(buffer).map_err(|err| {
        CliError::OperatorProtocol(format!("operator response is not valid UTF-8: {err}"))
    })?;
    decode_response(&text)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn path_family_wire_parse_is_case_insensitive() {
        assert_eq!(path_family_from_wire("v2"), Ok(PathFamily::V2));
        assert_eq!(path_family_from_wire("V3"), Ok(PathFamily::V3));
        assert_eq!(path_family_from_wire("v4"), Ok(PathFamily::V4));
    }

    #[test]
    fn path_family_wire_parse_unknown_names_the_raw_value_and_known_set() {
        let err = path_family_from_wire("v5").unwrap_err();
        assert_eq!(err.raw, "v5");
        let message = err.to_string();
        assert!(message.contains("v5"), "raw value missing from: {message}");
        assert!(
            message.contains("V2") && message.contains("V3") && message.contains("V4"),
            "known set missing from: {message}"
        );
    }

    #[test]
    fn path_direction_try_from_round_trips_the_argv_spellings() {
        for direction in PathDirection::ALL {
            assert_eq!(PathDirection::try_from(direction.as_str()), Ok(direction));
        }
    }

    #[test]
    fn path_direction_try_from_unknown_names_the_raw_value_and_known_set() {
        let err = PathDirection::try_from("zof").unwrap_err();
        assert_eq!(err.raw, "zof");
        let message = err.to_string();
        assert!(
            message.contains("zof") && message.contains("zfo") && message.contains("ozf"),
            "raw value and known set missing from: {message}"
        );
    }

    // -- the transport deadline -------------------------------------------------

    /// Bind `socket`, accept one connection, drain the request line, then write
    /// `partial` (no newline) and stall well past the client's budget. The line
    /// is never terminated, so a per-read timeout can only make progress by
    /// RETAINING the prefix it already buffered.
    #[cfg(unix)]
    fn dribble_once(socket: &Path, partial: &'static [u8]) {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let listener = UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            stream.write_all(partial).unwrap();
            stream.flush().unwrap();
            // Stall far longer than the client's short budget; the newline
            // never arrives.
            std::thread::sleep(Duration::from_secs(5));
        });
    }

    /// The whole exchange is bounded by ONE deadline, not a fresh full budget
    /// per phase. A server that dribbles a few bytes and then stalls must make
    /// the client return at roughly its small TOTAL budget - and the terminal
    /// timeout must report the partial bytes it gave up (the observability
    /// win that proves the prefix survived the dropped read future).
    #[cfg(unix)]
    #[test]
    fn a_dribbling_server_is_bounded_by_the_total_deadline() {
        use std::time::Instant as StdInstant;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("operator.sock");
        let partial = b"{\"ok\": true, \"det";
        dribble_once(&socket, partial);

        let request_timeout = Duration::from_millis(250);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = StdInstant::now();
        let err = runtime
            .block_on(exchange_within(
                &socket,
                "{\"op\": \"get_fleet_posture\", \"payload\": {}}\n".to_string(),
                request_timeout,
            ))
            .unwrap_err();
        let elapsed = started.elapsed();

        assert!(matches!(err, CliError::OperatorProtocol(_)), "got {err:?}");
        let message = err.message();
        assert!(
            message.contains("timed out waiting for a response from operator socket"),
            "kept prefix missing: {message}"
        );
        // The partial-byte suffix is additive; the retained prefix is nonempty
        // (and no larger than what the server actually sent).
        let count = message
            .split("discarded ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<usize>().ok());
        assert!(
            matches!(count, Some(retained) if retained > 0 && retained <= partial.len()),
            "prefix was not retained across the timeout (count {count:?}): {message}"
        );
        // ONE deadline: roughly the small budget, never a fresh per-phase
        // timeout (which here would be the full 60s read budget).
        assert!(
            elapsed >= request_timeout,
            "returned before the budget: {elapsed:?}"
        );
        assert!(
            elapsed < request_timeout * 4,
            "the exchange outran its single deadline: {elapsed:?}"
        );
    }

    /// The happy path under the same short-budget seam: a complete line is
    /// decoded, proving `exchange_within` did not regress the one-line read.
    #[cfg(unix)]
    #[test]
    fn exchange_within_reads_a_full_line_under_a_short_budget() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("operator.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            stream
                .write_all(b"{\"ok\": true, \"detail\": \"short budget ok\"}\n")
                .unwrap();
            stream.flush().unwrap();
            line
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(exchange_within(
                &socket,
                "{\"op\": \"get_fleet_posture\", \"payload\": {}}\n".to_string(),
                Duration::from_secs(5),
            ))
            .unwrap();
        assert_eq!(response.detail(), "short budget ok");
        let line = handle.join().unwrap();
        assert!(line.contains("get_fleet_posture"), "got {line:?}");
    }
}
