use llvm_sys::core::LLVMDisposeMessage;
use std::{
    error::Error,
    ffi::{CStr, c_char},
    fmt,
    ops::Deref,
    ptr::NonNull,
};

/// An owned, NUL-terminated string allocated by LLVM, such as an error message.
pub struct LLVMString {
    ptr: NonNull<c_char>,
}

// SAFETY: The string is uniquely owned and never mutated.
unsafe impl Send for LLVMString {}
unsafe impl Sync for LLVMString {}

impl LLVMString {
    /// Takes ownership of a string allocated by LLVM, to be freed with `LLVMDisposeMessage`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a valid NUL-terminated message that nothing else frees.
    ///
    /// # Panics
    ///
    /// Panics if `ptr` is null.
    pub const unsafe fn from_raw(ptr: *mut c_char) -> Self {
        Self { ptr: NonNull::new(ptr).expect("null LLVM string") }
    }
}

impl Deref for LLVMString {
    type Target = CStr;

    fn deref(&self) -> &CStr {
        // SAFETY: `from_raw` guarantees a valid NUL-terminated string.
        unsafe { CStr::from_ptr(self.ptr.as_ptr()) }
    }
}

impl fmt::Debug for LLVMString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl fmt::Display for LLVMString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

impl Error for LLVMString {}

impl Drop for LLVMString {
    fn drop(&mut self) {
        // SAFETY: `from_raw` guarantees the string came from LLVM and is owned by `self`.
        unsafe { LLVMDisposeMessage(self.ptr.as_ptr()) }
    }
}
