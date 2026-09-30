//! Recapture a path-investigation fixture's recorded *state* from the chain
//! at its `target_block` — every scalar and every initialized tick read
//! through an archive node, without consulting the tracker database. The
//! fixture's authored identity and narrative stay verbatim.
//!
//! Ticks come word-wise through a `TickLens` (V3-family) or directly from the
//! pool's bitmap + tick map (lens-free), and through the pool's StateView for
//! V4 — all batched over Multicall3 (`--scan lens|bitmap` selects the V3
//! vehicle; V4 always rides StateView).
//!
//! Config resolution follows ADR-062 exactly: the node URI comes from
//! `--node` / `DEGENBOT_RPC_*` / the `nodes.*` tables — nothing is hardcoded.
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --example capture_path_fixture -- [--check] [--scan lens|bitmap] \
//!         [--tick-lens <addr>] [--state-view <addr>] <fixture.json>...
//!
//! `--tick-lens` names the TickLens deployment to scan V3 word ranges through
//! (required for `--scan lens`); `--state-view` names the V4 pool StateView
//! (defaults to the canonical mainnet deployment the corpus recorded).
//! `--check` reports drifted state keys and exits 1 on any; the default mode
//! rewrites each fixture with canonical serde_json formatting.

// Run-once diagnostic example: stdout/stderr reports ARE its interface, its
// prose names CLI flags and deployments that pedantry would backtick, and the
// main loop reads clearer as one body than behind extraction helpers.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use alloy::primitives::{Address, B256};
use degenbot::config::loader::load_process_config;
use degenbot::config::resolvers::{resolve_chain_id, resolve_node_request_uri, NodeOverrides};
use degenbot::investigation::capture::{
    diff_state, emit_json, pool_spec, refresh_fixture, FetchedState,
};
use degenbot::investigation::chain_capture::{
    scrape_v2_state, scrape_v3_state, scrape_v4_state, TickScan,
};
use degenbot::rpc::provider::AlloyProvider;
use serde_json::Value;

/// The corpus's canonical V4 StateView deployment (the address both legacy
/// recorders read through), overridable with `--state-view`.
const DEFAULT_STATE_VIEW: &str = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227";

struct Args {
    check: bool,
    scan: ScanArg,
    tick_lens: Option<Address>,
    state_view: Address,
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanArg {
    Lens,
    Bitmap,
}

fn parse_addr(label: &str, raw: &str) -> Result<Address, String> {
    raw.parse()
        .map_err(|e| format!("{label} {raw:?} is not an address: {e}"))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        check: false,
        scan: ScanArg::Bitmap,
        tick_lens: None,
        state_view: parse_addr("--state-view", DEFAULT_STATE_VIEW)?,
        files: Vec::new(),
    };
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "--check" => args.check = true,
            "--scan" => {
                args.scan = match value("--scan")?.as_str() {
                    "lens" => ScanArg::Lens,
                    "bitmap" => ScanArg::Bitmap,
                    other => return Err(format!("--scan {other:?} (expected `lens` or `bitmap`)")),
                }
            }
            "--tick-lens" => {
                let raw = value("--tick-lens")?;
                args.tick_lens = Some(parse_addr("--tick-lens", &raw)?);
            }
            "--state-view" => {
                let raw = value("--state-view")?;
                args.state_view = parse_addr("--state-view", &raw)?;
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag:?}"));
            }
            _ => args.files.push(PathBuf::from(arg)),
        }
    }
    if args.files.is_empty() {
        return Err(
            "usage: capture_path_fixture [--check] [--scan lens|bitmap] \
             [--tick-lens <addr>] [--state-view <addr>] <fixture.json>..."
                .into(),
        );
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };
    let scan = match (args.scan, args.tick_lens) {
        (ScanArg::Lens, Some(lens)) => TickScan::Lens(lens),
        (ScanArg::Lens, None) => {
            eprintln!("error: --scan lens requires --tick-lens <address>");
            return ExitCode::from(2);
        }
        (ScanArg::Bitmap, Some(_)) => {
            eprintln!("error: --tick-lens requires --scan lens");
            return ExitCode::from(2);
        }
        (ScanArg::Bitmap, None) => TickScan::DirectPool,
    };
    let cfg = match load_process_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let chain_id = match resolve_chain_id(&cfg, None) {
        Ok(r) => r.value,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let node_uri = match resolve_node_request_uri(&cfg, chain_id, &NodeOverrides::new()) {
        Ok(r) => r.value,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let provider = match AlloyProvider::new(&node_uri, 3).await {
        Ok(provider) => provider,
        Err(e) => {
            eprintln!("error: node {node_uri:?}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for file in &args.files {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("error: {}: {e}", file.display());
                failed = true;
                continue;
            }
        };
        let doc: Value = match serde_json::from_str(&text) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("error: {}: {e}", file.display());
                failed = true;
                continue;
            }
        };
        let Some(target_block) = doc["target_block"].as_u64() else {
            eprintln!("error: {}: missing target_block", file.display());
            failed = true;
            continue;
        };
        let mut fetched: BTreeMap<String, FetchedState> = BTreeMap::new();
        let mut file_failed = false;
        for (key, entry) in doc["pools"].as_object().into_iter().flatten() {
            let spec = match pool_spec(entry) {
                Ok(spec) => spec,
                Err(e) => {
                    eprintln!("error: {}: pool `{key}`: {e}", file.display());
                    file_failed = true;
                    break;
                }
            };
            let spacing = entry["tick_spacing"].as_i64().unwrap_or(1) as i32;
            let state = if spec.family.ends_with("_v4") {
                scrape_v4_state(
                    &provider,
                    args.state_view,
                    spec.pool_id.unwrap_or(B256::ZERO),
                    spacing,
                    target_block,
                )
                .await
            } else if let Some(pool) = spec.address {
                if spec.family.ends_with("_v3") {
                    scrape_v3_state(&provider, pool, spacing, target_block, scan).await
                } else {
                    scrape_v2_state(&provider, pool, target_block).await
                }
            } else {
                Err(format!("pool `{key}`: no address/pool_id in fixture"))
            };
            match state {
                Ok(state) => {
                    fetched.insert(key.clone(), state);
                }
                Err(msg) => {
                    eprintln!("error: {}: {msg}", file.display());
                    file_failed = true;
                    break;
                }
            }
        }
        if file_failed {
            failed = true;
            continue;
        }
        let refreshed = match refresh_fixture(&doc, &fetched) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("error: {}: {e}", file.display());
                failed = true;
                continue;
            }
        };
        if args.check {
            let drift = diff_state(&doc, &refreshed);
            if drift.is_empty() {
                println!("ok    {}", file.display());
            } else {
                println!("DRIFT {}", file.display());
                for path in &drift {
                    // A chain snapshot stamps its capture block as the
                    // liquidity-update marker; corpus files recorded the live
                    // tracker's marker. Flagged as a known normalization, but
                    // still drift — the check stays honest.
                    let note = if path.ends_with("liquidity_update_block") {
                        "  (snapshot-marker normalization: chain snapshots are exact at target_block)"
                    } else {
                        ""
                    };
                    println!("  {path}{note}");
                }
                failed = true;
            }
        } else {
            match std::fs::write(file, emit_json(&refreshed)) {
                Ok(()) => println!("wrote {}", file.display()),
                Err(e) => {
                    eprintln!("error: {}: {e}", file.display());
                    failed = true;
                }
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
