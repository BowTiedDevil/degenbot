//! LLVM IR: contexts, modules, types, values, and the instruction builder.

use super::{DILocation, LLVMString, Metadata, TargetData, TargetMachine, to_c_str};
use llvm_sys::{
    LLVMAttributeFunctionIndex, LLVMIntPredicate, LLVMLinkage, LLVMModuleFlagBehavior,
    LLVMTailCallKind, LLVMTypeKind, LLVMUnnamedAddr,
    analysis::{LLVMVerifierFailureAction, LLVMVerifyModule},
    core::*,
    debuginfo::LLVMSetSubprogram,
    error::LLVMGetErrorMessage,
    prelude::*,
    target::LLVMIntPtrTypeInContext,
    transforms::pass_builder::{
        LLVMCreatePassBuilderOptions, LLVMDisposePassBuilderOptions, LLVMPassBuilderOptionsRef,
        LLVMRunPasses,
    },
};
use std::{ffi::CStr, fmt, mem::ManuallyDrop, path::Path, ptr};

/// An owned LLVM context.
///
/// Every other handle in this module belongs to a context and is valid only while the context is
/// alive.
#[derive(Debug)]
pub struct Context {
    raw: LLVMContextRef,
}

impl Context {
    /// Creates a new context.
    pub(crate) fn create() -> Self {
        Self { raw: unsafe { LLVMContextCreate() } }
    }

    /// Wraps a raw context, taking ownership of it.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid context. Wrap the result in [`ManuallyDrop`] to borrow a context that
    /// something else owns.
    pub(crate) const unsafe fn new(raw: LLVMContextRef) -> Self {
        Self { raw }
    }

    /// Returns the raw context.
    pub const fn raw(&self) -> LLVMContextRef {
        self.raw
    }

    /// Creates an instruction builder.
    pub(crate) fn create_builder(&self) -> Builder {
        Builder { raw: unsafe { LLVMCreateBuilderInContext(self.raw) } }
    }

    /// Creates an empty module.
    pub(crate) fn create_module(&self, name: &str) -> Module {
        let name = to_c_str(name);
        Module { raw: unsafe { LLVMModuleCreateWithNameInContext(name.as_ptr(), self.raw) } }
    }

    /// The `void` type.
    pub(crate) fn void_type(&self) -> Type {
        Type(unsafe { LLVMVoidTypeInContext(self.raw) })
    }

    /// The `i1` type.
    pub(crate) fn bool_type(&self) -> Type {
        self.int_type(1)
    }

    /// The `i8` type.
    pub(crate) fn i8_type(&self) -> Type {
        self.int_type(8)
    }

    /// The `i16` type.
    pub(crate) fn i16_type(&self) -> Type {
        self.int_type(16)
    }

    /// The `i32` type.
    pub(crate) fn i32_type(&self) -> Type {
        self.int_type(32)
    }

    /// The `i64` type.
    pub(crate) fn i64_type(&self) -> Type {
        self.int_type(64)
    }

    /// The `i128` type.
    pub(crate) fn i128_type(&self) -> Type {
        self.int_type(128)
    }

    /// The integer type with the given bit width.
    pub(crate) fn int_type(&self, bits: u32) -> Type {
        Type(unsafe { LLVMIntTypeInContext(self.raw, bits) })
    }

    /// The integer type as wide as a pointer of the given target.
    pub(crate) fn ptr_sized_int_type(&self, target_data: &TargetData) -> Type {
        Type(unsafe { LLVMIntPtrTypeInContext(self.raw, target_data.as_mut_ptr()) })
    }

    /// The opaque pointer type in the default address space.
    pub(crate) fn ptr_type(&self) -> Type {
        Type(unsafe { LLVMPointerTypeInContext(self.raw, 0) })
    }

    /// An anonymous struct type.
    pub(crate) fn struct_type(&self, fields: &[Type], packed: bool) -> Type {
        Type(unsafe {
            LLVMStructTypeInContext(
                self.raw,
                fields.as_ptr().cast_mut().cast(),
                fields.len() as u32,
                packed as LLVMBool,
            )
        })
    }

    /// Appends a basic block to `function`.
    pub(crate) fn append_basic_block(&self, function: Function, name: &str) -> Block {
        let name = to_c_str(name);
        Block(unsafe {
            LLVMAppendBasicBlockInContext(self.raw, function.as_value_ref(), name.as_ptr())
        })
    }

    /// Inserts a basic block right after `block`.
    pub(crate) fn insert_basic_block_after(&self, block: Block, name: &str) -> Block {
        unsafe {
            let next = LLVMGetNextBasicBlock(block.0);
            if next.is_null() {
                return self.append_basic_block(Function(LLVMGetBasicBlockParent(block.0)), name);
            }
            let name = to_c_str(name);
            Block(LLVMInsertBasicBlockInContext(self.raw, next, name.as_ptr()))
        }
    }

    /// Creates a metadata string.
    pub(crate) fn metadata_string(&self, string: &str) -> Metadata {
        let string = to_c_str(string);
        Metadata::new(unsafe {
            LLVMMDStringInContext2(self.raw, string.as_ptr(), string.to_bytes().len())
        })
    }

    /// Creates a metadata node holding `values`.
    pub(crate) fn metadata_node(&self, values: &[Metadata]) -> MetadataNode {
        unsafe {
            let node =
                LLVMMDNodeInContext2(self.raw, values.as_ptr().cast_mut().cast(), values.len());
            MetadataNode(LLVMMetadataAsValue(self.raw, node))
        }
    }

    /// Returns the ID of the metadata kind `name`, such as `prof`.
    pub(crate) fn get_kind_id(&self, name: &str) -> u32 {
        unsafe { LLVMGetMDKindIDInContext(self.raw, name.as_ptr().cast(), name.len() as u32) }
    }

    /// Creates an enum attribute. Look the kind up with [`Attribute::get_named_enum_kind_id`].
    pub(crate) fn create_enum_attribute(&self, kind_id: u32, value: u64) -> Attribute {
        Attribute(unsafe { LLVMCreateEnumAttribute(self.raw, kind_id, value) })
    }

    /// Creates a string attribute.
    pub(crate) fn create_string_attribute(&self, key: &str, value: &str) -> Attribute {
        Attribute(unsafe {
            LLVMCreateStringAttribute(
                self.raw,
                key.as_ptr().cast(),
                key.len() as u32,
                value.as_ptr().cast(),
                value.len() as u32,
            )
        })
    }

    /// Creates a type attribute, such as `sret`.
    pub(crate) fn create_type_attribute(&self, kind_id: u32, ty: Type) -> Attribute {
        Attribute(unsafe { LLVMCreateTypeAttribute(self.raw, kind_id, ty.0) })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { LLVMContextDispose(self.raw) }
    }
}

/// An owned LLVM module.
#[derive(Debug)]
pub struct Module {
    raw: LLVMModuleRef,
}

impl Module {
    /// Wraps a raw module, taking ownership of it.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid module that nothing else disposes. Wrap the result in
    /// [`ManuallyDrop`] to borrow a module that something else owns.
    pub(crate) const unsafe fn new(raw: LLVMModuleRef) -> Self {
        Self { raw }
    }

    /// Returns the raw module.
    pub(crate) const fn as_mut_ptr(&self) -> LLVMModuleRef {
        self.raw
    }

    /// Releases ownership of the raw module.
    pub(crate) fn into_raw(self) -> LLVMModuleRef {
        ManuallyDrop::new(self).raw
    }

    /// Sets the module identifier.
    pub(crate) fn set_name(&self, name: &str) {
        unsafe { LLVMSetModuleIdentifier(self.raw, name.as_ptr().cast(), name.len()) }
    }

    /// Sets the source file name.
    pub(crate) fn set_source_file_name(&self, name: &str) {
        unsafe { LLVMSetSourceFileName(self.raw, name.as_ptr().cast(), name.len()) }
    }

    /// Sets the data layout string.
    pub(crate) fn set_data_layout(&self, data_layout: &CStr) {
        unsafe { LLVMSetDataLayout(self.raw, data_layout.as_ptr()) }
    }

    /// Sets the target triple.
    pub(crate) fn set_triple(&self, triple: &CStr) {
        unsafe { LLVMSetTarget(self.raw, triple.as_ptr()) }
    }

    /// Adds a module flag holding an integer constant.
    pub(crate) fn add_basic_value_flag(
        &self,
        key: &str,
        behavior: LLVMModuleFlagBehavior,
        flag: Value,
    ) {
        unsafe {
            LLVMAddModuleFlag(
                self.raw,
                behavior,
                key.as_ptr().cast(),
                key.len(),
                LLVMValueAsMetadata(flag.0),
            )
        }
    }

    /// Adds a function declaration.
    pub(crate) fn add_function(
        &self,
        name: &str,
        ty: Type,
        linkage: Option<LLVMLinkage>,
    ) -> Function {
        let name = to_c_str(name);
        let function = Function(unsafe { LLVMAddFunction(self.raw, name.as_ptr(), ty.0) });
        if let Some(linkage) = linkage {
            function.set_linkage(linkage);
        }
        function
    }

    /// Returns the function named `name`, if any.
    pub(crate) fn get_function(&self, name: &str) -> Option<Function> {
        let name = to_c_str(name);
        Function::new(unsafe { LLVMGetNamedFunction(self.raw, name.as_ptr()) })
    }

    /// Iterates over the functions of the module.
    pub(crate) fn get_functions(&self) -> impl Iterator<Item = Function> {
        let first = Function::new(unsafe { LLVMGetFirstFunction(self.raw) });
        std::iter::successors(first, |f| {
            Function::new(unsafe { LLVMGetNextFunction(f.as_value_ref()) })
        })
    }

    /// Adds a global variable in the default address space.
    pub(crate) fn add_global(&self, ty: Type, name: &str) -> Global {
        let name = to_c_str(name);
        Global(unsafe { LLVMAddGlobal(self.raw, ty.0, name.as_ptr()) })
    }

    /// Checks the module for errors.
    pub(crate) fn verify(&self) -> Result<(), LLVMString> {
        let mut message = ptr::null_mut();
        let failed = unsafe {
            LLVMVerifyModule(
                self.raw,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut message,
            )
        };
        let message = (!message.is_null()).then(|| unsafe { LLVMString::from_raw(message) });
        match message {
            Some(message) if failed != 0 => Err(message),
            _ => Ok(()),
        }
    }

    /// Returns the textual IR.
    #[cfg(test)]
    pub(crate) fn print_to_string(&self) -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMPrintModuleToString(self.raw)) }
    }

    /// Writes the textual IR to `path`.
    pub(crate) fn print_to_file(&self, path: &Path) -> Result<(), LLVMString> {
        let path = to_c_str(path.to_str().expect("Did not find a valid Unicode path string"));
        let mut message = ptr::null_mut();
        let failed = unsafe { LLVMPrintModuleToFile(self.raw, path.as_ptr(), &mut message) };
        if failed == 0 {
            return Ok(());
        }
        Err(if message.is_null() {
            unsafe {
                LLVMString::from_raw(LLVMCreateMessage(c"Error string was uninitialized.".as_ptr()))
            }
        } else {
            unsafe { LLVMString::from_raw(message) }
        })
    }

    /// Runs the new-pass-manager pipeline `passes`.
    pub(crate) fn run_passes(
        &self,
        passes: &str,
        machine: &TargetMachine,
        options: &PassBuilderOptions,
    ) -> Result<(), LLVMString> {
        let passes = to_c_str(passes);
        let error =
            unsafe { LLVMRunPasses(self.raw, passes.as_ptr(), machine.as_mut_ptr(), options.raw) };
        if error.is_null() {
            return Ok(());
        }
        Err(unsafe { LLVMString::from_raw(LLVMGetErrorMessage(error)) })
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        unsafe { LLVMDisposeModule(self.raw) }
    }
}

/// Options for [`Module::run_passes`].
#[derive(Debug)]
pub(crate) struct PassBuilderOptions {
    raw: LLVMPassBuilderOptionsRef,
}

impl PassBuilderOptions {
    /// Creates the default options.
    pub(crate) fn create() -> Self {
        Self { raw: unsafe { LLVMCreatePassBuilderOptions() } }
    }
}

impl Drop for PassBuilderOptions {
    fn drop(&mut self) {
        unsafe { LLVMDisposePassBuilderOptions(self.raw) }
    }
}

/// An LLVM type.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Type(LLVMTypeRef);

impl Type {
    /// Creates a function type that returns `self`.
    pub(crate) fn fn_type(self, params: &[Self], var_args: bool) -> Self {
        Self(unsafe {
            LLVMFunctionType(
                self.0,
                params.as_ptr().cast_mut().cast(),
                params.len() as u32,
                var_args as LLVMBool,
            )
        })
    }

    /// Creates an array type of `size` elements of `self`.
    pub(crate) fn array_type(self, size: u32) -> Self {
        Self(unsafe { LLVMArrayType2(self.0, size as u64) })
    }

    /// The bit width of an integer type.
    pub(crate) fn bit_width(self) -> u32 {
        unsafe { LLVMGetIntTypeWidth(self.0) }
    }

    /// Creates an integer constant of this type.
    ///
    /// `value` is truncated to the bit width of the type, and sign-extended from it if
    /// `sign_extend` is set.
    pub(crate) fn const_int(self, mut value: u64, sign_extend: bool) -> Value {
        // LLVM 23 no longer truncates implicitly.
        let bit_width = self.bit_width();
        if bit_width < u64::BITS {
            value &= (1 << bit_width) - 1;
        }
        Value(unsafe { LLVMConstInt(self.0, value, sign_extend as LLVMBool) })
    }

    /// Creates an integer constant of this type from a hexadecimal string.
    pub(crate) fn const_int_from_hex(self, hex: &str) -> Value {
        Value(unsafe {
            LLVMConstIntOfStringAndSize(self.0, hex.as_ptr().cast(), hex.len() as u32, 16)
        })
    }

    /// Creates an array constant with element type `self`.
    pub(crate) fn const_array(self, values: &[Value]) -> Value {
        Value(unsafe {
            LLVMConstArray2(self.0, values.as_ptr().cast_mut().cast(), values.len() as u64)
        })
    }

    /// Creates the null constant of this type.
    pub(crate) fn const_null(self) -> Value {
        Value(unsafe { LLVMConstNull(self.0) })
    }

    /// Returns the textual IR of the type.
    pub(crate) fn print_to_string(self) -> LLVMString {
        unsafe { LLVMString::from_raw(LLVMPrintTypeToString(self.0)) }
    }
}

impl fmt::Debug for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.print_to_string(), f)
    }
}

/// An LLVM value: a constant, an argument, or the result of an instruction.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Value(LLVMValueRef);

impl Value {
    /// Returns the type of the value.
    pub(crate) fn get_type(self) -> Type {
        Type(unsafe { LLVMTypeOf(self.0) })
    }

    /// Sets the name of the value.
    pub(crate) fn set_name(self, name: &str) {
        unsafe { LLVMSetValueName2(self.0, name.as_ptr().cast(), name.len()) }
    }

    /// Sets the alignment of a load.
    pub(crate) fn set_alignment(self, align: u32) {
        assert!(align.is_power_of_two(), "alignment must be a power of two");
        unsafe { LLVMSetAlignment(self.0, align) }
    }

    /// Wraps the value as metadata.
    pub(crate) fn as_metadata(self) -> Metadata {
        Metadata::new(unsafe { LLVMValueAsMetadata(self.0) })
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value(self.0, f)
    }
}

/// An LLVM instruction.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Inst(LLVMValueRef);

impl Inst {
    /// Returns the result of the instruction, or `None` if its type is `void`.
    pub(crate) fn value(self) -> Option<Value> {
        let void = unsafe { LLVMGetTypeKind(LLVMTypeOf(self.0)) } == LLVMTypeKind::LLVMVoidTypeKind;
        (!void).then_some(Value(self.0))
    }

    /// Sets the alignment of a load or store.
    pub(crate) fn set_alignment(self, align: u32) {
        assert!(align.is_power_of_two(), "alignment must be a power of two");
        unsafe { LLVMSetAlignment(self.0, align) }
    }

    /// Attaches the metadata node `node` of kind `kind_id`.
    pub(crate) fn set_metadata(self, node: MetadataNode, kind_id: u32) {
        unsafe { LLVMSetMetadata(self.0, kind_id, node.0) }
    }

    /// Removes the instruction from its block and deletes it.
    pub(crate) fn erase_from_basic_block(self) {
        unsafe { LLVMInstructionEraseFromParent(self.0) }
    }

    /// Sets the tail call kind of a call.
    pub(crate) fn set_tail_call_kind(self, kind: LLVMTailCallKind) {
        unsafe { LLVMSetTailCallKind(self.0, kind) }
    }

    /// Sets the calling convention of a call.
    pub(crate) fn set_call_conv(self, call_conv: u32) {
        unsafe { LLVMSetInstructionCallConv(self.0, call_conv) }
    }
}

impl fmt::Debug for Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value(self.0, f)
    }
}

/// An LLVM function.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Function(LLVMValueRef);

impl Function {
    fn new(raw: LLVMValueRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }

    /// Returns the name of the function.
    pub(crate) fn get_name(self) -> String {
        let mut len = 0;
        unsafe {
            let name = LLVMGetValueName2(self.0, &mut len);
            String::from_utf8_lossy(std::slice::from_raw_parts(name.cast(), len)).into_owned()
        }
    }

    /// Returns the function type.
    pub(crate) fn get_type(self) -> Type {
        Type(unsafe { LLVMGlobalGetValueType(self.0) })
    }

    /// Returns the number of parameters.
    pub(crate) fn count_params(self) -> u32 {
        unsafe { LLVMCountParams(self.0) }
    }

    /// Returns the `nth` parameter, if it exists.
    pub(crate) fn get_nth_param(self, nth: u32) -> Option<Value> {
        (nth < self.count_params()).then(|| Value(unsafe { LLVMGetParam(self.0, nth) }))
    }

    /// Returns the number of basic blocks.
    pub(crate) fn count_basic_blocks(self) -> u32 {
        unsafe { LLVMCountBasicBlocks(self.0) }
    }

    /// Returns the entry block, if the function has a body.
    pub(crate) fn get_first_basic_block(self) -> Option<Block> {
        Block::new(unsafe { LLVMGetFirstBasicBlock(self.0) })
    }

    /// Sets the linkage.
    pub(crate) fn set_linkage(self, linkage: LLVMLinkage) {
        unsafe { LLVMSetLinkage(self.0, linkage) }
    }

    /// Returns the calling convention.
    pub(crate) fn get_call_conv(self) -> u32 {
        unsafe { LLVMGetFunctionCallConv(self.0) }
    }

    /// Sets the calling convention.
    pub(crate) fn set_call_conv(self, call_conv: u32) {
        unsafe { LLVMSetFunctionCallConv(self.0, call_conv) }
    }

    /// Adds an attribute.
    pub(crate) fn add_attribute(self, loc: AttributeLoc, attribute: Attribute) {
        unsafe { LLVMAddAttributeAtIndex(self.0, loc.index(), attribute.0) }
    }

    /// Attaches the debug info subprogram.
    pub(crate) fn set_subprogram(self, subprogram: Metadata) {
        unsafe { LLVMSetSubprogram(self.0, subprogram.raw()) }
    }

    /// Returns the raw value.
    pub(crate) const fn as_value_ref(self) -> LLVMValueRef {
        self.0
    }

    /// Deletes the function from its module.
    ///
    /// # Safety
    ///
    /// Nothing may use the function afterwards.
    pub(crate) unsafe fn delete(self) {
        unsafe { LLVMDeleteFunction(self.0) }
    }
}

impl fmt::Debug for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value(self.0, f)
    }
}

/// A global variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct Global(LLVMValueRef);

impl Global {
    /// Returns the address of the global, a pointer value.
    pub(crate) const fn as_pointer_value(self) -> Value {
        Value(self.0)
    }

    /// Sets the initial value.
    pub(crate) fn set_initializer(self, value: Value) {
        unsafe { LLVMSetInitializer(self.0, value.0) }
    }

    /// Marks the global as constant.
    pub(crate) fn set_constant(self, constant: bool) {
        unsafe { LLVMSetGlobalConstant(self.0, constant as LLVMBool) }
    }

    /// Sets the linkage.
    pub(crate) fn set_linkage(self, linkage: LLVMLinkage) {
        unsafe { LLVMSetLinkage(self.0, linkage) }
    }

    /// Sets whether the address is significant.
    pub(crate) fn set_unnamed_address(self, address: LLVMUnnamedAddr) {
        unsafe { LLVMSetUnnamedAddress(self.0, address) }
    }
}

/// A basic block.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Block(LLVMBasicBlockRef);

impl Block {
    fn new(raw: LLVMBasicBlockRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }

    /// Returns the last instruction, if the block is not empty.
    pub(crate) fn get_last_instruction(self) -> Option<Inst> {
        let inst = unsafe { LLVMGetLastInstruction(self.0) };
        (!inst.is_null()).then_some(Inst(inst))
    }

    /// Returns the address of the block, or `None` for an entry block or a detached block.
    ///
    /// # Safety
    ///
    /// The block must belong to a function that is not deleted.
    pub(crate) unsafe fn get_address(self) -> Option<Value> {
        unsafe {
            let parent = LLVMGetBasicBlockParent(self.0);
            // Taking the address of the entry block is illegal.
            if parent.is_null() || LLVMGetPreviousBasicBlock(self.0).is_null() {
                return None;
            }
            let address = LLVMBlockAddress(parent, self.0);
            (!address.is_null()).then_some(Value(address))
        }
    }
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value(unsafe { LLVMBasicBlockAsValue(self.0) }, f)
    }
}

/// A metadata node, usable as the operand of an instruction's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct MetadataNode(LLVMValueRef);

/// An LLVM attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct Attribute(LLVMAttributeRef);

impl Attribute {
    /// Wraps a raw attribute.
    pub(crate) const fn new(raw: LLVMAttributeRef) -> Self {
        Self(raw)
    }

    /// Returns the kind ID of the enum attribute `name`, or 0 if there is none.
    pub(crate) fn get_named_enum_kind_id(name: &str) -> u32 {
        unsafe { LLVMGetEnumAttributeKindForName(name.as_ptr().cast(), name.len()) }
    }
}

/// The place an attribute applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttributeLoc {
    /// The return value.
    Return,
    /// The parameter at the given index.
    Param(u32),
    /// The function itself.
    Function,
}

impl AttributeLoc {
    fn index(self) -> u32 {
        match self {
            Self::Return => 0,
            Self::Param(index) => {
                assert!(index <= u32::MAX - 2, "param index must be <= u32::MAX - 2");
                index + 1
            }
            Self::Function => LLVMAttributeFunctionIndex,
        }
    }
}

/// An instruction builder.
#[derive(Debug)]
pub(crate) struct Builder {
    raw: LLVMBuilderRef,
}

macro_rules! binary_ops {
    ($($(#[$meta:meta])* $name:ident => $llvm:ident),* $(,)?) => {$(
        $(#[$meta])*
        pub(crate) fn $name(&self, lhs: Value, rhs: Value, name: &str) -> Value {
            let name = to_c_str(name);
            Value(unsafe { $llvm(self.raw, lhs.0, rhs.0, name.as_ptr()) })
        }
    )*};
}

macro_rules! cast_ops {
    ($($(#[$meta:meta])* $name:ident => $llvm:ident),* $(,)?) => {$(
        $(#[$meta])*
        pub(crate) fn $name(&self, value: Value, ty: Type, name: &str) -> Value {
            let name = to_c_str(name);
            Value(unsafe { $llvm(self.raw, value.0, ty.0, name.as_ptr()) })
        }
    )*};
}

impl Builder {
    /// Positions the builder at the end of `block`.
    pub(crate) fn position_at_end(&self, block: Block) {
        unsafe { LLVMPositionBuilderAtEnd(self.raw, block.0) }
    }

    /// Returns the block the builder inserts into.
    pub(crate) fn get_insert_block(&self) -> Option<Block> {
        Block::new(unsafe { LLVMGetInsertBlock(self.raw) })
    }

    /// Sets the debug location attached to new instructions.
    pub(crate) fn set_current_debug_location(&self, location: DILocation) {
        unsafe { LLVMSetCurrentDebugLocation2(self.raw, location.raw()) }
    }

    /// Returns the debug location attached to new instructions, if any.
    pub(crate) fn get_current_debug_location(&self) -> Option<DILocation> {
        DILocation::new(unsafe { LLVMGetCurrentDebugLocation2(self.raw) })
    }

    /// Stops attaching a debug location to new instructions.
    pub(crate) fn unset_current_debug_location(&self) {
        unsafe { LLVMSetCurrentDebugLocation2(self.raw, ptr::null_mut()) }
    }

    /// Builds a call to `function`.
    pub(crate) fn build_call(&self, function: Function, args: &[Value], name: &str) -> Inst {
        let fn_ty = function.get_type();
        // LLVM rejects names on calls that return `void`.
        let void = unsafe { LLVMGetTypeKind(LLVMGetReturnType(fn_ty.0)) }
            == LLVMTypeKind::LLVMVoidTypeKind;
        let name = to_c_str(if void { "" } else { name });
        Inst(unsafe {
            LLVMBuildCall2(
                self.raw,
                fn_ty.0,
                function.as_value_ref(),
                args.as_ptr().cast_mut().cast(),
                args.len() as u32,
                name.as_ptr(),
            )
        })
    }

    /// Builds a stack allocation of `ty`.
    pub(crate) fn build_alloca(&self, ty: Type, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildAlloca(self.raw, ty.0, name.as_ptr()) })
    }

    /// Builds a load of `ty` from `ptr`.
    pub(crate) fn build_load(&self, ty: Type, ptr: Value, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildLoad2(self.raw, ty.0, ptr.0, name.as_ptr()) })
    }

    /// Builds a store of `value` to `ptr`.
    pub(crate) fn build_store(&self, ptr: Value, value: Value) -> Inst {
        Inst(unsafe { LLVMBuildStore(self.raw, value.0, ptr.0) })
    }

    /// Builds a GEP that stays inside the allocated object.
    ///
    /// # Safety
    ///
    /// The address must be in bounds, otherwise the result is poison.
    pub(crate) unsafe fn build_in_bounds_gep(
        &self,
        ty: Type,
        ptr: Value,
        indexes: &[Value],
        name: &str,
    ) -> Value {
        let name = to_c_str(name);
        Value(unsafe {
            LLVMBuildInBoundsGEP2(
                self.raw,
                ty.0,
                ptr.0,
                indexes.as_ptr().cast_mut().cast(),
                indexes.len() as u32,
                name.as_ptr(),
            )
        })
    }

    /// Builds a global string constant and returns a pointer to it.
    pub(crate) fn build_global_string_ptr(&self, value: &str, name: &str) -> Value {
        let value = to_c_str(value);
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildGlobalString(self.raw, value.as_ptr(), name.as_ptr()) })
    }

    /// Builds an extraction of the field `index` of an aggregate.
    pub(crate) fn build_extract_value(&self, aggregate: Value, index: u32, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildExtractValue(self.raw, aggregate.0, index, name.as_ptr()) })
    }

    /// Builds a `phi` of `ty` with the given incoming values.
    pub(crate) fn build_phi(&self, ty: Type, incoming: &[(Value, Block)], name: &str) -> Value {
        let name = to_c_str(name);
        let (mut values, mut blocks): (Vec<_>, Vec<_>) =
            incoming.iter().map(|&(value, block)| (value.0, block.0)).unzip();
        unsafe {
            let phi = LLVMBuildPhi(self.raw, ty.0, name.as_ptr());
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), incoming.len() as u32);
            Value(phi)
        }
    }

    /// Builds a `select`.
    pub(crate) fn build_select(
        &self,
        cond: Value,
        then: Value,
        otherwise: Value,
        name: &str,
    ) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildSelect(self.raw, cond.0, then.0, otherwise.0, name.as_ptr()) })
    }

    /// Builds an integer comparison.
    pub(crate) fn build_int_compare(
        &self,
        predicate: LLVMIntPredicate,
        lhs: Value,
        rhs: Value,
        name: &str,
    ) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildICmp(self.raw, predicate, lhs.0, rhs.0, name.as_ptr()) })
    }

    /// Builds a comparison of a pointer with null.
    pub(crate) fn build_is_null(&self, ptr: Value, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildIsNull(self.raw, ptr.0, name.as_ptr()) })
    }

    /// Builds a comparison of a pointer with non-null.
    pub(crate) fn build_is_not_null(&self, ptr: Value, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildIsNotNull(self.raw, ptr.0, name.as_ptr()) })
    }

    /// Builds a bitwise not.
    pub(crate) fn build_not(&self, value: Value, name: &str) -> Value {
        let name = to_c_str(name);
        Value(unsafe { LLVMBuildNot(self.raw, value.0, name.as_ptr()) })
    }

    /// Builds a right shift, arithmetic if `sign_extend` is set.
    pub(crate) fn build_right_shift(
        &self,
        lhs: Value,
        rhs: Value,
        sign_extend: bool,
        name: &str,
    ) -> Value {
        if sign_extend { self.build_ashr(lhs, rhs, name) } else { self.build_lshr(lhs, rhs, name) }
    }

    binary_ops! {
        /// Builds an integer addition.
        build_int_add => LLVMBuildAdd,
        /// Builds an integer subtraction.
        build_int_sub => LLVMBuildSub,
        /// Builds an integer multiplication.
        build_int_mul => LLVMBuildMul,
        /// Builds an unsigned division.
        build_int_unsigned_div => LLVMBuildUDiv,
        /// Builds a signed division.
        build_int_signed_div => LLVMBuildSDiv,
        /// Builds an unsigned remainder.
        build_int_unsigned_rem => LLVMBuildURem,
        /// Builds a signed remainder.
        build_int_signed_rem => LLVMBuildSRem,
        /// Builds a bitwise or.
        build_or => LLVMBuildOr,
        /// Builds a bitwise and.
        build_and => LLVMBuildAnd,
        /// Builds a bitwise xor.
        build_xor => LLVMBuildXor,
        /// Builds a left shift.
        build_left_shift => LLVMBuildShl,
        /// Builds a logical right shift.
        build_lshr => LLVMBuildLShr,
        /// Builds an arithmetic right shift.
        build_ashr => LLVMBuildAShr,
    }

    cast_ops! {
        /// Builds a zero extension to `ty`.
        build_int_z_extend => LLVMBuildZExt,
        /// Builds a sign extension to `ty`.
        build_int_s_extend => LLVMBuildSExt,
        /// Builds a truncation to `ty`.
        build_int_truncate => LLVMBuildTrunc,
        /// Builds an integer to pointer conversion.
        build_int_to_ptr => LLVMBuildIntToPtr,
    }

    /// Builds a `ret` with no value.
    pub(crate) fn build_return(&self, value: Option<Value>) -> Inst {
        Inst(unsafe {
            match value {
                Some(value) => LLVMBuildRet(self.raw, value.0),
                None => LLVMBuildRetVoid(self.raw),
            }
        })
    }

    /// Builds a `ret` of an aggregate made of `values`.
    pub(crate) fn build_aggregate_return(&self, values: &[Value]) -> Inst {
        Inst(unsafe {
            LLVMBuildAggregateRet(self.raw, values.as_ptr().cast_mut().cast(), values.len() as u32)
        })
    }

    /// Builds an unconditional branch.
    pub(crate) fn build_unconditional_branch(&self, dest: Block) -> Inst {
        Inst(unsafe { LLVMBuildBr(self.raw, dest.0) })
    }

    /// Builds a conditional branch.
    pub(crate) fn build_conditional_branch(
        &self,
        cond: Value,
        then: Block,
        otherwise: Block,
    ) -> Inst {
        Inst(unsafe { LLVMBuildCondBr(self.raw, cond.0, then.0, otherwise.0) })
    }

    /// Builds a `switch` on `value` with the given cases.
    pub(crate) fn build_switch(
        &self,
        value: Value,
        default: Block,
        cases: &[(Value, Block)],
    ) -> Inst {
        unsafe {
            let switch = LLVMBuildSwitch(self.raw, value.0, default.0, cases.len() as u32);
            for &(value, block) in cases {
                LLVMAddCase(switch, value.0, block.0);
            }
            Inst(switch)
        }
    }

    /// Builds an indirect branch to `address`, which is one of `destinations`.
    pub(crate) fn build_indirect_branch(&self, address: Value, destinations: &[Block]) -> Inst {
        unsafe {
            let branch = LLVMBuildIndirectBr(self.raw, address.0, destinations.len() as u32);
            for dest in destinations {
                LLVMAddDestination(branch, dest.0);
            }
            Inst(branch)
        }
    }

    /// Builds an `unreachable`.
    pub(crate) fn build_unreachable(&self) -> Inst {
        Inst(unsafe { LLVMBuildUnreachable(self.raw) })
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        unsafe { LLVMDisposeBuilder(self.raw) }
    }
}

fn fmt_value(value: LLVMValueRef, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(&unsafe { LLVMString::from_raw(LLVMPrintValueToString(value)) }, f)
}
