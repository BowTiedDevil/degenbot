//! The `path` command arms (ADR-051 D6;).
//!
//! Ports `cli/path.py`: both verbs are thin clients of a LIVE bot's
//! `OperatorServer` over the JSON-lines wire protocol in
//! [`crate::operator`]. The socket is resolved through the same cascade the
//! rest of the console uses (`--socket` > `DEGENBOT_OPERATOR_SOCKET` > the
//! `~/.config/degenbot/operator.sock` plugin default); no local pool or
//! database work happens here.
//!
//! - `path add` parses each `FAMILY:ADDRESS[:HASH]` hop
//!   ([`crate::operator::parse_hop_token`]) and sends `add_path` with an
//!   optional direction bit applied to every hop.
//! - `path discover` sends `discover` with an optional bound.

use crate::context::CliContext;
use crate::error::CliError;
use crate::operator::{
    parse_hop_token, resolve_socket, send_request, PathDirection, WireRequest, WireResponse,
};
use crate::prompt::PromptPlan;
use crate::report::PathReport;

/// The `path` command group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathCommand {
    /// `path add`: enqueue one specific path into the live registration
    /// pipeline.
    Add {
        /// The operator socket path override (`--socket`).
        socket: Option<String>,
        /// The raw `FAMILY:ADDRESS[:HASH]` hop tokens, in path order.
        hops: Vec<String>,
        /// The direction bit applied to every hop, or `None` to auto-resolve.
        direction: Option<PathDirection>,
    },
    /// `path discover`: one bounded on-demand discovery sweep.
    Discover {
        /// The operator socket path override (`--socket`).
        socket: Option<String>,
        /// The maximum number of paths to process, or `None` for the default.
        bound: Option<u64>,
    },
}

impl PathCommand {
    /// Neither path arm prompts.
    #[must_use]
    pub const fn prompt_plan(&self, _ctx: &CliContext<'_>) -> PromptPlan {
        PromptPlan::None
    }
}

/// Execute a `path` command.
///
/// # Errors
///
/// [`CliError::OperatorHygiene`] for a malformed/unknown hop-family token or
/// an empty hop list (raised before any socket work);
/// [`CliError::OperatorRefused`] for a `{"ok": false}` host reply;
/// [`CliError::OperatorProtocol`] for an unreachable socket or a malformed
/// response.
pub(crate) fn execute(command: &PathCommand, ctx: &CliContext<'_>) -> Result<PathReport, CliError> {
    match command {
        PathCommand::Add {
            socket,
            hops,
            direction,
        } => {
            if hops.is_empty() {
                return Err(CliError::OperatorHygiene(
                    "--hop is required: an add_path needs at least one hop".to_string(),
                ));
            }
            let steps = hops
                .iter()
                .map(String::as_str)
                .map(parse_hop_token)
                .collect::<Result<Vec<_>, _>>()?;
            let directions = direction
                .as_ref()
                .map(|direction| vec![direction.is_zfo(); steps.len()]);
            let request = WireRequest::AddPath { steps, directions };
            match call(ctx, socket.as_deref(), &request)? {
                WireResponse::Ok { detail, .. } => Ok(PathReport::Added { detail }),
                WireResponse::Err { error } => Err(CliError::OperatorRefused(error)),
            }
        }
        PathCommand::Discover { socket, bound } => {
            let request = WireRequest::Discover { bound: *bound };
            match call(ctx, socket.as_deref(), &request)? {
                WireResponse::Ok { detail, .. } => Ok(PathReport::Discovered { detail }),
                WireResponse::Err { error } => Err(CliError::OperatorRefused(error)),
            }
        }
    }
}

/// Send one path request and return the host's ok frame (or its refusal).
fn call(
    ctx: &CliContext<'_>,
    socket: Option<&str>,
    request: &WireRequest,
) -> Result<WireResponse, CliError> {
    let socket = resolve_socket(ctx.env(), socket);
    send_request(&socket, request)
}
