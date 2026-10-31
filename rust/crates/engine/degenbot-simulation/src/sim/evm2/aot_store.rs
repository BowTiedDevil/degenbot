//! The durable AOT artifact store (ADR-089 phase 6).
//!
//! Upstream's
//! [`RuntimeArtifactStore`](::evm2_jit::runtime::RuntimeArtifactStore) is
//! temp-directory backed: its index and object files die with the process, so
//! every restart recompiles the executor from scratch.
//! [`DurableArtifactStore`] keeps the same [`ArtifactStore`] contract over a
//! caller-supplied stable directory — one relocatable object per artifact
//! (`<stem>.o`) plus a manifest sidecar (`<stem>.manifest.json`), each written
//! through a temporary sibling and a rename so a reader never observes a
//! partial artifact.
//!
//! Loads are content-hash verified: the object bytes must hash (keccak-256) to
//! [`ArtifactManifest::content_hash`] and the sidecar's artifact key must equal
//! the requested one. A mismatched hash, an unreadable file, a missing sidecar,
//! or an unparseable manifest is a **miss** (`Ok(None)`; skipped by
//! [`load_all`](ArtifactStore::load_all)), never an error and never a panic —
//! a damaged cache directory degrades to recompilation, which is the correct
//! fallback. Storing is a verbatim write: verification is the loader's job, so
//! a manifest whose `content_hash` disagrees with its object stays a miss.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ::evm2::SpecId;
use ::evm2_jit::eyre::{self, WrapErr};
use ::evm2_jit::runtime::{
    ArtifactKey, ArtifactManifest, ArtifactStore, BackendSelection, RuntimeCacheKey, StoredArtifact,
};
use ::evm2_jit::OptimizationLevel;
use alloy::primitives::{keccak256, B256};

/// The relocatable-object suffix.
const OBJECT_SUFFIX: &str = ".o";
/// The manifest sidecar suffix.
const MANIFEST_SUFFIX: &str = ".manifest.json";
/// The sidecar schema version. A sidecar with any other version is a miss, so
/// a future on-disk change degrades to recompilation instead of a misread.
const MANIFEST_FORMAT_VERSION: u32 = 1;

/// An [`ArtifactStore`] over a stable on-disk directory (see the module doc).
#[derive(Clone, Debug)]
pub struct DurableArtifactStore {
    dir: PathBuf,
}

impl DurableArtifactStore {
    /// Opens (creating if absent) a durable artifact store rooted at `dir`.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be created.
    pub fn new(dir: impl Into<PathBuf>) -> eyre::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .wrap_err_with(|| format!("create artifact store directory {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// The store's root directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn object_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!("{}{OBJECT_SUFFIX}", key_stem(key)))
    }

    fn manifest_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!("{}{MANIFEST_SUFFIX}", key_stem(key)))
    }

    /// Reads one artifact, verifying the key and the content hash. Every
    /// failure class is a miss.
    fn read_artifact(&self, key: &ArtifactKey) -> Option<StoredArtifact> {
        let manifest = read_manifest(&self.manifest_path(key))?;
        if manifest.artifact_key != *key {
            return None;
        }
        let object_path = self.object_path(key);
        let object_bytes = fs::read(&object_path).ok()?;
        verified(manifest, object_path, &object_bytes)
    }
}

impl ArtifactStore for DurableArtifactStore {
    fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        let entries = fs::read_dir(&self.dir)
            .wrap_err_with(|| format!("read artifact store directory {}", self.dir.display()))?;
        let mut artifacts = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(MANIFEST_SUFFIX) else {
                continue;
            };
            let Some(manifest) = read_manifest(&entry.path()) else {
                continue;
            };
            let object_path = self.dir.join(format!("{stem}{OBJECT_SUFFIX}"));
            let Ok(object_bytes) = fs::read(&object_path) else {
                continue;
            };
            if let Some(stored) = verified(manifest, object_path, &object_bytes) {
                artifacts.push((stored.manifest.artifact_key, stored));
            }
        }
        Ok(artifacts)
    }

    fn load(&self, key: &ArtifactKey) -> eyre::Result<Option<StoredArtifact>> {
        Ok(self.read_artifact(key))
    }

    fn store(
        &self,
        key: &ArtifactKey,
        manifest: &ArtifactManifest,
        object_bytes: &[u8],
    ) -> eyre::Result<()> {
        // Object first: a sidecar without its object is a miss, never a read
        // of stale bytes.
        write_atomic(&self.object_path(key), object_bytes)?;
        let record = ManifestRecord::from_manifest(manifest);
        let encoded = serde_json::to_vec(&record)
            .wrap_err_with(|| format!("encode artifact manifest for {}", key_stem(key)))?;
        write_atomic(&self.manifest_path(key), &encoded)
    }

    fn delete(&self, key: &ArtifactKey) -> eyre::Result<()> {
        remove_if_present(&self.object_path(key))?;
        remove_if_present(&self.manifest_path(key))
    }

    fn clear(&self) -> eyre::Result<()> {
        let entries = fs::read_dir(&self.dir)
            .wrap_err_with(|| format!("read artifact store directory {}", self.dir.display()))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name.ends_with(OBJECT_SUFFIX) || file_name.ends_with(MANIFEST_SUFFIX) {
                remove_if_present(&entry.path())?;
            }
        }
        Ok(())
    }
}

/// The artifact and its bytes, or `None` when the bytes are not what the
/// manifest describes.
fn verified(
    manifest: ArtifactManifest,
    object_path: PathBuf,
    object_bytes: &[u8],
) -> Option<StoredArtifact> {
    if keccak256(object_bytes).0 != manifest.content_hash {
        return None;
    }
    // A truncated/over-long object is a miss even when a hash collision would
    // be needed to reach here: cheap structural check first.
    if object_bytes.len() != manifest.artifact_len {
        return None;
    }
    Some(StoredArtifact {
        manifest,
        object_path,
    })
}

/// Reads and decodes a manifest sidecar. Any failure (IO, JSON, unknown
/// discriminant, wrong schema version) is `None` — a miss.
fn read_manifest(path: &Path) -> Option<ArtifactManifest> {
    let bytes = fs::read(path).ok()?;
    let record: ManifestRecord = serde_json::from_slice(&bytes).ok()?;
    record.into_manifest()
}

/// The on-disk manifest encoding — an explicit, stable projection of
/// [`ArtifactManifest`] (its key types carry no serde derives, and the
/// encoding must not drift with a `Debug` format change).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ManifestRecord {
    format_version: u32,
    code_hash: [u8; 32],
    spec_id: u32,
    /// [`BackendSelection`] discriminant: `0` = auto, `1` = llvm.
    backend: u8,
    /// [`OptimizationLevel`] discriminant: `0` = none, `1` = less, `2` =
    /// default, `3` = aggressive.
    opt_level: u8,
    abi_version: u32,
    config_hash: [u8; 32],
    symbol_name: String,
    bytecode_len: u64,
    artifact_len: u64,
    created_at_unix_secs: u64,
    content_hash: [u8; 32],
}

impl ManifestRecord {
    fn from_manifest(manifest: &ArtifactManifest) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            code_hash: manifest.artifact_key.runtime.code_hash.0,
            spec_id: u32::from(manifest.artifact_key.runtime.spec_id),
            backend: match manifest.artifact_key.backend {
                BackendSelection::Auto => 0,
                BackendSelection::Llvm => 1,
            },
            opt_level: match manifest.artifact_key.opt_level {
                OptimizationLevel::None => 0,
                OptimizationLevel::Less => 1,
                OptimizationLevel::Default => 2,
                OptimizationLevel::Aggressive => 3,
            },
            abi_version: manifest.artifact_key.abi_version,
            config_hash: manifest.artifact_key.config_hash.0,
            symbol_name: manifest.symbol_name.clone(),
            bytecode_len: to_u64(manifest.bytecode_len),
            artifact_len: to_u64(manifest.artifact_len),
            created_at_unix_secs: manifest.created_at_unix_secs,
            content_hash: manifest.content_hash,
        }
    }

    fn into_manifest(self) -> Option<ArtifactManifest> {
        if self.format_version != MANIFEST_FORMAT_VERSION {
            return None;
        }
        let backend = match self.backend {
            0 => BackendSelection::Auto,
            1 => BackendSelection::Llvm,
            _ => return None,
        };
        let opt_level = match self.opt_level {
            0 => OptimizationLevel::None,
            1 => OptimizationLevel::Less,
            2 => OptimizationLevel::Default,
            3 => OptimizationLevel::Aggressive,
            _ => return None,
        };
        Some(ArtifactManifest {
            artifact_key: ArtifactKey {
                runtime: RuntimeCacheKey {
                    code_hash: B256::from(self.code_hash),
                    spec_id: SpecId::try_from_u32(self.spec_id)?,
                },
                backend,
                opt_level,
                abi_version: self.abi_version,
                config_hash: B256::from(self.config_hash),
            },
            symbol_name: self.symbol_name,
            bytecode_len: usize::try_from(self.bytecode_len).ok()?,
            artifact_len: usize::try_from(self.artifact_len).ok()?,
            created_at_unix_secs: self.created_at_unix_secs,
            content_hash: self.content_hash,
        })
    }
}

/// The stable file stem for an artifact key — every key field that selects
/// distinct machine code, in a fixed textual encoding (the loader never parses
/// it back; the manifest sidecar is authoritative).
fn key_stem(key: &ArtifactKey) -> String {
    let backend = match key.backend {
        BackendSelection::Auto => "auto",
        BackendSelection::Llvm => "llvm",
    };
    let opt_level = match key.opt_level {
        OptimizationLevel::None => "none",
        OptimizationLevel::Less => "less",
        OptimizationLevel::Default => "default",
        OptimizationLevel::Aggressive => "aggressive",
    };
    format!(
        "{code_hash:x}_s{spec}_{backend}_{opt_level}_v{abi_version}_{config_hash:x}",
        code_hash = key.runtime.code_hash,
        spec = u32::from(key.runtime.spec_id),
        abi_version = key.abi_version,
        config_hash = key.config_hash,
    )
}

/// Lossless `usize -> u64` (every supported target is 64-bit or narrower).
fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Writes `bytes` to `path` through a temporary sibling and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> eyre::Result<()> {
    let tmp = temporary_sibling(path);
    let write = || -> eyre::Result<()> {
        let mut file =
            fs::File::create(&tmp).wrap_err_with(|| format!("create {}", tmp.display()))?;
        file.write_all(bytes)
            .wrap_err_with(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .wrap_err_with(|| format!("sync {}", tmp.display()))?;
        Ok(())
    };
    if let Err(err) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    fs::rename(&tmp, path).wrap_err_with(|| {
        let _ = fs::remove_file(&tmp);
        format!("rename {} to {}", tmp.display(), path.display())
    })
}

/// A private, unique sibling path for the atomic write.
fn temporary_sibling(path: &Path) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id()))
}

/// Removes `path`, tolerating an absent file.
fn remove_if_present(path: &Path) -> eyre::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).wrap_err_with(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use ::evm2::SpecId;
    use ::evm2_jit::runtime::{
        ArtifactKey, ArtifactManifest, ArtifactStore, BackendSelection, RuntimeCacheKey,
    };
    use ::evm2_jit::OptimizationLevel;
    use alloy::primitives::{keccak256, B256};

    use super::DurableArtifactStore;

    /// A unique scratch directory removed on drop.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "degenbot-aot-store-{}-{sequence}-{label}",
                std::process::id()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key(code_byte: u8) -> ArtifactKey {
        ArtifactKey {
            runtime: RuntimeCacheKey {
                code_hash: B256::with_last_byte(code_byte),
                spec_id: SpecId::OSAKA,
            },
            backend: BackendSelection::Llvm,
            opt_level: OptimizationLevel::Default,
            abi_version: ::evm2_jit::ABI_VERSION,
            config_hash: B256::with_last_byte(0xFF),
        }
    }

    fn manifest(artifact_key: ArtifactKey, object: &[u8]) -> ArtifactManifest {
        ArtifactManifest {
            artifact_key,
            symbol_name: "evm2_execute".to_string(),
            bytecode_len: 3,
            artifact_len: object.len(),
            created_at_unix_secs: 42,
            content_hash: keccak256(object).0,
        }
    }

    fn open(dir: &ScratchDir) -> DurableArtifactStore {
        DurableArtifactStore::new(dir.path()).expect("the scratch store opens")
    }

    fn stage(dir: &ScratchDir, artifact_key: ArtifactKey, object: &[u8]) -> DurableArtifactStore {
        let store = open(dir);
        store
            .store(&artifact_key, &manifest(artifact_key, object), object)
            .expect("the artifact stores");
        store
    }

    #[test]
    fn store_starts_empty_and_missing_is_a_miss() {
        let dir = ScratchDir::new("empty");
        let store = open(&dir);

        assert!(store
            .load(&key(1))
            .expect("a missing artifact loads")
            .is_none());
        assert!(store.load_all().expect("the empty store lists").is_empty());
    }

    #[test]
    fn round_trips_an_artifact() {
        let dir = ScratchDir::new("roundtrip");
        let artifact_key = key(2);
        let object = b"\x7fELF-relocatable-object";
        let store = stage(&dir, artifact_key, object);

        let loaded = store
            .load(&artifact_key)
            .expect("the artifact loads")
            .expect("the artifact is present");
        assert_eq!(loaded.manifest.symbol_name, "evm2_execute");
        assert_eq!(loaded.manifest.artifact_len, object.len());
        assert_eq!(loaded.manifest.bytecode_len, 3);
        assert_eq!(loaded.manifest.created_at_unix_secs, 42);
        assert_eq!(loaded.manifest.content_hash, keccak256(object).0);
        assert_eq!(
            fs::read(&loaded.object_path).expect("the object file reads"),
            object
        );

        let all = store.load_all().expect("the store lists");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, artifact_key);
        assert_eq!(all[0].1.manifest.artifact_key, artifact_key);
    }

    #[test]
    fn artifacts_survive_across_store_instances() {
        let dir = ScratchDir::new("reload");
        let artifact_key = key(3);
        let object = b"persisted-object";
        drop(stage(&dir, artifact_key, object));

        // A fresh instance over the same directory — the process-restart shape.
        let reopened = open(&dir);
        let loaded = reopened
            .load(&artifact_key)
            .expect("the artifact loads across instances")
            .expect("the durable artifact is present");
        assert_eq!(
            fs::read(&loaded.object_path).expect("the object file reads"),
            object
        );
        assert_eq!(reopened.load_all().expect("the store lists").len(), 1);
    }

    #[test]
    fn corrupt_object_is_a_miss() {
        let dir = ScratchDir::new("corrupt-object");
        let artifact_key = key(4);
        let store = stage(&dir, artifact_key, b"original-object");

        let object_path = store
            .load(&artifact_key)
            .expect("load")
            .expect("present")
            .object_path;
        fs::write(&object_path, b"tampered-object").expect("the object is overwritten");

        assert!(store
            .load(&artifact_key)
            .expect("a corrupt artifact loads")
            .is_none());
        assert!(store.load_all().expect("the store lists").is_empty());
    }

    #[test]
    fn corrupt_manifest_is_a_miss() {
        let dir = ScratchDir::new("corrupt-manifest");
        let artifact_key = key(5);
        let store = stage(&dir, artifact_key, b"object");

        let manifest_path = store
            .dir()
            .join(format!("{}.manifest.json", super::key_stem(&artifact_key)));
        assert!(manifest_path.exists(), "the sidecar is written");
        fs::write(&manifest_path, b"{ not json").expect("the manifest is overwritten");

        assert!(store
            .load(&artifact_key)
            .expect("a corrupt sidecar loads")
            .is_none());
        assert!(store.load_all().expect("the store lists").is_empty());
    }

    #[test]
    fn content_hash_mismatch_is_a_miss() {
        let dir = ScratchDir::new("hash-mismatch");
        let artifact_key = key(6);
        let object = b"object";
        let store = open(&dir);
        let mut mismatched = manifest(artifact_key, object);
        mismatched.content_hash = [0xAB; 32];
        store
            .store(&artifact_key, &mismatched, object)
            .expect("the store writes verbatim");

        assert!(store
            .load(&artifact_key)
            .expect("a hash mismatch loads")
            .is_none());
        assert!(store.load_all().expect("the store lists").is_empty());
    }

    #[test]
    fn truncated_object_is_a_miss() {
        let dir = ScratchDir::new("truncated");
        let artifact_key = key(7);
        let store = stage(&dir, artifact_key, b"complete-object");

        let object_path = store
            .load(&artifact_key)
            .expect("load")
            .expect("present")
            .object_path;
        fs::write(&object_path, b"complete").expect("the object is truncated");

        assert!(store
            .load(&artifact_key)
            .expect("a truncated artifact loads")
            .is_none());
    }

    #[test]
    fn delete_and_clear_remove_files() {
        let dir = ScratchDir::new("delete-clear");
        let first = key(8);
        let second = key(9);
        let store = open(&dir);
        store
            .store(&first, &manifest(first, b"a"), b"a")
            .expect("stores");
        store
            .store(&second, &manifest(second, b"b"), b"b")
            .expect("stores");
        let first_path = store
            .load(&first)
            .expect("load")
            .expect("present")
            .object_path;
        let second_path = store
            .load(&second)
            .expect("load")
            .expect("present")
            .object_path;

        store.delete(&first).expect("delete succeeds");
        assert!(store.load(&first).expect("load").is_none());
        assert!(!first_path.exists(), "the deleted object is gone");
        assert!(store
            .load(&second)
            .expect("the surviving artifact loads")
            .is_some());

        store.clear().expect("clear succeeds");
        assert!(store.load(&second).expect("load").is_none());
        assert!(!second_path.exists(), "cleared objects are gone");
        assert!(store.load_all().expect("the store lists").is_empty());
    }

    #[test]
    fn replacing_an_artifact_keeps_the_latest_object() {
        let dir = ScratchDir::new("replace");
        let artifact_key = key(10);
        let store = open(&dir);
        store
            .store(&artifact_key, &manifest(artifact_key, b"one"), b"one")
            .expect("stores");
        store
            .store(
                &artifact_key,
                &manifest(artifact_key, b"a longer object"),
                b"a longer object",
            )
            .expect("stores");

        let loaded = store.load(&artifact_key).expect("load").expect("present");
        assert_eq!(
            fs::read(&loaded.object_path).expect("reads"),
            b"a longer object"
        );
        assert_eq!(loaded.manifest.artifact_len, b"a longer object".len());
        assert_eq!(store.load_all().expect("the store lists").len(), 1);
    }
}
