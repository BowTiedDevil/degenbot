#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
extern crate tracing;

use alloy_primitives::map::{FxBuildHasher, HashSet};
use evm2_jit_backend::{
    Backend, BackendConfig, BackendTypes, Builder, CallConv, IntCC, OptimizationLevel, Result,
    TailCallKind, TypeMethods, U256, eyre, eyre::OptionExt, format_bytes,
};
use llvm_sys::{
    LLVMCallConv, LLVMIntPredicate, LLVMLinkage, LLVMModuleFlagBehavior, LLVMTailCallKind,
    LLVMUnnamedAddr,
    error_handling::LLVMInstallFatalErrorHandler,
    support::LLVMParseCommandLineOptions,
    target_machine::{LLVMCodeGenFileType, LLVMCodeGenOptLevel, LLVMCodeModel, LLVMRelocMode},
};
use object::{Object, ObjectSymbol};
use std::{
    cell::Cell,
    ffi::{CStr, CString},
    fmt::{self, Write},
    iter,
    mem::ManuallyDrop,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use wrap::{
    Attribute, AttributeLoc, DICompileUnit, DebugInfoBuilder, Metadata, PassBuilderOptions, Target,
};

mod cpp;

mod dh;
pub mod orc;
mod wrap;
pub use wrap::{Block, Context, Function, Inst, LLVMString, Module, TargetMachine, Type, Value};

/// Branch weight for non-cold branches.
const DEFAULT_WEIGHT: u32 = 20000;

/// Largest function, in basic blocks, that [`EvmLlvmBackend::optimize_module`] runs LICM on.
///
/// LICM only pays off on hot loops in small functions. The cutoff comes from the sweep in
/// `EXPERIMENTS.md`.
const LICM_MAX_BLOCKS: u32 = 1000;

/// Current JIT memory usage counters.
///
/// Counters reflect live memory: bytes are added on compilation and
/// subtracted when JIT code is freed.
#[derive(Clone, Copy, Debug, Default)]
pub struct JitMemoryUsage {
    /// Bytes allocated for executable (code) sections.
    pub code_bytes: usize,
    /// Bytes allocated for non-executable (data) sections.
    pub data_bytes: usize,
}

impl JitMemoryUsage {
    /// Total bytes (code + data).
    pub const fn total_bytes(&self) -> usize {
        self.code_bytes + self.data_bytes
    }
}

impl fmt::Display for JitMemoryUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "total: {}, code: {}, data: {}",
            format_bytes(self.total_bytes()),
            format_bytes(self.code_bytes),
            format_bytes(self.data_bytes),
        )
    }
}

/// Atomic counters written by the C++ `MemoryUsagePlugin`.
///
/// Leaked into a `&'static` reference so the plugin (which lives as long as
/// the process-global LLJIT) always has valid pointers.
struct JitMemoryCounters {
    code_bytes: AtomicUsize,
    data_bytes: AtomicUsize,
}

impl JitMemoryCounters {
    fn get(&self) -> JitMemoryUsage {
        JitMemoryUsage {
            code_bytes: self.code_bytes.load(Ordering::Relaxed),
            data_bytes: self.data_bytes.load(Ordering::Relaxed),
        }
    }
}

/// Returns the current JIT memory usage.
///
/// Returns `None` if the JIT has not been initialized yet.
pub fn jit_memory_usage() -> Option<JitMemoryUsage> {
    GlobalOrcJit::try_get().map(|g| g.memory_counters.get())
}

/// Collect garbage on the global JIT.
pub fn global_gc() {
    if let Some(orc) = GlobalOrcJit::try_get() {
        orc.gc();
    }
}

type FxHashMap<K, V> = alloy_primitives::map::HashMap<K, V, FxBuildHasher>;

/// The LLVM-based EVM bytecode compiler backend.
#[derive(Debug)]
#[must_use]
pub struct EvmLlvmBackend {
    cx: &'static Context,
    dh: dh::DiagnosticHandlerGuard,
    /// LLVM debug info builder and compile unit, created lazily when `debug_file` is set.
    /// Dropped before `module`, which its builder refers to.
    di_state: Option<DiState>,
    bcx: wrap::Builder,
    module: Module,
    machine: TargetMachine,

    /// ORC JIT state. `None` in AOT mode.
    /// Dropped before `_tscx` so the JIT engine is disposed before the context.
    orc: Option<OrcJitState>,
    /// ORC thread-safe context that owns the LLVM context (JIT mode only).
    _tscx: Option<orc::ThreadSafeContext>,
    /// Non-owning context handle for JIT mode. See [`create_orc_context`].
    _cx_handle: Option<Box<ManuallyDrop<Context>>>,
    /// Owned context for AOT mode. `None` in JIT mode.
    _aot_cx: Option<Box<Context>>,

    ty_void: Type,
    ty_ptr: Type,
    ty_i1: Type,
    ty_i8: Type,
    ty_i32: Type,
    ty_i64: Type,
    ty_i256: Type,
    ty_isize: Type,

    aot: bool,
    backend_config: BackendConfig,
    scratch: String,
    /// Separate from `function_names` to have always increasing IDs.
    function_counter: u32,
    /// Persistent mapping from function ID to symbol name.
    function_names: FxHashMap<u32, String>,
}

// Thread-local slot for capturing compiled object buffers from the ObjectTransformLayer.
// Safe because LLJIT uses InPlaceTaskDispatcher — compilation runs inline on the calling thread.
thread_local! {
    static OBJ_CAPTURE: Cell<*mut Option<Vec<u8>>> = const { Cell::new(std::ptr::null_mut()) };
}

/// RAII guard that arms the thread-local object capture for the duration of a JIT commit.
struct ScopedObjCapture {
    prev: *mut Option<Vec<u8>>,
}

impl ScopedObjCapture {
    fn install(slot: &mut Option<Vec<u8>>) -> Self {
        Self { prev: OBJ_CAPTURE.replace(slot as *mut _) }
    }
}

impl Drop for ScopedObjCapture {
    fn drop(&mut self) {
        OBJ_CAPTURE.set(self.prev);
    }
}

fn obj_capture_transform(obj: &[u8]) -> Result<Option<Vec<u8>>, String> {
    OBJ_CAPTURE.with(|tls| {
        let ptr = tls.get();
        if !ptr.is_null() {
            unsafe { *ptr = Some(obj.to_vec()) };
        }
    });
    Ok(None)
}

/// Process-global shared LLJIT instance.
///
/// ORC/LLJIT is thread-safe and designed to be shared. Individual compilers get
/// their own [`JITDylib`](orc::JITDylibRef) for symbol isolation.
///
/// Builtin function pointers (absolute symbols) are defined once in a shared
/// `builtins` JITDylib. Each per-compiler JITDylib links against the builtins
/// JD so compiled code can resolve them without duplicating definitions.
struct GlobalOrcJit {
    /// Shared JITDylib containing absolute symbols for builtin functions.
    /// Added to each per-compiler JITDylib's link order.
    builtins_jd: orc::JITDylibRef,

    /// Symbols already defined in the builtins JD.
    builtins_defined: std::sync::Mutex<HashSet<CString>>,

    next_dylib_id: AtomicU64,

    /// Live JIT memory counters, updated by the C++ MemoryUsagePlugin.
    memory_counters: &'static JitMemoryCounters,

    jit: orc::LLJIT,
}

impl GlobalOrcJit {
    fn global() -> &'static OnceLock<Result<Self, String>> {
        static GLOBAL: OnceLock<Result<GlobalOrcJit, String>> = OnceLock::new();
        &GLOBAL
    }

    fn try_get() -> Option<&'static Self> {
        Self::global().get().and_then(|r| r.as_ref().ok())
    }

    fn get(
        debug_support: bool,
        profiling_support: bool,
        simple_perf: bool,
    ) -> Result<&'static Self> {
        let result = Self::global().get_or_init(|| {
            init().map_err(|e| e.to_string())?;
            let jit =
                orc::LLJIT::builder().concurrent_compiler().build().map_err(|e| e.to_string())?;
            jit.get_execution_session().set_default_error_reporter();
            jit.get_obj_transform_layer().set_transform(obj_capture_transform);

            // Register JIT debug info with debuggers and profilers.
            if debug_support && let Err(e) = jit.enable_debug_support() {
                warn!("failed to enable JIT debug support: {e}");
            }
            if profiling_support && let Err(e) = jit.enable_perf_support() {
                warn!("failed to enable JIT perf support: {e}");
            }
            if simple_perf && let Err(e) = jit.enable_simple_perf() {
                warn!("failed to enable simple perf map support: {e}");
            }

            // Track JIT memory usage.
            let memory_counters: &'static JitMemoryCounters =
                Box::leak(Box::new(JitMemoryCounters {
                    code_bytes: AtomicUsize::new(0),
                    data_bytes: AtomicUsize::new(0),
                }));
            orc::cvt(unsafe {
                cpp::evm2_jit_llvm_lljit_enable_memory_usage(
                    jit.as_inner(),
                    &memory_counters.code_bytes,
                    &memory_counters.data_bytes,
                )
            })
            .map_err(|e| e.to_string())?;

            let builtins_jd = jit.get_execution_session().create_bare_jit_dylib(c"revmc.builtins");

            Ok(Self {
                jit,
                builtins_jd,
                builtins_defined: Default::default(),
                next_dylib_id: Default::default(),
                memory_counters,
            })
        });
        match result {
            Ok(g) => Ok(g),
            Err(e) => Err(eyre::eyre!("{e}")),
        }
    }

    /// Creates a fresh JITDylib for a compiler.
    fn create_jit_dylib(&self) -> Result<orc::JITDylibRef> {
        // Create the process symbol generator first, so a failure leaves no dylib behind.
        // It lets the JITDylib resolve libc and other process-level symbols (e.g. printf,
        // memcpy) that JIT-compiled code may reference.
        let prefix = self.jit.get_global_prefix();
        let generator = orc::DefinitionGenerator::for_current_process(prefix).map_err(error_msg)?;
        let id = self.next_dylib_id.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("evm2_jit.compiler.{id}")).unwrap();
        let es = self.jit.get_execution_session();
        let jd = es.create_bare_jit_dylib(&name);
        // Link against the builtins JD so compiled code can resolve builtin symbols.
        jd.add_to_link_order(self.builtins_jd);
        jd.add_generator(generator);
        Ok(jd)
    }

    /// Removes a JITDylib from the ExecutionSession, freeing all its resources.
    fn remove_jit_dylib(&self, jd: orc::JITDylibRef) {
        if let Err(e) = self.jit.get_execution_session().remove_jit_dylib(jd) {
            error!("failed to remove JITDylib: {e}");
        }
        // Reclaim unreferenced interned strings after removing symbols.
        self.gc();
    }

    /// Defines absolute symbols in the shared builtins JITDylib, skipping any
    /// that are already defined.
    fn define_builtins(&self, symbols: &[(CString, usize)]) {
        if symbols.is_empty() {
            return;
        }
        let mut defined = self.builtins_defined.lock().unwrap();
        let new_syms: Vec<_> = symbols
            .iter()
            .filter(|(name, _)| defined.insert(name.clone()))
            .map(|(name, addr)| {
                orc::SymbolMapPair::new(
                    self.jit.mangle_and_intern(name),
                    orc::EvaluatedSymbol::new(
                        *addr as u64,
                        orc::SymbolFlags::none().exported().callable(),
                    ),
                )
            })
            .collect();
        drop(defined);
        if !new_syms.is_empty()
            && let Err((e, _)) =
                self.builtins_jd.define(orc::MaterializationUnit::absolute_symbols(new_syms))
        {
            error!("failed to define builtins: {e}");
        }
    }

    fn gc(&self) {
        self.jit.get_execution_session().get_symbol_string_pool().clear_dead_entries();
    }
}

/// Shared guard that keeps a JITDylib alive.
///
/// The JITDylib will not be cleared or returned to the pool until the last
/// `JitDylibGuard` is dropped. Runtime callers holding JIT function pointers
/// must retain a clone of this guard to prevent the code from being freed.
#[allow(missing_debug_implementations)]
pub struct JitDylibGuard {
    global: &'static GlobalOrcJit,
    jd: orc::JITDylibRef,
}

impl Drop for JitDylibGuard {
    fn drop(&mut self) {
        self.global.remove_jit_dylib(self.jd);
    }
}

/// ORC JIT state for the LLVM backend (JIT mode only).
///
/// The LLVM context is owned separately (via `tscx`) and persists across JIT resets.
/// Each compiler gets its own JITDylib in the global LLJIT for symbol isolation.
struct OrcJitState {
    /// Reference to the global LLJIT instance.
    global: &'static GlobalOrcJit,
    /// Shared guard that owns the JITDylib. The JD is not recycled until all
    /// `Arc<JitDylibGuard>` holders (including external callers) are dropped.
    ///
    /// Declared before `loaded_trackers` so it drops first: `removeJITDylib`
    /// (called in `JitDylibGuard::drop`) clears the JITDylib internally and
    /// must run while the tracker handles are still live.
    jd_guard: Arc<JitDylibGuard>,
    /// Functions in the current staging module (not yet committed to JIT).
    staged_functions: FxHashMap<u32, Function>,
    /// Absolute symbols collected during translation, flushed to the global
    /// builtins JITDylib before commit.
    pending_symbols: Vec<(CString, usize)>,
    /// Resource trackers for committed JIT modules, used for code removal.
    loaded_trackers: Vec<orc::ResourceTracker>,
    /// Maps committed function ID → index into `loaded_trackers`.
    committed_functions: FxHashMap<u32, usize>,
    /// Cached object buffer from the last `commit_staged_module`, captured via
    /// ObjectTransformLayer.
    last_compiled_object: Option<Vec<u8>>,
    /// Counter for throttling `SymbolStringPool::clearDeadEntries()` calls.
    clear_pool_counter: u32,
}

impl fmt::Debug for OrcJitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrcJitState")
            .field("staged_functions", &self.staged_functions.len())
            .field("pending_symbols", &self.pending_symbols.len())
            .field("loaded_trackers", &self.loaded_trackers.len())
            .field("committed_functions", &self.committed_functions.len())
            .finish_non_exhaustive()
    }
}

impl OrcJitState {
    fn new(debug_support: bool, profiling_support: bool, simple_perf: bool) -> Result<Self> {
        let global = GlobalOrcJit::get(debug_support, profiling_support, simple_perf)?;
        let jd = global.create_jit_dylib()?;
        let jd_guard = Arc::new(JitDylibGuard { global, jd });
        Ok(Self {
            global,
            staged_functions: FxHashMap::default(),
            pending_symbols: Vec::new(),
            loaded_trackers: Vec::new(),
            committed_functions: FxHashMap::default(),
            last_compiled_object: None,
            jd_guard,
            clear_pool_counter: 0,
        })
    }

    /// Clears all code and symbols from this compiler's JITDylib.
    fn clear(&mut self) -> Result<()> {
        self.staged_functions.clear();
        self.pending_symbols.clear();
        self.committed_functions.clear();
        self.last_compiled_object = None;
        // Clear the JITDylib before dropping resource trackers: `LLVMOrcJITDylibClear` calls
        // remove on all trackers associated with the dylib and must run while the tracker
        // handles are still live.
        self.jd().clear().map_err(error_msg)?;
        self.loaded_trackers.clear();
        Ok(())
    }

    /// Periodically clears dead entries from the global SymbolStringPool.
    ///
    /// `clearDeadEntries` is O(pool_size), so we throttle it to avoid
    /// O(N²) total cost over many compilations.
    fn maybe_clear_dead_pool_entries(&mut self) {
        self.clear_pool_counter += 1;
        if self.clear_pool_counter.is_multiple_of(256) {
            self.global.gc();
        }
    }

    fn jd(&self) -> orc::JITDylibRef {
        self.jd_guard.jd
    }
}

fn link_jit_object_in_dylib(
    orc: &OrcJitState,
    jd: orc::JITDylibRef,
    symbol_name: &CStr,
    object: &[u8],
    symbols: &[(CString, usize)],
) -> Result<(usize, orc::ResourceTracker)> {
    orc.global.define_builtins(symbols);
    let tracker = jd.create_resource_tracker();
    orc.global.jit.add_object_with_rt(symbol_name, object, &tracker).map_err(error_msg)?;
    let addr = orc.global.jit.lookup_in(jd, symbol_name).map_err(error_msg)?;
    Ok((addr, tracker))
}

/// Creates an ORC-owned LLVM context, returning a `&'static` reference to it.
///
/// In JIT mode, ORC owns the context via a [`orc::ThreadSafeContext`] so that modules can be
/// safely transferred to the JIT. A non-owning handle ([`ManuallyDrop<Context>`]) is
/// heap-allocated to provide a stable address for the `&'static` reference.
fn create_orc_context() -> (&'static Context, orc::ThreadSafeContext, Box<ManuallyDrop<Context>>) {
    let cx = Context::create();
    let raw = cx.raw();
    let tscx = orc::ThreadSafeContext::from_context(cx);
    // SAFETY: The TSC now owns the context. `from_context` uses `ManuallyDrop` internally,
    // so the LLVM context is still valid — ownership was just transferred to the TSC.
    let cx_handle = Box::new(ManuallyDrop::new(unsafe { Context::new(raw) }));
    // SAFETY: The Box provides a stable heap address. The context is valid as long as
    // the TSC lives.
    let cx: &'static Context = unsafe { &*(&**cx_handle as *const Context) };
    (cx, tscx, cx_handle)
}

/// LLVM debug info state for a module.
struct DiState {
    dibuilder: DebugInfoBuilder,
    compile_unit: DICompileUnit,
    finalized: bool,
}

impl fmt::Debug for DiState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiState").field("finalized", &self.finalized).finish()
    }
}

// SAFETY: In JIT mode the LLVM context is owned by an ORC ThreadSafeContext.
// In AOT mode the context is owned by `_aot_cx` (a Box<Context>).
// Both travel with the backend, so no thread-local or shared state is referenced.
unsafe impl Send for EvmLlvmBackend {}

impl EvmLlvmBackend {
    /// Creates a new LLVM backend for the host machine.
    #[instrument(name = "new_llvm_backend", level = "debug", skip_all)]
    pub fn new(aot: bool) -> Result<Self> {
        let config = BackendConfig::default();
        init()?;

        let target_info = TargetInfo::new()?;
        let target = &target_info.target;
        let machine = target
            .create_target_machine(
                &target_info.triple,
                &target_info.cpu,
                &target_info.features,
                convert_opt_level(config.opt_level),
                if aot { LLVMRelocMode::LLVMRelocPIC } else { LLVMRelocMode::LLVMRelocStatic },
                LLVMCodeModel::LLVMCodeModelDefault,
            )
            .ok_or_eyre("failed to create target machine")?;

        // In JIT mode, ORC owns the context via a ThreadSafeContext so that modules can be
        // safely transferred to the JIT without double-ownership issues with the TLS context.
        // In AOT mode, we use an owned Box<Context> so the backend is safely Send.
        let (cx, tscx, cx_handle, aot_cx) = if aot {
            let aot_cx = Box::new(Context::create());
            // SAFETY: The Box provides a stable heap address. The context is valid as long as
            // `_aot_cx` lives, and it is dropped after all LLVM objects due to field ordering.
            let cx: &'static Context = unsafe { &*(&*aot_cx as *const Context) };
            (cx, None, None, Some(aot_cx))
        } else {
            if !target.has_jit() {
                return Err(eyre::eyre!("target {:?} does not support JIT", target.get_name()));
            }
            if !target.has_target_machine() {
                return Err(eyre::eyre!(
                    "target {:?} does not have target machine",
                    target.get_name()
                ));
            }

            let (cx, tscx, cx_handle) = create_orc_context();
            (cx, Some(tscx), Some(cx_handle), None)
        };

        let module = create_module(cx, &machine, aot)?;
        let bcx = cx.create_builder();

        let ty_void = cx.void_type();
        let ty_i1 = cx.bool_type();
        let ty_i8 = cx.i8_type();
        let ty_i32 = cx.i32_type();
        let ty_i64 = cx.i64_type();
        let ty_i256 = cx.int_type(256);
        let ty_isize = cx.ptr_sized_int_type(&machine.get_target_data());
        let ty_ptr = cx.ptr_type();
        Ok(Self {
            cx,
            dh: dh::DiagnosticHandlerGuard::new(cx),
            bcx,
            module,
            machine,
            orc: None,
            _tscx: tscx,
            _cx_handle: cx_handle,
            _aot_cx: aot_cx,
            ty_void,
            ty_i1,
            ty_i8,
            ty_i32,
            ty_i64,
            ty_i256,
            ty_isize,
            ty_ptr,
            aot,
            backend_config: config,
            scratch: String::new(),
            function_counter: 0,
            function_names: FxHashMap::default(),
            di_state: None,
        })
    }

    /// Returns the LLVM context.
    #[inline]
    pub const fn cx(&self) -> &Context {
        self.cx
    }

    #[inline]
    const fn module(&self) -> &Module {
        &self.module
    }

    fn ensure_orc(&mut self) -> Result<&mut OrcJitState> {
        if self.orc.is_none() {
            self.orc = Some(OrcJitState::new(
                self.backend_config.debug_support,
                self.backend_config.profiling_support,
                self.backend_config.simple_perf,
            )?);
        }
        Ok(self.orc.as_mut().unwrap())
    }

    fn fn_type(&self, ret: Option<Type>, params: &[Type]) -> Type {
        ret.unwrap_or(self.ty_void).fn_type(params, false)
    }

    /// Returns the given name if IR output is being dumped, otherwise an empty string.
    /// LLVM skips internal name processing for empty names, avoiding overhead when names
    /// are not needed for readability.
    #[inline]
    const fn name<'a>(&self, name: &'a str) -> &'a str {
        if self.backend_config.is_dumping { name } else { "" }
    }

    fn id_to_name(&self, id: u32) -> &str {
        &self.function_names[&id]
    }

    /// Lazily initializes the debug info builder and compile unit for the module.
    fn ensure_di_state(&mut self) {
        if self.di_state.is_some() {
            return;
        }
        let Some(debug_file) = &self.backend_config.debug_file else { return };

        let filename =
            debug_file.file_name().map(|f| f.to_string_lossy()).unwrap_or_default().into_owned();
        let directory =
            debug_file.parent().map(|p| p.to_string_lossy()).unwrap_or_default().into_owned();

        // Add required module flags for debug info.
        self.module().add_basic_value_flag(
            "Debug Info Version",
            LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorWarning,
            self.ty_i32.const_int(wrap::debug_metadata_version() as u64, false),
        );
        self.module().add_basic_value_flag(
            "Dwarf Version",
            LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorWarning,
            self.ty_i32.const_int(5, false),
        );

        let opt_level = self.backend_config.opt_level;
        let is_optimized = opt_level != OptimizationLevel::None;
        let mut flags = Vec::new();
        flags.push(match opt_level {
            OptimizationLevel::None => "-O0",
            OptimizationLevel::Less
            | OptimizationLevel::Default
            | OptimizationLevel::Aggressive => "-O2",
        });
        flags.push(if self.aot { "--aot" } else { "--jit" });
        let flags = flags.join(" ");

        let (dibuilder, compile_unit) = DebugInfoBuilder::new(
            self.module(),
            &filename,
            &directory,
            "evm2_jit",
            is_optimized,
            &flags,
        );

        self.di_state = Some(DiState { dibuilder, compile_unit, finalized: false });
    }

    /// Commits the current staging module to the ORC JIT if there are pending functions.
    fn commit_staged_module(&mut self) -> Result<()> {
        if self.aot || self.orc.as_ref().is_none_or(|o| o.staged_functions.is_empty()) {
            return Ok(());
        }

        self.di_state = None;

        let new_module = create_module(self.cx, &self.machine, self.aot)?;
        let old_module = std::mem::replace(&mut self.module, new_module);

        let tscx = self._tscx.as_ref().expect("missing ThreadSafeContext");
        let orc = self.orc.as_mut().unwrap();

        // Flush pending absolute symbols to the shared builtins JITDylib.
        let pending = &mut orc.pending_symbols;
        if !pending.is_empty() {
            orc.global.define_builtins(pending);
            pending.clear();
        }

        let tracker = orc.jd().create_resource_tracker();

        orc.global
            .jit
            .add_module_with_rt(tscx.create_module(old_module), &tracker)
            .map_err(error_msg)?;

        let tracker_idx = orc.loaded_trackers.len();
        for &id in orc.staged_functions.keys() {
            orc.committed_functions.insert(id, tracker_idx);
        }
        orc.loaded_trackers.push(tracker);
        orc.staged_functions.clear();
        Ok(())
    }

    /// Clears all code from this compiler's JITDylib, freeing JIT-compiled code.
    /// The LLVM context and global LLJIT are reused across resets.
    fn reset_jit(&mut self) -> Result<()> {
        self.function_names.clear();
        self.di_state = None;

        if let Some(orc) = &mut self.orc {
            orc.clear()?;
        }

        self.module = create_module(self.cx, &self.machine, self.aot)?;

        Ok(())
    }

    /// Removes the committed module that contains `id`, with the code of all its functions.
    fn remove_committed_module(&mut self, id: u32) -> Result<()> {
        let Some(orc) = &mut self.orc else { return Ok(()) };
        let Some(tracker_idx) = orc.committed_functions.remove(&id) else { return Ok(()) };
        orc.committed_functions.retain(|_, idx| *idx != tracker_idx);
        orc.loaded_trackers[tracker_idx].remove().map_err(error_msg)
    }

    /// Returns pending absolute symbols collected while translating the current JIT module.
    pub fn pending_symbol_names(&self) -> Vec<CString> {
        self.orc
            .as_ref()
            .map(|orc| orc.pending_symbols.iter().map(|(name, _)| name.clone()).collect())
            .unwrap_or_default()
    }

    /// Links a relocatable object into this backend's JITDylib and returns the function address
    /// and resource tracker that owns the linked code.
    pub fn link_jit_object(
        &mut self,
        symbol_name: &CStr,
        object: &[u8],
        symbols: &[(CString, usize)],
    ) -> Result<(usize, orc::ResourceTracker)> {
        let orc = self.ensure_orc()?;
        let (addr, tracker) =
            link_jit_object_in_dylib(orc, orc.jd(), symbol_name, object, symbols)?;
        orc.loaded_trackers.push(tracker);
        Ok((addr, orc.loaded_trackers.pop().unwrap()))
    }

    /// Links a relocatable object into a fresh JITDylib and returns the function address,
    /// resource tracker, and guard for the JITDylib that owns the linked code.
    pub fn link_jit_object_in_fresh_dylib(
        &mut self,
        symbol_name: &CStr,
        object: &[u8],
        symbols: &[(CString, usize)],
    ) -> Result<(usize, orc::ResourceTracker, Arc<JitDylibGuard>)> {
        let orc = self.ensure_orc()?;
        let jd = orc.global.create_jit_dylib()?;
        let jd_guard = Arc::new(JitDylibGuard { global: orc.global, jd });
        let (addr, tracker) = link_jit_object_in_dylib(orc, jd, symbol_name, object, symbols)?;
        Ok((addr, tracker, jd_guard))
    }

    /// Pops and returns the [`ResourceTracker`](orc::ResourceTracker) for the last committed
    /// JIT module.
    ///
    /// Each call to [`jit_function`](evm2_jit_backend::Backend::jit_function) commits a module
    /// with its own tracker. The caller takes ownership and is responsible for calling
    /// `tracker.remove()` to free the machine code when it is no longer needed.
    ///
    /// The caller must also hold a [`JitDylibGuard`] to prevent the owning JITDylib from
    /// being recycled before the tracker is removed.
    ///
    /// Returns `None` in AOT mode or if no modules have been committed.
    pub fn take_last_resource_tracker(&mut self) -> Option<orc::ResourceTracker> {
        let orc = self.orc.as_mut()?;
        let tracker = orc.loaded_trackers.pop()?;
        let tracker_idx = orc.loaded_trackers.len();
        orc.committed_functions.retain(|_, idx| *idx != tracker_idx);
        Some(tracker)
    }

    /// Returns a shared handle that keeps this backend's JITDylib alive.
    ///
    /// The JITDylib will not be cleared or recycled until all `JitDylibGuard` handles
    /// (and the backend itself) are dropped. Callers holding JIT function pointers must
    /// retain a guard to prevent the backing code from being freed.
    pub fn jit_dylib_guard(&self) -> Arc<JitDylibGuard> {
        Arc::clone(&self.orc.as_ref().expect("jit_dylib_guard called in AOT mode").jd_guard)
    }
}

impl BackendTypes for EvmLlvmBackend {
    type Type = Type;
    type Value = Value;
    type StackSlot = Value;
    type BasicBlock = Block;
    type Function = Function;
    type Inst = Inst;
}

impl TypeMethods for EvmLlvmBackend {
    fn type_ptr(&self) -> Self::Type {
        self.ty_ptr
    }

    fn type_ptr_sized_int(&self) -> Self::Type {
        self.ty_isize
    }

    fn type_int(&self, bits: u32) -> Self::Type {
        match bits {
            1 => self.ty_i1,
            8 => self.ty_i8,
            16 => self.cx.i16_type(),
            32 => self.ty_i32,
            64 => self.ty_i64,
            128 => self.cx.i128_type(),
            256 => self.ty_i256,
            bits => self.cx.int_type(bits),
        }
    }

    fn type_array(&self, ty: Self::Type, size: u32) -> Self::Type {
        ty.array_type(size)
    }

    fn type_bit_width(&self, ty: Self::Type) -> u32 {
        ty.bit_width()
    }
}

impl Backend for EvmLlvmBackend {
    type Builder<'a>
        = EvmLlvmBuilder<'a>
    where
        Self: 'a;
    type FuncId = u32;

    fn ir_extension(&self) -> &'static str {
        "ll"
    }

    fn set_module_name(&mut self, name: &str) {
        self.module().set_name(name);
    }

    fn config(&self) -> &BackendConfig {
        &self.backend_config
    }

    fn apply_config(&mut self, config: BackendConfig) {
        if self.backend_config.is_dumping != config.is_dumping {
            self.machine.set_asm_verbosity(config.is_dumping);
        }
        if self.backend_config.opt_level != config.opt_level {
            unsafe {
                cpp::evm2_jit_llvm_target_machine_set_opt_level(
                    self.machine.as_mut_ptr(),
                    convert_opt_level(config.opt_level),
                );
            }
        }
        self.backend_config = config;
    }

    fn finalize_debug_info(&mut self) -> Result<()> {
        if let Some(di) = &mut self.di_state
            && !di.finalized
        {
            di.dibuilder.finalize();
            di.finalized = true;
        }
        Ok(())
    }

    fn is_aot(&self) -> bool {
        self.aot
    }

    fn function_name_is_unique(&self, name: &str) -> bool {
        self.module().get_function(name).is_none()
    }

    fn dump_ir(&mut self, path: &Path) -> Result<()> {
        self.module().print_to_file(path).map_err(error_msg)
    }

    fn dump_disasm(&mut self, path: &Path) -> Result<()> {
        let result = self
            .machine
            .write_to_file(self.module(), LLVMCodeGenFileType::LLVMAssemblyFile, path)
            .map_err(error_msg);
        self.dh.check(result)
    }

    fn build_function(
        &mut self,
        name: &str,
        ret: Option<Self::Type>,
        params: &[Self::Type],
        param_names: &[&str],
        linkage: evm2_jit_backend::Linkage,
    ) -> Result<(Self::Builder<'_>, Self::FuncId)> {
        if !self.aot {
            self.ensure_orc()?;
        }
        let (id, function) = if let Some((&id, _fname)) =
            self.function_names.iter().find(|(_k, fname)| fname.as_str() == name)
            && let Some(orc) = &self.orc
            && let Some(function) = orc.staged_functions.get(&id).copied()
            && let Some(function2) = self.module().get_function(name)
            && function == function2
        {
            self.bcx.position_at_end(function.get_first_basic_block().unwrap());
            (id, function)
        } else {
            let fn_type = self.fn_type(ret, params);
            let function =
                self.module().add_function(name, fn_type, Some(convert_linkage(linkage)));
            cpp::set_dso_local(function);
            if self.backend_config.is_dumping {
                for (i, &name) in param_names.iter().enumerate() {
                    function.get_nth_param(i as u32).expect(name).set_name(self.name(name));
                }
            }

            let entry = self.cx.append_basic_block(function, self.name("entry"));
            self.bcx.position_at_end(entry);

            let id = self.function_counter;
            self.function_counter += 1;
            self.function_names.insert(id, name.to_string());
            if let Some(orc) = &mut self.orc {
                orc.staged_functions.insert(id, function);
            }
            (id, function)
        };

        // Attach debug info subprogram if debug is active.
        self.ensure_di_state();
        let debug_scope = self.di_state.as_ref().map(|di| {
            let subprogram = di.dibuilder.create_subprogram(
                di.compile_unit,
                name,
                self.backend_config.opt_level != OptimizationLevel::None,
            );
            function.set_subprogram(subprogram);
            subprogram
        });

        let builder = EvmLlvmBuilder { backend: self, function, debug_scope };
        Ok((builder, id))
    }

    fn verify_module(&mut self) -> Result<()> {
        self.module().verify().map_err(error_msg)
    }

    fn optimize_module(&mut self) -> Result<()> {
        // We use a custom pipeline instead of `default<O3>` because GVN is extremely slow on
        // the huge single-function modules that EVM compilation produces. Replacing GVN with
        // `early-cse` + `sccp` achieves equivalent or better code size and runtime performance
        // at ~5x faster compile time.
        //
        // The standard `default<O1>` is also slow (~730ms on snailtracer) because the loop
        // analysis infrastructure (LoopInfo, DominatorTree, MemorySSA, LCSSA) is expensive to
        // compute on functions with thousands of basic blocks.
        //
        // LICM (Loop Invariant Code Motion) helps tight EVM loops by hoisting gas counter and
        // stack slot loads/stores into registers. It only pays off on small functions, and its
        // compile cost grows faster than the function: see `LICM_MAX_BLOCKS`.
        //
        // Can be overridden with `EVM2_JIT_PASSES` env var for experimentation.
        // From `opt --help`, `-passes`.

        static PASSES_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();
        static PASSES: OnceLock<String> = OnceLock::new();
        static PASSES_WITH_LICM: OnceLock<String> = OnceLock::new();

        let passes = PASSES_OVERRIDE.get_or_init(|| std::env::var("EVM2_JIT_PASSES").ok());
        let passes = passes.as_deref().unwrap_or_else(|| match self.backend_config.opt_level {
            OptimizationLevel::None => "default<O0>",
            OptimizationLevel::Less
            | OptimizationLevel::Default
            | OptimizationLevel::Aggressive => {
                let max_blocks =
                    self.module.get_functions().map(|f| f.count_basic_blocks()).max().unwrap_or(0);
                let with_licm = max_blocks <= LICM_MAX_BLOCKS;
                let passes = if with_licm { &PASSES_WITH_LICM } else { &PASSES };
                passes.get_or_init(|| build_pass_pipeline(with_licm))
            }
        });
        let opts = PassBuilderOptions::create();
        let result = self.module().run_passes(passes, &self.machine, &opts).map_err(error_msg);
        self.dh.check(result)
    }

    fn write_object<W: std::io::Write>(&mut self, mut w: W) -> Result<()> {
        let buffer = self
            .machine
            .write_to_memory_buffer(self.module(), LLVMCodeGenFileType::LLVMObjectFile)
            .map_err(error_msg);
        let buffer = self.dh.check(buffer)?;
        w.write_all(buffer.as_slice())?;
        Ok(())
    }

    fn jit_function(&mut self, id: Self::FuncId) -> Result<usize> {
        self.ensure_orc()?;
        self.commit_staged_module()?;
        let name_str = self.id_to_name(id);
        let name = CString::new(name_str).unwrap();
        let orc = self.orc.as_mut().unwrap();
        // Capture the compiled object buffer during lookup. LLJIT compiles lazily:
        // add_module_with_rt just registers the module, actual compilation happens
        // in lookup_in when the symbol is first requested.
        let mut captured = None;
        let _guard = ScopedObjCapture::install(&mut captured);
        let addr = orc.global.jit.lookup_in(orc.jd(), &name).map_err(error_msg);
        drop(_guard);
        let compiled = addr.is_ok();
        let addr = match self.dh.check(addr) {
            Ok(addr) => addr,
            Err(e) => {
                // ORC compiled the whole module and keeps it, so later lookups would return the
                // broken code without the error. Remove the module's code but keep the function
                // names, so a later lookup fails with an error instead of a missing name.
                if compiled && let Err(remove_err) = self.remove_committed_module(id) {
                    warn!("failed to remove module after an LLVM error: {remove_err}");
                }
                return Err(e);
            }
        };
        if captured.is_some() {
            self.orc.as_mut().unwrap().last_compiled_object = captured;
        }
        Ok(addr)
    }

    fn function_name(&self, id: Self::FuncId) -> Option<&str> {
        self.function_names.get(&id).map(|s| s.as_str())
    }

    fn function_sizes(&self) -> Vec<(String, usize)> {
        let Some(orc) = &self.orc else { return Vec::new() };
        let Some(data) = orc.last_compiled_object.as_deref() else { return Vec::new() };
        let Ok(obj) = object::File::parse(data) else { return Vec::new() };

        let mut result: Vec<_> = obj
            .symbols()
            .filter(|sym| sym.is_definition())
            .filter_map(|sym| {
                let name = sym.name().ok()?;
                self.function_names.values().any(|n| n == name).then_some(())?;
                Some((name.to_string(), sym.size() as usize))
            })
            .collect();
        result.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
        result
    }

    fn clear_ir(&mut self) -> Result<()> {
        self.di_state = None;
        self.function_names.clear();
        self.module = create_module(self.cx, &self.machine, self.aot)?;
        if let Some(orc) = &mut self.orc {
            orc.staged_functions.clear();
            orc.pending_symbols.clear();
            orc.maybe_clear_dead_pool_entries();
        }
        Ok(())
    }

    unsafe fn free_function(&mut self, id: Self::FuncId) -> Result<()> {
        if let Some(orc) = &mut self.orc {
            // Remove from staging if not yet committed.
            if let Some(function) = orc.staged_functions.remove(&id) {
                unsafe { function.delete() };
            }

            // Remove from committed trackers.
            if let Some(tracker_idx) = orc.committed_functions.remove(&id) {
                orc.loaded_trackers[tracker_idx].remove().map_err(error_msg)?;
                // Also remove co-committed functions since removing the
                // tracker frees all symbols in that module.
                orc.committed_functions.retain(|co_id, idx| {
                    if *idx == tracker_idx {
                        self.function_names.remove(co_id);
                        false
                    } else {
                        true
                    }
                });
                // Note: the tracker slot becomes dead but indices of later
                // trackers are unchanged, keeping the map consistent.
            }
        }
        self.function_names.remove(&id);
        Ok(())
    }

    unsafe fn free_all_functions(&mut self) -> Result<()> {
        self.reset_jit()
    }
}

/// Cached target information for the host machine.
#[derive(Debug)]
struct TargetInfo {
    triple: CString,
    target: Target,
    cpu: String,
    features: String,
}

impl TargetInfo {
    fn new() -> Result<&'static Self> {
        static HOST_TARGET_INFO: OnceLock<Result<TargetInfo, String>> = OnceLock::new();
        HOST_TARGET_INFO
            .get_or_init(|| {
                let triple = TargetMachine::get_default_triple().to_owned();
                let target = Target::from_triple(&triple)
                    .map_err(|e| e.to_string_lossy().trim_end().to_string())?;
                let cpu = TargetMachine::get_host_cpu_name().to_string_lossy().into_owned();
                let features =
                    TargetMachine::get_host_cpu_features().to_string_lossy().into_owned();
                Ok(Self { target, triple, cpu, features })
            })
            .as_ref()
            .map_err(|e| eyre::eyre!("{e}"))
    }
}

/// The LLVM-based EVM bytecode compiler function builder.
#[derive(Debug)]
#[must_use]
pub struct EvmLlvmBuilder<'a> {
    backend: &'a mut EvmLlvmBackend,
    function: Function,
    debug_scope: Option<Metadata>,
}

impl std::ops::Deref for EvmLlvmBuilder<'_> {
    type Target = EvmLlvmBackend;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.backend
    }
}

impl std::ops::DerefMut for EvmLlvmBuilder<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.backend
    }
}

impl EvmLlvmBuilder<'_> {
    #[allow(dead_code)]
    fn extract_value(&mut self, value: Value, index: u32, name: &str) -> Value {
        self.bcx.build_extract_value(value, index, self.name(name))
    }

    fn memcpy_inner(&mut self, dst: Value, src: Value, len: Value, inline: bool) {
        let volatile = self.bool_const(false);
        let len_ty = len.get_type();
        let name =
            format!("llvm.memcpy{}.p0.p0.{}", if inline { ".inline" } else { "" }, fmt_ty(len_ty),);
        let memcpy = self.get_or_add_function(&name, |this| {
            this.ty_void.fn_type(&[this.ty_ptr, this.ty_ptr, len_ty, this.ty_i1], false)
        });
        self.bcx.build_call(memcpy, &[dst, src, len, volatile], "");
    }

    #[allow(dead_code)]
    fn call_overflow_function(&mut self, name: &str, lhs: Value, rhs: Value) -> (Value, Value) {
        let f = self.get_overflow_function(name, lhs.get_type());
        let result = self.call(f, &[lhs, rhs]).unwrap();
        (self.extract_value(result, 0, "result"), self.extract_value(result, 1, "overflow"))
    }

    #[allow(dead_code)]
    fn get_overflow_function(&mut self, name: &str, ty: Type) -> Function {
        let name = format!("llvm.{name}.with.overflow.{}", fmt_ty(ty));
        self.get_or_add_function(&name, |this| {
            this.fn_type(Some(this.cx().struct_type(&[ty, this.ty_i1], false)), &[ty, ty])
        })
    }

    fn get_sat_function(&mut self, name: &str, ty: Type) -> Function {
        let name = format!("llvm.{name}.sat.{}", fmt_ty(ty));
        self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty, ty]))
    }

    fn get_or_add_function(
        &mut self,
        name: &str,
        mk_ty: impl FnOnce(&mut Self) -> Type,
    ) -> Function {
        self.module().get_function(name).unwrap_or_else(|| {
            let ty = mk_ty(self);
            self.module().add_function(name, ty, None)
        })
    }

    fn set_branch_weights(&self, inst: Inst, weights: impl IntoIterator<Item = u32>) {
        let weights = weights.into_iter();
        let mut values = Vec::<Metadata>::with_capacity(1 + weights.size_hint().1.unwrap());
        values.push(self.cx().metadata_string("branch_weights"));
        for weight in weights {
            values.push(self.ty_i32.const_int(weight as u64, false).as_metadata());
        }
        let metadata = self.cx().metadata_node(&values);
        let kind_id = self.cx().get_kind_id("prof");
        inst.set_metadata(metadata, kind_id);
    }
}

impl BackendTypes for EvmLlvmBuilder<'_> {
    type Type = <EvmLlvmBackend as BackendTypes>::Type;
    type Value = <EvmLlvmBackend as BackendTypes>::Value;
    type StackSlot = <EvmLlvmBackend as BackendTypes>::StackSlot;
    type BasicBlock = <EvmLlvmBackend as BackendTypes>::BasicBlock;
    type Function = <EvmLlvmBackend as BackendTypes>::Function;
    type Inst = <EvmLlvmBackend as BackendTypes>::Inst;
}

impl TypeMethods for EvmLlvmBuilder<'_> {
    fn type_ptr(&self) -> Self::Type {
        self.backend.type_ptr()
    }

    fn type_ptr_sized_int(&self) -> Self::Type {
        self.backend.type_ptr_sized_int()
    }

    fn type_int(&self, bits: u32) -> Self::Type {
        self.backend.type_int(bits)
    }

    fn type_array(&self, ty: Self::Type, size: u32) -> Self::Type {
        self.backend.type_array(ty, size)
    }

    fn type_bit_width(&self, ty: Self::Type) -> u32 {
        self.backend.type_bit_width(ty)
    }
}

impl Builder for EvmLlvmBuilder<'_> {
    fn create_block(&mut self, name: &str) -> Self::BasicBlock {
        self.cx().append_basic_block(self.function, self.name(name))
    }

    fn create_block_after(&mut self, after: Self::BasicBlock, name: &str) -> Self::BasicBlock {
        self.cx().insert_basic_block_after(after, self.name(name))
    }

    fn switch_to_block(&mut self, block: Self::BasicBlock) {
        self.bcx.position_at_end(block);
    }

    fn seal_block(&mut self, block: Self::BasicBlock) {
        let _ = block;
        // Nothing to do.
    }

    fn seal_all_blocks(&mut self) {
        // Nothing to do.
    }

    fn current_block(&mut self) -> Option<Self::BasicBlock> {
        self.bcx.get_insert_block()
    }

    fn block_addr(&mut self, block: Self::BasicBlock) -> Option<Self::Value> {
        unsafe { block.get_address() }
    }

    fn add_comment_to_current_inst(&mut self, comment: &str) {
        if !self.backend_config.is_dumping {
            return;
        }
        let Some(block) = self.current_block() else { return };
        let Some(ins) = block.get_last_instruction() else { return };
        let metadata = self.cx().metadata_string(comment);
        let metadata = self.cx().metadata_node(&[metadata]);
        ins.set_metadata(metadata, self.cx().get_kind_id("annotation"));
    }

    fn set_debug_location(&mut self, line: u32, col: u32) {
        let Some(scope) = self.debug_scope else { return };
        let Some(di) = &self.di_state else { return };
        let loc = di.dibuilder.create_debug_location(self.cx(), line, col, scope);
        self.bcx.set_current_debug_location(loc);
    }

    fn clear_debug_location(&mut self) {
        if self.debug_scope.is_some() {
            self.bcx.unset_current_debug_location();
        }
    }

    fn fn_param(&mut self, index: usize) -> Self::Value {
        self.function.get_nth_param(index as _).unwrap()
    }

    fn num_fn_params(&self) -> usize {
        self.function.count_params() as usize
    }

    fn bool_const(&mut self, value: bool) -> Self::Value {
        self.ty_i1.const_int(value as u64, false)
    }

    fn iconst(&mut self, ty: Self::Type, value: i64) -> Self::Value {
        ty.const_int(value as u64, value.is_negative())
    }

    fn uconst(&mut self, ty: Self::Type, value: u64) -> Self::Value {
        ty.const_int(value, false)
    }

    fn iconst_256(&mut self, value: impl TryInto<U256>) -> Self::Value {
        let value = value.try_into().ok().expect("invalid U256 value");
        if let Ok(low) = value.try_into() {
            return self.ty_i256.const_int(low, false);
        }

        self.scratch.clear();
        write!(self.scratch, "{value:x}").unwrap();
        self.ty_i256.const_int_from_hex(&self.scratch)
    }

    fn str_const(&mut self, value: &str) -> Self::Value {
        self.bcx.build_global_string_ptr(value, "")
    }

    fn const_array(&mut self, ty: Self::Type, values: &[u64]) -> Self::Value {
        let values = values.iter().map(|&v| ty.const_int(v, false)).collect::<Vec<_>>();
        let init = ty.const_array(&values);
        let global = self.module().add_global(init.get_type(), "");
        global.set_initializer(init);
        global.set_constant(true);
        global.set_linkage(convert_linkage(evm2_jit_backend::Linkage::Private));
        global.set_unnamed_address(LLVMUnnamedAddr::LLVMGlobalUnnamedAddr);
        global.as_pointer_value()
    }

    fn nullptr(&mut self) -> Self::Value {
        self.ty_ptr.const_null()
    }

    fn new_stack_slot_raw(&mut self, ty: Self::Type, name: &str) -> Self::StackSlot {
        // let ty = self.ty_i8.array_type(size);
        // let ptr = self.bcx.build_alloca(ty, self.name(name));
        // ptr.as_instruction().unwrap().set_alignment(align);
        // ptr
        self.bcx.build_alloca(ty, self.name(name))
    }

    fn stack_load(&mut self, ty: Self::Type, slot: Self::StackSlot, name: &str) -> Self::Value {
        self.load(ty, slot, name)
    }

    fn stack_store(&mut self, value: Self::Value, slot: Self::StackSlot) {
        self.store(value, slot);
    }

    fn stack_addr(&mut self, ty: Self::Type, slot: Self::StackSlot) -> Self::Value {
        let _ = ty;
        slot
    }

    fn load(&mut self, ty: Self::Type, ptr: Self::Value, name: &str) -> Self::Value {
        let value = self.bcx.build_load(ty, ptr, self.name(name));
        if ty == self.ty_i256 {
            value.set_alignment(8);
        }
        value
    }

    fn load_aligned(
        &mut self,
        ty: Self::Type,
        ptr: Self::Value,
        align: usize,
        name: &str,
    ) -> Self::Value {
        let value = self.bcx.build_load(ty, ptr, self.name(name));
        value.set_alignment(align as u32);
        value
    }

    fn store(&mut self, value: Self::Value, ptr: Self::Value) -> Self::Inst {
        let inst = self.bcx.build_store(ptr, value);
        if value.get_type() == self.ty_i256 {
            inst.set_alignment(8);
        }
        inst
    }

    fn store_aligned(&mut self, value: Self::Value, ptr: Self::Value, align: usize) {
        let inst = self.bcx.build_store(ptr, value);
        inst.set_alignment(align as u32);
    }

    fn remove_inst(&mut self, inst: Self::Inst) {
        inst.erase_from_basic_block();
    }

    fn nop(&mut self) {
        // LLVM doesn't have a NOP instruction.
    }

    fn ret(&mut self, values: &[Self::Value]) {
        match values {
            [] => self.bcx.build_return(None),
            [value] => self.bcx.build_return(Some(*value)),
            values => self.bcx.build_aggregate_return(values),
        };
    }

    fn assume(&mut self, cond: Self::Value) {
        let function = self
            .get_or_add_function("llvm.assume", |this| this.ty_void.fn_type(&[this.ty_i1], false));
        self.bcx.build_call(function, &[cond], "");
    }

    fn icmp(&mut self, cond: IntCC, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_compare(convert_intcc(cond), lhs, rhs, "")
    }

    fn icmp_imm(&mut self, cond: IntCC, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.icmp(cond, lhs, rhs)
    }

    fn is_null(&mut self, ptr: Self::Value) -> Self::Value {
        self.bcx.build_is_null(ptr, "")
    }

    fn is_not_null(&mut self, ptr: Self::Value) -> Self::Value {
        self.bcx.build_is_not_null(ptr, "")
    }

    fn br(&mut self, dest: Self::BasicBlock) {
        self.bcx.build_unconditional_branch(dest);
    }

    fn brif(
        &mut self,
        cond: Self::Value,
        then_block: Self::BasicBlock,
        else_block: Self::BasicBlock,
    ) {
        self.bcx.build_conditional_branch(cond, then_block, else_block);
    }

    fn brif_cold(
        &mut self,
        cond: Self::Value,
        then_block: Self::BasicBlock,
        else_block: Self::BasicBlock,
        then_is_cold: bool,
    ) {
        let inst = self.bcx.build_conditional_branch(cond, then_block, else_block);
        let weights = if then_is_cold { [1, DEFAULT_WEIGHT] } else { [DEFAULT_WEIGHT, 1] };
        self.set_branch_weights(inst, weights);
    }

    fn switch(
        &mut self,
        index: Self::Value,
        default: Self::BasicBlock,
        targets: &[(u64, Self::BasicBlock)],
        default_is_cold: bool,
    ) {
        let ty = index.get_type();
        let targets =
            targets.iter().map(|(v, b)| (ty.const_int(*v, false), *b)).collect::<Vec<_>>();
        let inst = self.bcx.build_switch(index, default, &targets);
        if default_is_cold {
            let weights = iter::once(1).chain(iter::repeat_n(DEFAULT_WEIGHT, targets.len()));
            self.set_branch_weights(inst, weights);
        }
    }

    fn br_indirect(&mut self, address: Self::Value, destinations: &[Self::BasicBlock]) {
        self.bcx.build_indirect_branch(address, destinations);
    }

    fn phi(&mut self, ty: Self::Type, incoming: &[(Self::Value, Self::BasicBlock)]) -> Self::Value {
        self.bcx.build_phi(ty, incoming, "")
    }

    fn select(
        &mut self,
        cond: Self::Value,
        then_value: Self::Value,
        else_value: Self::Value,
    ) -> Self::Value {
        self.bcx.build_select(cond, then_value, else_value, "")
    }

    fn iadd(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_add(lhs, rhs, "")
    }

    fn isub(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_sub(lhs, rhs, "")
    }

    fn imul(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_mul(lhs, rhs, "")
    }

    fn udiv(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_unsigned_div(lhs, rhs, "")
    }

    fn sdiv(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_signed_div(lhs, rhs, "")
    }

    fn urem(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_unsigned_rem(lhs, rhs, "")
    }

    fn srem(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_int_signed_rem(lhs, rhs, "")
    }

    fn iadd_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.iadd(lhs, rhs)
    }

    fn isub_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.isub(lhs, rhs)
    }

    fn imul_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.imul(lhs, rhs)
    }

    // - [Avoid using arithmetic intrinsics](https://llvm.org/docs/Frontend/PerformanceTips.html)
    // - [Don't use usub.with.overflow intrinsic](https://github.com/rust-lang/rust/pull/103299)
    // - [for unsigned add overflow the recommended pattern is x + y < x](https://github.com/rust-lang/rust/pull/124114#issuecomment-2066173305)
    fn uadd_overflow(&mut self, lhs: Self::Value, rhs: Self::Value) -> (Self::Value, Self::Value) {
        let result = self.iadd(lhs, rhs);
        let overflow = self.icmp(IntCC::UnsignedLessThan, result, rhs);
        (result, overflow)
    }

    fn usub_overflow(&mut self, lhs: Self::Value, rhs: Self::Value) -> (Self::Value, Self::Value) {
        let result = self.isub(lhs, rhs);
        let overflow = self.icmp(IntCC::UnsignedLessThan, lhs, rhs);
        (result, overflow)
    }

    fn uadd_sat(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        let f = self.get_sat_function("uadd", lhs.get_type());
        self.call(f, &[lhs, rhs]).unwrap()
    }

    fn umax(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        let ty = lhs.get_type();
        let name = format!("llvm.umax.{}", fmt_ty(ty));
        let max = self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty, ty]));
        self.call(max, &[lhs, rhs]).unwrap()
    }

    fn umin(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        let ty = lhs.get_type();
        let name = format!("llvm.umin.{}", fmt_ty(ty));
        let min = self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty, ty]));
        self.call(min, &[lhs, rhs]).unwrap()
    }

    fn bswap(&mut self, value: Self::Value) -> Self::Value {
        let ty = value.get_type();
        let name = format!("llvm.bswap.{}", fmt_ty(ty));
        let bswap = self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty]));
        self.call(bswap, &[value]).unwrap()
    }

    fn bitor(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_or(lhs, rhs, "")
    }

    fn bitand(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_and(lhs, rhs, "")
    }

    fn bitxor(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_xor(lhs, rhs, "")
    }

    fn bitnot(&mut self, value: Self::Value) -> Self::Value {
        self.bcx.build_not(value, "")
    }

    fn clz(&mut self, value: Self::Value) -> Self::Value {
        let ty = value.get_type();
        let i1_ty = self.type_int(1);
        let name = format!("llvm.ctlz.{}", fmt_ty(ty));
        let ctlz = self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty, i1_ty]));
        let is_poison_on_zero = self.bool_const(false);
        self.call(ctlz, &[value, is_poison_on_zero]).unwrap()
    }

    fn popcnt(&mut self, value: Self::Value) -> Self::Value {
        let ty = value.get_type();
        let name = format!("llvm.ctpop.{}", fmt_ty(ty));
        let ctpop = self.get_or_add_function(&name, |this| this.fn_type(Some(ty), &[ty]));
        self.call(ctpop, &[value]).unwrap()
    }

    fn bitor_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.bitor(lhs, rhs)
    }

    fn bitand_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.bitand(lhs, rhs)
    }

    fn bitxor_imm(&mut self, lhs: Self::Value, rhs: i64) -> Self::Value {
        let rhs = self.iconst(lhs.get_type(), rhs);
        self.bitxor(lhs, rhs)
    }

    fn ishl(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_left_shift(lhs, rhs, "")
    }

    fn ushr(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_right_shift(lhs, rhs, false, "")
    }

    fn sshr(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value {
        self.bcx.build_right_shift(lhs, rhs, true, "")
    }

    fn zext(&mut self, ty: Self::Type, value: Self::Value) -> Self::Value {
        self.bcx.build_int_z_extend(value, ty, "")
    }

    fn sext(&mut self, ty: Self::Type, value: Self::Value) -> Self::Value {
        self.bcx.build_int_s_extend(value, ty, "")
    }

    fn ireduce(&mut self, to: Self::Type, value: Self::Value) -> Self::Value {
        self.bcx.build_int_truncate(value, to, "")
    }

    fn inttoptr(&mut self, value: Self::Value, ty: Self::Type) -> Self::Value {
        self.bcx.build_int_to_ptr(value, ty, "")
    }

    fn gep(
        &mut self,
        elem_ty: Self::Type,
        ptr: Self::Value,
        indexes: &[Self::Value],
        name: &str,
    ) -> Self::Value {
        unsafe { self.bcx.build_in_bounds_gep(elem_ty, ptr, indexes, self.name(name)) }
    }

    fn tail_call(
        &mut self,
        function: Self::Function,
        args: &[Self::Value],
        tail_call: TailCallKind,
    ) -> Option<Self::Value> {
        let callsite = self.bcx.build_call(function, args, "");
        if let Some(call_conv) = function_call_conv(function) {
            callsite.set_call_conv(call_conv);
        }
        if tail_call != TailCallKind::None {
            callsite.set_tail_call_kind(convert_tail_call_kind(tail_call));
        }
        callsite.value()
    }

    fn is_compile_time_known(&mut self, value: Self::Value) -> Option<Self::Value> {
        let ty = value.get_type();
        let name = format!("llvm.is.constant.{}", fmt_ty(ty));
        let f = self.get_or_add_function(&name, |this| this.fn_type(Some(this.ty_i1), &[ty]));
        Some(self.call(f, &[value]).unwrap())
    }

    fn memcpy(&mut self, dst: Self::Value, src: Self::Value, len: Self::Value) {
        self.memcpy_inner(dst, src, len, false);
    }

    fn memcpy_inline(&mut self, dst: Self::Value, src: Self::Value, len: i64) {
        let len = self.iconst(self.ty_i64, len);
        self.memcpy_inner(dst, src, len, true);
    }

    fn unreachable(&mut self) {
        self.bcx.build_unreachable();
    }

    fn get_or_build_function(
        &mut self,
        name: &str,
        params: &[Self::Type],
        ret: Option<Self::Type>,
        linkage: evm2_jit_backend::Linkage,
        build: impl FnOnce(&mut Self),
    ) -> Self::Function {
        if let Some(function) = self.module().get_function(name) {
            return function;
        }

        let before = self.current_block();

        let func_ty = self.fn_type(ret, params);
        let function = self.module().add_function(name, func_ty, Some(convert_linkage(linkage)));
        cpp::set_dso_local(function);
        let prev_function = std::mem::replace(&mut self.function, function);

        let entry = self.cx().append_basic_block(function, self.name("entry"));
        self.bcx.position_at_end(entry);
        build(self);
        if let Some(before) = before {
            self.bcx.position_at_end(before);
        }

        self.function = prev_function;

        function
    }

    fn get_function(&mut self, name: &str) -> Option<Self::Function> {
        self.module().get_function(name)
    }

    fn get_printf_function(&mut self) -> Self::Function {
        let name = "printf";
        if let Some(function) = self.module().get_function(name) {
            return function;
        }

        let ty = self.cx().void_type().fn_type(&[self.ty_ptr], true);
        let function = self.module().add_function(name, ty, Some(LLVMLinkage::LLVMExternalLinkage));
        cpp::set_dso_local(function);
        function
    }

    fn add_function(
        &mut self,
        name: &str,
        params: &[Self::Type],
        ret: Option<Self::Type>,
        address: Option<usize>,
        linkage: evm2_jit_backend::Linkage,
        call_conv: CallConv,
    ) -> Self::Function {
        let func_ty = self.fn_type(ret, params);
        let function = self.module().add_function(name, func_ty, Some(convert_linkage(linkage)));
        set_function_call_conv(function, call_conv);
        cpp::set_dso_local(function);
        if let Some(address) = address
            && let Some(orc) = &mut self.orc
        {
            orc.pending_symbols.push((CString::new(name).unwrap(), address));
        }
        function
    }

    fn add_function_stub(
        &mut self,
        function: Self::Function,
        call_conv: CallConv,
    ) -> Self::Function {
        let name = function.get_name();
        let stub_name = format!("{name}.stub");
        if let Some(stub) = self.module().get_function(&stub_name) {
            return stub;
        }

        let func_ty = function.get_type();
        let stub =
            self.module().add_function(&stub_name, func_ty, Some(LLVMLinkage::LLVMInternalLinkage));
        set_function_call_conv(stub, call_conv);
        cpp::set_dso_local(stub);
        self.add_function_attribute(
            Some(stub),
            evm2_jit_backend::Attribute::NoInline,
            evm2_jit_backend::FunctionAttributeLocation::Function,
        );

        let before = self.bcx.get_insert_block();
        let debug_location = self.debug_scope.and_then(|_| self.bcx.get_current_debug_location());
        self.bcx.unset_current_debug_location();

        let entry = self.cx().append_basic_block(stub, self.name("stub"));
        self.bcx.position_at_end(entry);
        let args =
            (0..stub.count_params()).map(|i| stub.get_nth_param(i).unwrap()).collect::<Vec<_>>();
        let value = self.call(function, &args);
        self.bcx.build_return(value);

        if let Some(before) = before {
            self.bcx.position_at_end(before);
        }
        if let Some(debug_location) = debug_location {
            self.bcx.set_current_debug_location(debug_location);
        }

        stub
    }

    fn add_function_attribute(
        &mut self,
        function: Option<Self::Function>,
        attribute: evm2_jit_backend::Attribute,
        loc: evm2_jit_backend::FunctionAttributeLocation,
    ) {
        let func = function.unwrap_or(self.function);
        let loc = convert_attribute_loc(loc);
        let attr = convert_attribute(self, attribute);
        func.add_attribute(loc, attr);
    }
}

/// Builds the LLVM pass pipeline string. See [`EvmLlvmBackend::optimize_module`].
fn build_pass_pipeline(with_licm: bool) -> String {
    // `globalopt`, `cgscc(inline)`, a second `sroa`, and `adce` change no benchmark's
    // assembly, so they are left out: the module has no global data, its only internal
    // function (the `mresize` stub) is `noinline`, the first `sroa` promotes the one
    // `alloca`, and `dse`/`simplifycfg` remove what `adce` would. Revisit them when
    // codegen emits inlinable helper functions or global data.
    let mut passes = String::from("module(function(");
    let function_passes: &[&str] = &[
        "simplifycfg",
        "sroa",
        "early-cse",
        "jump-threading",
        "correlated-propagation",
        "simplifycfg",
        "instcombine<no-verify-fixpoint>",
    ];
    let licm_passes: &[&str] =
        &["loop-mssa(licm,loop-rotate,licm)", "simplifycfg", "instcombine<no-verify-fixpoint>"];
    let post_passes: &[&str] =
        &["early-cse", "sccp", "instcombine<no-verify-fixpoint>", "dse", "simplifycfg"];

    let iter =
        function_passes.iter().chain(if with_licm { licm_passes } else { &[] }).chain(post_passes);
    for (i, pass) in iter.enumerate() {
        if i > 0 {
            passes.push(',');
        }
        passes.push_str(pass);
    }
    passes.push_str("),globaldce)");
    passes
}

fn init() -> Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    match INIT.get_or_init(|| init_().map_err(|e| format!("{e:#}"))) {
        Ok(()) => Ok(()),
        Err(e) => Err(eyre::eyre!("{e}")),
    }
}

fn init_() -> Result<()> {
    extern "C" fn report_fatal_error(msg: *const std::ffi::c_char) {
        let msg_cstr = unsafe { CStr::from_ptr(msg) };
        let msg = msg_cstr.to_string_lossy();
        error!(target: "llvm", "LLVM fatal error: {msg}");
    }

    unsafe {
        LLVMInstallFatalErrorHandler(Some(report_fatal_error));
    }

    // Collect extra LLVM args from `EVM2_JIT_LLVM_ARGS` env var (space-separated).
    let extra: Vec<CString> = std::env::var("EVM2_JIT_LLVM_ARGS")
        .ok()
        .iter()
        .flat_map(|s| s.split_whitespace().filter_map(|s| CString::new(s).ok()).collect::<Vec<_>>())
        .collect();
    if !extra.is_empty() {
        debug!(extra = ?&extra, "passing extra LLVM args");
    }

    // The first arg is only used in `-help` output AFAICT.
    let mut args = vec![
        c"evm2-jit-llvm".as_ptr(),
        c"-x86-asm-syntax=intel".as_ptr(),
        // CodeGenPrepare restarts from the first block whenever it changes the dominator tree;
        // in "huge" functions it revisits only the changed blocks. Our functions hit this often,
        // so the restarts are superlinear: CGP took 40% of codegen time in a 973-block function
        // (the default threshold is 10000 blocks). Treat every function as huge; codegen barely
        // changes.
        c"--cgpp-huge-func=0".as_ptr(),
        // Greedy register allocation's global live range splitting is superlinear on large
        // functions with many long-lived values. Treat smaller live ranges as huge so LLVM uses
        // the cheaper splitting path; this preserves code size on snailtracer while cutting llc
        // time by ~3x.
        c"--huge-size-for-split=64".as_ptr(),
        // SimplifyCFG folds each block's out-of-gas and stack-bound checks into a logical-or
        // `select` with branch weights. CodeGenPrepare would turn that back into a branch
        // diamond with an `i1` phi; kept as a `select`, SelectionDAG splits it into two plain
        // conditional branches instead. The flag applies to every `select`, but the IR has few
        // others.
        c"-disable-cgp-select2branch".as_ptr(),
        // A JUMPDEST that dominates the rest of the code and is reached again through the
        // dynamic jump table or an internal function's return switch heads a loop that spans
        // most of the function. MachineLICM hoists every constant materialization in it, often
        // hundreds of constant pool loads, into the preheader, and greedy register allocation
        // then spends most of codegen splitting those function-wide live ranges. Sink them back
        // into their users when the preheader's register pressure exceeds the limit: this cut
        // codegen by up to 15x on large mainnet contracts.
        c"-sink-insts-to-avoid-spills".as_ptr(),
        // PHI elimination splits critical edges to put each phi copy on its own edge. The dynamic
        // jump table and internal-function return switches have many critical edges into blocks
        // with phis, and the new blocks and jumps give greedy register allocation more places to
        // split live ranges and reload. Keeping the copies in the predecessors cut reloads by 12%
        // on the mainnet corpus.
        c"-disable-phi-elim-edge-splitting".as_ptr(),
        // These change little or no code on the benchmarks and the mainnet corpus but still walk
        // every machine instruction.
        c"-disable-x86-domain-reassignment".as_ptr(),
        c"-disable-postra-machine-licm".as_ptr(),
        // Capture tracking gives up after 100 uses and then treats the pointer as captured.
        // Every stack slot access is a use of the stack argument, so in all but the smallest
        // functions alias analysis lets EVM memory and builtin calls alias the stack, and LICM
        // cannot promote loop-carried stack slots to registers.
        c"-capture-tracking-max-uses-to-explore=1000000".as_ptr(),
        // Emit a trap for `unreachable`, as rustc does, so that a builtin call that is expected
        // to leave through `evm2_jit_exit` but returns faults instead of running into the next
        // block.
        c"-trap-unreachable".as_ptr(),
    ];
    args.extend(extra.iter().map(|s| s.as_ptr()));
    unsafe { LLVMParseCommandLineOptions(args.len() as i32, args.as_ptr(), std::ptr::null()) }
    // TODO: https://github.com/llvm/llvm-project/issues/235937
    unsafe { cpp::evm2_jit_llvm_expand_ir_insts_after_lsr() };

    Target::initialize_native().map_err(|e| eyre::eyre!(e))?;

    Ok(())
}

fn create_module(cx: &Context, machine: &TargetMachine, aot: bool) -> Result<Module> {
    let module_name = "evm";
    let module = cx.create_module(module_name);
    module.set_source_file_name(module_name);
    module.set_data_layout(&machine.get_target_data().get_data_layout());
    module.set_triple(&machine.get_triple());
    if aot {
        module.add_basic_value_flag(
            "PIC Level",
            LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorError, // TODO: Min
            cx.i32_type().const_int(2, false),
        );
        module.add_basic_value_flag(
            "RtLibUseGOT",
            LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorWarning,
            cx.i32_type().const_int(1, false),
        );
    }
    Ok(module)
}

const fn convert_intcc(cond: IntCC) -> LLVMIntPredicate {
    match cond {
        IntCC::Equal => LLVMIntPredicate::LLVMIntEQ,
        IntCC::NotEqual => LLVMIntPredicate::LLVMIntNE,
        IntCC::SignedLessThan => LLVMIntPredicate::LLVMIntSLT,
        IntCC::SignedGreaterThanOrEqual => LLVMIntPredicate::LLVMIntSGE,
        IntCC::SignedGreaterThan => LLVMIntPredicate::LLVMIntSGT,
        IntCC::SignedLessThanOrEqual => LLVMIntPredicate::LLVMIntSLE,
        IntCC::UnsignedLessThan => LLVMIntPredicate::LLVMIntULT,
        IntCC::UnsignedGreaterThanOrEqual => LLVMIntPredicate::LLVMIntUGE,
        IntCC::UnsignedGreaterThan => LLVMIntPredicate::LLVMIntUGT,
        IntCC::UnsignedLessThanOrEqual => LLVMIntPredicate::LLVMIntULE,
    }
}

/// Maps to the codegen level of the target machine, used for AOT and dumps. `Less` skips the
/// post-RA scheduler on CPUs that enable it (znver), saving about 5% of compile instructions for
/// no runtime gain. The JIT compiler in `lib.cpp` always uses `Less`, whatever the level.
const fn convert_opt_level(level: OptimizationLevel) -> LLVMCodeGenOptLevel {
    match level {
        OptimizationLevel::None => LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
        OptimizationLevel::Less | OptimizationLevel::Default | OptimizationLevel::Aggressive => {
            LLVMCodeGenOptLevel::LLVMCodeGenLevelLess
        }
    }
}

fn convert_attribute(bcx: &EvmLlvmBuilder<'_>, attr: evm2_jit_backend::Attribute) -> Attribute {
    use evm2_jit_backend::Attribute as OurAttr;

    enum AttrValue<'a> {
        String(&'a str),
        Enum(u64),
        Type(Type),
    }

    let cpu;
    let (key, value) = match attr {
        OurAttr::WillReturn => ("willreturn", AttrValue::Enum(0)),
        OurAttr::NoReturn => ("noreturn", AttrValue::Enum(0)),
        OurAttr::NoFree => ("nofree", AttrValue::Enum(0)),
        OurAttr::NoSync => ("nosync", AttrValue::Enum(0)),
        OurAttr::NoUnwind => ("nounwind", AttrValue::Enum(0)),
        OurAttr::NonLazyBind => ("nonlazybind", AttrValue::Enum(0)),
        OurAttr::UWTable => return cpp::create_async_uwtable_attr(bcx.cx()),
        OurAttr::AllFramePointers => ("frame-pointer", AttrValue::String("all")),
        OurAttr::NativeTargetCpu => (
            "target-cpu",
            AttrValue::String({
                cpu = bcx.machine.get_cpu();
                cpu.to_str().unwrap()
            }),
        ),
        OurAttr::Cold => ("cold", AttrValue::Enum(0)),
        OurAttr::Hot => ("hot", AttrValue::Enum(0)),
        OurAttr::HintInline => ("inlinehint", AttrValue::Enum(0)),
        OurAttr::AlwaysInline => ("alwaysinline", AttrValue::Enum(0)),
        OurAttr::NoInline => ("noinline", AttrValue::Enum(0)),
        OurAttr::Speculatable => ("speculatable", AttrValue::Enum(0)),
        OurAttr::InlineStackProbes => ("probe-stack", AttrValue::String("inline-asm")),

        OurAttr::NoAlias => ("noalias", AttrValue::Enum(0)),
        OurAttr::NoCapture => return cpp::create_no_captures_attr(bcx.cx()),
        OurAttr::NoUndef => ("noundef", AttrValue::Enum(0)),
        OurAttr::Align(n) => ("align", AttrValue::Enum(n)),
        OurAttr::NonNull => ("nonnull", AttrValue::Enum(0)),
        OurAttr::Dereferenceable(n) => ("dereferenceable", AttrValue::Enum(n)),
        OurAttr::SRet(n) => ("sret", AttrValue::Type(bcx.type_array(bcx.ty_i8, n as _))),
        OurAttr::ReadNone => ("readnone", AttrValue::Enum(0)),
        OurAttr::ReadOnly => ("readonly", AttrValue::Enum(0)),
        OurAttr::WriteOnly => ("writeonly", AttrValue::Enum(0)),
        OurAttr::Writable => ("writable", AttrValue::Enum(0)),
        OurAttr::ArgMemOnly => return cpp::create_arg_mem_only_attr(bcx.cx()),
        // `u64::MAX` marks all reachable memory as dead; `0` would mark zero bytes.
        OurAttr::DeadOnReturn => ("dead_on_return", AttrValue::Enum(u64::MAX)),
        OurAttr::DeadOnUnwind => ("dead_on_unwind", AttrValue::Enum(0)),

        OurAttr::Initializes(size) => {
            return cpp::create_initializes_attr(bcx.cx(), 0, size as i64);
        }

        attr => unimplemented!("llvm attribute conversion: {attr:?}"),
    };
    match value {
        AttrValue::String(value) => bcx.cx().create_string_attribute(key, value),
        AttrValue::Enum(value) => {
            let id = Attribute::get_named_enum_kind_id(key);
            bcx.cx().create_enum_attribute(id, value)
        }
        AttrValue::Type(ty) => {
            let id = Attribute::get_named_enum_kind_id(key);
            bcx.cx().create_type_attribute(id, ty)
        }
    }
}

const fn convert_attribute_loc(loc: evm2_jit_backend::FunctionAttributeLocation) -> AttributeLoc {
    match loc {
        evm2_jit_backend::FunctionAttributeLocation::Return => AttributeLoc::Return,
        evm2_jit_backend::FunctionAttributeLocation::Param(i) => AttributeLoc::Param(i),
        evm2_jit_backend::FunctionAttributeLocation::Function => AttributeLoc::Function,
    }
}

fn function_call_conv(function: Function) -> Option<u32> {
    let call_conv = function.get_call_conv();
    (call_conv != LLVMCallConv::LLVMCCallConv as u32).then_some(call_conv)
}

fn set_function_call_conv(function: Function, call_conv: CallConv) {
    let Some(call_conv) = convert_call_conv(call_conv) else { return };
    function.set_call_conv(call_conv as _);
}

const fn convert_call_conv(call_conv: CallConv) -> Option<LLVMCallConv> {
    match call_conv {
        CallConv::Default => None,
        CallConv::Cold => Some(LLVMCallConv::LLVMPreserveMostCallConv),
    }
}

const fn convert_linkage(linkage: evm2_jit_backend::Linkage) -> LLVMLinkage {
    match linkage {
        evm2_jit_backend::Linkage::Public => LLVMLinkage::LLVMExternalLinkage,
        evm2_jit_backend::Linkage::Import => LLVMLinkage::LLVMExternalLinkage,
        evm2_jit_backend::Linkage::Private => LLVMLinkage::LLVMPrivateLinkage,
    }
}

const fn convert_tail_call_kind(kind: TailCallKind) -> LLVMTailCallKind {
    match kind {
        TailCallKind::None => LLVMTailCallKind::LLVMTailCallKindNone,
        TailCallKind::Tail => LLVMTailCallKind::LLVMTailCallKindTail,
        TailCallKind::MustTail => LLVMTailCallKind::LLVMTailCallKindMustTail,
        TailCallKind::NoTail => LLVMTailCallKind::LLVMTailCallKindNoTail,
    }
}

// No `#[track_caller]` because `map_err` doesn't propagate it.
fn error_msg(msg: LLVMString) -> evm2_jit_backend::Error {
    evm2_jit_backend::Error::msg(msg.to_string_lossy().trim_end().to_string())
}

fn fmt_ty(ty: Type) -> impl std::fmt::Display {
    ty.print_to_string().to_str().unwrap().trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_sys::{LLVMInlineAsmDialect, core::*};

    /// An error diagnostic LLVM raises while emitting code must fail `write_object`, and must not
    /// leak into the next operation.
    #[test]
    fn diagnostic_error_fails_write_object() {
        const ARGS: usize = 64;

        let mut backend = EvmLlvmBackend::new(true).unwrap();
        unsafe {
            let cx = backend.cx().raw();
            let void = LLVMVoidTypeInContext(cx);
            let i32 = LLVMInt32TypeInContext(cx);
            let function_ty = LLVMFunctionType(void, std::ptr::null_mut(), 0, 0);
            let function = LLVMAddFunction(backend.module.as_mut_ptr(), c"f".as_ptr(), function_ty);
            let entry = LLVMAppendBasicBlockInContext(cx, function, c"entry".as_ptr());
            let builder = LLVMCreateBuilderInContext(cx);
            LLVMPositionBuilderAtEnd(builder, entry);

            // More register inputs than the target has registers.
            let mut param_tys = [i32; ARGS];
            let asm_ty = LLVMFunctionType(void, param_tys.as_mut_ptr(), ARGS as u32, 0);
            let constraints = vec!["r"; ARGS].join(",");
            let asm = LLVMGetInlineAsm(
                asm_ty,
                c"".as_ptr().cast_mut(),
                0,
                constraints.as_ptr().cast::<std::ffi::c_char>().cast_mut(),
                constraints.len(),
                1,
                0,
                LLVMInlineAsmDialect::LLVMInlineAsmDialectATT,
                0,
            );
            let mut args = [LLVMConstInt(i32, 1, 0); ARGS];
            LLVMBuildCall2(builder, asm_ty, asm, args.as_mut_ptr(), ARGS as u32, c"".as_ptr());
            LLVMBuildRetVoid(builder);
            LLVMDisposeBuilder(builder);
        }

        let err = backend.write_object(Vec::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "LLVM error: inline assembly requires more registers than available"
        );

        backend.dh.check(Ok(())).unwrap();
    }
}
