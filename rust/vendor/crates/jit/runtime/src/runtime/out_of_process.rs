//! Out-of-process JIT helper process and IPC.

use crate::{
    CompileTimings, OptimizationLevel, eyre,
    runtime::{
        config::{CompilationKind, HostConfig, HostOpcodes, RuntimeConfig},
        stats::RuntimeStats,
        storage::RuntimeCacheKey,
        worker::{
            CompileJob, CompilerState, CompilerTarget, JitObjectSuccess, SyncNotifier,
            WorkerResult, WorkerSuccess, compile_with_state,
        },
    },
};
use alloy_primitives::{B256, Bytes};
use crossbeam_channel as chan;
use evm2::{
    BaseEvmTypes, OpcodeView, SpecId, Version,
    version::{GasId, GasParams},
};
use evm2_jit_codegen::HostLayout;
use eyre::OptionExt;
use rayon::ThreadPoolBuilder;
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{BufReader, BufWriter, Read, Write},
    ops::ControlFlow,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;
use wincode::{SchemaRead, SchemaWrite};

const HELPER_ENV: &str = "EVM2_JIT_HELPER";
const GAS_PARAM_COUNT: usize = GasId::COUNT;
const HELPER_PAUSE_TIMEOUT: Duration = Duration::from_millis(100);
const RECV_POLL_INTERVAL: Duration = Duration::from_millis(50);

type GasParamPairs = Vec<(u8, u32)>;
type PendingResponses = Arc<Mutex<HashMap<u64, chan::Sender<Result<HelperJobResult, String>>>>>;
type PendingPauses = Arc<Mutex<HashMap<u64, chan::Sender<Result<(), String>>>>>;

/// Runs the out-of-process JIT helper if this process was launched as one.
pub(super) fn maybe_run_jit_helper() -> eyre::Result<ControlFlow<()>> {
    if std::env::var_os(HELPER_ENV).is_none() {
        return Ok(ControlFlow::Continue(()));
    }
    run_jit_helper_stdio()?;
    Ok(ControlFlow::Break(()))
}

/// Compiles a job in the out-of-process helper.
pub(super) fn compile_job(
    job: CompileJob,
    config: &RuntimeConfig,
    helper: &HelperProcess,
) -> WorkerResult {
    let t0 = Instant::now();
    let (outcome, timings, transient) = match run_helper_job(&job, config, helper) {
        Ok(result) => (result.outcome, result.timings, false),
        Err(failure) => (Err(failure.error), CompileTimings::default(), failure.transient),
    };
    WorkerResult {
        key: job.key,
        outcome,
        kind: job.kind,
        sync_notifier: job.sync_notifier,
        generation: job.generation,
        compile_duration: t0.elapsed(),
        timings,
        transient,
    }
}

fn run_helper_job(
    job: &CompileJob,
    config: &RuntimeConfig,
    helper: &HelperProcess,
) -> Result<HelperJobResult, HelperFailure> {
    helper.compile(job, config)
}

/// A job that did not produce a compile outcome.
struct HelperFailure {
    error: String,
    /// The helper went away under the job: the job may not be at fault.
    transient: bool,
}

impl HelperFailure {
    const fn deterministic(error: String) -> Self {
        Self { error, transient: false }
    }

    const fn transient(error: String) -> Self {
        Self { error, transient: true }
    }
}

struct HelperJobResult {
    outcome: Result<WorkerSuccess, String>,
    timings: CompileTimings,
}

struct HelperIo {
    stdin: BufWriter<ChildStdin>,
}

pub(super) struct HelperProcess {
    inner: Mutex<Option<Arc<HelperProcessInner>>>,
    paused: Arc<AtomicBool>,
    stats: Arc<RuntimeStats>,
}

impl HelperProcess {
    pub(super) fn new(stats: Arc<RuntimeStats>) -> Self {
        Self { inner: Mutex::new(None), paused: Arc::new(AtomicBool::new(false)), stats }
    }

    fn compile(
        &self,
        job: &CompileJob,
        config: &RuntimeConfig,
    ) -> Result<HelperJobResult, HelperFailure> {
        let helper = {
            let mut slot = self.inner.lock().unwrap();
            if slot.as_ref().is_none_or(|helper| !helper.matches_config(config)) {
                let restarting = slot.is_some();
                debug!("spawning JIT helper");
                match HelperProcessInner::spawn(config, self.stats.clone(), self.paused.clone()) {
                    Ok(helper) => {
                        if self.paused.load(Ordering::Relaxed) {
                            helper.pause();
                        }
                        self.stats.jit_helper_spawns.fetch_add(1, Ordering::Relaxed);
                        if restarting {
                            self.stats.jit_helper_restarts.fetch_add(1, Ordering::Relaxed);
                        }
                        *slot = Some(Arc::new(helper));
                    }
                    Err(err) => {
                        self.stats.jit_helper_spawn_failures.fetch_add(1, Ordering::Relaxed);
                        return Err(HelperFailure::deterministic(err));
                    }
                }
            }
            slot.as_ref().unwrap().clone()
        };

        match helper.compile(job, config) {
            Ok(result) => Ok(result),
            Err(err) => {
                let mut slot = self.inner.lock().unwrap();
                if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &helper)) {
                    warn!(error = %err.error, "discarding JIT helper after failed job");
                    self.stats.jit_helper_restarts.fetch_add(1, Ordering::Relaxed);
                    *slot = None;
                }
                Err(err)
            }
        }
    }

    pub(super) fn cancel_in_flight(&self) {
        if let Some(helper) = self.inner.lock().unwrap().take() {
            helper.kill();
        }
    }

    pub(super) fn pause(&self) {
        self.stats.jit_helper_pause_requests.fetch_add(1, Ordering::Relaxed);
        self.paused.store(true, Ordering::Relaxed);
        if let Some(helper) = self.inner.lock().unwrap().as_ref() {
            helper.pause();
        }
    }

    pub(super) fn resume(&self) {
        self.stats.jit_helper_resume_requests.fetch_add(1, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        if let Some(helper) = self.inner.lock().unwrap().as_ref() {
            helper.resume();
        }
    }
}

struct HelperProcessInner {
    configured_path: Option<PathBuf>,
    init: HelperInit,
    child: Mutex<Child>,
    io: Mutex<HelperIo>,
    pending: PendingResponses,
    pending_pauses: PendingPauses,
    reader: Mutex<Option<JoinHandle<()>>>,
    next_job_id: AtomicU64,
    shutdown_timeout: Duration,
    stats: Arc<RuntimeStats>,
    paused: Arc<AtomicBool>,
}

impl HelperProcessInner {
    fn spawn(
        config: &RuntimeConfig,
        stats: Arc<RuntimeStats>,
        paused: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let path = match &config.jit_helper_path {
            Some(path) => path.clone(),
            None => default_helper_path()?,
        };
        let init = helper_init(config);
        let mut command = Command::new(&path);
        command
            .env(HELPER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_helper_limits(&mut command, config);

        let mut child = command.spawn().map_err(|e| format!("failed to spawn JIT helper: {e}"))?;
        let mut stdin = BufWriter::new(child.stdin.take().ok_or("helper stdin unavailable")?);
        write_init(&mut stdin, &init).map_err(|e| format!("failed to write helper init: {e}"))?;
        stdin.flush().map_err(|e| format!("failed to flush helper init: {e}"))?;
        let stdout = child.stdout.take().ok_or("helper stdout unavailable")?;
        let pending = PendingResponses::default();
        let pending_pauses = PendingPauses::default();
        let reader_pending = pending.clone();
        let reader_pending_pauses = pending_pauses.clone();
        let reader_stats = stats.clone();
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let result = read_helper_response(&mut stdout);
                match result {
                    Ok(HelperResponseMessage::Job(id, result)) => {
                        if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(Ok(result));
                        }
                    }
                    Ok(HelperResponseMessage::Paused(id)) => {
                        if let Some(tx) = reader_pending_pauses.lock().unwrap().remove(&id) {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    Err(error) => {
                        reader_stats.jit_helper_disconnects.fetch_add(1, Ordering::Relaxed);
                        for (_, tx) in reader_pending.lock().unwrap().drain() {
                            let _ = tx.send(Err(error.clone()));
                        }
                        for (_, tx) in reader_pending_pauses.lock().unwrap().drain() {
                            let _ = tx.send(Err(error.clone()));
                        }
                        break;
                    }
                }
            }
        });
        Ok(Self {
            configured_path: config.jit_helper_path.clone(),
            init,
            child: Mutex::new(child),
            io: Mutex::new(HelperIo { stdin }),
            pending,
            pending_pauses,
            reader: Mutex::new(Some(reader)),
            next_job_id: AtomicU64::new(0),
            shutdown_timeout: config.tuning.shutdown_timeout,
            stats,
            paused,
        })
    }

    fn matches_config(&self, config: &RuntimeConfig) -> bool {
        self.init == helper_init(config) && self.configured_path == config.jit_helper_path
    }

    fn compile(
        &self,
        job: &CompileJob,
        config: &RuntimeConfig,
    ) -> Result<HelperJobResult, HelperFailure> {
        let id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = chan::bounded(1);
        self.pending.lock().unwrap().insert(id, tx);

        {
            let mut io = self.io.lock().unwrap();
            if let Err(e) = write_job(&mut io.stdin, id, job) {
                self.pending.lock().unwrap().remove(&id);
                return Err(HelperFailure::transient(format!("failed to write helper job: {e}")));
            }
            if let Err(e) = io.stdin.flush() {
                self.pending.lock().unwrap().remove(&id);
                return Err(HelperFailure::transient(format!("failed to flush helper job: {e}")));
            }
        }

        match recv_unpaused(&rx, config.tuning.jit_timeout, &self.paused) {
            // An error here comes from the reader draining jobs after the helper died.
            Ok(result) => result.map_err(HelperFailure::transient),
            Err(chan::RecvTimeoutError::Timeout) => {
                warn!(timeout = ?config.tuning.jit_timeout, "JIT helper timed out");
                self.pending.lock().unwrap().remove(&id);
                self.stats.jit_helper_timeouts.fetch_add(1, Ordering::Relaxed);
                self.kill();
                Err(HelperFailure::deterministic(format!(
                    "JIT helper timed out after {:?}; helper will be restarted",
                    config.tuning.jit_timeout
                )))
            }
            Err(chan::RecvTimeoutError::Disconnected) => {
                let status = self.child.lock().unwrap().try_wait().ok().flatten();
                let message = match status {
                    Some(status) => format!("JIT helper exited with {status}"),
                    None => "JIT helper disconnected".into(),
                };
                warn!(message, "JIT helper disconnected");
                self.stats.jit_helper_disconnects.fetch_add(1, Ordering::Relaxed);
                Err(HelperFailure::transient(format!("{message}; helper will be restarted")))
            }
        }
    }

    fn kill(&self) -> bool {
        let mut child = self.child.lock().unwrap();
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        kill_helper(&mut child);
        match child.wait_timeout(self.shutdown_timeout) {
            Ok(Some(_)) => true,
            Ok(None) => {
                warn!(timeout = ?self.shutdown_timeout, "timed out waiting for JIT helper exit");
                false
            }
            Err(err) => {
                warn!(%err, "failed to wait for JIT helper exit");
                false
            }
        }
    }

    fn pause(&self) {
        let id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = chan::bounded(1);
        self.pending_pauses.lock().unwrap().insert(id, tx);

        let send_result = {
            let mut io = self.io.lock().unwrap();
            write_pause(&mut io.stdin, id).and_then(|()| io.stdin.flush())
        };

        if let Err(err) = send_result {
            self.pending_pauses.lock().unwrap().remove(&id);
            self.stats.jit_helper_pause_failures.fetch_add(1, Ordering::Relaxed);
            warn!(%err, "failed to request graceful JIT helper pause");
        } else {
            match rx.recv_timeout(HELPER_PAUSE_TIMEOUT) {
                Ok(Ok(())) => {
                    self.stats.jit_helper_pause_acknowledgements.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Err(err)) => {
                    self.stats.jit_helper_pause_failures.fetch_add(1, Ordering::Relaxed);
                    warn!(%err, "JIT helper graceful pause failed");
                }
                Err(chan::RecvTimeoutError::Timeout) => {
                    self.pending_pauses.lock().unwrap().remove(&id);
                    self.stats.jit_helper_pause_timeouts.fetch_add(1, Ordering::Relaxed);
                    warn!(timeout = ?HELPER_PAUSE_TIMEOUT, "timed out waiting for JIT helper pause");
                }
                Err(chan::RecvTimeoutError::Disconnected) => {
                    self.stats.jit_helper_pause_failures.fetch_add(1, Ordering::Relaxed);
                    warn!("JIT helper disconnected before pause acknowledgement");
                }
            }
        }

        self.signal(libc::SIGSTOP, "pause");
    }

    fn resume(&self) {
        self.signal(libc::SIGCONT, "resume");
        let send_result = {
            let mut io = self.io.lock().unwrap();
            write_resume(&mut io.stdin).and_then(|()| io.stdin.flush())
        };
        if let Err(err) = send_result {
            self.stats.jit_helper_resume_failures.fetch_add(1, Ordering::Relaxed);
            warn!(%err, "failed to request JIT helper resume");
        }
    }

    fn signal(&self, signal: libc::c_int, action: &str) {
        let mut child = self.child.lock().unwrap();
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        signal_helper(&child, signal, action);
    }
}

impl Drop for HelperProcessInner {
    fn drop(&mut self) {
        let exited = self.kill();
        if exited && let Some(reader) = self.reader.lock().unwrap().take() {
            let _ = reader.join();
        }
    }
}

fn apply_helper_limits(command: &mut Command, config: &RuntimeConfig) {
    let memory_limit = config.tuning.jit_helper_memory_limit_bytes;
    let cpu_count = config.tuning.jit_helper_cpu_count;

    // SAFETY: `pre_exec` runs in the child after fork and before exec. The closure only calls
    // libc process/resource/affinity syscalls and constructs an `io::Error` if they fail.
    unsafe {
        command.pre_exec(move || {
            set_process_group()?;
            if memory_limit > 0 {
                set_rlimit(libc::RLIMIT_AS as _, memory_limit)?;
            }
            if cpu_count > 0 {
                limit_cpu_affinity(cpu_count)?;
            }
            Ok(())
        });
    }
}

fn set_process_group() -> std::io::Result<()> {
    if unsafe { libc::setpgid(0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn kill_helper(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
        return;
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::ESRCH) {
        warn!(%err, "failed to kill JIT helper process group");
    }
    if let Err(err) = child.kill() {
        warn!(%err, "failed to kill JIT helper");
    }
}

fn signal_helper(child: &Child, signal: libc::c_int, action: &str) {
    let pid = child.id() as libc::pid_t;
    if unsafe { libc::kill(-pid, signal) } == 0 {
        return;
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::ESRCH) {
        warn!(%err, signal, action, "failed to signal JIT helper process group");
    }
}

fn set_rlimit(resource: libc::c_int, value: u64) -> std::io::Result<()> {
    let value = libc::rlim_t::try_from(value).unwrap_or(libc::rlim_t::MAX);
    let limit = libc::rlimit { rlim_cur: value, rlim_max: value };
    if unsafe { libc::setrlimit(resource as _, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn limit_cpu_affinity(cpu_count: usize) -> std::io::Result<()> {
    let mut current = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    let size = std::mem::size_of::<libc::cpu_set_t>();
    if unsafe { libc::sched_getaffinity(0, size, &mut current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut limited = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    let mut remaining = cpu_count;
    for cpu in 0..(8 * size) {
        if unsafe { libc::CPU_ISSET(cpu, &current) } {
            unsafe { libc::CPU_SET(cpu, &mut limited) };
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
    }
    if remaining == cpu_count {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "current CPU affinity mask is empty",
        ));
    }

    if unsafe { libc::sched_setaffinity(0, size, &limited) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
const fn limit_cpu_affinity(_cpu_count: usize) -> std::io::Result<()> {
    Ok(())
}

#[derive(Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
struct HelperInit {
    debug_assertions: bool,
    single_error: bool,
    no_dedup: bool,
    no_dse: bool,
    dump_dir: Option<String>,
    gas_params: Option<GasParamPairs>,
    /// Words of the host layout, see [`HostLayout::to_words`].
    layout: Vec<u64>,
    opcode_view: Option<Box<OpcodeView>>,
    /// Makes the helper's builtin symbols unique to the host's resolver.
    symbol_suffix: Option<u64>,
    jit_worker_count: usize,
    compiler_recycle_threshold: usize,
}

#[derive(SchemaWrite, SchemaRead)]
enum HelperRequest {
    Init(HelperInit),
    Compile(HelperCompile),
    Pause { id: u64 },
    Resume,
}

#[derive(SchemaWrite, SchemaRead)]
struct HelperCompile {
    id: u64,
    code_hash: [u8; 32],
    spec_id: u8,
    opt_level: u8,
    symbol_name: String,
    bytecode: Vec<u8>,
}

#[derive(SchemaWrite, SchemaRead)]
enum HelperResponse {
    Ok {
        id: u64,
        symbol_name: String,
        object_bytes: Vec<u8>,
        builtin_symbols: Vec<String>,
        timings: HelperTimings,
    },
    Err {
        id: u64,
        error: String,
        timings: HelperTimings,
    },
    Paused {
        id: u64,
    },
}

#[derive(Clone, Copy, Default, SchemaWrite, SchemaRead)]
struct HelperTimings {
    parse: u64,
    translate: u64,
    optimize: u64,
    codegen: u64,
}

fn write_init<W: Write + ?Sized>(w: &mut BufWriter<W>, init: &HelperInit) -> std::io::Result<()> {
    write_message(w, &HelperRequest::Init(init.clone()))
}

fn write_job<W: Write + ?Sized>(
    w: &mut BufWriter<W>,
    id: u64,
    job: &CompileJob,
) -> std::io::Result<()> {
    let req = HelperRequest::Compile(HelperCompile {
        id,
        code_hash: job.key.code_hash.0,
        spec_id: u8::try_from(u32::from(job.key.spec_id)).expect("evm2 SpecId does not fit in u8"),
        opt_level: opt_level_to_u8(job.opt_level),
        symbol_name: job.symbol_name.clone(),
        bytecode: job.bytecode.to_vec(),
    });
    write_message(w, &req)
}

fn write_pause<W: Write + ?Sized>(w: &mut BufWriter<W>, id: u64) -> std::io::Result<()> {
    write_message(w, &HelperRequest::Pause { id })
}

fn write_resume<W: Write + ?Sized>(w: &mut BufWriter<W>) -> std::io::Result<()> {
    write_message(w, &HelperRequest::Resume)
}

enum HelperResponseMessage {
    Job(u64, HelperJobResult),
    Paused(u64),
}

fn read_helper_response<R: Read + ?Sized>(
    r: &mut BufReader<R>,
) -> Result<HelperResponseMessage, String> {
    match read_message(r).map_err(|e| format!("failed to decode helper result: {e}"))? {
        HelperResponse::Ok { id, symbol_name, object_bytes, builtin_symbols, timings } => {
            Ok(HelperResponseMessage::Job(
                id,
                HelperJobResult {
                    outcome: Ok(WorkerSuccess::JitObject(JitObjectSuccess {
                        symbol_name,
                        object_bytes: Bytes::from(object_bytes),
                        builtin_symbols,
                    })),
                    timings: timings.into(),
                },
            ))
        }
        HelperResponse::Err { id, error, timings } => Ok(HelperResponseMessage::Job(
            id,
            HelperJobResult { outcome: Err(error), timings: timings.into() },
        )),
        HelperResponse::Paused { id } => Ok(HelperResponseMessage::Paused(id)),
    }
}

impl From<CompileTimings> for HelperTimings {
    fn from(timings: CompileTimings) -> Self {
        Self {
            parse: duration_to_nanos(timings.parse),
            translate: duration_to_nanos(timings.translate),
            optimize: duration_to_nanos(timings.optimize),
            codegen: duration_to_nanos(timings.codegen),
        }
    }
}

impl From<HelperTimings> for CompileTimings {
    fn from(timings: HelperTimings) -> Self {
        Self {
            parse: Duration::from_nanos(timings.parse),
            translate: Duration::from_nanos(timings.translate),
            optimize: Duration::from_nanos(timings.optimize),
            codegen: Duration::from_nanos(timings.codegen),
        }
    }
}

fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Receives from `rx`, failing with [`chan::RecvTimeoutError::Timeout`] once `timeout` of time
/// spent unpaused has passed. A stopped helper makes no progress, so time under pause does not
/// count.
fn recv_unpaused<T>(
    rx: &chan::Receiver<T>,
    timeout: Duration,
    paused: &AtomicBool,
) -> Result<T, chan::RecvTimeoutError> {
    let mut remaining = timeout;
    loop {
        let paused_before = paused.load(Ordering::Relaxed);
        let start = Instant::now();
        match rx.recv_timeout(remaining.min(RECV_POLL_INTERVAL)) {
            Err(chan::RecvTimeoutError::Timeout) => {
                if !paused_before && !paused.load(Ordering::Relaxed) {
                    remaining = remaining.saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        return Err(chan::RecvTimeoutError::Timeout);
                    }
                }
            }
            result => return result,
        }
    }
}

thread_local! {
    static HELPER_COMPILER: RefCell<Option<CompilerState>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct HelperPauseState {
    paused: Mutex<bool>,
    resumed: Condvar,
}

impl HelperPauseState {
    fn pause(&self) {
        self.set_paused(true);
    }

    fn resume(&self) {
        self.set_paused(false);
        self.resumed.notify_all();
    }

    fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap() = paused;
    }

    fn wait_resumed(&self) {
        let mut paused = self.paused.lock().unwrap();
        while *paused {
            paused = self.resumed.wait(paused).unwrap();
        }
    }
}

fn run_jit_helper_stdio() -> eyre::Result<()> {
    let mut stdin = BufReader::new(std::io::stdin().lock());
    let config = Arc::new(read_helper_init(&mut stdin)?);
    let worker_count = config.tuning.jit_worker_count.max(1);
    let pool = ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .thread_name(|i| format!("evm2_jit-helper-{i:02}"))
        .exit_handler(|_| {
            HELPER_COMPILER.with_borrow_mut(Option::take);
        })
        .build()?;
    let stdout = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let pause_state = Arc::new(HelperPauseState::default());

    loop {
        let request = match read_helper_request(&mut stdin) {
            Ok(request) => request,
            Err(err) if is_unexpected_eof(&err) => break,
            Err(err) => return Err(err),
        };
        match request {
            HelperWork::Compile { id, job } => {
                let config = Arc::clone(&config);
                let stdout = Arc::clone(&stdout);
                let pause_state = Arc::clone(&pause_state);
                pool.spawn_fifo(move || {
                    let result = HELPER_COMPILER.with_borrow_mut(|compiler| {
                        compile_with_state(job, &config, CompilerTarget::JitObject, compiler)
                    });
                    pause_state.wait_resumed();
                    let mut stdout = stdout.lock().unwrap();
                    if let Err(err) = write_helper_result(&mut stdout, id, result)
                        .and_then(|()| stdout.flush().map_err(Into::into))
                    {
                        error!(%err, "failed to write helper result");
                        std::process::exit(1);
                    }
                });
            }
            HelperWork::Pause { id } => {
                pause_state.pause();
                let mut stdout = stdout.lock().unwrap();
                write_pause_ack(&mut stdout, id)?;
                stdout.flush()?;
            }
            HelperWork::Resume => {
                pause_state.resume();
            }
        }
    }

    Ok(())
}

fn read_helper_init<R: Read + ?Sized>(stdin: &mut BufReader<R>) -> eyre::Result<RuntimeConfig> {
    match read_message(stdin)? {
        HelperRequest::Init(init) => runtime_config_from_init(init),
        HelperRequest::Compile(_) | HelperRequest::Pause { .. } | HelperRequest::Resume => {
            eyre::bail!("JIT helper received request before init");
        }
    }
}

enum HelperWork {
    Compile { id: u64, job: CompileJob },
    Pause { id: u64 },
    Resume,
}

fn read_helper_request<R: Read + ?Sized>(stdin: &mut BufReader<R>) -> eyre::Result<HelperWork> {
    let req = match read_message(stdin)? {
        HelperRequest::Compile(req) => req,
        HelperRequest::Pause { id } => return Ok(HelperWork::Pause { id }),
        HelperRequest::Resume => return Ok(HelperWork::Resume),
        HelperRequest::Init(_) => {
            eyre::bail!("JIT helper received duplicate init");
        }
    };
    let spec_id = SpecId::try_from_u32(u32::from(req.spec_id)).ok_or_eyre("invalid spec id")?;
    let opt_level = opt_level_from_u8(req.opt_level)?;

    let job = CompileJob {
        kind: CompilationKind::Jit,
        key: RuntimeCacheKey { code_hash: B256::from(req.code_hash), spec_id },
        bytecode: Bytes::from(req.bytecode),
        symbol_name: req.symbol_name,
        opt_level,
        sync_notifier: SyncNotifier::none(),
        generation: 0,
    };
    Ok(HelperWork::Compile { id: req.id, job })
}

fn write_helper_result<W: Write + ?Sized>(
    stdout: &mut BufWriter<W>,
    id: u64,
    result: WorkerResult,
) -> eyre::Result<()> {
    let timings = result.timings.into();
    let response = match result.outcome {
        Ok(WorkerSuccess::JitObject(success)) => HelperResponse::Ok {
            id,
            symbol_name: success.symbol_name,
            object_bytes: success.object_bytes.to_vec(),
            builtin_symbols: success.builtin_symbols,
            timings,
        },
        Ok(_) => unreachable!(),
        Err(error) => HelperResponse::Err { id, error, timings },
    };
    write_message(stdout, &response)?;
    Ok(())
}

fn write_pause_ack<W: Write + ?Sized>(stdout: &mut BufWriter<W>, id: u64) -> eyre::Result<()> {
    write_message(stdout, &HelperResponse::Paused { id })?;
    Ok(())
}

fn write_message<T, W: Write + ?Sized>(w: &mut BufWriter<W>, message: &T) -> std::io::Result<()>
where
    T: SchemaWrite<wincode::config::DefaultConfig, Src = T>,
{
    wincode::serialize_into(w, message)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn read_message<T, R: Read + ?Sized>(r: &mut BufReader<R>) -> std::io::Result<T>
where
    T: wincode::SchemaReadOwned<wincode::config::DefaultConfig, Dst = T>,
{
    wincode::deserialize_from(r).map_err(|e| std::io::Error::new(read_error_kind(&e), e))
}

const fn read_error_kind(err: &wincode::ReadError) -> std::io::ErrorKind {
    match err {
        wincode::ReadError::Io(wincode::io::ReadError::ReadSizeLimit(_)) => {
            std::io::ErrorKind::UnexpectedEof
        }
        _ => std::io::ErrorKind::InvalidData,
    }
}

fn is_unexpected_eof(err: &eyre::Report) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|err| err.kind() == std::io::ErrorKind::UnexpectedEof)
}

const fn opt_level_to_u8(level: OptimizationLevel) -> u8 {
    match level {
        OptimizationLevel::None => 0,
        OptimizationLevel::Less => 1,
        OptimizationLevel::Default => 2,
        OptimizationLevel::Aggressive => 3,
    }
}

fn opt_level_from_u8(level: u8) -> eyre::Result<OptimizationLevel> {
    Ok(match level {
        0 => OptimizationLevel::None,
        1 => OptimizationLevel::Less,
        2 => OptimizationLevel::Default,
        3 => OptimizationLevel::Aggressive,
        _ => {
            eyre::bail!("invalid optimization level");
        }
    })
}

fn gas_params_to_pairs(gas_params: &GasParams) -> GasParamPairs {
    (0..GAS_PARAM_COUNT)
        .map(|i| {
            let id = i as u8;
            let gas_id = GasId::from_usize(i).expect("gas parameter index should be valid");
            (id, gas_params[gas_id])
        })
        .collect()
}

fn gas_params_from_pairs(pairs: GasParamPairs) -> eyre::Result<GasParams> {
    eyre::ensure!(pairs.len() == GAS_PARAM_COUNT, "invalid gas params length: {}", pairs.len());

    let mut seen = [false; GAS_PARAM_COUNT];
    let mut gas_params = Version::base(SpecId::default()).gas_params;
    for (id, value) in pairs {
        let index = usize::from(id);
        let Some(gas_id) = GasId::from_usize(index) else {
            eyre::bail!("invalid gas param id: {id}");
        };
        eyre::ensure!(!seen[index], "duplicate gas param id: {id}");
        seen[index] = true;
        gas_params.set(gas_id, value);
    }
    if let Some((id, _)) = seen.iter().enumerate().find(|(_, seen)| !**seen) {
        eyre::bail!("missing gas param id: {id}");
    }

    Ok(gas_params)
}

/// Returns the path that re-executes the running binary, even after it is replaced on disk.
///
/// Only Linux can do this; elsewhere the executable path is resolved once and cached.
fn default_helper_path() -> Result<PathBuf, String> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    static EXE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    EXE.get_or_init(|| {
        std::env::current_exe().map_err(|e| format!("failed to locate current exe: {e}"))
    })
    .clone()
}

fn helper_init(config: &RuntimeConfig) -> HelperInit {
    HelperInit {
        debug_assertions: config.debug_assertions,
        single_error: config.single_error,
        no_dedup: config.no_dedup,
        no_dse: config.no_dse,
        dump_dir: config.dump_dir.as_ref().map(|path| path.to_string_lossy().into_owned()),
        gas_params: config.gas_params.as_ref().map(gas_params_to_pairs),
        layout: config.host.layout.to_words().into_iter().map(|word| word as u64).collect(),
        opcode_view: config.host.opcodes.as_ref().map(|opcodes| Box::new((*opcodes.view).clone())),
        symbol_suffix: config.host.resolver.map(|resolver| resolver as usize as u64),
        jit_worker_count: config.tuning.jit_worker_count,
        compiler_recycle_threshold: config.tuning.compiler_recycle_threshold,
    }
}

fn runtime_config_from_init(init: HelperInit) -> eyre::Result<RuntimeConfig> {
    // The helper only compiles: builtins stay symbolic and the parent links them to the
    // addresses of its own host type.
    let mut host = HostConfig::for_types::<BaseEvmTypes>();
    let Ok(words) = <[u64; HostLayout::WORDS]>::try_from(init.layout) else {
        eyre::bail!("invalid host layout length");
    };
    host.layout = HostLayout::from_words(words.map(|word| word as usize));
    host.symbol_suffix = init.symbol_suffix.map(|suffix| suffix as usize);
    host.opcodes = init.opcode_view.map(|view| HostOpcodes { view: Arc::new(*view), config: 0 });
    let mut config = RuntimeConfig {
        host,
        dump_dir: init.dump_dir.map(PathBuf::from),
        debug_assertions: init.debug_assertions,
        single_error: init.single_error,
        no_dedup: init.no_dedup,
        no_dse: init.no_dse,
        gas_params: init.gas_params.map(gas_params_from_pairs).transpose()?,
        ..Default::default()
    };
    config.tuning.jit_worker_count = init.jit_worker_count;
    config.tuning.compiler_recycle_threshold = init.compiler_recycle_threshold;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::OpcodeConfig;

    #[test]
    fn helper_init_carries_the_host_layout_and_opcode_view() {
        let view = OpcodeConfig::<BaseEvmTypes>::base_for_spec(SpecId::CANCUN).view(None);
        let mut host = HostConfig::for_types::<BaseEvmTypes>();
        host.opcodes = Some(HostOpcodes { view: Arc::new(view.clone()), config: 0 });
        let mut words = host.layout.to_words();
        words[1] += 8;
        host.layout = HostLayout::from_words(words);
        let config = RuntimeConfig { host: host.clone(), ..Default::default() };
        let helper = runtime_config_from_init(helper_init(&config)).unwrap();
        assert_eq!(helper.host.layout, host.layout);
        assert_eq!(helper.host.opcodes.as_ref().map(|o| &*o.view), Some(&view));
    }

    #[test]
    fn job_timeout_does_not_count_paused_time() {
        let paused = Arc::new(AtomicBool::new(true));
        let (tx, rx) = chan::bounded(1);
        let thread = std::thread::spawn({
            let paused = Arc::clone(&paused);
            move || {
                std::thread::sleep(Duration::from_millis(300));
                paused.store(false, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(400));
                tx.send(()).unwrap();
            }
        });
        // 700ms elapse, but only 400ms of them unpaused.
        assert!(recv_unpaused(&rx, Duration::from_millis(500), &paused).is_ok());
        thread.join().unwrap();

        let (_tx, rx) = chan::bounded::<()>(1);
        let start = Instant::now();
        let result = recv_unpaused(&rx, Duration::from_millis(100), &paused);
        assert_eq!(result, Err(chan::RecvTimeoutError::Timeout));
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn helper_pause_state_blocks_until_resume() {
        let pause_state = Arc::new(HelperPauseState::default());
        pause_state.pause();

        let (tx, rx) = chan::bounded(1);
        let thread_pause_state = Arc::clone(&pause_state);
        let thread = std::thread::spawn(move || {
            thread_pause_state.wait_resumed();
            tx.send(()).unwrap();
        });

        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        pause_state.resume();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn helper_pause_protocol_roundtrips() {
        let mut request = Vec::new();
        {
            let mut writer = BufWriter::new(&mut request);
            write_pause(&mut writer, 42).unwrap();
            writer.flush().unwrap();
        }
        let mut reader = BufReader::new(request.as_slice());
        match read_helper_request(&mut reader).unwrap() {
            HelperWork::Pause { id } => assert_eq!(id, 42),
            HelperWork::Compile { .. } | HelperWork::Resume => panic!("unexpected helper request"),
        }

        let mut response = Vec::new();
        {
            let mut writer = BufWriter::new(&mut response);
            write_pause_ack(&mut writer, 42).unwrap();
            writer.flush().unwrap();
        }
        let mut reader = BufReader::new(response.as_slice());
        match read_helper_response(&mut reader).unwrap() {
            HelperResponseMessage::Paused(id) => assert_eq!(id, 42),
            HelperResponseMessage::Job(..) => panic!("unexpected helper response"),
        }
    }

    #[test]
    fn helper_pause_resume_requests_update_stats() {
        let stats = Arc::new(RuntimeStats::default());
        let helper = HelperProcess::new(Arc::clone(&stats));

        helper.pause();
        helper.pause();
        helper.resume();

        let snapshot = stats.snapshot(crate::runtime::stats::RuntimeStatsGauges::default());
        assert_eq!(snapshot.jit_helper_pause_requests, 2);
        assert_eq!(snapshot.jit_helper_pause_acknowledgements, 0);
        assert_eq!(snapshot.jit_helper_pause_failures, 0);
        assert_eq!(snapshot.jit_helper_pause_timeouts, 0);
        assert_eq!(snapshot.jit_helper_resume_requests, 1);
        assert_eq!(snapshot.jit_helper_resume_failures, 0);
    }

    #[cfg(target_os = "linux")]
    const REPLACE_EXE_ENV: &str = "EVM2_TEST_REPLACE_EXE";

    #[cfg(target_os = "linux")]
    #[test]
    fn default_helper_path_survives_exe_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("test-bin");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();

        let output = Command::new(&exe)
            .env(REPLACE_EXE_ENV, "1")
            .args(["--exact", "runtime::out_of_process::tests::replace_exe_child", "--ignored"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Runs only as a child of `default_helper_path_survives_exe_replacement`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "child of default_helper_path_survives_exe_replacement"]
    fn replace_exe_child() {
        if std::env::var_os(REPLACE_EXE_ENV).is_none() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let original = std::fs::read(&exe).unwrap();

        let replacement = exe.with_extension("new");
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::rename(&replacement, &exe).unwrap();

        let helper_path = default_helper_path().unwrap();
        assert_eq!(std::fs::read(helper_path).unwrap(), original);
    }
}
