//! Custom bindings to LLVM.

use crate::wrap::{Attribute, Context, Function};
use llvm_sys::{
    error::LLVMErrorRef,
    orc2::{
        LLVMOrcExecutionSessionRef, LLVMOrcExecutorAddress, LLVMOrcJITDylibRef,
        lljit::{LLVMOrcLLJITBuilderRef, LLVMOrcLLJITRef},
    },
    prelude::{LLVMAttributeRef, LLVMContextRef, LLVMValueRef},
    target_machine::{LLVMCodeGenOptLevel, LLVMTargetMachineRef},
};
use std::{ffi::c_char, sync::atomic::AtomicUsize};

#[link(name = "evm2_jit_llvm_cpp", kind = "static")]
unsafe extern "C" {
    fn evm2_jit_llvm_create_initializes_attr(
        ctx: LLVMContextRef,
        lower: i64,
        upper: i64,
    ) -> LLVMAttributeRef;

    fn evm2_jit_llvm_create_arg_mem_only_attr(ctx: LLVMContextRef) -> LLVMAttributeRef;

    fn evm2_jit_llvm_create_async_uwtable_attr(ctx: LLVMContextRef) -> LLVMAttributeRef;

    fn evm2_jit_llvm_create_no_captures_attr(ctx: LLVMContextRef) -> LLVMAttributeRef;

    pub(crate) fn evm2_jit_llvm_jit_dylib_add_to_link_order(
        jd: LLVMOrcJITDylibRef,
        other: LLVMOrcJITDylibRef,
    );

    pub(crate) fn evm2_jit_llvm_lljit_builder_set_concurrent_compiler(
        builder: LLVMOrcLLJITBuilderRef,
    );

    pub(crate) fn evm2_jit_llvm_execution_session_remove_jit_dylib(
        es: LLVMOrcExecutionSessionRef,
        jd: LLVMOrcJITDylibRef,
    ) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_lljit_lookup_in(
        jit: LLVMOrcLLJITRef,
        jd: LLVMOrcJITDylibRef,
        result: *mut LLVMOrcExecutorAddress,
        name: *const c_char,
    ) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_lljit_enable_debug_support(jit: LLVMOrcLLJITRef) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_lljit_enable_perf_support(jit: LLVMOrcLLJITRef) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_lljit_enable_simple_perf(jit: LLVMOrcLLJITRef) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_lljit_enable_memory_usage(
        jit: LLVMOrcLLJITRef,
        code_bytes: *const AtomicUsize,
        data_bytes: *const AtomicUsize,
    ) -> LLVMErrorRef;

    pub(crate) fn evm2_jit_llvm_target_machine_set_opt_level(
        tm: LLVMTargetMachineRef,
        level: LLVMCodeGenOptLevel,
    );

    fn evm2_jit_llvm_set_dso_local(val: LLVMValueRef, local: bool);

    pub(crate) fn evm2_jit_llvm_expand_ir_insts_after_lsr();
}

pub(crate) fn set_dso_local(f: Function) {
    unsafe { evm2_jit_llvm_set_dso_local(f.as_value_ref(), true) };
}

pub(crate) fn create_initializes_attr(cx: &Context, lower: i64, upper: i64) -> Attribute {
    Attribute::new(unsafe { evm2_jit_llvm_create_initializes_attr(cx.raw(), lower, upper) })
}

pub(crate) fn create_arg_mem_only_attr(cx: &Context) -> Attribute {
    Attribute::new(unsafe { evm2_jit_llvm_create_arg_mem_only_attr(cx.raw()) })
}

pub(crate) fn create_async_uwtable_attr(cx: &Context) -> Attribute {
    Attribute::new(unsafe { evm2_jit_llvm_create_async_uwtable_attr(cx.raw()) })
}

pub(crate) fn create_no_captures_attr(cx: &Context) -> Attribute {
    Attribute::new(unsafe { evm2_jit_llvm_create_no_captures_attr(cx.raw()) })
}
