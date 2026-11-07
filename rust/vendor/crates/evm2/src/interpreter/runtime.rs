use super::{
    BytecodeRef, Gas, Host, InstrStop, Memory, Message, MessageInput, Pc, Result, StackBacking,
    StackMut, StackRef, Word,
};
use crate::{
    EvmTypesHost, ExecutionConfig, ExecutionError, HostError, SpecId, Version,
    bytecode::Bytecode,
    evm::inspector::{Inspector, OpcodeSet},
    interpreter::{
        dispatch::{self, InstrTable},
        opcode::OpCode,
    },
    trustme,
    version::{EvmFeatures, GasParams},
};
use alloc::{boxed::Box, format, vec::Vec};
use alloy_primitives::{Address, B256, Bytes};
use core::{fmt, hint::cold_path, ops::Range, ptr::NonNull};
use derive_where::derive_where;

/// Instruction table for a sparse set of stepped opcodes.
struct SparseTable<T: EvmTypesHost> {
    /// The stepped opcodes.
    steps: OpcodeSet,
    /// The config's inspect table the entries at `steps` came from.
    source: *const InstrTable<T>,
    instructions: InstrTable<T>,
}

/// EVM interpreter.
#[derive_where(Debug)]
pub struct Interpreter<'frame, 'host, T: EvmTypesHost> {
    // Borrowed from the frame's message rather than cloned: a per-frame `Arc` clone writes the
    // shared refcount next to the read-mostly bytecode, which bounces between last-level caches
    // when threads on several of them run the same contracts.
    pub(in crate::interpreter) bytecode: &'frame Bytecode,
    // Borrows the immutable allocations of `bytecode`. Cleared before replacing it; accessors
    // must shorten the erased lifetime to the borrow of this interpreter.
    bytecode_ref: Option<BytecodeRef<'static>>,
    pub(in crate::interpreter) memory: Memory,
    pub(in crate::interpreter) return_data: Bytes,

    pub(in crate::interpreter) pc: *const u8,
    output: Range<u32>,
    #[derive_where(skip)]
    message: Option<&'frame Message<T>>,
    host: Option<NonNull<T::Host<'host>>>,
    inspector: Option<NonNull<dyn Inspector<T> + 'host>>,
    /// Opcodes the inspector steps on the table this frame runs on.
    steps: OpcodeSet,
    /// Whether the running instruction is `hooked`.
    hooked_running: bool,
    /// The last sparse inspected run's step set, the config table it was built from, and the
    /// table.
    #[derive_where(skip)]
    inspect_table: Option<Box<SparseTable<T>>>,
    pub(in crate::interpreter) stack_len: usize,
    #[derive_where(skip)]
    pub(in crate::interpreter) stack: Box<StackBacking>,

    pub(in crate::interpreter) gas: Gas,
    pub(in crate::interpreter) result: Result,
    error: Option<ExecutionError>,
    spec: SpecId,
    features: EvmFeatures,
    is_static: bool,
    /// Whether [`Self::call_input`] borrows memory. Set only for frames that [`Evm`](crate::Evm)
    /// runs, whose host keeps a borrowed input from being read after the call or while the memory
    /// changes.
    lends_memory: bool,
}

// SAFETY: The interpreter's internal pointers are always valid. `pc` and `bytecode_ref` point into
// the frame's immutable bytecode and its jump table. Frame-local references are cleared before
// pooling, and host/inspector pointers are installed for
// execution and not used after the owning execution context is gone. The `Sync` bounds make the
// retained shared frame references safe to transfer between threads.
unsafe impl<T> Send for Interpreter<'_, '_, T>
where
    T: EvmTypesHost,
    T::MessageExt: Sync,
{
}

impl<'frame, 'host, T: EvmTypesHost> Interpreter<'frame, 'host, T> {
    /// Creates an interpreter from a frame-local message, which carries the bytecode to run.
    pub fn new(message: &'frame Message<T>) -> Self {
        // SAFETY: Calling init right after.
        let mut interp = unsafe { Self::uninit() };
        interp.init(message);
        interp
    }

    /// # Safety
    ///
    /// Must call `init` before use.
    unsafe fn uninit() -> Self {
        let bytecode = <&Bytecode>::default();
        Self {
            pc: bytecode.original_byte_slice().as_ptr(),
            bytecode,
            bytecode_ref: None,
            stack_len: 0,
            gas: Gas::new(0),
            memory: Memory::new(),
            result: Ok(()),
            error: None,
            output: 0..0,
            message: None,
            is_static: false,
            lends_memory: false,
            return_data: Bytes::new(),
            host: None,
            inspector: None,
            steps: OpcodeSet::EMPTY,
            hooked_running: false,
            inspect_table: None,
            spec: SpecId::DEFAULT,
            features: EvmFeatures::empty(),
            // SAFETY: `MaybeUninit<Word>` does not need initialization.
            stack: unsafe { Box::new_uninit().assume_init() },
        }
    }

    /// Initializes this interpreter for a new frame, retaining reusable allocations.
    fn init(&mut self, message: &'frame Message<T>) {
        let gas_limit = message.gas_limit;
        let is_static = message.is_static();
        self.set_bytecode(&message.code);
        self.stack_len = 0;
        self.gas = Gas::new_with_execution_gas_and_reservoir(gas_limit, message.reservoir);
        self.memory.clear();
        self.result = Ok(());
        self.error = None;
        self.output = 0..0;
        self.message = Some(message);
        self.is_static = is_static;
        self.lends_memory = false;
        self.return_data = Bytes::new();
    }

    /// Points the interpreter at `bytecode` and resets the program counter and the cached view.
    pub(crate) fn set_bytecode(&mut self, bytecode: &'frame Bytecode) {
        self.pc = bytecode.bytes_slice().as_ptr();
        self.bytecode_ref = None;
        self.bytecode = bytecode;
    }

    pub(crate) fn clear_frame_refs(&mut self) {
        self.bytecode = Default::default();
        self.bytecode_ref = None;
        self.message = None;
        self.host = None;
        self.inspector = None;
    }

    #[cfg(test)]
    pub(crate) fn into_parts(self) -> (Box<StackBacking>, usize, Gas, Memory, Range<u32>) {
        (self.stack, self.stack_len, self.gas, self.memory, self.output)
    }

    /// Returns output produced by `RETURN` or `REVERT`.
    #[inline]
    pub fn output(&self) -> &[u8] {
        let start = self.output.start as usize;
        self.memory.slice(start, self.output.len())
    }

    /// Returns the current frame output memory range.
    #[inline]
    pub const fn output_range(&self) -> &Range<u32> {
        &self.output
    }

    /// Sets the current frame output memory range.
    #[inline]
    pub const fn set_output(&mut self, output: Range<u32>) {
        self.output = output;
    }

    /// Returns the current bytecode-relative program counter.
    #[inline]
    pub fn pc(&self) -> usize {
        // SAFETY: `pc` is always in bounds of `bytecode`.
        unsafe { self.pc.offset_from(self.bytecode.original_byte_slice().as_ptr()) as usize }
    }

    /// Sets the current bytecode-relative program counter.
    ///
    /// # Panics
    ///
    /// Panics if `pc` is out of bounds of the active bytecode.
    #[inline]
    pub fn set_pc(&mut self, pc: usize) {
        let bytecode = self.bytecode.bytes_slice();
        assert!(pc < bytecode.len());
        self.pc = unsafe { bytecode.as_ptr().add(pc) };
    }

    /// Returns the current opcode.
    #[inline]
    pub const fn opcode(&self) -> u8 {
        // SAFETY: `pc` is always in bounds of `bytecode`.
        unsafe { *self.pc }
    }

    /// Returns the active bytecode.
    #[inline]
    pub fn bytecode(&self) -> BytecodeRef<'_> {
        self.bytecode_ref.unwrap_or_else(|| BytecodeRef::new(self.bytecode))
    }

    /// Returns the original active bytecode bytes.
    #[inline]
    pub fn original_bytecode(&self) -> Bytes {
        self.bytecode.original_bytes()
    }

    /// Returns the original active bytecode as a byte slice.
    #[inline]
    pub fn original_bytecode_slice(&self) -> &[u8] {
        self.bytecode.original_byte_slice()
    }

    /// Calculates or returns the cached hash of the original active bytecode.
    #[inline]
    pub fn original_bytecode_hash(&self) -> B256 {
        self.bytecode.hash_slow()
    }

    /// Returns the current operand stack.
    #[inline]
    pub const fn stack(&self) -> StackRef<'_> {
        StackRef::new(&self.stack, self.stack_len)
    }

    /// Returns the current mutable operand stack.
    #[inline]
    pub const fn stack_mut(&mut self) -> StackMut<'_> {
        StackMut { stack: &mut self.stack, len: &mut self.stack_len }
    }

    /// Returns the current gas state.
    #[inline]
    pub const fn gas(&self) -> Gas {
        self.gas
    }

    /// Returns a reference to the current gas state.
    #[inline]
    pub const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.gas
    }

    /// Sets the current interpreter gas state.
    #[inline]
    pub const fn set_gas(&mut self, gas: Gas) {
        self.gas = gas;
    }

    /// Returns the current linear memory.
    #[inline]
    pub const fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Returns the current mutable linear memory.
    #[inline]
    pub const fn memory_mut(&mut self) -> &mut Memory {
        &mut self.memory
    }

    /// Returns `len` bytes of memory at `offset` as the input of a call message, borrowed when
    /// this frame is run by [`Evm`](crate::Evm) and copied otherwise.
    ///
    /// # Safety
    ///
    /// The input must only be read as the input of a message passed to this frame's
    /// [`Host::execute_message`](crate::interpreter::Host::execute_message), and memory must not
    /// change and no other code may access this frame in between. It must not be read after
    /// `execute_message` returns.
    ///
    /// # Panics
    ///
    /// Panics if the bytes are out of bounds of memory.
    #[inline]
    #[doc(hidden)]
    pub unsafe fn call_input(&self, offset: usize, len: usize) -> MessageInput {
        let bytes = self.memory.slice(offset, len);
        if !self.lends_memory {
            return Bytes::copy_from_slice(bytes).into();
        }
        // SAFETY: The caller keeps memory unchanged until the host returns, and the host is
        // `Evm`, which copies the input before any inspector hook can reach this frame or the
        // message. Otherwise it only reads the message through shared references, which cannot
        // move the input out, and cloning copies it.
        unsafe { MessageInput::borrowed(bytes) }
    }

    /// Lets [`Self::call_input`] borrow memory for the rest of this frame.
    #[inline]
    pub(crate) const fn lend_memory(&mut self) {
        self.lends_memory = true;
    }

    /// Returns the current interpreter result.
    #[inline]
    pub const fn result(&self) -> Result {
        self.result
    }

    /// Sets the interpreter result.
    #[inline]
    pub const fn set_result(&mut self, result: Result) {
        self.result = result;
    }

    /// Sets the current instruction result to `stop`.
    #[inline]
    pub const fn set_stop(&mut self, stop: InstrStop) {
        self.set_result(Err(stop));
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized before inspected execution starts.
        unsafe { self.message.unwrap_unchecked() }
    }

    #[inline]
    #[doc(hidden)]
    pub const fn message_field_offset_for_jit() -> usize {
        core::mem::offset_of!(Self, message)
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.return_data
    }

    /// Returns a mutable reference to return data from the last call-like operation.
    #[inline]
    pub const fn return_data_mut(&mut self) -> &mut Bytes {
        &mut self.return_data
    }

    /// Borrows the operand stack, linear memory, and host independently for inspection.
    #[inline]
    pub const fn stack_memory_host(&mut self) -> (StackRef<'_>, &[u8], &mut T::Host<'host>) {
        // SAFETY: As in `host`, the host pointer is initialized during execution and points
        // outside the interpreter's owned stack and memory.
        let host = unsafe { self.host.unwrap_unchecked().as_mut() };
        (self.stack(), self.memory.as_slice(), host)
    }

    /// Borrows linear memory and the host independently, so memory can be passed to the host
    /// without copying.
    #[inline]
    pub const fn memory_host(&mut self) -> (&Memory, &mut T::Host<'host>) {
        // SAFETY: As in `host`, the host pointer is initialized during execution and points
        // outside the interpreter's owned memory.
        let host = unsafe { self.host.unwrap_unchecked().as_mut() };
        (&self.memory, host)
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host<'host> {
        // SAFETY: `host` is initialized at the beginning of inspected execution.
        unsafe { self.host.unwrap_unchecked().as_mut() }
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// Returns the active runtime version data.
    #[inline]
    pub fn version(&self) -> &Version {
        // SAFETY: The result is tied to `&self`, which excludes host calls through `self`.
        unsafe { self.version_detached() }
    }

    /// Returns the active runtime version data without borrowing the interpreter.
    ///
    /// # Safety
    ///
    /// The host must be initialized, and the result must not be used after the host is borrowed
    /// mutably.
    #[inline]
    unsafe fn version_detached(&self) -> &'host Version {
        unsafe { self.execution_config_detached() }.version()
    }

    /// Returns the active execution config without borrowing the interpreter.
    ///
    /// # Safety
    ///
    /// Same as [`Self::version_detached`].
    #[inline]
    unsafe fn execution_config_detached(&self) -> &'host ExecutionConfig<T> {
        unsafe { self.host.unwrap_unchecked().as_ref() }.execution_config()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.is_static
    }

    /// Sets the static-call flag for external execution adapters.
    #[inline]
    pub const fn set_static(&mut self, is_static: bool) {
        self.is_static = is_static;
    }

    /// Converts a host failure into an instruction stop, retaining external errors only in this
    /// frame.
    #[inline]
    pub fn fail(&mut self, error: impl Into<HostError>) -> InstrStop {
        match error.into() {
            HostError::Halt(stop) => stop,
            HostError::Execution(error) => {
                self.error = Some(error);
                InstrStop::FatalExternalError
            }
        }
    }

    /// Finishes a backend run, returning its owned error and clearing execution references.
    pub(crate) fn finish_run(&mut self, stop: InstrStop) -> Result<InstrStop, ExecutionError> {
        self.host = None;
        self.inspector = None;
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        if stop.is_fatal() {
            return Err(ExecutionError::Fatal(
                "interpreter returned a fatal stop without an error".into(),
            ));
        }
        Ok(stop)
    }

    /// Runs the interpreter until it stops.
    #[inline]
    pub fn run(&mut self, host: &mut T::Host<'host>) -> Result<InstrStop, ExecutionError> {
        self.run_inner(host, None)
    }

    /// Runs the interpreter until it stops with an execution inspector.
    #[inline]
    pub fn run_inspect(
        &mut self,
        host: &mut T::Host<'host>,
        inspector: &mut (dyn Inspector<T> + 'host),
    ) -> Result<InstrStop, ExecutionError> {
        self.run_inner(host, Some(NonNull::from(inspector)))
    }

    /// Prepares this interpreter for external execution.
    #[inline]
    #[doc(hidden)]
    pub fn prepare_run(&mut self, host: &mut T::Host<'host>) {
        if self.bytecode_ref.is_none() {
            // SAFETY: The view borrows immutable allocations of the frame's `Bytecode`, which
            // outlives the interpreter's `'frame` borrow, so moving the interpreter does not
            // invalidate it. `init` and `clear_frame_refs` clear the view before replacing the
            // borrow, and accessors tie the returned view to `&self`.
            let bytecode = unsafe { trustme::decouple_lt(self.bytecode) };
            self.bytecode_ref = Some(BytecodeRef::new(bytecode));
        }
        let config = host.execution_config();
        let version = config.version();
        self.memory.set_memory_limit(version.memory_limit);
        self.spec = config.base_spec_id();
        self.features = version.features;
        self.host = Some(NonNull::from(host));
        self.inspector = None;
    }

    /// Runs the host's instruction implementation for the opcode at `pc` on the interpreter
    /// stack, which holds `stack_len` words, and returns the new stack length.
    ///
    /// Gas is read from and written to this interpreter, so the caller must keep it current.
    ///
    /// # Safety
    ///
    /// The first `stack_len` words of the stack must be initialized.
    ///
    /// # Panics
    ///
    /// Panics if [`prepare_run`](Self::prepare_run) has not run, or if `pc` or `stack_len` is out
    /// of bounds.
    #[doc(hidden)]
    pub unsafe fn run_external_instruction(
        &mut self,
        pc: usize,
        stack_len: usize,
    ) -> core::result::Result<usize, InstrStop> {
        let bytecode = self.bytecode_ref.expect("`prepare_run` must precede external instructions");
        assert!(pc < self.bytecode.original_byte_slice().len(), "pc out of bounds");
        assert!(stack_len <= StackMut::CAPACITY, "stack length out of bounds");
        let host = self.host.expect("`prepare_run` must precede external instructions");
        // SAFETY: `prepare_run` installed the host, which outlives the run.
        let config = unsafe { host.as_ref() }.execution_config();
        // SAFETY: `pc` is in bounds.
        let offset = pc;
        let mut pc = Pc::new(unsafe { bytecode.as_ptr().add(offset) });
        let instr = config.opcode_config().instruction(pc.op()).instr;
        self.stack_len = stack_len;
        let this: *mut Self = self;
        // SAFETY: Instructions must not access the stack through `InterpreterState` while the
        // separate stack view is live, as in the dispatch loop.
        let (state, mut stack) = unsafe {
            (
                InterpreterState::wrap_mut(&mut *this),
                StackMut { stack: &mut (*this).stack, len: &mut (*this).stack_len },
            )
        };
        let start = pc.as_ptr();
        instr(&mut pc, stack.reborrow(), state)?;
        // Compiled code continues at the next instruction.
        if pc.as_ptr() != start {
            return Err(self.fail_host_instruction(
                offset,
                format_args!("moved the program counter, which compiled code cannot follow"),
            ));
        }
        Ok(self.stack_len)
    }

    /// Records a fatal error for the host instruction at `pc` that compiled code cannot continue
    /// after.
    ///
    /// # Panics
    ///
    /// Panics if `pc` is out of bounds.
    #[doc(hidden)]
    pub fn fail_host_instruction(&mut self, pc: usize, detail: fmt::Arguments<'_>) -> InstrStop {
        let opcode = OpCode::new_or_unknown(self.original_bytecode_slice()[pc]);
        self.fail(ExecutionError::Fatal(
            format!(
                "host instruction {} (0x{:02x}) at pc {pc} {detail}",
                opcode.as_str(),
                opcode.get()
            )
            .into(),
        ))
    }

    #[inline(never)]
    fn run_inner(
        &mut self,
        host: &mut T::Host<'host>,
        inspector: Option<NonNull<dyn Inspector<T> + 'host>>,
    ) -> Result<InstrStop, ExecutionError> {
        self.prepare_run(host);
        self.inspector = inspector;

        let stop = if self.error.is_some() {
            InstrStop::FatalExternalError
        } else {
            // Load the table only after setup: passing it in keeps it in a callee-saved register
            // across setup, which makes the dispatch loop spill the stack pointer.
            match inspector {
                Some(_) => self.run_inspected(),
                None => {
                    // SAFETY: `prepare_run` initialized the host, and the config is only read
                    // here.
                    let config = unsafe { self.execution_config_detached() };
                    dispatch::run(self, config.instructions)
                }
            }
        };
        self.finish_run(stop)
    }

    /// Runs the frame on the table for its inspector's config, and again on the table for the new
    /// config each time a child message changes it.
    ///
    /// Only `self` is live across the dispatch call, which recurses once per nested frame.
    #[inline(always)]
    fn run_inspected(&mut self) -> InstrStop {
        loop {
            // A hook before the run, such as `initialize_interp`, may have stopped the frame.
            if let Err(stop) = self.result {
                cold_path();
                return stop;
            }
            let instructions = self.select_inspect_table();
            // SAFETY: The table is a static one, or the one `self` holds until the next selection.
            let instructions = unsafe { &*instructions };
            let stop = if self.steps == OpcodeSet::ALL {
                dispatch::run(self, instructions)
            } else {
                self.run_inspected_table(instructions)
            };
            if stop != InstrStop::Reload {
                return stop;
            }
            cold_path();
            self.result = Ok(());
            // SAFETY: The stop left the pc at the message instruction, which a next one follows.
            self.pc = unsafe { self.pc.add(dispatch::instruction_len(*self.pc)) };
        }
    }

    /// Runs a frame that steps some opcodes, or none. Behind a call, so that its loops are not
    /// copies inlined into `run_inner`, where they compile worse than the loop for no inspector.
    #[inline(never)]
    fn run_inspected_table(&mut self, instructions: &InstrTable<T>) -> InstrStop {
        dispatch::run(self, instructions)
    }

    /// Records the opcodes the inspector steps now and returns the table for them. Not inlined, so
    /// that its temporaries stay out of the frame of `run_inspected`.
    #[inline(never)]
    fn select_inspect_table(&mut self) -> *const InstrTable<T> {
        // SAFETY: `prepare_run` initialized the host. The config is read again on each reload and
        // only its static tables are used, so nothing outlives a replaced config.
        let config = unsafe { self.execution_config_detached() };
        // SAFETY: `run_inner` installed the inspector, which is valid for this run.
        let inspector = unsafe { self.inspector.unwrap_unchecked().as_ref() };
        self.steps = inspector.inspector_config().steps();
        if self.steps == OpcodeSet::EMPTY {
            return config.instructions;
        }
        if self.steps == OpcodeSet::ALL {
            return config.inspect_instructions;
        }
        // A sparse set runs the plain handlers, with the inspect table's at the opcodes in the
        // set. The table lives in this interpreter and is rebuilt when the set or the config
        // differs from the previous run's, so the pooled interpreters of a deep call stack mostly
        // reuse theirs.
        let source = core::ptr::from_ref(config.inspect_instructions);
        let table = self.inspect_table.get_or_insert_with(|| {
            Box::new(SparseTable {
                steps: OpcodeSet::EMPTY,
                source: core::ptr::null(),
                instructions: *config.instructions,
            })
        });
        if table.steps != self.steps || table.source != source {
            table.steps = self.steps;
            table.source = source;
            table.instructions = *config.instructions;
            for op in 0..=u8::MAX {
                if self.steps.contains(op) {
                    table.instructions[op as usize] = config.inspect_instructions[op as usize];
                }
            }
        }
        &raw const table.instructions
    }

    /// Takes an owned error recorded by an instruction or inspector hook.
    pub(crate) const fn take_error(&mut self) -> Option<ExecutionError> {
        self.error.take()
    }
}

/// Interpreter state exposed to instruction implementations.
#[repr(transparent)]
pub struct InterpreterState<'frame, 'host, T: EvmTypesHost>(
    pub(crate) Interpreter<'frame, 'host, T>,
);

impl<T: EvmTypesHost> fmt::Debug for InterpreterState<'_, '_, T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'frame, 'host, T: EvmTypesHost> InterpreterState<'frame, 'host, T> {
    /// Converts a host failure into an instruction stop in this frame.
    #[inline]
    pub fn fail(&mut self, error: impl Into<HostError>) -> InstrStop {
        self.0.fail(error)
    }

    #[inline]
    pub(crate) const fn wrap_mut<'a>(
        interp: &'a mut Interpreter<'frame, 'host, T>,
    ) -> &'a mut Self {
        // SAFETY: `InterpreterState` is a transparent wrapper over `Interpreter`.
        unsafe { core::mem::transmute::<&mut Interpreter<'frame, 'host, T>, &mut Self>(interp) }
    }

    #[inline]
    pub(in crate::interpreter) const unsafe fn gas_from_state_ptr(state: *mut Self) -> *mut Gas {
        // SAFETY: The caller upholds that `state` points to a valid interpreter state.
        unsafe { &raw mut (*state).0.gas }
    }

    #[inline]
    pub(crate) const fn gas_mut(&mut self) -> &mut Gas {
        &mut self.0.gas
    }

    #[inline(always)]
    pub(crate) const fn set_result(&mut self, result: Result) {
        self.0.result = result;
    }

    #[inline]
    pub(crate) const fn result(&self) -> Result {
        self.0.result
    }

    #[inline]
    #[cfg(not(tco))]
    pub(crate) const fn is_inspecting(&self) -> bool {
        self.0.inspector.is_some()
    }

    /// Returns the opcodes the inspector steps in this frame.
    #[inline]
    #[cfg(not(tco))]
    pub(crate) const fn steps(&self) -> OpcodeSet {
        self.0.steps
    }

    /// Stops the instruction with [`InstrStop::Reload`] if the inspector's config no longer
    /// matches the table this frame runs on. A message instruction calls it after its child
    /// message and returns the result.
    #[inline(always)]
    pub(crate) fn note_inspector_config_change(&mut self) {
        if self.0.inspector.is_some() {
            self.reload_if_config_changed();
        }
    }

    #[inline(never)]
    fn reload_if_config_changed(&mut self) {
        assert!(self.0.hooked_running, "an instruction that runs a message must be `hooked`");
        if self.0.result.is_ok()
            && let Some(inspector) = self.0.inspector
            // SAFETY: The inspector pointer is valid while the frame executes.
            && unsafe { inspector.as_ref() }.inspector_config().steps() != self.0.steps
        {
            self.set_result(Err(InstrStop::Reload));
        }
    }

    /// Records whether the running instruction is `hooked`, so that the dispatch stored the latest
    /// state into the interpreter before it ran.
    #[inline]
    pub(crate) const fn set_hooked_running(&mut self, running: bool) {
        self.0.hooked_running = running;
    }

    #[inline]
    pub(crate) const fn set_pc_stack_len(&mut self, pc: *const u8, stack_len: usize) {
        self.0.pc = pc;
        self.0.stack_len = stack_len;
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec(&self) -> SpecId {
        self.0.spec
    }

    /// Returns the active bytecode.
    #[inline]
    pub const fn bytecode(&self) -> BytecodeRef<'_> {
        // SAFETY: `prepare_run` initializes the view before instruction execution. It remains
        // valid across call/resume cycles and is only cleared when initializing a new frame.
        unsafe { self.0.bytecode_ref.unwrap_unchecked() }
    }

    /// Returns the host implementation.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host<'host> {
        self.0.host()
    }

    /// Returns the active runtime version data.
    #[inline]
    pub fn version(&self) -> &Version {
        self.0.version()
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        // SAFETY: `message` is initialized at the beginning of `run` and remains set for
        // instruction execution.
        unsafe { self.0.message.unwrap_unchecked() }
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.0.is_static
    }

    /// Returns `true` if the active feature set contains `feature`.
    #[inline]
    pub const fn feature(&self, feature: EvmFeatures) -> bool {
        self.0.features.contains(feature)
    }

    /// Returns the active dynamic gas parameters.
    #[inline]
    pub fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns the active dynamic gas parameters without borrowing the interpreter state.
    ///
    /// # Safety
    ///
    /// The result must not be used after the host is borrowed mutably.
    #[inline]
    pub(in crate::interpreter) unsafe fn gas_params_detached(&self) -> &'host GasParams {
        unsafe { &self.0.version_detached().gas_params }
    }

    /// Returns linear memory.
    #[inline]
    pub const fn memory(&mut self) -> &mut Memory {
        &mut self.0.memory
    }

    /// Resizes linear memory using the active runtime gas parameters.
    #[inline]
    pub fn resize_memory(&mut self, gas: &mut Gas, offset: usize, len: usize) -> Result {
        // SAFETY: Resizing memory makes no host calls.
        let gas_params = unsafe { self.gas_params_detached() };
        self.0.memory.resize_evm(gas, gas_params, offset, len)
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        &self.0.return_data
    }

    /// Returns a mutable reference to return data from the last call-like operation.
    #[inline]
    pub const fn return_data_mut(&mut self) -> &mut Bytes {
        &mut self.0.return_data
    }

    /// Clears return data from the last call-like operation.
    #[inline]
    pub fn clear_return_data(&mut self) {
        self.0.return_data.clear();
    }

    /// Swaps return data from the last call-like operation.
    #[inline]
    pub(crate) const fn swap_return_data(&mut self, return_data: &mut Bytes) {
        core::mem::swap(&mut self.0.return_data, return_data);
    }

    /// Returns the current frame output memory range.
    #[inline]
    pub const fn output_range(&self) -> &Range<u32> {
        &self.0.output
    }

    /// Sets the current frame output memory range.
    #[inline]
    pub const fn set_output(&mut self, output: Range<u32>) {
        self.0.output = output;
    }

    #[inline]
    pub(crate) fn inspect_step(&mut self, pc: Pc, stack_len: usize) {
        self.set_pc_stack_len(pc.as_ptr(), stack_len);
        unsafe {
            let mut inspector = self.0.inspector.unwrap_unchecked();
            inspector.as_mut().step(&mut self.0);
        }
    }

    #[inline]
    pub(crate) fn inspect_step_end(&mut self, pc: Pc, stack_len: usize) {
        self.set_pc_stack_len(pc.as_ptr(), stack_len);
        let reload = self.0.result == Err(InstrStop::Reload);
        if reload {
            cold_path();
            // The hook sees the instruction as done, with the pc after it.
            self.set_result(Ok(()));
            // SAFETY: A next instruction follows the instruction at `pc`.
            self.0.pc = unsafe { pc.as_ptr().add(dispatch::instruction_len(pc.op())) };
        } else if self.0.result.is_err() {
            cold_path();
            if self.0.result.is_err_and(InstrStop::is_out_of_gas) {
                // An out-of-gas frame has burned all its gas, so 0 is the real remaining gas.
                self.0.gas.set_remaining(0);
            }
        }
        unsafe {
            let mut inspector = self.0.inspector.unwrap_unchecked();
            inspector.as_mut().step_end(&mut self.0);
        }
        // A stop the hook set wins over the reload.
        if reload {
            self.0.pc = pc.as_ptr();
            if self.0.result.is_ok() {
                self.set_result(Err(InstrStop::Reload));
            }
        }
    }

    #[inline]
    pub(crate) fn inspect_selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &Word,
    ) {
        if let Some(mut inspector) = self.0.inspector {
            unsafe {
                let mut host = self.0.host.unwrap_unchecked();
                inspector.as_mut().selfdestruct(contract, target, value, host.as_mut());
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct InterpreterPool<T: EvmTypesHost> {
    frames: Vec<Box<Interpreter<'static, 'static, T>>>,
}

impl<T: EvmTypesHost> InterpreterPool<T> {
    pub(crate) const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    pub(crate) fn pop<'frame, 'host>(
        &mut self,
        message: &'frame Message<T>,
    ) -> Box<Interpreter<'frame, 'host, T>> {
        let frame = match self.frames.pop() {
            Some(mut frame) => {
                frame.init(message);
                frame
            }
            None => Box::new(Interpreter::new(message)),
        };
        // SAFETY: Frames stored in the pool have their frame-local references cleared before they
        // are erased to `'static`. Rebinding the lifetime is only used to initialize the next
        // frame.
        unsafe { trustme::decouple_lt_box(frame) }
    }

    pub(crate) fn push<'pool, 'frame, 'host>(
        &'pool mut self,
        mut frame: Box<Interpreter<'frame, 'host, T>>,
    ) -> &'pool mut Interpreter<'frame, 'host, T> {
        frame.clear_frame_refs();
        // SAFETY: `clear_frame_refs` removes every reference carrying `'frame`, so the boxed
        // interpreter can be stored in the pool with the erased `'static` lifetime.
        let frame = unsafe { trustme::decouple_lt_box(frame) };
        let frame = self.frames.push_mut(frame);
        // SAFETY: The returned borrow is tied to `&mut self`; the erased frame references are
        // empty.
        unsafe { trustme::decouple_interpreter_lt_mut(frame) }
    }

    pub(crate) fn last_mut<'frame, 'host>(&mut self) -> Option<&mut Interpreter<'frame, 'host, T>> {
        let frame = self.frames.last_mut()?.as_mut();
        // SAFETY: Frames stored in the pool have had their frame-local references cleared by
        // `push`, and this borrow is tied to the pool borrow.
        Some(unsafe { trustme::decouple_interpreter_lt_mut(frame) })
    }
}

#[cfg(test)]
mod owned_error_tests {
    use super::*;
    use crate::{DatabaseError, interpreter::MessageExt, test_utils::TestTypes};

    #[test]
    fn owned_error_is_taken_on_exit() {
        let message = MessageExt::default();
        let mut interpreter = Interpreter::<TestTypes>::new(&message);
        let error = DatabaseError::new(core::fmt::Error, false);
        let stop = interpreter.fail(error.clone());
        let result = interpreter.finish_run(stop);
        assert_eq!(result, Err(ExecutionError::Database(error)));
        assert!(interpreter.error.is_none());
        assert_eq!(interpreter.finish_run(InstrStop::Stop), Ok(InstrStop::Stop));
    }
}

#[cfg(test)]
mod bytecode_padding_tests {
    use super::*;
    use crate::{
        interpreter::op,
        test_utils::{RunConfig, run},
    };

    #[test]
    fn empty_code_reads_padding_stop() {
        let interp = run(RunConfig::default());
        assert_eq!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn push_past_end_reads_zero_padding() {
        let interp = run(RunConfig::new([op::PUSH2, 0x01]));
        assert_eq!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0x0100)]);
    }

    #[test]
    fn jump_to_final_jumpdest_reads_padding_stop() {
        let interp = run(RunConfig::new([op::PUSH1, 3, op::JUMP, op::JUMPDEST]));
        assert_eq!(interp.err, InstrStop::Stop);
        assert!(interp.stack().is_empty());
    }
}
