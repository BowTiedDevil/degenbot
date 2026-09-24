// Regression guard for the build receipt: a dependency-only source edit must
// (a) appear in the `cargo:rerun-if-changed` set cargo watches and (b) move the
// fingerprint the receipt bakes in. Before this, `build.rs` hashed sibling
// crates but emitted no directives, so cargo never re-ran it on a dep-only edit
// and the stale receipt blessed an old cdylib as fresh.

#![expect(clippy::unwrap_used, clippy::expect_used)]

include!("../build_scan.rs");

/// Build a throwaway workspace (`<root>/crates/{app,dep}`) mirroring the real
/// layout `scan_workspace` expects.
fn workspace(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("degenbot-build-scan-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let app = root.join("crates/app");
    let dep = root.join("crates/dep");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/app\", \"crates/dep\"]\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
    fs::write(app.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
    fs::write(app.join("build.rs"), "fn main() {}\n").unwrap();
    fs::write(app.join("build_scan.rs"), "fn scanner() {}\n").unwrap();
    fs::write(app.join("src/lib.rs"), "pub fn app() {}\n").unwrap();
    fs::write(dep.join("Cargo.toml"), "[package]\nname = \"dep\"\n").unwrap();
    fs::write(dep.join("src/lib.rs"), "pub fn dep() {}\n").unwrap();
    root
}

fn scan_app(root: &Path) -> WorkspaceScan {
    scan_workspace(&root.join("crates/app")).expect("workspace layout should scan")
}

#[test]
fn dep_only_edit_moves_fingerprint_and_is_a_rerun_trigger() {
    let root = workspace("dep-edit");
    let dep_file = root.join("crates/dep/src/lib.rs");
    let dep_src = root.join("crates/dep/src");

    let before = scan_app(&root);
    assert!(
        before.files.iter().any(|f| f.path == dep_file),
        "the dependency source file must be folded into the fingerprint"
    );
    assert!(
        before.dirs.contains(&dep_src),
        "the dependency src tree must be watched so added files are caught"
    );

    fs::write(&dep_file, "pub fn dep() { let _ = 1; }\n").unwrap();
    let after = scan_app(&root);

    assert_ne!(
        before.fingerprint, after.fingerprint,
        "a dependency-only source edit must move the fingerprint"
    );
    assert!(
        after.files.iter().any(|f| f.path == dep_file),
        "the edited dependency file must be a rerun trigger"
    );
}

#[test]
fn scanner_self_edit_moves_fingerprint_and_is_a_rerun_trigger() {
    let root = workspace("scanner-edit");
    let scanner = root.join("crates/app/build_scan.rs");

    let before = scan_app(&root);
    assert!(
        before.files.iter().any(|f| f.path == scanner),
        "the included scanner source must be folded into the fingerprint"
    );

    fs::write(&scanner, "fn scanner() { let _ = 1; }\n").unwrap();
    let after = scan_app(&root);

    assert_ne!(
        before.fingerprint, after.fingerprint,
        "a scanner self-edit must move the fingerprint"
    );
    assert!(
        after.files.iter().any(|f| f.path == scanner),
        "the scanner source must be a rerun trigger"
    );
}

#[test]
fn build_inputs_move_fingerprint_and_are_rerun_triggers() {
    let root = workspace("build-inputs");
    let cargo_config_dir = root.join(".cargo");
    fs::create_dir_all(&cargo_config_dir).unwrap();
    let cargo_config = cargo_config_dir.join("config.toml");
    fs::write(&cargo_config, "[build]\nrustflags = []\n").unwrap();
    let sibling_build = root.join("crates/dep/build.rs");
    fs::write(&sibling_build, "fn main() {}\n").unwrap();

    let before = scan_app(&root);
    assert!(before.files.iter().any(|f| f.path == cargo_config));
    assert!(before.files.iter().any(|f| f.path == sibling_build));
    assert!(before.dirs.contains(&cargo_config_dir));

    fs::write(
        &cargo_config,
        "[build]\nrustflags = [\"--cfg\", \"test_cfg\"]\n",
    )
    .unwrap();
    fs::write(&sibling_build, "fn main() { println!(\"rerun\"); }\n").unwrap();
    let after = scan_app(&root);

    assert_ne!(
        before.fingerprint, after.fingerprint,
        "Cargo config and sibling build scripts must move the fingerprint"
    );
    assert!(after.files.iter().any(|f| f.path == cargo_config));
    assert!(after.files.iter().any(|f| f.path == sibling_build));
}

#[test]
fn unchanged_workspace_is_stable() {
    let root = workspace("stable");
    let first = scan_app(&root).fingerprint;
    let second = scan_app(&root).fingerprint;
    assert_eq!(first, second, "no-change rescans must be byte-stable");
}

#[test]
fn fold_chains_tag_and_content() {
    let one = fold(&[ScannedFile {
        path: PathBuf::from("x"),
        tag: b"tag".to_vec(),
        content: b"one".to_vec(),
    }]);
    let two = fold(&[ScannedFile {
        path: PathBuf::from("x"),
        tag: b"tag".to_vec(),
        content: b"two".to_vec(),
    }]);
    assert_ne!(one, two, "different content must move the fingerprint");
    assert_ne!(fnv1a(b"a", 0), fnv1a(b"b", 0));
}
