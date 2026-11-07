//! EVM bytecode.

use crate::{
    constants::{EIP7702_BYTECODE_LEN, EIP7702_MAGIC_BYTES, EIP7702_VERSION},
    interpreter::op,
    once_lock::OnceLock,
};
use alloc::sync::Arc;
use alloy_primitives::{Address, B256, Bytes, KECCAK256_EMPTY, keccak256};
use analysis::pad_legacy;
use core::{cmp::Ordering, fmt, hash};
use thiserror::Error;

mod analysis;
mod jump_table;

#[cfg(feature = "serde")]
mod serde_impl;

pub use analysis::analyze_legacy;
pub use jump_table::{JumpTable, JumpTableRef};

/// EIP-7702 decode errors.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Eip7702DecodeError {
    /// Invalid length of the raw bytecode.
    #[error("Eip7702 is not 23 bytes long")]
    InvalidLength,
    /// Invalid magic number.
    #[error("Bytecode is not starting with 0xEF01")]
    InvalidMagic,
    /// Unsupported version.
    #[error("Unsupported Eip7702 version.")]
    UnsupportedVersion,
}

/// Bytecode decode errors.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BytecodeDecodeError {
    /// EIP-7702 decode error.
    #[error(transparent)]
    Eip7702(#[from] Eip7702DecodeError),
}

/// Ethereum EVM bytecode.
#[derive(Clone)]
pub struct Bytecode(Option<Arc<BytecodeInner>>);

/// Inner bytecode representation.
///
/// This struct is flattened to avoid nested allocations. The `kind` field determines
/// how the bytecode should be interpreted.
#[derive(Debug)]
struct BytecodeInner {
    /// The kind of bytecode.
    kind: BytecodeKind,
    /// The bytecode bytes.
    ///
    /// For legacy bytecode, this may be padded with zeros at the end.
    bytecode: Bytes,
    /// The original length of the bytecode before padding.
    original_len: usize,
    /// Lazily initialized jump table for legacy bytecode.
    jump_table: OnceLock<JumpTable>,
    /// Cached hash of the original bytecode.
    hash: OnceLock<B256>,
}

/// The kind of bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Default)]
#[non_exhaustive]
pub enum BytecodeKind {
    /// Legacy bytecode with a lazily initialized jump table.
    #[default]
    Legacy,
    /// EIP-7702 delegated bytecode.
    Eip7702,
}

impl Default for Bytecode {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Default for &Bytecode {
    #[inline]
    fn default() -> Self {
        static EMPTY: Bytecode = Bytecode::new();
        &EMPTY
    }
}

impl PartialEq for Bytecode {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.original_byte_slice() == other.original_byte_slice()
    }
}

impl Eq for Bytecode {}

impl hash::Hash for Bytecode {
    #[inline]
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.original_byte_slice().hash(state);
    }
}

impl PartialOrd for Bytecode {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytecode {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.original_byte_slice().cmp(other.original_byte_slice())
    }
}

impl fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Bytecode").field(self.inner()).finish()
    }
}

impl Bytecode {
    /// Creates an empty legacy [`Bytecode`] backed by exactly one STOP opcode.
    #[inline]
    pub const fn new() -> Self {
        Self(None)
    }

    #[inline]
    fn empty_inner() -> &'static BytecodeInner {
        static EMPTY: OnceLock<BytecodeInner> = OnceLock::new();
        EMPTY.get_or_init(|| BytecodeInner {
            kind: BytecodeKind::Legacy,
            bytecode: Bytes::from_static(&[op::STOP]),
            original_len: 0,
            jump_table: OnceLock::new(),
            hash: {
                let hash = OnceLock::new();
                let _ = hash.set(KECCAK256_EMPTY);
                hash
            },
        })
    }

    #[inline]
    fn inner(&self) -> &BytecodeInner {
        match &self.0 {
            Some(inner) => inner,
            None => Self::empty_inner(),
        }
    }

    /// Creates a new legacy [`Bytecode`] with padding and deferred jump analysis.
    ///
    /// Appends 33 zero bytes unless the input already ends with 33 zeros.
    /// Empty code retains its single STOP representation.
    #[inline(never)]
    pub fn new_legacy(raw: Bytes) -> Self {
        if raw.is_empty() {
            return Self::new();
        }

        let original_len = raw.len();
        let bytecode = pad_legacy(raw);
        Self(Some(Arc::new(BytecodeInner {
            kind: BytecodeKind::Legacy,
            original_len,
            bytecode,
            jump_table: OnceLock::new(),
            hash: OnceLock::new(),
        })))
    }

    /// Creates a new raw [`Bytecode`].
    ///
    /// # Panics
    ///
    /// Panics if bytecode is in incorrect format. If you want to handle errors use
    /// [`Self::new_raw_checked`].
    #[inline]
    pub fn new_raw(bytecode: Bytes) -> Self {
        Self::new_raw_checked(bytecode).expect("Expect correct bytecode")
    }

    /// Creates a new EIP-7702 [`Bytecode`] from [`Address`].
    #[inline]
    pub fn new_eip7702(address: Address) -> Self {
        let raw: Bytes = [EIP7702_MAGIC_BYTES, &[EIP7702_VERSION], &address[..]].concat().into();
        Self::from_valid_eip7702(raw)
    }

    /// Creates a new raw [`Bytecode`].
    ///
    /// Returns an error on incorrect bytecode format.
    #[inline]
    pub fn new_raw_checked(bytes: Bytes) -> Result<Self, BytecodeDecodeError> {
        if bytes.starts_with(EIP7702_MAGIC_BYTES) {
            Self::new_eip7702_raw(bytes).map_err(Into::into)
        } else {
            Ok(Self::new_legacy(bytes))
        }
    }

    /// Creates a new [`Bytecode`] from code read from state.
    ///
    /// Valid EIP-7702 delegation designators become EIP-7702 bytecode and anything else is legacy
    /// code. Unlike [`Self::new_raw_checked`], code starting with the EIP-7702 magic that is not a
    /// valid designator is accepted as legacy, since it may have been deployed before EIP-3541.
    #[inline]
    pub fn from_stored(bytes: Bytes) -> Self {
        if Self::check_eip7702(&bytes).is_ok() {
            Self::from_valid_eip7702(bytes)
        } else {
            Self::new_legacy(bytes)
        }
    }

    /// Creates a new EIP-7702 [`Bytecode`] from raw bytes.
    ///
    /// Returns an error if the bytes are not valid EIP-7702 bytecode.
    #[inline]
    pub fn new_eip7702_raw(bytes: Bytes) -> Result<Self, Eip7702DecodeError> {
        Self::check_eip7702(&bytes)?;
        Ok(Self::from_valid_eip7702(bytes))
    }

    /// Checks that `bytes` is a valid EIP-7702 delegation designator.
    fn check_eip7702(bytes: &[u8]) -> Result<(), Eip7702DecodeError> {
        if bytes.len() != EIP7702_BYTECODE_LEN {
            return Err(Eip7702DecodeError::InvalidLength);
        }
        if !bytes.starts_with(EIP7702_MAGIC_BYTES) {
            return Err(Eip7702DecodeError::InvalidMagic);
        }
        if bytes[2] != EIP7702_VERSION {
            return Err(Eip7702DecodeError::UnsupportedVersion);
        }
        Ok(())
    }

    /// Wraps `bytes`, which [`Self::check_eip7702`] accepted, as EIP-7702 bytecode.
    fn from_valid_eip7702(bytes: Bytes) -> Self {
        Self(Some(Arc::new(BytecodeInner {
            kind: BytecodeKind::Eip7702,
            original_len: bytes.len(),
            bytecode: bytes,
            jump_table: OnceLock::new(),
            hash: OnceLock::new(),
        })))
    }

    /// Create new checked bytecode from pre-analyzed components.
    ///
    /// # Safety
    ///
    /// `bytecode` must satisfy the same padding invariants produced by
    /// `pad_legacy`. In particular, execution must never cause the
    /// interpreter to read past the backing allocation when decoding opcode
    /// immediates (`PUSH1`-`PUSH32` via `read_slice`, and `DUPN`/`SWAPN`/
    /// `EXCHANGE` via `read_u8`).
    ///
    /// [`Bytecode::new_legacy`] handles this automatically.
    /// This constructor is only for restoring trusted, previously analyzed
    /// bytecode where the padding was already applied.
    ///
    /// Violating this causes undefined behavior during execution due to
    /// out-of-bounds reads from raw pointers.
    ///
    /// # Panics
    ///
    /// * If `original_len` is greater than `bytecode.len()`
    /// * If jump table length is less than `original_len`
    /// * If bytecode is empty
    #[inline]
    pub unsafe fn new_analyzed(
        bytecode: Bytes,
        original_len: usize,
        jump_table: JumpTable,
    ) -> Self {
        assert!(original_len <= bytecode.len(), "original_len is greater than bytecode length");
        assert!(original_len <= jump_table.len(), "jump table length is less than original length");
        assert!(!bytecode.is_empty(), "bytecode cannot be empty");
        Self(Some(Arc::new(BytecodeInner {
            kind: BytecodeKind::Legacy,
            bytecode,
            original_len,
            jump_table: {
                let cached = OnceLock::new();
                let _ = cached.set(jump_table);
                cached
            },
            hash: OnceLock::new(),
        })))
    }

    /// Returns the kind of bytecode.
    #[inline]
    pub fn kind(&self) -> BytecodeKind {
        self.inner().kind
    }

    /// Returns `true` if bytecode is legacy.
    #[inline]
    pub fn is_legacy(&self) -> bool {
        self.kind() == BytecodeKind::Legacy
    }

    /// Returns `true` if bytecode is EIP-7702.
    #[inline]
    pub fn is_eip7702(&self) -> bool {
        self.kind() == BytecodeKind::Eip7702
    }

    /// Returns the EIP-7702 delegated address if this is EIP-7702 bytecode.
    #[inline]
    pub fn eip7702_address(&self) -> Option<Address> {
        if self.is_eip7702() {
            Some(Address::from_slice(&self.inner().bytecode[3..23]))
        } else {
            None
        }
    }

    /// Returns the legacy jump table, initializing it on first access.
    #[inline]
    pub fn legacy_jump_table(&self) -> Option<&JumpTable> {
        if self.is_legacy() { Some(self.jump_table()) } else { None }
    }

    #[inline]
    pub(crate) fn jump_table(&self) -> &JumpTable {
        self.inner().jump_table.get_or_init(|| {
            if self.is_legacy() {
                analyze_legacy(self.original_byte_slice())
            } else {
                JumpTable::default()
            }
        })
    }

    /// Calculates or returns cached hash of the bytecode.
    #[inline]
    pub fn hash_slow(&self) -> B256 {
        *self.inner().hash.get_or_init(|| keccak256(self.original_byte_slice()))
    }

    /// Caches `hash` as the hash of the original bytecode unless one is already cached.
    ///
    /// `hash` must be the keccak256 of [`Self::original_byte_slice`]. Does nothing for empty
    /// bytecode or the zero hash.
    #[inline]
    pub fn seed_hash(&self, hash: B256) {
        if self.is_empty() || hash.is_zero() {
            return;
        }
        if self.inner().hash.set(hash).is_err() {
            debug_assert_eq!(self.hash_slow(), hash, "seeded hash differs from cached hash");
        }
    }

    /// Returns a reference to the potentially padded bytecode bytes.
    #[inline]
    pub fn bytes(&self) -> &Bytes {
        &self.inner().bytecode
    }

    /// Returns the bytecode as a slice.
    #[inline]
    pub fn bytes_slice(&self) -> &[u8] {
        &self.inner().bytecode
    }

    /// Returns the original bytecode without padding.
    #[inline]
    pub fn original_bytes(&self) -> Bytes {
        let inner = self.inner();
        inner.bytecode.slice(..inner.original_len)
    }

    /// Returns the original bytecode as a byte slice without padding.
    #[inline]
    pub fn original_byte_slice(&self) -> &[u8] {
        let inner = self.inner();
        unsafe { inner.bytecode.get_unchecked(..inner.original_len) }
    }

    /// Returns the length of the original bytes (without padding).
    #[inline]
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |inner| inner.original_len)
    }

    /// Returns whether the bytecode is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "std"))]
    extern crate std;

    use super::*;
    use crate::interpreter::op;
    use alloc::format;
    use alloy_primitives::{Address, Bytes};
    use std::hash::{BuildHasher, RandomState};

    #[test]
    fn test_new_empty() {
        // SAFETY: a single STOP is padded.
        let analyzed = unsafe {
            Bytecode::new_analyzed(Bytes::from_static(&[op::STOP]), 0, JumpTable::default())
        };
        let hasher = RandomState::new();
        let empty = Bytecode::new();
        for bytecode in [
            Bytecode::default(),
            empty.clone(),
            empty,
            Bytecode::new_legacy(Bytes::new()),
            Bytecode::new_raw(Bytes::new()),
            Bytecode::new_raw_checked(Bytes::new()).unwrap(),
        ] {
            assert_eq!(bytecode.kind(), BytecodeKind::Legacy);
            assert!(bytecode.is_legacy());
            assert!(!bytecode.is_eip7702());
            assert_eq!(bytecode.eip7702_address(), None);
            assert_eq!(bytecode.len(), 0);
            assert!(bytecode.is_empty());
            assert_eq!(bytecode.bytes_slice(), [op::STOP]);
            assert_eq!(bytecode.bytes(), &Bytes::from_static(&[op::STOP]));
            assert!(bytecode.original_byte_slice().is_empty());
            assert_eq!(bytecode.original_bytes(), Bytes::new());
            assert_eq!(bytecode.hash_slow(), KECCAK256_EMPTY);
            assert!(bytecode.legacy_jump_table().unwrap().is_empty());
            assert!(core::ptr::eq(bytecode.jump_table(), bytecode.legacy_jump_table().unwrap()));
            assert_eq!(bytecode, analyzed);
            assert_eq!(bytecode.cmp(&analyzed), Ordering::Equal);
            assert_eq!(hasher.hash_one(&bytecode), hasher.hash_one(&analyzed));
            assert!(bytecode < Bytecode::new_legacy(Bytes::from_static(&[op::STOP])));
        }
    }

    #[test]
    fn test_new_analyzed() {
        let raw = Bytes::from_static(&[op::PUSH1, 0x01]);
        let bytecode = Bytecode::new_legacy(raw);
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytes().clone(),
                bytecode.len(),
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "original_len is greater than bytecode length")]
    fn test_panic_on_large_original_len() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01]));
        let _ = unsafe {
            Bytecode::new_analyzed(
                bytecode.bytes().clone(),
                100,
                bytecode.legacy_jump_table().unwrap().clone(),
            )
        };
    }

    #[test]
    #[should_panic(expected = "jump table length is less than original length")]
    fn test_panic_on_short_jump_table() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0x01]));
        let jump_table = JumpTable::from_slice(&[0], 1);
        let _ = unsafe { Bytecode::new_analyzed(bytecode.bytes().clone(), 2, jump_table) };
    }

    #[test]
    #[should_panic(expected = "bytecode cannot be empty")]
    fn test_panic_on_empty_bytecode() {
        let bytecode = Bytes::from_static(&[]);
        let jump_table = JumpTable::default();
        let _ = unsafe { Bytecode::new_analyzed(bytecode, 0, jump_table) };
    }

    #[test]
    fn eip7702_sanity_decode() {
        let raw = Bytes::from_static(&[0xEF, 0x01, 0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(Bytecode::new_eip7702_raw(raw), Err(Eip7702DecodeError::InvalidLength));

        let mut raw = [0u8; EIP7702_BYTECODE_LEN];
        raw[..2].copy_from_slice(EIP7702_MAGIC_BYTES);
        raw[2] = 1;
        assert_eq!(
            Bytecode::new_eip7702_raw(Bytes::from(raw)),
            Err(Eip7702DecodeError::UnsupportedVersion)
        );

        raw[2] = EIP7702_VERSION;
        raw[3..7].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let raw = Bytes::from(raw);
        let bytecode = Bytecode::new_eip7702_raw(raw.clone()).unwrap();
        assert!(bytecode.is_eip7702());
        assert_eq!(bytecode.eip7702_address(), Some(Address::from_slice(&raw[3..])));
        assert_eq!(bytecode.original_bytes(), raw);
    }

    #[test]
    fn eip7702_from_address() {
        let address = Address::repeat_byte(0x01);
        let bytecode = Bytecode::new_eip7702(address);
        assert_eq!(bytecode.eip7702_address(), Some(address));
        assert_eq!(bytecode.original_bytes().len(), EIP7702_BYTECODE_LEN);
        assert_eq!(&bytecode.original_byte_slice()[..3], &[0xEF, 0x01, 0x00]);
    }

    #[test]
    fn from_stored_accepts_non_designator_magic_as_legacy() {
        let address = Address::repeat_byte(0x01);
        let designator = Bytecode::new_eip7702(address).original_bytes();
        let from_designator = Bytecode::from_stored(designator.clone());
        assert_eq!(from_designator.kind(), BytecodeKind::Eip7702);
        assert_eq!(from_designator.eip7702_address(), Some(address));

        for bytes in [
            Bytes::from_static(&[0xEF, 0x01, 0x00, 0x60, 0x01]),
            designator.slice(..EIP7702_BYTECODE_LEN - 1),
            Bytes::from([designator.as_ref(), &[op::STOP]].concat()),
            Bytes::from_static(&[0xEF, 0x01, 0x01]),
        ] {
            assert!(Bytecode::new_raw_checked(bytes.clone()).is_err());
            let bytecode = Bytecode::from_stored(bytes.clone());
            assert_eq!(bytecode.kind(), BytecodeKind::Legacy);
            assert_eq!(bytecode.original_bytes(), bytes);
        }
    }

    #[test]
    fn seed_hash_sets_only_an_empty_cache() {
        let bytecode = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let seed = B256::repeat_byte(0x5e);
        bytecode.seed_hash(seed);
        assert_eq!(bytecode.hash_slow(), seed);

        let empty = Bytecode::new();
        empty.seed_hash(seed);
        assert_eq!(empty.hash_slow(), KECCAK256_EMPTY);
        let bytecode = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        bytecode.seed_hash(B256::ZERO);
        assert_eq!(bytecode.hash_slow(), keccak256([0x60, 0x01]));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "seeded hash differs from cached hash")]
    fn seed_hash_panics_on_conflicting_hash_in_debug() {
        let bytecode = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        bytecode.seed_hash(B256::repeat_byte(0x5e));
        bytecode.seed_hash(B256::repeat_byte(0x5f));
    }

    #[test]
    fn analysis_is_lazy_and_shared() {
        let raw = Bytes::from_static(&[op::JUMPDEST, op::PUSH1, op::JUMPDEST, op::PUSH32]);
        for bytecode in [
            Bytecode::new_legacy(raw.clone()),
            Bytecode::new_raw(raw.clone()),
            Bytecode::new_raw_checked(raw.clone()).unwrap(),
        ] {
            let cloned = bytecode.clone();
            assert_eq!(bytecode.original_byte_slice(), raw.as_ref());
            assert_eq!(bytecode.original_bytes().as_ptr(), bytecode.bytes().as_ptr());
            assert_eq!(bytecode.len(), raw.len());
            assert!(!bytecode.is_empty());
            assert_eq!(bytecode, cloned);
            assert_eq!(bytecode.cmp(&cloned), Ordering::Equal);
            assert_eq!(bytecode.hash_slow(), keccak256(&raw));
            assert_eq!(bytecode.bytes_slice().len(), raw.len() + 33);
            assert_eq!(&bytecode.bytes_slice()[raw.len()..], &[0; 33]);
            assert!(bytecode.inner().jump_table.get().is_none());

            let table = bytecode.legacy_jump_table().unwrap();
            assert_eq!(table.len(), raw.len());
            assert!(table.is_valid(0));
            assert!(!table.is_valid(2)); // JUMPDEST in PUSH data.
            assert!(!table.is_valid(raw.len())); // Padding is not original code.
            assert!(core::ptr::eq(table, cloned.legacy_jump_table().unwrap()));
            assert!(core::ptr::eq(table, bytecode.jump_table()));
        }
    }

    #[test]
    fn interpreter_accessor_initializes_analysis() {
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::JUMPDEST]));
        assert!(bytecode.inner().jump_table.get().is_none());
        let table = bytecode.jump_table();
        assert!(table.is_valid(0));
        assert!(core::ptr::eq(table, bytecode.inner().jump_table.get().unwrap()));
        assert!(core::ptr::eq(table, bytecode.legacy_jump_table().unwrap()));
    }

    #[test]
    fn supplied_analysis_is_preserved() {
        let padded = Bytes::from_static(&[op::JUMPDEST, op::PUSH1, 0, op::STOP]);
        let ptr = padded.as_ptr();
        let table = JumpTable::from_slice(&[1], 2);
        let table_ptr = table.as_slice().as_ptr();
        // SAFETY: PUSH1 has a complete immediate and a terminating STOP.
        let bytecode = unsafe { Bytecode::new_analyzed(padded, 2, table) };
        assert!(bytecode.inner().jump_table.get().is_some());
        assert_eq!(bytecode.bytes().as_ptr(), ptr);
        assert_eq!(bytecode.bytes().len(), 4);
        assert_eq!(bytecode.original_byte_slice(), &[op::JUMPDEST, op::PUSH1]);
        assert_eq!(bytecode.jump_table().as_slice().as_ptr(), table_ptr);
        assert!(bytecode.jump_table().is_valid(0));
    }

    #[test]
    fn empty_and_delegated_jump_tables() {
        assert!(Bytecode::new().jump_table().is_empty());
        let delegated = Bytecode::new_eip7702(Address::with_last_byte(op::JUMPDEST));
        let from_raw = Bytecode::new_eip7702_raw(delegated.original_bytes()).unwrap();
        for bytecode in [delegated, from_raw] {
            assert!(bytecode.legacy_jump_table().is_none());
            assert!(bytecode.inner().jump_table.get().is_none());
            assert_eq!(bytecode.bytes_slice(), bytecode.original_byte_slice());
            assert!(bytecode.jump_table().is_empty());
        }
    }

    #[test]
    fn existing_zero_suffix_is_original_code() {
        let mut raw = [0; 34];
        raw[0] = op::PUSH32;
        let raw = Bytes::from(raw);
        let bytecode = Bytecode::new_legacy(raw.clone());
        assert_eq!(bytecode.bytes().as_ptr(), raw.as_ptr());
        assert_eq!(bytecode.len(), raw.len());
        assert_eq!(bytecode.original_bytes(), raw);
        assert_eq!(bytecode.hash_slow(), keccak256(&raw));
        assert!(bytecode.inner().jump_table.get().is_none());
        assert_eq!(bytecode.jump_table().len(), raw.len());
    }

    #[test]
    fn empty_debug_matches_shared_form() {
        let empty = Bytecode::new();
        let shared = Arc::new(empty.inner());
        let _ = empty.hash_slow();
        let _ = empty.jump_table();
        assert_eq!(format!("{empty:?}"), format!("Bytecode({shared:?})"));
    }
}
