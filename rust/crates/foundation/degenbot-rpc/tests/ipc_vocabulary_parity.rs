//! Parity: the `nodes.ipc` values the config validator admits are a subset
//! of the forms the transport classifies as IPC. A form the validator admits
//! but `is_ipc_path` rejects falls through to the URL parser and silently
//! routes to a nonexistent IPC file, so the two predicates must not drift.

use std::collections::BTreeMap;

use degenbot_config::BotConfig;

/// Whether `BotConfig::validate` accepts `value` as a `nodes.ipc` entry.
fn config_accepts_ipc(value: &str) -> bool {
    let mut config = BotConfig::default();
    config.nodes.ipc = Some(BTreeMap::from([(String::from("1"), value.to_string())]));
    config.validate().is_ok()
}

#[test]
fn config_ipc_vocabulary_is_a_subset_of_the_transport_predicate() {
    for value in [
        "ipc:///tmp/anvil.ipc",
        "/tmp/anvil.ipc",
        "\\\\.\\pipe\\geth.ipc",
        "~/node.ipc",
        "./node.ipc",
        "../node.ipc",
        "C:\\node.ipc",
        "C:/node.ipc",
        "http://127.0.0.1:8545",
        "localhost:8545",
    ] {
        if config_accepts_ipc(value) {
            assert!(
                degenbot_rpc::provider::is_ipc_path(value),
                "the config validator admits {value:?} for nodes.ipc, but the transport does not classify it IPC; every accepted form must be one the transport dials"
            );
        }
    }
}

#[test]
fn the_config_validator_admits_the_transport_forms_and_refuses_the_rest() {
    for value in [
        "ipc:///tmp/anvil.ipc",
        "/tmp/anvil.ipc",
        "\\\\.\\pipe\\geth.ipc",
    ] {
        assert!(
            config_accepts_ipc(value),
            "the validator must admit the transport form {value:?}"
        );
    }
    for value in [
        "~/node.ipc",
        "./node.ipc",
        "../node.ipc",
        "C:\\node.ipc",
        "C:/node.ipc",
    ] {
        assert!(
            !config_accepts_ipc(value),
            "the validator must refuse {value:?}: a relative or `~` path resolves against the process working directory, and a drive path is no named pipe"
        );
    }
}
