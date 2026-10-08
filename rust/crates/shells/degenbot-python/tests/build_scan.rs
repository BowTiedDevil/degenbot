// Regression guard for the build receipt: a dependency-only source edit must
// (a) appear in the `cargo:rerun-if-changed` set cargo watches and (b) move the
// fingerprint the receipt bakes in. Before this, `build.rs` hashed sibling
// crates but emitted no directives, so cargo never re-ran it on a dep-only edit
// and the stale receipt blessed an old cdylib as fresh.

#![expect(clippy::unwrap_used, clippy::expect_used)]

include!("../build_scan.rs");
include!("../build_counter.rs");

/// Build a throwaway workspace with the binding in `shells/` and a dependency
/// in `engine/`, mirroring the role-grouped layout `scan_workspace` expects.
fn workspace(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("degenbot-build-scan-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let app = root.join("crates/shells/app");
    let dep = root.join("crates/engine/dep");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::create_dir_all(dep.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/shells/app\", \"crates/engine/dep\"]\n",
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
    scan_workspace(&root.join("crates/shells/app")).expect("workspace layout should scan")
}

#[test]
fn nested_binding_discovers_workspace_and_repository_roots() {
    let root = workspace("roots");
    let binding = root.join("crates/shells/app");

    assert_eq!(workspace_root(&binding).as_deref(), Some(root.as_path()));
    assert_eq!(repo_root(&binding).as_deref(), root.parent());
}

#[test]
fn dep_only_edit_moves_fingerprint_and_is_a_rerun_trigger() {
    let root = workspace("dep-edit");
    let dep_file = root.join("crates/engine/dep/src/lib.rs");
    let dep_src = root.join("crates/engine/dep/src");

    let before = scan_app(&root);
    assert!(
        before.files.iter().any(|f| f.path == dep_file),
        "the dependency source file must be folded into the fingerprint"
    );
    assert!(
        before.dirs.contains(&dep_src),
        "the dependency src tree must be watched so added files are caught"
    );
    assert!(
        before
            .files
            .iter()
            .any(|file| file.path == dep_file && file.tag == b"crates/engine/dep/src/lib.rs"),
        "the dependency identity tag must preserve its role-grouped path"
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
    let scanner = root.join("crates/shells/app/build_scan.rs");

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
    let sibling_build = root.join("crates/engine/dep/build.rs");
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
fn same_basename_files_each_move_fingerprint_independently() {
    let root = workspace("basename-collision");
    let nested_a = root.join("crates/engine/dep/src/nested/mod.rs");
    let nested_b = root.join("crates/engine/dep/src/deeper/mod.rs");
    fs::create_dir_all(nested_a.parent().unwrap()).unwrap();
    fs::create_dir_all(nested_b.parent().unwrap()).unwrap();
    fs::write(&nested_a, "pub fn nested_a() {}\n").unwrap();
    fs::write(&nested_b, "pub fn nested_b() {}\n").unwrap();

    let before = scan_app(&root);
    let tag_of = |scan: &WorkspaceScan, path: &Path| {
        scan.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.tag.clone())
    };

    // Both same-basename files must survive the scan as DISTINCT entries,
    // each tagged by its full tree-relative path - a basename key would
    // shadow one of them out of the fingerprint entirely.
    assert_eq!(
        tag_of(&before, &nested_a).as_deref(),
        Some(b"crates/engine/dep/src/nested/mod.rs".as_slice()),
        "the first nested file must be scanned under its path, not its basename"
    );
    assert_eq!(
        tag_of(&before, &nested_b).as_deref(),
        Some(b"crates/engine/dep/src/deeper/mod.rs".as_slice()),
        "the second nested file must be scanned under its path, not its basename"
    );

    // Each file must move the emitted fingerprint on its own edit.
    fs::write(&nested_a, "pub fn nested_a() { let _ = 1; }\n").unwrap();
    let after_a = scan_app(&root);
    assert_ne!(
        before.fingerprint, after_a.fingerprint,
        "an edit to the first same-basename file must move the fingerprint"
    );

    fs::write(&nested_a, "pub fn nested_a() {}\n").unwrap();
    fs::write(&nested_b, "pub fn nested_b() { let _ = 2; }\n").unwrap();
    let after_b = scan_app(&root);
    assert_ne!(
        before.fingerprint, after_b.fingerprint,
        "an edit to the second same-basename file must move the fingerprint"
    );
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

#[test]
fn fingerprint_is_independent_of_the_scanning_crate() {
    // The receipt writer and the console build script scan one shared tree
    // from different crate roots; the fingerprint must be a function of the
    // file set alone, or the freshness gate compares two unlike hashes
    // forever and warns on every invocation of an actually-fresh console.
    let root = workspace("caller-order");
    let console = root.join("crates/shells/console");
    fs::create_dir_all(console.join("src")).unwrap();
    fs::write(
        console.join("Cargo.toml"),
        "[package]\nname = \"console\"\n",
    )
    .unwrap();
    fs::write(console.join("build.rs"), "fn main() {}\n").unwrap();
    fs::write(console.join("src/lib.rs"), "pub fn console() {}\n").unwrap();

    let from_app = scan_workspace(&root.join("crates/shells/app")).expect("app scan");
    let from_console = scan_workspace(&console).expect("console scan");
    assert_eq!(
        from_app.fingerprint, from_console.fingerprint,
        "one tree must fingerprint identically from either crate root"
    );
    assert_eq!(
        from_app.files.len(),
        from_console.files.len(),
        "the two scans must still see the same input set"
    );
}

#[test]
fn receipt_writer_and_console_scans_agree_in_this_repository() {
    // Live-tree pin: the receipt writer (degenbot-python) and the console
    // recomputation (degenbot-cli) must derive ONE fingerprint for the
    // checkout both build scripts share.
    let binding = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shells = binding
        .parent()
        .expect("the binding crate sits under shells/");
    let writer = scan_workspace(&binding).expect("degenbot-python scan");
    let console = scan_workspace(&shells.join("degenbot-cli")).expect("degenbot-cli scan");
    assert_eq!(
        writer.fingerprint, console.fingerprint,
        "the receipt writer and the console must compute one fingerprint for this tree"
    );
}

#[test]
fn counter_advances_only_on_a_fingerprint_change() {
    // A fresh checkout starts the sequence at 1.
    assert_eq!(advance_count(None, Some(0x11)), 1);
    // No content change re-emits the stored count: no-change rebuilds
    // (test/clippy/feature variants) must never mark an installed wheel
    // stale.
    assert_eq!(advance_count(Some((2120, Some(0x11))), Some(0x11)), 2120);
    // A moved fingerprint advances exactly one step.
    assert_eq!(advance_count(Some((2120, Some(0x11))), Some(0x22)), 2121);
}

#[test]
fn counter_advances_when_freshness_cannot_be_proven() {
    // A legacy receipt with no stored fingerprint cannot prove byte-identity.
    assert_eq!(advance_count(Some((2120, None)), Some(0x11)), 2121);
    // Neither can a failed scan.
    assert_eq!(advance_count(Some((2120, Some(0x11))), None), 2121);
    // The counter saturates rather than wrapping back over itself.
    assert_eq!(advance_count(Some((u64::MAX, Some(0))), Some(1)), u64::MAX);
}
