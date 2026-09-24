//! XDG Base Directory acceptance for the config-file discovery seam:
//! `$DEGENBOT_CONFIG` > `$XDG_CONFIG_HOME/degenbot/config.toml` (absolute
//! only) > `$HOME/.config/degenbot/config.toml`, with an absent user file
//! contractually meaning "defaults".
//!
//! The seam reads XDG/HOME through [`MapEnv`] and the filesystem through temp
//! dirs, so no test mutates the process environment or the real home.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use degenbot_config::{standard_file_path_with, MapEnv};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn map_env(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "degenbot-config-xdg-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn seed(path: &PathBuf) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("seed parent");
    }
    std::fs::write(path, b"[telemetry]\notel = false\n").expect("seed file");
}

#[test]
fn xdg_config_home_absolute_selects_its_own_degenbot_file() {
    let home = scratch("home");
    let xdg = scratch("xdg");
    let expected = xdg.join("degenbot").join("config.toml");
    seed(&expected);

    let path = standard_file_path_with(&map_env(&[
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
    ]));
    assert_eq!(path, Some(expected), "absolute XDG_CONFIG_HOME wins");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&xdg);
}

#[test]
fn xdg_config_home_unset_falls_back_to_home_config() {
    let home = scratch("home-fallback");
    let expected = home.join(".config").join("degenbot").join("config.toml");
    seed(&expected);

    let path = standard_file_path_with(&map_env(&[("HOME", home.to_str().unwrap())]));
    assert_eq!(path, Some(expected), "unset XDG -> $HOME/.config");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn empty_or_relative_xdg_config_home_is_ignored() {
    let home = scratch("home-ignored");
    let expected = home.join(".config").join("degenbot").join("config.toml");
    seed(&expected);

    for xdg in ["", "relative/config"] {
        let path = standard_file_path_with(&map_env(&[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", xdg),
        ]));
        assert_eq!(
            path,
            Some(expected.clone()),
            "XDG_CONFIG_HOME={xdg:?} is not absolute and must be ignored"
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn degenbot_config_env_still_outranks_xdg() {
    let home = scratch("home-explicit");
    let xdg = scratch("xdg-explicit");
    let expected = xdg.join("degenbot").join("config.toml");
    seed(&expected);
    let explicit = home.join("explicit.toml");

    let path = standard_file_path_with(&map_env(&[
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("DEGENBOT_CONFIG", explicit.to_str().unwrap()),
    ]));
    assert_eq!(path, Some(explicit), "DEGENBOT_CONFIG is the highest layer");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&xdg);
}

#[test]
fn absent_file_is_none_not_an_error() {
    let home = scratch("home-absent");
    let path = standard_file_path_with(&map_env(&[("HOME", home.to_str().unwrap())]));
    assert_eq!(path, None, "an absent standard file means defaults");

    let _ = std::fs::remove_dir_all(&home);
}
