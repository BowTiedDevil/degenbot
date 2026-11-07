use crate::wrap::{Context, LLVMString};
use evm2_jit_backend::{Result, eyre};
use llvm_sys::{LLVMDiagnosticHandler, LLVMDiagnosticSeverity::*, core::*, prelude::*};
use std::{ffi::c_void, fmt, sync::Mutex};

/// LLVM diagnostic handler guard.
///
/// LLVM keeps compiling after an error diagnostic such as "ran out of registers" and then emits
/// broken code, so the handler records the first error and [`check`](Self::check) returns it to
/// the caller.
pub(crate) struct DiagnosticHandlerGuard {
    cx: LLVMContextRef,
    prev_dh: LLVMDiagnosticHandler,
    prev_dhc: *mut c_void,
    /// First error diagnostic since the last [`check`](Self::check). Boxed so the
    /// handler's context pointer stays valid when the guard moves.
    error: Box<Mutex<Option<String>>>,
}

impl fmt::Debug for DiagnosticHandlerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticHandlerGuard").finish_non_exhaustive()
    }
}

impl DiagnosticHandlerGuard {
    pub(crate) fn new(cx: &Context) -> Self {
        unsafe {
            let c = cx.raw();
            let prev_dh = LLVMContextGetDiagnosticHandler(c);
            let prev_dhc = LLVMContextGetDiagnosticContext(c);
            let error = Box::new(Mutex::new(None));
            let error_ptr = &*error as *const Mutex<Option<String>> as *mut c_void;
            LLVMContextSetDiagnosticHandler(c, Some(Self::diagnostic_handler), error_ptr);
            Self { cx: c, prev_dh, prev_dhc, error }
        }
    }

    /// Returns `result`, adding the first error LLVM reported since the last call to it. Always
    /// clears the recorded error so it does not leak into the next operation.
    pub(crate) fn check<T>(&self, result: Result<T>) -> Result<T> {
        let Some(msg) = self.error.lock().unwrap().take() else { return result };
        match result {
            Ok(_) => Err(eyre::eyre!("LLVM error: {msg}")),
            Err(e) => Err(e.wrap_err(format!("LLVM error: {msg}"))),
        }
    }

    extern "C" fn diagnostic_handler(di: LLVMDiagnosticInfoRef, context: *mut c_void) {
        unsafe {
            // `LLVMGetDiagInfoDescription` returns an LLVM `Message`.
            let msg_cstr = LLVMString::from_raw(LLVMGetDiagInfoDescription(di));
            let msg = msg_cstr.to_string_lossy();
            match LLVMGetDiagInfoSeverity(di) {
                LLVMDSError => {
                    error!(target: "llvm", "{msg}");
                    let error = &*(context as *const Mutex<Option<String>>);
                    error.lock().unwrap().get_or_insert_with(|| msg.into_owned());
                }
                LLVMDSWarning => warn!(target: "llvm", "{msg}"),
                LLVMDSRemark => trace!(target: "llvm", "{msg}"),
                LLVMDSNote => debug!(target: "llvm", "{msg}"),
            }
        }
    }
}

impl Drop for DiagnosticHandlerGuard {
    fn drop(&mut self) {
        unsafe {
            LLVMContextSetDiagnosticHandler(self.cx, self.prev_dh, self.prev_dhc);
        }
    }
}
