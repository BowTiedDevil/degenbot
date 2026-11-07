use crate::{
    BaseEvmConfig, EvmConfig, EvmTypesHost, SpecId,
    interpreter::{
        instructions as instr, op,
        private::{Instruction, InstructionFlags, InstructionImplFn},
    },
};
use core::fmt;

/// Type-specific opcode configuration.
///
/// Stores the static gas table and instruction implementations for a concrete `EvmTypesHost`
/// family. This is a compile-time input used to build the final interpreter dispatch table.
pub struct OpcodeConfig<T: EvmTypesHost> {
    /// Static opcode gas table.
    static_gas_table: StaticGasTable,
    /// Instruction implementations.
    instruction_impls: InstructionImplTable<T>,
    /// Per-opcode revision for dispatch-table rebuild decisions.
    revisions: [u8; 256],
    /// Number of times each opcode's instruction was set.
    instruction_sets: [u8; 256],
    /// The base spec whose table this config started from.
    origin: SpecId,
}

pub(crate) struct InstructionInfo<T: EvmTypesHost> {
    pub(crate) instr: InstructionImplFn<T>,
    pub(crate) flags: InstructionFlags,
}

impl<T: EvmTypesHost> fmt::Debug for OpcodeConfig<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpcodeConfig").finish_non_exhaustive()
    }
}

impl<T: EvmTypesHost> OpcodeConfig<T> {
    /// Returns the base type-specific opcode config for `C`.
    pub const fn base<C: EvmConfig<T>>() -> Self {
        super::base_opcode_config::<T, C>()
    }

    /// Creates empty type-specific opcode config.
    #[inline]
    pub(super) const fn empty(origin: SpecId) -> Self {
        Self {
            origin,
            static_gas_table: StaticGasTable::empty(),
            instruction_impls: InstructionImplTable::empty(),
            revisions: [0; 256],
            instruction_sets: [0; 256],
        }
    }

    /// Returns the static gas cost for `opcode`.
    #[inline(always)]
    pub const fn static_gas(&self, opcode: u8) -> u16 {
        self.static_gas_table.get(opcode)
    }

    /// Returns the base opcode config for `spec`.
    pub fn base_for_spec(spec: SpecId) -> &'static Self {
        crate::spec_to_generic!(spec, |BASE_SPEC_ID| {
            <BaseEvmConfig<BASE_SPEC_ID> as EvmConfig<T>>::OPCODE_CONFIG
        })
    }

    /// Returns the base spec whose table this config started from.
    #[inline]
    pub const fn origin_spec(&self) -> SpecId {
        self.origin
    }

    /// Returns whether `opcode` has an instruction implementation.
    #[inline]
    pub const fn is_defined(&self, opcode: u8) -> bool {
        !self.is_unknown_opcode(opcode)
    }

    /// Returns whether the instruction of `opcode` was overridden or added relative to `base`, a
    /// config derived from the same base spec.
    ///
    /// This holds for an instruction that was set again to an identical implementation.
    #[inline]
    pub const fn is_overridden(&self, base: &Self, opcode: u8) -> bool {
        self.instruction_sets[opcode as usize] != base.instruction_sets[opcode as usize]
    }

    /// Returns whether `self` and `other` define the same opcodes, static gas and instruction
    /// overrides.
    pub fn same_table(&self, other: &Self) -> bool {
        core::ptr::eq(self, other)
            || self.origin == other.origin
                && (0..=u8::MAX).all(|op| {
                    self.static_gas(op) == other.static_gas(op)
                        && self.is_defined(op) == other.is_defined(op)
                        && !self.is_overridden(other, op)
                })
    }

    /// Returns a type-independent view of this config.
    ///
    /// `base` is the base config the opcodes are compared with, which must derive from the same
    /// base spec. Without one, every defined opcode counts as [`OpcodeChange::Instruction`].
    pub fn view(&self, base: Option<&Self>) -> OpcodeView {
        OpcodeView {
            static_gas: core::array::from_fn(|op| self.static_gas(op as u8)),
            changes: core::array::from_fn(|op| {
                let op = op as u8;
                if !self.is_defined(op) {
                    OpcodeChange::Undefined
                } else if let Some(base) = base {
                    if self.is_overridden(base, op) {
                        OpcodeChange::Instruction
                    } else if self.static_gas(op) != base.static_gas(op) {
                        OpcodeChange::Gas
                    } else {
                        OpcodeChange::Unchanged
                    }
                } else {
                    OpcodeChange::Instruction
                }
            }),
        }
    }

    /// Returns the dispatch-table revision for `opcode`.
    #[inline(always)]
    pub(crate) const fn revision(&self, opcode: u8) -> u8 {
        self.revisions[opcode as usize]
    }

    /// Sets the static gas cost for `opcode`.
    #[inline(always)]
    pub const fn set_static_gas(&mut self, opcode: u8, cost: u16) {
        assert!(opcode != op::INVALID, "INVALID opcode cannot be overridden");
        if self.static_gas_table.set(opcode, cost) {
            self.bump_revision(opcode);
        }
    }

    /// Returns instruction metadata for `opcode`.
    #[inline(always)]
    pub(crate) const fn instruction(&self, opcode: u8) -> InstructionInfo<T> {
        self.instruction_impls.get(opcode)
    }

    /// Returns whether `opcode` has no instruction implementation in this config.
    #[inline(always)]
    pub(crate) const fn is_unknown_opcode(&self, opcode: u8) -> bool {
        self.instruction_impls.is_unknown(opcode)
    }

    /// Sets the static gas cost and instruction for `opcode`.
    ///
    /// An `I: Instruction` is implemented using the [`#[instruction]`](evm2_macros::instruction)
    /// proc macro.
    #[inline(always)]
    pub const fn set_instruction<I: Instruction<T>>(&mut self, opcode: u8, gas: u16) {
        assert!(opcode != op::INVALID, "INVALID opcode cannot be overridden");
        self.set_static_gas(opcode, gas);
        self.instruction_sets[opcode as usize] += 1;
        if self.instruction_impls.set(opcode, I::execute, I::FLAGS) {
            self.bump_revision(opcode);
        }
    }

    #[inline(always)]
    const fn bump_revision(&mut self, opcode: u8) {
        self.revisions[opcode as usize] += 1;
    }
}

/// How an opcode of an [`OpcodeConfig`] differs from the base config it is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "wincode", derive(wincode::SchemaWrite, wincode::SchemaRead))]
pub enum OpcodeChange {
    /// The config has no instruction for the opcode.
    Undefined,
    /// The instruction and static gas are those of the base config.
    Unchanged,
    /// Only the static gas differs from the base config.
    Gas,
    /// The instruction differs from the base config.
    Instruction,
}

/// A type-independent view of an [`OpcodeConfig`], for consumers that cannot be generic over the
/// host types.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "wincode", derive(wincode::SchemaWrite, wincode::SchemaRead))]
pub struct OpcodeView {
    static_gas: [u16; 256],
    changes: [OpcodeChange; 256],
}

impl OpcodeView {
    /// Returns the static gas cost for `opcode`.
    #[inline]
    pub const fn static_gas(&self, opcode: u8) -> u16 {
        self.static_gas[opcode as usize]
    }

    /// Returns how `opcode` differs from the base config.
    #[inline]
    pub const fn change(&self, opcode: u8) -> OpcodeChange {
        self.changes[opcode as usize]
    }
}

struct StaticGasTable {
    table: [u16; 256],
    _align: [usize; 0],
}

impl StaticGasTable {
    /// Creates an empty gas table.
    #[inline]
    const fn empty() -> Self {
        Self { table: [0; 256], _align: [] }
    }

    /// Returns the gas cost for `opcode`.
    #[inline(always)]
    const fn get(&self, opcode: u8) -> u16 {
        self.table[opcode as usize]
    }

    /// Sets the gas cost for `opcode`.
    #[inline(always)]
    const fn set(&mut self, opcode: u8, cost: u16) -> bool {
        let index = opcode as usize;
        if self.table[index] != cost {
            self.table[index] = cost;
            return true;
        }
        false
    }
}

struct InstructionImplTable<T: EvmTypesHost> {
    instrs: [Option<InstructionImplFn<T>>; 256],
    flags: [InstructionFlags; 256],
}

impl<T: EvmTypesHost> InstructionImplTable<T> {
    /// Creates an empty instruction implementation table.
    #[inline]
    const fn empty() -> Self {
        Self { instrs: [None; 256], flags: [InstructionFlags::empty(); 256] }
    }

    /// Returns the instruction implementation for `opcode`.
    #[inline(always)]
    const fn get(&self, opcode: u8) -> InstructionInfo<T> {
        match self.instrs[opcode as usize] {
            Some(instr) => InstructionInfo { instr, flags: self.flags[opcode as usize] },
            None => InstructionInfo {
                instr: instr::invalid::<T>::execute,
                flags: InstructionFlags::empty(),
            },
        }
    }

    /// Returns whether `opcode` has no instruction implementation.
    #[inline(always)]
    const fn is_unknown(&self, opcode: u8) -> bool {
        self.instrs[opcode as usize].is_none()
    }

    /// Sets the instruction implementation for `opcode`.
    #[inline(always)]
    const fn set(
        &mut self,
        opcode: u8,
        instr: InstructionImplFn<T>,
        flags: InstructionFlags,
    ) -> bool {
        let index = opcode as usize;
        self.instrs[index] = Some(instr);
        self.flags[index] = flags;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::config::BaseEvmTypes;

    #[test]
    #[should_panic(expected = "INVALID opcode cannot be overridden")]
    fn invalid_static_gas_cannot_be_overridden() {
        let mut tables = OpcodeConfig::<BaseEvmTypes>::empty(SpecId::default());
        tables.set_static_gas(op::INVALID, 1);
    }

    #[test]
    #[should_panic(expected = "INVALID opcode cannot be overridden")]
    fn invalid_instruction_cannot_be_overridden() {
        let mut tables = OpcodeConfig::<BaseEvmTypes>::empty(SpecId::default());
        tables.set_instruction::<instr::stop<BaseEvmTypes>>(op::INVALID, 0);
    }

    #[test]
    fn view_compares_with_the_base_config() {
        let mut base = OpcodeConfig::<BaseEvmTypes>::empty(SpecId::default());
        base.set_instruction::<instr::add<BaseEvmTypes>>(op::ADD, 3);
        base.set_instruction::<instr::mul<BaseEvmTypes>>(op::MUL, 5);
        let mut config = OpcodeConfig::<BaseEvmTypes>::empty(SpecId::default());
        config.set_instruction::<instr::add<BaseEvmTypes>>(op::ADD, 3);
        config.set_instruction::<instr::mul<BaseEvmTypes>>(op::MUL, 7);
        config.set_instruction::<instr::stop<BaseEvmTypes>>(op::STOP, 0);

        let view = config.view(Some(&base));
        assert_eq!(view.change(op::ADD), OpcodeChange::Unchanged);
        assert_eq!(view.change(op::MUL), OpcodeChange::Gas);
        assert_eq!(view.static_gas(op::MUL), 7);
        assert_eq!(view.change(op::STOP), OpcodeChange::Instruction);
        assert_eq!(view.change(op::SUB), OpcodeChange::Undefined);

        let view = config.view(None);
        assert_eq!(view.change(op::ADD), OpcodeChange::Instruction);
        assert_eq!(view.change(op::SUB), OpcodeChange::Undefined);
    }
}
