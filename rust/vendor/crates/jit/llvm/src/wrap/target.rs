//! Targets, target machines, and code generation.

use super::{LLVMString, Module, to_c_str};
use llvm_sys::{
    core::{LLVMDisposeMemoryBuffer, LLVMGetBufferSize, LLVMGetBufferStart},
    prelude::LLVMMemoryBufferRef,
    target::*,
    target_machine::*,
};
use std::{ffi::CStr, path::Path, ptr, slice};

/// A registered LLVM target.
///
/// Targets live in a process-wide registry and are never freed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target {
    raw: LLVMTargetRef,
}

// SAFETY: The registry entry is immutable and lives for the whole process.
unsafe impl Send for Target {}
unsafe impl Sync for Target {}

impl Target {
    /// Initializes the native target, assembly printer, and disassembler.
    pub(crate) fn initialize_native() -> Result<(), String> {
        unsafe {
            if LLVM_InitializeNativeTarget() == 1 {
                return Err("Unknown error in initializing native target".into());
            }
            if LLVM_InitializeNativeAsmPrinter() == 1 {
                return Err("Unknown error in initializing native asm printer".into());
            }
            if LLVM_InitializeNativeDisassembler() == 1 {
                return Err("Unknown error in initializing native disassembler".into());
            }
        }
        Ok(())
    }

    /// Looks up the target for `triple`.
    pub(crate) fn from_triple(triple: &CStr) -> Result<Self, LLVMString> {
        let mut target = ptr::null_mut();
        let mut message = ptr::null_mut();
        let failed = unsafe { LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut message) };
        if failed == 1 {
            return Err(unsafe { LLVMString::from_raw(message) });
        }
        Ok(Self { raw: target })
    }

    /// Returns the name of the target.
    pub(crate) fn get_name(&self) -> &CStr {
        unsafe { CStr::from_ptr(LLVMGetTargetName(self.raw)) }
    }

    /// Returns whether the target supports JIT compilation.
    pub(crate) fn has_jit(&self) -> bool {
        unsafe { LLVMTargetHasJIT(self.raw) == 1 }
    }

    /// Returns whether the target has a target machine.
    pub(crate) fn has_target_machine(&self) -> bool {
        unsafe { LLVMTargetHasTargetMachine(self.raw) == 1 }
    }

    /// Creates a target machine, or `None` if LLVM cannot.
    pub(crate) fn create_target_machine(
        &self,
        triple: &CStr,
        cpu: &str,
        features: &str,
        level: LLVMCodeGenOptLevel,
        reloc_mode: LLVMRelocMode,
        code_model: LLVMCodeModel,
    ) -> Option<TargetMachine> {
        let cpu = to_c_str(cpu);
        let features = to_c_str(features);
        let machine = unsafe {
            LLVMCreateTargetMachine(
                self.raw,
                triple.as_ptr(),
                cpu.as_ptr(),
                features.as_ptr(),
                level,
                reloc_mode,
                code_model,
            )
        };
        (!machine.is_null()).then_some(TargetMachine { raw: machine })
    }
}

/// An owned target machine.
#[derive(Debug)]
pub struct TargetMachine {
    raw: LLVMTargetMachineRef,
}

impl TargetMachine {
    /// Returns the raw target machine.
    pub(crate) const fn as_mut_ptr(&self) -> LLVMTargetMachineRef {
        self.raw
    }

    /// Returns the default target triple of the host.
    pub(crate) fn get_default_triple() -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMGetDefaultTargetTriple()) }
    }

    /// Returns the CPU name of the host.
    pub(crate) fn get_host_cpu_name() -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMGetHostCPUName()) }
    }

    /// Returns the comma-separated CPU features of the host.
    pub(crate) fn get_host_cpu_features() -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMGetHostCPUFeatures()) }
    }

    /// Returns the target triple of the machine.
    pub(crate) fn get_triple(&self) -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMGetTargetMachineTriple(self.raw)) }
    }

    /// Returns the CPU name of the machine.
    pub(crate) fn get_cpu(&self) -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMGetTargetMachineCPU(self.raw)) }
    }

    /// Creates the data layout of the machine.
    pub(crate) fn get_target_data(&self) -> TargetData {
        TargetData { raw: unsafe { LLVMCreateTargetDataLayout(self.raw) } }
    }

    /// Sets whether the assembly output carries comments.
    pub(crate) fn set_asm_verbosity(&self, verbose: bool) {
        unsafe { LLVMSetTargetMachineAsmVerbosity(self.raw, verbose as i32) }
    }

    /// Compiles `module` to assembly or an object file in memory.
    pub(crate) fn write_to_memory_buffer(
        &self,
        module: &Module,
        file_type: LLVMCodeGenFileType,
    ) -> Result<MemoryBuffer, LLVMString> {
        let mut buffer = ptr::null_mut();
        let mut message = ptr::null_mut();
        let failed = unsafe {
            LLVMTargetMachineEmitToMemoryBuffer(
                self.raw,
                module.as_mut_ptr(),
                file_type,
                &mut message,
                &mut buffer,
            )
        };
        if failed == 1 {
            return Err(unsafe { LLVMString::from_raw(message) });
        }
        Ok(MemoryBuffer { raw: buffer })
    }

    /// Compiles `module` to assembly or an object file at `path`.
    pub(crate) fn write_to_file(
        &self,
        module: &Module,
        file_type: LLVMCodeGenFileType,
        path: &Path,
    ) -> Result<(), LLVMString> {
        let path = to_c_str(path.to_str().expect("Did not find a valid Unicode path string"));
        let mut message = ptr::null_mut();
        let failed = unsafe {
            LLVMTargetMachineEmitToFile(
                self.raw,
                module.as_mut_ptr(),
                path.as_ptr().cast_mut(),
                file_type,
                &mut message,
            )
        };
        if failed == 1 {
            return Err(unsafe { LLVMString::from_raw(message) });
        }
        Ok(())
    }
}

impl Drop for TargetMachine {
    fn drop(&mut self) {
        unsafe { LLVMDisposeTargetMachine(self.raw) }
    }
}

/// An owned target data layout.
#[derive(Debug)]
pub(crate) struct TargetData {
    raw: LLVMTargetDataRef,
}

impl TargetData {
    /// Returns the raw target data.
    pub(crate) const fn as_mut_ptr(&self) -> LLVMTargetDataRef {
        self.raw
    }

    /// Returns the data layout string.
    pub(crate) fn get_data_layout(&self) -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMCopyStringRepOfTargetData(self.raw)) }
    }
}

impl Drop for TargetData {
    fn drop(&mut self) {
        unsafe { LLVMDisposeTargetData(self.raw) }
    }
}

/// An owned memory buffer, such as the output of [`TargetMachine::write_to_memory_buffer`].
#[derive(Debug)]
pub(crate) struct MemoryBuffer {
    raw: LLVMMemoryBufferRef,
}

impl MemoryBuffer {
    /// Returns the contents of the buffer.
    pub(crate) fn as_slice(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(LLVMGetBufferStart(self.raw).cast(), LLVMGetBufferSize(self.raw))
        }
    }
}

impl Drop for MemoryBuffer {
    fn drop(&mut self) {
        unsafe { LLVMDisposeMemoryBuffer(self.raw) }
    }
}
