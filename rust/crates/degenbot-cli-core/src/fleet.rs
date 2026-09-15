//! The `fleet` command arms (ADR-051 D6;).
//!
//! Ports `cli/fleet.py`: both verbs are thin clients of a LIVE bot's
//! `OperatorServer` over the JSON-lines wire protocol in
//! [`crate::operator`]. The socket is resolved through the same cascade the
//! rest of the console uses (`--socket` > `DEGENBOT_OPERATOR_SOCKET` > the
//! `~/.config/degenbot/operator.sock` plugin default); no local config or
//! database work happens here.
//!
//! - `fleet posture show` sends `get_fleet_posture` and renders the echoed
//!   effective policy as one sorted JSON line.
//! - `fleet posture set` is a PARTIAL patch: it validates wire hygiene
//!   (unknown key / empty patch, in [`crate::operator::validate_posture_patch`])
//!   BEFORE the socket is touched, sends `set_fleet_posture`, and renders the
//!   echoed effective policy. Value validation stays server-side.

use crate::context::CliContext;
use crate::error::CliError;
use crate::operator::{
    render_json_sorted, resolve_socket, send_request, validate_posture_patch, PosturePatchEntry,
    WireRequest, WireResponse,
};
use crate::prompt::PromptPlan;
use crate::report::FleetReport;

/// The `fleet` command group.
#[derive(Debug, Clone, PartialEq)]
pub enum FleetCommand {
    /// `fleet posture show`: read the live cordon posture.
    PostureShow {
        /// The operator socket path override (`--socket`).
        socket: Option<String>,
    },
    /// `fleet posture set`: re-tune a subset of the six cordon thresholds.
    PostureSet {
        /// The operator socket path override (`--socket`).
        socket: Option<String>,
        /// The partial patch (at least one entry; known keys only).
        patch: Vec<PosturePatchEntry>,
    },
}

impl FleetCommand {
    /// Neither fleet arm prompts.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }
}

/// Execute a `fleet` command.
///
/// # Errors
///
/// [`CliError::OperatorHygiene`] for a client-side wire-hygiene refusal (an
/// empty or unknown-key patch, raised before any socket work);
/// [`CliError::OperatorRefused`] for a `{"ok": false}` host reply;
/// [`CliError::OperatorProtocol`] for an unreachable socket or a malformed
/// response.
pub(crate) fn execute(
    command: &FleetCommand,
    ctx: &CliContext<'_>,
) -> Result<FleetReport, CliError> {
    match command {
        FleetCommand::PostureShow { socket } => Ok(FleetReport::Posture {
            effective: call(ctx, socket.as_deref(), &WireRequest::GetFleetPosture)?,
        }),
        FleetCommand::PostureSet { socket, patch } => {
            validate_posture_patch(patch)?;
            let request = WireRequest::SetFleetPosture {
                patch: patch.clone(),
            };
            Ok(FleetReport::Posture {
                effective: call(ctx, socket.as_deref(), &request)?,
            })
        }
    }
}

/// Send one fleet request and render the echoed effective policy.
fn call(
    ctx: &CliContext<'_>,
    socket: Option<&str>,
    request: &WireRequest,
) -> Result<String, CliError> {
    let socket = resolve_socket(ctx.env(), socket);
    match send_request(&socket, request)? {
        WireResponse::Ok { effective, .. } => Ok(render_json_sorted(effective.as_ref())),
        WireResponse::Err { error } => Err(CliError::OperatorRefused(error)),
    }
}
