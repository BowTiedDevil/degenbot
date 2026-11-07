//! Debug info.

use super::{Context, Module};
use llvm_sys::{
    debuginfo::*,
    prelude::{LLVMDIBuilderRef, LLVMMetadataRef},
};
use std::ptr;

/// Returns the debug info metadata version LLVM emits, for the `Debug Info Version` module flag.
pub(crate) fn debug_metadata_version() -> u32 {
    unsafe { LLVMDebugMetadataVersion() }
}

/// An LLVM metadata node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct Metadata(LLVMMetadataRef);

impl Metadata {
    pub(crate) const fn new(raw: LLVMMetadataRef) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> LLVMMetadataRef {
        self.0
    }
}

/// A debug location, attached to the instructions a [`Builder`](super::Builder) creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DILocation(LLVMMetadataRef);

impl DILocation {
    pub(crate) fn new(raw: LLVMMetadataRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }

    pub(crate) const fn raw(self) -> LLVMMetadataRef {
        self.0
    }
}

/// A compile unit and the file it describes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DICompileUnit {
    unit: LLVMMetadataRef,
    file: LLVMMetadataRef,
}

/// An owned debug info builder.
#[derive(Debug)]
pub(crate) struct DebugInfoBuilder {
    raw: LLVMDIBuilderRef,
}

impl DebugInfoBuilder {
    /// Creates a builder for `module` and a C compile unit for the file `filename` in `directory`.
    pub(crate) fn new(
        module: &Module,
        filename: &str,
        directory: &str,
        producer: &str,
        is_optimized: bool,
        flags: &str,
    ) -> (Self, DICompileUnit) {
        unsafe {
            let builder = Self { raw: LLVMCreateDIBuilder(module.as_mut_ptr()) };
            let file = LLVMDIBuilderCreateFile(
                builder.raw,
                filename.as_ptr().cast(),
                filename.len(),
                directory.as_ptr().cast(),
                directory.len(),
            );
            let unit = LLVMDIBuilderCreateCompileUnit(
                builder.raw,
                LLVMDWARFSourceLanguage::LLVMDWARFSourceLanguageC,
                file,
                producer.as_ptr().cast(),
                producer.len(),
                is_optimized as _,
                flags.as_ptr().cast(),
                flags.len(),
                0,
                c"".as_ptr(),
                0,
                LLVMDWARFEmissionKind::LLVMDWARFEmissionKindFull,
                0,
                false as _,
                false as _,
                c"".as_ptr(),
                0,
                c"".as_ptr(),
                0,
            );
            (builder, DICompileUnit { unit, file })
        }
    }

    /// Describes a public, local function without parameters and line information.
    pub(crate) fn create_subprogram(
        &self,
        unit: DICompileUnit,
        name: &str,
        is_optimized: bool,
    ) -> Metadata {
        unsafe {
            // A null return type means `void`.
            let mut types = [ptr::null_mut()];
            let ty = LLVMDIBuilderCreateSubroutineType(
                self.raw,
                unit.file,
                types.as_mut_ptr(),
                types.len() as u32,
                LLVMDIFlagPublic,
            );
            Metadata(LLVMDIBuilderCreateFunction(
                self.raw,
                unit.unit,
                name.as_ptr().cast(),
                name.len(),
                name.as_ptr().cast(),
                name.len(),
                unit.file,
                0,
                ty,
                true as _,
                true as _,
                0,
                LLVMDIFlagPublic,
                is_optimized as _,
            ))
        }
    }

    /// Creates a debug location in `scope`.
    pub(crate) fn create_debug_location(
        &self,
        cx: &Context,
        line: u32,
        column: u32,
        scope: Metadata,
    ) -> DILocation {
        DILocation(unsafe {
            LLVMDIBuilderCreateDebugLocation(cx.raw(), line, column, scope.raw(), ptr::null_mut())
        })
    }

    /// Resolves forward references. Call before emitting the module.
    pub(crate) fn finalize(&self) {
        unsafe { LLVMDIBuilderFinalize(self.raw) }
    }
}

impl Drop for DebugInfoBuilder {
    fn drop(&mut self) {
        self.finalize();
        unsafe { LLVMDisposeDIBuilder(self.raw) }
    }
}
