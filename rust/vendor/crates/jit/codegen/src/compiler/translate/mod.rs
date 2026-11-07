//! EVM to IR translation.

use crate::{
    Backend, Builder, Bytecode, EvmContext, Inst, InstData, InstFlags, IntCC, Result, StackSection,
    decode_pair, decode_single,
};
use alloy_primitives::U256;
use evm2::{
    BaseEvmTypes, EvmTypesHost,
    env::{BlockEnv, TxEnv},
    interpreter::{InstrStop, Interpreter, Message, op},
};
use evm2_jit_backend::{BackendTypes, Pointer, TypeMethods};
use evm2_jit_builtins::{Builtin, Builtins, CallKind, CreateKind};
use oxc_index::IndexVec;
use std::{fmt::Write, mem, ops::Range};

mod peephole;

mod vstack;
use vstack::{VSlot, VStack};

const STACK_CAP: usize = 1024;
/// The size of a stack word in bytes. Stack lengths are kept in bytes so that stack addresses
/// need no scaling.
const WORD_SIZE: i64 = 32;
/// The most gas that sections entered only by fall-through may charge before a signed check.
const GAS_CHECK_THRESHOLD: u64 = 1000;

#[derive(Clone, Copy, Debug)]
pub(super) struct FcxConfig {
    pub(super) comments: bool,
    pub(super) debug_assertions: bool,
    pub(super) frame_pointers: bool,

    pub(super) debug: bool,
    pub(super) inspect_stack: bool,
    pub(super) local_stack: bool,
    pub(super) stack_bound_checks: bool,
    pub(super) gas_metering: bool,
    pub(super) single_error: bool,
    pub(super) layout: HostLayout,
}

impl Default for FcxConfig {
    fn default() -> Self {
        Self {
            debug_assertions: cfg!(debug_assertions),
            comments: false,
            frame_pointers: cfg!(debug_assertions) || cfg!(force_frame_pointers),
            debug: false,
            inspect_stack: false,
            local_stack: false,
            stack_bound_checks: true,
            gas_metering: true,
            single_error: true,
            layout: HostLayout::for_types::<BaseEvmTypes>(),
        }
    }
}

/// Offsets read directly by generated code for a concrete interpreter host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostLayout {
    interpreter_message: usize,
    destination: usize,
    caller: usize,
    value: usize,
    tx_origin: usize,
    tx_gas_price: usize,
    tx_chain_id: usize,
    block_beneficiary: usize,
    block_timestamp: usize,
    block_number: usize,
    block_gas_limit: usize,
    block_basefee: usize,
    block_blob_basefee: usize,
    block_slot_num: usize,
}

impl HostLayout {
    /// Number of words in the serialized layout.
    pub const WORDS: usize = 14;

    /// Returns the offsets as words, in field order.
    pub const fn to_words(&self) -> [usize; Self::WORDS] {
        [
            self.interpreter_message,
            self.destination,
            self.caller,
            self.value,
            self.tx_origin,
            self.tx_gas_price,
            self.tx_chain_id,
            self.block_beneficiary,
            self.block_timestamp,
            self.block_number,
            self.block_gas_limit,
            self.block_basefee,
            self.block_blob_basefee,
            self.block_slot_num,
        ]
    }

    /// Rebuilds a layout from [`to_words`](Self::to_words).
    pub const fn from_words(words: [usize; Self::WORDS]) -> Self {
        let [
            interpreter_message,
            destination,
            caller,
            value,
            tx_origin,
            tx_gas_price,
            tx_chain_id,
            block_beneficiary,
            block_timestamp,
            block_number,
            block_gas_limit,
            block_basefee,
            block_blob_basefee,
            block_slot_num,
        ] = words;
        Self {
            interpreter_message,
            destination,
            caller,
            value,
            tx_origin,
            tx_gas_price,
            tx_chain_id,
            block_beneficiary,
            block_timestamp,
            block_number,
            block_gas_limit,
            block_basefee,
            block_blob_basefee,
            block_slot_num,
        }
    }

    /// Computes offsets without assuming that custom message extensions preserve Rust layout.
    pub const fn for_types<T: EvmTypesHost>() -> Self {
        Self {
            interpreter_message: Interpreter::<T>::message_field_offset_for_jit(),
            destination: mem::offset_of!(Message<T>, destination),
            caller: mem::offset_of!(Message<T>, caller),
            value: mem::offset_of!(Message<T>, value),
            tx_origin: mem::offset_of!(TxEnv<T>, origin),
            tx_gas_price: mem::offset_of!(TxEnv<T>, gas_price),
            tx_chain_id: mem::offset_of!(TxEnv<T>, chain_id),
            block_beneficiary: mem::offset_of!(BlockEnv<T>, beneficiary),
            block_timestamp: mem::offset_of!(BlockEnv<T>, timestamp),
            block_number: mem::offset_of!(BlockEnv<T>, number),
            block_gas_limit: mem::offset_of!(BlockEnv<T>, gas_limit),
            block_basefee: mem::offset_of!(BlockEnv<T>, basefee),
            block_blob_basefee: mem::offset_of!(BlockEnv<T>, blob_basefee),
            block_slot_num: mem::offset_of!(BlockEnv<T>, slot_num),
        }
    }
}

/// A list of incoming values for a block. Represents a `phi` node.
type Incoming<B> = Vec<(<B as BackendTypes>::Value, <B as BackendTypes>::BasicBlock)>;

pub(super) struct FunctionCx<'a, B: Backend> {
    // Configuration.
    config: FcxConfig,

    /// The backend's function builder.
    bcx: B::Builder<'a>,

    // Common types.
    isize_type: B::Type,
    word_type: B::Type,
    address_type: B::Type,
    i8_type: B::Type,

    // Locals.
    /// The stack length in bytes. Either passed in the arguments as a pointer or allocated
    /// locally.
    stack_bytes: Pointer<B::Builder<'a>>,
    /// The stack value. Constant throughout the function, either passed in the arguments as a
    /// pointer or allocated locally.
    stack: Pointer<B::Builder<'a>>,
    /// The stack argument pointer. Only used when `local_stack` is enabled and the stack needs
    /// to be copied in/out at entry/exit boundaries.
    sp_arg: Option<B::Value>,
    /// The EVM context. Opaque pointer, only passed to builtins.
    ecx: B::Value,
    /// Stack length in bytes before the current instruction.
    bytes_before: B::Value,
    /// Stack length offset for the current instruction, used for push/pop.
    len_offset: i8,

    /// Section-local virtual stack that caches values as SSA instead of
    /// immediately storing/loading from the stack alloca.
    vstack: VStack<B::Value>,
    /// Stack length in bytes at the start of the current stack section, loaded once from the
    /// alloca. All intra-section `bytes_before` values are derived from this and
    /// `section_len_offset`.
    section_start_bytes: B::Value,
    /// Stack pointer at the start of the current stack section
    /// (`&stack[section_start_bytes / 32]`). All intra-section stack pointer GEPs are derived
    /// from this base to preserve pointer provenance, which lets LLVM prove aliasing and fold
    /// redundant operations.
    section_start_sp: B::Value,
    /// Cumulative stack diff from the section start to the current instruction (compile-time).
    /// Updated after the opcode handler so that push/pop/sp helpers see the pre-diff value.
    section_len_offset: i32,
    /// The cumulative offset, in words, that `stack_bytes.addr` currently holds relative to
    /// `section_start_bytes`. At section start this is 0
    /// (`stack_bytes.addr == section_start_bytes`). After each store it becomes
    /// `section_len_offset + diff`. Stores are skipped when the new offset matches this value.
    stored_len_offset: i32,
    cached_mem_base: Option<B::Value>,
    /// Gas subtracted without a check since the last check, in translation order.
    unchecked_gas: u64,
    /// Memory ranges already ensured in the current stack section, as `(base, lo, hi)`: memory
    /// covers every access at `base + c` with `lo <= c` and `c + len <= hi`. Memory never
    /// shrinks within a frame, so these hold until the next section head.
    ensured_memory: Vec<(B::Value, u64, u64)>,
    /// Values in the current stack section known to equal another value plus a small constant,
    /// as `(value, base, addend)`: `ADD` results and reloads of materialized stack slots.
    value_offsets: Vec<(B::Value, B::Value, u64)>,
    /// Stores from [`relieve_vstack_pressure`](Self::relieve_vstack_pressure) that nothing has
    /// read yet, as `(offset, store, joins)`. Popping such a slot makes its store dead.
    relief_stores: Vec<(i32, B::Inst, u32)>,
    /// Number of join blocks built so far. A popped relief store's value is used instead of a
    /// reload only before the next join, so its live range stays short.
    joins: u32,

    /// The bytecode being translated.
    bytecode: &'a Bytecode<'a>,
    /// Instruction index to 1-based line number in bytecode.txt (for debug info).
    inst_lines: IndexVec<Inst, u32>,
    /// All entry blocks for each instruction.
    inst_entries: IndexVec<Inst, B::BasicBlock>,
    /// The current instruction being translated.
    current_inst: Option<Inst>,

    // Basic blocks are `None` when outside of a main function.
    /// `dynamic_jump_table` incoming values.
    incoming_dynamic_jumps: Incoming<B>,
    /// The dynamic jump table block where all dynamic jumps branch to.
    dynamic_jump_table: B::BasicBlock,

    /// `failure_block` incoming values.
    incoming_failures: Incoming<B>,
    /// The block that all failures branch to.
    failure_block: Option<B::BasicBlock>,
    /// `return_block` incoming values.
    incoming_returns: Incoming<B>,
    /// The return block that all return instructions branch to.
    return_block: Option<B::BasicBlock>,

    /// Builtins.
    builtins: &'a mut Builtins<B>,
}

impl<'a, B: Backend> FunctionCx<'a, B> {
    /// Translates an EVM bytecode into a native function.
    ///
    /// Example pseudo-code:
    ///
    /// ```ignore (pseudo-code)
    /// fn evm_bytecode(args: ...) {
    ///     setup_locals();
    ///
    ///     #[cfg(debug_assertions)]
    ///     if args.<ptr>.is_null() { panic!("...") };
    ///
    ///     load_arguments();
    ///
    ///     op.inst0: { /* ... */ };
    ///     op.inst1: { /* ... */ };
    ///     // ...
    ///     // There will always be at least one diverging instruction.
    ///     op.stop: {
    ///         goto return(InstrStop::Stop);
    ///     };
    ///
    ///     // All paths lead to here.
    ///     return(ir: InstrStop): {
    ///         #[cfg(inspect_stack)]
    ///         *args.stack_len = stack_bytes / 32;
    ///         return ir;
    ///     }
    /// }
    /// ```
    #[allow(rustdoc::invalid_rust_codeblocks)] // Syntax highlighting.
    pub(super) fn translate(
        mut bcx: B::Builder<'a>,
        config: FcxConfig,
        builtins: &'a mut Builtins<B>,
        bytecode: &'a Bytecode<'a>,
    ) -> Result<()> {
        let entry_block = bcx.current_block().unwrap();

        // Clear debug location for prologue code.
        if config.debug {
            bcx.clear_debug_location();
        }

        // Get common types.
        let isize_type = bcx.type_ptr_sized_int();
        let i8_type = bcx.type_int(8);
        let word_type = bcx.type_int(256);
        let address_type = bcx.type_int(160);

        // Set up entry block.
        let ecx = bcx.fn_param(0);

        let sp_arg = bcx.fn_param(1);
        // Use a local alloca for the stack to allow the backend to eliminate dead stores.
        let local_stack = config.local_stack && !config.inspect_stack;
        let stack = if local_stack {
            let stack_type = bcx.type_array(word_type, STACK_CAP as u32);
            bcx.new_stack_slot(stack_type, "stack.addr")
        } else {
            Pointer::new_address(word_type, sp_arg)
        };

        let stack_len_arg = bcx.fn_param(2);
        // This is initialized later in `post_entry_block`.
        let stack_bytes = bcx.new_stack_slot(isize_type, "stack_bytes.addr");

        // Create all instruction entry blocks. Dead-code instructions map to
        // `unreachable_block`; dedup redirects are resolved at control-flow callsites.
        let unreachable_block = bcx.create_block("unreachable");
        let inst_entries: IndexVec<Inst, _> = bytecode
            .iter_all_insts()
            .map(|(i, data)| {
                if data.is_dead_code() {
                    unreachable_block
                } else {
                    let name = if config.debug { &bytecode.op_block_name(Some(i), "") } else { "" };
                    bcx.create_block(name)
                }
            })
            .collect();
        assert!(!inst_entries.is_empty(), "translating empty bytecode");

        let dynamic_jump_table = bcx.create_block("dynamic_jump_table");
        let failure_block = bcx.create_block("failure");
        let return_block = bcx.create_block("return");

        let section_start_sp = stack.addr(&mut bcx);
        let zero = bcx.iconst(isize_type, 0);
        let mut fx = FunctionCx {
            config,

            isize_type,
            word_type,
            address_type,
            i8_type,
            stack_bytes,
            stack,
            sp_arg: local_stack.then_some(sp_arg),
            ecx,
            bytes_before: zero,
            len_offset: 0,
            section_start_bytes: zero,
            section_start_sp,
            section_len_offset: 0,
            stored_len_offset: 0,
            cached_mem_base: None,
            unchecked_gas: 0,
            ensured_memory: Vec::new(),
            value_offsets: Vec::new(),
            relief_stores: Vec::new(),
            joins: 0,
            bcx,

            bytecode,
            inst_lines: if config.debug { bytecode.take_inst_lines() } else { IndexVec::new() },
            inst_entries,
            current_inst: None,

            incoming_dynamic_jumps: Vec::new(),
            dynamic_jump_table,

            incoming_failures: Vec::new(),
            failure_block: Some(failure_block),
            incoming_returns: Vec::new(),
            return_block: Some(return_block),

            builtins,

            vstack: VStack::default(),
        };

        // Add debug assertions for the parameters.
        if config.debug_assertions {
            // Assert that the runtime spec_id matches the compilation spec_id.
            let spec_type = fx.bcx.type_int(32);
            let compiled_spec = fx.bcx.iconst(spec_type, u32::from(bytecode.spec_id).into());
            let _ = fx.call_builtin(Builtin::AssertSpecId, &[ecx, compiled_spec]);
        }

        // The bytecode is guaranteed to have at least one instruction.
        let first_inst_block = fx.inst_entries[Inst::new(0)];
        let post_entry_block = fx.bcx.create_block_after(entry_block, "entry.post");
        fx.bcx.br(post_entry_block);

        // Translate individual instructions into their respective blocks.
        for (inst, _) in bytecode.iter_insts() {
            fx.translate_inst(inst)?;
        }

        // Clear debug location for all synthetic / epilogue blocks.
        if config.debug {
            fx.bcx.clear_debug_location();
        }

        // Finalize the dynamic jump table.
        fx.bcx.switch_to_block(unreachable_block);
        fx.bcx.unreachable();
        if bytecode.has_dynamic_jumps() {
            fx.build_dynamic_jump_table(unreachable_block);
        } else {
            // No dynamic jumps.
            debug_assert!(fx.incoming_dynamic_jumps.is_empty());
            fx.bcx.switch_to_block(fx.dynamic_jump_table);
            fx.bcx.unreachable();
        }

        // Initialize stack length in the entry path.
        fx.bcx.switch_to_block(post_entry_block);
        if config.inspect_stack {
            let stack_len = fx.bcx.load(fx.isize_type, stack_len_arg, "stack_len");
            let stack_bytes = fx.bcx.imul_imm(stack_len, WORD_SIZE);
            fx.stack_bytes.store(&mut fx.bcx, stack_bytes);
            fx.copy_stack_from_arg(stack_bytes);
        } else {
            fx.stack_bytes.store_imm(&mut fx.bcx, 0);
        }
        fx.bcx.br(first_inst_block);

        // Finalize the failure block.
        fx.bcx.switch_to_block(fx.failure_block.unwrap());
        if !fx.incoming_failures.is_empty() {
            // Semantically, the EVM has only one halt; the distinct failure
            // `InstrStop` codes exist purely for debugging and error messages.
            // `single_error` collapses every failure path to a single placeholder
            // (OOG), letting LLVM DCE the per-site materialization and the phi.
            let out_of_gas = fx.bcx.iconst(fx.i8_type, InstrStop::OutOfGas as i64);
            let failure_value = if config.single_error {
                out_of_gas
            } else {
                let failure_value = fx.bcx.phi(fx.i8_type, &fx.incoming_failures);
                // Gas subtracted without a check may have run out before this failure.
                if config.gas_metering {
                    let remaining = fx.load_gas_remaining();
                    let is_negative = fx.bcx.icmp_imm(IntCC::SignedLessThan, remaining, 0);
                    fx.bcx.select(is_negative, out_of_gas, failure_value)
                } else {
                    failure_value
                }
            };
            // Do not sync the stack here: this is a shared synthetic merge block for
            // failures that may occur before their instruction body has materialized
            // the current virtual stack. Syncing here can use values that do not
            // dominate all failure predecessors.
            fx.build_return_inner(failure_value);
        } else {
            fx.bcx.unreachable();
        }

        // Finalize the return block.
        fx.bcx.switch_to_block(fx.return_block.unwrap());
        if !fx.incoming_returns.is_empty() {
            let return_value = fx.bcx.phi(fx.i8_type, &fx.incoming_returns);
            if config.inspect_stack {
                fx.copy_stack_to_arg();
                fx.save_stack_len();
            }
            fx.bcx.ret(&[return_value]);
        } else {
            fx.bcx.unreachable();
        }

        fx.bcx.seal_all_blocks();

        Ok(())
    }

    /// Returns the instruction that is physically next in bytecode order.
    fn lexical_next_inst(&self, inst: Inst) -> Option<Inst> {
        let next = inst + 1;
        self.inst_entries.get(next)?;
        Some(next)
    }

    /// Returns the IR block for an instruction after applying dedup redirects.
    fn effective_entry(&self, inst: Inst) -> B::BasicBlock {
        self.inst_entries[self.bytecode.effective_inst(inst)]
    }

    /// Returns the runtime fallthrough target after applying dedup redirects.
    fn effective_next_inst(&self, inst: Inst) -> Option<Inst> {
        self.lexical_next_inst(inst).map(|next| self.bytecode.effective_inst(next))
    }

    /// Returns the IR block for the runtime fallthrough target.
    fn effective_next_entry(&self, inst: Inst) -> Option<B::BasicBlock> {
        self.effective_next_inst(inst).map(|next| self.effective_entry(next))
    }

    #[instrument(level = "debug", skip_all, fields(inst = %self.bytecode.inst(inst).to_op()))]
    fn translate_inst(&mut self, inst: Inst) -> Result<()> {
        self.current_inst = Some(inst);
        let data = self.bytecode.inst(inst);
        let opcode = data.opcode;
        let entry_block = self.inst_entries[inst];
        self.bcx.switch_to_block(entry_block);

        if self.config.debug {
            self.bcx.set_debug_location(self.inst_lines[inst], 1);
        }

        let branch_to_next_opcode = |this: &mut Self| {
            debug_assert!(
                !this.bytecode.is_instr_diverging(inst),
                "attempted to branch to next instruction in a diverging instruction: {data:?}",
            );
            if let Some(next) = this.effective_next_entry(inst) {
                this.bcx.br(next);
            }
        };

        /// Makes sure to run cleanup code and return.
        /// Use `no_branch` to skip the branch to the next opcode.
        /// Use `build` to build the return instruction and skip the branch.
        macro_rules! goto_return {
            (no_branch $($comment:expr)?) => {
                $(
                    if self.config.comments {
                        self.add_comment($comment);
                    }
                )?
                return Ok(());
            };
            (build $ret:expr) => {{
                self.build_return_imm($ret);
                goto_return!(no_branch);
            }};
            (fail $ret:expr) => {{
                self.build_fail_imm($ret);
                goto_return!(no_branch);
            }};
            ($($comment:expr)?) => {
                // Flush virtual stack before leaving the section.
                if let Some(next_inst) = self.effective_next_inst(inst) {
                    let next = self.bytecode.inst(next_inst);
                    if !next.is_dead_code() && next.is_stack_section_head() {
                        self.materialize_live_stack();
                    } else {
                        self.relieve_vstack_pressure();
                    }
                }
                branch_to_next_opcode(self);
                goto_return!(no_branch $($comment)?);
            };
        }

        // Assert that we already skipped the block.
        debug_assert!(!data.is_dead_code());

        // Disabled instructions don't pay gas.
        if data.flags.contains(InstFlags::DISABLED) {
            goto_return!(fail InstrStop::NotActivated);
        }
        if data.flags.contains(InstFlags::UNKNOWN) {
            goto_return!(fail InstrStop::OpcodeNotFound);
        }

        // Pay static gas for the current section.
        debug_assert!(data.is_stack_section_head() || data.gas_section.is_empty());
        if data.is_stack_section_head() {
            self.charge_section_gas(inst);
        }

        // Compute bytes_before for this instruction.
        // At section heads: load from the alloca once and reset the section offset.
        // Within a section: derive from section_start_bytes + compile-time offset.
        let (inp, out) = data.stack_io();
        let diff = out as i32 - inp as i32;
        self.len_offset = 0;
        if data.is_stack_section_head() {
            let section = data.stack_section;
            if data.continues_stack_section() {
                // Only the preceding `JUMPI` falls through here, and it kept the section state.
                let top = self.section_len_offset;
                self.vstack.extend(top - section.inputs as i32, top + section.max_growth as i32);
            } else {
                self.section_start_bytes = self.stack_bytes.load(&mut self.bcx, "stack_bytes");
                self.section_start_sp = self.sp_at(self.section_start_bytes);
                self.section_len_offset = 0;
                self.stored_len_offset = 0;
                self.cached_mem_base = None;
                self.ensured_memory.clear();
                self.value_offsets.clear();
                self.relief_stores.clear();
                self.vstack.reset(section.inputs as usize, section.max_growth.max(0) as usize);
            }
        }
        self.bytes_before = if self.section_len_offset == 0 {
            self.section_start_bytes
        } else {
            self.bcx.iadd_imm(self.section_start_bytes, self.section_len_offset as i64 * WORD_SIZE)
        };

        // Check stack length for the current section.
        if self.config.stack_bound_checks && data.is_stack_section_head() {
            self.check_stack_bounds(data.stack_section);
        }

        // No-op instructions need no codegen beyond gas and bounds checks.
        if data.flags.contains(InstFlags::NOOP) {
            self.sync_noop_diff(inst, diff);
            self.section_len_offset += diff;
            goto_return!("noop");
        }

        // Store the updated stack length. Skip when `stack_bytes.addr` already holds the
        // correct value, i.e. the offset we'd write matches what's already stored.
        let new_len_offset = self.section_len_offset + diff;
        if new_len_offset != self.stored_len_offset {
            let bytes_after = self.bcx.iadd_imm(self.bytes_before, diff as i64 * WORD_SIZE);
            self.stack_bytes.store(&mut self.bcx, bytes_after);
            self.stored_len_offset = new_len_offset;
        }

        // If the output is a known constant and the opcode has no dynamic gas or side effects,
        // skip the real logic and just write the result.
        // The inputs are not loaded; we simply adjust the stack offset to consume them and
        // push the folded constant. This turns e.g. `PUSH 3, PUSH 4, ADD` into a single
        // store of `7`. Works for single-output arithmetic (ADD, MUL, ...) and DUP (which adds
        // one new TOS without consuming its input).
        if out >= 1
            && let Some(const_out) = self.bytecode.foldable_output(inst)
        {
            debug_assert!(
                out == 1 || out == inp + 1,
                "const_output assumes single synthesized push: inp={inp}, out={out}",
            );
            debug_assert!(!data.is_branching());
            // We push exactly 1 value, so consume `inp + 1 - out` to match the
            // real stack diff. For DUP (out = inp+1) this is 0; for ADD (out = 1)
            // this equals inp.
            let drop_count = (inp + 1 - out) as usize;
            self.drop_top(drop_count);
            self.len_offset -= drop_count as i8;
            let value = self.bcx.iconst_256(const_out);
            self.push(value);
            self.section_len_offset += diff;
            goto_return!("const output");
        }

        if self.try_peephole(data) {
            self.sync_virtual_stack_diff(diff);
            self.section_len_offset += diff;
            if self.current_inst().is_diverging() {
                goto_return!(no_branch);
            } else {
                goto_return!("peephole");
            }
        }

        // Macro utils.
        macro_rules! unop {
            ($op:ident) => {{
                let mut a = self.pop();
                a = self.bcx.$op(a);
                self.push(a);
            }};
        }

        macro_rules! binop {
            ($op:ident) => {{
                let [a, b] = self.popn();
                let r = self.bcx.$op(a, b);
                self.push(r);
            }};
            (@shift $op:ident, | $value:ident, $shift:ident | $default:expr) => {{
                let [$shift, $value] = self.popn();
                let r = self.bcx.$op($value, $shift);
                let overflow = self.bcx.icmp_imm(IntCC::UnsignedGreaterThan, $shift, 255);
                let default = $default;
                let r = self.bcx.select(overflow, default, r);
                self.push(r);
            }};
        }

        /// Gets the pointer to a field at a byte offset from `base`.
        macro_rules! field {
            // Gets the pointer to a field, named `<base>.<field>.addr`.
            (@get $base:ident, $offset:expr; $($spec:tt).*) => {
                self.get_field(
                    $base,
                    $offset,
                    concat!(stringify!($base), ".", stringify!($($spec).*), ".addr"),
                )
            };
            // Loads a field.
            // `@[endian]` is the endianness of the stored value. If native, omit it.
            (@load $(@[endian = $endian:tt])? $ty:expr, $base:ident, $offset:expr; $($spec:tt).*) => {{
                let ptr = field!(@get $base, $offset; $($spec).*);
                #[allow(unused_mut)]
                let mut value = self.bcx.load_aligned(
                    $ty,
                    ptr,
                    1,
                    concat!(stringify!($base), ".", stringify!($($spec).*)),
                );
                $(
                    if !cfg!(target_endian = $endian) {
                        value = self.bcx.bswap(value);
                    }
                )?
                value
            }};
            // Loads, extends (if necessary), and pushes a field.
            // `@[endian]` is the endianness of the stored value. If native, omit it.
            (@push $(@[endian = $endian:tt])? $ty:expr, $base:ident, $($rest:tt)*) => {{
                let mut value = field!(@load $(@[endian = $endian])? $ty, $base, $($rest)*);
                if self.bcx.type_bit_width($ty) < 256 {
                    value = self.bcx.zext(self.word_type, value);
                }
                self.push(value);
            }};
        }

        match data.opcode {
            _ if data.flags.contains(InstFlags::HOST) => {
                // The host runs on the interpreter's own stack.
                evm2_jit_backend::eyre::ensure!(
                    !self.config.local_stack,
                    "host opcodes need the interpreter stack"
                );
                // The host instruction sees the whole stack.
                self.materialize_live_stack();
                let sp = self.sp_after_inputs();
                let (inputs, outputs) = data.stack_io();
                let pc = self.bcx.iconst(self.isize_type, self.bytecode.pc(inst) as i64);
                let io =
                    self.bcx.iconst(self.isize_type, (i64::from(inputs) << 8) | i64::from(outputs));
                self.call_fallible_builtin(Builtin::HostInstruction, &[self.ecx, sp, pc, io]);
                // It may have rewritten any stack word.
                let live_start = self.vstack.live_range().start;
                self.vstack.forget_stored_from(live_start);
            }

            op::STOP => goto_return!(build InstrStop::Stop),

            op::ADD => {
                let [a, b] = self.popn();
                let sum = self.bcx.iadd(a, b);
                self.push(sum);
                let operand = match self.const_operands() {
                    [None, Some(c)] => Some((a, c)),
                    [Some(c), None] => Some((b, c)),
                    _ => None,
                };
                if let Some((operand, c)) = operand
                    && let Ok(c) = u32::try_from(c)
                {
                    let (base, addend) = self.memory_offset_parts(operand);
                    if let Ok(addend) = u32::try_from(addend + u64::from(c)) {
                        self.value_offsets.push((sum, base, addend.into()));
                    }
                }
            }
            op::MUL => binop!(imul),
            op::SUB => binop!(isub),
            op::DIV => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::Div, &[sp]);
            }
            op::SDIV => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::SDiv, &[sp]);
            }
            op::MOD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::Mod, &[sp]);
            }
            op::SMOD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::SMod, &[sp]);
            }
            op::ADDMOD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::AddMod, &[sp]);
            }
            op::MULMOD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::MulMod, &[sp]);
            }
            op::EXP => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Exp, &[self.ecx, sp]);
            }
            op::SIGNEXTEND => {
                // let shift = 248 - 8 * ext;
                // ext < 31
                //   ? (x << shift) >>s shift
                //   : x
                let [ext, x] = self.popn();

                let might_do_something = self.bcx.icmp_imm(IntCC::UnsignedLessThan, ext, 31);

                let shift = self.bcx.imul_imm(ext, 8);
                let c248 = self.bcx.iconst_256(248);
                let shift = self.bcx.isub(c248, shift);
                let shifted = self.bcx.ishl(x, shift);
                let sext = self.bcx.sshr(shifted, shift);

                let r = self.bcx.select(might_do_something, sext, x);
                self.push(r);
            }

            op::LT | op::GT | op::SLT | op::SGT | op::EQ => {
                let cond = match opcode {
                    op::LT => IntCC::UnsignedLessThan,
                    op::GT => IntCC::UnsignedGreaterThan,
                    op::SLT => IntCC::SignedLessThan,
                    op::SGT => IntCC::SignedGreaterThan,
                    op::EQ => IntCC::Equal,
                    _ => unreachable!(),
                };

                let [a, b] = self.popn();
                let r = self.bcx.icmp(cond, a, b);
                let r = self.bcx.zext(self.word_type, r);
                self.push(r);
            }
            op::ISZERO => {
                let a = self.pop();
                let r = self.bcx.icmp_imm(IntCC::Equal, a, 0);
                let r = self.bcx.zext(self.word_type, r);
                self.push(r);
            }
            op::AND => binop!(bitand),
            op::OR => binop!(bitor),
            op::XOR => binop!(bitxor),
            op::NOT => unop!(bitnot),
            op::BYTE => {
                // index < 32
                //   ? (value >> (248 - index * 8)) & 0xFF
                //   : 0
                let [index, value] = self.popn();

                let in_range = self.bcx.icmp_imm(IntCC::UnsignedLessThan, index, 32);

                let shift = self.bcx.imul_imm(index, 8);
                let c248 = self.bcx.iconst_256(248);
                let shift = self.bcx.isub(c248, shift);
                let shifted = self.bcx.ushr(value, shift);
                let mask = self.bcx.iconst_256(0xFF);
                let byte = self.bcx.bitand(shifted, mask);

                let zero = self.bcx.iconst_256(0);

                let r = self.bcx.select(in_range, byte, zero);
                self.push(r);
            }
            op::SHL => binop!(@shift ishl, |value, shift| self.bcx.iconst_256(0)),
            op::SHR => binop!(@shift ushr, |value, shift| self.bcx.iconst_256(0)),
            op::SAR => binop!(@shift sshr, |value, shift| {
                let is_negative = self.bcx.icmp_imm(IntCC::SignedLessThan, value, 0);
                let max = self.bcx.iconst_256(U256::MAX);
                let zero = self.bcx.iconst_256(0);
                self.bcx.select(is_negative, max, zero)
            }),
            op::CLZ => unop!(clz),

            op::KECCAK256 => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Keccak256, &[self.ecx, sp]);
            }

            op::ADDRESS => {
                let message = self.load_message_ptr();
                field!(@push @[endian = "big"] self.address_type, message, self.config.layout.destination; destination);
            }
            op::BALANCE => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Balance, &[self.ecx, sp]);
            }
            op::ORIGIN => {
                let tx_env = self.load_tx_env_ptr();
                field!(@push @[endian = "big"] self.address_type, tx_env, self.config.layout.tx_origin; origin);
            }
            op::CALLER => {
                let message = self.load_message_ptr();
                field!(@push @[endian = "big"] self.address_type, message, self.config.layout.caller; caller);
            }
            op::CALLVALUE => {
                let message = self.load_message_ptr();
                field!(@push self.word_type, message, self.config.layout.value; value);
            }
            op::CALLDATALOAD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::CallDataLoad, &[self.ecx, sp]);
            }
            op::CALLDATASIZE => {
                let ecx = self.ecx;
                field!(@push self.isize_type, ecx, mem::offset_of!(EvmContext<'_, '_, '_>, calldatasize); calldatasize);
            }
            op::CALLDATACOPY => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::CallDataCopy, &[self.ecx, sp]);
            }
            op::CODESIZE => {
                let len = self.bcx.iconst(self.word_type, self.bytecode.codesize() as i64);
                self.push(len);
            }
            op::CODECOPY => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::CodeCopy, &[self.ecx, sp]);
            }

            op::GASPRICE => {
                let tx_env = self.load_tx_env_ptr();
                field!(@push self.word_type, tx_env, self.config.layout.tx_gas_price; gas_price);
            }
            op::EXTCODESIZE => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::ExtCodeSize, &[self.ecx, sp]);
            }
            op::EXTCODECOPY => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::ExtCodeCopy, &[self.ecx, sp]);
            }
            op::RETURNDATASIZE => {
                let ecx = self.ecx;
                field!(@push self.isize_type, ecx, mem::offset_of!(EvmContext<'_, '_, '_>, return_data_len); return_data_len);
            }
            op::RETURNDATACOPY => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::ReturnDataCopy, &[self.ecx, sp]);
            }
            op::EXTCODEHASH => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::ExtCodeHash, &[self.ecx, sp]);
            }
            op::BLOCKHASH => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::BlockHash, &[self.ecx, sp]);
            }
            op::COINBASE => {
                let block_env = self.load_block_env_ptr();
                field!(@push @[endian = "big"] self.address_type, block_env, self.config.layout.block_beneficiary; beneficiary);
            }
            op::TIMESTAMP => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_timestamp; timestamp);
            }
            op::NUMBER => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_number; number);
            }
            op::DIFFICULTY => {
                let slot = self.sp_at_top();
                let _ = self.call_builtin(Builtin::Difficulty, &[self.ecx, slot]);
            }
            op::GASLIMIT => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_gas_limit; gas_limit);
            }
            op::CHAINID => {
                let tx_env = self.load_tx_env_ptr();
                field!(@push self.word_type, tx_env, self.config.layout.tx_chain_id; chain_id);
            }
            op::SELFBALANCE => {
                let slot = self.sp_at_top();
                self.call_fallible_builtin(Builtin::SelfBalance, &[self.ecx, slot]);
            }
            op::BASEFEE => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_basefee; basefee);
            }
            op::BLOBHASH => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::BlobHash, &[self.ecx, sp]);
            }
            op::BLOBBASEFEE => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_blob_basefee; blob_basefee);
            }
            op::SLOTNUM => {
                let block_env = self.load_block_env_ptr();
                field!(@push self.word_type, block_env, self.config.layout.block_slot_num; slot_num);
            }

            op::POP => {
                self.pop_ignore(1);
            }
            op::MLOAD => {
                let offset = self.pop();
                let addr = self.build_ensure_memory(offset, 32);
                let value = self.bcx.load_aligned(self.word_type, addr, 1, "mload.value");
                let value = self.bcx.bswap(value);
                self.push(value);
            }
            op::MSTORE => {
                let [offset, value] = self.popn();
                let addr = self.build_ensure_memory(offset, 32);
                let value = self.bcx.bswap(value);
                self.bcx.store_aligned(value, addr, 1);
            }
            op::MSTORE8 => {
                let [offset, value] = self.popn();
                let addr = self.build_ensure_memory(offset, 1);
                let value = self.bcx.ireduce(self.i8_type, value);
                self.bcx.store_aligned(value, addr, 1);
            }
            op::SLOAD => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Sload, &[self.ecx, sp]);
            }
            op::SSTORE => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Sstore, &[self.ecx, sp]);
            }
            op::JUMP | op::JUMPI => {
                let is_invalid = data.flags.contains(InstFlags::INVALID_JUMP);
                let has_const_jumpi_condition = data.has_const_jumpi_condition();
                if is_invalid && opcode == op::JUMP {
                    // Pop and discard the target; it's always on the stack.
                    self.pop_ignore(1);
                    self.build_fail_imm(InstrStop::InvalidJump);
                    goto_return!(no_branch);
                }

                let is_multi = data.flags.contains(InstFlags::MULTI_JUMP);
                let target_value = if is_multi || !data.flags.contains(InstFlags::STATIC_JUMP) {
                    Some(self.pop())
                } else {
                    // Pop and discard the target; it's always on the stack.
                    self.pop_ignore(1);
                    None
                };

                // The condition and fall-through block of a `JUMPI` whose taken edge is still
                // to be built.
                let mut jumpi = None;
                if opcode == op::JUMPI && !has_const_jumpi_condition {
                    let cond_word = self.pop();
                    let cond = self.bcx.icmp_imm(IntCC::NotEqual, cond_word, 0);
                    let next = self
                        .effective_next_entry(inst)
                        .expect("JUMPI must have a fallthrough target");
                    // When the fall-through continues this section, it keeps the virtual stack
                    // in SSA and only the taken edge stores it, in its own block.
                    let keep_vstack = self.bytecode.inst(inst + 1).continues_stack_section()
                        && self.vstack.virtual_count() != 0;
                    if !keep_vstack {
                        self.materialize_live_stack();
                    }
                    if keep_vstack || is_multi {
                        let name = if is_multi { "multi_jump" } else { "jumpi.taken" };
                        let current_block = self.current_block();
                        let taken = self.create_block_after(current_block, name);
                        self.bcx.brif(cond, taken, next);
                        self.bcx.switch_to_block(taken);
                        if keep_vstack {
                            self.store_range(self.vstack.live_range());
                        }
                    } else {
                        jumpi = Some((cond, next));
                    }
                } else {
                    if opcode == op::JUMPI {
                        self.pop_ignore(1);
                    }
                    // Flush virtual values before leaving the section.
                    self.materialize_live_stack();
                }
                // A continuation of this section starts from the stack after the jump.
                self.section_len_offset += diff;

                if is_multi {
                    // The analysis proved the target is one of `targets`, all of which fit in
                    // 64 bits, so switch on the low bits instead of the whole word.
                    let i64_type = self.bcx.type_int(64);
                    let target_value = self.bcx.ireduce(i64_type, target_value.unwrap());
                    let targets = self.bytecode.multi_jump_targets(inst).unwrap();
                    let switch_targets: Vec<_> = targets
                        .iter()
                        .map(|&t| {
                            let pc = self.bytecode.inst(t).jumpdest_pc() as u64;
                            (pc, self.effective_entry(t))
                        })
                        .collect();
                    let invalid_jump = self.add_invalid_jump();
                    self.bcx.switch(target_value, invalid_jump, &switch_targets, true);
                    goto_return!(no_branch);
                }

                let target = if is_invalid {
                    debug_assert_eq!(*data, op::JUMPI);
                    self.add_invalid_jump()
                } else if let Some(target) = target_value {
                    // Dynamic jump.
                    debug_assert!(self.bytecode.has_dynamic_jumps());
                    let target = self.u256_to_u64_saturating(target, 64);
                    let current_block = self.current_block();
                    self.incoming_dynamic_jumps.push((target, current_block));
                    self.dynamic_jump_table
                } else {
                    let target_inst = data.static_jump_target();
                    debug_assert!(
                        *self.bytecode.inst(target_inst) == op::JUMPDEST
                            || (opcode == op::JUMPI && has_const_jumpi_condition),
                        "jumping to non-JUMPDEST; target_inst={target_inst}",
                    );
                    self.effective_entry(target_inst)
                };
                if let Some((cond, next)) = jumpi {
                    self.bcx.brif(cond, target, next);
                } else {
                    self.bcx.br(target);
                }

                goto_return!(no_branch);
            }
            op::PC => {
                let pc = self.bcx.iconst_256(data.pc_imm());
                self.push(pc);
            }
            op::MSIZE => {
                let mem_len_field = self.get_field(
                    self.ecx,
                    mem::offset_of!(EvmContext<'_, '_, '_>, mem_len),
                    "ecx.mem_len.addr",
                );
                let mem_len = self.bcx.load(self.isize_type, mem_len_field, "ecx.mem_len");
                let msize = self.bcx.zext(self.word_type, mem_len);
                self.push(msize);
            }
            op::GAS => {
                self.build_gas_check();
                let mut remaining = self.load_gas_remaining();
                // The section head already charged the static gas of the instructions after this.
                let owed = data.gas_owed();
                if self.config.gas_metering && owed != 0 {
                    remaining = self.bcx.iadd_imm(remaining, owed as i64);
                }
                let remaining = self.bcx.zext(self.word_type, remaining);
                self.push(remaining);
            }
            op::JUMPDEST => {
                self.bcx.nop();
            }
            op::TLOAD => {
                let sp = self.sp_after_inputs();
                let _ = self.call_builtin(Builtin::Tload, &[self.ecx, sp]);
            }
            op::TSTORE => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Tstore, &[self.ecx, sp]);
            }
            op::MCOPY => {
                let sp = self.sp_after_inputs();
                self.call_fallible_builtin(Builtin::Mcopy, &[self.ecx, sp]);
            }

            op::PUSH0..=op::PUSH32 => {
                let value = self.bytecode.get_push_value(data);
                let value = self.bcx.iconst_256(value);
                self.push(value);
            }

            op::DUP1..=op::DUP16 => self.dup((opcode - op::DUP1 + 1) as usize),
            op::DUPN => match decode_single(data.imm_byte()) {
                Some(n) => self.dup(n as usize),
                None => goto_return!(fail InstrStop::InvalidImmediateEncoding),
            },

            op::SWAP1..=op::SWAP16 => self.swap((opcode - op::SWAP1 + 1) as usize),
            op::SWAPN => match decode_single(data.imm_byte()) {
                Some(n) => self.swap(n as usize),
                None => goto_return!(fail InstrStop::InvalidImmediateEncoding),
            },

            op::EXCHANGE => match decode_pair(data.imm_byte()) {
                Some((n, m)) => self.exchange(n as usize, (m - n) as usize),
                None => goto_return!(fail InstrStop::InvalidImmediateEncoding),
            },

            op::LOG0..=op::LOG4 => {
                let n = opcode - op::LOG0;
                let sp = self.sp_after_inputs();
                let n = self.bcx.iconst(self.i8_type, n as i64);
                self.call_fallible_builtin(Builtin::Log, &[self.ecx, sp, n]);
            }

            op::CREATE => {
                self.create_common(CreateKind::Create);
            }
            op::CALL => {
                self.call_common(CallKind::Call);
            }
            op::CALLCODE => {
                self.call_common(CallKind::CallCode);
            }
            op::RETURN => {
                self.return_common(InstrStop::Return);
                goto_return!(no_branch);
            }
            op::DELEGATECALL => {
                self.call_common(CallKind::DelegateCall);
            }
            op::CREATE2 => {
                self.create_common(CreateKind::Create2);
            }

            op::STATICCALL => {
                self.call_common(CallKind::StaticCall);
            }

            op::REVERT => {
                self.return_common(InstrStop::Revert);
                goto_return!(no_branch);
            }
            op::INVALID => goto_return!(fail InstrStop::InvalidFEOpcode),
            op::SELFDESTRUCT => {
                let sp = self.sp_after_inputs();
                self.sync_diverging_stack_effect();
                self.call_fallible_builtin(Builtin::SelfDestruct, &[self.ecx, sp]);
                self.bcx.unreachable();
                goto_return!(no_branch);
            }

            _ => unreachable!("unimplemented instruction: {data:?}"),
        }

        self.sync_virtual_stack_diff(diff);
        self.section_len_offset += diff;
        goto_return!("normal exit");
    }

    /// Syncs the virtual stack for a NOOP instruction. Unlike `sync_virtual_stack_diff`,
    /// this preserves known-constant outputs as `Virtual` values so that boundary
    /// materialization (e.g. `materialize_live_stack` before a branch) can flush
    /// them to physical memory. Without this, NOOP'd constants would be marked
    /// `Materialized` with no actual store, leaving garbage in memory.
    fn sync_noop_diff(&mut self, inst: Inst, diff: i32) {
        let expected_top = self.section_len_offset + diff;
        let current_top = self.vstack.top_offset();
        if current_top == expected_top {
            return;
        }
        let delta = expected_top - current_top;
        if delta < 0 {
            self.drop_top((-delta) as usize);
        } else {
            // For NOOP instructions that push values, use the known constant output
            // if available. This ensures the value can be materialized later.
            if delta == 1
                && let Some(c) = self.bytecode.const_output(inst)
            {
                let value = self.bcx.iconst_256(c);
                self.vstack.push(value);
            } else {
                for _ in 0..delta {
                    self.vstack.push_mem();
                }
            }
        }
    }

    /// Syncs the virtual stack's `top_offset` with the expected value after applying
    /// the instruction's stack diff. For inline ops (push/pop), the virtual stack is
    /// already up to date. For builtin ops (sp_after_inputs/sp_at_top), this adjusts
    /// the virtual stack to account for consumed inputs and materialized outputs.
    ///
    /// Also invalidates any virtual slots in the output area that the builtin may have
    /// overwritten in physical memory, to prevent stale virtual values from shadowing
    /// the builtin's actual output.
    fn sync_virtual_stack_diff(&mut self, diff: i32) {
        let expected_top = self.section_len_offset + diff;
        let current_top = self.vstack.top_offset();
        if current_top == expected_top {
            return;
        }
        let delta = expected_top - current_top;
        if expected_top < self.vstack.live_range().start {
            let inst = self.current_inst.unwrap();
            // Walk backward to find the section head for diagnostic context.
            let mut head = inst;
            for i in (0..inst.index()).rev() {
                let idx = Inst::new(i);
                let d = self.bytecode.inst(idx);
                if d.is_dead_code() {
                    continue;
                }
                if d.is_stack_section_head() {
                    head = idx;
                    break;
                }
            }
            // Dump the section from head to current inst.
            let mut section_dump = String::new();
            for i in head.index()..=inst.index() {
                let idx = Inst::new(i);
                let d = self.bytecode.inst(idx);
                if d.is_dead_code() {
                    continue;
                }
                let _ = write!(
                    section_dump,
                    "\n  ic{i} pc={} {:?} io={:?} flags={:?} gas={:?} stack={:?}{}{}",
                    self.bytecode.pc(idx),
                    d.to_op(),
                    d.stack_io(),
                    d.flags,
                    d.gas_section,
                    d.stack_section,
                    if d.is_stack_section_head() { " SECTION_HEAD" } else { "" },
                    if d.is_dead_code() { " DEAD" } else { "" },
                );
            }
            let head_data = self.bytecode.inst(head);
            panic!(
                "sync: expected_top={expected_top} < base={}, section_len_offset={}, \
                 diff={diff}, current_top={current_top}, inst={:?} (ic{})\n\
                 section head=ic{}, head_stack_section={:?}, section:{section_dump}",
                self.vstack.live_range().start,
                self.section_len_offset,
                self.current_inst().to_op(),
                inst.index(),
                head.index(),
                head_data.stack_section,
            );
        }
        if delta < 0 {
            self.drop_top((-delta) as usize);
        } else {
            for _ in 0..delta {
                self.vstack.push_mem();
            }
        }

        // Mark the builtin's output area as materialized. The builtin wrote to
        // physical memory at `sp[0..outputs]`, which corresponds to offsets
        // `expected_top - outputs .. expected_top`. Any pre-existing Virtual
        // entries (e.g. from NOOP'd producers) must be invalidated.
        let (_, outputs) = self.current_inst().stack_io();
        let outputs = outputs as i32;
        if outputs > 0 {
            self.vstack.mark_materialized_range(expected_top - outputs..expected_top);
        }

        debug_assert_eq!(
            self.vstack.top_offset(),
            expected_top,
            "virtual stack sync mismatch after {:?}",
            self.current_inst().to_op(),
        );
    }

    /// Pushes a 256-bit value onto the stack.
    fn push(&mut self, value: B::Value) {
        self.vstack.push(value);
        self.len_offset += 1;
    }

    /// Returns the known constant values of the topmost `N` stack operands, in the same order
    /// as [`popn`](Self::popn): index 0 is TOS, index 1 is second from top, etc.
    fn const_operands<const N: usize>(&self) -> [Option<U256>; N] {
        let inst = self.current_inst.unwrap();
        std::array::from_fn(|i| self.bytecode.const_operand(inst, i))
    }

    /// Discards `n` stack inputs and pushes a compile-time constant.
    fn fold_const(&mut self, value: impl TryInto<U256>) {
        self.pop_ignore(self.current_inst().stack_io().0 as usize);
        let v = self.bcx.iconst_256(value);
        self.push(v);
    }

    /// Consumes the topmost `n` elements from the stack without loading them.
    fn pop_ignore(&mut self, n: usize) {
        self.drop_top(n);
        self.len_offset -= n as i8;
    }

    /// Drops the topmost `n` slots from the virtual stack and removes the relief stores to
    /// them that nothing read.
    fn drop_top(&mut self, n: usize) {
        self.vstack.drop_top(n);
        let top = self.vstack.top_offset();
        let bcx = &mut self.bcx;
        self.relief_stores.retain(|&(off, store, _)| {
            let live = off < top;
            if !live {
                bcx.remove_inst(store);
            }
            live
        });
    }

    /// Removes the topmost element from the stack and returns it.
    fn pop(&mut self) -> B::Value {
        self.popn::<1>()[0]
    }

    /// Removes the topmost `N` elements from the stack and returns them.
    fn popn<const N: usize>(&mut self) -> [B::Value; N] {
        assert_ne!(N, 0);

        let inst = self.current_inst.unwrap();
        let operand_depth_base = (-self.len_offset) as usize;
        let values = std::array::from_fn(|i| {
            let operand_depth = operand_depth_base + i;
            // Using the value of a relief store made since the last join instead of reloading
            // it leaves the store unread, so the pop below removes it.
            if self.bytecode.const_operand(inst, operand_depth).is_none()
                && let VSlot::Materialized(Some(stored)) = self.vstack.get(i)
                && self.is_recent_relief_store(self.vstack.offset_at_depth(i))
            {
                return stored;
            }
            let name = b'a' + i as u8;
            self.stack_value_at_depth(operand_depth, i, std::str::from_utf8(&[name]).unwrap())
        });
        self.pop_ignore(N);
        values
    }

    /// Duplicates the `n`th value from the top of the stack.
    /// `n` cannot be `0`.
    fn dup(&mut self, n: usize) {
        assert_ne!(n, 0);
        let name = if self.config.debug { &format!("dup{n}") } else { "" };
        let value = self.stack_value_at_depth(n - 1, n - 1, name);
        self.push(value);
    }

    /// Swaps the topmost value with the `n`th value from the top.
    /// `n` cannot be `0`.
    fn swap(&mut self, n: usize) {
        self.exchange(0, n);
    }

    /// Exchange two values on the stack.
    /// `n` is the first index, and the second index is calculated as `n + m`.
    /// `m` cannot be `0`.
    fn exchange(&mut self, n: usize, m: usize) {
        assert_ne!(m, 0);
        let a = self.stack_value_at_depth(n, n, "swap.a");
        let b = self.stack_value_at_depth(n + m, n + m, "swap.b");
        self.vstack.set(n, b);
        self.vstack.set(n + m, a);
    }

    /// `RETURN` or `REVERT` instruction.
    fn return_common(&mut self, ir: InstrStop) {
        let sp = self.sp_after_inputs();
        let ir_const = self.bcx.iconst(self.i8_type, ir as i64);
        self.sync_diverging_stack_effect();
        self.call_fallible_builtin(Builtin::DoReturn, &[self.ecx, sp, ir_const]);
        self.bcx.unreachable();
    }

    fn sync_diverging_stack_effect(&mut self) {
        let data = self.current_inst();
        let (inp, out) = data.stack_io();
        let diff = out as i32 - inp as i32;
        self.sync_virtual_stack_diff(diff);
        if self.config.inspect_stack {
            self.materialize_live_stack();
            self.copy_stack_to_arg();
            self.save_stack_len();
        }
    }

    /// Builds a `CREATE` or `CREATE2` instruction.
    fn create_common(&mut self, create_kind: CreateKind) {
        let sp = self.sp_after_inputs();
        let create_kind = self.bcx.iconst(self.i8_type, create_kind as i64);
        self.call_fallible_builtin(Builtin::Create, &[self.ecx, sp, create_kind]);
    }

    /// Builds `*CALL*` instructions.
    fn call_common(&mut self, call_kind: CallKind) {
        let sp = self.sp_after_inputs();
        let call_kind = self.bcx.iconst(self.i8_type, call_kind as i64);
        self.call_fallible_builtin(Builtin::Call, &[self.ecx, sp, call_kind]);
    }

    /// Loads the word at the given pointer.
    fn load_word(&mut self, ptr: B::Value, name: &str) -> B::Value {
        self.bcx.load(self.word_type, ptr, name)
    }

    /// Loads the pointer stored in the `EvmContext` field at the given offset.
    fn load_ecx_ptr(&mut self, offset: usize, addr_name: &str, name: &str) -> B::Value {
        let ptr_type = self.bcx.type_ptr();
        let field = self.get_field(self.ecx, offset, addr_name);
        self.bcx.load(ptr_type, field, name)
    }

    fn load_interpreter_ptr(&mut self) -> B::Value {
        self.load_ecx_ptr(
            mem::offset_of!(EvmContext<'_, '_, '_>, interpreter),
            "ecx.interpreter.addr",
            "ecx.interpreter",
        )
    }

    fn load_message_ptr(&mut self) -> B::Value {
        let ptr_type = self.bcx.type_ptr();
        let interpreter = self.load_interpreter_ptr();
        let field = self.get_field(
            interpreter,
            self.config.layout.interpreter_message,
            "interpreter.message.addr",
        );
        self.bcx.load(ptr_type, field, "interpreter.message")
    }

    fn load_block_env_ptr(&mut self) -> B::Value {
        self.load_ecx_ptr(
            mem::offset_of!(EvmContext<'_, '_, '_>, block_env),
            "ecx.block_env.addr",
            "ecx.block_env",
        )
    }

    fn load_tx_env_ptr(&mut self) -> B::Value {
        self.load_ecx_ptr(
            mem::offset_of!(EvmContext<'_, '_, '_>, tx_env),
            "ecx.tx_env.addr",
            "ecx.tx_env",
        )
    }

    /// Gets a field at the given offset.
    fn get_field(&mut self, ptr: B::Value, offset: usize, name: &str) -> B::Value {
        get_field(&mut self.bcx, ptr, offset, name)
    }

    fn gas_remaining_addr(&mut self) -> B::Value {
        const OFFSET: usize = mem::offset_of!(EvmContext<'_, '_, '_>, gas);
        self.get_field(self.ecx, OFFSET, "ecx.gas.remaining.addr")
    }

    fn load_gas_remaining(&mut self) -> B::Value {
        let addr = self.gas_remaining_addr();
        let i64_type = self.bcx.type_int(64);
        self.bcx.load(i64_type, addr, "ecx.gas.remaining")
    }

    /// Saves the local `stack_bytes` to `stack_len_arg` as a number of words.
    fn save_stack_len(&mut self) {
        let stack_bytes = self.stack_bytes.load(&mut self.bcx, "stack_bytes");
        let word_size = self.bcx.iconst(self.isize_type, WORD_SIZE);
        let len = self.bcx.udiv(stack_bytes, word_size);
        let ptr = self.stack_len_arg();
        self.bcx.store(len, ptr);
    }

    /// Copies the live prefix of the stack from the argument to the local alloca.
    /// `stack_bytes` is the size of the live stack in bytes.
    fn copy_stack_from_arg(&mut self, stack_bytes: B::Value) {
        if let Some(src) = self.sp_arg {
            let dst = self.stack.addr(&mut self.bcx);
            self.bcx.memcpy(dst, src, stack_bytes);
        }
    }

    /// Copies the live prefix of the stack from the local alloca to the argument.
    fn copy_stack_to_arg(&mut self) {
        if let Some(dst) = self.sp_arg {
            let stack_bytes = self.stack_bytes.load(&mut self.bcx, "stack_bytes");
            let src = self.stack.addr(&mut self.bcx);
            self.bcx.memcpy(dst, src, stack_bytes);
        }
    }

    /// Returns the stack length argument.
    fn stack_len_arg(&mut self) -> B::Value {
        self.bcx.fn_param(2)
    }

    /// Returns the stack pointer at the top (`&stack[stack.len]`).
    ///
    /// Used by builtins that write a single output directly to memory.
    /// The virtual stack is synced at instruction end via `sync_virtual_stack_diff`.
    #[must_use]
    fn sp_at_top(&mut self) -> B::Value {
        self.sp_from_section(self.section_len_offset as i64)
    }

    /// Returns the stack pointer after the input has been popped
    /// (`&stack[stack.len - op.input()]`).
    ///
    /// This materializes all virtual values in the input/output window so builtins
    /// can read/write the physical stack. For any input whose value is a known constant,
    /// the constant is written into the corresponding stack slot. This allows DSE to NOOP
    /// the producing PUSH even for builtin-delegated opcodes that read operands directly
    /// from the stack pointer.
    ///
    /// The virtual stack is synced with the builtin's stack effect at instruction end
    /// via `sync_virtual_stack_diff`.
    #[must_use]
    fn sp_after_inputs(&mut self) -> B::Value {
        let (inputs, outputs) = self.current_inst().stack_io();
        let inputs = inputs as usize;
        let outputs = outputs as usize;
        let top = self.section_len_offset;
        let start = top - inputs as i32;
        let window = inputs.max(outputs) as i32;
        self.materialize_range(start, start + window);
        self.write_const_operands(inputs);
        self.sp_from_top(inputs)
    }

    /// Like [`sp_after_inputs`](Self::sp_after_inputs) but only writes the known-constant
    /// operands, leaving the other inputs virtual.
    #[must_use]
    fn sp_after_const_inputs(&mut self) -> B::Value {
        let inputs = self.current_inst().stack_io().0 as usize;
        self.write_const_operands(inputs);
        self.sp_from_top(inputs)
    }

    /// Writes known-constant operands into the physical stack so that builtins see
    /// correct values even when DSE has NOOP'd the producing instruction.
    fn write_const_operands(&mut self, inputs: usize) {
        let inst = self.current_inst.unwrap();
        let top = self.section_len_offset;
        for depth in 0..inputs {
            let off = top - inputs as i32 + (inputs - 1 - depth) as i32;
            if let VSlot::Materialized(_) = self.vstack.get_at_offset(off)
                && let Some(c) = self.bytecode.const_operand(inst, depth)
            {
                let value = self.bcx.iconst_256(c);
                let sp = self.sp_from_section(off as i64);
                self.bcx.store(value, sp);
            }
        }
    }

    /// Returns a stack pointer offset from `section_start_sp`.
    fn sp_from_section(&mut self, offset: i64) -> B::Value {
        if offset == 0 {
            return self.section_start_sp;
        }
        let offset = self.bcx.iconst(self.isize_type, offset);
        self.bcx.gep(self.word_type, self.section_start_sp, &[offset], "sp")
    }

    /// Returns the stack pointer at `stack_bytes` (`&stack[stack_bytes / 32]`).
    fn sp_at(&mut self, stack_bytes: B::Value) -> B::Value {
        let ptr = self.stack.addr(&mut self.bcx);
        self.bcx.gep(self.i8_type, ptr, &[stack_bytes], "sp")
    }

    /// Returns the stack pointer at `n` from the top (`&stack[len - n]`) for a builtin, which may
    /// read the slots above it.
    fn sp_from_top(&mut self, n: usize) -> B::Value {
        let top = self.section_len_offset;
        self.observe_slots(top - n as i32..top);
        self.sp_from_section(top as i64 - n as i64)
    }

    /// Resolves a stack value at the given depth via the virtual stack.
    ///
    /// - `operand_depth`: depth for `const_operand` lookup (0 = first popped by the instruction).
    /// - `live_depth`: depth into the virtual stack's current live range (0 = current TOS).
    ///
    /// These differ only inside `popn` where multiple pops happen: `operand_depth` counts
    /// from the instruction start, while `live_depth` counts from the current virtual TOS.
    fn stack_value_at_depth(
        &mut self,
        operand_depth: usize,
        live_depth: usize,
        name: &str,
    ) -> B::Value {
        let inst = self.current_inst.unwrap();
        if let Some(c) = self.bytecode.const_operand(inst, operand_depth) {
            return self.bcx.iconst_256(c);
        }
        match self.vstack.get(live_depth) {
            VSlot::Virtual(v) => v,
            VSlot::Materialized(stored) => {
                let off = self.vstack.offset_at_depth(live_depth);
                self.observe_slots(off..off + 1);
                let sp = self.sp_from_section(off as i64);
                let value = self.load_word(sp, name);
                if let Some(stored) = stored {
                    let (base, addend) = self.memory_offset_parts(stored);
                    self.value_offsets.push((value, base, addend));
                }
                // Memory still holds the reloaded value, so don't store it back.
                self.vstack.set_stored_at_offset(off, value);
                value
            }
        }
    }

    /// Materializes all live virtual slots in the current section to memory.
    fn materialize_live_stack(&mut self) {
        let range = self.vstack.live_range();
        self.materialize_range(range.start, range.end);
    }

    /// Eagerly materializes the coldest virtual slots when too many are live,
    /// preventing excessive register pressure in long sections.
    fn relieve_vstack_pressure(&mut self) {
        /// Materialize when more than this many virtual slots are live.
        const HIGH_WATER: usize = 2;
        /// Never materialize the top N slots (they're likely used soon).
        const KEEP_HOT: usize = 2;

        let live = self.vstack.live_range();
        if (live.end - live.start) as usize <= HIGH_WATER {
            return;
        }

        let virtual_count = self.vstack.virtual_count();
        if virtual_count <= HIGH_WATER {
            return;
        }

        // Materialize everything below the hot window.
        let cold_end = (self.vstack.top_offset() - KEEP_HOT as i32).max(live.start);
        if cold_end > live.start {
            let range = live.start..cold_end;
            let pending: Vec<_> = self.vstack.pending_stores(range.clone()).collect();
            for (off, value) in pending {
                self.observe_slots(off..off + 1);
                let sp = self.sp_from_section(off as i64);
                let store = self.bcx.store(value, sp);
                self.relief_stores.push((off, store, self.joins));
            }
            self.vstack.mark_stored_range(range);
        }
    }

    /// Returns whether the slot at `offset` holds a relief store made since the last join.
    fn is_recent_relief_store(&self, offset: i32) -> bool {
        self.relief_stores.iter().any(|&(off, _, joins)| off == offset && joins == self.joins)
    }

    /// Keeps the relief stores to the given slots, which something may read.
    fn observe_slots(&mut self, range: Range<i32>) {
        self.relief_stores.retain(|(off, ..)| !range.contains(off));
    }

    /// Materializes all virtual slots in the given section-relative offset range.
    fn materialize_range(&mut self, start: i32, end: i32) {
        self.store_range(start..end);
        self.vstack.mark_stored_range(start..end);
    }

    /// Stores all virtual slots in the given section-relative offset range without marking
    /// them materialized.
    fn store_range(&mut self, range: Range<i32>) {
        self.observe_slots(range.clone());
        let pending: Vec<_> = self.vstack.pending_stores(range).collect();
        for (off, value) in pending {
            let sp = self.sp_from_section(off as i64);
            self.bcx.store(value, sp);
        }
    }

    /// Charges the static gas of the section headed by `head`.
    ///
    /// Only a section entered through a `JUMPDEST` can start unbounded work, so sections entered
    /// only by falling through subtract their gas without a branch until the gas charged since
    /// the last check reaches [`GAS_CHECK_THRESHOLD`]. The check is signed, so a skipped deficit
    /// stays visible as negative remaining gas. Sections that return or call builtins that charge
    /// gas check at their head; [`build_gas_check`](Self::build_gas_check) covers the rest.
    fn charge_section_gas(&mut self, head: Inst) {
        if !self.config.gas_metering {
            return;
        }

        if !self.bytecode.is_fall_through_only(head) {
            // Other paths can arrive with gas left unchecked, so force a check.
            self.unchecked_gas = GAS_CHECK_THRESHOLD;
        }
        let cost = self.bytecode.inst(head).gas_section.gas_cost as u64;
        if cost != 0 {
            let remaining = self.load_gas_remaining();
            let remaining = self.bcx.isub_imm(remaining, cost as i64);
            let addr = self.gas_remaining_addr();
            self.bcx.store(remaining, addr);
            self.unchecked_gas += cost;
        }
        if self.unchecked_gas >= GAS_CHECK_THRESHOLD
            || (self.unchecked_gas != 0 && self.section_needs_gas_check(head))
        {
            self.build_gas_check();
        }
    }

    /// Returns `true` if the section headed by `head` reads the remaining gas, charges it in a
    /// builtin, or ends execution.
    ///
    /// Memory resizes are left out: they are conditional and check on their own cold path.
    fn section_needs_gas_check(&self, head: Inst) -> bool {
        std::iter::successors(Some(head), |&inst| self.lexical_next_inst(inst))
            .map(|inst| (inst, self.bytecode.inst(inst)))
            .filter(|(_, data)| !data.is_dead_code())
            .take_while(|&(inst, data)| inst == head || !data.is_stack_section_head())
            .any(|(_, data)| {
                data.is_diverging()
                    || data.ends_sections()
                    || matches!(
                        data.opcode,
                        op::GAS
                            | op::EXP
                            | op::KECCAK256
                            | op::CALLDATACOPY
                            | op::CODECOPY
                            | op::RETURNDATACOPY
                            | op::MCOPY
                            | op::LOG0..=op::LOG4
                    )
            })
    }

    /// Fails with `OutOfGas` if gas subtracted without a check left the remaining gas negative.
    ///
    /// The check covers all gas charged so far, so it clears `unchecked_gas`;
    /// [`if_then`](Self::if_then) restores it after a conditional path.
    fn build_gas_check(&mut self) {
        if mem::take(&mut self.unchecked_gas) == 0 {
            return;
        }
        let remaining = self.load_gas_remaining();
        let out_of_gas = self.bcx.icmp_imm(IntCC::SignedLessThan, remaining, 0);
        self.build_check(out_of_gas, InstrStop::OutOfGas);
    }

    /// Charges a dynamic gas cost, failing with `OutOfGas` if the remaining gas goes negative.
    fn gas_cost(&mut self, cost: B::Value) {
        if !self.config.gas_metering {
            return;
        }
        let remaining = self.load_gas_remaining();
        let remaining = self.bcx.isub(remaining, cost);
        let addr = self.gas_remaining_addr();
        self.bcx.store(remaining, addr);
        let out_of_gas = self.bcx.icmp_imm(IntCC::SignedLessThan, remaining, 0);
        self.build_check(out_of_gas, InstrStop::OutOfGas);
    }

    /// Ensures the memory is large enough for `offset + len` bytes, calling the `mresize`
    /// builtin on the cold path if needed. Returns the pointer to `mem_base + offset`.
    fn build_ensure_memory(&mut self, offset: B::Value, len: u64) -> B::Value {
        let (base, addend) = self.memory_offset_parts(offset);
        let end = addend + len;
        let ensured =
            self.ensured_memory.iter().find(|&&(b, ..)| b == base).map(|&(_, lo, hi)| (lo, hi));
        // An earlier access at `base + lo` passed its check, so that offset fits in 63 bits, and
        // `base + addend` for `addend >= lo` fits in 64 bits.
        let fits = ensured.is_some_and(|(lo, _)| lo <= addend);
        let offset = if fits {
            self.bcx.ireduce(self.isize_type, offset)
        } else {
            // 63 bits lets us avoid overflow in the addition below.
            self.u256_to_u64_saturating(offset, 63)
        };
        // An earlier access in this section already ensured this range.
        if fits && ensured.is_some_and(|(_, hi)| end <= hi) {
            return self.build_memory_addr(offset);
        }
        // Every path below either fails or leaves memory covering this access.
        if let Some((_, lo, hi)) = self.ensured_memory.iter_mut().find(|(b, ..)| *b == base) {
            *lo = (*lo).min(addend);
            *hi = (*hi).max(end);
        } else {
            self.ensured_memory.push((base, addend, end));
        }

        // Analysis proved this access is already covered on every path.
        if self.current_inst.is_some_and(|inst| self.can_skip_ensure_memory(inst)) {
            return self.build_memory_addr(offset);
        }

        let isize_type = self.isize_type;
        let len_const = self.bcx.iconst(isize_type, len as i64);
        let min_size = self.bcx.iadd(offset, len_const);

        let direct_resize_size = self
            .current_inst
            .map(|inst| self.bytecode.memory_section(inst).direct_resize_size)
            .unwrap_or_default();
        // Analysis proved this access always grows memory to exactly `min_size`.
        if direct_resize_size != 0 {
            self.call_fallible_builtin(Builtin::Mresize, &[self.ecx, min_size]);
            self.cached_mem_base = None;
            return self.build_memory_addr(offset);
        }

        // Otherwise emit the normal checked-resize branch.
        // Fast path: offset + len <= mem_len (no overflow).
        let mem_len_field = self.get_field(
            self.ecx,
            mem::offset_of!(EvmContext<'_, '_, '_>, mem_len),
            "ecx.mem_len.addr",
        );
        let mem_len = self.bcx.load(isize_type, mem_len_field, "ecx.mem_len");
        let exceeds = self.bcx.icmp(IntCC::UnsignedGreaterThan, min_size, mem_len);
        self.cached_mem_base = None;

        self.if_then(exceeds, |this| {
            this.call_fallible_builtin(Builtin::Mresize, &[this.ecx, min_size]);
        });
        self.build_memory_addr(offset)
    }

    /// Splits a value into a base value and a constant addend.
    fn memory_offset_parts(&self, offset: B::Value) -> (B::Value, u64) {
        self.value_offsets
            .iter()
            .rev()
            .find(|&&(sum, ..)| sum == offset)
            .map_or((offset, 0), |&(_, base, addend)| (base, addend))
    }

    fn can_skip_ensure_memory(&self, inst: Inst) -> bool {
        let section = self.bytecode.memory_section(inst);
        if section.known_size < section.required_size {
            return false;
        }
        let mut has_memory_access = false;
        for (offset, len) in self.bytecode.const_memory_accesses(inst).into_iter().flatten() {
            has_memory_access = true;
            if offset.is_none() || len.is_none() {
                return false;
            }
        }
        has_memory_access
    }

    fn build_memory_addr(&mut self, offset: B::Value) -> B::Value {
        let mem_base = self.cached_mem_base.unwrap_or_else(|| self.load_memory_base());
        self.cached_mem_base = Some(mem_base);
        self.bcx.gep(self.i8_type, mem_base, &[offset], "mem.addr")
    }

    fn load_memory_base(&mut self) -> B::Value {
        self.load_ecx_ptr(
            mem::offset_of!(EvmContext<'_, '_, '_>, mem_base),
            "ecx.mem_base.addr",
            "ecx.mem_base",
        )
    }

    /// Emits under/overflow bounds checks for a stack section.
    fn check_stack_bounds(&mut self, stack_section: StackSection) {
        let inp = stack_section.inputs;
        let diff = stack_section.max_growth as i64;

        let underflow = |this: &mut Self| {
            debug_assert!(inp > 0);
            this.bcx.icmp_imm(IntCC::UnsignedLessThan, this.bytes_before, inp as i64 * WORD_SIZE)
        };
        let overflow = |this: &mut Self| {
            debug_assert!(diff > 0);
            if diff > STACK_CAP as i64 {
                return this.bcx.bool_const(true);
            }
            let max = (STACK_CAP as i64 - diff) * WORD_SIZE;
            this.bcx.icmp_imm(IntCC::UnsignedGreaterThan, this.bytes_before, max)
        };

        let may_underflow = inp > 0;
        let may_overflow = diff > 0;
        if may_underflow && may_overflow {
            let underflow = underflow(self);
            let overflow = overflow(self);
            let cond = self.bcx.bitor(underflow, overflow);
            let ret = {
                let under = self.bcx.iconst(self.i8_type, InstrStop::StackUnderflow as i64);
                let over = self.bcx.iconst(self.i8_type, InstrStop::StackOverflow as i64);
                self.bcx.select(underflow, under, over)
            };
            let target = self.build_check_inner(true, cond, ret);
            self.bcx.switch_to_block(target);
        } else if may_underflow {
            let cond = underflow(self);
            self.build_check(cond, InstrStop::StackUnderflow);
        } else if may_overflow {
            let cond = overflow(self);
            self.build_check(cond, InstrStop::StackOverflow);
        }
    }

    /// Builds a check, failing if the condition is true.
    ///
    /// `if failure_cond { return ret } else { ... }`
    fn build_check(&mut self, failure_cond: B::Value, ret: InstrStop) {
        self.build_check_imm_inner(true, failure_cond, ret);
    }

    fn build_check_imm_inner(&mut self, is_failure: bool, cond: B::Value, ret: InstrStop) {
        let ret_value = self.bcx.iconst(self.i8_type, ret as i64);
        let target = self.build_check_inner(is_failure, cond, ret_value);
        if self.config.comments {
            self.add_comment(&format!("check {ret:?}"));
        }
        self.bcx.switch_to_block(target);
    }

    #[must_use]
    fn build_check_inner(
        &mut self,
        is_failure: bool,
        cond: B::Value,
        ret: B::Value,
    ) -> B::BasicBlock {
        let current_block = self.current_block();
        let target = self.create_block_after(current_block, "contd");

        let exit_block = if is_failure {
            if let Some(failure_block) = self.failure_block {
                self.incoming_failures.push((ret, current_block));
                failure_block
            } else {
                self.create_block_after(target, "failure")
            }
        } else if let Some(return_block) = self.return_block {
            self.incoming_returns.push((ret, current_block));
            return_block
        } else {
            self.create_block_after(target, "return")
        };
        let then_block = if is_failure { exit_block } else { target };
        let else_block = if is_failure { target } else { exit_block };
        self.bcx.brif_cold(cond, then_block, else_block, is_failure);

        if (is_failure && self.failure_block.is_none())
            || (!is_failure && self.return_block.is_none())
        {
            self.bcx.switch_to_block(exit_block);
            self.bcx.ret(&[ret]);
        }

        target
    }

    /// Builds a branch to the failure block.
    fn build_fail_imm(&mut self, ret: InstrStop) {
        let ret_value = self.bcx.iconst(self.i8_type, ret as i64);
        self.build_fail(ret_value);
        if self.config.comments {
            self.add_comment(&format!("fail {ret:?}"));
        }
    }

    /// Builds a branch to the failure block.
    fn build_fail(&mut self, ret: B::Value) {
        if self.config.inspect_stack {
            self.materialize_live_stack();
        }
        if let Some(block) = self.failure_block {
            self.incoming_failures.push((ret, self.bcx.current_block().unwrap()));
            self.bcx.br(block);
        } else {
            self.bcx.ret(&[ret]);
        }
    }

    /// Builds a branch to the return block.
    fn build_return_imm(&mut self, ret: InstrStop) {
        let ret_value = self.bcx.iconst(self.i8_type, ret as i64);
        self.build_return(ret_value);
        if self.config.comments {
            self.add_comment(&format!("return {ret:?}"));
        }
    }

    /// Builds a branch to the return block.
    fn build_return(&mut self, ret: B::Value) {
        self.build_gas_check();
        if self.config.inspect_stack {
            self.materialize_live_stack();
        }
        self.build_return_inner(ret);
    }

    fn build_return_inner(&mut self, ret: B::Value) {
        if let Some(block) = self.return_block {
            self.incoming_returns.push((ret, self.bcx.current_block().unwrap()));
            self.bcx.br(block);
        } else {
            self.bcx.ret(&[ret]);
        }
    }

    /// Builds the block that dispatches unresolved dynamic jumps to their target.
    fn build_dynamic_jump_table(&mut self, unreachable_block: B::BasicBlock) {
        self.bcx.switch_to_block(self.dynamic_jump_table);
        let i64_type = self.bcx.type_int(64);
        let i32_type = self.bcx.type_int(32);
        let pc = self.bcx.phi(i64_type, &self.incoming_dynamic_jumps);

        // A decoded JUMPDEST is not necessarily a valid jump target: EIP-8024
        // immediates can desynchronize decoding from the legacy JUMPDEST analysis,
        // so a `0x5b` masked by the analysis still decodes as an instruction. Only
        // analysis-valid targets belong in the dispatch table.
        let bytecode = self.bytecode;
        let jumpdests = bytecode
            .iter_insts()
            .filter(|(_, data)| {
                data.is_jumpdest() && bytecode.is_valid_jump(data.jumpdest_pc() as usize)
            })
            .map(|(inst, data)| (data.jumpdest_pc() as usize, inst))
            .collect::<Vec<_>>();

        let Some(&(max_pc, _)) = jumpdests.last() else {
            let invalid_jump = self.add_invalid_jump();
            self.bcx.br(invalid_jump);
            return;
        };

        // Switch on the target's rank among the valid targets instead of its sparse PC,
        // so that the switch lowers to a jump table. Ranks count down from the last target.
        // Each pair in the table covers 32 PCs: the number of targets in later pairs minus
        // one, then the bitmap of targets in this pair, with the first PC in the top bit.
        let mut table = vec![[0u64; 2]; max_pc / 32 + 1];
        let mut targets = Vec::with_capacity(jumpdests.len());
        for (i, &(pc, inst)) in jumpdests.iter().rev().enumerate() {
            table[pc / 32][1] |= 1 << (31 - pc % 32);
            targets.push((i as u64, self.effective_entry(inst)));
        }
        let mut rank = 0u32;
        for pair in table.iter_mut().rev() {
            pair[0] = rank.wrapping_sub(1).into();
            rank += pair[1].count_ones();
        }

        let in_range = self.bcx.icmp_imm(IntCC::UnsignedLessThanOrEqual, pc, max_pc as i64);
        let lookup = self.bcx.create_block("jump_table.lookup");
        let invalid_jump = self.add_invalid_jump();
        self.bcx.brif_cold(in_range, lookup, invalid_jump, false);

        self.bcx.switch_to_block(lookup);
        let five = self.bcx.iconst(i64_type, 5);
        let pair_index = self.bcx.ushr(pc, five);
        let table = self.bcx.const_array(i32_type, table.as_flattened());
        let pair_type = self.bcx.type_array(i32_type, 2);
        // The base is the pair's first field, so the pair's address is also the base's.
        let base_ptr = self.bcx.gep(pair_type, table, &[pair_index], "jump_table.base.addr");
        let base = self.bcx.load(i32_type, base_ptr, "jump_table.base");
        let one = self.bcx.iconst(i32_type, 1);
        let bitmap_ptr =
            self.bcx.gep(pair_type, table, &[pair_index, one], "jump_table.bitmap.addr");
        let bitmap = self.bcx.load(i32_type, bitmap_ptr, "jump_table.bitmap");
        // Shift the target's bit into the sign bit, dropping the bits of earlier PCs.
        let shift = self.bcx.ireduce(i32_type, pc);
        let shift = self.bcx.bitand_imm(shift, 31);
        let bits = self.bcx.ishl(bitmap, shift);
        let is_valid = self.bcx.icmp_imm(IntCC::SignedLessThan, bits, 0);
        let dispatch = self.bcx.create_block("jump_table.dispatch");
        let invalid_jump = self.add_invalid_jump();
        self.bcx.brif_cold(is_valid, dispatch, invalid_jump, false);

        self.bcx.switch_to_block(dispatch);
        let at_or_after = self.bcx.popcnt(bits);
        let index = self.bcx.iadd(base, at_or_after);
        self.bcx.switch(index, unreachable_block, &targets, false);
    }

    fn add_invalid_jump(&mut self) -> B::BasicBlock {
        let block = self.failure_block.unwrap();
        self.incoming_failures.push((
            self.bcx.iconst(self.i8_type, InstrStop::InvalidJump as i64),
            self.bcx.current_block().unwrap(),
        ));
        block
    }

    /// Build a call to a fallible builtin.
    ///
    /// The builtin longjmps on error, so no return value check is needed.
    fn call_fallible_builtin(&mut self, builtin: Builtin, args: &[B::Value]) {
        self.build_gas_check();
        let _ = self.call_builtin(builtin, args);
    }

    /// Build a call to a builtin.
    #[must_use]
    fn call_builtin(&mut self, builtin: Builtin, args: &[B::Value]) -> Option<B::Value> {
        let function = self.builtin_function(builtin);
        let invalidate_mem_base =
            !self.current_inst.is_some_and(|inst| self.can_skip_ensure_memory(inst));
        let value = self.bcx.call(function, args);
        if invalidate_mem_base {
            self.cached_mem_base = None;
        }
        // Builtins may write the current instruction's operands and outputs on the physical
        // stack.
        if let Some(inst) = self.current_inst {
            let inputs = self.bytecode.inst(inst).stack_io().0 as i32;
            self.vstack.forget_stored_from(self.section_len_offset - inputs);
        }
        value
    }

    /// Gets the function for the given builtin.
    fn builtin_function(&mut self, builtin: Builtin) -> B::Function {
        self.builtins.get(builtin, &mut self.bcx)
    }

    /// Adds a comment to the current instruction.
    fn add_comment(&mut self, comment: &str) {
        if comment.is_empty() || !self.config.comments {
            return;
        }
        self.bcx.add_comment_to_current_inst(comment);
    }

    /// Returns the current instruction.
    fn current_inst(&self) -> &InstData {
        self.bytecode.inst(self.current_inst.unwrap())
    }

    fn if_then(&mut self, cond: B::Value, then: impl FnOnce(&mut Self)) {
        let current_block = self.current_block();
        let then_block = self.create_block_after(current_block, "then");
        let done_block = self.create_block_after(then_block, "contd");

        self.bcx.brif(cond, then_block, done_block);

        self.bcx.switch_to_block(then_block);
        // A gas check on this path does not cover the path that skips it.
        let unchecked_gas = self.unchecked_gas;
        then(self);
        self.unchecked_gas = unchecked_gas;
        self.bcx.br(done_block);

        self.bcx.switch_to_block(done_block);
        self.joins += 1;
    }

    /// Returns the current block.
    fn current_block(&mut self) -> B::BasicBlock {
        // There always is a block present.
        self.bcx.current_block().expect("no blocks")
    }

    /// Creates a named block after the given block.
    fn create_block_after(&mut self, after: B::BasicBlock, name: &str) -> B::BasicBlock {
        let name = self.op_block_name(name);
        self.bcx.create_block_after(after, &name)
    }

    /// Returns the block name for the current opcode with the given suffix.
    fn op_block_name(&self, name: &str) -> String {
        if !self.config.debug {
            return String::new();
        }
        self.bytecode.op_block_name(self.current_inst, name)
    }

    /// Converts a 256-bit unsigned integer to a 64-bit unsigned integer, saturating at
    /// `2^bits - 1`.
    fn u256_to_u64_saturating(&mut self, value: B::Value, bits: usize) -> B::Value {
        let i64_type = self.bcx.type_int(64);
        let reduced = self.bcx.ireduce(i64_type, value);
        let sentinel_lit = 1u128.checked_shl(bits as u32).unwrap_or(0).wrapping_sub(1);
        let sentinel_u256 = self.bcx.iconst_256(U256::from(sentinel_lit));
        let fits = self.bcx.icmp(IntCC::UnsignedLessThanOrEqual, value, sentinel_u256);
        let sentinel = self.bcx.iconst(i64_type, sentinel_lit as i64);
        self.bcx.select(fits, reduced, sentinel)
    }
}

fn get_field<B: Builder>(bcx: &mut B, ptr: B::Value, offset: usize, name: &str) -> B::Value {
    // A field at offset 0 is `ptr` itself; a zero GEP would only clutter the IR.
    if offset == 0 {
        return ptr;
    }
    let offset = bcx.iconst(bcx.type_ptr_sized_int(), offset as i64);
    bcx.gep(bcx.type_int(8), ptr, &[offset], name)
}
