use crate::bytecode::{Bytecode, JumpTableRef};
use core::ptr::NonNull;

/// EVM bytecode view.
#[derive(Clone, Copy, Debug)]
pub struct BytecodeRef<'a> {
    base: NonNull<u8>,
    jump_table: JumpTableRef<'a>,
}

/// Program counter state.
#[derive(Clone, Copy, Debug)]
pub struct Pc {
    pc: *const u8,
}

impl<'a> BytecodeRef<'a> {
    pub(crate) fn new(bytecode: &'a Bytecode) -> Self {
        Self {
            base: NonNull::from(bytecode.bytes_slice()).cast(),
            jump_table: bytecode.jump_table().as_ref(),
        }
    }

    /// Returns a pointer to the start of the bytecode.
    #[inline]
    pub const fn as_ptr(&self) -> *const u8 {
        self.base.as_ptr()
    }

    /// Returns whether `pc` points to a valid jump destination.
    #[inline]
    pub const fn is_valid_jumpdest(&self, pc: usize) -> bool {
        self.jump_table.is_valid(pc)
    }

    /// # Safety
    ///
    /// Caller must ensure `offset..offset + len` is in bounds of the bytecode allocation.
    #[inline]
    pub const unsafe fn code_slice_unchecked(&self, offset: usize, len: usize) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.as_ptr().add(offset), len) }
    }

    /// Returns the bytecode-relative offset for `pc`.
    #[inline]
    pub const fn pc_offset(&self, pc: Pc) -> usize {
        unsafe { pc.as_ptr().offset_from(self.as_ptr()) as usize }
    }
}

impl Pc {
    /// Creates a program counter from a byte pointer.
    #[inline(always)]
    pub(crate) const fn new(pc: *const u8) -> Self {
        Self { pc }
    }

    /// Returns the opcode at the current program counter.
    #[inline(always)]
    pub const fn op(&self) -> u8 {
        unsafe { *self.pc }
    }

    /// Returns the current program counter pointer.
    #[inline(always)]
    pub const fn as_ptr(&self) -> *const u8 {
        self.pc
    }

    /// # Safety
    ///
    /// Caller must ensure advancing by `n` keeps `pc` within valid bytecode bounds for
    /// subsequent reads.
    #[inline(always)]
    pub const unsafe fn advance_unchecked(&mut self, n: usize) {
        self.pc = unsafe { self.pc.add(n) };
    }

    /// # Safety
    ///
    /// Caller must ensure `pc` is a valid offset for the current bytecode.
    #[inline(always)]
    pub const unsafe fn set_unchecked(&mut self, bytecode: BytecodeRef<'_>, pc: usize) {
        self.pc = unsafe { bytecode.as_ptr().add(pc) };
    }

    /// # Safety
    ///
    /// Caller must ensure the requested range is in bounds of the bytecode allocation.
    #[inline(always)]
    pub const unsafe fn read_bytes_unchecked(&self, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.pc, n) }
    }

    /// # Safety
    ///
    /// Caller must ensure the requested range is in bounds of the bytecode allocation.
    #[inline(always)]
    pub const unsafe fn read_bytes_offset_unchecked(&self, offset: usize, n: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.pc.add(offset), n) }
    }
}
