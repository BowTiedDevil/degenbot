#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use alloy_primitives::{Address, B256, Bytes, U256, ruint};
use core::{
    fmt,
    marker::PhantomData,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::atomic::{Ordering, compiler_fence},
};
pub use evm2::interpreter::{Gas, InstrStop, Memory};
use evm2::{
    BaseEvmTypes, EvmTypesHost, SpecId,
    env::{BlockEnv, TxEnv},
    interpreter::{Host, Interpreter, Message},
    version::{EvmFeatures, GasParams},
};

mod arch;
use arch::evm2_jit_entry;
pub use arch::evm2_jit_exit;

/// Returns `true` if compiled code can run the interpreter's frame.
///
/// Compiled code keeps the remaining gas in a signed counter, so a frame with more than
/// `i64::MAX` gas must run on the interpreter. Check this before calling
/// [`EvmCompilerFn::call_with_interpreter`] or [`EvmCompilerFn::call`].
#[inline]
pub const fn can_run_compiled<T: EvmTypesHost>(interpreter: &Interpreter<'_, '_, T>) -> bool {
    interpreter.gas().remaining() <= i64::MAX as u64
}

/// The EVM bytecode compiler runtime context.
///
/// This is a simple wrapper around the interpreter's resources, allowing the compiled function to
/// access the memory, input, gas, host, and other resources.
///
/// # Safety
/// This struct uses `#[repr(C)]` to ensure a stable field layout since the JIT compiler
/// generates code that accesses fields by offset using `offset_of!`.
#[repr(C)]
pub struct EvmContext<'ctx, 'frame, 'host, T: EvmTypesHost = BaseEvmTypes> {
    /// The gas.
    ///
    /// First, so compiled code addresses the remaining gas through the context pointer
    /// itself instead of keeping a separate pointer live.
    pub gas: Gas,

    /// Active interpreter frame.
    pub interpreter: NonNull<Interpreter<'frame, 'host, T>>,

    /// The size of return data from the last call-like operation.
    pub return_data_len: usize,
    /// The size of the call input data, cached for CALLDATASIZE.
    pub calldatasize: usize,

    /// The result set by a builtin before exiting via [`evm2_jit_exit`].
    pub exit_result: InstrStop,
    /// Saved RSP from the entry trampoline, used by [`evm2_jit_exit`] to unwind.
    pub exit_sp: *mut u8,

    /// Cached base pointer for the current memory context.
    /// Refreshed after any memory resize.
    pub mem_base: *mut u8,
    /// Cached length of the current memory context in bytes.
    /// Refreshed after any memory resize.
    pub mem_len: usize,

    /// The host's block environment, cached for block opcodes.
    pub block_env: NonNull<BlockEnv<T>>,
    /// The host's transaction environment, cached for transaction opcodes.
    pub tx_env: NonNull<TxEnv<T>>,

    _marker: PhantomData<&'ctx mut Interpreter<'frame, 'host, T>>,
}

impl<T: EvmTypesHost> fmt::Debug for EvmContext<'_, '_, '_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmContext").field("memory", &self.memory()).finish_non_exhaustive()
    }
}

impl<'ctx, 'frame, 'host, T: EvmTypesHost> EvmContext<'ctx, 'frame, 'host, T> {
    /// Creates a new context from an interpreter.
    #[inline]
    pub fn from_interpreter(interpreter: &'ctx mut Interpreter<'frame, 'host, T>) -> Self {
        unsafe { Self::from_interpreter_with_stack(interpreter).0 }
    }

    /// Creates a new context from an interpreter and returns the borrowed stack.
    ///
    /// # Safety
    ///
    /// The caller must keep the returned stack length at or below the stack capacity and ensure
    /// the first `stack_len` words are initialized before accessing the interpreter stack.
    #[inline]
    pub unsafe fn from_interpreter_with_stack(
        interpreter: &'ctx mut Interpreter<'frame, 'host, T>,
    ) -> (Self, &'ctx mut EvmStack, &'ctx mut usize) {
        let interpreter_ptr = ptr::from_mut(&mut *interpreter);
        let interpreter_ptr = unsafe { NonNull::new_unchecked(interpreter_ptr) };
        let block_env = NonNull::from(interpreter.host().block_env());
        let tx_env = NonNull::from(interpreter.host().tx_env());
        let message = interpreter.message();
        let gas = interpreter.gas();
        let calldatasize = message.input.len();
        let return_data_len = interpreter.return_data().len();
        let (stack_ptr, stack_len) = unsafe { interpreter.stack_mut().into_raw_parts() };
        let stack = unsafe { EvmStack::from_mut_ptr(stack_ptr.cast()) };
        let mut this = Self {
            interpreter: interpreter_ptr,
            gas,
            return_data_len,
            calldatasize,
            exit_result: InstrStop::Stop,
            exit_sp: ptr::null_mut(),
            mem_base: ptr::null_mut(),
            mem_len: 0,
            block_env,
            tx_env,
            _marker: PhantomData,
        };
        this.refresh_memory_cache();
        (this, stack, stack_len)
    }

    /// Finishes state owned by the JIT context after compiled execution.
    #[inline]
    pub const fn finish_interpreter_run(&mut self) {
        let gas = self.gas;
        self.interpreter_mut().set_gas(gas);
    }

    /// Returns the active interpreter frame.
    #[inline]
    pub const fn interpreter(&self) -> &Interpreter<'frame, 'host, T> {
        unsafe { self.interpreter.as_ref() }
    }

    /// Returns the active interpreter frame mutably.
    #[inline]
    pub const fn interpreter_mut(&mut self) -> &mut Interpreter<'frame, 'host, T> {
        unsafe { self.interpreter.as_mut() }
    }

    /// Returns the current linear memory.
    #[inline]
    pub const fn memory(&self) -> &Memory {
        self.interpreter().memory()
    }

    /// Returns the current linear memory.
    #[inline]
    pub const fn memory_mut(&mut self) -> &mut Memory {
        self.interpreter_mut().memory_mut()
    }

    /// Resizes memory using EVM memory gas accounting.
    #[inline]
    pub fn resize_memory(&mut self, offset: usize, len: usize) -> Result<(), InstrStop> {
        let gas_params = self.gas_params() as *const GasParams;
        let memory = self.memory_mut() as *mut Memory;
        let gas = &mut self.gas as *mut Gas;
        unsafe { (*memory).resize_evm(&mut *gas, &*gas_params, offset, len)? };
        self.refresh_memory_cache();
        Ok(())
    }

    /// Returns host state consumed by host-touching builtins.
    #[inline]
    pub const fn host(&mut self) -> &mut T::Host<'host> {
        self.interpreter_mut().host()
    }

    /// Returns calldata bytes.
    #[inline]
    pub fn input(&self) -> &[u8] {
        &self.message().input
    }

    /// Returns the current block environment.
    #[inline]
    pub const fn block_env(&self) -> &BlockEnv<T> {
        unsafe { self.block_env.as_ref() }
    }

    /// Returns the current transaction environment.
    #[inline]
    pub const fn tx_env(&self) -> &TxEnv<T> {
        unsafe { self.tx_env.as_ref() }
    }

    /// Returns active runtime version data.
    #[inline]
    pub fn version(&self) -> &evm2::Version {
        self.interpreter().version()
    }

    /// Returns active runtime gas parameters.
    #[inline]
    pub fn gas_params(&self) -> &GasParams {
        &self.version().gas_params
    }

    /// Returns the active base specification ID.
    #[inline]
    pub const fn spec_id(&self) -> SpecId {
        self.interpreter().spec()
    }

    /// Returns whether the active runtime version enables `feature`.
    #[inline]
    pub fn enables(&self, feature: EvmFeatures) -> bool {
        self.version().feature(feature)
    }

    /// Returns the active frame-local call/create message.
    #[inline]
    pub const fn message(&self) -> &'frame Message<T> {
        self.interpreter().message()
    }

    /// Returns whether the active frame forbids state-changing operations.
    #[inline]
    pub const fn is_static(&self) -> bool {
        self.interpreter().is_static()
    }

    /// Sets the static-call flag.
    #[inline]
    pub const fn set_static(&mut self, is_static: bool) {
        self.interpreter_mut().set_static(is_static);
    }

    /// Returns active original bytecode.
    #[inline]
    pub fn bytecode(&self) -> Bytes {
        self.interpreter().original_bytecode()
    }

    /// Returns return data from the last call-like operation.
    #[inline]
    pub const fn return_data(&self) -> &Bytes {
        self.interpreter().return_data()
    }

    /// Sets return data from the last call-like operation.
    #[inline]
    pub fn set_return_data(&mut self, return_data: Bytes) {
        self.return_data_len = return_data.len();
        *self.interpreter_mut().return_data_mut() = return_data;
    }

    /// Refreshes the cached memory base pointer and length.
    ///
    /// Must be called after any operation that may resize memory.
    #[inline]
    pub fn refresh_memory_cache(&mut self) {
        let mem_len = self.memory().len();
        // Keep LLVM from merging both reads into one 16-byte load: right after a resize it
        // overlaps the 8-byte length store and stalls on store forwarding.
        compiler_fence(Ordering::SeqCst);
        let mem_base = self.memory_mut().as_mut_ptr();
        self.mem_base = mem_base;
        self.mem_len = mem_len;
    }
}

/// Declare [`RawEvmCompilerFn`] functions in an `extern "C"` block.
///
/// # Examples
///
/// ```no_run
/// use evm2_jit_context::{EvmCompilerFn, extern_evm2_jit};
///
/// extern_evm2_jit! {
///    /// A simple function.
///    pub fn test_fn;
/// }
///
/// let test_fn = EvmCompilerFn::new(test_fn);
/// ```
#[macro_export]
macro_rules! extern_evm2_jit {
    ($( $(#[$attr:meta])* $vis:vis fn $name:ident; )+) => {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            $(
                $(#[$attr])*
                $vis fn $name(
                    ecx: ::core::ptr::NonNull<$crate::EvmContext<'_, '_, '_>>,
                    stack: ::core::ptr::NonNull<$crate::EvmStack>,
                    stack_len: ::core::ptr::NonNull<usize>,
                ) -> $crate::InstrStop;
            )+
        }
    };
}

/// The raw function signature of a bytecode function.
///
/// Prefer using [`EvmCompilerFn`] instead of this type. See [`EvmCompilerFn::call`] for more
/// information.
// When changing the signature, also update the corresponding declarations in `fn translate`.
pub type RawEvmCompilerFn = unsafe extern "C" fn(
    ecx: NonNull<EvmContext<'_, '_, '_>>,
    stack: NonNull<EvmStack>,
    stack_len: NonNull<usize>,
) -> InstrStop;

/// An EVM bytecode function.
#[derive(Clone, Copy, Debug, Hash)]
pub struct EvmCompilerFn(RawEvmCompilerFn);

impl From<RawEvmCompilerFn> for EvmCompilerFn {
    #[inline]
    fn from(f: RawEvmCompilerFn) -> Self {
        Self::new(f)
    }
}

impl From<EvmCompilerFn> for RawEvmCompilerFn {
    #[inline]
    fn from(f: EvmCompilerFn) -> Self {
        f.into_inner()
    }
}

impl EvmCompilerFn {
    /// Wraps the function.
    #[inline]
    pub const fn new(f: RawEvmCompilerFn) -> Self {
        Self(f)
    }

    /// Unwraps the function.
    #[inline]
    pub const fn into_inner(self) -> RawEvmCompilerFn {
        self.0
    }

    /// Calls the function by re-using an evm2 interpreter's resources.
    ///
    /// Execution errors remain in the interpreter for the EVM to extract after the runner returns.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the function is safe to call for this interpreter state, and
    /// that [`can_run_compiled`] holds.
    #[inline]
    pub unsafe fn call_with_interpreter<'ctx, 'frame, 'host, T: EvmTypesHost>(
        self,
        interpreter: &'ctx mut Interpreter<'frame, 'host, T>,
    ) -> InstrStop {
        let (mut ecx, stack, stack_len) =
            unsafe { EvmContext::from_interpreter_with_stack(interpreter) };
        let result = unsafe { self.call(&mut ecx, stack, stack_len) };
        if result == InstrStop::OutOfGas {
            ecx.gas.spend_all();
        }
        ecx.finish_interpreter_run();
        result
    }

    /// Calls the function.
    ///
    /// Arguments:
    /// - `stack`: The stack buffer.
    /// - `stack_len`: The stack length.
    /// - `ecx`: The context object.
    ///
    /// Use of this method is discouraged, as setup and cleanup need to be done manually.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the arguments are valid, that the function is safe to call, and
    /// that [`can_run_compiled`] holds.
    #[inline]
    pub unsafe fn call<T: EvmTypesHost>(
        self,
        ecx: &mut EvmContext<'_, '_, '_, T>,
        stack: &mut EvmStack,
        stack_len: &mut usize,
    ) -> InstrStop {
        unsafe {
            evm2_jit_entry(
                NonNull::from(ecx),
                NonNull::from(stack),
                NonNull::from(stack_len),
                self.0,
            )
        }
    }

    /// Same as [`call`](Self::call) but with `#[inline(never)]`.
    ///
    /// Use of this method is discouraged, as setup and cleanup need to be done manually.
    ///
    /// # Safety
    ///
    /// See [`call`](Self::call).
    #[inline(never)]
    pub unsafe fn call_noinline<T: EvmTypesHost>(
        self,
        ecx: &mut EvmContext<'_, '_, '_, T>,
        stack: &mut EvmStack,
        stack_len: &mut usize,
    ) -> InstrStop {
        unsafe { self.call(ecx, stack, stack_len) }
    }
}

/// EVM context stack.
#[repr(C)]
#[allow(missing_debug_implementations)]
pub struct EvmStack([MaybeUninit<EvmWord>; 1024]);

#[allow(clippy::new_without_default)]
impl EvmStack {
    /// The size of the stack in bytes.
    pub const SIZE: usize = 32 * Self::CAPACITY;

    /// The size of the stack in U256 elements.
    pub const CAPACITY: usize = 1024;

    /// Creates a new EVM stack, allocated on the stack.
    ///
    /// Use [`EvmStack::new_heap`] to create a stack on the heap.
    #[inline]
    pub const fn new() -> Self {
        Self(unsafe { MaybeUninit::uninit().assume_init() })
    }

    /// Creates a vector that can be used as a stack.
    #[inline]
    pub fn new_heap() -> Vec<EvmWord> {
        Vec::with_capacity(1024)
    }

    /// Creates a stack from a vector's buffer.
    ///
    /// # Panics
    ///
    /// Panics if the vector's capacity is less than the required stack capacity.
    #[inline]
    pub fn from_vec(vec: &Vec<EvmWord>) -> &Self {
        assert!(vec.capacity() >= Self::CAPACITY);
        unsafe { Self::from_ptr(vec.as_ptr()) }
    }

    /// Creates a stack from a mutable vector's buffer.
    ///
    /// # Panics
    ///
    /// Panics if the vector's capacity is less than the required stack capacity.
    #[inline]
    pub fn from_mut_vec(vec: &mut Vec<EvmWord>) -> &mut Self {
        assert!(vec.capacity() >= Self::CAPACITY);
        unsafe { Self::from_mut_ptr(vec.as_mut_ptr()) }
    }

    /// Creates a stack from a pointer to a buffer.
    ///
    /// # Safety
    ///
    /// See [`from_vec`](Self::from_vec).
    #[inline]
    pub unsafe fn from_ptr<'a>(ptr: *const EvmWord) -> &'a Self {
        debug_assert!(ptr.is_aligned());
        unsafe { &*ptr.cast::<Self>() }
    }

    /// Creates a stack from a mutable pointer to a buffer.
    ///
    /// # Safety
    ///
    /// See [`from_mut_vec`](Self::from_mut_vec).
    #[inline]
    pub unsafe fn from_mut_ptr<'a>(ptr: *mut EvmWord) -> &'a mut Self {
        debug_assert!(ptr.is_aligned());
        unsafe { &mut *ptr.cast::<Self>() }
    }

    /// Returns a pointer to the stack.
    #[inline]
    pub const fn as_ptr(&self) -> *const EvmWord {
        self.0.as_ptr().cast()
    }

    /// Returns a mutable pointer to the stack.
    #[inline]
    pub const fn as_mut_ptr(&mut self) -> *mut EvmWord {
        self.0.as_mut_ptr().cast()
    }

    /// Returns a slice of the initialized portion of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `len` slots are initialized.
    #[inline]
    pub unsafe fn as_slice(&self, len: usize) -> &[EvmWord] {
        assert!(len <= Self::CAPACITY);
        unsafe { core::slice::from_raw_parts(self.as_ptr(), len) }
    }

    /// Returns a mutable slice of the initialized portion of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `len` slots are initialized.
    #[inline]
    pub unsafe fn as_mut_slice(&mut self, len: usize) -> &mut [EvmWord] {
        assert!(len <= Self::CAPACITY);
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), len) }
    }

    /// Sets the value at the given index.
    ///
    /// # Panics
    ///
    /// Panics if the index is out of bounds.
    #[inline]
    pub const fn set(&mut self, index: usize, value: EvmWord) {
        self.0[index] = MaybeUninit::new(value);
    }

    /// Returns the word at the given index as a reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the slot at `index` is initialized.
    #[inline]
    pub unsafe fn get(&self, index: usize) -> Option<&EvmWord> {
        self.0.get(index).map(|slot| unsafe { slot.assume_init_ref() })
    }

    /// Returns the word at the given index as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the slot at `index` is initialized.
    #[inline]
    pub unsafe fn get_mut(&mut self, index: usize) -> Option<&mut EvmWord> {
        self.0.get_mut(index).map(|slot| unsafe { slot.assume_init_mut() })
    }

    /// Returns the word at the given index as a reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn get_unchecked(&self, index: usize) -> &EvmWord {
        unsafe { self.0.get_unchecked(index).assume_init_ref() }
    }

    /// Returns the word at the given index as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut EvmWord {
        unsafe { self.0.get_unchecked_mut(index).assume_init_mut() }
    }

    /// Sets the value at the top of the stack to `value`, and grows the stack by 1.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not full.
    #[inline]
    pub unsafe fn push(&mut self, value: EvmWord, len: &mut usize) {
        unsafe { self.set_unchecked(*len, value) };
        *len += 1;
    }

    /// Returns the value at the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked(&self, len: usize) -> &EvmWord {
        unsafe { self.get_unchecked(len - 1) }
    }

    /// Returns the value at the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the stack is not empty.
    #[inline]
    pub unsafe fn top_unchecked_mut(&mut self, len: usize) -> &mut EvmWord {
        unsafe { self.get_unchecked_mut(len - 1) }
    }

    /// Returns the value at the given index from the top of the stack.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked(&self, len: usize, n: usize) -> &EvmWord {
        unsafe { self.get_unchecked(len - n - 1) }
    }

    /// Returns the value at the given index from the top of the stack as a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len >= n + 1`.
    #[inline]
    pub unsafe fn from_top_unchecked_mut(&mut self, len: usize, n: usize) -> &mut EvmWord {
        unsafe { self.get_unchecked_mut(len - n - 1) }
    }

    /// Sets the value at the given index.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the index is within bounds.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, index: usize, value: EvmWord) {
        unsafe { *self.0.get_unchecked_mut(index) = MaybeUninit::new(value) };
    }
}

/// An EVM stack word, which is stored in native-endian order.
#[repr(C, align(8))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EvmWord(B256);

impl Default for EvmWord {
    #[inline]
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Debug for EvmWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_u256().fmt(f)
    }
}

impl fmt::Display for EvmWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_u256().fmt(f)
    }
}

impl TryFrom<EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: EvmWord) -> Result<Self, Self::Error> {
        Self::try_from(&w)
    }
}

impl TryFrom<&EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: &EvmWord) -> Result<Self, Self::Error> {
        w.to_u256().try_into()
    }
}

impl TryFrom<&mut EvmWord> for usize {
    type Error = ruint::FromUintError<Self>;

    #[inline]
    fn try_from(w: &mut EvmWord) -> Result<Self, Self::Error> {
        Self::try_from(&*w)
    }
}

impl From<U256> for EvmWord {
    #[inline]
    fn from(u: U256) -> Self {
        Self::from_u256(u)
    }
}

impl EvmWord {
    /// Zero.
    pub const ZERO: Self = Self(B256::ZERO);

    /// Create a new word from big-endian bytes.
    #[inline]
    pub const fn from_be_bytes(bytes: B256) -> Self {
        Self::from_be(Self(bytes))
    }

    /// Create a new word from big-endian bytes.
    #[inline]
    pub const fn from_be_slice(bytes: &[u8]) -> Self {
        Self::from_u256(U256::from_be_slice(bytes))
    }

    /// Create a new word from little-endian bytes.
    #[inline]
    pub const fn from_le_bytes(bytes: B256) -> Self {
        Self::from_le(Self(bytes))
    }

    /// Create a new word from little-endian slice.
    #[inline]
    pub const fn from_le_slice(bytes: &[u8]) -> Self {
        Self::from_u256(U256::from_le_slice(bytes))
    }

    /// Create a new word from native-endian bytes.
    #[inline]
    pub const fn from_ne_bytes(bytes: B256) -> Self {
        Self(bytes)
    }

    /// Create a new word from a [`U256`]. This is a no-op on little-endian systems.
    #[inline]
    pub const fn from_u256(u: U256) -> Self {
        #[cfg(target_endian = "little")]
        return unsafe { core::mem::transmute::<U256, Self>(u) };
        #[cfg(target_endian = "big")]
        return Self(B256::new(u.to_be_bytes()));
    }

    /// Converts a big-endian representation into a native one.
    #[inline]
    pub const fn from_be(x: Self) -> Self {
        #[cfg(target_endian = "little")]
        return x.swap_bytes();
        #[cfg(target_endian = "big")]
        return x;
    }

    /// Converts a little-endian representation into a native one.
    #[inline]
    pub const fn from_le(x: Self) -> Self {
        #[cfg(target_endian = "little")]
        return x;
        #[cfg(target_endian = "big")]
        return x.swap_bytes();
    }

    /// Return the memory representation of this integer as a byte array in big-endian byte order.
    #[inline]
    pub const fn to_be_bytes(self) -> B256 {
        self.to_be().to_ne_bytes()
    }

    /// Return the memory representation of this integer as a byte array in little-endian byte
    /// order.
    #[inline]
    pub const fn to_le_bytes(self) -> B256 {
        self.to_le().to_ne_bytes()
    }

    /// Return the memory representation of this integer as a byte array in native byte order.
    #[inline]
    pub const fn to_ne_bytes(self) -> B256 {
        self.0
    }

    /// Converts `self` to big endian from the target's endianness.
    #[inline]
    pub const fn to_be(self) -> Self {
        #[cfg(target_endian = "little")]
        return self.swap_bytes();
        #[cfg(target_endian = "big")]
        return self;
    }

    /// Converts `self` to little endian from the target's endianness.
    #[inline]
    pub const fn to_le(self) -> Self {
        #[cfg(target_endian = "little")]
        return self;
        #[cfg(target_endian = "big")]
        return self.swap_bytes();
    }

    /// Reverses the byte order of the integer.
    #[inline]
    pub const fn swap_bytes(mut self) -> Self {
        self.0.0.reverse();
        self
    }

    /// Casts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[cfg(target_endian = "little")]
    #[inline]
    pub const fn as_u256(&self) -> &U256 {
        unsafe { &*(self as *const Self as *const U256) }
    }

    /// Casts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[cfg(target_endian = "little")]
    #[inline]
    pub const fn as_u256_mut(&mut self) -> &mut U256 {
        unsafe { &mut *(self as *mut Self as *mut U256) }
    }

    /// Converts this value to a [`U256`]. This is a simple copy on little-endian systems.
    #[inline]
    pub const fn to_u256(&self) -> U256 {
        #[cfg(target_endian = "little")]
        return *self.as_u256();
        #[cfg(target_endian = "big")]
        return U256::from_be_bytes(self.0.0);
    }

    /// Converts this value to a [`U256`]. This is a no-op on little-endian systems.
    #[inline]
    pub const fn into_u256(self) -> U256 {
        #[cfg(target_endian = "little")]
        return unsafe { core::mem::transmute::<Self, U256>(self) };
        #[cfg(target_endian = "big")]
        return U256::from_be_bytes(self.0.0);
    }

    /// Converts this value to an [`Address`].
    #[inline]
    pub fn to_address(self) -> Address {
        Address::from_word(self.to_be_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::{
        DatabaseError, Evm, ExecutionError, InterpreterRunner,
        bytecode::Bytecode,
        env::BlockEnvExt,
        evm::{EmptyDB, precompile::NoPrecompiles},
        interpreter::MessageExt,
        registry::TxRegistry,
    };

    #[derive(Debug)]
    struct TestRunner(EvmCompilerFn);

    impl InterpreterRunner<BaseEvmTypes> for TestRunner {
        fn run<'frame, 'host>(
            &self,
            interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
            host: &mut Evm<'host, BaseEvmTypes>,
        ) -> Option<InstrStop> {
            interpreter.prepare_run(host);
            // SAFETY: The test functions only access this initialized interpreter's context.
            Some(unsafe { self.0.call_with_interpreter(interpreter) })
        }
    }

    #[test]
    fn compiled_boundary_returns_owned_error_and_can_be_reused() {
        unsafe extern "C" fn fail(
            mut ecx: NonNull<EvmContext<'_, '_, '_>>,
            _stack: NonNull<EvmStack>,
            _stack_len: NonNull<usize>,
        ) -> InstrStop {
            // SAFETY: The compiled invocation owns the live context.
            unsafe { ecx.as_mut() }
                .interpreter_mut()
                .fail(DatabaseError::new(core::fmt::Error, false))
        }
        let mut message = MessageExt {
            gas_limit: 30_000,
            code: Bytecode::new_legacy(Bytes::from_static(&[0x00])),
            ..MessageExt::default()
        };
        let mut host = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            EmptyDB::default(),
            NoPrecompiles::default(),
        );
        host.set_interpreter_runner(TestRunner(EvmCompilerFn::new(fail)));
        let error = host.execute_message(&mut message).unwrap_err();
        let ExecutionError::Database(error) = error else { panic!("expected database error") };
        assert!(!error.is_fatal());
        assert!(error.downcast_ref::<core::fmt::Error>().is_some());
        host.set_interpreter_runner(TestRunner(EvmCompilerFn::new(__test_fn)));
        assert_eq!(host.execute_message(&mut message).unwrap().stop, InstrStop::Stop);
    }

    #[test]
    fn conversions() {
        let mut word = EvmWord::ZERO;
        assert_eq!(usize::try_from(word), Ok(0));
        assert_eq!(usize::try_from(&word), Ok(0));
        assert_eq!(usize::try_from(&mut word), Ok(0));
    }

    extern_evm2_jit! {
        #[link_name = "__test_fn"]
        fn test_fn;
    }

    #[unsafe(no_mangle)]
    extern "C" fn __test_fn(
        _ecx: NonNull<EvmContext<'_, '_, '_>>,
        _stack: NonNull<EvmStack>,
        _stack_len: NonNull<usize>,
    ) -> InstrStop {
        InstrStop::Stop
    }

    #[test]
    fn extern_macro() {
        let f1 = EvmCompilerFn::new(test_fn).into_inner();
        let f2 = EvmCompilerFn::new(__test_fn).into_inner();
        assert!(core::ptr::fn_addr_eq(f1, f2), "{f1:?} != {f2:?}");
    }
}
