//! Endpoint rate-limit policy: which endpoints count as LOCAL, and the throttle
//! a transport built for a remote one gets.
//!
//! Hosted free tiers enforce a per-IP compute budget, so a remote endpoint is
//! throttled to the config-declared `nodes.rate_limit_rps` / `rate_limit_burst`
//! while a local node — which has no upstream budget to protect — is exempt.
//! Both halves of that rule live here, beside the hostname list they read.

use std::num::NonZeroU32;

use alloy::transports::http::reqwest::Url;

/// The hostnames that make an endpoint LOCAL — the exemption list
/// `nodes.rate_limit_rps` documents. A local node has no per-IP upstream
/// budget to protect.
const LOCAL_RPC_HOSTS: [&str; 5] = [
    "localhost",
    "127.0.0.1",
    "::1",
    "host.containers.internal",
    "host.docker.internal",
];

/// A local endpoint has no per-IP budget to protect, and throttling it
/// would only slow the local lanes — the configured throttle skips it.
///
/// The classification compares the URL's parsed host against
/// [`LOCAL_RPC_HOSTS`], so a remote endpoint that merely *contains* a local
/// marker (`http://localhost.example.com`, a `127.0.0.1` in the path or query)
/// is throttled like the remote endpoint it is, while a local one is recognized
/// whatever its userinfo or hostname case. A URL that does not parse is not
/// local: no transport is ever built from one, so no throttle could apply to
/// it.
///
/// This is the predicate's ONE home. The Python shell reaches the same function
/// through `degenbot._ffi.provider.is_local_rpc_url` when it derives a fork
/// target's default upstream cap, so the throttle lane and the fork lane cannot
/// disagree about which hostnames a local node owns.
#[must_use]
pub fn is_local_rpc_url(rpc_url: &str) -> bool {
    let Ok(url) = Url::parse(rpc_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // `host_str` renders an IPv6 literal between brackets (`[::1]`).
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    LOCAL_RPC_HOSTS
        .iter()
        .any(|local| host.eq_ignore_ascii_case(local))
}

/// Resolve the config-declared throttle (`nodes.rate_limit_rps` /
/// `nodes.rate_limit_burst`) for `rpc_url`, if any.
pub(crate) fn configured_rate_limit(rpc_url: &str) -> Option<(u32, NonZeroU32)> {
    if is_local_rpc_url(rpc_url) {
        return None;
    }
    let nodes = &::degenbot_config::holder::config().nodes;
    let rps = NonZeroU32::new(u32::try_from(nodes.rate_limit_rps?).ok()?)?;
    let burst = nodes
        .rate_limit_burst
        .and_then(|v| u32::try_from(v).ok())
        .and_then(NonZeroU32::new)
        .unwrap_or(rps);
    Some((rps.get(), burst))
}
