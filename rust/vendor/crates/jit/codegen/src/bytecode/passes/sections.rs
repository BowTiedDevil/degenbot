use crate::bytecode::{Bytecode, Inst, InstFlags};
use core::{fmt, ops::Range};
use evm2::interpreter::op;
use smallvec::SmallVec;

/// A gas section tracks the total base gas cost of a sequence of instructions.
///
/// Gas sections end at branching instructions and at host effects that depend on `gasleft`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GasSection {
    /// The total base gas cost of all instructions in the section.
    pub(crate) gas_cost: u32,
}

impl fmt::Debug for GasSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            f.write_str("GasSection::EMPTY")
        } else {
            f.debug_struct("GasSection").field("gas_cost", &self.gas_cost).finish()
        }
    }
}

impl GasSection {
    /// The cost recorded for a section that costs at least this much.
    ///
    /// A section's cost is stored in 32 bits, so a larger one saturates here. Section gas is
    /// charged ahead of the instructions, so a saturated cost would charge too little, and
    /// [`Bytecode::analyze`] rejects the bytecode instead.
    pub(crate) const SATURATED: u32 = u32::MAX;

    /// Returns `true` if the section is empty.
    #[inline]
    pub(crate) fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// A stack section tracks stack height requirements for a sequence of instructions.
///
/// Stack sections end where gas sections end. This keeps stack failures after host-effect
/// instructions from being hoisted ahead of those instructions.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StackSection {
    /// The stack height required to execute the section.
    pub(crate) inputs: u16,
    /// The maximum stack height growth relative to the stack height at section start.
    pub(crate) max_growth: i16,
}

impl fmt::Debug for StackSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            f.write_str("StackSection::EMPTY")
        } else {
            f.debug_struct("StackSection")
                .field("inputs", &self.inputs)
                .field("max_growth", &self.max_growth)
                .finish()
        }
    }
}

impl StackSection {
    /// Returns `true` if the section is empty.
    #[inline]
    pub(crate) fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// Compute a stack section from an iterator of `(inputs, outputs)` pairs.
    pub(crate) fn from_stack_io(iter: impl IntoIterator<Item = (u8, u8)>) -> Self {
        let mut analysis = StackSectionAnalysis::default();
        for (inp, out) in iter {
            analysis.process(inp, out);
        }
        analysis.section()
    }
}

/// Gas section analysis state.
pub(crate) struct GasSectionAnalysis {
    gas_cost: u64,
    start_inst: Inst,
    /// `GAS` instructions in the section, with the section gas up to and including each one.
    gas_insts: SmallVec<[(Inst, u64); 2]>,
}

impl Default for GasSectionAnalysis {
    fn default() -> Self {
        Self { gas_cost: 0, start_inst: Inst::new(0), gas_insts: SmallVec::new() }
    }
}

impl GasSectionAnalysis {
    /// Accumulates a base gas cost.
    #[inline]
    pub(crate) const fn process(&mut self, base_gas: u16) {
        self.process_extra(base_gas as u64);
    }

    /// Accumulates extra gas (e.g. pre-computed dynamic gas for folded instructions).
    #[inline]
    pub(crate) const fn process_extra(&mut self, gas: u64) {
        self.gas_cost += gas;
    }

    /// Records a `GAS` instruction after accumulating its base gas.
    fn process_gas(&mut self, inst: Inst) {
        self.gas_insts.push((inst, self.gas_cost));
    }

    fn save_to_reset(&mut self, bytecode: &mut Bytecode<'_>, next_section_inst: Inst) {
        self.save_to(bytecode, next_section_inst);
        self.reset(next_section_inst);
    }

    /// Saves the current gas section to the bytecode.
    fn save_to(&self, bytecode: &mut Bytecode<'_>, next_section_inst: Inst) {
        if self.start_inst >= bytecode.insts.len_idx() {
            return;
        }
        let section = self.section();
        if !section.is_empty() {
            let _ = next_section_inst;
            // trace!(
            //     inst = %self.start_inst,
            //     len = %(next_section_inst - self.start_inst).0,
            //     ?section,
            //     "saving gas"
            // );
            let mut insts = bytecode.insts[self.start_inst..].iter_mut();
            if let Some(inst) = insts.find(|inst| !inst.is_dead_code()) {
                inst.gas_section = section;
            }
        }
        for &(inst, gas_cost) in &self.gas_insts {
            let owed = (self.gas_cost - gas_cost).try_into().unwrap_or(u32::MAX);
            bytecode.inst_mut(inst).set_gas_owed(owed);
        }
    }

    /// Resets the analysis for a new section.
    pub(crate) fn reset(&mut self, inst: Inst) {
        *self = Self { start_inst: inst, ..Default::default() };
    }

    /// Returns the current gas section.
    pub(crate) fn section(&self) -> GasSection {
        GasSection { gas_cost: self.gas_cost.try_into().unwrap_or(u32::MAX) }
    }
}

/// Stack section analysis state.
pub(crate) struct StackSectionAnalysis {
    inputs: i32,
    diff: i32,
    max_growth: i32,
    start_inst: Inst,
}

impl Default for StackSectionAnalysis {
    fn default() -> Self {
        Self { inputs: 0, diff: 0, max_growth: 0, start_inst: Inst::new(0) }
    }
}

impl StackSectionAnalysis {
    /// Returns the cumulative stack delta so far.
    #[inline]
    pub(crate) const fn diff(&self) -> i32 {
        self.diff
    }

    /// Accumulates a single instruction's stack I/O.
    #[inline]
    pub(crate) fn process(&mut self, inputs: u8, outputs: u8) {
        self.inputs = self.inputs.max(inputs as i32 - self.diff);
        self.diff += outputs as i32 - inputs as i32;
        self.max_growth = self.max_growth.max(self.diff);
    }

    /// Saves the current stack section to the bytecode.
    fn save_to(&self, bytecode: &mut Bytecode<'_>, section: StackSection, continues: bool) {
        if self.start_inst >= bytecode.insts.len_idx() {
            return;
        }
        let mut insts = bytecode.insts[self.start_inst..].iter_mut();
        if let Some(inst) = insts.find(|inst| !inst.is_dead_code()) {
            inst.flags |= InstFlags::STACK_SECTION_HEAD;
            inst.flags.set(InstFlags::CONTINUES_STACK_SECTION, continues);
            inst.stack_section = section;
        }
    }

    /// Resets the analysis for a new section.
    pub(crate) fn reset(&mut self, inst: Inst) {
        *self = Self { start_inst: inst, ..Default::default() };
    }

    /// Returns the current stack section.
    pub(crate) fn section(&self) -> StackSection {
        StackSection {
            inputs: self.inputs.try_into().unwrap_or(u16::MAX),
            max_growth: self.max_growth.try_into().unwrap_or(i16::MAX),
        }
    }
}

/// Instruction section analysis, tracking gas and stack sections separately.
#[derive(Default)]
pub(crate) struct SectionsAnalysis {
    gas: GasSectionAnalysis,
    stack: StackSectionAnalysis,
    /// Whether the current stack section continues the previous one. See
    /// [`InstFlags::CONTINUES_STACK_SECTION`].
    continues: bool,
    /// Start of the current stack section, relative to the start of the first section in its
    /// chain of continuations.
    chain_offset: i32,
    /// Stack range already checked by the current chain of continuations, relative to the
    /// start of its first section.
    checked: Range<i32>,
}

impl SectionsAnalysis {
    /// Process a single instruction.
    pub(crate) fn process(&mut self, bytecode: &mut Bytecode<'_>, inst: Inst) {
        // JUMPDEST starts both gas and stack sections.
        if bytecode.inst(inst).is_reachable_jumpdest(bytecode.has_dynamic_jumps()) {
            self.save_stack_reset(bytecode, inst, false);
            self.gas.save_to_reset(bytecode, inst);
        }

        let data = bytecode.inst(inst);
        let (inp, out) = data.stack_io();
        self.stack.process(inp, out);
        self.gas.process(data.base_gas);
        if data.opcode == op::GAS {
            self.gas.process_gas(inst);
        }

        // The exponent is known, so translation does not charge EXP's dynamic gas.
        if data.opcode == op::EXP
            && let Some(exponent) = bytecode.exp_static_exponent(inst)
        {
            self.gas.process_extra(bytecode.gas_params.exp_cost(exponent));
        }

        // Branching instructions and host effects that depend on the remaining gas end both
        // sections.
        let next = inst + 1;
        if data.is_branching() || data.ends_sections() {
            let continues = continues_into(bytecode, inst);
            self.save_stack_reset(bytecode, next, continues);
            self.gas.save_to_reset(bytecode, next);
        }
    }

    /// Saves the current stack section and starts a new one at `next_section_inst`.
    fn save_stack_reset(
        &mut self,
        bytecode: &mut Bytecode<'_>,
        next_section_inst: Inst,
        next_continues: bool,
    ) {
        self.save_stack(bytecode);
        self.chain_offset += self.stack.diff();
        self.continues = next_continues;
        self.stack.reset(next_section_inst);
    }

    /// Saves the current stack section. A continuation keeps only the bounds that its chain
    /// has not checked yet.
    fn save_stack(&mut self, bytecode: &mut Bytecode<'_>) {
        let mut section = self.stack.section();
        if self.continues {
            let low = self.chain_offset - section.inputs as i32;
            let high = self.chain_offset + section.max_growth as i32;
            if low >= self.checked.start {
                section.inputs = 0;
            }
            if high <= self.checked.end {
                section.max_growth = 0;
            }
            self.checked = self.checked.start.min(low)..self.checked.end.max(high);
        } else {
            self.chain_offset = 0;
            self.checked = -(section.inputs as i32)..section.max_growth as i32;
        }
        self.stack.save_to(bytecode, section, self.continues);
    }

    /// Finishes the analysis.
    pub(crate) fn finish(mut self, bytecode: &mut Bytecode<'_>) {
        let last = bytecode.insts.len_idx() - 1;
        self.gas.save_to(bytecode, last);
        self.save_stack(bytecode);
        if enabled!(tracing::Level::DEBUG) {
            let mut max_len = 0usize;
            let mut count = 0usize;
            let mut section_len = 0usize;
            for (_inst, data) in bytecode.iter_insts() {
                section_len += 1;
                if !data.is_stack_section_head() {
                    continue;
                }
                // `section_len` includes this head; the previous section had `section_len - 1`.
                if count > 0 {
                    max_len = max_len.max(section_len - 1);
                }
                section_len = 1;
                count += 1;
            }
            // Account for the last section.
            max_len = max_len.max(section_len);
            debug!(count, max_len, "sections");
        }
    }
}

/// Returns `true` if the stack section can continue past `inst` into the next instruction:
/// `inst` is a `JUMPI` that may fall through, and nothing else enters the next instruction.
fn continues_into(bytecode: &Bytecode<'_>, inst: Inst) -> bool {
    let next = inst + 1;
    bytecode.inst(inst).opcode == op::JUMPI
        && next < bytecode.insts.len_idx()
        && !bytecode.inst(next).is_dead_code()
        && bytecode.is_fall_through_only(next)
}

#[cfg(test)]
mod tests {
    use crate::bytecode::{
        Inst,
        passes::block_analysis::tests::{analyze_asm, analyze_asm_spec, analyze_code_spec},
    };
    use evm2::{SpecId, interpreter::op};

    /// Returns the gas section cost for the first non-dead instruction (the section head).
    fn section_gas(src: &str) -> u32 {
        let bytecode = analyze_asm(src);
        bytecode.inst(Inst::new(0)).gas_section.gas_cost
    }

    /// EXP with an asymmetric base/exponent: base is 1 byte (2), exponent is 2 bytes (256).
    /// If the wrong operand were used for gas, the dynamic cost would be 50*1=50 instead of
    /// 50*2=100, and the total would differ.
    #[test]
    fn exp_folded_gas_uses_exponent_not_base() {
        // PUSH 256, PUSH 2, EXP, PUSH0, MSTORE, STOP
        // Gas: PUSH(3) + PUSH(3) + EXP_base(10) + EXP_dynamic(50*2=100) + PUSH0(2) + MSTORE(3) +
        // STOP(0)    = 3 + 3 + 10 + 100 + 2 + 3 + 0 = 121
        let gas = section_gas("PUSH 256 PUSH 2 EXP PUSH0 MSTORE STOP");
        assert_eq!(gas, 121, "dynamic gas must use exponent (256=2 bytes), not base (2=1 byte)");
    }

    /// EXP with zero exponent has no dynamic gas.
    #[test]
    fn exp_folded_gas_zero_exponent() {
        // PUSH0(2) + PUSH(3) + EXP_base(10) + EXP_dynamic(0) + PUSH0(2) + MSTORE(3) + STOP(0) = 20
        let gas = section_gas("PUSH 0 PUSH 2 EXP PUSH0 MSTORE STOP");
        assert_eq!(gas, 20, "zero exponent should have no dynamic gas");
    }

    /// EXP with max exponent (32 bytes).
    #[test]
    fn exp_folded_gas_max_exponent() {
        // PUSH U256::MAX (32 bytes), PUSH 2, EXP, PUSH0, MSTORE, STOP
        // Gas: PUSH(3) + PUSH(3) + EXP_base(10) + EXP_dynamic(50*32=1600) + PUSH0(2) + MSTORE(3) +
        // STOP(0)    = 3 + 3 + 10 + 1600 + 2 + 3 + 0 = 1621
        let gas = section_gas(
            "PUSH 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff \
             PUSH 2 EXP PUSH0 MSTORE STOP",
        );
        assert_eq!(gas, 1621, "max exponent should cost 50*32=1600 dynamic gas");
    }

    #[test]
    fn call_ends_section() {
        let bytecode = analyze_code_spec(
            vec![
                op::PUSH1,
                0,
                op::PUSH1,
                0,
                op::PUSH1,
                0,
                op::PUSH1,
                0,
                op::CALLVALUE,
                op::PUSH1,
                0,
                op::CALLDATALOAD,
                op::GAS,
                op::CALL,
                op::PUSH1,
                0,
                op::SSTORE,
            ],
            SpecId::CANCUN,
        );
        let head = bytecode.inst(Inst::new(0));
        let gas = bytecode.inst(Inst::new(7));
        let call = bytecode.inst(Inst::new(8));
        let after_call = bytecode.inst(Inst::new(9));
        // `GAS` does not end the section: 4 * PUSH1 + CALLVALUE + PUSH1 + CALLDATALOAD + GAS +
        // CALL.
        assert_eq!(head.gas_section.gas_cost, 4 * 3 + 2 + 3 + 3 + 2 + 100);
        assert_eq!(gas.gas_owed(), 100);
        assert!(call.gas_section.is_empty());
        assert_eq!(after_call.gas_section.gas_cost, 3);
        assert!(after_call.is_stack_section_head());
    }

    #[test]
    fn gas_owes_rest_of_section() {
        // PUSH0(2) GAS(2) PUSH 256(3) PUSH 2(3) EXP(10 + 50 * 2) GAS(2) POP(2) SLOAD(100) |
        // PUSH0(2)
        let bytecode = analyze_asm_spec(
            "PUSH0 GAS PUSH 256 PUSH 2 EXP GAS POP SLOAD PUSH0 STOP",
            SpecId::CANCUN,
        );
        let head = bytecode.inst(Inst::new(0));
        assert_eq!(head.gas_section.gas_cost, 2 + 2 + 3 + 3 + 110 + 2 + 2 + 100);
        let owed = bytecode
            .iter_insts()
            .filter(|(_, data)| data.opcode == op::GAS)
            .map(|(_, data)| data.gas_owed())
            .collect::<Vec<_>>();
        assert_eq!(owed, [3 + 3 + 110 + 2 + 2 + 100, 2 + 100]);
    }

    #[test]
    fn cold_load_ends_section() {
        let cases = [
            ("{p} BALANCE {p} STOP", op::BALANCE),
            ("{p} EXTCODESIZE {p} STOP", op::EXTCODESIZE),
            ("{p} {p} {p} {p} EXTCODECOPY {p} STOP", op::EXTCODECOPY),
            ("{p} EXTCODEHASH {p} STOP", op::EXTCODEHASH),
            ("{p} SLOAD {p} STOP", op::SLOAD),
            ("{p} {p} SSTORE {p} STOP", op::SSTORE),
        ];

        // Active features can enable EIP-2200 and EIP-2929 over any base spec, so sections end at
        // these opcodes even before Istanbul and Berlin.
        for (spec, push, push_gas) in
            [(SpecId::AMSTERDAM, "PUSH0", 2), (SpecId::PETERSBURG, "PUSH1 0", 3)]
        {
            for (src, opcode) in cases {
                let bytecode = analyze_asm_spec(&src.replace("{p}", push), spec);
                let (inst, data) =
                    bytecode.iter_insts().find(|(_, data)| data.opcode == opcode).unwrap();
                assert!(data.ends_sections());

                let after = bytecode.inst(inst + 1);
                assert!(after.is_stack_section_head());
                assert_eq!(after.gas_section.gas_cost, push_gas);
            }
        }
    }

    /// Disabled opcodes must not contribute stack I/O or gas to the preceding section.
    /// Otherwise the section-head underflow check fires before the disabled opcode's
    /// `NotActivated` guard, producing a divergent `StackUnderflow`.
    #[test]
    fn disabled_opcode_does_not_poison_section() {
        // CALLDATASIZE(0→1) ; TSTORE(2→0, disabled before Cancun)
        let bytecode = analyze_asm_spec("CALLDATASIZE TSTORE", SpecId::SHANGHAI);
        let head = bytecode.inst(Inst::new(0));
        assert_eq!(
            head.stack_section.inputs, 0,
            "CALLDATASIZE needs 0 inputs; disabled TSTORE must not inflate the section"
        );
        assert_eq!(head.gas_section.gas_cost, 2, "only CALLDATASIZE gas (2) should be charged");
    }

    /// DUPN with an invalid immediate diverges like a disabled opcode, so the instructions after
    /// it do not join its section: translate halts with `InvalidImmediateEncoding` before them.
    #[test]
    fn invalid_dupn_imm_diverges() {
        // PUSH0, DUPN 0x5b (invalid immediate), PUSH0, SSTORE, STOP
        let bytecode = analyze_code_spec(
            vec![op::PUSH0, op::DUPN, 0x5b, op::PUSH0, op::SSTORE, op::STOP],
            SpecId::AMSTERDAM,
        );
        let dupn = bytecode.inst(Inst::new(1));
        assert!(dupn.is_diverging(), "DUPN with invalid immediate must diverge");
        assert_eq!(dupn.stack_io(), (0, 0), "DUPN with invalid immediate must have zero stack I/O");

        // The instructions after it are dead, so they add no stack inputs or gas to a section.
        let after = bytecode.inst(Inst::new(3));
        assert!(after.is_dead_code());
    }
}
