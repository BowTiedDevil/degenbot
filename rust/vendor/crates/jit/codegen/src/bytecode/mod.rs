//! Internal EVM bytecode and opcode representation.

use crate::FxHashMap;
use alloy_primitives::U256;
use evm2::{
    OpcodeView, SpecId,
    bytecode::{JumpTable, analyze_legacy},
    interpreter::{op, opcode::OpCode},
    version::GasParams,
};
use evm2_jit_backend::{Result, eyre::bail};
use oxc_index::IndexVec;
use smallvec::SmallVec;
use std::{borrow::Cow, cell::RefCell, fmt::Write};

mod passes;
pub(crate) use passes::{
    Block, Cfg, GasSection, MemorySection, MemorySectionAnalysis, SectionsAnalysis, Snapshots,
    StackSection,
};

mod asm;
pub use asm::parse_asm;

mod fmt;

mod interner;
pub(crate) use interner::Interner;

mod info;
pub use info::*;

mod opcode;
pub use opcode::*;

/// Implements `Display` for a nonmax index type using a format string.
macro_rules! impl_index_display {
    ($ty:ty, $fmt:literal) => {
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, $fmt, self.index())
            }
        }
    };
}
pub(crate) use impl_index_display;

oxc_index::define_nonmax_u32_index_type! {
    /// An EVM instruction index into [`Bytecode`] instructions.
    ///
    /// Also known as `ic`, or instruction counter; not to be confused with SSA `inst`s.
    pub(crate) struct Inst;
}
impl_index_display!(Inst, "ic{}");

oxc_index::define_nonmax_u32_index_type! {
    /// Index into the deduplicated U256 constant pool.
    pub(crate) struct U256Idx;
}
impl_index_display!(U256Idx, "{}");

/// Type alias for the bytecode-level U256 constant interner.
pub(crate) type U256Interner = Interner<U256Idx, U256, alloy_primitives::map::FbBuildHasher<32>>;

/// Compact handle to a `U256` constant.
///
/// Either a `u32` inline value or a reference into a [`U256Interner`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct U256Imm(u32);

impl U256Imm {
    /// High bit set means the handle is an inline `u8` value.
    const INLINE_TAG: u32 = 1 << 31;

    /// Decodes a handle previously produced by [`Self::to_raw`].
    #[inline]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Encodes this handle as a `u32` for storage in [`InstData::data`].
    #[inline]
    pub(crate) const fn to_raw(self) -> u32 {
        self.0
    }

    /// Constructs a handle for `value`, inlining `u8`-sized values and otherwise interning.
    #[inline]
    pub(crate) fn new(value: U256, interner: &mut U256Interner) -> Self {
        if let Ok(x) = u32::try_from(value)
            && x & Self::INLINE_TAG == 0
        {
            Self(Self::INLINE_TAG | x)
        } else {
            let idx = interner.intern(value).index() as u32;
            debug_assert!(
                idx & Self::INLINE_TAG == 0,
                "U256 interner exceeded 2^31 entries; bump `U256Imm` encoding",
            );
            Self(idx)
        }
    }

    /// Returns the underlying `U256` value.
    #[inline]
    pub(crate) fn get(self, interner: &U256Interner) -> U256 {
        if self.0 & Self::INLINE_TAG != 0 {
            U256::from(self.0 & !Self::INLINE_TAG)
        } else {
            *interner.get(U256Idx::new(self.0 as usize))
        }
    }

    /// Returns the inline `u32` value, if any.
    #[inline]
    pub(crate) fn get_inline(self) -> Option<u32> {
        (self.0 & Self::INLINE_TAG != 0).then_some(self.0 & !Self::INLINE_TAG)
    }
}

impl std::fmt::Debug for U256Imm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.get_inline() {
            Some(x) => write!(f, "U256Imm::Inline({x})"),
            None => write!(f, "U256Imm::Idx({})", self.0),
        }
    }
}

bitflags::bitflags! {
    /// Controls which analysis passes run during [`Bytecode::analyze`].
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct AnalysisConfig: u8 {
        /// Run block deduplication.
        const DEDUP = 1 << 0;
        /// The stack is observable outside the function (`inspect_stack` mode).
        /// When set, DSE must not assume diverging terminators kill the stack.
        const INSPECT_STACK = 1 << 1;
        /// Run dead store elimination.
        const DSE = 1 << 2;

        /// All passes enabled.
        const ALL = Self::DEDUP.bits() | Self::DSE.bits();
    }
}

impl Default for AnalysisConfig {
    #[inline]
    fn default() -> Self {
        Self::ALL
    }
}

/// Default compiler gas limit for compile-time evaluation (100k gas).
pub(crate) const DEFAULT_COMPILER_GAS_LIMIT: u64 = 100_000;

/// EVM bytecode.
#[doc(hidden)] // Not public API.
pub struct Bytecode<'a> {
    /// The original bytecode.
    pub(crate) code: Cow<'a, [u8]>,
    /// The instructions.
    insts: IndexVec<Inst, InstData>,
    /// `JUMPDEST` opcode map: `jumpdests.is_valid(pc)` is `true` if `code[pc] == op::JUMPDEST`
    /// and `pc` is not inside `PUSH` data.
    jumpdests: JumpTable,
    /// The [`SpecId`].
    pub(crate) spec_id: SpecId,
    /// Gas parameters for dynamic gas folding. Defaults to the evm2 spec gas schedule.
    pub(crate) gas_params: GasParams,
    /// Whether the bytecode contains dynamic jumps.
    has_dynamic_jumps: bool,
    /// Mapping from program counter to instruction.
    pc_to_inst: FxHashMap<u32, Inst>,
    /// Mapping from instruction index to its program counter (`code[pc]` is the opcode byte).
    ///
    /// `JUMPDEST` and `PC` mirror this in `data` for hot codegen access; everything else
    /// reads it through [`Bytecode::pc`].
    inst_to_pc: IndexVec<Inst, u32>,

    /// Deduplicated constant pool for U256 values.
    u256_interner: RefCell<U256Interner>,

    /// Per-instruction operand snapshots computed by block analysis.
    snapshots: Snapshots,
    /// Per-instruction memory sections computed by section analysis.
    memory_sections: IndexVec<Inst, MemorySection>,
    /// Multi-target jump table: maps a JUMP/JUMPI instruction to its set of known targets.
    /// Only populated for jumps resolved to multiple targets by block analysis.
    multi_jump_targets: FxHashMap<Inst, SmallVec<[Inst; 4]>>,

    /// Instruction index to 1-based line number in the formatted dump, built during formatting.
    inst_lines: RefCell<IndexVec<Inst, u32>>,

    /// Dead-block redirects: maps the first instruction of a dead/merged block to the target
    /// instruction. Used mainly for fallthrough jumps.
    pub(crate) redirects: FxHashMap<Inst, Inst>,
    /// Basic-block CFG, rebuilt by [`Bytecode::rebuild_cfg`].
    cfg: Cfg,
    /// Controls which analysis passes are enabled.
    pub(crate) config: AnalysisConfig,
    /// Gas budget for compile-time evaluation of user-supplied bytecode.
    ///
    /// The compiler evaluates EVM operations at compile time during analysis passes. Without a
    /// budget, adversarial bytecode (e.g. thousands of `EXP(U256::MAX, U256::MAX)`) can make
    /// compilation arbitrarily slow. This limit uses the EVM gas schedule to bound work.
    ///
    /// When exhausted, further compile-time evaluation is skipped (values remain dynamic).
    /// Defaults to 100k gas.
    pub(crate) compiler_gas_limit: u64,
    /// Cumulative compiler gas consumed so far.
    pub(crate) compiler_gas_used: u64,
}

impl<'a> Bytecode<'a> {
    pub(crate) fn new(
        code: impl Into<Cow<'a, [u8]>>,
        spec_id: SpecId,
        gas_params: Option<GasParams>,
        opcodes: Option<&OpcodeView>,
    ) -> Self {
        Self::new_mono(code.into(), spec_id, gas_params, opcodes)
    }

    #[cfg(test)]
    pub(crate) fn test(code: impl Into<Cow<'a, [u8]>>) -> Self {
        Self::new(code, crate::tests::DEF_SPEC, None, None)
    }

    #[instrument(name = "Bytecode::new", level = "debug", skip_all)]
    fn new_mono(
        code: Cow<'a, [u8]>,
        spec_id: SpecId,
        gas_params: Option<GasParams>,
        opcodes: Option<&OpcodeView>,
    ) -> Self {
        let gas_params = gas_params.unwrap_or(evm2::Version::base(spec_id).gas_params);
        let mut insts = IndexVec::with_capacity(code.len() + 8);
        let mut inst_to_pc = IndexVec::with_capacity(code.len() + 8);
        // EIP-8024: JUMPDEST analysis is unchanged by DUPN/SWAPN/EXCHANGE: only PUSH
        // data is skipped, their immediates are not. Compute the valid-jump-target
        // bitmap with the legacy analysis over the raw bytes, independent of instruction
        // decoding below (which does consume DUPN/SWAPN/EXCHANGE immediates and can
        // therefore desynchronize from the analysis).
        let jumpdests = analyze_legacy(&code);
        let mut pc_to_inst = FxHashMap::with_capacity_and_hasher(code.len(), Default::default());
        let mut u256_interner = U256Interner::new();
        let owned_infos = opcodes.map(|opcodes| opcode_infos(opcodes, spec_id));
        let op_infos = owned_infos.as_ref().unwrap_or_else(|| op_info_map(spec_id));
        let mut iter = OpcodesIter::with_infos(&code, op_infos).with_pc();
        while let Some((pc, Opcode { opcode, immediate })) = iter.next() {
            let inst: Inst = insts.next_idx();
            pc_to_inst.insert(pc as u32, inst);
            inst_to_pc.push(pc as u32);

            let mut flags = InstFlags::empty();
            let info = op_infos[opcode as usize];
            if info.is_unknown() {
                flags |= InstFlags::UNKNOWN;
            }
            if info.is_disabled() {
                flags |= InstFlags::DISABLED;
            }
            if info.is_host() {
                flags |= InstFlags::HOST;
            }
            if info.is_unsupported_host() {
                // Analysis treats it as an unknown opcode; `analyze` rejects the program if one
                // is reachable.
                flags |= InstFlags::UNKNOWN | InstFlags::UNSUPPORTED_HOST;
            }
            let (base_gas, stack_io) =
                if info.is_unknown() || info.is_disabled() || info.is_unsupported_host() {
                    (0, (0, 0))
                } else if let Some(stack_io) = compute_stack_io(opcode, immediate) {
                    (info.base_gas(), stack_io)
                } else {
                    // Pays its static gas, then halts before touching the stack.
                    flags |= InstFlags::INVALID_IMMEDIATE;
                    (info.base_gas(), (0, 0))
                };

            let data = match opcode {
                op::PUSH0..=op::PUSH32 | op::DUPN | op::SWAPN | op::EXCHANGE => {
                    let imm_len = min_imm_len(opcode) as usize;
                    // `OpcodesIter` returns `None` for truncated EOF immediates; recover
                    // the available bytes directly from `code` and right-pad with zeros
                    // per EVM spec.
                    let start = pc + 1;
                    let avail = code.get(start..).map(|s| &s[..s.len().min(imm_len)]);
                    let value = match avail {
                        Some(slice) if slice.len() == imm_len => U256::from_be_slice(slice),
                        Some(slice) if !slice.is_empty() => {
                            let mut padded = [0u8; 32];
                            padded[..slice.len()].copy_from_slice(slice);
                            U256::from_be_slice(&padded[..imm_len])
                        }
                        _ => U256::ZERO,
                    };
                    U256Imm::new(value, &mut u256_interner).to_raw()
                }
                op::JUMPDEST | op::PC => {
                    let pc = pc as u32;
                    debug_assert!(pc & InstData::JUMPDEST_REACHABLE == 0, "pc too large");
                    pc
                }
                _ => 0,
            };

            let gas_section = GasSection::default();
            let stack_section = StackSection::default();

            insts.push(InstData {
                opcode,
                flags,
                base_gas,
                stack_io,
                data,
                gas_section,
                stack_section,
            });

            // A valid jump target (per the legacy analysis above) can sit inside a
            // consumed immediate: directly as a DUPN/SWAPN/EXCHANGE immediate byte, or
            // inside a PUSH immediate once the decoded stream has diverged from the
            // analysis past such an opcode. Rewind the iterator to the first such
            // target so it is re-yielded and jumps have an instruction to land on.
            // Analysis-aligned PUSH data never contains valid targets, so pre-EIP-8024
            // bytecode never rewinds. Execution cannot fall through into the rewound
            // bytes: every desynchronizing immediate is an invalid DUPN/SWAPN/EXCHANGE
            // encoding, so the opcode preceding them halts at runtime.
            // A truncated immediate at the end of the code consumes the remaining bytes.
            let imm_len = if info.is_unknown() || info.is_disabled() || info.is_unsupported_host() {
                0
            } else {
                min_imm_len(opcode) as usize
            };
            let imm_end = (pc + 1 + imm_len).min(code.len());
            if let Some(target) = (pc + 1..imm_end).find(|&p| jumpdests.is_valid(p)) {
                // SAFETY: we just consumed the immediate bytes at `pc + 1..imm_end`.
                unsafe { iter.rewind(imm_end - target) };
            }
        }

        // Pad code to ensure there is at least one diverging instruction.
        if insts.last().is_none_or(|last| last.can_fall_through()) {
            trace!("adding STOP padding");
            insts.push(InstData::new(op::STOP));
            inst_to_pc.push(code.len() as u32);
        }

        Self {
            code,
            insts,
            jumpdests,
            spec_id,
            gas_params,
            has_dynamic_jumps: false,
            snapshots: Snapshots::default(),
            memory_sections: IndexVec::new(),
            u256_interner: RefCell::new(u256_interner),
            multi_jump_targets: FxHashMap::default(),
            pc_to_inst,
            inst_to_pc,
            inst_lines: RefCell::new(IndexVec::new()),
            redirects: FxHashMap::default(),
            cfg: Cfg::default(),
            config: AnalysisConfig::default(),
            compiler_gas_limit: DEFAULT_COMPILER_GAS_LIMIT,
            compiler_gas_used: 0,
        }
    }

    /// Takes the instruction-to-line map built during formatting.
    ///
    /// If the bytecode has not been formatted yet, formats it first to build the map.
    pub(crate) fn take_inst_lines(&self) -> IndexVec<Inst, u32> {
        if self.inst_lines.borrow().is_empty() {
            self.collect_lines();
        }
        self.inst_lines.take()
    }

    /// Returns an iterator over the opcodes.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn opcodes(&self) -> OpcodesIter<'_> {
        OpcodesIter::new(&self.code, self.spec_id)
    }

    /// Returns the instruction at the given instruction counter.
    #[inline]
    #[track_caller]
    pub(crate) fn inst(&self, inst: Inst) -> &InstData {
        &self.insts[inst]
    }

    /// Returns a mutable reference the instruction at the given instruction counter.
    #[inline]
    #[track_caller]
    #[allow(dead_code)]
    pub(crate) fn inst_mut(&mut self, inst: Inst) -> &mut InstData {
        &mut self.insts[inst]
    }

    /// Returns the opcode + immediate slice for the given instruction.
    #[inline]
    pub(crate) fn opcode(&self, inst: Inst) -> Opcode<'_> {
        Opcode { opcode: self.inst(inst).opcode, immediate: self.get_imm(inst) }
    }

    /// Returns an iterator over the instructions.
    #[inline]
    pub(crate) fn iter_insts(&self) -> impl DoubleEndedIterator<Item = (Inst, &InstData)> + Clone {
        self.iter_all_insts().filter(|(_, data)| !data.is_dead_code())
    }

    /// Returns an iterator over the instructions.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn iter_mut_insts(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (Inst, &mut InstData)> {
        self.iter_mut_all_insts().filter(|(_, data)| !data.is_dead_code())
    }

    /// Returns an iterator over all the instructions, including dead code.
    #[inline]
    pub(crate) fn iter_all_insts(
        &self,
    ) -> impl DoubleEndedIterator<Item = (Inst, &InstData)> + ExactSizeIterator + Clone {
        self.insts.iter_enumerated()
    }

    /// Returns an iterator over all the instructions, including dead code.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn iter_mut_all_insts(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (Inst, &mut InstData)> + ExactSizeIterator {
        self.insts.iter_mut_enumerated()
    }

    /// Runs a list of analysis passes on the instructions.
    #[instrument(level = "debug", skip_all)]
    pub(crate) fn analyze(&mut self) -> Result<()> {
        self.refresh_cfg();

        self.block_analysis_local();
        self.refresh_cfg();

        let local_snapshots = self.snapshots.clone();

        self.block_analysis(&local_snapshots);
        self.refresh_cfg();

        if self.config.contains(AnalysisConfig::DEDUP) {
            self.dedup_blocks(&local_snapshots);
            self.refresh_cfg();
        }
        drop(local_snapshots);

        if self.config.contains(AnalysisConfig::DSE) {
            self.dead_store_elim();
        }

        self.construct_sections()?;
        self.construct_memory_sections();

        debug!(
            compiler_gas_used = self.compiler_gas_used,
            compiler_gas_limit = self.compiler_gas_limit,
            "constant folding gas budget",
        );

        if tracing::enabled!(tracing::Level::TRACE) {
            self.log_ir_stats();
            self.log_const_input_stats();
        }

        if let Some((_, data)) =
            self.iter_insts().find(|(_, data)| data.flags.contains(InstFlags::UNSUPPORTED_HOST))
        {
            bail!("host opcode {:#04x} is not supported by compiled code", data.opcode);
        }

        Ok(())
    }

    /// Recomputes the `has_dynamic_jumps` flag based on the current instruction set.
    #[instrument(name = "dyn_jumps", level = "debug", skip_all)]
    #[inline(never)]
    pub(crate) fn recompute_has_dynamic_jumps(&mut self) {
        let mut unresolved = self.insts.iter().filter(|inst| {
            inst.is_jump() && !inst.flags.contains(InstFlags::STATIC_JUMP) && !inst.is_dead_code()
        });
        self.has_dynamic_jumps = unresolved.next().is_some();
        if self.has_dynamic_jumps {
            debug!(n = 1 + unresolved.count(), "unresolved dynamic jumps remain");
        }
    }

    #[instrument(name = "jumpdests", level = "debug", skip_all)]
    #[inline(never)]
    pub(crate) fn recompute_reachable_jumpdests(&mut self) {
        for data in &mut self.insts.raw {
            if data.is_jumpdest() {
                data.clear_jumpdest_reachable();
            }
        }

        let mut targets = Vec::new();
        for (inst, data) in self.insts.iter_enumerated() {
            if !data.is_static_jump()
                || data.flags.contains(InstFlags::INVALID_JUMP)
                || data.is_dead_code()
            {
                continue;
            }
            if data.flags.contains(InstFlags::MULTI_JUMP) {
                if let Some(multi_targets) = self.multi_jump_targets.get(&inst) {
                    targets.extend(multi_targets.iter().copied());
                }
            } else {
                targets.push(data.static_jump_target());
            }
        }

        for mut target in targets {
            while let Some(&next) = self.redirects.get(&target) {
                target = next;
            }
            if self.insts[target].is_jumpdest() {
                self.insts[target].set_jumpdest_reachable();
            }
        }
    }

    /// Mark unreachable instructions as `DEAD_CODE` to not generate any code for them.
    ///
    /// This pass is technically unnecessary as the backend will very likely optimize any
    /// unreachable code that we generate, but this is trivial for us to do and significantly speeds
    /// up code generation.
    ///
    /// We can simply mark all instructions that are between diverging instructions and
    /// `JUMPDEST`s.
    #[instrument(name = "dce", level = "debug", skip_all)]
    #[inline(never)]
    fn mark_dead_code(&mut self) {
        loop {
            if !self.mark_dead_code_once() {
                break;
            }
            self.recompute_has_dynamic_jumps();
            self.recompute_reachable_jumpdests();
        }
    }

    fn mark_dead_code_once(&mut self) -> bool {
        let mut iter = self.insts.iter_mut_enumerated();
        let mut marked_jump_dead = false;
        while let Some((i, data)) = iter.next() {
            if !data.can_fall_through() {
                let static_target = data
                    .is_static_jump()
                    .then(|| data.static_jump_target())
                    .filter(|&target| target > i);
                if static_target == Some(i + 1) {
                    continue;
                }
                let mut end = i;
                let mut any_new = false;
                for (j, data) in &mut iter {
                    end = j;
                    if static_target == Some(j) {
                        break;
                    }
                    if data.is_reachable_jumpdest(self.has_dynamic_jumps) {
                        break;
                    }
                    if !data.is_dead_code() {
                        any_new = true;
                        marked_jump_dead |= data.is_jump();
                    }
                    data.flags |= InstFlags::DEAD_CODE;
                }
                let start = i + 1;
                if any_new && end > start {
                    debug!("found dead code: {start}..{end}");
                }
            }
        }
        marked_jump_dead
    }

    /// Constructs the sections in the bytecode.
    #[instrument(name = "sections", level = "debug", skip_all)]
    #[inline(never)]
    fn construct_sections(&mut self) -> Result<()> {
        let mut analysis = SectionsAnalysis::default();
        for inst in self.insts.indices() {
            if !self.inst(inst).is_dead_code() {
                analysis.process(self, inst);
            }
        }
        analysis.finish(self);

        if let Some((inst, _)) = self
            .insts
            .iter_enumerated()
            .find(|(_, data)| data.gas_section.gas_cost == GasSection::SATURATED)
        {
            bail!(
                "the gas section at instruction {} costs at least {} gas, which compiled code \
                 cannot charge ahead of its instructions",
                inst.index(),
                GasSection::SATURATED,
            );
        }
        Ok(())
    }

    /// Constructs the memory sections in the bytecode.
    #[instrument(name = "memory_sections", level = "debug", skip_all)]
    #[inline(never)]
    fn construct_memory_sections(&mut self) {
        self.memory_sections = MemorySectionAnalysis::new(self).run(self);
    }

    /// Returns the immediate value of the given instruction, if any.
    ///
    /// For truncated immediates at EOF, returns the available bytes (which may be shorter than
    /// `imm_len`). Returns `None` only when `imm_len` is 0 or `start` is completely out of bounds.
    pub(crate) fn get_imm(&self, inst: Inst) -> Option<&[u8]> {
        let data = self.inst(inst);
        let imm_len = data.imm_len() as usize;
        if imm_len == 0 {
            return None;
        }
        let start = self.pc(inst) as usize + 1;
        let end = (start + imm_len).min(self.code.len());
        if start >= end {
            return None;
        }
        Some(&self.code[start..end])
    }

    /// Returns the program counter (offset into the original bytecode) of the given instruction.
    #[inline]
    pub(crate) fn pc(&self, inst: Inst) -> u32 {
        self.inst_to_pc[inst]
    }

    /// Returns the value of a PUSH instruction, right-padding truncated EOF immediates with zeros
    /// per EVM spec.
    pub(crate) fn get_push_value(&self, data: &InstData) -> U256 {
        debug_assert!(matches!(data.opcode, op::PUSH0..=op::PUSH32));
        data.imm().get(&self.u256_interner.borrow())
    }

    /// Returns the length of the original bytecode in bytes.
    #[inline]
    pub(crate) fn codesize(&self) -> usize {
        self.code.len()
    }

    /// Returns `true` if the given program counter is a valid jump destination.
    pub(crate) const fn is_valid_jump(&self, pc: usize) -> bool {
        self.jumpdests.is_valid(pc)
    }

    /// Returns `true` if the bytecode has dynamic jumps.
    pub(crate) const fn has_dynamic_jumps(&self) -> bool {
        self.has_dynamic_jumps
    }

    /// Returns `true` if the stack argument's contents are observed by the caller.
    pub(crate) const fn stack_observed(&self) -> bool {
        self.config.contains(AnalysisConfig::INSPECT_STACK)
    }

    /// Returns `true` if the instruction is diverging.
    pub(crate) fn is_instr_diverging(&self, inst: Inst) -> bool {
        self.insts[inst].is_diverging()
    }

    /// Returns `true` if `inst` can only be entered by falling through from the previous
    /// instruction.
    pub(crate) fn is_fall_through_only(&self, inst: Inst) -> bool {
        let data = self.inst(inst);
        if data.is_reachable_jumpdest(self.has_dynamic_jumps())
            || data.flags.contains(InstFlags::REDIRECT_TARGET)
        {
            return false;
        }
        if inst.index() == 0 {
            return true;
        }
        let prev = self.inst(inst - 1);
        !prev.is_dead_code() && prev.can_fall_through()
    }

    /// Converts a program counter (`self.code[pc]`) to an instruction (`self.inst(inst)`).
    #[inline]
    pub(crate) fn pc_to_inst(&self, pc: usize) -> Inst {
        match self.pc_to_inst.get(&(pc as u32)) {
            Some(&inst) => inst,
            None => panic!("pc out of bounds: {pc}"),
        }
    }

    /// Returns the multi-target jump table entry for the given instruction, if any.
    pub(crate) fn multi_jump_targets(&self, inst: Inst) -> Option<&[Inst]> {
        self.multi_jump_targets.get(&inst).map(|v| v.as_slice())
    }

    /// Resolves an instruction through dead-block redirects created by dedup.
    pub(crate) fn effective_inst(&self, inst: Inst) -> Inst {
        self.redirects.get(&inst).copied().unwrap_or(inst)
    }

    /// Resolves a jump target instruction to its CFG block index.
    pub(crate) fn target_block(&self, target: Inst) -> Option<Block> {
        self.cfg.inst_to_block.get(target).copied().flatten()
    }

    /// Returns the known constant value of a stack operand at the given instruction.
    ///
    /// `depth` 0 is TOS (first popped by this instruction), 1 is second, etc.
    /// Returns `None` if the value is unknown or the analysis didn't cover this instruction.
    #[allow(dead_code)]
    pub(crate) fn const_operand(&self, inst: Inst, depth: usize) -> Option<U256> {
        let imm = self.snapshots.input(inst, depth)?.as_const()?;
        Some(imm.get(&self.u256_interner.borrow()))
    }

    /// Returns the known constant output value of the given instruction.
    ///
    /// Returns `None` if the output is unknown, the instruction has no output, or the
    /// analysis didn't cover this instruction.
    pub(crate) fn const_output(&self, inst: Inst) -> Option<U256> {
        let imm = self.snapshots.outputs[inst]?.as_const()?;
        Some(imm.get(&self.u256_interner.borrow()))
    }

    /// Returns the known output of the given instruction if translation can store it instead of
    /// executing the instruction.
    ///
    /// A constant output joined across call contexts does not imply that EXP's exponent is
    /// constant, so EXP's dynamic gas must also be static.
    pub(crate) fn foldable_output(&self, inst: Inst) -> Option<U256> {
        if self.insts[inst].opcode == op::EXP && self.exp_static_exponent(inst).is_none() {
            return None;
        }
        self.const_output(inst)
    }

    /// Returns the exponent of the EXP instruction if its dynamic gas is charged statically with
    /// the section.
    ///
    /// This holds exactly when the exponent is a known constant. Translation then never charges
    /// the dynamic gas itself, whether it folds, inlines or calls the gas-free builtin; otherwise
    /// it charges the gas at run time.
    pub(crate) fn exp_static_exponent(&self, inst: Inst) -> Option<U256> {
        debug_assert_eq!(self.insts[inst].opcode, op::EXP);
        self.const_operand(inst, 1)
    }

    /// Returns the first constant EVM memory access `(offset, len)` for the given instruction.
    #[allow(dead_code)]
    pub(crate) fn const_memory_access(&self, inst: Inst) -> Option<(Option<u64>, Option<u64>)> {
        self.const_memory_accesses(inst).into_iter().flatten().next()
    }

    /// Returns the constant EVM memory accesses `(offset, len)` for the given instruction.
    pub(crate) fn const_memory_accesses(
        &self,
        inst: Inst,
    ) -> [Option<(Option<u64>, Option<u64>)>; 2] {
        let data = self.inst(inst);
        if data.flags.intersects(InstFlags::DISABLED | InstFlags::UNKNOWN | InstFlags::HOST) {
            return [None, None];
        }

        let operand =
            |depth| self.const_operand(inst, depth).and_then(|value| u64::try_from(value).ok());
        match data.opcode {
            op::KECCAK256 | op::RETURN | op::REVERT | op::LOG0..=op::LOG4 => {
                [Some((operand(0), operand(1))), None]
            }
            op::MLOAD | op::MSTORE => [Some((operand(0), Some(32))), None],
            op::MSTORE8 => [Some((operand(0), Some(1))), None],
            op::CALLDATACOPY | op::CODECOPY | op::RETURNDATACOPY => {
                [Some((operand(0), operand(2))), None]
            }
            op::EXTCODECOPY => [Some((operand(1), operand(3))), None],
            op::MCOPY => [
                Some((operand(0).zip(operand(1)).map(|(dst, src)| dst.max(src)), operand(2))),
                None,
            ],
            op::CREATE | op::CREATE2 => [Some((operand(1), operand(2))), None],
            op::CALL | op::CALLCODE => {
                [Some((operand(3), operand(4))), Some((operand(5), operand(6)))]
            }
            op::DELEGATECALL | op::STATICCALL => {
                [Some((operand(2), operand(3))), Some((operand(4), operand(5)))]
            }
            _ => [None, None],
        }
    }

    /// Returns the memory section for the given instruction.
    #[allow(dead_code)]
    pub(crate) fn memory_section(&self, inst: Inst) -> MemorySection {
        self.memory_sections.get(inst).copied().unwrap_or_default()
    }

    /// Logs per-opcode constant-input statistics at trace level.
    #[inline(never)]
    fn log_const_input_stats(&self) {
        use op::*;

        // per_input[depth] = [total, const_count, fits_usize_count].
        struct Entry {
            inputs: usize,
            outputs: usize,
            total: u32,
            all_const: u32,
            const_output: u32,
            per_input: Vec<[u32; 3]>,
        }
        let mut stats = [const { None }; 256];
        for (inst, data) in self.iter_insts() {
            let (inputs, outputs) = data.stack_io();
            if matches!(data.opcode, PUSH0..=PUSH32 | DUP1..=DUP16 | SWAP1..=SWAP16 | DUPN | SWAPN)
            {
                continue;
            }
            let entry = stats[data.opcode as usize].get_or_insert_with(|| Entry {
                inputs: inputs as usize,
                outputs: outputs as usize,
                total: 0,
                all_const: 0,
                const_output: 0,
                per_input: vec![[0u32; 3]; inputs as usize],
            });
            entry.total += 1;
            let mut all = true;
            for (depth, counts) in entry.per_input.iter_mut().enumerate() {
                counts[0] += 1;
                if let Some(val) = self.const_operand(inst, depth) {
                    counts[1] += 1;
                    if val <= usize::MAX {
                        counts[2] += 1;
                    }
                } else {
                    all = false;
                }
            }
            if all {
                entry.all_const += 1;
            }
            if self.const_output(inst).is_some() {
                entry.const_output += 1;
            }
        }

        let mut buf = String::from("const input stats:");
        for (opcode, entry) in stats.iter().enumerate() {
            let Some(entry) = entry else { continue };
            if entry.total == 0 {
                continue;
            }
            let name = OpCode::new(opcode as u8)
                .map_or_else(|| format!("0x{opcode:02x}"), |o| o.as_str().to_string());
            let all_pct = entry.all_const as f64 / entry.total as f64 * 100.0;
            let _ = write!(
                buf,
                "\n  {name:<16} 0x{opcode:02x}, {total} occ, {inputs} inputs, all_const={ac}/{total} ({all_pct:.0}%)",
                opcode = opcode,
                total = entry.total,
                inputs = entry.inputs,
                ac = entry.all_const,
            );
            if entry.outputs > 0 {
                let co = entry.const_output;
                let co_pct = co as f64 / entry.total as f64 * 100.0;
                let _ = write!(buf, ", const_output={co}/{t} ({co_pct:.0}%)", t = entry.total);
            }
            for (i, [t, c, usize_c]) in entry.per_input.iter().enumerate() {
                if *t > 0 {
                    let pct = *c as f64 / *t as f64 * 100.0;
                    let usize_pct = *usize_c as f64 / *t as f64 * 100.0;
                    let _ = write!(
                        buf,
                        "\n    [{i}]: const={c}/{t} ({pct:.0}%), fits_usize={usize_c}/{t} ({usize_pct:.0}%)",
                    );
                }
            }
        }
        trace!("{buf}");
    }

    /// Collects IR statistics: instruction counts and block size distribution.
    fn ir_stats(&self) -> IrStats {
        let total = self.insts.len();
        let mut live = 0usize;
        let mut noops = 0usize;
        let mut dead = 0usize;
        for (_inst, data) in self.iter_all_insts() {
            if data.is_dead_code() {
                dead += 1;
            } else {
                live += 1;
                if data.flags.contains(InstFlags::NOOP) {
                    noops += 1;
                }
            }
        }

        let mut lens: Vec<usize> = self.cfg.blocks.iter().map(|data| data.insts().len()).collect();
        let n = lens.len();
        lens.sort_unstable();
        let block_min = lens.first().copied().unwrap_or(0);
        let block_max = lens.last().copied().unwrap_or(0);
        let sum: usize = lens.iter().sum();
        let block_avg = if n > 0 { sum as f64 / n as f64 } else { 0.0 };
        let block_median = if n > 0 { lens[n / 2] } else { 0 };

        IrStats {
            total,
            live,
            dead,
            noops,
            blocks: n,
            block_min,
            block_max,
            block_avg,
            block_median,
        }
    }

    /// Logs IR statistics and top 5 longest blocks at trace level.
    #[inline(never)]
    fn log_ir_stats(&self) {
        let s = self.ir_stats();
        trace!(
            total_insts = s.total,
            live = s.live,
            dead = s.dead,
            noops = s.noops,
            blocks = s.blocks,
            block_min = s.block_min,
            block_max = s.block_max,
            block_avg = format_args!("{:.1}", s.block_avg),
            block_median = s.block_median,
            "ir stats",
        );

        let mut lens: Vec<(Block, usize)> = self
            .cfg
            .blocks
            .iter_enumerated()
            .map(|(block, data)| (block, data.insts().len()))
            .collect();
        lens.sort_unstable_by_key(|b| std::cmp::Reverse(b.1));
        lens.truncate(5);
        let mut buf = String::from("top 5 longest blocks:");
        for (block, len) in &lens {
            let data = &self.cfg.blocks[*block];
            let _ = write!(buf, "\n  {block} (pc={}, len={len})", self.pc(data.insts.start));
        }
        trace!("{buf}");
    }

    /// Returns the name for a basic block.
    pub(crate) fn op_block_name(&self, inst: Option<Inst>, name: &str) -> String {
        let Some(inst) = inst else {
            return format!("entry.{name}");
        };
        let data = self.inst(inst);

        let mut s = String::with_capacity(64);
        if let Some(block) = self.cfg.inst_to_block[inst] {
            let _ = write!(s, "{block}.");
        }
        let _ = write!(s, "{inst}.{}", data.to_op());
        if !name.is_empty() {
            let _ = write!(s, ".{name}");
        }
        s
    }
}

/// Summary IR statistics for a compiled bytecode.
pub(crate) struct IrStats {
    pub(crate) total: usize,
    pub(crate) live: usize,
    pub(crate) dead: usize,
    pub(crate) noops: usize,
    pub(crate) blocks: usize,
    pub(crate) block_min: usize,
    pub(crate) block_max: usize,
    pub(crate) block_avg: f64,
    pub(crate) block_median: usize,
}

/// A single instruction in the bytecode.
#[derive(Clone, Default)]
pub(crate) struct InstData {
    /// The opcode byte.
    pub(crate) opcode: u8,
    /// Flags.
    pub(crate) flags: InstFlags,
    /// The base gas cost of the opcode.
    ///
    /// This may not be the final/full gas cost of the opcode as it may also have a dynamic cost.
    base_gas: u16,
    /// Stack inputs and outputs.
    stack_io: (u8, u8),
    /// Instruction-specific data, populated at parse time and refined by analysis.
    /// Interpretation depends on `opcode`; access via the typed methods on [`InstData`]:
    /// - `PUSH0..=PUSH32`, `DUPN | SWAPN | EXCHANGE`: encoded [`U256Imm`].
    /// - `PC`: program counter of this instruction.
    /// - `JUMPDEST`: program counter in low 31 bits, reachable flag in high bit
    ///   ([`InstData::JUMPDEST_REACHABLE`]).
    /// - `JUMP | JUMPI` with [`InstFlags::STATIC_JUMP`]: target instruction index.
    /// - `GAS`: static gas of the rest of its gas section, which the section head already charged.
    /// - otherwise: unused.
    pub(crate) data: u32,
    /// The gas section this instruction belongs to.
    pub(crate) gas_section: GasSection,
    /// The stack section this instruction belongs to.
    pub(crate) stack_section: StackSection,
}

impl PartialEq<u8> for InstData {
    #[inline]
    fn eq(&self, other: &u8) -> bool {
        self.opcode == *other
    }
}

impl PartialEq<InstData> for u8 {
    #[inline]
    fn eq(&self, other: &InstData) -> bool {
        *self == other.opcode
    }
}

impl InstData {
    /// Creates a new instruction data with the given opcode byte.
    /// Note that this may not be a valid instruction.
    #[inline]
    fn new(opcode: u8) -> Self {
        Self { opcode, stack_io: stack_io(opcode), ..Default::default() }
    }

    /// Returns the length of the immediate data of this instruction.
    #[inline]
    pub(crate) const fn imm_len(&self) -> u8 {
        min_imm_len(self.opcode)
    }

    /// Returns the number of input and output stack elements of this instruction.
    #[inline]
    pub(crate) const fn stack_io(&self) -> (u8, u8) {
        self.stack_io
    }

    /// Converts this instruction to a raw opcode. Note that the immediate data is not resolved.
    #[inline]
    pub(crate) const fn to_op(&self) -> Opcode<'static> {
        Opcode { opcode: self.opcode, immediate: None }
    }

    /// Returns `true` if this instruction is a jump instruction (`JUMP`/`JUMPI`).
    #[inline]
    pub(crate) const fn is_jump(&self) -> bool {
        matches!(self.opcode, op::JUMP | op::JUMPI)
    }

    /// Returns `true` if this instruction is a jump instruction (`JUMP`/`JUMPI`), and the
    /// target known statically.
    #[inline]
    pub(crate) const fn is_static_jump(&self) -> bool {
        self.is_jump() && self.flags.contains(InstFlags::STATIC_JUMP)
    }

    /// Returns `true` if this instruction is a `JUMPI` whose condition is known statically.
    #[inline]
    pub(crate) const fn has_const_jumpi_condition(&self) -> bool {
        self.opcode == op::JUMPI && self.flags.contains(InstFlags::CONST_JUMP_CONDITION)
    }

    /// Returns `true` if this instruction is a `JUMPDEST`.
    #[inline]
    pub(crate) const fn is_jumpdest(&self) -> bool {
        self.opcode == op::JUMPDEST
    }

    /// Returns `true` if this instruction is a reachable `JUMPDEST`.
    #[inline]
    pub(crate) fn is_reachable_jumpdest(&self, has_dynamic_jumps: bool) -> bool {
        self.is_jumpdest() && (has_dynamic_jumps || self.is_jumpdest_reachable())
    }

    /// Returns the [`U256Imm`] of a `PUSH*`/`DUPN`/`SWAPN`/`EXCHANGE` instruction.
    #[inline]
    pub(crate) fn imm(&self) -> U256Imm {
        debug_assert!(matches!(
            self.opcode,
            op::PUSH0..=op::PUSH32 | op::DUPN | op::SWAPN | op::EXCHANGE
        ));
        U256Imm::from_raw(self.data)
    }

    /// Returns the inline `u8` immediate of a `DUPN`/`SWAPN`/`EXCHANGE` instruction.
    #[inline]
    pub(crate) fn imm_byte(&self) -> u8 {
        debug_assert!(matches!(self.opcode, op::DUPN | op::SWAPN | op::EXCHANGE));
        // Parse-time encoding for these opcodes always produces an inline `u8`.
        self.imm().get_inline().unwrap().try_into().unwrap()
    }

    /// Returns the program counter stored inline for a `PC` instruction.
    #[inline]
    pub(crate) fn pc_imm(&self) -> u32 {
        debug_assert_eq!(self.opcode, op::PC);
        self.data
    }

    /// High bit of `data` set when a `JUMPDEST` is a reachable static jump target.
    pub(crate) const JUMPDEST_REACHABLE: u32 = 1 << 31;

    /// Returns the static gas that the section head charged for instructions after this `GAS`.
    #[inline]
    pub(crate) fn gas_owed(&self) -> u32 {
        debug_assert_eq!(self.opcode, op::GAS);
        self.data
    }

    /// Sets the static gas charged ahead of this `GAS` instruction.
    #[inline]
    pub(crate) fn set_gas_owed(&mut self, gas: u32) {
        debug_assert_eq!(self.opcode, op::GAS);
        self.data = gas;
    }

    /// Returns the program counter of a `JUMPDEST`.
    #[inline]
    pub(crate) fn jumpdest_pc(&self) -> u32 {
        debug_assert_eq!(self.opcode, op::JUMPDEST);
        self.data & !Self::JUMPDEST_REACHABLE
    }

    /// Returns `true` if this `JUMPDEST` is the target of a resolved static jump.
    #[inline]
    pub(crate) fn is_jumpdest_reachable(&self) -> bool {
        debug_assert_eq!(self.opcode, op::JUMPDEST);
        self.data & Self::JUMPDEST_REACHABLE != 0
    }

    /// Marks this `JUMPDEST` as a reachable static jump target.
    #[inline]
    pub(crate) fn set_jumpdest_reachable(&mut self) {
        debug_assert_eq!(self.opcode, op::JUMPDEST);
        self.data |= Self::JUMPDEST_REACHABLE;
    }

    #[inline]
    pub(crate) fn clear_jumpdest_reachable(&mut self) {
        debug_assert_eq!(self.opcode, op::JUMPDEST);
        self.data &= !Self::JUMPDEST_REACHABLE;
    }

    /// Returns the static target of a `JUMP`/`JUMPI` with [`InstFlags::STATIC_JUMP`].
    #[inline]
    pub(crate) fn static_jump_target(&self) -> Inst {
        debug_assert!(self.is_static_jump());
        Inst::new(self.data as usize)
    }

    /// Sets the static jump target. Caller must also set [`InstFlags::STATIC_JUMP`].
    #[inline]
    pub(crate) fn set_static_jump_target(&mut self, target: Inst) {
        debug_assert!(self.is_jump());
        self.data = target.index() as u32;
    }

    /// Returns `true` if this instruction starts a new stack section.
    #[inline]
    pub(crate) const fn is_stack_section_head(&self) -> bool {
        self.flags.contains(InstFlags::STACK_SECTION_HEAD)
    }

    /// Returns `true` if this stack section head continues the previous section.
    #[inline]
    pub(crate) const fn continues_stack_section(&self) -> bool {
        self.flags.contains(InstFlags::CONTINUES_STACK_SECTION)
    }

    /// Returns `true` if this instruction is dead code.
    pub(crate) const fn is_dead_code(&self) -> bool {
        self.flags.contains(InstFlags::DEAD_CODE)
    }

    /// Returns `true` if this instruction has host effects that depend on `gasleft()`.
    ///
    /// These end the gas and stack sections: charging the static gas of later instructions, or
    /// failing their stack checks, ahead of the instruction would change its gas checks and skip
    /// host reads that survive a revert, such as EIP-7928 block access list entries. `GAS` also
    /// reads `gasleft()`, but it is pure, so it adds back the gas charged ahead of it instead.
    #[inline]
    pub(crate) const fn ends_sections(&self) -> bool {
        // The host's instruction may read the remaining gas.
        if self.flags.contains(InstFlags::HOST) {
            return true;
        }
        match self.opcode {
            // Calls and contract creation cap the gas forwarded based on the remaining gas, and
            // the callee can return a value transfer's stipend, leaving more gas than before.
            op::CREATE
            | op::CALL
            | op::CALLCODE
            | op::DELEGATECALL
            | op::CREATE2
            | op::STATICCALL => true,
            // EIP-2200 rejects SSTORE when the remaining gas is at most the call stipend, and
            // EIP-2929 skips cold host loads when the remaining gas cannot cover their cost. The
            // active features, not the base spec, decide whether either applies.
            op::SSTORE
            | op::BALANCE
            | op::EXTCODESIZE
            | op::EXTCODECOPY
            | op::EXTCODEHASH
            | op::SLOAD
            | op::SELFDESTRUCT => true,
            _ => false,
        }
    }

    /// Returns `true` if execution can fall through to the next sequential instruction.
    #[inline]
    pub(crate) const fn can_fall_through(&self) -> bool {
        !self.is_diverging() && self.opcode != op::JUMP && !self.has_const_jumpi_condition()
    }

    /// Returns `true` if we know that this instruction will branch or stop execution.
    #[inline]
    pub(crate) const fn is_branching(&self) -> bool {
        self.is_jump() || self.is_diverging()
    }

    /// Returns `true` if we know that this instruction will stop execution.
    #[inline]
    pub(crate) const fn is_diverging(&self) -> bool {
        (self.opcode == op::JUMP && self.flags.contains(InstFlags::INVALID_JUMP))
            || self.flags.contains(InstFlags::DISABLED)
            || self.flags.contains(InstFlags::UNKNOWN)
            || self.flags.contains(InstFlags::INVALID_IMMEDIATE)
            || matches!(
                self.opcode,
                op::STOP | op::RETURN | op::REVERT | op::INVALID | op::SELFDESTRUCT
            )
    }
}

bitflags::bitflags! {
    /// [`InstrData`] flags.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct InstFlags: u16 {
        /// The `JUMP`/`JUMPI` target is known at compile time.
        const STATIC_JUMP = 1 << 0;
        /// The jump target is known to be invalid.
        /// Always returns [`InvalidJump`](evm2::interpreter::InstrStop::InvalidJump) at runtime.
        const INVALID_JUMP = 1 << 1;
        /// The jump has multiple known targets (see `Bytecode::multi_jump_targets`).
        /// The target value is still on the stack and must be popped and switched on at runtime.
        const MULTI_JUMP = 1 << 2;
        /// The `JUMPI` condition is known at compile time.
        const CONST_JUMP_CONDITION = 1 << 3;

        /// The instruction is disabled in this EVM version.
        /// Always returns [`NotActivated`](evm2::interpreter::InstrStop::NotActivated) at runtime.
        const DISABLED = 1 << 4;
        /// The instruction is unknown.
        /// Always returns [`OpcodeNotFound`](evm2::interpreter::InstrStop::OpcodeNotFound) at runtime.
        const UNKNOWN = 1 << 5;

        /// Instruction is a no-op: skip generating logic, but keep the gas calculation.
        const NOOP = 1 << 6;
        /// This instruction starts a new stack section.
        const STACK_SECTION_HEAD = 1 << 7;
        /// Don't generate any code.
        const DEAD_CODE = 1 << 8;
        /// Dedup redirected another block here, so it can be entered from more than one place
        /// without a jump.
        const REDIRECT_TARGET = 1 << 9;
        /// This stack section head is only entered by falling through the preceding `JUMPI`,
        /// so it keeps the previous section's virtual stack and stack length. Its
        /// `stack_section` holds only the bounds that the previous sections have not checked;
        /// the virtual stack already tracks the checked range.
        const CONTINUES_STACK_SECTION = 1 << 10;
        /// The host's own instruction implementation executes this opcode, through a builtin.
        const HOST = 1 << 11;
        /// The host implements this opcode in a way compiled code cannot reproduce, so a
        /// program where it is reachable is not compiled.
        const UNSUPPORTED_HOST = 1 << 12;
        /// The `DUPN`, `SWAPN`, or `EXCHANGE` immediate is invalid.
        /// Always returns [`InvalidImmediateEncoding`](evm2::interpreter::InstrStop::InvalidImmediateEncoding)
        /// at runtime.
        const INVALID_IMMEDIATE = 1 << 13;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_push_imm_right_pads_with_zeros() {
        // PUSH2 followed by a single byte 0x42 — truncated at EOF.
        // EVM spec: missing bytes are zero, so the value is 0x4200.
        let code = [op::PUSH2, 0x42];
        let bc = Bytecode::test(&code);
        let inst = Inst::new(0);
        let data = bc.inst(inst);
        assert_eq!(bc.get_imm(inst), Some([0x42].as_slice()));
        assert_eq!(bc.get_push_value(data), U256::from(0x4200));

        // PUSH3 followed by two bytes — truncated at EOF.
        let code = [op::PUSH3, 0xAB, 0xCD];
        let bc = Bytecode::test(&code);
        let inst = Inst::new(0);
        let data = bc.inst(inst);
        assert_eq!(bc.get_imm(inst), Some([0xAB, 0xCD].as_slice()));
        assert_eq!(bc.get_push_value(data), U256::from(0xABCD00));

        // PUSH1 with no immediate bytes at all.
        let code = [op::PUSH1];
        let bc = Bytecode::test(&code);
        let inst = Inst::new(0);
        let data = bc.inst(inst);
        assert_eq!(bc.get_imm(inst), None);
        assert_eq!(bc.get_push_value(data), U256::ZERO);

        // Non-truncated PUSH2 — full immediate.
        let code = [op::PUSH2, 0x42, 0xFF];
        let bc = Bytecode::test(&code);
        let data = bc.inst(Inst::new(0));
        assert_eq!(bc.get_push_value(data), U256::from(0x42FF));
    }

    #[test]
    fn false_jumpi_fallthrough_terminator_marks_following_dead_code() {
        let mut bc = Bytecode::test(&[
            op::PUSH0,
            op::CALLDATASIZE,
            op::JUMPI,
            op::STOP,
            op::CALLDATALOAD,
            op::JUMP,
        ]);
        let jumpi = Inst::new(2);
        let fallthrough = jumpi + 1;
        let dynamic_jump = Inst::new(5);

        bc.inst_mut(jumpi).flags |= InstFlags::STATIC_JUMP | InstFlags::CONST_JUMP_CONDITION;
        bc.inst_mut(jumpi).set_static_jump_target(fallthrough);
        bc.has_dynamic_jumps = true;
        bc.mark_dead_code();

        assert!(!bc.inst(fallthrough).is_dead_code());
        assert!(bc.inst(Inst::new(4)).is_dead_code());
        assert!(bc.inst(dynamic_jump).is_dead_code());
        assert!(!bc.has_dynamic_jumps);
    }

    #[test]
    fn dead_fallthrough_jump_does_not_keep_jumpdest_reachable() {
        let mut bc = Bytecode::test(&[
            op::PUSH1,
            5,
            op::PUSH1,
            1,
            op::JUMPI,
            op::PUSH1,
            8,
            op::JUMP,
            op::JUMPDEST,
            op::STOP,
            op::JUMPDEST,
            op::STOP,
        ]);
        let jumpi = Inst::new(2);
        let dead_jump = Inst::new(4);
        let live_target = Inst::new(5);
        let stale_target = Inst::new(7);

        bc.inst_mut(jumpi).flags |= InstFlags::STATIC_JUMP | InstFlags::CONST_JUMP_CONDITION;
        bc.inst_mut(jumpi).set_static_jump_target(live_target);
        bc.inst_mut(dead_jump).flags |= InstFlags::STATIC_JUMP;
        bc.inst_mut(dead_jump).set_static_jump_target(stale_target);
        bc.inst_mut(live_target).set_jumpdest_reachable();
        bc.inst_mut(stale_target).set_jumpdest_reachable();

        bc.mark_dead_code();

        assert!(bc.inst(dead_jump).is_dead_code());
        assert!(bc.inst(live_target).is_jumpdest_reachable());
        assert!(!bc.inst(stale_target).is_jumpdest_reachable());
        assert!(bc.inst(stale_target).is_dead_code());
    }
}
