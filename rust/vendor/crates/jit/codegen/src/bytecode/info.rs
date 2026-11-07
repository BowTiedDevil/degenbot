use evm2::{
    BaseEvmConfigSelector, BaseEvmTypes, EvmConfig, EvmConfigSelector, OpcodeChange, OpcodeView,
    SpecId,
    interpreter::{op, opcode::OpCode},
};

/// Opcode information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpcodeInfo(u32);

impl OpcodeInfo {
    /// The unknown flag.
    pub const UNKNOWN: u32 = 1 << 31;
    /// The disabled flag.
    pub const DISABLED: u32 = 1 << 29;
    /// The host-instruction flag.
    pub const HOST: u32 = 1 << 28;
    /// The unsupported-host-opcode flag.
    pub const UNSUPPORTED_HOST: u32 = 1 << 27;
    /// The mask for the gas cost (16 bits, max 65535).
    pub const MASK: u32 = 0xFFFF;

    /// Creates a new gas info with the given gas cost.
    #[inline]
    pub const fn new(gas: u16) -> Self {
        Self(gas as u32)
    }

    /// Returns `true` if the opcode is unknown.
    #[inline]
    pub const fn is_unknown(self) -> bool {
        self.0 == Self::UNKNOWN
    }

    /// Returns `true` if the opcode is known, but disabled in the current EVM
    /// version.
    #[inline]
    pub const fn is_disabled(self) -> bool {
        self.0 & Self::DISABLED != 0
    }

    /// Returns the base gas cost of the opcode.
    ///
    /// This may not be the final/full gas cost of the opcode as it may also have a dynamic cost.
    #[inline]
    pub const fn base_gas(self) -> u16 {
        (self.0 & Self::MASK) as u16
    }

    /// Returns `true` if the host's own instruction implementation executes the opcode.
    #[inline]
    pub const fn is_host(self) -> bool {
        self.0 & Self::HOST != 0
    }

    /// Returns `true` if the opcode is implemented by the host in a way compiled code cannot
    /// reproduce.
    #[inline]
    pub const fn is_unsupported_host(self) -> bool {
        self.0 & Self::UNSUPPORTED_HOST != 0
    }

    /// Sets the host-instruction flag.
    #[inline]
    pub const fn set_host(&mut self) {
        self.0 |= Self::HOST;
    }

    /// Sets the unsupported-host-opcode flag.
    #[inline]
    pub const fn set_unsupported_host(&mut self) {
        self.0 |= Self::UNSUPPORTED_HOST;
    }

    /// Sets the unknown flag.
    #[inline]
    pub const fn set_unknown(&mut self) {
        self.0 = Self::UNKNOWN;
    }

    /// Sets the disabled flag.
    #[inline]
    pub const fn set_disabled(&mut self) {
        self.0 |= Self::DISABLED;
    }

    /// Sets the gas cost.
    #[inline]
    pub const fn set_gas(&mut self, gas: u16) {
        self.0 = (self.0 & !Self::MASK) | (gas as u32);
    }
}

/// Returns the static info map for the given `SpecId`.
pub const fn op_info_map(spec_id: SpecId) -> &'static [OpcodeInfo; 256] {
    macro_rules! make_maps {
        ([$($extra:tt)*] $($spec:ident $name:ident,)*) => {
            [$(make_map::<{ SpecId::$spec as u32 }>(),)*]
        };
    }

    static MAPS: [[OpcodeInfo; 256]; SpecId::COUNT] = evm2::for_each_spec!([] make_maps);
    &MAPS[spec_id as usize]
}

/// Opcodes whose builtin pays the whole gas cost, including the static part, at runtime. Core
/// gives them no static gas, so a host cannot change it without running its own instruction.
const FULLY_DYNAMIC: &[u8] = &[op::SSTORE, op::CREATE, op::CREATE2];

const fn contains(haystack: &[u8], needle: u8) -> bool {
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i] == needle {
            return true;
        }
        i += 1;
    }
    false
}

const fn make_map<const BASE_SPEC_ID: u32>() -> [OpcodeInfo; 256] {
    let config = <<BaseEvmConfigSelector as EvmConfigSelector<BaseEvmTypes>>::Config<
        BASE_SPEC_ID,
        { u32::MAX },
    > as EvmConfig<BaseEvmTypes>>::OPCODE_CONFIG;
    let mut map = [OpcodeInfo(OpcodeInfo::UNKNOWN); 256];

    let mut i = 0u16;
    while i < 256 {
        let op = i as u8;

        // Skip opcodes not defined in the opcode table.
        if OpCode::new(op).is_none() {
            i += 1;
            continue;
        }

        // Mark opcodes the version leaves out as disabled rather than unknown, so they return
        // `NotActivated` instead of `OpcodeNotFound` at runtime. `INVALID` is never in a config
        // and keeps its own stop reason.
        let mut info = OpcodeInfo::new(0);
        if !config.is_defined(op) && op != op::INVALID {
            info.set_disabled();
        } else {
            info.set_gas(config.static_gas(op));
        }

        map[op as usize] = info;
        i += 1;
    }

    map
}

/// Opcodes a host may override or add: compiled code runs the host's own instruction for them
/// through a builtin. Every other host-implemented opcode keeps its contract on the interpreter.
const HOST_INSTRUCTIONS: &[u8] = &[
    op::BALANCE,
    op::EXTCODESIZE,
    op::EXTCODECOPY,
    op::EXTCODEHASH,
    op::BLOCKHASH,
    op::SELFBALANCE,
    op::SLOAD,
    op::SSTORE,
    op::TLOAD,
    op::TSTORE,
    op::LOG0,
    op::LOG1,
    op::LOG2,
    op::LOG3,
    op::LOG4,
];

/// Returns the opcode info map for `spec_id` under the host's opcode `view`.
pub(crate) fn opcode_infos(view: &OpcodeView, spec_id: SpecId) -> [OpcodeInfo; 256] {
    let base = op_info_map(spec_id);
    let mut map = *base;
    for op in 0..=u8::MAX {
        let i = op as usize;
        let change = view.change(op);
        let base_known = !base[i].is_unknown() && !base[i].is_disabled();
        if change == OpcodeChange::Undefined {
            // The interpreter halts on an opcode that the table leaves out with
            // `NotActivated`. `INVALID` is never in a table and keeps its own stop reason.
            if base_known && op != op::INVALID {
                let mut info = OpcodeInfo::new(0);
                info.set_disabled();
                map[i] = info;
            }
            continue;
        }
        // The host runs its own instruction when it overrides the base one, or when it only
        // changes the static gas of an opcode whose base builtin pays its whole cost.
        let host = change == OpcodeChange::Instruction
            || (change == OpcodeChange::Gas && contains(FULLY_DYNAMIC, op));
        if !base_known || (host && !contains(HOST_INSTRUCTIONS, op)) {
            map[i] = OpcodeInfo(OpcodeInfo::UNSUPPORTED_HOST);
            continue;
        }
        let mut info = base[i];
        info.set_gas(view.static_gas(op));
        if host {
            info.set_host();
        }
        map[i] = info;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::OpcodeConfig;

    #[test]
    fn matches_core_opcode_config() {
        for spec in 0..SpecId::COUNT as u32 {
            let spec_id = SpecId::try_from_u32(spec).unwrap();
            let config = OpcodeConfig::<BaseEvmTypes>::base_for_spec(spec_id);
            let map = op_info_map(spec_id);
            for op in 0..=u8::MAX {
                let info = map[op as usize];
                if OpCode::new(op).is_none() {
                    assert!(info.is_unknown(), "{spec_id:?} {op:#04x}");
                } else if op == op::INVALID || config.is_defined(op) {
                    assert!(!info.is_unknown() && !info.is_disabled(), "{spec_id:?} {op:#04x}");
                    assert_eq!(info.base_gas(), config.static_gas(op), "{spec_id:?} {op:#04x}");
                } else {
                    assert!(info.is_disabled(), "{spec_id:?} {op:#04x}");
                }
            }
        }
    }

    #[test]
    fn test_clz_flags() {
        let london = op_info_map(SpecId::LONDON);
        let cancun = op_info_map(SpecId::CANCUN);
        let osaka = op_info_map(SpecId::OSAKA);

        let clz_london = london[op::CLZ as usize];
        let clz_cancun = cancun[op::CLZ as usize];
        let clz_osaka = osaka[op::CLZ as usize];

        eprintln!(
            "CLZ on LONDON: is_unknown={}, is_disabled={}, raw={:#06x}",
            clz_london.is_unknown(),
            clz_london.is_disabled(),
            clz_london.0
        );
        eprintln!(
            "CLZ on CANCUN: is_unknown={}, is_disabled={}, raw={:#06x}",
            clz_cancun.is_unknown(),
            clz_cancun.is_disabled(),
            clz_cancun.0
        );
        eprintln!(
            "CLZ on OSAKA: is_unknown={}, is_disabled={}, raw={:#06x}",
            clz_osaka.is_unknown(),
            clz_osaka.is_disabled(),
            clz_osaka.0
        );

        assert!(!clz_london.is_unknown(), "CLZ should not be unknown on pre-OSAKA");
        assert!(clz_london.is_disabled(), "CLZ should be disabled on LONDON");
        assert!(!clz_cancun.is_unknown(), "CLZ should not be unknown on pre-OSAKA");
        assert!(clz_cancun.is_disabled(), "CLZ should be disabled on CANCUN");
        assert!(!clz_osaka.is_unknown(), "CLZ should not be unknown on OSAKA");
        assert!(!clz_osaka.is_disabled(), "CLZ should not be disabled on OSAKA");
    }

    #[test]
    fn test_gas_values() {
        let cancun = op_info_map(SpecId::CANCUN);

        // Basic arithmetic.
        assert_eq!(cancun[op::ADD as usize].base_gas(), 3);
        assert_eq!(cancun[op::MUL as usize].base_gas(), 5);
        assert_eq!(cancun[op::ADDMOD as usize].base_gas(), 8);

        // EXP: static gas only; the dynamic part is paid in builtins.
        assert_eq!(cancun[op::EXP as usize].base_gas(), 10);

        // PUSH/DUP/SWAP gas.
        assert_eq!(cancun[op::PUSH1 as usize].base_gas(), 3);
        assert_eq!(cancun[op::DUP1 as usize].base_gas(), 3);
        assert_eq!(cancun[op::SWAP1 as usize].base_gas(), 3);
        assert_eq!(cancun[op::PUSH0 as usize].base_gas(), 2);

        // LOG: base gas only (topic + data cost charged dynamically in builtin).
        assert_eq!(cancun[op::LOG0 as usize].base_gas(), 375);
        assert_eq!(cancun[op::LOG1 as usize].base_gas(), 375);
        assert_eq!(cancun[op::LOG2 as usize].base_gas(), 375);
        assert_eq!(cancun[op::LOG3 as usize].base_gas(), 375);
        assert_eq!(cancun[op::LOG4 as usize].base_gas(), 375);

        // Memory ops: static gas only.
        assert_eq!(cancun[op::MLOAD as usize].base_gas(), 3);
        assert_eq!(cancun[op::KECCAK256 as usize].base_gas(), 30);

        // Transient storage (Cancun).
        assert_eq!(cancun[op::TLOAD as usize].base_gas(), 100);
        assert!(!cancun[op::TLOAD as usize].is_disabled());

        // Unknown opcode.
        assert!(cancun[0x0C].is_unknown());

        // AMSTERDAM-gated opcodes should be disabled on CANCUN.
        assert!(cancun[op::DUPN as usize].is_disabled());
        assert!(!cancun[op::DUPN as usize].is_unknown());
        assert!(cancun[op::SWAPN as usize].is_disabled());
        assert!(cancun[op::EXCHANGE as usize].is_disabled());
        assert!(cancun[op::SLOTNUM as usize].is_disabled());

        // AMSTERDAM-gated opcodes should be enabled on AMSTERDAM.
        let amsterdam = op_info_map(SpecId::AMSTERDAM);
        assert!(!amsterdam[op::DUPN as usize].is_disabled());
        assert!(!amsterdam[op::SWAPN as usize].is_disabled());
        assert!(!amsterdam[op::EXCHANGE as usize].is_disabled());
        assert!(!amsterdam[op::SLOTNUM as usize].is_disabled());

        // Spec-gated: PUSH0 disabled before Shanghai.
        let pre_shanghai = op_info_map(SpecId::MERGE);
        assert!(pre_shanghai[op::PUSH0 as usize].is_disabled());
        assert!(!cancun[op::PUSH0 as usize].is_disabled());

        // Static gas of opcodes with a dynamic component varies by spec.
        let frontier = op_info_map(SpecId::FRONTIER);
        assert_eq!(frontier[op::SLOAD as usize].base_gas(), 50);
        assert_eq!(frontier[op::SELFDESTRUCT as usize].base_gas(), 0);
        assert_eq!(frontier[op::CALL as usize].base_gas(), 40);
        assert_eq!(frontier[op::BALANCE as usize].base_gas(), 20);
        assert_eq!(frontier[op::EXTCODESIZE as usize].base_gas(), 20);
        let tangerine = op_info_map(SpecId::TANGERINE);
        assert_eq!(tangerine[op::SLOAD as usize].base_gas(), 200);
        assert_eq!(tangerine[op::SELFDESTRUCT as usize].base_gas(), 5000);
        assert_eq!(tangerine[op::CALL as usize].base_gas(), 700);
        assert_eq!(tangerine[op::BALANCE as usize].base_gas(), 400);
        assert_eq!(tangerine[op::EXTCODESIZE as usize].base_gas(), 700);
        let berlin = op_info_map(SpecId::BERLIN);
        assert_eq!(berlin[op::SLOAD as usize].base_gas(), 100);
        assert_eq!(berlin[op::CALL as usize].base_gas(), 100);
        assert_eq!(berlin[op::BALANCE as usize].base_gas(), 100);
        assert_eq!(berlin[op::EXTCODESIZE as usize].base_gas(), 100);
    }
}
