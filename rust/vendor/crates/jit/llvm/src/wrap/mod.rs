//! Small owned wrappers over the `llvm-sys` C API.
//!
//! The wrappers cover only what the backend uses. Handles ([`Type`], [`Value`], [`Block`], ...) are
//! `Copy` pointers into an LLVM context and carry no lifetime: the backend owns the [`Context`] and
//! keeps it alive for as long as any handle exists.

use std::{
    borrow::Cow,
    ffi::{CStr, CString},
};

mod di;
mod ir;
mod string;
mod target;

pub(crate) use di::*;
pub use ir::*;
pub use string::*;
pub use target::*;

/// Converts `s` to a C string, truncating it at the first NUL byte.
///
/// The empty string maps to a static empty C string without allocating.
#[inline]
pub(crate) fn to_c_str(s: &str) -> Cow<'_, CStr> {
    if s.is_empty() {
        return Cow::Borrowed(c"");
    }
    match CStr::from_bytes_until_nul(s.as_bytes()) {
        Ok(c) => Cow::Borrowed(c),
        // SAFETY: There is no NUL byte in `s`, otherwise `from_bytes_until_nul` succeeds.
        Err(_) => Cow::Owned(unsafe { CString::from_vec_unchecked(s.as_bytes().to_vec()) }),
    }
}
