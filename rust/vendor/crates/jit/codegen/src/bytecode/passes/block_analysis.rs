//! Abstract stack interpretation for resolving dynamic jump targets and constant propagation.
//!
//! This pass builds a basic-block CFG and runs worklist-based abstract interpretation to a
//! fixpoint, propagating abstract stack states across blocks. It resolves jump targets that the
//! block-local pass `block_analysis_local` (which interprets each block independently) cannot
//! handle — including internal function return patterns where multiple callers push different
//! return addresses.
//!
//! After the fixpoint converges, the known-constant operands at each instruction are persisted
//! as [`OperandSnapshot`]s so that later passes (e.g. code generation) can query the
//! known-constant value of stack operands.
//!
//! ## Abstract domain
//!
//! Per-value lattice (ascending order):
//! - `Const(U256Imm)` — a single known constant.
//! - `ConstSet(ConstSetIdx)` — multiple known constants (interned, sorted, deduplicated).
//! - `Range(RangeInclusive<u32>)` — a value in a small range.
//! - `Top` — reachable but unknown.
//!
//! Sets only hold jump targets, and ranges only absorb the constants and ranges they contain.
//!
//! Per-block lattice:
//! - `Bottom` — block not yet reached.
//! - `Known(Vec<AbsValue>)` — reached with a known stack state (top-aligned; when predecessors have
//!   different stack heights, only the entries of the shorter stack are kept).
//!
//! Each block keeps one such state per context: the return addresses and other valid jump target
//! constants on the stack, so that internal functions are analyzed separately for each call chain
//! (see [`ContextMode`]).
//!
//! A recursive call chain repeats return addresses, and would need a new context at every level.
//! Instead, a stack that holds a jump target twice keeps only the entries above the deeper copy,
//! and continues in one of a set of segments: shared stack fragments that may in turn continue in
//! segments, including themselves. The recursion then reaches a fixed set of contexts without
//! joining the frames of different levels, so that the code below the recursion still finds its
//! own return addresses. Calls that share a jump target without a recursion fold as well, which
//! can join their frames, so the analysis only folds when it cannot resolve every jump without
//! folds, and keeps the folded result only when it resolves every jump.
//!
//! A `JUMPI` propagates only along the edges that its condition allows in each context, and the
//! values that its condition bounds on an edge, such as an index below an array length, get that
//! bound there. Memory writes at ranged offsets only forget the words in their range.
//! A jump to a range goes to the valid jump targets in it, such as the entries of a jump table.
//! When a jump's operand is no range but an instruction before it leaves a narrow one, such as the
//! index of a table that the code reads with a shift, the analysis runs the rest of the block on
//! each value of that range, and the jump goes to the targets it finds.
//!
//! ## Soundness
//!
//! When the fixpoint does not converge (iteration cap reached) or unresolved `Top` jumps remain,
//! a transitive predecessor analysis invalidates suspect resolutions that may be based on
//! incomplete information, ensuring that only sound jump targets are reported as resolved.
//! When it converges with no unresolved `Top` jumps, blocks it never reached are unreachable.

use super::StackSection;
use crate::{
    FxHashMap, FxHashSet, InstData,
    bytecode::{Bytecode, Inst, InstFlags, Interner, U256Imm, U256Interner},
};
use alloy_primitives::{U256, map::FxHasher};
use bitvec::vec::BitVec;
use either::Either;
use evm2::{bytecode::JumpTable, constants::STACK_LIMIT, interpreter::op};
use oxc_index::IndexVec;
use smallvec::SmallVec;
use std::{
    cmp::{Ordering, Reverse},
    collections::{BinaryHeap, VecDeque},
    hash::{Hash, Hasher},
    ops::Range,
    range::RangeInclusive,
};

oxc_index::define_nonmax_u32_index_type! {
    /// Index into the interned constant-set pool.
    pub(crate) struct ConstSetIdx;
}
crate::bytecode::impl_index_display!(ConstSetIdx, "{}");

/// Per-instruction snapshot of abstract operand values.
///
/// Stored in stack order: index 0 is the deepest operand, last element is TOS.
/// Reached instructions have one entry per input; operands outside the tracked abstract
/// stack are stored as `Top`. Only the instruction's inputs are stored, not the entire stack.
pub(crate) type OperandSnapshot = SmallVec<[AbsValue; 4]>;

/// Bundles input and output snapshots for recording during abstract interpretation.
#[derive(Clone, Default)]
pub(crate) struct Snapshots {
    /// Pre-instruction input operand snapshots.
    pub(crate) inputs: IndexVec<Inst, OperandSnapshot>,
    /// Post-instruction output snapshot (single value per instruction).
    pub(crate) outputs: IndexVec<Inst, Option<AbsValue>>,
}

impl Snapshots {
    /// Restores snapshots for the given instructions from a previously saved copy.
    pub(crate) fn restore_from(&mut self, insts: impl Iterator<Item = Inst>, saved: &Self) {
        for inst in insts {
            self.inputs[inst].clone_from(&saved.inputs[inst]);
            self.outputs[inst] = saved.outputs[inst];
        }
    }

    /// Returns an input operand by stack depth, where depth zero is TOS.
    pub(crate) fn input(&self, inst: Inst, depth: usize) -> Option<AbsValue> {
        let snapshot = &self.inputs[inst];
        snapshot.get(snapshot.len().checked_sub(1 + depth)?).copied()
    }
}

/// Abstract value on the stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AbsValue {
    Const(U256Imm),
    /// Multiple known constant values (interned, sorted, deduplicated).
    ConstSet(ConstSetIdx),
    /// A value in a range of inline immediates with more than one value, such as an index that a
    /// bounds check limits.
    Range(RangeInclusive<u32>),
    /// Top value (unknown concrete value).
    #[default]
    Top,
}

impl AbsValue {
    /// Returns the single constant if this is `Const`, or `None` otherwise.
    pub(crate) const fn as_const(self) -> Option<U256Imm> {
        match self {
            Self::Const(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value in `range`, or `None` if its last value is not an inline immediate.
    pub(crate) fn range(range: RangeInclusive<u64>, interner: &mut U256Interner) -> Option<Self> {
        debug_assert!(range.start <= range.last);
        // Inline immediates are below 2^31.
        let last = u32::try_from(range.last).ok().filter(|&last| last < 1 << 31)?;
        let start = range.start as u32;
        Some(if start == last {
            Self::Const(U256Imm::new(U256::from(last), interner))
        } else {
            Self::Range((start..=last).into())
        })
    }

    /// Returns the range of an inline constant or a range.
    pub(crate) fn bounds(self) -> Option<RangeInclusive<u32>> {
        match self {
            Self::Const(v) => v.get_inline().map(|v| (v..=v).into()),
            Self::Range(range) => Some(range),
            Self::ConstSet(_) | Self::Top => None,
        }
    }

    /// Returns the value on an edge where it is at most `bound`, which must be an inline
    /// immediate.
    fn at_most(self, bound: u32, interner: &mut U256Interner) -> Self {
        let (start, last) = match self.bounds() {
            _ if bound == 0 => (0, 0),
            Some(range) if range.start <= bound => (range.start, range.last.min(bound)),
            None if self == Self::Top => (0, bound),
            _ => return self,
        };
        Self::range((u64::from(start)..=u64::from(last)).into(), interner).unwrap()
    }
}

/// Constant-set interner used by the abstract interpreter.
struct ConstSetInterner {
    interner: Interner<ConstSetIdx, Box<[U256Imm]>>,
    /// `JUMPDEST` map: sets only keep valid jump targets.
    jumpdests: JumpTable,
    /// Number of constants merged by [`Self::join`], for the fixpoint budget.
    merged: u64,
}

impl ConstSetInterner {
    fn new(jumpdests: JumpTable) -> Self {
        Self { interner: Interner::new(), jumpdests, merged: 0 }
    }

    /// Returns `true` if `value` may be part of a set.
    fn is_set_member(&self, value: AbsValue) -> bool {
        match value {
            AbsValue::Const(imm) => {
                imm.get_inline().is_some_and(|pc| self.jumpdests.is_valid(pc as usize))
            }
            AbsValue::ConstSet(_) => true,
            AbsValue::Range(..) | AbsValue::Top => false,
        }
    }

    /// Returns the constants in the given set.
    fn get(&self, idx: ConstSetIdx) -> &[U256Imm] {
        self.interner.get(idx)
    }

    /// Returns the set of known constants for an abstract value, or `None` if `Range` or `Top`.
    fn abs_const_set<'a>(&'a self, val: &'a AbsValue) -> Option<&'a [U256Imm]> {
        match val {
            AbsValue::Range(..) | AbsValue::Top => None,
            AbsValue::Const(v) => Some(std::slice::from_ref(v)),
            AbsValue::ConstSet(idx) => Some(self.get(*idx)),
        }
    }

    /// Interns a sorted, deduplicated set and returns the corresponding `AbsValue`.
    fn intern_set(&mut self, set: &[U256Imm]) -> AbsValue {
        match set.len() {
            0 => unreachable!("empty const set"),
            1 => AbsValue::Const(set[0]),
            _ => AbsValue::ConstSet(self.interner.intern(set.into())),
        }
    }

    /// Lattice join: two values merge to their least upper bound.
    ///
    /// A range only absorbs the values it contains, and `a` only grows from a constant to a range,
    /// so that repeated joins cannot widen a range step by step.
    fn join(&mut self, a: AbsValue, b: AbsValue) -> AbsValue {
        match (a, b) {
            _ if a == b => a,
            (AbsValue::Range(range), other) | (other, AbsValue::Range(range)) => {
                match other.bounds() {
                    Some(o) if range.contains(&o.start) && range.contains(&o.last) => {
                        AbsValue::Range(range)
                    }
                    _ => AbsValue::Top,
                }
            }
            // Sets only matter as jump targets, and any other member makes a jump unresolvable.
            _ if !self.is_set_member(a) || !self.is_set_member(b) => AbsValue::Top,
            _ => {
                let a = self.abs_const_set(&a).unwrap();
                let b = self.abs_const_set(&b).unwrap();
                // Sorted merge with dedup.
                let mut merged = SmallVec::<[U256Imm; 8]>::new();
                let (mut i, mut j) = (0, 0);
                while i < a.len() && j < b.len() {
                    match a[i].cmp(&b[j]) {
                        Ordering::Less => {
                            merged.push(a[i]);
                            i += 1;
                        }
                        Ordering::Greater => {
                            merged.push(b[j]);
                            j += 1;
                        }
                        Ordering::Equal => {
                            merged.push(a[i]);
                            i += 1;
                            j += 1;
                        }
                    }
                }
                merged.extend_from_slice(&a[i..]);
                merged.extend_from_slice(&b[j..]);
                self.merged += merged.len() as u64;
                self.intern_set(&merged)
            }
        }
    }
}

/// Maximum abstract stack depth tracked by the analysis: the EVM stack limit, so that the return
/// addresses of deep call chains stay known. A deeper stack overflows at runtime.
const MAX_ABS_STACK_DEPTH: usize = STACK_LIMIT;

/// Stack depth down to which [`Bytecode::context_key`] keys on the stack height and the positions
/// of jump targets.
///
/// Below it only the order of jump targets counts, without repeats, so that a loop that pushes a
/// runtime-dependent number of values, or a recursive call that pushes the same return address
/// at every level, reaches a fixed context instead of a new one per iteration.
const MAX_CONTEXT_DEPTH: usize = 128;

/// Maximum number of values in a range that a jump resolves to the jump targets in it.
const MAX_RANGE_JUMP_WIDTH: usize = 4096;

/// Maximum total number of jump targets that the range jumps of one bytecode resolve to.
///
/// Each resolved range jump becomes its own switch, so later range jumps stay dynamic to bound
/// compile time.
const MAX_RANGE_JUMP_TARGETS: usize = 1024;

/// Number of values below which [`Bytecode::enumerate_jump`] runs the rest of a block on each value
/// of a range. Ranges of two values are conditions, which no table indexes.
const MAX_ENUMERATED_VALUES: u32 = 256;

/// Maximum number of segments that may continue a stack, beyond which the stack is truncated
/// instead, so that the joins and the runs of a block on each segment stay cheap.
const MAX_SEGS_BELOW: usize = 64;

/// Memory words at constant offsets whose values are known, sorted by offset and disjoint.
///
/// Only words written by `MSTORE` or by a constant `CODECOPY` are tracked, so memory already
/// covers a word that `MLOAD` folds and the load needs no memory expansion. Every instruction
/// that writes memory clears the words it may overlap, and joins keep only the words known on
/// every path. The number of words is not capped: dropping a word to make room for another would
/// make the transfer non-monotone (a more precise state could forget a word that a less precise
/// one keeps), which lets the fixpoint reach a block that the committed jump resolutions no
/// longer justify.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MemWords {
    words: SmallVec<[(u32, AbsValue); 4]>,
    /// Whether no instruction has written memory yet, so every byte not in `words` is zero.
    pristine: bool,
}

impl MemWords {
    /// Returns the state at the start of the code, where memory is empty if the caller passes no
    /// stack.
    fn entry(stack_observed: bool) -> Self {
        Self { words: SmallVec::new(), pristine: !stack_observed }
    }

    /// Applies the memory effect of `opcode` with the given `inputs` (TOS last), where `code` is
    /// the code being analyzed.
    ///
    /// Returns the known value loaded by an `MLOAD`.
    fn transfer(
        &mut self,
        opcode: u8,
        inputs: &[AbsValue],
        code: &[u8],
        interner: &mut U256Interner,
    ) -> Option<AbsValue> {
        // Offsets and lengths that fit in an inline immediate, and their bounds; larger ones are
        // unknown.
        let imm = |depth: usize| inputs[inputs.len() - 1 - depth].as_const()?.get_inline();
        let bounds = |depth: usize| inputs[inputs.len() - 1 - depth].bounds();
        let len = |depth: usize| bounds(depth).map(|len| len.last);
        match opcode {
            op::MLOAD => {
                let offset = imm(0)?;
                let i = self.words.binary_search_by_key(&offset, |&(o, _)| o).ok()?;
                return Some(self.words[i].1);
            }
            op::MSTORE => {
                let value = inputs[inputs.len() - 2];
                self.clobber(bounds(0), Some(32));
                if let Some(offset) = imm(0)
                    && value != AbsValue::Top
                {
                    self.set(offset, value);
                }
            }
            op::MSTORE8 => self.clobber(bounds(0), Some(1)),
            op::CODECOPY => {
                let word = self.codecopy_word(imm(0), imm(1), imm(2), code, interner);
                self.clobber(bounds(0), len(2));
                if let Some((offset, value)) = word {
                    self.set(offset, AbsValue::Const(U256Imm::new(value, interner)));
                }
            }
            op::CALLDATACOPY | op::RETURNDATACOPY | op::MCOPY => self.clobber(bounds(0), len(2)),
            op::EXTCODECOPY => self.clobber(bounds(1), len(3)),
            op::CALL | op::CALLCODE => self.clobber(bounds(5), len(6)),
            op::DELEGATECALL | op::STATICCALL => self.clobber(bounds(4), len(5)),
            _ => {}
        }
        None
    }

    /// Returns the word that a `CODECOPY` of `len` bytes at `src` to `dest` leaves at the end of
    /// its destination, where memory expansion covers it, if the rest of that word is known: a
    /// tracked word at the same offset, or zeros when memory is `pristine`.
    fn codecopy_word(
        &self,
        dest: Option<u32>,
        src: Option<u32>,
        len: Option<u32>,
        code: &[u8],
        interner: &U256Interner,
    ) -> Option<(u32, U256)> {
        let (dest, src, len) = (dest?, src? as usize, len?);
        let offset = dest.checked_add(len)?.checked_sub(32).filter(|_| (1..=32).contains(&len))?;
        let len = len as usize;
        let base = match self.words.binary_search_by_key(&offset, |&(o, _)| o) {
            Ok(i) => self.words[i].1.as_const()?.get(interner),
            Err(_) if self.pristine => U256::ZERO,
            Err(_) => return None,
        };
        let mut word = base.to_be_bytes::<32>();
        let copied = &mut word[32 - len..];
        copied.fill(0);
        // Bytes past the end of the code are zero.
        if let Some(code) = code.get(src..) {
            let n = len.min(code.len());
            copied[..n].copy_from_slice(&code[..n]);
        }
        Some((offset, U256::from_be_bytes(word)))
    }

    /// Records the known `value` of the word at `offset`.
    fn set(&mut self, offset: u32, value: AbsValue) {
        let i = self.words.partition_point(|&(o, _)| o < offset);
        self.words.insert(i, (offset, value));
    }

    /// Forgets every word that a write of at most `len` bytes at an offset in `offset` may
    /// overlap, and clears `pristine`.
    fn clobber(&mut self, offset: Option<RangeInclusive<u32>>, len: Option<u32>) {
        self.pristine = false;
        match (offset, len) {
            (_, Some(0)) => {}
            (Some(offset), len) => {
                // Inline immediates are below 2^31, so these sums cannot overflow.
                let end = len.map_or(u32::MAX, |len| offset.last + len);
                self.words.retain(|&mut (o, _)| o + 32 <= offset.start || end <= o);
            }
            (None, _) => self.words.clear(),
        }
    }

    /// Keeps the words known in both states, joining their values. Returns `true` if `self`
    /// changed.
    fn join(&mut self, other: &Self, sets: &mut ConstSetInterner) -> bool {
        let len = self.words.len();
        let mut changed = self.pristine && !other.pristine;
        self.pristine &= other.pristine;
        self.words.retain(|(offset, value)| {
            let Ok(i) = other.words.binary_search_by_key(offset, |&(o, _)| o) else { return false };
            let joined = sets.join(*value, other.words[i].1);
            changed |= joined != *value;
            *value = joined;
            joined != AbsValue::Top
        });
        changed || self.words.len() != len
    }
}

/// Abstract state at the entry of a block.
#[derive(Clone, Debug)]
enum BlockState {
    /// Block has not been reached yet.
    Bottom,
    /// Block has been reached with a known stack state (top-aligned).
    Known {
        stack: Vec<AbsValue>,
        /// Whether entries below `stack` were dropped by a join or by [`MAX_ABS_STACK_DEPTH`]
        /// clamping, so that running out of tracked entries is not a stack underflow.
        truncated: bool,
        /// The segments, one of which continues the stack below `stack`, sorted. Empty when the
        /// stack ends at `stack` or is truncated.
        below: SmallVec<[Seg; 2]>,
        /// Known memory words.
        mem: MemWords,
    },
}

impl BlockState {
    /// Join another incoming state into this one. Returns `true` if the state changed.
    ///
    /// When stack heights differ, the stacks are top-aligned and the entries below the shorter
    /// one are dropped, marking the state truncated. This handles the common Solidity dispatch
    /// pattern where the "no selector match" fallthrough leaves an extra item on the stack that
    /// the fallback ignores. Stacks of the same height that both continue in segments keep the
    /// segments of both.
    fn join(
        &mut self,
        incoming: &[AbsValue],
        incoming_truncated: bool,
        incoming_below: &[Seg],
        incoming_mem: &MemWords,
        sets: &mut ConstSetInterner,
    ) -> bool {
        let incoming_truncated = incoming_truncated || incoming.len() > MAX_ABS_STACK_DEPTH;
        match self {
            Self::Bottom => {
                let start = incoming.len().saturating_sub(MAX_ABS_STACK_DEPTH);
                *self = Self::Known {
                    stack: incoming[start..].to_vec(),
                    truncated: incoming_truncated,
                    below: if incoming_truncated { SmallVec::new() } else { incoming_below.into() },
                    mem: incoming_mem.clone(),
                };
                true
            }
            Self::Known { stack: existing, truncated, below, mem } => {
                let mut changed = mem.join(incoming_mem, sets);

                // Keep only the entries that both stacks have, so that joins never grow the
                // stack.
                let join_len = existing.len().min(incoming.len());
                if !*truncated {
                    let lost = incoming_truncated
                        || incoming.len() != existing.len()
                        || incoming_below.is_empty() != below.is_empty();
                    for &seg in incoming_below {
                        if let Err(i) = below.binary_search(&seg) {
                            below.insert(i, seg);
                            changed = true;
                        }
                    }
                    if lost || below.len() > MAX_SEGS_BELOW {
                        *truncated = true;
                        below.clear();
                        changed = true;
                    }
                }
                if existing.len() > join_len {
                    existing.drain(..existing.len() - join_len);
                    changed = true;
                }

                // Join element-wise, top-aligned. Both stacks have their top at the end.
                let incoming_top = &incoming[incoming.len() - join_len..];
                for (value, &inc) in existing.iter_mut().zip(incoming_top) {
                    if *value == inc {
                        continue;
                    }
                    let joined = sets.join(*value, inc);
                    if joined != *value {
                        *value = joined;
                        changed = true;
                    }
                }

                changed
            }
        }
    }
}

/// How [`Bytecode::run_fixpoint`] splits block states into contexts.
///
/// Splitting keeps the frames of different call chains apart: a context-insensitive join of
/// the states that reach an internal function's return site mixes the frames of all its
/// callers, losing the return addresses deeper in the stack.
#[derive(Clone, Copy, Debug)]
enum ContextMode {
    /// Key on the tracked stack shape, every constant on the tracked stack that is a valid jump
    /// target, such as the return addresses of the whole call chain, as described at
    /// [`MAX_CONTEXT_DEPTH`], and the known memory words that are valid jump targets.
    ///
    /// Old Vyper saves return addresses in memory, and saves and restores the memory frame of a
    /// caller on the stack in loops around a call. The memory words keep the callers of a
    /// function apart, and the stack shape keeps the iterations of these loops apart, so that
    /// their memory offsets stay constant.
    Full,
    /// Like [`Full`](Self::Full), but the stacks of recursive calls continue in segments, as the
    /// module docs describe, cut at [`Bytecode::recursion_cut`].
    Folded,
    /// Key on the tracked stack shape and the topmost valid jump target constant.
    Coarse,
    /// One state per block.
    Insensitive,
}

impl ContextMode {
    /// Returns the budget of a fixpoint attempt in this mode.
    fn budget(self, num_blocks: usize, num_insts: usize) -> Budget {
        match self {
            Self::Full | Self::Folded => Budget {
                block_parts: 8192,
                parts: (num_blocks * 32).min(1 << 17),
                pops: num_blocks * 128,
                pop_work: 64,
                work: num_insts as u64 * 320,
            },
            Self::Coarse => Budget {
                block_parts: 128,
                parts: (num_blocks * 8).min(1 << 15),
                pops: num_blocks * 32,
                pop_work: 0,
                work: num_insts as u64 * 64,
            },
            Self::Insensitive => Budget {
                block_parts: 1,
                parts: num_blocks,
                pops: num_blocks * 8,
                pop_work: 0,
                work: num_insts as u64 * 64,
            },
        }
    }
}

/// Limits of a fixpoint attempt. Exceeding any of them abandons the attempt.
struct Budget {
    /// Partitions per block.
    block_parts: usize,
    /// Partitions in total.
    parts: usize,
    /// Worklist pops.
    pops: usize,
    /// Work of a worklist pop of a state that continues in segments, besides its instructions.
    /// Segments leave many pops of short stacks, which then cost about as much as the few pops
    /// of long stacks without them.
    pop_work: u64,
    /// Worklist pops, interpreted instructions, propagated edges, joined stack entries, and
    /// merged set constants.
    work: u64,
}

oxc_index::define_nonmax_u32_index_type! {
    /// A block state in one context.
    struct Part;
}

oxc_index::define_nonmax_u32_index_type! {
    /// A stack segment that continues the stacks of block states below their tracked entries.
    struct Seg;
}

/// How [`Bytecode::interpret_inst`] records snapshots.
enum Record<'a> {
    Skip,
    Overwrite,
    /// Join with the recorded values.
    Join(&'a mut ConstSetInterner),
}

/// Result of [`Bytecode::run_fixpoint`].
struct Fixpoint {
    /// Blocks reached by the fixpoint.
    reached: BitVec,
    /// Discovered dynamic-jump target edges per block.
    discovered: IndexVec<Block, SmallVec<[Block; 4]>>,
    /// Constant sets referenced by the snapshots.
    const_sets: ConstSetInterner,
    /// What enumerating ranges found for the dynamic jump that ends each block.
    enumerated: IndexVec<Block, Enumerated>,
    converged: bool,
}

/// What [`Bytecode::enumerate_jump`] found over all states of the dynamic jump that ends a block.
#[derive(Clone, Copy, Default)]
struct Enumerated {
    /// Whether a state enumerated the range of its jump operand.
    done: bool,
    /// Whether the operand may be an invalid jump target in some state.
    invalid: bool,
    /// Whether some state has an operand that is neither resolvable nor enumerable.
    failed: bool,
}

/// Scratch stacks of [`Bytecode::enumerate_jump`].
#[derive(Default)]
struct EnumerateBuf {
    stack: Vec<AbsValue>,
    mem: MemWords,
    /// The state after the instruction that leaves the range.
    split_stack: Vec<AbsValue>,
    split_mem: MemWords,
}

/// Block states of [`Bytecode::run_fixpoint`], one per context of each block.
struct Parts {
    states: IndexVec<Part, (Block, BlockState)>,
    /// The part of each context of each block.
    by_key: IndexVec<Block, FxHashMap<u64, Part>>,
    /// Parts to visit, in code order, which mostly follows the CFG, so that a state is complete
    /// before it propagates.
    worklist: BinaryHeap<Reverse<(Block, Part)>>,
    queued: BitVec,
}

impl Parts {
    /// Joins a state into the part of context `key` of `bid`, queueing the part if it changes.
    ///
    /// Returns `false` if a new part would exceed `budget`.
    #[expect(clippy::too_many_arguments)]
    fn join(
        &mut self,
        bid: Block,
        key: u64,
        stack: &[AbsValue],
        truncated: bool,
        below: &[Seg],
        mem: &MemWords,
        sets: &mut ConstSetInterner,
        budget: &Budget,
    ) -> bool {
        let part = match self.by_key[bid].get(&key) {
            Some(&part) => part,
            None => {
                if self.by_key[bid].len() == budget.block_parts || self.states.len() == budget.parts
                {
                    return false;
                }
                let part = self.states.push((bid, BlockState::Bottom));
                self.queued.push(false);
                self.by_key[bid].insert(key, part);
                part
            }
        };
        if self.states[part].1.join(stack, truncated, below, mem, sets) {
            self.queue(part);
        }
        true
    }

    fn queue(&mut self, part: Part) {
        if !self.queued[part.index()] {
            self.queued.set(part.index(), true);
            self.worklist.push(Reverse((self.states[part].0, part)));
        }
    }
}

/// Stack segments of [`Bytecode::run_fixpoint`], one per context.
#[derive(Default)]
struct Segs {
    /// The entries of each segment, as a state without memory words.
    states: IndexVec<Seg, BlockState>,
    by_key: FxHashMap<u64, Seg>,
    /// The parts whose blocks read into each segment, to visit again when it changes.
    users: IndexVec<Seg, Vec<Part>>,
}

oxc_index::define_nonmax_u32_index_type! {
    /// A block index in the CFG.
    pub(crate) struct Block;
}
crate::bytecode::impl_index_display!(Block, "bb{}");

/// FIFO worklist with deduplication.
struct Worklist {
    queue: VecDeque<Block>,
    in_queue: BitVec,
}

impl Worklist {
    fn new(size: usize) -> Self {
        Self { queue: VecDeque::new(), in_queue: BitVec::repeat(false, size) }
    }

    fn push(&mut self, id: Block) {
        let idx = id.index();
        if !self.in_queue[idx] {
            self.in_queue.set(idx, true);
            self.queue.push_back(id);
        }
    }

    fn pop(&mut self) -> Option<Block> {
        let id = self.queue.pop_front()?;
        self.in_queue.set(id.index(), false);
        Some(id)
    }
}

/// A basic block in the CFG.
pub(crate) struct BlockData {
    /// Instruction index range (exclusive end). All instructions in this range are live.
    pub(crate) insts: Range<Inst>,
    /// Predecessor block IDs.
    pub(crate) preds: SmallVec<[Block; 4]>,
    /// Successor block IDs.
    pub(crate) succs: SmallVec<[Block; 4]>,
}

impl BlockData {
    #[inline]
    pub(crate) fn terminator(&self) -> Inst {
        self.insts.end - 1
    }

    /// Returns the instruction range as `Range<usize>` for indexing into raw arrays.
    #[inline]
    pub(crate) fn insts(
        &self,
    ) -> impl DoubleEndedIterator<Item = Inst> + ExactSizeIterator + use<> {
        (self.insts.start.index()..self.insts.end.index()).map(Inst::new)
    }
}

/// Resolved jump including compile-time condition information.
#[derive(Clone, Debug)]
struct JumpResolution {
    target: JumpTarget,
    condition: JumpCondition,
}

/// Resolved jump target kind after fixpoint.
#[derive(Clone, Debug)]
enum JumpTarget {
    /// Not yet observed.
    Bottom,
    /// One or more known constant target instruction indices.
    Resolved(SmallVec<[Inst; 4]>),
    /// Known constant but invalid target.
    Invalid,
    /// Unknown target.
    Top,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum JumpCondition {
    #[default]
    Unknown,
    AlwaysTrue,
    AlwaysFalse,
}

/// What the condition of a block's terminating `JUMPI` shows about its successors.
struct Branch {
    /// The fall-through block.
    fallthrough: Option<Block>,
    /// Whether the jump may also target the fall-through block.
    fallthrough_is_target: bool,
    /// The edge, `true` for the jump, on which the condition shows a value to be at most a bound,
    /// the depth of that value in the exit stack, with zero at the top, and the bound.
    bound: Option<(bool, usize, u32)>,
}

impl JumpResolution {
    const fn new(target: JumpTarget) -> Self {
        Self { target, condition: JumpCondition::Unknown }
    }

    const fn bottom() -> Self {
        Self::new(JumpTarget::Bottom)
    }

    const fn invalid() -> Self {
        Self::new(JumpTarget::Invalid)
    }

    const fn top() -> Self {
        Self::new(JumpTarget::Top)
    }

    const fn resolved(targets: SmallVec<[Inst; 4]>) -> Self {
        Self::new(JumpTarget::Resolved(targets))
    }

    /// Creates a resolved target with a single constant.
    fn single(inst: Inst) -> Self {
        Self::resolved(SmallVec::from_elem(inst, 1))
    }

    /// Returns the single resolved target, if exactly one.
    fn as_single(&self) -> Option<Inst> {
        match &self.target {
            JumpTarget::Resolved(targets) if targets.len() == 1 => Some(targets[0]),
            _ => None,
        }
    }

    const fn with_condition(mut self, condition: JumpCondition) -> Self {
        self.condition = condition;
        self
    }

    const fn is_top(&self) -> bool {
        matches!(self.target, JumpTarget::Top)
    }

    const fn is_resolved(&self) -> bool {
        matches!(self.target, JumpTarget::Resolved(_) | JumpTarget::Invalid)
    }
}

/// CFG for abstract interpretation.
#[derive(Default)]
pub(crate) struct Cfg {
    pub(crate) blocks: IndexVec<Block, BlockData>,
    /// Maps instruction index to block ID. `None` for dead-code instructions.
    pub(crate) inst_to_block: IndexVec<Inst, Option<Block>>,
}

impl Bytecode<'_> {
    /// Ensures `self.snapshots` is sized for the current instruction count.
    fn init_snapshots(&mut self) {
        let n = self.insts.len();
        self.snapshots.inputs.resize(n, SmallVec::new());
        self.snapshots.outputs.resize(n, None);
    }

    /// Block-local jump resolution: interpret each block independently to discover
    /// jumps resolvable from block-local computation alone.
    ///
    /// Also initializes `self.snapshots`.
    #[instrument(name = "local_jumps", level = "debug", skip_all)]
    #[inline(never)]
    pub(crate) fn block_analysis_local(&mut self) {
        self.init_snapshots();

        let empty_sets = ConstSetInterner::new(JumpTable::default());
        let mut resolved = Vec::new();
        let mut stack = Vec::new();
        let trace_logs = enabled!(tracing::Level::TRACE);

        for bid in self.cfg.blocks.indices() {
            let block = &self.cfg.blocks[bid];

            // Compute required entry depth for this block using stack section analysis.
            let section =
                StackSection::from_stack_io(block.insts().map(|i| self.insts[i].stack_io()));

            // Interpret the block with `Top` as inputs.
            stack.clear();
            stack.resize(section.inputs as usize, AbsValue::Top);
            if !self.interpret_block(block.insts(), &mut stack, false, &mut MemWords::default()) {
                continue;
            }

            // Check if the block's terminator is an unresolved jump.
            // We interpret every block to initialize `snapshots`, so we check this after.
            let block = &self.cfg.blocks[bid];
            let term_inst = block.terminator();
            let term = &self.insts[term_inst];
            if !term.is_jump() || term.flags.contains(InstFlags::STATIC_JUMP) {
                continue;
            }

            // Ranges never resolve to a single target, so they need no budget here.
            let target = self.resolve_jump(term_inst, &empty_sets, &mut 0);
            let Some(target_inst) = target.as_single() else { continue };

            // Log non-adjacent resolutions (not simple PUSH+JUMP).
            if trace_logs
                && let is_adjacent = (term_inst > 0 && {
                    let prev_inst = term_inst - 1;
                    let prev = &self.insts[prev_inst];
                    matches!(prev.opcode, op::PUSH0..=op::PUSH32)
                        && !prev.is_dead_code()
                        && block.insts.contains(&prev_inst)
                })
                && !is_adjacent
            {
                trace!(%term_inst, %target_inst, pc = self.pc(term_inst), "resolved non-adjacent jump");
            }

            resolved.push((term_inst, target));
        }

        let newly_resolved = self.commit_resolved_jumps(&resolved);
        debug!(newly_resolved, "finished");
        self.recompute_has_dynamic_jumps();
    }

    /// Runs abstract stack interpretation to resolve additional jump targets.
    ///
    /// Also computes and stores per-instruction stack snapshots for constant propagation.
    #[instrument(name = "ba", level = "debug", skip_all)]
    #[inline(never)]
    pub(crate) fn block_analysis(&mut self, local_snapshots: &Snapshots) {
        self.init_snapshots();
        let resolved = self.run_abstract_interp(local_snapshots);
        let newly_resolved = self.commit_resolved_jumps(&resolved);
        debug!(newly_resolved, "resolved jumps");
        self.recompute_has_dynamic_jumps();
    }

    /// Commits resolved jump targets by setting flags and data on the corresponding instructions.
    ///
    /// Returns the number of newly resolved jumps.
    fn commit_resolved_jumps(&mut self, resolved: &[(Inst, JumpResolution)]) -> u32 {
        let has_top_jump = resolved.iter().any(|(_, target)| target.is_top());

        let mut newly_resolved = 0u32;
        for &(jump_inst, ref target) in resolved {
            let was_static = self.insts[jump_inst].flags.contains(InstFlags::STATIC_JUMP);
            if was_static && target.condition == JumpCondition::Unknown {
                continue;
            }

            if target.condition != JumpCondition::Unknown {
                self.insts[jump_inst].flags |= InstFlags::CONST_JUMP_CONDITION;
            }

            match &target.target {
                JumpTarget::Resolved(targets) => {
                    self.insts[jump_inst]
                        .flags
                        .remove(InstFlags::INVALID_JUMP | InstFlags::MULTI_JUMP);
                    if target.condition != JumpCondition::AlwaysFalse {
                        for &target_inst in targets {
                            debug_assert_eq!(
                                self.insts[target_inst].opcode,
                                op::JUMPDEST,
                                "block_analysis resolved to non-JUMPDEST"
                            );
                            self.insts[target_inst].set_jumpdest_reachable();
                        }
                    }
                    if targets.len() == 1 {
                        self.multi_jump_targets.remove(&jump_inst);
                        self.insts[jump_inst].flags |= InstFlags::STATIC_JUMP;
                        self.insts[jump_inst].set_static_jump_target(targets[0]);
                        trace!(%jump_inst, target_inst = %targets[0], "resolved jump");
                    } else {
                        self.insts[jump_inst].flags |=
                            InstFlags::STATIC_JUMP | InstFlags::MULTI_JUMP;
                        self.multi_jump_targets.insert(jump_inst, targets.clone());
                        trace!(%jump_inst, n_targets = targets.len(), "resolved multi-target jump");
                    }
                    if !was_static {
                        newly_resolved += 1;
                    }
                }
                JumpTarget::Invalid => {
                    self.multi_jump_targets.remove(&jump_inst);
                    self.insts[jump_inst].flags |= InstFlags::STATIC_JUMP | InstFlags::INVALID_JUMP;
                    self.insts[jump_inst].flags.remove(InstFlags::MULTI_JUMP);
                    if !was_static {
                        newly_resolved += 1;
                    }
                    trace!(%jump_inst, "resolved invalid jump");
                }
                JumpTarget::Bottom if !has_top_jump => {
                    // Truly unreachable: no unresolved jumps remain, so this
                    // code cannot be reached at runtime. Mark as invalid.
                    self.multi_jump_targets.remove(&jump_inst);
                    self.insts[jump_inst].flags |= InstFlags::STATIC_JUMP | InstFlags::INVALID_JUMP;
                    self.insts[jump_inst].flags.remove(InstFlags::MULTI_JUMP);
                    if !was_static {
                        newly_resolved += 1;
                    }
                    trace!(%jump_inst, "unreachable jump");
                }
                JumpTarget::Bottom => {
                    // Unreachable according to the analysis, but there are
                    // unresolved (Top) jumps that might reach this code at
                    // runtime. Leave as-is.
                    trace!(%jump_inst, "unreachable jump (not marking, has_top_jump)");
                }
                JumpTarget::Top => {
                    trace!(%jump_inst, "unresolved jump (Top)");
                }
            }
        }
        self.recompute_reachable_jumpdests();
        newly_resolved
    }

    /// Refresh reachability and CFG-derived state after control-flow changes.
    #[instrument(level = "debug", name = "re_cfg", skip_all)]
    #[inline(never)]
    pub(crate) fn refresh_cfg(&mut self) {
        self.recompute_has_dynamic_jumps();
        self.recompute_reachable_jumpdests(); // Depends on has_dynamic_jumps
        self.mark_dead_code(); // Depends on reachable jumpdests
        self.compress_redirects();
        self.rebuild_cfg();
    }

    #[instrument(level = "debug", name = "redirects", skip_all)]
    fn compress_redirects(&mut self) {
        // Compress redirect chains so that earlier redirects (e.g. t2 -> t1) are
        // updated when their target gets deduped in a later round (t1 -> t0).
        // Without this, rebuild_cfg resolves edges only one hop, leaving stale
        // intermediate targets in DedupKey.succs (preventing valid merges) and
        // in translate.rs inst_entries (causing InvalidJump on valid bytecode).
        for inst in self.redirects.keys().copied().collect::<Vec<_>>() {
            let mut target = self.redirects[&inst];
            let original = target;
            while let Some(&next) = self.redirects.get(&target) {
                target = next;
            }
            if target != original {
                self.redirects.insert(inst, target);
            }
        }
    }

    /// Rebuild the basic-block CFG from the current instruction state.
    #[instrument(level = "debug", name = "cfg", skip_all)]
    fn rebuild_cfg(&mut self) {
        let finish_block = |cfg: &mut Cfg, start: usize, end: usize| {
            debug_assert!(start < end, "empty block range: {start}..{end}");
            let bid = cfg.blocks.push(BlockData {
                insts: Inst::new(start)..Inst::new(end),
                preds: SmallVec::new(),
                succs: SmallVec::new(),
            });
            for i in start..end {
                cfg.inst_to_block[Inst::new(i)] = Some(bid);
            }
        };

        let n = self.insts.len();
        assert_ne!(n, 0, "insts should never be empty");

        let cfg = &mut self.cfg;
        cfg.blocks.clear();
        cfg.inst_to_block.clear();

        // Identify block leaders.
        let mut is_leader: BitVec = BitVec::repeat(false, n);
        is_leader.set(0, true);

        for (i, inst) in self.insts.raw.iter().enumerate() {
            // Reachable JUMPDESTs must be leaders even when dead (e.g. deduped).
            // Without this, `pending_leader` in the block-building loop below never
            // fires for a dead JUMPDEST, and the preceding live block absorbs the
            // next alive instruction — potentially a diverging terminator that
            // poisons DSE.
            if inst.is_reachable_jumpdest(self.has_dynamic_jumps) {
                is_leader.set(i, true);
            }
            if inst.is_dead_code() {
                continue;
            }
            if (inst.is_branching() || inst.is_diverging()) && i + 1 < n {
                is_leader.set(i + 1, true);
            }
        }

        // Build blocks. Dead instructions are skipped; `current_end` tracks the
        // exclusive end of live instructions so block ranges never include dead code.
        cfg.inst_to_block.resize(n, None);
        let mut current_start = None;
        let mut current_end = 0;
        let mut pending_leader = false;

        for i in 0..n {
            if self.insts.raw[i].is_dead_code() {
                // Propagate leader marks past dead code so that the next alive
                // instruction starts a new block. Without this, a block ending
                // in JUMPI whose fall-through was deduped (dead) would merge
                // with the next alive instruction, potentially changing the
                // block's terminator and poisoning DSE liveness.
                pending_leader |= is_leader[i];
                continue;
            }

            if is_leader[i] || pending_leader || current_start.is_none() {
                if let Some(start) = current_start {
                    finish_block(cfg, start, current_end);
                }
                current_start = Some(i);
            }
            pending_leader = false;
            current_end = i + 1;
        }

        // Close the last block.
        if let Some(start) = current_start {
            finish_block(cfg, start, current_end);
        }

        // Build edges based on known control flow.
        for bid in cfg.blocks.indices() {
            let term = &self.insts[cfg.blocks[bid].terminator()];

            // Resolve a target instruction through redirects to a CFG block.
            let resolve = |target: Inst| -> Option<Block> {
                let target = self.redirects.get(&target).copied().unwrap_or(target);
                cfg.inst_to_block.get(target).copied().flatten()
            };

            // Fallthrough edge.
            if term.can_fall_through()
                && let Some(target_block) = resolve(cfg.blocks[bid].terminator() + 1)
            {
                cfg.blocks[target_block].preds.push(bid);
                cfg.blocks[bid].succs.push(target_block);
            }

            // Jump edges: static single-target, or multi-jump.
            let term_inst = cfg.blocks[bid].terminator();
            if term.flags.contains(InstFlags::MULTI_JUMP)
                && let Some(targets) = self.multi_jump_targets.get(&term_inst)
            {
                for &t in targets {
                    if let Some(target_block) = resolve(t)
                        && !cfg.blocks[bid].succs.contains(&target_block)
                    {
                        cfg.blocks[target_block].preds.push(bid);
                        cfg.blocks[bid].succs.push(target_block);
                    }
                }
            } else if term.is_static_jump()
                && !term.flags.contains(InstFlags::INVALID_JUMP)
                && let Some(target_block) = resolve(term.static_jump_target())
                && !cfg.blocks[bid].succs.contains(&target_block)
            {
                cfg.blocks[target_block].preds.push(bid);
                cfg.blocks[bid].succs.push(target_block);
            }
        }

        assert_ne!(cfg.blocks.len(), 0, "should always build at least one block");
    }

    /// Run worklist-based abstract interpretation over the CFG.
    ///
    /// Returns a list of (jump_inst, resolved_target) pairs.
    /// Stack snapshots are recorded into `self.snapshots` once the fixpoint converges.
    fn run_abstract_interp(&mut self, local_snapshots: &Snapshots) -> Vec<(Inst, JumpResolution)> {
        // Collect unresolved jumps, plus static JUMPI instructions whose condition
        // may become constant only after CFG fixpoint.
        let mut jump_insts: Vec<Inst> = Vec::new();
        for (i, inst) in self.insts.iter_enumerated() {
            if inst.is_jump()
                && !inst.is_dead_code()
                && (!inst.flags.contains(InstFlags::STATIC_JUMP)
                    || (inst.opcode == op::JUMPI && !inst.has_const_jumpi_condition()))
            {
                jump_insts.push(i);
            }
        }

        // Try the most precise contexts first, falling back to coarser ones when an attempt
        // exceeds its budget. Recursive calls need a new context at every level, so when `Full`
        // runs out of budget or leaves a jump unresolved, try folding them. Folds also join the
        // frames of calls that share a jump target without a recursion, so their result
        // only counts when it resolves every jump.
        let has_top = |targets: &[(Inst, JumpResolution)]| targets.iter().any(|(_, t)| t.is_top());
        let mut fixpoint = self.run_fixpoint(ContextMode::Full);
        let mut jump_targets = self.resolve_jumps(&jump_insts, &fixpoint);
        if !fixpoint.converged || has_top(&jump_targets) {
            let full_snapshots = std::mem::replace(&mut self.snapshots, local_snapshots.clone());
            let folded = self.run_fixpoint(ContextMode::Folded);
            let folded_targets = self.resolve_jumps(&jump_insts, &folded);
            if folded.converged && !has_top(&folded_targets) {
                (fixpoint, jump_targets) = (folded, folded_targets);
            } else {
                self.snapshots = full_snapshots;
                for mode in [ContextMode::Coarse, ContextMode::Insensitive] {
                    if fixpoint.converged {
                        break;
                    }
                    fixpoint = self.run_fixpoint(mode);
                    jump_targets = self.resolve_jumps(&jump_insts, &fixpoint);
                }
            }
        }
        let has_top_jump = has_top(&jump_targets);
        let Fixpoint { reached, discovered: discovered_edges, converged, .. } = fixpoint;

        // Invalidate resolutions that may be unsound due to incomplete analysis.
        // When the fixpoint didn't converge, partially-discovered ConstSets may be
        // incomplete, so we must conservatively invalidate them too.
        if has_top_jump || !converged {
            self.invalidate_suspect_jumps(
                &mut jump_targets,
                &reached,
                &discovered_edges,
                local_snapshots,
            );
        } else {
            // Blocks the fixpoint never reached are unreachable, even when the static jumps of
            // other such blocks, such as a loop's back edge, target them.
            for bid in reached.iter_zeros().map(Block::new) {
                for i in self.cfg.blocks[bid].insts() {
                    self.insts[i].flags |= InstFlags::DEAD_CODE;
                }
            }
        }

        jump_targets
    }

    /// Resolves each jump in `jump_insts` from its snapshot operand after `fixpoint`.
    fn resolve_jumps(
        &self,
        jump_insts: &[Inst],
        fixpoint: &Fixpoint,
    ) -> Vec<(Inst, JumpResolution)> {
        let Fixpoint { reached, discovered, const_sets, enumerated, converged } = fixpoint;
        let mut range_budget = MAX_RANGE_JUMP_TARGETS;
        jump_insts
            .iter()
            .map(|&jump_inst| {
                let bid = self.cfg.inst_to_block[jump_inst];
                // Blocks the fixpoint never reached keep their block-local snapshots.
                let mut target = if *converged && bid.is_some_and(|bid| !reached[bid.index()]) {
                    JumpResolution::bottom()
                } else {
                    self.resolve_jump(jump_inst, const_sets, &mut range_budget)
                };
                // The snapshot of a jump operand that was enumerated is too imprecise, but the
                // targets that the fixpoint discovered are all the jump can reach, as long as every
                // state resolved or enumerated its operand.
                if *converged
                    && target.is_top()
                    && let Some(bid) = bid
                    && let Enumerated { done: true, invalid, failed: false } = enumerated[bid]
                {
                    let mut targets: SmallVec<[Inst; 4]> =
                        discovered[bid].iter().map(|&tb| self.cfg.blocks[tb].insts.start).collect();
                    targets.sort_unstable();
                    // As for a range, a single target would skip the invalid jump check.
                    let count = targets.len();
                    if count > 1 && count <= range_budget {
                        range_budget -= count;
                    } else if count != 1 || invalid {
                        return (jump_inst, target);
                    }
                    target = JumpResolution::resolved(targets).with_condition(target.condition);
                }
                (jump_inst, target)
            })
            .collect()
    }

    fn resolve_jump(
        &self,
        jump_inst: Inst,
        const_sets: &ConstSetInterner,
        range_budget: &mut usize,
    ) -> JumpResolution {
        let condition = if self.insts[jump_inst].opcode == op::JUMPI {
            self.snapshots
                .input(jump_inst, 1)
                .map(|value| self.resolve_jump_condition(value, const_sets))
                .unwrap_or_default()
        } else {
            JumpCondition::Unknown
        };

        if condition == JumpCondition::AlwaysFalse {
            return JumpResolution::single(jump_inst + 1)
                .with_condition(JumpCondition::AlwaysFalse);
        }

        match self.snapshots.input(jump_inst, 0) {
            Some(operand) => self
                .resolve_jump_operand(operand, const_sets, range_budget)
                .with_condition(condition),
            None => {
                trace!(%jump_inst, pc = self.pc(jump_inst), "jump in unreached block");
                JumpResolution::bottom()
            }
        }
    }

    /// Resolves a jump target from the snapshot operand recorded during the fixpoint.
    ///
    /// A range resolves only if its targets fit in `range_budget`, which they then use up.
    fn resolve_jump_operand(
        &self,
        operand: AbsValue,
        const_sets: &ConstSetInterner,
        range_budget: &mut usize,
    ) -> JumpResolution {
        match operand {
            AbsValue::Const(imm) => {
                let val = imm.get(&self.u256_interner.borrow());
                match usize::try_from(val) {
                    Ok(target_pc) if self.is_valid_jump(target_pc) => {
                        JumpResolution::single(self.pc_to_inst(target_pc))
                    }
                    _ => JumpResolution::invalid(),
                }
            }
            AbsValue::ConstSet(set_idx) => {
                // Sets only hold valid jump targets, but stay unresolved if one ever doesn't: the
                // value might be any member of the set.
                let interner = self.u256_interner.borrow();
                let targets = const_sets.get(set_idx).iter().map(|imm| {
                    let pc = usize::try_from(imm.get(&interner)).ok()?;
                    self.is_valid_jump(pc).then(|| self.pc_to_inst(pc))
                });
                targets
                    .collect::<Option<_>>()
                    .map_or_else(JumpResolution::top, JumpResolution::resolved)
            }
            AbsValue::Range(range) => {
                match self.range_jumpdests(range).map(Iterator::collect::<SmallVec<_>>) {
                    Some(targets) if targets.is_empty() => JumpResolution::invalid(),
                    // A single target would become a static jump that skips the invalid jump check
                    // for the other values in the range, while a multi-target switch keeps it.
                    Some(targets) if targets.len() > 1 && targets.len() <= *range_budget => {
                        *range_budget -= targets.len();
                        JumpResolution::resolved(targets)
                    }
                    _ => JumpResolution::top(),
                }
            }
            AbsValue::Top => JumpResolution::top(),
        }
    }

    /// Returns the valid jump targets in `range`, or `None` if it is wider than
    /// [`MAX_RANGE_JUMP_WIDTH`].
    fn range_jumpdests(
        &self,
        range: RangeInclusive<u32>,
    ) -> Option<impl Iterator<Item = Inst> + '_> {
        let (first, last) = (range.start as usize, range.last as usize);
        // Clamped to the code, which the range may extend past.
        let end = (last + 1).min(self.jumpdests.len());
        let start = first.min(end);
        (last - first < MAX_RANGE_JUMP_WIDTH).then(|| {
            (start..end)
                .filter(move |&pc| self.jumpdests.is_valid(pc))
                .map(move |pc| self.pc_to_inst(pc))
        })
    }

    fn resolve_jump_condition(
        &self,
        condition: AbsValue,
        const_sets: &ConstSetInterner,
    ) -> JumpCondition {
        let consts = match condition {
            AbsValue::Const(imm) => Either::Left(std::iter::once(imm)),
            AbsValue::ConstSet(set_idx) => Either::Right(const_sets.get(set_idx).iter().copied()),
            AbsValue::Range(range) if range.start > 0 => return JumpCondition::AlwaysTrue,
            AbsValue::Range(..) | AbsValue::Top => return JumpCondition::Unknown,
        };
        let interner = self.u256_interner.borrow();
        let mut saw_zero = false;
        let mut saw_nonzero = false;
        for imm in consts {
            if imm.get(&interner).is_zero() {
                saw_zero = true;
            } else {
                saw_nonzero = true;
            }
        }
        match (saw_zero, saw_nonzero) {
            (true, false) => JumpCondition::AlwaysFalse,
            (false, true) => JumpCondition::AlwaysTrue,
            _ => JumpCondition::Unknown,
        }
    }

    fn local_jumpi_condition(&self, jump_inst: Inst, local_snapshots: &Snapshots) -> JumpCondition {
        if self.insts[jump_inst].opcode != op::JUMPI {
            return JumpCondition::Unknown;
        }
        let Some(condition) = local_snapshots.input(jump_inst, 1) else {
            return JumpCondition::Unknown;
        };
        let Some(imm) = condition.as_const() else {
            return JumpCondition::Unknown;
        };
        if imm.get(&self.u256_interner.borrow()).is_zero() {
            JumpCondition::AlwaysFalse
        } else {
            JumpCondition::AlwaysTrue
        }
    }

    /// Invalidates jump resolutions and operand snapshots that may be unsound due to
    /// unresolved `Top` jumps.
    ///
    /// When unresolved dynamic jumps remain, any reachable `JUMPDEST` block could
    /// potentially be reached by those jumps with an arbitrary stack state. This means
    /// resolutions and snapshots derived from the fixpoint may be based on incomplete
    /// information.
    ///
    /// The conservative rule: seed every reachable `JUMPDEST` block as suspect,
    /// propagate forward through the CFG and discovered edges, and invalidate all
    /// resolved jumps and operand snapshots in suspect blocks.
    fn invalidate_suspect_jumps(
        &mut self,
        jump_targets: &mut [(Inst, JumpResolution)],
        reached: &BitVec,
        discovered_edges: &IndexVec<Block, SmallVec<[Block; 4]>>,
        local_snapshots: &Snapshots,
    ) {
        let num_blocks = self.cfg.blocks.len();

        // Seed: every reachable JUMPDEST block is suspect when Top jumps exist.
        let mut suspect: BitVec = BitVec::repeat(false, num_blocks);
        for bid in self.cfg.blocks.indices() {
            if reached[bid.index()] && self.insts[self.cfg.blocks[bid].insts.start].is_jumpdest() {
                suspect.set(bid.index(), true);
            }
        }

        // Propagate suspect flag forward through the CFG.
        let mut propagate = Worklist::new(num_blocks);
        for bid in self.cfg.blocks.indices() {
            if suspect[bid.index()] {
                propagate.push(bid);
            }
        }
        while let Some(bid) = propagate.pop() {
            for &succ in self.cfg.blocks[bid].succs.iter().chain(discovered_edges[bid].iter()) {
                let si = succ.index();
                if !suspect[si] {
                    suspect.set(si, true);
                    propagate.push(succ);
                }
            }
        }

        for (inst, target) in jump_targets.iter_mut() {
            if let Some(bid) = self.cfg.inst_to_block[*inst]
                && suspect[bid.index()]
            {
                let condition = self.local_jumpi_condition(*inst, local_snapshots);
                if condition == JumpCondition::AlwaysFalse {
                    *target = JumpResolution::single(*inst + 1).with_condition(condition);
                    continue;
                }
                target.condition = condition;
                if target.is_resolved() {
                    *target = JumpResolution::top().with_condition(condition);
                }
            }
        }

        // Restore block-local snapshots in suspect blocks: the fixpoint may have recorded
        // precise values that are unsound because undiscovered edges (from Top jumps)
        // could bring in different stack states. However, block-local constants (computed
        // by block_analysis_local without depending on the incoming stack) are always valid.
        for bid in self.cfg.blocks.indices() {
            if !suspect[bid.index()] {
                continue;
            }
            self.snapshots.restore_from(self.cfg.blocks[bid].insts(), local_snapshots);
        }
    }

    /// Run a worklist-based fixpoint to compute abstract block states.
    ///
    /// Each block keeps one state per [context](Self::context_key) of the states reaching it, as
    /// chosen by `mode`. Snapshots join the values of all contexts.
    ///
    /// In [`ContextMode::Folded`], the stacks of recursive calls continue in segments. Segments
    /// with the same context share a state. A block that reads below its state's entries runs
    /// again on each segment below them, with the segment's entries prepended.
    ///
    /// Snapshots are only recorded once the fixpoint converges, so on non-convergence they keep
    /// the safe block-local values computed by `block_analysis_local`.
    fn run_fixpoint(&mut self, mode: ContextMode) -> Fixpoint {
        let num_blocks = self.cfg.blocks.len();
        let mut const_sets = ConstSetInterner::new(self.jumpdests.clone());
        let budget = mode.budget(num_blocks, self.insts.len());

        let mut parts = Parts {
            states: IndexVec::new(),
            by_key: IndexVec::from_vec(vec![FxHashMap::default(); num_blocks]),
            worklist: BinaryHeap::new(),
            queued: BitVec::new(),
        };
        let mut segs = Segs::default();

        // Entry block starts with an empty stack, or with unknown entries when the caller may
        // pass a stack.
        let no_mem = MemWords::default();
        let entry_mem = MemWords::entry(self.stack_observed());
        let entry = Block::new(0);
        parts.join(entry, 0, &[], self.stack_observed(), &[], &entry_mem, &mut const_sets, &budget);

        // Discovered dynamic-jump target edges per block.
        let mut discovered: IndexVec<Block, SmallVec<[Block; 4]>> =
            IndexVec::from_vec(vec![SmallVec::new(); num_blocks]);
        // The same edges, to deduplicate them in constant time: a shared return block can have
        // thousands of targets.
        let mut discovered_set = FxHashSet::default();
        // The parts in `segs.users`, to deduplicate them in constant time.
        let mut seg_users = FxHashSet::default();

        let mut pops = 0;
        let mut work = 0;
        let mut converged = true;

        // Reusable buffers to avoid per-iteration allocations.
        let mut stack_buf: Vec<AbsValue> = Vec::with_capacity(MAX_ABS_STACK_DEPTH);
        let mut below_buf = SmallVec::<[Seg; 2]>::new();
        let mut input_buf = Vec::new();
        let mut targets = SmallVec::<[Block; 4]>::new();
        let mut mem_buf = MemWords::default();
        let mut refined_buf = Vec::new();

        let branches: IndexVec<Block, Option<Branch>> =
            self.cfg.blocks.indices().map(|bid| self.branch(bid)).collect();
        let forwarders: BitVec =
            self.cfg.blocks.indices().map(|bid| self.is_forwarder(bid)).collect();
        let mut copies = vec![0; self.jumpdests.len()];
        // The blocks that end in a dynamic jump, whose operand ranges the fixpoint may enumerate.
        let dynamic_jumps: BitVec = self
            .cfg
            .blocks
            .iter()
            .map(|block| {
                let term = &self.insts[block.terminator()];
                term.is_jump() && !term.flags.contains(InstFlags::STATIC_JUMP)
            })
            .collect();
        let mut enumerated =
            IndexVec::<Block, _>::from_vec(vec![Enumerated::default(); num_blocks]);
        let mut enumerate_buf = EnumerateBuf::default();

        'fixpoint: while let Some(Reverse((_, pid))) = parts.worklist.pop() {
            parts.queued.set(pid.index(), false);
            pops += 1;
            if pops > budget.pops || work + const_sets.merged > budget.work {
                converged = false;
                break;
            }

            // Copy input state into reusable buffer.
            let (bid, state) = &parts.states[pid];
            let bid = *bid;
            stack_buf.clear();
            let mut truncated = match state {
                BlockState::Known { stack, truncated, below, mem } => {
                    stack_buf.extend_from_slice(stack);
                    below_buf.clone_from(below);
                    mem_buf.clone_from(mem);
                    *truncated
                }
                BlockState::Bottom => continue,
            };

            let insts = self.cfg.blocks[bid].insts();
            work += insts.len() as u64;
            if !below_buf.is_empty() {
                work += budget.pop_work;
            }
            let term_inst = self.cfg.blocks[bid].terminator();
            let mut operand = None;
            let mut condition = AbsValue::Top;
            // The narrowest range that an instruction of a dynamic jump's block leaves on the
            // stack.
            let dynamic_jump = dynamic_jumps[bid.index()];
            let mut narrowest: Option<(Inst, RangeInclusive<u32>)> = None;
            for i in insts {
                if i == term_inst {
                    operand = stack_buf.last().copied();
                    condition = stack_buf.iter().rev().nth(1).copied().unwrap_or_default();
                }
                // A host instruction may write any stack word, including the entries in segments.
                if !below_buf.is_empty() && self.insts[i].flags.contains(InstFlags::HOST) {
                    converged = false;
                    break 'fixpoint;
                }
                if self.interpret_inst(i, &mut stack_buf, truncated, &mut mem_buf, Record::Skip) {
                    if dynamic_jump
                        && i != term_inst
                        && let Some(&AbsValue::Range(range)) = stack_buf.last()
                        && (2..MAX_ENUMERATED_VALUES).contains(&(range.last - range.start))
                        && narrowest
                            .as_ref()
                            .is_none_or(|(_, n)| range.last - range.start < n.last - n.start)
                    {
                        narrowest = Some((i, range));
                    }
                    continue;
                }
                // If the block reads below the state's entries rather than halts, run it on each
                // segment below.
                if stack_buf.len() >= self.insts[i].stack_io().0 as usize {
                    continue 'fixpoint;
                }
                let BlockState::Known { stack, mem, .. } = &parts.states[pid].1 else {
                    unreachable!()
                };
                input_buf.clone_from(stack);
                mem_buf.clone_from(mem);
                for &seg in &below_buf {
                    if seg_users.insert((seg, pid)) {
                        segs.users[seg].push(pid);
                    }
                    let BlockState::Known { stack, truncated, below, .. } = &segs.states[seg]
                    else {
                        unreachable!()
                    };
                    stack_buf.clone_from(stack);
                    stack_buf.extend_from_slice(&input_buf);
                    let key =
                        self.context_key(mode, &stack_buf, *truncated, !below.is_empty(), &mem_buf);
                    work += budget.pop_work + 1 + stack_buf.len().min(MAX_ABS_STACK_DEPTH) as u64;
                    if !parts.join(
                        bid,
                        key,
                        &stack_buf,
                        *truncated,
                        below,
                        &mem_buf,
                        &mut const_sets,
                        &budget,
                    ) {
                        converged = false;
                        break 'fixpoint;
                    }
                }
                continue 'fixpoint;
            }
            let branch = branches[bid].as_ref();
            let condition = match branch {
                Some(_) => self.resolve_jump_condition(condition, &const_sets),
                None => JumpCondition::Unknown,
            };

            // Discover dynamic-jump target edges from this context's jump operand.
            targets.clear();
            if dynamic_jump && condition != JumpCondition::AlwaysFalse {
                let state = &mut enumerated[bid];
                match operand {
                    Some(operand) => {
                        self.jump_target_blocks(operand, &const_sets, &mut targets);
                        let unresolved = match operand {
                            AbsValue::Top => true,
                            AbsValue::Range(range) => self.range_jumpdests(range).is_none(),
                            AbsValue::Const(_) | AbsValue::ConstSet(_) => false,
                        };
                        if unresolved {
                            let len = targets.len();
                            let found = narrowest.and_then(|(split, range)| {
                                // Each value copies the stack and runs the rest of the block.
                                let values = u64::from(range.last - range.start + 1);
                                let rest = term_inst.index() - split.index();
                                let copy = stack_buf.len().min(MAX_ABS_STACK_DEPTH);
                                work += self.cfg.blocks[bid].insts().len() as u64
                                    + values * (rest + copy) as u64;
                                // The runs repeat folds that the analysis keeps, so they must not
                                // drain that budget, but their cost counts as work.
                                let gas = self.compiler_gas_used;
                                let found = self.enumerate_jump(
                                    bid,
                                    &parts.states[pid].1,
                                    split,
                                    range,
                                    &mut enumerate_buf,
                                    &mut targets,
                                );
                                work += self.compiler_gas_used - gas;
                                self.compiler_gas_used = gas;
                                found
                            });
                            match found {
                                Some(invalid) => {
                                    state.done = true;
                                    state.invalid |= invalid;
                                }
                                None => {
                                    targets.truncate(len);
                                    state.failed = true;
                                }
                            }
                        } else {
                            // Sets only hold valid targets, and a range may hold others.
                            state.invalid |= match operand {
                                AbsValue::Const(_) => self.jump_target_pc(operand).is_none(),
                                AbsValue::ConstSet(_) => false,
                                AbsValue::Range(_) | AbsValue::Top => true,
                            };
                            if let AbsValue::Const(imm) = operand
                                && !self.fits_switch_case(imm)
                            {
                                state.failed = true;
                            }
                        }
                    }
                    None => state.failed = true,
                }
                for &tb in &targets {
                    if discovered_set.insert((bid, tb)) {
                        discovered[bid].push(tb);
                    }
                }
            }

            // Propagate to static CFG successors and discovered dynamic-jump targets.
            if self.cfg.blocks[bid].succs.is_empty() && targets.is_empty() {
                continue;
            }

            // Move the entries from a repeated return address down into a segment.
            if let ContextMode::Folded = mode
                && let Some(depth) = self.recursion_cut(&stack_buf, &forwarders, &mut copies)
            {
                let split = stack_buf.len() - depth;
                let seg_stack = &stack_buf[..split];
                let key =
                    self.context_key(mode, seg_stack, truncated, !below_buf.is_empty(), &no_mem);
                let seg = match segs.by_key.get(&key) {
                    Some(&seg) => seg,
                    None => {
                        if segs.states.len() == budget.parts {
                            converged = false;
                            break 'fixpoint;
                        }
                        segs.users.push(Vec::new());
                        let seg = segs.states.push(BlockState::Bottom);
                        segs.by_key.insert(key, seg);
                        seg
                    }
                };
                work += 1 + split.min(MAX_ABS_STACK_DEPTH) as u64;
                if segs.states[seg].join(seg_stack, truncated, &below_buf, &no_mem, &mut const_sets)
                {
                    for &user in &segs.users[seg] {
                        parts.queue(user);
                    }
                }
                stack_buf.drain(..split);
                truncated = false;
                below_buf.clear();
                below_buf.push(seg);
            }

            let continued = !below_buf.is_empty();
            let key = self.context_key(mode, &stack_buf, truncated, continued, &mem_buf);
            let mut refined_key = None;
            for &succ in self.cfg.blocks[bid].succs.iter().chain(&targets) {
                let (mut stack, mut key) = (&stack_buf, key);
                if let Some(branch) = branch {
                    // Whether this edge is the jump rather than the fall-through. Both may reach
                    // the fall-through block.
                    let taken = if Some(succ) != branch.fallthrough {
                        Some(true)
                    } else if branch.fallthrough_is_target || targets.contains(&succ) {
                        None
                    } else {
                        Some(false)
                    };
                    match (taken, condition) {
                        (Some(true), JumpCondition::AlwaysFalse)
                        | (Some(false), JumpCondition::AlwaysTrue) => continue,
                        _ => {}
                    }
                    // The value that the branch condition bounds on this edge.
                    if let Some((bound_taken, depth, bound)) = branch.bound
                        && taken == Some(bound_taken)
                        && depth < stack_buf.len()
                    {
                        let refined = *refined_key.get_or_insert_with(|| {
                            refined_buf.clone_from(&stack_buf);
                            let value = &mut refined_buf[stack_buf.len() - 1 - depth];
                            *value = value.at_most(bound, &mut self.u256_interner.borrow_mut());
                            self.context_key(mode, &refined_buf, truncated, continued, &mem_buf)
                        });
                        (stack, key) = (&refined_buf, refined);
                    }
                }
                work += 1 + stack.len().min(MAX_ABS_STACK_DEPTH) as u64;
                if !parts.join(
                    succ,
                    key,
                    stack,
                    truncated,
                    &below_buf,
                    &mem_buf,
                    &mut const_sets,
                    &budget,
                ) {
                    converged = false;
                    break 'fixpoint;
                }
            }
        }

        debug!(
            ?mode,
            parts = parts.states.len(),
            segs = segs.states.len(),
            work = work + const_sets.merged,
            "{msg} after {pops} iterations (max={max})",
            msg = if converged { "converged" } else { "did not converge" },
            max = budget.pops,
        );

        // Record the snapshots of the final states, joined across contexts. A state with
        // segments below stops where its block reads below it: the runs of the block on the
        // segments record the rest.
        if converged {
            let mut visited: BitVec = BitVec::repeat(false, self.insts.len());
            for (bid, state) in &parts.states {
                let BlockState::Known { stack, truncated, mem, .. } = state else { continue };
                stack_buf.clone_from(stack);
                mem_buf.clone_from(mem);
                for i in self.cfg.blocks[*bid].insts() {
                    let record = if visited[i.index()] {
                        Record::Join(&mut const_sets)
                    } else {
                        Record::Overwrite
                    };
                    if !self.interpret_inst(i, &mut stack_buf, *truncated, &mut mem_buf, record) {
                        break;
                    }
                    visited.set(i.index(), true);
                }
            }
        }
        let reached = parts.by_key.iter().map(|p| !p.is_empty()).collect();
        Fixpoint { reached, discovered, const_sets, enumerated, converged }
    }

    /// Finds the targets of the dynamic jump that ends `bid` by running the rest of the block on
    /// each value of `range`, which the instruction `split` leaves on top of the stack, such as
    /// the index of a jump table that the code reads with a shift.
    ///
    /// `entry` is the state at the entry of the block. Returns `None` if a value leaves a jump
    /// operand that is not constant, and otherwise whether a value jumps to an invalid target,
    /// after adding the blocks of the valid targets to `targets`. A value whose path halts before
    /// the jump has no target.
    fn enumerate_jump(
        &mut self,
        bid: Block,
        entry: &BlockState,
        split: Inst,
        range: RangeInclusive<u32>,
        buf: &mut EnumerateBuf,
        targets: &mut SmallVec<[Block; 4]>,
    ) -> Option<bool> {
        let BlockState::Known { stack, truncated, mem, .. } = entry else { return None };
        let EnumerateBuf { stack: values, mem: values_mem, split_stack, split_mem } = buf;
        split_stack.clone_from(stack);
        split_mem.clone_from(mem);
        let term = self.cfg.blocks[bid].terminator();
        for i in self.cfg.blocks[bid].insts().take_while(|&i| i <= split) {
            if !self.interpret_inst(i, split_stack, *truncated, split_mem, Record::Skip) {
                return None;
            }
        }

        let mut invalid = false;
        for value in range.start..=range.last {
            values.clone_from(split_stack);
            values_mem.clone_from(split_mem);
            let imm = U256Imm::new(U256::from(value), &mut self.u256_interner.borrow_mut());
            *values.last_mut()? = AbsValue::Const(imm);
            let mut rest =
                self.cfg.blocks[bid].insts().skip_while(|&i| i <= split).take_while(|&i| i < term);
            let reaches_jump =
                rest.all(|i| self.interpret_inst(i, values, *truncated, values_mem, Record::Skip));
            if !reaches_jump {
                continue;
            }
            let target = *values.last()?;
            let AbsValue::Const(_) = target else { return None };
            match self.jump_target_pc(target) {
                Some(pc) => {
                    let tb = self.cfg.inst_to_block[self.pc_to_inst(pc as usize)]?;
                    if !targets.contains(&tb) {
                        targets.push(tb);
                    }
                }
                None => {
                    let AbsValue::Const(imm) = target else { return None };
                    if !self.fits_switch_case(imm) {
                        return None;
                    }
                    invalid = true;
                }
            }
        }
        Some(invalid)
    }

    /// Returns whether a switch on the low 64 bits of a jump operand tells `imm` from the valid
    /// jump targets, which it does unless `imm` has bits above them.
    fn fits_switch_case(&self, imm: U256Imm) -> bool {
        u64::try_from(imm.get(&self.u256_interner.borrow())).is_ok()
    }

    /// Returns what the condition of the `JUMPI` that ends `bid` shows about its successors.
    ///
    /// A block that ends in a test, any number of `ISZERO`s, a `PUSH` and the `JUMPI` leaves the
    /// tested value on the stack, and the test bounds it on one edge. The test is a `DUP` of the
    /// value, which is then zero where the condition is, or an `LT` or `GT` of a `DUP` of the
    /// value and a `PUSH`ed constant in either order. `ISZERO`s swap the edges.
    fn branch(&self, bid: Block) -> Option<Branch> {
        let block = &self.cfg.blocks[bid];
        let term = block.terminator();
        let data = &self.insts[term];
        if data.opcode != op::JUMPI {
            return None;
        }

        let resolve = |inst: Inst| self.target_block(self.effective_inst(inst));
        let fallthrough = resolve(term + 1);
        let fallthrough_is_target = if data.flags.contains(InstFlags::MULTI_JUMP) {
            self.multi_jump_targets
                .get(&term)
                .is_some_and(|targets| targets.iter().any(|&target| resolve(target) == fallthrough))
        } else {
            data.is_static_jump()
                && !data.flags.contains(InstFlags::INVALID_JUMP)
                && resolve(data.static_jump_target()) == fallthrough
        };
        /// Returns [`Branch::bound`] from the live instructions before the `JUMPI`, last first.
        fn bound<'a>(mut tail: impl Iterator<Item = &'a InstData>) -> Option<(bool, usize, u32)> {
            // The depth of the value that a `DUP` copies, before the `DUP`.
            let dup = |data: &InstData| match data.opcode {
                op @ op::DUP1..=op::DUP16 => Some((op - op::DUP1) as usize),
                _ => None,
            };
            let is_push = |data: &InstData| matches!(data.opcode, op::PUSH0..=op::PUSH32);
            let push = |data: &InstData| if is_push(data) { data.imm().get_inline() } else { None };

            if !is_push(tail.next()?) {
                return None;
            }
            // The edge on which the test is zero.
            let mut zero_taken = false;
            let mut test = tail.next()?;
            while test.opcode == op::ISZERO {
                zero_taken = !zero_taken;
                test = tail.next()?;
            }
            if let Some(depth) = dup(test) {
                return Some((zero_taken, depth, 0));
            }
            if !matches!(test.opcode, op::LT | op::GT) {
                return None;
            }
            // `x < c` or `x > c`, where the `DUP` copies `x` from `depth`.
            let (a, b) = (tail.next()?, tail.next()?);
            let (less, depth, c) = if let Some(depth) = dup(a)
                && let Some(c) = push(b)
            {
                // The `DUP` counts `c` above `x`.
                (test.opcode == op::LT, depth.checked_sub(1)?, c)
            } else {
                (test.opcode == op::GT, dup(b)?, push(a)?)
            };
            // `x < c` holds where the test is nonzero, and `x > c` fails where it is zero.
            if less {
                Some((!zero_taken, depth, c.checked_sub(1)?))
            } else {
                Some((zero_taken, depth, c))
            }
        }

        let tail = (block.insts().rev().skip(1).map(|i| &self.insts[i]))
            .filter(|data| !data.is_dead_code())
            .take_while(|data| !data.flags.contains(InstFlags::DISABLED));
        Some(Branch { fallthrough, fallthrough_is_target, bound: bound(tail) })
    }

    /// Returns the context of an abstract stack: a hash of the constants `mode` keys on, and of
    /// whether segments continue the stack.
    ///
    /// Keys that collide share a state, which only costs precision.
    fn context_key(
        &self,
        mode: ContextMode,
        stack: &[AbsValue],
        truncated: bool,
        continued: bool,
        mem: &MemWords,
    ) -> u64 {
        let targets = (stack.iter().rev().enumerate())
            .filter_map(|(depth, &value)| Some((depth, self.jump_target_pc(value)?)));
        let mut hasher = FxHasher::default();
        let tracked = stack.len().min(MAX_CONTEXT_DEPTH);
        (tracked as u8, truncated || stack.len() > tracked, continued).hash(&mut hasher);
        match mode {
            ContextMode::Full | ContextMode::Folded => {
                for &(offset, value) in &mem.words {
                    if let Some(pc) = self.jump_target_pc(value) {
                        (offset, pc).hash(&mut hasher);
                    }
                }
                let mut prev_deep = None;
                for (depth, pc) in targets {
                    if depth < MAX_CONTEXT_DEPTH {
                        (depth as u8, pc).hash(&mut hasher);
                    } else if prev_deep.replace(pc) != Some(pc) {
                        pc.hash(&mut hasher);
                    }
                }
            }
            ContextMode::Coarse => {
                targets
                    .take(1)
                    .filter(|&(depth, _)| depth < MAX_CONTEXT_DEPTH)
                    .for_each(|(depth, pc)| (depth as u8, pc).hash(&mut hasher));
            }
            ContextMode::Insensitive => return 0,
        }
        hasher.finish()
    }

    /// Returns the depth of the shallowest valid jump target constant in `stack` that repeats a
    /// shallower one, such as the return address of a recursive call one level up.
    ///
    /// Targets in `forwarders` count from their third copy: compilers share such blocks as the
    /// return address of many calls, which nest without a recursion.
    ///
    /// `copies` holds zero for every pc, and does again on return.
    fn recursion_cut(
        &self,
        stack: &[AbsValue],
        forwarders: &BitVec,
        copies: &mut [u8],
    ) -> Option<usize> {
        let cut = stack.iter().rev().enumerate().find_map(|(depth, &value)| {
            let pc = self.jump_target_pc(value)?;
            let left = &mut copies[pc as usize];
            if *left == 0 {
                let bid = self.cfg.inst_to_block[self.pc_to_inst(pc as usize)]?;
                *left = if forwarders[bid.index()] { 2 } else { 1 };
                return None;
            }
            *left -= 1;
            (*left == 0).then_some(depth)
        });
        let scanned = cut.map_or(stack.len(), |depth| depth + 1);
        for &value in stack.iter().rev().take(scanned) {
            if let Some(pc) = self.jump_target_pc(value) {
                copies[pc as usize] = 0;
            }
        }
        cut
    }

    /// Returns whether `bid` only moves stack entries and jumps, such as a shared block that
    /// returns to the address below the top of the stack.
    fn is_forwarder(&self, bid: Block) -> bool {
        let block = &self.cfg.blocks[bid];
        self.insts[block.terminator()].opcode == op::JUMP
            && block.insts().rev().skip(1).map(|i| &self.insts[i]).all(|data| {
                data.is_dead_code()
                    || matches!(
                        data.opcode,
                        op::JUMPDEST | op::POP | op::DUP1..=op::DUP16 | op::SWAP1..=op::SWAP16
                    )
            })
    }

    /// Returns the pc of `value` if it is a valid jump target constant.
    fn jump_target_pc(&self, value: AbsValue) -> Option<u32> {
        let pc = value.as_const()?.get_inline()?;
        self.is_valid_jump(pc as usize).then_some(pc)
    }

    /// Collects the blocks of the valid jump targets in `operand`.
    fn jump_target_blocks(
        &self,
        operand: AbsValue,
        const_sets: &ConstSetInterner,
        targets: &mut SmallVec<[Block; 4]>,
    ) {
        let consts = match operand {
            AbsValue::Const(imm) => Either::Left(std::iter::once(imm)),
            AbsValue::ConstSet(set_idx) => Either::Right(const_sets.get(set_idx).iter().copied()),
            AbsValue::Range(range) => {
                let insts = self.range_jumpdests(range).into_iter().flatten();
                targets.extend(insts.filter_map(|inst| self.cfg.inst_to_block[inst]));
                return;
            }
            AbsValue::Top => return,
        };
        let interner = self.u256_interner.borrow();
        for imm in consts {
            let Ok(target_pc) = usize::try_from(imm.get(&interner)) else { continue };
            if !self.is_valid_jump(target_pc) {
                continue;
            }
            // Set members are distinct jump targets, and each starts its own block.
            if let Some(tb) = self.cfg.inst_to_block[self.pc_to_inst(target_pc)] {
                targets.push(tb);
            }
        }
    }

    /// Interpret a sequence of instructions on the abstract stack.
    /// Returns `false` on stack underflow (conflict).
    ///
    /// The caller must pre-fill `stack` with the input state; on return it contains the output.
    /// When `truncated` is set, missing entries below `stack` are `Top` instead of an underflow.
    /// `mem` holds the known memory words in the same way as `stack`.
    /// Records per-instruction operand snapshots into `self.snapshots`.
    fn interpret_block(
        &mut self,
        insts: impl IntoIterator<Item = Inst>,
        stack: &mut Vec<AbsValue>,
        truncated: bool,
        mem: &mut MemWords,
    ) -> bool {
        insts.into_iter().all(|i| self.interpret_inst(i, stack, truncated, mem, Record::Overwrite))
    }

    /// Interpret a single instruction on the abstract stack, like [`Self::interpret_block`],
    /// recording its snapshots as `record` says.
    fn interpret_inst(
        &mut self,
        i: Inst,
        stack: &mut Vec<AbsValue>,
        truncated: bool,
        mem: &mut MemWords,
        mut record: Record<'_>,
    ) -> bool {
        let inst = &self.insts[i];
        if inst.is_dead_code() {
            return true;
        }

        // Only jump operands need the constant sets; elsewhere only single constants are used.
        let is_jump = inst.is_jump();
        let join = |sets: &mut ConstSetInterner, a: AbsValue, b: AbsValue| {
            if is_jump {
                sets.join(a, b)
            } else if a == b {
                a
            } else {
                AbsValue::Top
            }
        };

        let (inp, out) = inst.stack_io();
        let inp = inp as usize;
        let out = out as usize;

        // Check the stack height before recording, so that a state with segments below records
        // nothing for an instruction that reads them.
        if stack.len() < inp {
            if !truncated {
                return false;
            }
            stack.splice(0..0, std::iter::repeat_n(AbsValue::Top, inp - stack.len()));
        }

        // Record pre-instruction input operand snapshot (in stack order, TOS last).
        if inp > 0 {
            let inputs = &stack[stack.len() - inp..];
            let snap = &mut self.snapshots.inputs[i];
            match &mut record {
                Record::Skip => {}
                Record::Overwrite => {
                    snap.clear();
                    snap.extend_from_slice(inputs);
                }
                Record::Join(sets) => {
                    for (slot, &value) in snap.iter_mut().zip(inputs) {
                        *slot = join(sets, *slot, value);
                    }
                }
            }
            debug_assert!(matches!(record, Record::Skip) || snap.len() == inp);
        }

        match inst.opcode {
            op::PUSH0..=op::PUSH32 => {
                stack.push(AbsValue::Const(inst.imm()));
            }
            op::POP => {
                stack.pop();
            }
            // Unknown, disabled, or an invalid immediate.
            op::DUP1..=op::DUP16 | op::SWAP1..=op::SWAP16 | op::DUPN | op::SWAPN | op::EXCHANGE
                if inst.is_diverging() =>
            {
                return false;
            }
            op::DUP1..=op::DUP16 | op::DUPN => {
                stack.push(stack[stack.len() - inp]);
            }
            op::SWAP1..=op::SWAP16 | op::SWAPN => {
                let len = stack.len();
                stack.swap(len - 1, len - inp);
            }
            op::EXCHANGE => {
                let (n, _) = crate::decode_pair(inst.imm_byte()).unwrap();
                let len = stack.len();
                stack.swap(len - 1 - n as usize, len - inp);
            }
            _ => {
                // Disabled opcodes halt and have no stack inputs.
                let loaded = if inst.flags.contains(InstFlags::DISABLED) {
                    None
                } else if inst.flags.contains(InstFlags::HOST) {
                    // The host's instruction may write any memory and any stack word.
                    *mem = MemWords::default();
                    stack.fill(AbsValue::Top);
                    None
                } else {
                    mem.transfer(
                        inst.opcode,
                        &stack[stack.len() - inp..],
                        &self.code,
                        &mut self.u256_interner.borrow_mut(),
                    )
                };

                // Try constant folding for common arithmetic, respecting the gas budget.
                let result = if loaded.is_some() {
                    loaded
                } else if out > 0 && self.compiler_gas_used < self.compiler_gas_limit {
                    let inputs_slice = &stack[stack.len() - inp..];
                    let mut interner = self.u256_interner.borrow_mut();

                    // Check gas cost before doing the actual fold.
                    let gas =
                        super::const_fold::const_fold_gas(inst.opcode, inputs_slice, &interner);
                    if let Some(cost) = gas
                        && self.compiler_gas_used.saturating_add(cost) <= self.compiler_gas_limit
                    {
                        let folded = super::const_fold::try_const_fold(
                            inst,
                            inputs_slice,
                            &mut interner,
                            self.code.len(),
                        );
                        // Range folds take constant time.
                        if matches!(folded, Some(AbsValue::Const(_))) {
                            self.compiler_gas_used += cost;
                        }
                        folded
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Pop inputs.
                stack.truncate(stack.len() - inp);

                // Push outputs.
                if let Some(folded) = result {
                    debug_assert_eq!(out, 1);
                    stack.push(folded);
                } else {
                    stack.resize(stack.len() + out, AbsValue::Top);
                }
            }
        }

        // Record post-instruction output snapshot.
        // Skip SWAP/SWAPN/EXCHANGE: they modify two positions and have no single "output".
        if out > 0 && !matches!(inst.opcode, op::SWAP1..=op::SWAP16 | op::SWAPN | op::EXCHANGE) {
            let value = stack.last().copied();
            let slot = &mut self.snapshots.outputs[i];
            match (&mut record, *slot, value) {
                (Record::Skip, ..) => {}
                (Record::Join(sets), Some(old), Some(new)) => *slot = Some(join(sets, old, new)),
                _ => *slot = value,
            }
        }

        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) use crate::bytecode::Inst;
    pub(crate) use alloy_primitives::{U256, hex};
    pub(crate) use evm2::SpecId;

    pub(crate) fn analyze_hex(hex: &str) -> Bytecode<'static> {
        let code = hex::decode(hex.trim()).unwrap();
        analyze_code(code)
    }

    pub(crate) fn analyze_code(code: Vec<u8>) -> Bytecode<'static> {
        analyze_code_with(code, Default::default())
    }

    pub(crate) fn analyze_code_spec(code: Vec<u8>, spec_id: SpecId) -> Bytecode<'static> {
        crate::tests::init_tracing();

        eprintln!("{}", hex::encode(&code));
        let mut bytecode = Bytecode::new(code, spec_id, None, None);
        bytecode.analyze().unwrap();
        eprintln!("{bytecode}");
        bytecode
    }

    pub(crate) fn analyze_code_with(
        code: Vec<u8>,
        config: crate::bytecode::AnalysisConfig,
    ) -> Bytecode<'static> {
        crate::tests::init_tracing();

        eprintln!("{}", hex::encode(&code));
        let mut bytecode = Bytecode::test(code);
        bytecode.config = config;
        bytecode.analyze().unwrap();
        eprintln!("{bytecode}");
        bytecode
    }

    pub(crate) fn analyze_asm(src: &str) -> Bytecode<'static> {
        analyze_code(crate::parse_asm(src).unwrap())
    }

    pub(crate) fn analyze_asm_spec(src: &str, spec_id: SpecId) -> Bytecode<'static> {
        analyze_code_spec(crate::parse_asm(src).unwrap(), spec_id)
    }

    pub(crate) fn analyze_asm_with(
        src: &str,
        config: crate::bytecode::AnalysisConfig,
    ) -> Bytecode<'static> {
        analyze_code_with(crate::parse_asm(src).unwrap(), config)
    }

    #[test]
    fn revert_sub_call_storage_oog() {
        let bytecode = analyze_hex(
            "60606040526000357c0100000000000000000000000000000000000000000000000000000000900463ffffffff168063b28175c4146046578063c0406226146052575b6000565b3460005760506076565b005b34600057605c6081565b604051808215151515815260200191505060405180910390f35b600c6000819055505b565b600060896076565b600d600181905550600e600281905550600190505b905600a165627a7a723058202a8a75d7d795b5bcb9042fb18b283daa90b999a11ddec892f5487322",
        );
        assert!(!bytecode.has_dynamic_jumps());
    }

    #[test]
    fn revert_remote_sub_call_storage_oog() {
        let bytecode = analyze_hex(
            "608060405234801561001057600080fd5b506004361061002b5760003560e01c806373027f6d14610030575b600080fd5b61004a600480360381019061004591906101a9565b61004c565b005b6000808273ffffffffffffffffffffffffffffffffffffffff166040516024016040516020818303038152906040527fb28175c4000000000000000000000000000000000000000000000000000000007bffffffffffffffffffffffffffffffffffffffffffffffffffffffff19166020820180517bffffffffffffffffffffffffffffffffffffffffffffffffffffff83818316178352505050506040516100f69190610247565b6000604051808303816000865af19150503d8060008114610133576040519150601f19603f3d011682016040523d82523d6000602084013e610138565b606091505b509150915081600155505050565b600080fd5b600073ffffffffffffffffffffffffffffffffffffffff82169050919050565b60006101768261014b565b9050919050565b6101868161016b565b811461019157600080fd5b50565b6000813590506101a38161017d565b92915050565b6000602082840312156101bf576101be610146565b5b60006101cd84828501610194565b91505092915050565b600081519050919050565b600081905092915050565b60005b8381101561020a5780820151818401526020810190506101ef565b60008484015250505050565b6000610221826101d6565b61022b81856101e1565b935061023b8185602086016101ec565b80840191505092915050565b60006102538284610216565b91508190509291505056fea2646970667358221220b4673c55c7b0268d7d118059e6509196d2185bb7fe040a7d3900f902c8542ea464736f6c63430008180033",
        );
        assert!(!bytecode.has_dynamic_jumps());
    }

    #[test]
    fn trans_storage_ok() {
        let contracts = [
            "366012575b600b5f6020565b5f5260205ff35b601c6160a75f6024565b6004565b5c90565b5d56",
            "60106001600a5f6012565b015f6016565b005b5c90565b5d56",
            "3033146033575b303303600e57005b601b5f35806001555f608d565b5f80808080305af1600255602e60016089565b600355005b603a5f6089565b8015608757604a600182035f608d565b5f80808080305af1156083576001606191035f608d565b5f80808080305af115608357607f60016078816089565b016001608d565b6006565b5f80fd5b005b5c90565b5d56",
            "60065f601d565b5f5560106001601d565b6001555f80808080335af1005b5c9056",
        ];
        let has_dynamic = [false, false, false, false];
        for (hex, &expected) in contracts.iter().zip(&has_dynamic) {
            let bytecode = analyze_hex(hex);
            assert_eq!(bytecode.has_dynamic_jumps(), expected, "contract: {hex}");
        }
    }

    #[test]
    fn const_operand_basic() {
        // inst 0: PUSH1 0x42 -> stack: [0x42]
        // inst 1: PUSH1 0x01 -> stack: [0x42, 0x01]
        // inst 2: ADD        -> stack: [0x43]  (const-folded)
        // inst 3: PUSH1 0x00 -> stack: [0x43, 0x00]
        // inst 4: MSTORE     -> pops 2
        // inst 5: JUMPDEST
        // inst 6: STOP
        let bytecode = analyze_asm(
            "
            PUSH1 0x42
            PUSH1 0x01
            ADD
            PUSH1 0x00
            MSTORE
            JUMPDEST
            STOP
        ",
        );
        // At inst 2 (ADD), operand 0 (TOS) = 0x01, operand 1 = 0x42.
        assert_eq!(bytecode.const_operand(Inst::new(2), 0), Some(U256::ONE));
        assert_eq!(bytecode.const_operand(Inst::new(2), 1), Some(U256::from(0x42)));
        // At inst 4 (MSTORE), operand 0 (TOS) = 0x00, operand 1 = 0x43 (folded ADD result).
        assert_eq!(bytecode.const_operand(Inst::new(4), 0), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(4), 1), Some(U256::from(0x43)));
    }

    #[test]
    fn const_operand_dynamic() {
        // CALLDATALOAD pushes an unknown value -> const_operand should return None.
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH0
            MSTORE
            STOP
        ",
        );
        // At inst 3 (MSTORE), operand 0 (TOS) = 0x00, operand 1 = unknown (CALLDATALOAD result).
        assert_eq!(bytecode.const_operand(Inst::new(3), 0), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(3), 1), None);
    }

    #[test]
    fn const_output_basic() {
        // PUSH1 0x42 -> output[0] = 0x42
        // PUSH1 0x01 -> output[0] = 0x01
        // ADD        -> output[0] = 0x43 (const-folded)
        // PUSH0      -> output[0] = 0x00
        // MSTORE     -> no outputs
        // STOP
        let bytecode = analyze_asm(
            "
            PUSH1 0x42
            PUSH1 0x01
            ADD
            PUSH0
            MSTORE
            STOP
        ",
        );
        assert_eq!(bytecode.const_output(Inst::new(0)), Some(U256::from(0x42)));
        assert_eq!(bytecode.const_output(Inst::new(1)), Some(U256::ONE));
        assert_eq!(bytecode.const_output(Inst::new(2)), Some(U256::from(0x43)));
        assert_eq!(bytecode.const_output(Inst::new(3)), Some(U256::ZERO));
        // MSTORE has no outputs.
        assert_eq!(bytecode.const_output(Inst::new(4)), None);
    }

    #[test]
    fn const_output_dynamic() {
        // CALLDATALOAD pushes an unknown -> output should be None.
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            STOP
        ",
        );
        assert_eq!(bytecode.const_output(Inst::new(0)), Some(U256::ZERO));
        assert_eq!(bytecode.const_output(Inst::new(1)), None);
    }

    /// DUPN, SWAPN, and EXCHANGE should propagate constants like their fixed counterparts.
    #[test]
    fn const_snapshot_eof_stack_ops() {
        // DUPN/SWAPN min index is 17, so we need 17 values on the stack.
        // 16 × PUSH0, PUSH1 0xAA, DUPN 17 (raw=0x80), STOP.
        let mut code: Vec<u8> = vec![op::PUSH0; 16];
        code.extend([op::PUSH1, 0xAA]); // inst 16: TOS = 0xAA
        code.extend([op::DUPN, 0x80]); // inst 17: DUPN 17 (dup bottom = 0x00)
        code.push(op::STOP); // inst 18
        let bytecode = analyze_code_spec(code, SpecId::AMSTERDAM);
        // DUPN duplicates the 17th item (bottom PUSH0 = 0x00).
        assert_eq!(bytecode.const_output(Inst::new(17)), Some(U256::ZERO));
        // The PUSH1 0xAA is still there.
        assert_eq!(bytecode.const_output(Inst::new(16)), Some(U256::from(0xAA)));
    }

    #[test]
    fn snapshots_all_blocks() {
        // Blocks with unresolvable dynamic jumps must still get snapshots from
        // block_analysis_local. Here CALLDATALOAD produces an opaque jump target,
        // so the JUMP stays dynamic, but the block's other instructions should
        // still have snapshots populated.
        let bytecode = analyze_asm(
            "
            PUSH1 0x42          ; inst 0
            PUSH1 0x01          ; inst 1
            ADD                 ; inst 2: 0x42 + 0x01 = 0x43
            PUSH0               ; inst 3
            CALLDATALOAD        ; inst 4: opaque value
            JUMP                ; inst 5: dynamic, unresolvable
        target1:
            JUMPDEST            ; inst 6
            PUSH1 0x01          ; inst 7
            ADD                 ; inst 8
            STOP                ; inst 9
        target2:
            JUMPDEST            ; inst 10
            PUSH1 0x02          ; inst 11
            SUB                 ; inst 12
            STOP                ; inst 13
        ",
        );
        // The JUMP at inst 5 should remain dynamic.
        assert!(bytecode.has_dynamic_jumps, "expected unresolved dynamic jump");
        let jump = bytecode.inst(Inst::new(5));
        assert!(jump.is_jump());
        assert!(!jump.flags.contains(InstFlags::STATIC_JUMP));
        // JUMP's TOS operand is Top (CALLDATALOAD result).
        assert_eq!(bytecode.const_operand(Inst::new(5), 0), None);

        // ADD 1
        assert_eq!(bytecode.const_operand(Inst::new(2), 0), Some(U256::ONE));
        assert_eq!(bytecode.const_operand(Inst::new(2), 1), Some(U256::from(0x42)));
        assert_eq!(bytecode.const_output(Inst::new(2)), Some(U256::from(0x43)));

        // ADD 2
        assert_eq!(bytecode.const_operand(Inst::new(8), 0), Some(U256::ONE));
        assert_eq!(bytecode.const_operand(Inst::new(8), 1), None);
        assert_eq!(bytecode.const_output(Inst::new(8)), None);

        // SUB
        assert_eq!(bytecode.const_operand(Inst::new(12), 0), Some(U256::from(0x02)));
        assert_eq!(bytecode.const_operand(Inst::new(12), 1), None);
        assert_eq!(bytecode.const_output(Inst::new(12)), None);
    }

    #[test]
    fn multi_target_jump() {
        // Internal function called from two sites with different return addresses.
        // The return JUMP at the end should resolve to Multi([ret1, ret2]).
        let bytecode = analyze_asm(
            "
            ; Call site 1.
            PUSH %ret1          ; push return address
            PUSH %func          ; push function entry
            JUMP                ; call function (static: PUSH+JUMP)
        ret1:
            JUMPDEST
            POP                 ; consume function result
            ; Call site 2.
            PUSH %ret2          ; push return address
            PUSH %func          ; push function entry
            JUMP                ; call function (static: PUSH+JUMP)
        ret2:
            JUMPDEST
            POP                 ; consume function result
            STOP
            ; Internal function.
        func:
            JUMPDEST
            PUSH1 0x42          ; push a result
            SWAP1               ; swap result and return address
            JUMP                ; return (dynamic)
        ",
        );

        // The return JUMP (last inst before STOP's block) should be multi-target.
        let return_jump = bytecode
            .iter_insts()
            .find(|(_, d)| d.is_jump() && d.flags.contains(InstFlags::MULTI_JUMP));
        assert!(return_jump.is_some(), "expected a multi-target jump");
        let (rj_inst, _) = return_jump.unwrap();
        let targets = bytecode.multi_jump_targets(rj_inst).unwrap();
        assert_eq!(targets.len(), 2, "expected 2 targets, got {}", targets.len());
        // Verify both targets are JUMPDESTs.
        for &t in targets {
            assert_eq!(bytecode.inst(t).opcode, op::JUMPDEST);
        }
        // No dynamic jumps should remain.
        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
    }

    /// Demonstrates a known unsoundness in `invalidate_suspect_jumps`.
    ///
    /// The invalidation pass only seeds suspicion from `Bottom + JUMPDEST` predecessors,
    /// but an unresolved dynamic jump can also reach a `Known` JUMPDEST block at runtime
    /// with a different stack than the fixpoint computed. This test constructs a case
    /// where the analysis incorrectly resolves a return JUMP to `Multi` even though an
    /// opaque (MLOAD-based) dynamic jump could reach the same function entry with an
    /// arbitrary return address.
    ///
    /// Layout:
    ///   Call site 1: PUSH ret1, PUSH fn, JUMP → fn returns to ret1
    ///   Call site 2: PUSH ret2, PUSH fn, JUMP → fn returns to ret2
    ///   Opaque jump: PUSH0, MLOAD, JUMP      → Top target, could be fn at runtime
    ///   fn:          JUMPDEST, PUSH 0x42, SWAP1, JUMP → return (should be Top)
    ///
    /// The fn JUMPDEST block is `Known([ConstSet({ret1, ret2})])` from the two static
    /// callers, so the return JUMP resolves to `Multi([ret1, ret2])`. But the opaque
    /// jump could call fn with any return address, making that resolution unsound.
    /// `invalidate_suspect_jumps` now seeds every reachable JUMPDEST block as suspect,
    /// so the return jump is correctly invalidated.
    #[test]
    fn known_jumpdest_unsound_resolution() {
        let bytecode = analyze_asm(
            "
            ; Call site 1.
            PUSH %ret1              ; pc=0
            PUSH %fn_entry          ; pc=2
            JUMP                    ; pc=4: -> fn_entry
        ret1:
            JUMPDEST                ; pc=5
            POP                     ; pc=6: drop result
            ; Call site 2.
            PUSH %ret2              ; pc=7
            PUSH %fn_entry          ; pc=9
            JUMP                    ; pc=11: -> fn_entry
        ret2:
            JUMPDEST                ; pc=12
            POP                     ; pc=13: drop result
            ; Opaque jump: loads target from memory (Top).
            PUSH0                   ; pc=14
            MLOAD                   ; pc=15: -> Top
            JUMP                    ; pc=16: dynamic, target = Top
            INVALID                 ; pc=17
            INVALID                 ; pc=18
            INVALID                 ; pc=19
            ; Internal function: pushes result, swaps with return addr, jumps back.
        fn_entry:
            JUMPDEST                ; pc=20
            PUSH1 0x42              ; pc=21: result
            SWAP1                   ; pc=23: swap result and return addr
            JUMP                    ; pc=24: return (UNSOUND: resolved to Multi)
        ",
        );

        // The return JUMP at pc=24 should NOT be resolved — the opaque jump at
        // pc=16 can reach fn_entry with any return address. But the current
        // invalidation misses this because fn_entry's block is Known, not Bottom.
        let return_jump = bytecode.iter_insts().find(|(i, d)| bytecode.pc(*i) == 24 && d.is_jump());
        let (_, rj) = return_jump.unwrap();
        assert!(
            !rj.flags.contains(InstFlags::STATIC_JUMP),
            "unsound: return jump should not be resolved"
        );
    }

    #[test]
    fn nested_call_return() {
        // Two-level call chain: outer calls wrapper, wrapper calls inner function.
        // Direct caller also calls the inner function directly.
        //
        //   call site 1 (direct):   PUSH ret1, PUSH inner, JUMP
        //   call site 2 (wrapper):  PUSH ret2, PUSH wrapper, JUMP
        //     wrapper:              PUSH ret_w, PUSH inner, JUMP
        //     ret_w:                SWAP1 POP JUMP  (returns to ret2)
        //
        // The inner function is called with different stack depths from the two
        // sites. Its return JUMP resolves to Multi([ret1, ret_w]). The wrapper's
        // final JUMP (at ret_w) must recover the outer return address from deep in
        // the stack, which a single joined state per block pads to Top; keeping
        // inner's states apart per caller context resolves it to ret2.
        let bytecode = analyze_asm(
            "
            ; Call site 1: direct call to inner.
            PUSH %ret1              ; pc=0
            PUSH %inner             ; pc=2
            JUMP                    ; pc=4: -> inner
        ret1:
            JUMPDEST                ; pc=5
            POP                     ; pc=6: drop result
            ; Call site 2: call through wrapper.
            PUSH %ret2              ; pc=7
            PUSH1 0x42              ; pc=9: an argument
            PUSH %wrapper           ; pc=11
            JUMP                    ; pc=13: -> wrapper
        ret2:
            JUMPDEST                ; pc=14
            POP                     ; pc=15: drop result
            POP                     ; pc=16: drop arg
            STOP                    ; pc=17
            INVALID                 ; pc=18
            ; Wrapper function: calls inner, then returns to caller.
            ; Entry stack: [outer_ret, arg]
        wrapper:
            JUMPDEST                ; pc=19
            PUSH %ret_w             ; pc=20: push return addr for inner
            PUSH %inner             ; pc=22: push inner entry
            JUMP                    ; pc=24: -> inner
            INVALID                 ; pc=25
            INVALID                 ; pc=26
        ret_w:
            JUMPDEST                ; pc=27: inner returned here
            ; Stack: [outer_ret, arg, inner_result]
            POP                     ; pc=28: drop inner_result
            POP                     ; pc=29: drop arg
            JUMP                    ; pc=30: return to caller via outer_ret (dynamic)
            INVALID                 ; pc=31
            ; Inner function: pushes a result and returns.
            ; Entry stack: [..., ret_addr]
        inner:
            JUMPDEST                ; pc=32
            PUSH1 0x42              ; pc=33: push result
            SWAP1                   ; pc=35
            JUMP                    ; pc=36: jump to ret_addr
        ",
        );

        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
        let jump_at =
            |pc| bytecode.iter_insts().find(|&(i, d)| bytecode.pc(i) == pc && d.is_jump());
        let (_, data) = jump_at(30).unwrap();
        assert!(!data.flags.contains(InstFlags::MULTI_JUMP));
        assert_eq!(bytecode.pc(data.static_jump_target()), 14, "wrapper must return to ret2");
        let (inner_ret, _) = jump_at(36).unwrap();
        let mut targets: Vec<_> = bytecode
            .multi_jump_targets(inner_ret)
            .unwrap()
            .iter()
            .map(|&t| bytecode.pc(t))
            .collect();
        targets.sort_unstable();
        assert_eq!(targets, [5, 27]);
    }

    /// Regression test: deep DUPN on Amsterdam must not cause the abstract interpreter to
    /// abort a block and leave a reachable JUMP as Bottom → INVALID_JUMP.
    #[test]
    fn amsterdam_deep_dupn_jump_not_invalid() {
        // Build a stack with 70 items, where the deepest is the final JUMP target.
        // A dispatcher JUMP is resolvable; a later JUMP in a block that uses DUPN 70
        // must also be correctly resolved (not marked INVALID_JUMP).
        let bytecode = analyze_asm_spec(
            "
            PUSH %target
            ; 69 × PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0 PUSH0
            PUSH %resolved
            PUSH %dispatcher
            JUMP
        dispatcher:
            JUMPDEST
            JUMP
        resolved:
            JUMPDEST
            DUPN 70
            POP
        problem:
            JUMPDEST
            DUPN 70
            JUMP
        target:
            JUMPDEST
            STOP
        ",
            SpecId::AMSTERDAM,
        );

        // The "problem" JUMP must NOT be marked INVALID_JUMP.
        let jumps: Vec<_> = bytecode.iter_insts().filter(|(_, d)| d.opcode == op::JUMP).collect();
        for &(inst, data) in &jumps {
            assert!(
                !data.flags.contains(InstFlags::INVALID_JUMP),
                "JUMP inst={inst} pc={} incorrectly marked INVALID_JUMP",
                bytecode.pc(inst),
            );
        }
    }

    /// Asserts that no live instruction directly follows a dead one inside a CFG block, and
    /// that no dedup redirect targets a block that contains a dead instruction.
    fn assert_no_dead_gap_in_blocks(bytecode: &Bytecode<'_>) {
        for block in &bytecode.cfg.blocks {
            for (a, b) in block.insts().zip(block.insts().skip(1)) {
                assert!(
                    !(bytecode.inst(a).is_dead_code() && !bytecode.inst(b).is_dead_code()),
                    "live inst={b} pc={} follows dead inst={a} pc={} inside block {:?}",
                    bytecode.pc(b),
                    bytecode.pc(a),
                    block.insts,
                );
            }
        }
        for (&from, &to) in &bytecode.redirects {
            let Some(bid) = bytecode.cfg.inst_to_block[to] else { continue };
            let block = &bytecode.cfg.blocks[bid];
            assert!(
                !block.insts().any(|i| bytecode.inst(i).is_dead_code()),
                "redirect {from} -> {to} targets block {:?}, which contains dead code",
                block.insts,
            );
        }
    }

    /// Regression test: capping [`MemWords`] by count made the transfer non-monotone. The
    /// fixpoint reached a `JUMPDEST` that the committed jumps no longer justify, which left it
    /// live right after a dead instruction inside one CFG block.
    #[test]
    fn mem_words_no_dead_gap() {
        let bytecode = analyze_code_spec(
            hex::decode("60006100205260006100405260006100605260006100805260006100a05260006100c05260006100e05260006101005260006101205260006101405260006101605260006101805260006101a05260006101c05260006101e0526000610200525f356100825761006a565b60015f52610200516100805760405161007f57505b5b005b600160205361006a56").unwrap(),
            SpecId::AMSTERDAM,
        );
        assert_no_dead_gap_in_blocks(&bytecode);
    }

    /// Regression test: the dead gap left by the [`MemWords`] cap let dedup redirect a
    /// reachable block into a copy that runs into an untranslated dead instruction.
    #[test]
    fn mem_words_no_dedup_into_dead_gap() {
        let bytecode = analyze_code_spec(
            hex::decode("6020356100935760006100205260006100405260006100605260006100805260006100a05260006100c05260006100e05260006101005260006101205260006101405260006101605260006101805260006101a05260006101c05260006101e0526000610200525f3561008957610071565b60015f52610200516100875760405161008657505b5b005b6001602053610071565b60016040356100a157505b5b005b00").unwrap(),
            SpecId::AMSTERDAM,
        );
        assert_no_dead_gap_in_blocks(&bytecode);
    }

    #[test]
    fn hash_10k() {
        let code = fixture_entry_code(include_str!("../../../../../../data/hash_10k.json"));
        let mut bytecode = Bytecode::test(code);
        bytecode.analyze().unwrap();
        assert!(!bytecode.has_dynamic_jumps, "expected all jumps to be resolved");
    }

    pub(crate) fn fixture_entry_code(json: &str) -> Vec<u8> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let case = v.as_object().unwrap().values().next().unwrap();
        let transaction = &case["transaction"];
        let to = transaction
            .get("to")
            .or_else(|| {
                transaction
                    .as_array()
                    .and_then(|transactions| transactions.first())
                    .and_then(|transaction| transaction.get("to"))
            })
            .and_then(|to| to.as_str())
            .unwrap();
        hex::decode(case["pre"][to]["code"].as_str().unwrap()).unwrap()
    }
}

#[cfg(test)]
mod tests_edge_cases {
    use super::{tests::*, *};
    use evm2::{BaseEvmTypes, OpcodeConfig};

    /// Three callers to the same internal function. The return JUMP should
    /// resolve to Multi with three targets.
    #[test]
    fn multi_target_jump_three_callers() {
        let bytecode = analyze_asm(
            "
            ; Call site 1.
            PUSH %ret1
            PUSH %func
            JUMP
        ret1:
            JUMPDEST
            POP
            ; Call site 2.
            PUSH %ret2
            PUSH %func
            JUMP
        ret2:
            JUMPDEST
            POP
            ; Call site 3.
            PUSH %ret3
            PUSH %func
            JUMP
        ret3:
            JUMPDEST
            POP
            STOP
            ; Internal function.
        func:
            JUMPDEST
            PUSH1 0x42
            SWAP1
            JUMP
        ",
        );

        let return_jump = bytecode
            .iter_insts()
            .find(|(_, d)| d.is_jump() && d.flags.contains(InstFlags::MULTI_JUMP));
        assert!(return_jump.is_some(), "expected a multi-target jump");
        let (rj_inst, _) = return_jump.unwrap();
        let targets = bytecode.multi_jump_targets(rj_inst).unwrap();
        assert_eq!(targets.len(), 3, "expected 3 targets, got {}", targets.len());
        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
    }

    /// A dead jump with an unknown target must not block the resolution of
    /// live jumps.
    #[test]
    fn dead_unknown_jump_ignored() {
        let bytecode = analyze_asm(
            "
            PUSH %ret
            PUSH %func
            JUMP
        ret:
            JUMPDEST
            PUSH1 1
            PUSH %t
            JUMPI
            PUSH0
            CALLDATALOAD
            JUMP
        t:
            JUMPDEST
            STOP
        func:
            JUMPDEST
            JUMP
        ",
        );

        let (_, ret) = bytecode.iter_insts().last().unwrap();
        assert!(ret.flags.contains(InstFlags::STATIC_JUMP), "{:?}", ret.flags);
        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
    }

    /// DUP and SWAP should preserve constant values through the stack.
    #[test]
    fn dup_swap_const_propagation() {
        // PUSH1 0xAA -> [0xAA]
        // PUSH1 0xBB -> [0xAA, 0xBB]
        // DUP2       -> [0xAA, 0xBB, 0xAA]
        // SWAP1      -> [0xAA, 0xAA, 0xBB]
        // PUSH0      -> [0xAA, 0xAA, 0xBB, 0x00]
        // MSTORE     -> pops (0x00, 0xBB), leaves [0xAA, 0xAA]
        // STOP
        let bytecode = analyze_asm(
            "
            PUSH1 0xAA          ; inst 0
            PUSH1 0xBB          ; inst 1
            DUP2                ; inst 2
            SWAP1               ; inst 3
            PUSH0               ; inst 4
            MSTORE              ; inst 5
            STOP                ; inst 6
        ",
        );
        // DUP2 output should be 0xAA.
        assert_eq!(bytecode.const_output(Inst::new(2)), Some(U256::from(0xAA)));
        // At MSTORE (inst 5): TOS = 0x00, second = 0xBB (swapped back).
        assert_eq!(bytecode.const_operand(Inst::new(5), 0), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(5), 1), Some(U256::from(0xBB)));
    }

    /// JUMPI with a constant condition should still record both operands.
    #[test]
    fn jumpi_const_condition_snapshot() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x01              ; inst 0: condition = 1 (always taken)
            PUSH %target            ; inst 1: target
            JUMPI                   ; inst 2: static JUMPI
            STOP                    ; inst 3: fallthrough
        target:
            JUMPDEST                ; inst 4: target
            STOP                    ; inst 5
        ",
        );
        // The JUMPI should be resolved as static.
        let (jump_inst, jump) =
            bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(jump.flags.contains(InstFlags::STATIC_JUMP));
        assert_eq!(bytecode.inst(jump.static_jump_target()).opcode, op::JUMPDEST);
        assert_ne!(jump.static_jump_target(), jump_inst + 1);
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A loop where the stack grows each iteration.
    /// Verifies the analysis converges without losing precision at the top.
    #[test]
    fn loop_stack_growth_clamping() {
        // bb0: PUSH1 0x42, then fall through to bb1.
        // bb1: JUMPDEST, PUSH1 0x01, PUSH1 bb1_pc, JUMP  (loop back, growing stack).
        //
        // The stack grows by 1 each iteration (net +1 from PUSH before the loop jump).
        // Joining the back edge keeps the shorter stack, so the analysis converges without
        // conflict.
        let bytecode = analyze_asm(
            "
            PUSH1 0x42              ; pc=0: initial value
            PUSH1 0x01              ; pc=2: another
        loop_header:
            JUMPDEST                ; loop header (bb1)
            PUSH1 0x01              ; push each iteration
            PUSH %loop_header       ; loop target
            JUMP                    ; back-edge
        ",
        );
        // Should converge without panicking.
        // The JUMP should be static (resolved by adjacent PUSH+JUMP).
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A single-instruction block (just JUMPDEST as the terminator) used as a
    /// jump target. This tests the edge case where block.len() == 1.
    #[test]
    fn single_inst_block_jump_target() {
        let bytecode = analyze_asm(
            "
            PUSH %target            ; pc=0
            JUMP                    ; static jump
            INVALID
            INVALID
        target:
            JUMPDEST                ; single-inst block
            STOP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A JUMP with an invalid target (not a JUMPDEST) should be marked invalid.
    #[test]
    fn invalid_jump_target() {
        // The dynamic jump target points to a non-JUMPDEST instruction.
        // The function pushes a constant that isn't a valid JUMPDEST pc.
        let bytecode = analyze_asm(
            "
            PUSH1 0xFF              ; pc=0: push invalid target
            PUSH %func              ; pc=2: push function entry
            JUMP                    ; pc=4: -> func
            ; Function: swap and jump to the caller-provided target.
        func:
            JUMPDEST
            JUMP                    ; jump to 0xFF (invalid)
        ",
        );
        // The dynamic JUMP at pc=6 should be resolved as invalid.
        let jump_inst = bytecode
            .iter_insts()
            .find(|(_, d)| d.is_jump() && d.flags.contains(InstFlags::INVALID_JUMP));
        assert!(jump_inst.is_some(), "expected an invalid jump");
    }

    #[test]
    fn jumpi_with_zero_condition_ignores_unknown_target() {
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATASIZE
            JUMPI
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (_, jump) = bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(jump.flags.contains(InstFlags::STATIC_JUMP));
        assert!(!jump.flags.contains(InstFlags::INVALID_JUMP));
        assert_eq!(jump.static_jump_target(), Inst::new(3));
        assert!(!bytecode.has_dynamic_jumps);
    }

    #[test]
    fn jumpi_with_zero_condition_ignores_valid_target() {
        let bytecode = analyze_asm(
            "
            PUSH0
            PUSH %target
            JUMPI
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (_, jump) = bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(jump.flags.contains(InstFlags::STATIC_JUMP));
        assert!(!jump.flags.contains(InstFlags::INVALID_JUMP));
        assert_eq!(jump.static_jump_target(), Inst::new(3));
        assert!(!bytecode.has_dynamic_jumps);
    }

    #[test]
    fn jumpi_with_zero_condition_cfg_only_falls_through() {
        let bytecode = analyze_asm(
            "
            PUSH0
            PUSH %target
            JUMPI
            PUSH1 0xAA
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (jump_inst, jump) =
            bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        let jump_block = bytecode.cfg.inst_to_block[jump_inst].unwrap();
        let fallthrough_block = bytecode.cfg.inst_to_block[jump_inst + 1].unwrap();
        let target_inst =
            bytecode.iter_all_insts().rev().find(|(_, data)| data.is_jumpdest()).unwrap().0;

        assert!(jump.has_const_jumpi_condition());
        assert_eq!(jump.static_jump_target(), jump_inst + 1);
        assert_eq!(bytecode.cfg.blocks[jump_block].succs.as_slice(), &[fallthrough_block]);
        assert!(
            bytecode.cfg.inst_to_block[target_inst].is_none(),
            "never-taken JUMPI target should be dead"
        );
    }

    #[test]
    fn jumpi_with_zero_condition_from_predecessor_rewrites_local_static_cfg() {
        let bytecode = analyze_asm(
            "
            PUSH0
            PUSH %branch
            JUMP
        branch:
            JUMPDEST
            PUSH %target
            JUMPI
            PUSH1 0xAA
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (jump_inst, jump) =
            bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        let jump_block = bytecode.cfg.inst_to_block[jump_inst].unwrap();
        let fallthrough_block = bytecode.cfg.inst_to_block[jump_inst + 1].unwrap();
        let target_inst =
            bytecode.iter_all_insts().rev().find(|(_, data)| data.is_jumpdest()).unwrap().0;

        assert!(jump.has_const_jumpi_condition());
        assert_eq!(jump.static_jump_target(), jump_inst + 1);
        assert_eq!(bytecode.cfg.blocks[jump_block].succs.as_slice(), &[fallthrough_block]);
        assert!(
            bytecode.cfg.inst_to_block[target_inst].is_none(),
            "locally resolved but never-taken JUMPI target should be dead"
        );
    }

    #[test]
    fn jumpi_with_true_condition_cfg_only_jumps_to_target() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x01
            PUSH %target
            JUMPI
            PUSH1 0xAA
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (jump_inst, jump) =
            bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        let jump_block = bytecode.cfg.inst_to_block[jump_inst].unwrap();
        let target_block = bytecode.cfg.inst_to_block[jump.static_jump_target()].unwrap();

        assert!(jump.has_const_jumpi_condition());
        assert_ne!(jump.static_jump_target(), jump_inst + 1);
        assert_eq!(bytecode.cfg.blocks[jump_block].succs.as_slice(), &[target_block]);
        assert!(
            bytecode.cfg.inst_to_block[jump_inst + 1].is_none(),
            "always-taken JUMPI fallthrough should be dead"
        );
    }

    #[test]
    fn jumpi_with_true_condition_invalid_target_has_no_fallthrough() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x01
            PUSH1 0xFF
            JUMPI
            STOP
        ",
        );

        let (jump_inst, jump) =
            bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        let jump_block = bytecode.cfg.inst_to_block[jump_inst].unwrap();

        assert!(jump.has_const_jumpi_condition());
        assert!(jump.flags.contains(InstFlags::STATIC_JUMP | InstFlags::INVALID_JUMP));
        assert!(bytecode.cfg.blocks[jump_block].succs.is_empty());
        assert!(
            bytecode.cfg.inst_to_block[jump_inst + 1].is_none(),
            "always-taken invalid JUMPI fallthrough should be dead"
        );
    }

    #[test]
    fn jumpi_with_true_condition_keeps_unknown_target_dynamic() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x01
            CALLDATASIZE
            JUMPI
            STOP
        target:
            JUMPDEST
            STOP
        ",
        );

        let (_, jump) = bytecode.iter_insts().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(!jump.flags.contains(InstFlags::STATIC_JUMP));
        assert!(jump.has_const_jumpi_condition());
        assert!(!jump.can_fall_through());
        assert!(bytecode.has_dynamic_jumps);
    }

    /// Constant propagation through a diamond CFG (if-then-else merge).
    /// Both branches push the same constant, so the merge should preserve it.
    #[test]
    fn diamond_cfg_same_const() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x42              ; push same const on both paths
            CALLDATASIZE            ; condition
            PUSH %then_pc           ; then target
            JUMPI                   ; branch
            ; Else: push same const.
            PUSH %merge
            JUMP                    ; -> merge
            ; Then:
        then_pc:
            JUMPDEST
            PUSH %merge
            JUMP                    ; -> merge
            ; Merge:
        merge:
            JUMPDEST
            PUSH0
            MSTORE                  ; MSTORE(0, 0x42)
            STOP
        ",
        );
        // At the merge MSTORE, the value (operand 1) should still be 0x42
        // since both branches had the same constant on the stack.
        let mstore = bytecode.iter_insts().find(|(_, d)| d.opcode == op::MSTORE).unwrap().0;
        assert_eq!(bytecode.const_operand(mstore, 1), Some(U256::from(0x42)));
    }

    /// Diamond CFG where branches push different constants — the merge should
    /// yield None (Top) for const_operand.
    #[test]
    fn diamond_cfg_different_const() {
        let bytecode = analyze_asm(
            "
            CALLDATASIZE            ; condition
            PUSH %then_pc
            JUMPI                   ; branch
            ; Else: push 0xAA.
            PUSH1 0xAA
            PUSH %merge
            JUMP                    ; -> merge
            INVALID
            ; Then: push 0xBB.
        then_pc:
            JUMPDEST
            PUSH1 0xBB
            PUSH %merge
            JUMP                    ; -> merge
            ; Merge:
        merge:
            JUMPDEST
            PUSH0
            MSTORE                  ; MSTORE(0, ???)
            STOP
        ",
        );
        // At the merge MSTORE, the value (operand 1) should be None
        // since the two branches push different constants.
        let mstore = bytecode.iter_insts().find(|(_, d)| d.opcode == op::MSTORE).unwrap().0;
        assert_eq!(bytecode.const_operand(mstore, 1), None);
    }

    /// A private-return trampoline (`JUMPDEST; JUMP`) reached by an opaque jump
    /// can forward arbitrary stacks into a real function entry. The trampoline's
    /// Top jump must not be filtered out by the `is_private_return` check.
    #[test]
    fn trampoline_private_return_unsound() {
        let bytecode = analyze_asm(
            "
            ; Entry: branch to opaque path or call site 1.
            PUSH0                       ; pc=0
            CALLDATALOAD                ; pc=1
            PUSH %opaque_path           ; pc=2
            JUMPI                       ; pc=4
            ; Fallthrough: call site 1.
            PUSH %ret1                  ; pc=5
            PUSH %fn_entry              ; pc=7
            JUMP                        ; pc=9
        ret1:
            JUMPDEST                    ; pc=10
            POP                         ; pc=11
            ; Call site 2.
            PUSH %ret2                  ; pc=12
            PUSH %fn_entry              ; pc=14
            JUMP                        ; pc=16
        ret2:
            JUMPDEST                    ; pc=17
            POP                         ; pc=18
            STOP                        ; pc=19
            ; Opaque path (reachable from JUMPI).
        opaque_path:
            JUMPDEST                    ; pc=20
            PUSH0                       ; pc=21
            MLOAD                       ; pc=22: any_ret (Top)
            PUSH %tramp                 ; pc=23
            JUMP                        ; pc=25
            ; Trampoline: bare JUMPDEST + JUMP (is_private_return = true).
        tramp:
            JUMPDEST                    ; pc=26
            JUMP                        ; pc=27: target = any_ret (Top)
            ; Internal function.
        fn_entry:
            JUMPDEST                    ; pc=28
            PUSH1 0x42                  ; pc=29
            SWAP1                       ; pc=31
            JUMP                        ; pc=32: return
        ",
        );

        // The fn return JUMP at pc=32 must NOT be resolved — the trampoline
        // can reach fn_entry with any return address.
        let return_jump = bytecode.iter_insts().find(|(i, d)| bytecode.pc(*i) == 32 && d.is_jump());
        let (_, rj) = return_jump.unwrap();
        assert!(
            !rj.flags.contains(InstFlags::STATIC_JUMP),
            "unsound: return jump should not be resolved when trampoline exists"
        );
    }

    /// A JUMPI-based return where the target comes from the entry stack should
    /// be invalidated when a Top jump exists, just like JUMP-based returns.
    #[test]
    fn jumpi_return_unsound() {
        let bytecode = analyze_asm(
            "
            ; Call site 1.
            PUSH %ret1              ; pc=0
            PUSH %fn_entry          ; pc=2
            JUMP                    ; pc=4
        ret1:
            JUMPDEST                ; pc=5
            POP                     ; pc=6
            ; Call site 2.
            PUSH %ret2              ; pc=7
            PUSH %fn_entry          ; pc=9
            JUMP                    ; pc=11
        ret2:
            JUMPDEST                ; pc=12
            POP                     ; pc=13
            ; Opaque jump.
            PUSH0                   ; pc=14
            MLOAD                   ; pc=15
            JUMP                    ; pc=16: Top
            INVALID                 ; pc=17
            INVALID                 ; pc=18
            INVALID                 ; pc=19
            ; Internal function using JUMPI to return.
        fn_entry:
            JUMPDEST                ; pc=20
            PUSH1 0x42              ; pc=21: result
            SWAP1                   ; pc=23: [result, ret_addr]
            PUSH1 0x01              ; pc=24: condition (always true)
            SWAP1                   ; pc=26: [result, 1, ret_addr]
            JUMPI                   ; pc=27: conditional return (always taken)
            STOP                    ; pc=28: fallthrough (dead)
        ",
        );

        // The JUMPI at pc=27 should NOT be resolved — opaque jump can reach
        // fn_entry with any return address.
        let return_jumpi =
            bytecode.iter_insts().find(|(i, d)| bytecode.pc(*i) == 27 && d.is_jump());
        let (_, rj) = return_jumpi.unwrap();
        assert!(
            !rj.flags.contains(InstFlags::STATIC_JUMP),
            "unsound: JUMPI return should not be resolved"
        );
    }

    /// A suspect JUMPI block must not keep a fixpoint-derived constant
    /// condition when block-local analysis cannot prove it.
    #[test]
    fn suspect_jumpi_top_target_invalidates_const_condition() {
        let bytecode = analyze_asm(
            "
            ; Branch to an opaque caller or fall through to a known caller.
            PUSH0                       ; pc=0
            CALLDATALOAD                ; pc=1
            PUSH %opaque_caller         ; pc=2
            JUMPI                       ; pc=4
            ; Known caller reaches fn_entry with condition=1 and a Top target.
            PUSH1 0x01                  ; pc=5: condition
            PUSH0                       ; pc=7
            MLOAD                       ; pc=8: target = Top
            PUSH %fn_entry              ; pc=9
            JUMP                        ; pc=11
            ; Opaque caller could reach fn_entry with condition=0.
        opaque_caller:
            JUMPDEST                    ; pc=12
            PUSH0                       ; pc=13: condition
            PUSH %taken                 ; pc=14: target
            PUSH0                       ; pc=16
            MLOAD                       ; pc=17: jump destination = Top
            JUMP                        ; pc=18
        fn_entry:
            JUMPDEST                    ; pc=19
            JUMPI                       ; pc=20
            STOP                        ; pc=21
        taken:
            JUMPDEST                    ; pc=22
            STOP                        ; pc=23
        ",
        );

        let (_, jumpi) = bytecode
            .iter_insts()
            .find(|(i, d)| bytecode.pc(*i) == 20 && d.opcode == op::JUMPI)
            .unwrap();
        assert!(
            !jumpi.has_const_jumpi_condition(),
            "suspect JUMPI should not keep a non-local const condition"
        );
        assert!(
            !jumpi.flags.contains(InstFlags::STATIC_JUMP),
            "suspect JUMPI target should remain dynamic"
        );
    }

    /// A block-local false JUMPI condition is sound even in a suspect block.
    #[test]
    fn suspect_jumpi_top_target_keeps_local_false_condition() {
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH %opaque
            JUMPI
            PUSH %fn_entry
            JUMP
        opaque:
            JUMPDEST
            PUSH0
            MLOAD
            JUMP
        fn_entry:
            JUMPDEST
            PUSH0
            PUSH0
            MLOAD
            JUMPI
            STOP
        taken:
            JUMPDEST
            STOP
        ",
        );

        let (jump_inst, jumpi) =
            bytecode.iter_insts().rev().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(jumpi.has_const_jumpi_condition());
        assert!(jumpi.flags.contains(InstFlags::STATIC_JUMP));
        assert_eq!(jumpi.static_jump_target(), jump_inst + 1);
    }

    /// A block-local true JUMPI condition is also sound in a suspect block, but
    /// an unknown target must stay dynamic and must not retain a fallthrough.
    #[test]
    fn suspect_jumpi_top_target_keeps_local_true_condition() {
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH %opaque
            JUMPI
            PUSH %fn_entry
            JUMP
        opaque:
            JUMPDEST
            PUSH0
            MLOAD
            JUMP
        fn_entry:
            JUMPDEST
            PUSH1 0x01
            PUSH0
            MLOAD
            JUMPI
            STOP
        taken:
            JUMPDEST
            STOP
        ",
        );

        let (jump_inst, jumpi) =
            bytecode.iter_insts().rev().find(|(_, data)| data.opcode == op::JUMPI).unwrap();
        assert!(jumpi.has_const_jumpi_condition());
        assert!(!jumpi.flags.contains(InstFlags::STATIC_JUMP));
        assert!(!jumpi.can_fall_through());
        assert!(bytecode.has_dynamic_jumps);
        assert!(
            bytecode.cfg.inst_to_block[jump_inst + 1].is_none(),
            "always-taken dynamic JUMPI fallthrough should be dead"
        );
    }

    #[test]
    fn truncated_stack_does_not_make_jumpi_unconditional() {
        // Both paths reach `body` in one context, as their stacks are deeper than
        // `MAX_CONTEXT_DEPTH`. The join keeps the shorter stack, which does not hold the JUMPI
        // condition.
        let src = format!(
            "
            CALLDATASIZE
            PUSH %short
            JUMPI
            PUSH0
            PUSH %taken
            {0}
            PUSH %body
            JUMP
        short:
            JUMPDEST
            PUSH %taken
            {0}
            PUSH %body
            JUMP
        body:
            JUMPDEST
            {1}
            JUMPI
            PUSH1 0x01
            PUSH1 0x01
            SSTORE
            STOP
        taken:
            JUMPDEST
            PUSH1 0x02
            PUSH1 0x02
            SSTORE
            STOP
            ",
            "PUSH1 0x11\n".repeat(MAX_CONTEXT_DEPTH + 2),
            "POP\n".repeat(MAX_CONTEXT_DEPTH + 2),
        );
        let bytecode = analyze_asm(&src);

        let (jump_inst, jumpi) =
            bytecode.iter_insts().filter(|(_, data)| data.opcode == op::JUMPI).nth(1).unwrap();
        assert_eq!(bytecode.snapshots.inputs[jump_inst].len(), 2);
        assert!(jumpi.flags.contains(InstFlags::STATIC_JUMP));
        assert!(!jumpi.has_const_jumpi_condition());

        let (_, fallthrough_store) =
            bytecode.iter_all_insts().find(|(_, data)| data.opcode == op::SSTORE).unwrap();
        assert!(!fallthrough_store.is_dead_code());
    }

    /// A third caller reaches the function entry with a known callee (static
    /// PUSH+JUMP) but an opaque return address (from MLOAD). The function's
    /// return jump must not be resolved because the return address is Top.
    #[test]
    fn opaque_return_addr_caller_unsound() {
        let bytecode = analyze_asm(
            "
            ; Entry: branch to opaque caller or fallthrough.
            PUSH0                       ; pc=0
            CALLDATALOAD                ; pc=1
            PUSH %opaque_caller         ; pc=2
            JUMPI                       ; pc=4
            ; Fallthrough: call site 1.
            PUSH %ret1                  ; pc=5
            PUSH %fn_entry              ; pc=7
            JUMP                        ; pc=9
        ret1:
            JUMPDEST                    ; pc=10
            POP                         ; pc=11
            ; Call site 2.
            PUSH %ret2                  ; pc=12
            PUSH %fn_entry              ; pc=14
            JUMP                        ; pc=16
        ret2:
            JUMPDEST                    ; pc=17
            POP                         ; pc=18
            STOP                        ; pc=19
            ; Opaque third caller: loads return addr from memory (reachable via JUMPI).
        opaque_caller:
            JUMPDEST                    ; pc=20
            PUSH0                       ; pc=21
            MLOAD                       ; pc=22: any_ret
            PUSH %fn_entry              ; pc=23
            JUMP                        ; pc=25: -> fn_entry with arbitrary return addr
            ; Internal function.
        fn_entry:
            JUMPDEST                    ; pc=26
            PUSH1 0x42                  ; pc=27
            SWAP1                       ; pc=29
            JUMP                        ; pc=30: return
        ",
        );

        // The return JUMP at pc=30 must NOT be resolved — the opaque caller
        // at pc=25 reaches fn_entry with an arbitrary return address.
        let return_jump = bytecode.iter_insts().find(|(i, d)| bytecode.pc(*i) == 30 && d.is_jump());
        let (_, rj) = return_jump.unwrap();
        assert!(
            !rj.flags.contains(InstFlags::STATIC_JUMP),
            "unsound: return jump should not be resolved with opaque caller"
        );
    }

    /// Loop counter (inline increment) must be Top after fixpoint.
    #[test]
    fn const_snapshot_loop_counter_inline() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x00          ; inst 0: i = 0
        loop:
            JUMPDEST            ; inst 1
            DUP1                ; inst 2
            PUSH1 0x0a          ; inst 3
            GT                  ; inst 4: i < 10
            ISZERO              ; inst 5
            PUSH %exit          ; inst 6
            JUMPI               ; inst 7
            PUSH1 0x01          ; inst 8
            ADD                 ; inst 9: i++
            PUSH %loop          ; inst 10
            JUMP                ; inst 11
        exit:
            JUMPDEST            ; inst 12
            POP                 ; inst 13
            STOP                ; inst 14
        ",
        );

        // DUP1 at inst 2: counter is loop-variant.
        assert_eq!(bytecode.const_operand(Inst::new(2), 0), None);
        // ADD at inst 9: TOS=1 (const), second=counter (Top).
        assert_eq!(bytecode.const_operand(Inst::new(9), 0), Some(U256::ONE));
        assert_eq!(bytecode.const_operand(Inst::new(9), 1), None);
    }

    /// Loop counter incremented via single-caller subroutine must be Top.
    #[test]
    fn const_snapshot_loop_counter_subroutine() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x00
        loop:
            JUMPDEST            ; inst 1
            DUP1                ; inst 2
            PUSH1 0x0a
            GT
            ISZERO
            PUSH %exit
            JUMPI
            ; call add_fn(counter, 1) -> counter'
            PUSH1 0x01
            DUP2
            PUSH %ret
            SWAP2
            SWAP1
            PUSH %add_fn
            JUMP
        ret:
            JUMPDEST
            SWAP1
            POP
            PUSH %loop
            JUMP
        exit:
            JUMPDEST
            POP
            STOP
        add_fn:
            JUMPDEST
            PUSH1 0x00
            DUP3
            DUP3
            ADD
            SWAP1
            POP
            DUP1
            DUP3
            GT
            ISZERO
            PUSH %add_ok
            JUMPI
            INVALID
        add_ok:
            JUMPDEST
            SWAP3
            SWAP2
            POP
            POP
            JUMP
        ",
        );

        assert_eq!(bytecode.const_operand(Inst::new(2), 0), None);
    }

    /// Loop counter incremented via multi-caller subroutine must be Top.
    #[test]
    fn const_snapshot_loop_counter_multi_caller() {
        let bytecode = analyze_asm(
            "
            ; Caller 1 (outside loop) to make add_fn multi-target.
            PUSH1 0x02
            PUSH1 0x03
            PUSH %ret_outer
            SWAP2
            SWAP1
            PUSH %add_fn
            JUMP
        ret_outer:
            JUMPDEST            ; inst 7
            POP
            PUSH1 0x00          ; i = 0
        loop:
            JUMPDEST            ; inst 10
            DUP1                ; inst 11
            PUSH1 0x0a
            GT
            ISZERO
            PUSH %exit
            JUMPI
            ; Caller 2 (inside loop).
            PUSH1 0x01
            DUP2
            PUSH %ret_loop
            SWAP2
            SWAP1
            PUSH %add_fn
            JUMP
        ret_loop:
            JUMPDEST
            SWAP1
            POP
            PUSH %loop
            JUMP
        exit:
            JUMPDEST
            POP
            STOP
        add_fn:
            JUMPDEST
            PUSH1 0x00
            DUP3
            DUP3
            ADD
            SWAP1
            POP
            DUP1
            DUP3
            GT
            ISZERO
            PUSH %add_ok
            JUMPI
            INVALID
        add_ok:
            JUMPDEST
            SWAP3
            SWAP2
            POP
            POP
            JUMP
        ",
        );

        // DUP1 at inst 11: counter is loop-variant.
        assert_eq!(bytecode.const_operand(Inst::new(11), 0), None);
    }

    /// loopExp statetest: loop counters through checked_add subroutine must be Top.
    #[test]
    fn const_snapshot_loop_exp() {
        // tests/GeneralStateTests/VMTests/vmPerformance/loopExp.json
        let bytecode = analyze_hex(
            "608060405234801561001057600080fd5b506004361061004c5760003560e01c806363ad973214610051578063a34f51e414610081578063dfaf4a87146100b1578063f078245b146100e1575b600080fd5b61006b60048036038101906100669190610275565b610111565b60405161007891906102d7565b60405180910390f35b61009b60048036038101906100969190610275565b610199565b6040516100a891906102d7565b60405180910390f35b6100cb60048036038101906100c69190610275565b6101cb565b6040516100d891906102d7565b60405180910390f35b6100fb60048036038101906100f69190610275565b610208565b60405161010891906102d7565b60405180910390f35b60008083905060005b838110156101865785820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915085820a915060108161017f9190610321565b905061011a565b5080600081905550809150509392505050565b6000805b828110156101b9576001816101b29190610321565b905061019d565b508260008190555082905093925050505650565b60008083905060005b838110156101f55785820a91506001816101ee9190610321565b90506101d4565b5080600081905550809150509392505050565b6000805b82811015610228576010816102219190610321565b905061020c565b50826000819055508290509392505050565b600080fd5b6000819050919050565b6102528161023f565b811461025d57600080fd5b50565b60008135905061026f81610249565b92915050565b60008060006060848603121561028e5761028d61023a565b5b600061029c86828701610260565b93505060206102ad86828701610260565b92505060406102be86828701610260565b9150509250925092565b6102d18161023f565b82525050565b60006020820190506102ec60008301846102c8565b92915050565b7f4e487b71000000000000000000000000000000000000000000000000000000006000526011600452602460006000fd5b600061032c8261023f565b91506103378361023f565b9250828201905080821115610340f5761034e6102f2565b5b9291505056fea264697066735822122000b253a2b246ddfeb0f09ddf5ec7dc70e2235369b8a2f3f9b55e32a5ca01e04ff64736f6c63430008180033",
        );

        // simpleLoop LT at pc=416: counter (depth=0) is loop-variant.
        let lt_inst = bytecode
            .insts
            .iter_enumerated()
            .find(|(i, inst)| inst.opcode == op::LT && bytecode.pc(*i) == 416)
            .map(|(i, _)| i)
            .expect("LT at pc=416 not found");
        assert_eq!(bytecode.const_operand(lt_inst, 0), None);
    }

    /// Suspect-block invalidation must preserve block-local constants while
    /// restoring cross-block values to Top.
    ///
    /// The target block receives 0xAA from a predecessor (cross-block) and also
    /// has block-local PUSHes. Without restoration, the fixpoint's precise 0xAA
    /// would persist unsoundly; with restoration, only block-local constants survive.
    #[test]
    fn suspect_block_preserves_local_const_operand() {
        let bytecode = analyze_asm(
            "
            PUSH1 0xAA          ; inst 0: inherited into target
            CALLDATASIZE        ; inst 1: unknown condition
            PUSH %target        ; inst 2
            JUMPI               ; inst 3: target reachable, fallthrough reachable

            PUSH0               ; inst 4
            CALLDATALOAD        ; inst 5: Top
            JUMP                ; inst 6: unresolved -> triggers suspect invalidation

        target:
            JUMPDEST            ; inst 7: suspect seed
            PUSH0               ; inst 8: block-local constant
            MSTORE              ; inst 9: offset=0 (local), value=inherited 0xAA
            STOP                ; inst 10
        ",
        );
        assert!(bytecode.has_dynamic_jumps);
        // Block-local constant survives.
        assert_eq!(bytecode.const_output(Inst::new(8)), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(9), 0), Some(U256::ZERO));
        // Cross-block value must be restored to Top.
        assert_eq!(bytecode.const_operand(Inst::new(9), 1), None);
    }

    /// Same as above but asserting const_output: block-local PUSH output survives,
    /// but anything depending on cross-block values becomes None.
    #[test]
    fn suspect_block_preserves_local_const_output() {
        let bytecode = analyze_asm(
            "
            PUSH1 0xAA          ; inst 0: inherited into target
            CALLDATASIZE        ; inst 1
            PUSH %target        ; inst 2
            JUMPI               ; inst 3

            PUSH0               ; inst 4
            CALLDATALOAD        ; inst 5: Top
            JUMP                ; inst 6: unresolvable

        target:
            JUMPDEST            ; inst 7: suspect seed
            PUSH1 0x01          ; inst 8: block-local const output
            ADD                 ; inst 9: 0xAA + 0x01 in fixpoint, Top + 0x01 after restore
            STOP                ; inst 10
        ",
        );
        assert!(bytecode.has_dynamic_jumps);
        // Purely local push output preserved.
        assert_eq!(bytecode.const_output(Inst::new(8)), Some(U256::ONE));
        // ADD: local operand preserved, inherited operand restored to Top.
        assert_eq!(bytecode.const_operand(Inst::new(9), 0), Some(U256::ONE));
        assert_eq!(bytecode.const_operand(Inst::new(9), 1), None);
        assert_eq!(bytecode.const_output(Inst::new(9)), None);
    }

    /// Fallthrough block after a JUMPI in a suspect JUMPDEST block becomes
    /// suspect via forward propagation. Block-local constants in the fallthrough
    /// survive, but cross-block inherited values are restored to Top.
    #[test]
    fn suspect_jumpi_fallthrough_preserves_consts() {
        let bytecode = analyze_asm(
            "
            PUSH1 0xAA          ; inst 0: inherited into target
            CALLDATASIZE        ; inst 1: unknown condition
            PUSH %target        ; inst 2
            JUMPI               ; inst 3

            PUSH0               ; inst 4
            CALLDATALOAD        ; inst 5: Top
            JUMP                ; inst 6: unresolved

        target:
            JUMPDEST            ; inst 7: suspect seed, entry stack [0xAA]
            PUSH1 0x01          ; inst 8: always take
            PUSH %after         ; inst 9
            JUMPI               ; inst 10: leaves [0xAA] for after
            STOP                ; inst 11

        after:
            JUMPDEST            ; inst 12: suspect via forward propagation
            PUSH0               ; inst 13: block-local constant
            MSTORE              ; inst 14: offset=0 (local), value=inherited 0xAA
            STOP                ; inst 15
        ",
        );
        assert!(bytecode.has_dynamic_jumps);
        // Block-local constant in propagated-suspect fallthrough survives.
        assert_eq!(bytecode.const_output(Inst::new(13)), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(14), 0), Some(U256::ZERO));
        // Cross-block inherited value restored to Top.
        assert_eq!(bytecode.const_operand(Inst::new(14), 1), None);
    }

    /// Non-suspect blocks must retain their fixpoint-derived snapshots even when
    /// invalidation runs elsewhere. The fallthrough after a JUMPI is NOT a
    /// JUMPDEST, so it is never suspect. Block-local constants, const-folded
    /// results, and cross-block values resolved via the single-predecessor path
    /// (MLOAD after MSTORE) must all be preserved.
    #[test]
    fn nonsuspect_fallthrough_keeps_fixpoint_snapshots() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x69          ; inst 0: cross-block value
            CALLDATASIZE        ; inst 1: unknown condition
            PUSH %target        ; inst 2
            JUMPI               ; inst 3
            ; Non-suspect fallthrough block (no JUMPDEST).
            PUSH1 0x0A          ; inst 4
            PUSH1 0x0B          ; inst 5
            ADD                 ; inst 6: 0x0A + 0x0B = 0x15, block-local
            PUSH0               ; inst 7
            MSTORE              ; inst 8
            MLOAD               ; inst 9: cross-block via single predecessor
            PUSH1 0x01          ; inst 10
            MSTORE              ; inst 11
            JUMPDEST            ; inst 12: falls through into the opaque jump
            ; Opaque dynamic jump — triggers suspect invalidation.
            JUMPDEST            ; inst 13
            PUSH0               ; inst 14
            CALLDATALOAD        ; inst 15: Top
            JUMP                ; inst 16: unresolvable
        target:
            JUMPDEST            ; inst 17: suspect seed
            PUSH1 0x42          ; inst 18
            STOP                ; inst 19
        ",
        );
        assert!(bytecode.has_dynamic_jumps);
        // Fallthrough block: block-local constants survive.
        // ADD
        assert_eq!(bytecode.const_operand(Inst::new(6), 0), Some(U256::from(0x0B)));
        assert_eq!(bytecode.const_operand(Inst::new(6), 1), Some(U256::from(0x0A)));
        assert_eq!(bytecode.const_output(Inst::new(6)), Some(U256::from(0x15)));
        // MSTORE 1
        assert_eq!(bytecode.const_operand(Inst::new(8), 0), Some(U256::ZERO));
        assert_eq!(bytecode.const_operand(Inst::new(8), 1), Some(U256::from(0x15)));
        // MLOAD: single predecessor (no JUMPDEST), so cross-block value resolves.
        assert_eq!(bytecode.const_operand(Inst::new(9), 0), Some(U256::from(0x69)));
        // Target block: block-local PUSH survives.
        assert_eq!(bytecode.const_output(Inst::new(18)), Some(U256::from(0x42)));
    }

    /// JUMPI in a suspect JUMPDEST block: the fallthrough block inherits
    /// suspect status via forward propagation. Incoming stack values from the
    /// predecessor become Top (block-local pass doesn't see cross-block flow),
    /// while block-local PUSHes in the fallthrough are preserved.
    #[test]
    fn suspect_jumpi_fallthrough_mixed_operands() {
        let bytecode = analyze_asm(
            "
            ; Jump into the suspect JUMPDEST block with a value on the stack.
            PUSH1 0xAA          ; inst 0: will be on stack at entry to target
            PUSH0               ; inst 1
            CALLDATALOAD        ; inst 2: unknown condition
            PUSH %opaque        ; inst 3
            JUMPI               ; inst 4
            PUSH %target        ; inst 5
            JUMP                ; inst 6: static jump
            ; Opaque dynamic jump — triggers suspect invalidation.
        opaque:
            JUMPDEST            ; inst 7
            PUSH0               ; inst 8
            CALLDATALOAD        ; inst 9: Top
            JUMP                ; inst 10: unresolvable
        target:
            ; Suspect JUMPDEST block with JUMPI.
            ; Entry stack: [0xAA] (from pred, but unknown to block-local pass).
            JUMPDEST            ; inst 11: suspect seed
            PUSH1 0x01          ; inst 12: condition
            PUSH %after         ; inst 13
            JUMPI               ; inst 14: pops condition+target, [0xAA] remains
            STOP                ; inst 15
        after:
            ; Fallthrough/target of JUMPI — suspect via propagation.
            ; Entry stack: [0xAA] (inherited, but Top in block-local pass).
            JUMPDEST            ; inst 16
            PUSH1 0xBB          ; inst 17: block-local
            ADD                 ; inst 18: 0xAA + 0xBB — depth 1 is incoming
            STOP                ; inst 19
        ",
        );
        assert!(bytecode.has_dynamic_jumps);
        // ADD at inst 18: TOS (depth 0) = 0xBB (block-local), depth 1 = Top
        // (incoming from suspect predecessor, block-local pass saw it as Top).
        assert_eq!(bytecode.const_operand(Inst::new(18), 0), Some(U256::from(0xBB)));
        assert_eq!(bytecode.const_operand(Inst::new(18), 1), None);
        assert_eq!(bytecode.const_output(Inst::new(18)), None);
    }

    /// Bytecode that is just STOP — minimal edge case.
    #[test]
    fn empty_bytecode() {
        let bytecode = analyze_asm("STOP");
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A dynamic jump in dead code should not cause `has_dynamic_jumps` to be true.
    #[test]
    fn dead_code_dynamic_jump() {
        let bytecode = analyze_asm(
            "
            STOP
            PUSH0
            CALLDATALOAD
            JUMP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A dynamic jump that only the jump itself could reach, behind a `JUMPDEST`, as in data
    /// contracts whose code starts with `STOP`.
    #[test]
    fn unreached_jumpdest_dynamic_jump() {
        let bytecode = analyze_asm(
            "
            STOP
            JUMPDEST
            PUSH0
            CALLDATALOAD
            JUMP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// The return trampoline of a function that never returns is unreachable once every
    /// other jump is resolved, even though its block-local operand is unknown.
    #[test]
    fn unreached_return_trampoline() {
        let src = |opaque: &str| {
            format!(
                "
                {opaque}
                PUSH %tramp
                PUSH %ret
                PUSH %func
                JUMP
            ret:
                JUMPDEST
                PUSH %exit
                JUMP
            tramp:
                JUMPDEST
                JUMP            ; reached only if `exit` returned
            func:
                JUMPDEST
                JUMP            ; returns to `ret`
            exit:
                JUMPDEST
                STOP
            opaque:
                JUMPDEST
                PUSH0
                MLOAD
                JUMP
            "
            )
        };
        let bytecode = analyze_asm(&src(""));
        assert!(!bytecode.has_dynamic_jumps);

        // An unresolved jump could reach the trampoline, so it must stay dynamic.
        let bytecode = analyze_asm(&src("PUSH0 CALLDATALOAD PUSH %opaque JUMPI"));
        assert!(bytecode.has_dynamic_jumps);
        let tramp_jump = bytecode
            .iter_insts()
            .filter(|(_, d)| d.is_jump() && !d.is_dead_code())
            // `tramp` is the first `JUMPDEST JUMP` block.
            .find(|&(i, _)| bytecode.inst(i - 1).is_jumpdest())
            .unwrap();
        assert!(!tramp_jump.1.flags.contains(InstFlags::STATIC_JUMP));
    }

    /// Many callers to a shared internal function through relay blocks.
    /// When the fixpoint iteration cap is exceeded, partially-discovered
    /// multi-target jumps must NOT be committed as closed MULTI_JUMPs.
    #[test]
    fn non_converged_multi_jump_stays_dynamic() {
        // Generate bytecode with k call sites, b relay blocks, and one shared function.
        // Each call site pushes a different return address and jumps through the relay
        // chain into the shared function, which does PUSH1 0x42; SWAP1; JUMP.
        // With enough callers and relays, every fixpoint attempt exceeds its budget
        // before all return addresses are discovered.
        let k = 200;
        let b = 200;
        let mut lines = Vec::new();
        lines.push("PUSH %call0".to_string());
        lines.push("JUMP".to_string());
        for i in 0..k {
            lines.push(format!("call{i}:"));
            lines.push("JUMPDEST".to_string());
            lines.push(format!("PUSH %ret{i}"));
            lines.push("PUSH %relay0".to_string());
            lines.push("JUMP".to_string());

            lines.push(format!("ret{i}:"));
            lines.push("JUMPDEST".to_string());
            lines.push("POP".to_string());
            if i + 1 < k {
                lines.push(format!("PUSH %call{}", i + 1));
                lines.push("JUMP".to_string());
            } else {
                lines.push("STOP".to_string());
            }
        }
        for i in 0..b {
            lines.push(format!("relay{i}:"));
            lines.push("JUMPDEST".to_string());
            if i + 1 < b {
                lines.push(format!("PUSH %relay{}", i + 1));
            } else {
                lines.push("PUSH %fn_entry".to_string());
            }
            lines.push("JUMP".to_string());
        }
        lines.push("fn_entry:".to_string());
        lines.push("JUMPDEST".to_string());
        lines.push("PUSH1 0x42".to_string());
        lines.push("SWAP1".to_string());
        lines.push("JUMP".to_string());

        let bytecode = analyze_asm(&lines.join("\n"));

        // The shared function's return JUMP must NOT be committed as a closed
        // MULTI_JUMP when the fixpoint didn't converge — doing so would produce
        // an incomplete switch table missing some valid return addresses.
        let return_jump = bytecode
            .iter_insts()
            .find(|(_, d)| d.is_jump() && d.flags.contains(InstFlags::MULTI_JUMP));
        assert!(return_jump.is_none(), "non-converged fixpoint must not commit partial MULTI_JUMP");
        // It should remain dynamic instead.
        assert!(
            bytecode.has_dynamic_jumps,
            "return jump should remain dynamic when fixpoint doesn't converge"
        );
    }

    /// Many callers of a shared return block over a deep stack. Full and Coarse contexts exceed
    /// their partition limits, so the context-insensitive fixpoint is the last attempt. Its work
    /// grows with callers squared times stack depth, so it must run out of budget rather than
    /// converge, leaving the return jump dynamic.
    #[test]
    fn insensitive_fixpoint_work_is_bounded() {
        let callers = 8200;
        let depth = 64;
        let mut lines = vec!["CALLDATASIZE".to_string(); depth];
        lines.push("PUSH %caller0".to_string());
        lines.push("JUMP".to_string());
        for i in 0..callers {
            lines.push(format!("caller{i}:"));
            lines.push("JUMPDEST".to_string());
            if i + 1 < callers {
                lines.push(format!("PUSH %caller{}", i + 1));
                lines.push("PUSH %func".to_string());
                lines.push("JUMP".to_string());
            } else {
                lines.push("STOP".to_string());
            }
        }
        lines.extend(["func:", "JUMPDEST", "JUMP"].map(String::from));

        let bytecode = analyze_asm(&lines.join("\n"));
        assert!(bytecode.has_dynamic_jumps());
    }

    /// Snapshots inside a function analyzed once per caller context must join the values of
    /// all contexts: an argument that differs between callers is not a constant.
    #[test]
    fn context_snapshots_join_callers() {
        let bytecode = analyze_asm(
            "
            PUSH %ret1          ; inst 0
            PUSH1 0x07          ; inst 1: same in both calls
            PUSH1 0x01          ; inst 2: differs between calls
            PUSH %func          ; inst 3
            JUMP                ; inst 4
        ret1:
            JUMPDEST            ; inst 5
            POP                 ; inst 6
            PUSH %ret2          ; inst 7
            PUSH1 0x07          ; inst 8
            PUSH1 0x02          ; inst 9
            PUSH %func          ; inst 10
            JUMP                ; inst 11
        ret2:
            JUMPDEST            ; inst 12
            POP                 ; inst 13
            STOP                ; inst 14
        func:
            JUMPDEST            ; inst 15
            ADD                 ; inst 16: [ret, 7 + arg]
            SWAP1               ; inst 17
            JUMP                ; inst 18: return
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(bytecode.const_operand(Inst::new(16), 1), Some(U256::from(7)));
        assert_eq!(bytecode.const_operand(Inst::new(16), 0), None);
        assert_eq!(bytecode.const_output(Inst::new(16)), None);
        let targets = bytecode.multi_jump_targets(Inst::new(18)).unwrap();
        assert_eq!(targets, [Inst::new(5), Inst::new(12)]);
    }

    /// A recursive function's return must resolve to all of its return sites.
    #[test]
    fn recursive_return() {
        let bytecode = analyze_asm(
            "
            PUSH %done
            CALLDATASIZE
            PUSH %rec
            JUMP
        done:
            JUMPDEST
            STOP
        rec:
            JUMPDEST            ; [ret, n]
            DUP1
            ISZERO
            PUSH %base
            JUMPI
            PUSH %back          ; [ret, n, back]
            PUSH1 0x01
            DUP3
            SUB                 ; [ret, n, back, n - 1]
            PUSH %rec
            JUMP
        back:
            JUMPDEST            ; [ret, n]
        base:
            JUMPDEST            ; [ret, n]
            POP
            JUMP                ; return
        ",
        );
        let (ret, _) = bytecode.iter_insts().rfind(|(_, d)| d.is_jump()).unwrap();
        let mut targets: Vec<_> =
            bytecode.multi_jump_targets(ret).unwrap().iter().map(|&t| bytecode.pc(t)).collect();
        targets.sort_unstable();
        assert_eq!(targets, [6, 23]);
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A `try`/`catch` shape: the failed call jumps to the block that checks the decoded
    /// result with one entry fewer. That jump is only taken when the call result is zero, so
    /// the check never jumps on, and `ok` keeps only the success path's stack.
    #[test]
    fn failed_call_does_not_take_success_jump() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x42
            PUSH %ret
            PUSH %func
            JUMP
        ret:
            JUMPDEST
            STOP
        func:
            JUMPDEST            ; [0x42, ret]
            CALLDATASIZE        ; [0x42, ret, success]
            DUP1
            ISZERO
            PUSH %check
            JUMPI
            PUSH1 0x80          ; [0x42, ret, success, value]
            SWAP1               ; [0x42, ret, value, success]
        check:
            JUMPDEST
            PUSH %ok
            JUMPI
            PUSH0
            DUP1
            REVERT
        ok:
            JUMPDEST            ; [0x42, ret, value]
            POP
            JUMP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// The fall-through of a `JUMPI` knows that its condition is zero.
    #[test]
    fn jumpi_fallthrough_knows_zero_condition() {
        let bytecode = analyze_asm(
            "
            CALLDATASIZE
            DUP1
            PUSH %nonzero
            JUMPI
            PUSH1 0x01
            ADD
            PUSH0
            SSTORE
            STOP
        nonzero:
            JUMPDEST
            STOP
        ",
        );
        let (add, _) = bytecode.iter_insts().find(|(_, d)| d.opcode == op::ADD).unwrap();
        assert_eq!(bytecode.const_operand(add, 1), Some(U256::ZERO));
    }

    /// A return address below 64 other stack entries, as in deep call chains.
    #[test]
    fn deep_return_address() {
        let src = format!(
            "
            PUSH %ret
            {}
            PUSH %func
            JUMP
        ret:
            JUMPDEST
            STOP
        func:
            JUMPDEST
            {}
            JUMP
            ",
            "PUSH1 0x11\n".repeat(70),
            "POP\n".repeat(70),
        );
        let bytecode = analyze_asm(&src);
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// Stores `0x80` at `0x40`, runs `src`, and returns the folded value of `mload(0x40)`.
    fn mload_0x40_after(src: &str) -> Option<U256> {
        let bytecode = analyze_asm(&format!(
            "PUSH 0x80 PUSH 0x40 MSTORE\n{src}\nPUSH 0x40 MLOAD PUSH0 MSTORE STOP"
        ));
        let (mload, _) = bytecode.iter_insts().rfind(|(_, data)| data.opcode == op::MLOAD).unwrap();
        bytecode.const_output(mload)
    }

    #[test]
    fn mload_known_word() {
        let known = Some(U256::from(0x80));
        assert_eq!(mload_0x40_after(""), known);
        assert_eq!(mload_0x40_after("PUSH %next\nJUMP\nnext:\nJUMPDEST"), known);
        // Memory that was never written is not assumed to be zero.
        assert_eq!(mload_0x40_after("PUSH 0x60 MLOAD POP"), known);
        let bytecode = analyze_asm("PUSH 0x60 MLOAD PUSH0 MSTORE STOP");
        assert_eq!(bytecode.const_output(Inst::new(1)), None);
    }

    #[test]
    fn mload_known_word_after_mstore() {
        let known = Some(U256::from(0x80));
        assert_eq!(mload_0x40_after("PUSH 1 PUSH0 CALLDATALOAD MSTORE"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x40 MSTORE"), Some(U256::ONE));
        assert_eq!(mload_0x40_after("PUSH0 CALLDATALOAD PUSH 0x40 MSTORE"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x21 MSTORE"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x5f MSTORE"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x20 MSTORE"), known);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x60 MSTORE"), known);
    }

    #[test]
    fn mload_known_word_after_mstore8() {
        let known = Some(U256::from(0x80));
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x40 MSTORE8"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x5f MSTORE8"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH0 CALLDATALOAD MSTORE8"), None);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x3f MSTORE8"), known);
        assert_eq!(mload_0x40_after("PUSH 1 PUSH 0x60 MSTORE8"), known);
    }

    #[test]
    fn mload_known_word_after_copy() {
        let known = Some(U256::from(0x80));
        // Operands: length, source offset, destination offset (, address).
        let cases = [
            ("PUSH 1 PUSH0 PUSH 0x5f", None),
            ("PUSH 0x21 PUSH0 PUSH 0x20", None),
            ("PUSH 1 PUSH0 PUSH0 CALLDATALOAD", None),
            ("PUSH0 CALLDATALOAD PUSH0 PUSH 0x5f", None),
            ("PUSH0 CALLDATALOAD PUSH0 PUSH 0x60", known),
            ("PUSH 0x20 PUSH0 PUSH 0x20", known),
            ("PUSH0 PUSH0 PUSH0 CALLDATALOAD", known),
        ];
        for op in ["CALLDATACOPY", "CODECOPY", "RETURNDATACOPY", "MCOPY", "PUSH0 EXTCODECOPY"] {
            for (operands, expected) in cases {
                // A constant `CODECOPY` ending the word rewrites it, see
                // `mload_word_after_codecopy`.
                if op == "CODECOPY" && operands == "PUSH 1 PUSH0 PUSH 0x5f" {
                    continue;
                }
                assert_eq!(
                    mload_0x40_after(&format!("{operands} {op}")),
                    expected,
                    "{operands} {op}"
                );
            }
        }
    }

    /// A constant `CODECOPY` that ends the word at `0x40` replaces its last bytes with the copied
    /// code, and zeros past the end of the code.
    #[test]
    fn mload_word_after_codecopy() {
        for len in [1u8, 2, 16, 31, 32] {
            for src in [0usize, 3, 20, 1000] {
                let copy = format!("PUSH {len} PUSH {src} PUSH {:#x} CODECOPY", 0x60 - len);
                let code = crate::parse_asm(&format!(
                    "PUSH 0x80 PUSH 0x40 MSTORE\n{copy}\nPUSH 0x40 MLOAD PUSH0 MSTORE STOP"
                ))
                .unwrap();
                let mut memory = [0u8; 0x60];
                memory[0x5f] = 0x80;
                for (i, byte) in memory[0x60 - len as usize..].iter_mut().enumerate() {
                    *byte = code.get(src + i).copied().unwrap_or(0);
                }
                let expected = U256::from_be_slice(&memory[0x40..]);
                assert_eq!(mload_0x40_after(&copy), Some(expected), "{copy}");
            }
        }
    }

    #[test]
    fn mload_known_word_after_call() {
        let known = Some(U256::from(0x80));
        // Operands: return length and offset, then arguments that cover the word, which calls
        // only read.
        let cases = [
            ("PUSH 1 PUSH 0x5f", None),
            ("PUSH 1 PUSH0 CALLDATALOAD", None),
            ("PUSH0 CALLDATALOAD PUSH 0x20", None),
            ("PUSH 0x20 PUSH 0x60", known),
            ("PUSH0 CALLDATALOAD PUSH 0x60", known),
            ("PUSH0 PUSH0 CALLDATALOAD", known),
        ];
        for op in [
            "PUSH0 PUSH 4 GAS CALL",
            "PUSH0 PUSH 4 GAS CALLCODE",
            "PUSH 4 GAS STATICCALL",
            "PUSH 4 GAS DELEGATECALL",
        ] {
            for (ret, expected) in cases {
                let src = format!("{ret} PUSH 0x40 PUSH0 {op} POP");
                assert_eq!(mload_0x40_after(&src), expected, "{src}");
            }
        }
    }

    #[test]
    fn mload_known_word_join() {
        let join = |a: &str, b: &str| {
            mload_0x40_after(&format!(
                "PUSH0 CALLDATALOAD PUSH %b JUMPI\n{a}\nPUSH %join JUMP\nb:\nJUMPDEST\n{b}\njoin:\nJUMPDEST"
            ))
        };
        assert_eq!(join("", ""), Some(U256::from(0x80)));
        assert_eq!(
            join("PUSH 0xa0 PUSH 0x40 MSTORE", "PUSH 0xa0 PUSH 0x40 MSTORE"),
            Some(U256::from(0xa0))
        );
        assert_eq!(join("PUSH 0xa0 PUSH 0x40 MSTORE", ""), None);
        assert_eq!(join("", "PUSH 1 PUSH 0x5f MSTORE8"), None);
        // A word written on one path only is unknown after the join.
        let bytecode = analyze_asm(
            "
            PUSH0 CALLDATALOAD PUSH %b JUMPI
            PUSH 1 PUSH 0x60 MSTORE
        b:
            JUMPDEST
            PUSH 0x60 MLOAD PUSH0 MSTORE STOP
            ",
        );
        let (mload, _) = bytecode.iter_insts().find(|(_, data)| data.opcode == op::MLOAD).unwrap();
        assert_eq!(bytecode.const_output(mload), None);
    }

    /// A loop that only its own back edge reaches, behind a `JUMPI` whose condition the fixpoint
    /// knows, as in the `checked_exp_helper` that Solidity skips for constant exponents.
    #[test]
    fn unreached_loop_is_dead() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x01
            PUSH0
            MSTORE
            PUSH0
            MLOAD
            PUSH %done
            JUMPI
            PUSH %ret
        loop:
            JUMPDEST
            CALLDATASIZE
            PUSH %loop
            JUMPI
            JUMP
        ret:
            JUMPDEST
            STOP
        done:
            JUMPDEST
            STOP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
        let insts = bytecode.iter_all_insts();
        assert!(insts.filter(|(_, d)| d.opcode == op::CALLDATASIZE).all(|(_, d)| d.is_dead_code()));
    }

    /// Old Vyper saves the return address of a private function in memory, and a loop with a
    /// bounds check stores to an array next to it at an unknown index.
    #[test]
    fn bounded_store_keeps_memory_return_address() {
        let src = |array: u32, check: &str| {
            format!(
                "
                PUSH %ret
                PUSH2 0x0140
                MSTORE              ; return address
                PUSH0
                PUSH2 0x0200
                MSTORE              ; i = 0
            loop:
                JUMPDEST
                CALLDATASIZE
                PUSH2 {array:#x}
                PUSH2 0x0200
                MLOAD               ; [value, array, i]
                {check}
            body:
                JUMPDEST
                PUSH1 0x20
                MUL
                ADD
                MSTORE              ; array[i] = value
                PUSH2 0x0200
                MLOAD
                PUSH1 0x01
                ADD
                DUP1
                PUSH2 0x0200
                MSTORE
                PUSH1 0x03
                EQ
                ISZERO
                PUSH %loop
                JUMPI
                PUSH2 0x0140
                MLOAD
                JUMP                ; return
            ret:
                JUMPDEST
                STOP
            oob:
                JUMPDEST
                PUSH0
                DUP1
                REVERT
                "
            )
        };
        // `i < 3`, as each form of the bounds check shows on the edge to `body`.
        let checks = [
            "PUSH1 0x03 DUP2 LT PUSH %body JUMPI PUSH %oob JUMP",
            "DUP1 PUSH1 0x03 GT PUSH %body JUMPI PUSH %oob JUMP",
            "PUSH1 0x02 DUP2 GT ISZERO PUSH %body JUMPI PUSH %oob JUMP",
            "DUP1 PUSH1 0x02 LT ISZERO PUSH %body JUMPI PUSH %oob JUMP",
            "PUSH1 0x03 DUP2 LT ISZERO PUSH %oob JUMPI",
        ];
        for check in checks {
            // The array ends where the return address starts.
            let bytecode = analyze_asm(&src(0xe0, check));
            assert!(!bytecode.has_dynamic_jumps, "{check}");
            let bytecode = analyze_asm(&src(0x160, check));
            assert!(!bytecode.has_dynamic_jumps, "{check}");
        }
        // With `i < 4`, `array[3]` overwrites the return address.
        let bytecode =
            analyze_asm(&src(0xe0, "PUSH1 0x04 DUP2 LT PUSH %body JUMPI PUSH %oob JUMP"));
        assert!(bytecode.has_dynamic_jumps);
    }

    /// Old Vyper keeps return addresses in memory. Around a call, the caller pushes its memory
    /// frame onto the stack in a loop and pops it back in another, with the loop pointer in
    /// memory, so each iteration needs its own context to keep the frame's offsets constant.
    #[test]
    fn memory_frame_saved_on_stack() {
        let bytecode = analyze_asm(
            "
            PUSH1 0x11          ; the call sites differ in stack height
            PUSH %r1
            PUSH %f
            JUMP
        r1:
            JUMPDEST
            PUSH %r2
            PUSH %f
            JUMP
        r2:
            JUMPDEST
            STOP
        f:
            JUMPDEST
            PUSH2 0x0140
            MSTORE              ; return address
            CALLDATASIZE
            PUSH2 0x0160
            MSTORE
            PUSH2 0x0140
            PUSH2 0x0200
            MSTORE              ; p = 0x140
        push:
            JUMPDEST            ; push mload(p) while p < 0x180
            PUSH2 0x0200
            MLOAD
            MLOAD
            PUSH2 0x0200
            MLOAD
            PUSH1 0x20
            ADD
            DUP1
            PUSH2 0x0200
            MSTORE
            PUSH2 0x0180
            SWAP1
            LT
            PUSH %push
            JUMPI
            PUSH %fret
            PUSH %g
            JUMP
        g:
            JUMPDEST            ; overwrites the frame of `f`
            PUSH2 0x0140
            MSTORE
            CALLDATASIZE
            PUSH2 0x0160
            MSTORE
            PUSH2 0x0140
            MLOAD
            JUMP
        fret:
            JUMPDEST
            PUSH2 0x0160
            PUSH2 0x0200
            MSTORE              ; p = 0x160
        pop:
            JUMPDEST            ; pop into mload(p) while p >= 0x140
            PUSH2 0x0200
            MLOAD
            MSTORE
            PUSH2 0x0200
            MLOAD
            PUSH1 0x20
            SWAP1
            SUB
            DUP1
            PUSH2 0x0200
            MSTORE
            PUSH2 0x0140
            GT
            ISZERO
            PUSH %pop
            JUMPI
            PUSH2 0x0140
            MLOAD
            JUMP                ; return
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// A function that calls itself directly and through another function has a different call
    /// chain at every recursion level. Its return must resolve to its three return sites, and its
    /// caller must still find its own return address below the recursion.
    #[test]
    fn recursive_call_return() {
        let bytecode = analyze_asm(
            "
            PUSH %main_ret
            CALLDATASIZE
            PUSH %outer
            JUMP
        main_ret:
            JUMPDEST
            STOP
        outer:
            JUMPDEST            ; [main_ret, n]
            PUSH %outer_ret
            DUP2
            PUSH %rec
            JUMP
        outer_ret:
            JUMPDEST            ; [main_ret, n]
            POP
            JUMP
        rec:
            JUMPDEST            ; [ret, n]
            DUP1
            ISZERO
            PUSH %done
            JUMPI
            PUSH %r1
            PUSH1 1
            DUP3
            SUB
            PUSH %rec
            JUMP
        r1:
            JUMPDEST            ; [ret, n]
            PUSH %r2
            PUSH1 2
            DUP3
            SUB
            PUSH %g
            JUMP
        r2:
            JUMPDEST
        done:
            JUMPDEST            ; [ret, n]
            POP
            JUMP
        g:
            JUMPDEST            ; [ret, n]
            PUSH %g_ret
            DUP2
            PUSH %rec
            JUMP
        g_ret:
            JUMPDEST            ; [ret, n]
            POP
            JUMP
        ",
        );

        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
        let jump_at =
            |pc| bytecode.iter_insts().find(|&(i, d)| bytecode.pc(i) == pc && d.is_jump()).unwrap();
        let (_, data) = jump_at(17);
        assert!(!data.flags.contains(InstFlags::MULTI_JUMP));
        assert_eq!(bytecode.pc(data.static_jump_target()), 6, "outer must return to main_ret");
        let (rec_ret, _) = jump_at(46);
        let mut targets: Vec<_> =
            bytecode.multi_jump_targets(rec_ret).unwrap().iter().map(|&t| bytecode.pc(t)).collect();
        targets.sort_unstable();
        assert_eq!(targets, [15, 33, 54]);
        let (_, data) = jump_at(56);
        assert!(!data.flags.contains(InstFlags::MULTI_JUMP));
        assert_eq!(bytecode.pc(data.static_jump_target()), 43, "g must return to r2");
    }

    /// Callers that pass a jump target twice, without a recursion, would fold into segments, which
    /// joins their entries above the segments. That joins the flags that pick the branch of each
    /// caller, so each return would also reach `t`. Each return must resolve to its caller alone.
    #[test]
    fn calls_sharing_a_jump_target_keep_full_precision() {
        let bytecode = analyze_asm(
            "
            PUSH %t             ; w
            PUSH %ra
            PUSH %t
            PUSH1 0x00          ; flag
            PUSH %t
            PUSH %f
            JUMP
        ra:
            JUMPDEST            ; [w]
            PUSH %rb
            PUSH %t             ; z
            PUSH %t
            PUSH1 0x01          ; flag
            PUSH %t
            PUSH %f
            JUMP
        rb:
            JUMPDEST
            STOP
        t:
            JUMPDEST
            STOP
        f:
            JUMPDEST            ; [.., flag, t]
            POP
            PUSH %b
            JUMPI
            POP                 ; [w, ra, t]
            JUMP
        b:
            JUMPDEST            ; [w, rb, z, t]
            POP
            POP
            JUMP
        ",
        );

        assert!(!bytecode.has_dynamic_jumps, "expected no dynamic jumps");
        let mut jumps = bytecode.iter_insts().filter(|(_, d)| d.is_jump()).rev();
        for (ret, label) in [(27, "rb"), (13, "ra")] {
            let (_, data) = jumps.next().unwrap();
            assert!(!data.flags.contains(InstFlags::MULTI_JUMP));
            assert_eq!(bytecode.pc(data.static_jump_target()), ret, "must return to {label}");
        }
    }

    /// Returns the pcs of the targets of the only multi-target jump.
    fn multi_jump_target_pcs(bytecode: &Bytecode<'_>) -> Vec<u32> {
        let mut jumps =
            bytecode.iter_insts().filter(|(_, d)| d.flags.contains(InstFlags::MULTI_JUMP));
        let (jump, _) = jumps.next().expect("expected a multi-target jump");
        assert!(jumps.next().is_none());
        bytecode.multi_jump_targets(jump).unwrap().iter().map(|&t| bytecode.pc(t)).collect()
    }

    /// Solar lowers a dense `switch` to a jump into a table of equal-width entries, after a bounds
    /// check on the index.
    #[test]
    fn stride_jump_table() {
        let src = |n: u8, entry1: &str| {
            format!(
                "
                PUSH0
                CALLDATALOAD        ; idx
                PUSH1 {n:#x}
                DUP2
                LT
                PUSH %body
                JUMPI
                PUSH0
                DUP1
                REVERT
            body:
                JUMPDEST
                PUSH1 0x04
                MUL
                PUSH %table
                ADD
                JUMP                ; table + 4 * idx
            table:
                JUMPDEST            ; 0x14
                PUSH1 0x01
                STOP
                {entry1}            ; 0x18
                PUSH1 0x02
                STOP
                JUMPDEST            ; 0x1c
                PUSH1 0x03
                STOP
                "
            )
        };
        let bytecode = analyze_asm(&src(3, "JUMPDEST"));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x14, 0x18, 0x1c]);

        // Entries that are not jump targets fail at runtime, in the switch's default case.
        let bytecode = analyze_asm(&src(3, "STOP"));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x14, 0x1c]);

        // A static jump to the only target would skip that failure.
        let bytecode = analyze_asm(&src(2, "STOP"));
        assert!(bytecode.has_dynamic_jumps);

        // A table past the end of the code has no targets.
        let bytecode = analyze_asm("PUSH0 CALLDATALOAD PUSH1 0x03 AND PUSH2 0x1000 ADD JUMP");
        assert!(!bytecode.has_dynamic_jumps);
    }

    /// Solady picks a jump target out of a word of packed targets with `BYTE`, at an index that
    /// `AND` masks.
    #[test]
    fn byte_jump_table() {
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH1 0x03
            AND
            PUSH1 0x1c
            ADD                 ; 28 + (x & 3)
            PUSH4 0x14101210
            SWAP1
            BYTE
            JUMP                ; one of the last 4 bytes of the word
            JUMPDEST            ; 0x10
            STOP
            JUMPDEST            ; 0x12
            STOP
            JUMPDEST            ; 0x14
            STOP
            JUMPDEST            ; 0x16
            STOP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x10, 0x12, 0x14]);
    }

    /// Solar's selector dispatch picks a target with `BYTE` at `16 + selector % 16`.
    #[test]
    fn mod_byte_jump_table() {
        let bytecode = analyze_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH1 0xe0
            SHR                 ; selector
            PUSH1 0x10
            DUP2
            MOD
            PUSH1 0x10
            ADD
            PUSH16 0x20222426202224262022242620222426
            SWAP1
            BYTE
            JUMP
            JUMPDEST            ; 0x20
            STOP
            JUMPDEST            ; 0x22
            STOP
            JUMPDEST            ; 0x24
            STOP
            JUMPDEST            ; 0x26
            STOP
        ",
        );
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x20, 0x22, 0x24, 0x26]);
    }

    /// Vyper's dense selector table copies the entry for `selector % n` from the code into the
    /// last two bytes of the first word of memory, which is all zeros otherwise, and jumps to that
    /// word.
    #[test]
    fn codecopy_jump_table() {
        let src = |prefix: &str| {
            format!(
                "
                {prefix}
                PUSH0
                CALLDATALOAD
                PUSH1 0xe0
                SHR                 ; selector
                PUSH1 0x03
                SWAP1
                MOD
                PUSH1 0x01
                SHL                 ; 2 * (selector % 3)
                PUSH %table
                ADD
                PUSH1 0x01
                ADD                 ; skip the PUSH6 opcode
                PUSH1 0x02
                SWAP1
                PUSH1 0x1e
                CODECOPY            ; memory[0x1e..0x20] = entry
                PUSH0
                MLOAD
                JUMP
                JUMPDEST            ; 0x1b
                STOP
                JUMPDEST            ; 0x1d
                STOP
                JUMPDEST            ; 0x1f
                STOP
            table:
                PUSH6 0x001f001d001b
                "
            )
        };
        let bytecode = analyze_asm(&src(""));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x1b, 0x1d, 0x1f]);

        // The word is only known over zeros: after a write to memory the rest of it is unknown.
        let bytecode = analyze_asm(&src("PUSH0 CALLDATALOAD PUSH1 0x40 MSTORE"));
        assert!(bytecode.has_dynamic_jumps);
    }

    /// Range jumps resolve to at most [`MAX_RANGE_JUMP_TARGETS`] targets in total, since each
    /// becomes its own switch.
    #[test]
    fn range_jump_budget() {
        let jumpdests = "JUMPDEST ".repeat(600);
        let jump = "PUSH0 CALLDATALOAD PUSH2 0x03ff AND JUMP";
        let bytecode = analyze_asm(&format!("{jumpdests} {jump}"));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode).len(), 600);

        let bytecode = analyze_asm(&format!(
            "{jumpdests} PUSH0 CALLDATALOAD PUSH0 CALLDATALOAD PUSH2 0x03ff AND JUMPI {jump}"
        ));
        assert!(bytecode.has_dynamic_jumps);
    }

    /// Solar's `-Ogas` dispatcher shifts a packed table of `u16` targets right by `16 * (selector %
    /// n)`. The shift of a constant by a range is not a range, so only enumerating the values of
    /// the index finds the targets.
    #[test]
    fn packed_jump_table() {
        let src = |entry1: &str, table: &str| {
            format!(
                "
                PUSH0
                CALLDATALOAD
                PUSH1 0xe0
                SHR                 ; selector
                PUSH1 0x03
                DUP2
                MOD
                PUSH1 0x04
                SHL                 ; 16 * (selector % 3)
                PUSH6 {table}
                SWAP1
                SHR
                PUSH2 0xffff
                AND
                JUMP
                JUMPDEST            ; 0x1a
                STOP
                {entry1}            ; 0x1c
                STOP
                JUMPDEST            ; 0x1e
                STOP
                "
            )
        };
        let bytecode = analyze_asm(&src("JUMPDEST", "0x001e001c001a"));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x1a, 0x1c, 0x1e]);

        // An entry that is not a jump target fails at runtime, in the switch's default case.
        let bytecode = analyze_asm(&src("STOP", "0x001e001c001a"));
        assert!(!bytecode.has_dynamic_jumps);
        assert_eq!(multi_jump_target_pcs(&bytecode), [0x1a, 0x1e]);

        // A static jump to the only target would skip that failure.
        let bytecode = analyze_asm(&src("STOP", "0x001c001c001a"));
        assert!(bytecode.has_dynamic_jumps);

        // The switch compares only the low 64 bits of the operand, so an invalid target above
        // them could take a valid one.
        let wide = |entry2: &str| {
            format!(
                "
                PUSH0
                CALLDATALOAD
                PUSH1 0xe0
                SHR
                PUSH1 0x03
                DUP2
                MOD
                PUSH1 0x48
                MUL                 ; 72 * (selector % 3)
                PUSH27 0x{entry2}000000000000000038000000000000000036
                SWAP1
                SHR
                PUSH9 0xffffffffffffffffff
                AND
                JUMP
                JUMPDEST            ; 0x36
                STOP
                JUMPDEST            ; 0x38
                STOP
                JUMPDEST            ; 0x3a
                STOP
                "
            )
        };
        let bytecode = analyze_asm(&wide("00000000000000003a"));
        assert!(!bytecode.has_dynamic_jumps);
        let bytecode = analyze_asm(&wide("01000000000000003a"));
        assert!(bytecode.has_dynamic_jumps);

        // A narrow range that is not the index of the jump operand.
        let bytecode = analyze_asm(
            "
            JUMPDEST
            JUMPDEST
            JUMPDEST
            PUSH0
            CALLDATALOAD
            PUSH1 0x03
            AND
            PUSH0
            CALLDATALOAD
            JUMP
            ",
        );
        assert!(bytecode.has_dynamic_jumps);

        // A narrow range below the jump operand, which the jump leaves on top of the stack.
        let bytecode = analyze_asm(
            "
            JUMPDEST
            JUMPDEST
            JUMPDEST
            PUSH0
            CALLDATALOAD
            PUSH1 0x03
            AND
            PUSH0
            CALLDATALOAD
            PUSH %end
            JUMPI
            PUSH0
            CALLDATALOAD
            JUMP
        end:
            JUMPDEST
            STOP
            ",
        );
        assert!(bytecode.has_dynamic_jumps);
    }

    /// A view that overrides every opcode leaves the stack opcodes unsupported, without stack
    /// inputs. Analysis must reject them rather than read the stack.
    #[test]
    fn overridden_stack_opcodes_are_rejected() {
        let view = OpcodeConfig::<BaseEvmTypes>::base_for_spec(crate::tests::DEF_SPEC).view(None);
        for opcode in [op::DUP1, op::DUP16, op::SWAP1, op::SWAP16] {
            let code = vec![opcode, op::STOP];
            let mut bytecode = Bytecode::new(code, crate::tests::DEF_SPEC, None, Some(&view));
            assert!(bytecode.analyze().is_err(), "opcode {opcode:#04x}");
        }
    }
}
