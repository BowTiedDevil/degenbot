// Shared scan/fingerprint logic for the `degenbot_rs` build receipt.
//
// Included verbatim by `build.rs` (to compute the identity and the
// `cargo:rerun-if-changed` triggers of everything that can link into the
// shipped cdylib) and by `tests/build_scan.rs` (to pin the behavior that a
// dependency-only source edit both moves the fingerprint and appears in the
// rerun-trigger set). std-only, and free of inner attributes: `include!`
// splices these items into the build script's crate root, where `//!` is
// illegal.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// FNV-1a 64-bit offset basis (std-only; no hash dependency in a build script).
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit: chain `seed` with `data`.
#[must_use]
pub fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// One observed file: the path (for `cargo:rerun-if-changed`), the
/// workspace-relative identity tag, and the content folded into the hash.
pub struct ScannedFile {
    pub path: PathBuf,
    pub tag: Vec<u8>,
    pub content: Vec<u8>,
}

/// The scanned workspace: every file that defines the cdylib's behavior, the
/// directories to watch for added/removed files, and the folded fingerprint.
pub struct WorkspaceScan {
    pub files: Vec<ScannedFile>,
    pub dirs: Vec<PathBuf>,
    pub fingerprint: u64,
}

/// Recursively collect `dir`'s files, keyed by path relative to `dir`.
pub fn collect_source_files(dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, out);
        } else if let (Some(rel), Ok(content)) = (
            path.strip_prefix(dir).ok().and_then(|p| p.to_str()),
            fs::read(&path),
        ) {
            out.insert(rel.to_owned(), content);
        }
    }
}

/// Read a single file when it exists.
fn read_file(path: &Path, tag: &str) -> Option<ScannedFile> {
    fs::read(path).ok().map(|content| ScannedFile {
        path: path.to_path_buf(),
        tag: tag.as_bytes().to_vec(),
        content,
    })
}

/// Read every file under `dir`, tagging each by `tag_prefix` plus its path
/// relative to `dir`. The directory itself is recorded so cargo can also catch
/// files added or removed from the tree.
fn read_tree(dir: &Path, tag_prefix: &str, dirs: &mut Vec<PathBuf>) -> Vec<ScannedFile> {
    if !dir.is_dir() {
        return Vec::new();
    }
    dirs.push(dir.to_path_buf());
    let mut tree = BTreeMap::new();
    collect_source_files(dir, &mut tree);
    tree.into_iter()
        .map(|(rel, content)| ScannedFile {
            path: dir.join(&rel),
            tag: format!("{tag_prefix}{rel}").into_bytes(),
            content,
        })
        .collect()
}

/// `.cargo` config directories that can change the compile (e.g. the repo-root
/// `--cfg tokio_unstable` rustflag), paired with a distinguishing tag prefix.
fn cargo_config_dirs(workspace_root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let local = workspace_root.join(".cargo");
    if local.is_dir() {
        out.push((local, String::from(".cargo/")));
    }
    if let Some(parent) = workspace_root.parent() {
        let root = parent.join(".cargo");
        if root.is_dir() {
            out.push((root, String::from("../.cargo/")));
        }
    }
    out
}

/// Fold every scanned file (tag then content) into a fresh fingerprint.
#[must_use]
pub fn fold(files: &[ScannedFile]) -> u64 {
    let mut hash = FNV_OFFSET;
    for file in files {
        hash = fnv1a(&file.tag, hash);
        hash = fnv1a(&file.content, hash);
    }
    hash
}

/// Scan `crate_dir`, its sibling workspace crates under `rust/crates`, the
/// workspace manifests, and any `.cargo` config, folding each file's identity
/// and content into the fingerprint. Returns `None` when the directory layout
/// is not the expected workspace.
#[must_use]
pub fn scan_workspace(crate_dir: &Path) -> Option<WorkspaceScan> {
    let own_name = crate_dir.file_name()?.to_str()?;
    let workspace_crates_dir = crate_dir.parent()?;
    let workspace_root = workspace_crates_dir.parent()?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();

    // This crate: build.rs + manifest + src tree.
    if let Some(file) = read_file(
        &crate_dir.join("build.rs"),
        &format!("crates/{own_name}/build.rs"),
    ) {
        files.push(file);
    }
    if let Some(file) = read_file(
        &crate_dir.join("Cargo.toml"),
        &format!("crates/{own_name}/Cargo.toml"),
    ) {
        files.push(file);
    }
    files.extend(read_tree(
        &crate_dir.join("src"),
        &format!("crates/{own_name}/src/"),
        &mut dirs,
    ));

    // Workspace-level manifests.
    for extra in ["Cargo.toml", "Cargo.lock"] {
        if let Some(file) = read_file(&workspace_root.join(extra), extra) {
            files.push(file);
        }
    }

    // Every sibling member crate under `rust/crates`, deterministic order. The
    // linked dependency closure lives entirely here; over-including a crate
    // that is not linked only costs a spurious rebuild, never a false-fresh.
    let mut crate_dirs: Vec<PathBuf> = fs::read_dir(workspace_crates_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    crate_dirs.sort();
    for dir in crate_dirs {
        if dir == *crate_dir {
            continue;
        }
        let name = dir.file_name()?.to_str()?;
        if let Some(file) = read_file(
            &dir.join("Cargo.toml"),
            &format!("crates/{name}/Cargo.toml"),
        ) {
            files.push(file);
        }
        if let Some(file) = read_file(&dir.join("build.rs"), &format!("crates/{name}/build.rs")) {
            files.push(file);
        }
        files.extend(read_tree(
            &dir.join("src"),
            &format!("crates/{name}/src/"),
            &mut dirs,
        ));
    }

    // Cargo config that can change rustflags/cfg.
    for (cfg_dir, prefix) in cargo_config_dirs(workspace_root) {
        files.extend(read_tree(&cfg_dir, &prefix, &mut dirs));
    }

    let fingerprint = fold(&files);
    Some(WorkspaceScan {
        files,
        dirs,
        fingerprint,
    })
}
