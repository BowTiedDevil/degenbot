//! EVM execution inspection hooks.

use crate::{
    EvmTypesHost,
    evm::NonStaticAny,
    interpreter::{Interpreter, Message, MessageResult},
};
use alloc::boxed::Box;
use alloy_primitives::{Address, Log, U256};
use auto_impl::auto_impl;

/// Set of opcodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpcodeSet(U256);

impl OpcodeSet {
    /// Set containing no opcode.
    pub const EMPTY: Self = Self(U256::ZERO);

    /// Set containing every opcode value.
    pub const ALL: Self = Self(U256::MAX);

    /// Returns whether the set contains `opcode`.
    #[inline]
    pub const fn contains(&self, opcode: u8) -> bool {
        self.0.bit(opcode as usize)
    }

    /// Adds `opcode` to the set.
    #[inline]
    pub const fn insert(&mut self, opcode: u8) {
        self.0.set_bit(opcode as usize, true);
    }

    /// Returns the opcodes in `self` or `other`.
    #[inline]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl FromIterator<u8> for OpcodeSet {
    fn from_iter<I: IntoIterator<Item = u8>>(opcodes: I) -> Self {
        let mut set = Self::EMPTY;
        for opcode in opcodes {
            set.insert(opcode);
        }
        set
    }
}

/// What an [`Inspector`] observes of execution.
///
/// Returned by [`Inspector::inspector_config`]. The EVM reads it when a frame starts and after each
/// `hooked` instruction of the frame, such as a CALL or CREATE, and runs the frame so that it only
/// pays for what the config observes. The default observes everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectorConfig {
    steps: OpcodeSet,
}

impl InspectorConfig {
    /// Creates a config that observes everything.
    #[inline]
    pub const fn new() -> Self {
        Self { steps: OpcodeSet::ALL }
    }

    /// Creates a config that calls no step hooks.
    #[inline]
    pub const fn without_steps() -> Self {
        Self { steps: OpcodeSet::EMPTY }
    }

    /// Sets the opcodes for which [`Inspector::step`] and [`Inspector::step_end`] are called.
    ///
    /// The EVM may call the step hooks for more opcodes than set, for example when several
    /// inspectors share a frame, so a hook must ignore the opcodes it does not handle. Call,
    /// create, log and self-destruct hooks run regardless.
    #[inline]
    pub const fn with_steps(mut self, steps: OpcodeSet) -> Self {
        self.steps = steps;
        self
    }

    /// Returns the opcodes for which [`Inspector::step`] and [`Inspector::step_end`] are called.
    #[inline]
    pub const fn steps(&self) -> OpcodeSet {
        self.steps
    }

    /// Returns a config that observes everything either `self` or `other` observes.
    #[inline]
    pub fn union(self, other: Self) -> Self {
        self.with_steps(self.steps.union(other.steps))
    }
}

impl Default for InspectorConfig {
    /// Returns a config that observes everything.
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// EVM execution inspector.
#[auto_impl(&mut, Box)]
pub trait Inspector<T: EvmTypesHost>: NonStaticAny {
    /// Called after a frame interpreter has been initialized.
    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Returns what this inspector observes, as an [`InspectorConfig`].
    ///
    /// Read when a frame starts and after each `hooked` instruction of the frame returns, such as a
    /// CALL or CREATE, so a child message may change the answer. A change that a hook makes
    /// itself, such as in [`Self::step`], takes effect after the next `hooked` instruction of the
    /// frame. The default observes everything.
    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        InspectorConfig::new()
    }

    /// Called before each instruction executes.
    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called after each instruction executes.
    ///
    /// If the instruction ran out of gas, the interpreter's remaining gas reads 0.
    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        let _ = interp;
    }

    /// Called when a log is emitted.
    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        let _ = log;
        let _ = host;
    }

    /// Called before a call message executes.
    ///
    /// The interpreter is the currently running frame whose instruction produced the message; for
    /// the top-level message it is a frame initialized with the message itself.
    ///
    /// A first frame that runs out of gas in the transaction's runtime gas phase halts without
    /// executing: this hook still fires for it, with a message that carries no bytecode, a
    /// returned result is ignored, and [`Inspector::call_end`] receives the out-of-gas result.
    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let _ = interp;
        let _ = message;
        None
    }

    /// Called after a call message executes.
    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        let _ = interp;
        let _ = message;
        let _ = result;
    }

    /// Called before a create message executes.
    ///
    /// The interpreter is the currently running frame whose instruction produced the message; for
    /// the top-level message it is a frame initialized with the message itself.
    ///
    /// A first frame that runs out of gas in the transaction's runtime gas phase halts without
    /// executing: this hook still fires for it, with a message that carries no bytecode, a
    /// returned result is ignored, and [`Inspector::create_end`] receives the out-of-gas result.
    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let _ = interp;
        let _ = message;
        None
    }

    /// Called after a create message executes.
    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        let _ = interp;
        let _ = message;
        let _ = result;
    }

    /// Called after a contract self-destructs.
    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        let _ = contract;
        let _ = target;
        let _ = value;
        let _ = host;
    }
}

#[inline]
pub(crate) fn boxed_inspector<'a, T: EvmTypesHost>(
    inspector: impl Inspector<T> + 'a,
) -> Box<dyn Inspector<T> + 'a> {
    Box::new(inspector)
}

/// Inspector that does nothing.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoopInspector(());

impl<T: EvmTypesHost> Inspector<T> for NoopInspector {
    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        InspectorConfig::without_steps()
    }
}

/// Runs both inspectors, `L` first.
///
/// Every hook is forwarded to both, and the steps are those of either inspector. When both `call`
/// or `create` hooks return a result, the result from `L` wins.
impl<T: EvmTypesHost, L: Inspector<T>, R: Inspector<T>> Inspector<T> for (L, R) {
    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        self.0.inspector_config().union(self.1.inspector_config())
    }

    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.0.initialize_interp(interp);
        self.1.initialize_interp(interp);
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.0.step(interp);
        self.1.step(interp);
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        self.0.step_end(interp);
        self.1.step_end(interp);
    }

    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        self.0.log(log, host);
        self.1.log(log, host);
    }

    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let first = self.0.call(interp, message);
        let second = self.1.call(interp, message);
        first.or(second)
    }

    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.0.call_end(interp, message, result);
        self.1.call_end(interp, message, result);
    }

    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        let first = self.0.create(interp, message);
        let second = self.1.create(interp, message);
        first.or(second)
    }

    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        self.0.create_end(interp, message, result);
        self.1.create_end(interp, message, result);
    }

    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        self.0.selfdestruct(contract, target, value, host);
        self.1.selfdestruct(contract, target, value, host);
    }
}

/// Forwards every hook to the inner inspector when present; `None` behaves like
/// [`NoopInspector`] and steps nothing.
impl<T: EvmTypesHost, I: Inspector<T>> Inspector<T> for Option<I> {
    #[inline]
    fn inspector_config(&self) -> InspectorConfig {
        self.as_ref().map_or_else(InspectorConfig::without_steps, I::inspector_config)
    }

    #[inline]
    fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if let Some(inspector) = self {
            inspector.initialize_interp(interp);
        }
    }

    #[inline]
    fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if let Some(inspector) = self {
            inspector.step(interp);
        }
    }

    #[inline]
    fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
        if let Some(inspector) = self {
            inspector.step_end(interp);
        }
    }

    #[inline]
    fn log(&mut self, log: &Log, host: &mut T::Host<'_>) {
        if let Some(inspector) = self {
            inspector.log(log, host);
        }
    }

    #[inline]
    fn call(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.as_mut()?.call(interp, message)
    }

    #[inline]
    fn call_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if let Some(inspector) = self {
            inspector.call_end(interp, message, result);
        }
    }

    #[inline]
    fn create(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &mut Message<T>,
    ) -> Option<MessageResult<T>> {
        self.as_mut()?.create(interp, message)
    }

    #[inline]
    fn create_end(
        &mut self,
        interp: &mut Interpreter<'_, '_, T>,
        message: &Message<T>,
        result: &mut MessageResult<T>,
    ) {
        if let Some(inspector) = self {
            inspector.create_end(interp, message, result);
        }
    }

    #[inline]
    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        host: &mut T::Host<'_>,
    ) {
        if let Some(inspector) = self {
            inspector.selfdestruct(contract, target, value, host);
        }
    }
}

impl<'a, T: EvmTypesHost> core::ops::Deref for dyn Inspector<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypesHost> core::ops::DerefMut for dyn Inspector<T> + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfig, BaseEvmTypes, DatabaseError, Evm, EvmConfig, EvmTypesHost, ExecutionConfig,
        OpcodeConfig, Precompiles, SpecId, Version,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, EmptyDB, InMemoryDB, SYSTEM_ADDRESS, State, StateCheckpoint},
        interpreter::{
            GasTracker, Host, InstrStop, Interpreter, Message, MessageExt, MessageKind,
            MessageResult, MessageResultExt, Word, instructions as instr, op,
        },
        registry::TxRegistry,
        test_utils::{
            TestHost, TestTypes, call_code, create_code, legacy_bytecode, push, push_all,
            return_top_word,
        },
        utils::{WordExt, address_to_word},
    };
    use alloc::{boxed::Box, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, Bytes, Log, TxKind, U256};
    use core::assert_matches;
    use evm2_macros::instruction;

    #[derive(Default)]
    struct SelfdestructInspector {
        selfdestruct: Option<(Address, Address, Word)>,
    }

    impl<T: EvmTypesHost> Inspector<T> for SelfdestructInspector {
        fn selfdestruct(
            &mut self,
            contract: &Address,
            target: &Address,
            value: &Word,
            _host: &mut T::Host<'_>,
        ) {
            self.selfdestruct = Some((*contract, *target, *value));
        }
    }

    #[derive(Default)]
    struct HookInspector {
        call_depths: Vec<u16>,
        call_end_stops: Vec<InstrStop>,
        create_depths: Vec<u16>,
        create_destinations: Vec<Address>,
        create_end_stops: Vec<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for HookInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.call_depths.push(message.depth);
            None
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.call_end_stops.push(result.stop);
        }

        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            assert_eq!(message.code_address, message.destination);
            self.create_depths.push(message.depth);
            self.create_destinations.push(message.destination);
            None
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.create_end_stops.push(result.stop);
        }
    }

    struct OverrideCallInspector {
        result: MessageResult<BaseEvmTypes>,
        min_depth: u16,
        call_depth: Option<u16>,
        call_end_stop: Option<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for OverrideCallInspector {
        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            if message.depth < self.min_depth {
                return None;
            }
            self.call_depth = Some(message.depth);
            let mut result = self.result.clone();
            result.gas.set_remaining(message.gas_limit);
            Some(result)
        }

        fn call_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            if message.depth >= self.min_depth {
                self.call_end_stop = Some(result.stop);
            }
        }
    }

    struct OverrideCreateInspector {
        created: Address,
        create_depth: Option<u16>,
        create_end_stop: Option<InstrStop>,
    }

    impl Inspector<BaseEvmTypes> for OverrideCreateInspector {
        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.create_depth = Some(message.depth);
            Some(MessageResultExt {
                stop: InstrStop::Return,
                gas: GasTracker::new(message.gas_limit),
                created_address: Some(self.created),
                ..Default::default()
            })
        }

        fn create_end(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            result: &mut MessageResult<BaseEvmTypes>,
        ) {
            self.create_end_stop = Some(result.stop);
        }
    }

    #[derive(Default)]
    struct LogInspector {
        logs: Vec<Log>,
    }

    impl<T: EvmTypesHost> Inspector<T> for LogInspector {
        fn log(&mut self, log: &Log, _host: &mut T::Host<'_>) {
            self.logs.push(log.clone());
        }
    }

    #[derive(Default)]
    struct E2eState {
        initialized: usize,
        steps: usize,
        step_ends: usize,
        logs: Vec<Log>,
        calls: usize,
        creates: usize,
    }

    #[derive(Default)]
    struct SharedE2eInspector {
        state: E2eState,
    }

    impl Inspector<BaseEvmTypes> for SharedE2eInspector {
        fn initialize_interp(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.initialized += 1;
        }

        fn step(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.steps += 1;
        }

        fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.state.step_ends += 1;
        }

        fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
            self.state.logs.push(log.clone());
        }

        fn call(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.calls += 1;
            None
        }

        fn create(
            &mut self,
            _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.state.creates += 1;
            None
        }
    }

    fn run_evm_with_inspector<I: Inspector<BaseEvmTypes> + 'static>(
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        run_evm_with_inspector_db(InMemoryDB::default(), code, message, gas_limit, inspector)
    }

    fn run_evm_with_inspector_db<I: Inspector<BaseEvmTypes> + 'static>(
        db: InMemoryDB,
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        run_evm_with_inspector_db_spec(SpecId::OSAKA, db, code, message, gas_limit, inspector)
    }

    fn run_evm_with_inspector_db_spec<I: Inspector<BaseEvmTypes> + 'static>(
        spec_id: SpecId,
        db: InMemoryDB,
        code: Vec<u8>,
        message: &Message<BaseEvmTypes>,
        gas_limit: u64,
        inspector: I,
    ) -> (MessageResult<BaseEvmTypes>, Box<I>, Evm<'static, BaseEvmTypes>) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            spec_id,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(spec_id),
        );
        evm.set_inspector(inspector);
        let bytecode = legacy_bytecode(code);
        let mut message = MessageExt { gas_limit, code: bytecode, ..message.clone() };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        let inspector = evm.clear_inspector_as::<I>().unwrap();
        (result, inspector, evm)
    }

    #[test]
    fn empty_calls_do_not_inspect_padding_stop() {
        for nested in [false, true] {
            let code = if nested {
                let mut code = call_code(Address::repeat_byte(0x11));
                code.extend([op::CALL, op::POP, op::STOP]);
                code
            } else {
                Vec::new()
            };
            let (result, inspector, _) = run_evm_with_inspector(
                code,
                &MessageExt::default(),
                100_000,
                SharedE2eInspector::default(),
            );
            assert_eq!(result.stop, InstrStop::Stop);
            assert!(result.output.is_empty());
            assert_eq!(inspector.state.calls, if nested { 2 } else { 1 });
            assert_eq!(inspector.state.initialized, usize::from(nested));
            assert_eq!(inspector.state.steps, if nested { 10 } else { 0 });
            assert_eq!(inspector.state.step_ends, inspector.state.steps);
            if !nested {
                assert_eq!(result.gas.remaining(), 100_000);
            }
        }
    }

    #[test]
    fn inspect_run_steps() {
        #[derive(Default)]
        struct StepInspector {
            steps: usize,
            step_ends: usize,
        }

        impl<T: EvmTypesHost> Inspector<T> for StepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::STOP]),
            &MessageExt::default(),
            10_000,
            StepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    /// Records the stack and gas around the steps its config asks for.
    #[derive(Default)]
    struct ConfigInspector {
        before: OpcodeSet,
        after: Option<OpcodeSet>,
        switched: bool,
        /// Opcodes between their `step` and `step_end`, innermost frame last.
        running: Vec<u8>,
        steps: Vec<(u8, u64, Vec<Word>)>,
        step_ends: Vec<(u8, u64, Vec<Word>)>,
        calls: usize,
        creates: usize,
        /// The pc, opcode and stack that the call and create hooks of child messages see.
        views: Vec<(usize, u8, Vec<Word>)>,
        logs: Vec<Log>,
        /// Stops the parent frame when a child message ends.
        stop_after_child: bool,
        /// Stops the frame in `step_end` of this opcode.
        stop_in_step_end: Option<u8>,
        /// Stops every frame before it runs.
        stop_on_init: bool,
        /// Answers the calls of child messages in the `call` hook, so no child frame runs.
        answer_calls: Option<MessageResult<BaseEvmTypes>>,
    }

    impl ConfigInspector {
        fn new(steps: OpcodeSet) -> Self {
            Self { before: steps, ..Default::default() }
        }

        /// Uses `after` instead of `before` once a child message starts.
        fn switching_to(mut self, after: OpcodeSet) -> Self {
            self.after = Some(after);
            self
        }

        /// Answers the calls of child messages with `result` in the `call` hook.
        fn answering_calls(mut self, result: MessageResult<BaseEvmTypes>) -> Self {
            self.answer_calls = Some(result);
            self
        }
    }

    impl Inspector<BaseEvmTypes> for ConfigInspector {
        fn inspector_config(&self) -> InspectorConfig {
            let steps = match self.after {
                Some(after) if self.switched => after,
                _ => self.before,
            };
            InspectorConfig::new().with_steps(steps)
        }

        fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            let opcode = interp.opcode();
            self.running.push(opcode);
            self.steps.push((opcode, interp.gas().remaining(), interp.stack().to_vec()));
        }

        fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            let opcode = self.running.pop().unwrap();
            self.step_ends.push((opcode, interp.gas().remaining(), interp.stack().to_vec()));
            if self.stop_in_step_end == Some(opcode) {
                interp.set_stop(InstrStop::Revert);
            }
        }

        fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            if self.stop_on_init {
                interp.set_stop(InstrStop::Revert);
            }
        }

        fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
            self.logs.push(log.clone());
        }

        fn call(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.calls += 1;
            if message.depth > 0 {
                self.views.push((interp.pc(), interp.opcode(), interp.stack().to_vec()));
            }
            // Like a cheatcode that changes what its caller's inspector wants to see.
            self.switched |= message.depth > 0;
            if message.depth > 0
                && let Some(answer) = &self.answer_calls
            {
                let mut result = answer.clone();
                result.gas.set_remaining(message.gas_limit);
                return Some(result);
            }
            None
        }

        fn call_end(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &Message<BaseEvmTypes>,
            _result: &mut MessageResult<BaseEvmTypes>,
        ) {
            if self.stop_after_child && message.depth > 0 {
                interp.set_stop(InstrStop::Revert);
            }
        }

        fn create(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message<BaseEvmTypes>,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.creates += 1;
            if message.depth > 0 {
                self.views.push((interp.pc(), interp.opcode(), interp.stack().to_vec()));
            }
            None
        }
    }

    fn opcode_set(opcodes: impl IntoIterator<Item = u8>) -> OpcodeSet {
        opcodes.into_iter().collect()
    }

    /// Runs `code` through the first call of a frame without an inspector.
    fn run_evm_uninspected(
        db: InMemoryDB,
        code: Vec<u8>,
        gas_limit: u64,
    ) -> (MessageResult<BaseEvmTypes>, Vec<Log>) {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::OSAKA),
        );
        let mut message =
            MessageExt { gas_limit, code: legacy_bytecode(code), ..Default::default() };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        (result, evm.logs().to_vec())
    }

    /// Code that stores, loads, logs, calls a child that loads a slot, and creates.
    fn config_test_code() -> (InMemoryDB, Vec<u8>) {
        let child = Address::repeat_byte(0x66);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &child,
            AccountInfo::default().with_code(legacy_bytecode(vec![
                op::PUSH0,
                op::SLOAD,
                op::POP,
                op::STOP,
            ])),
        );
        let mut code = vec![op::PUSH1, 5, op::PUSH0, op::SSTORE, op::PUSH0, op::SLOAD, op::POP];
        code.extend([op::PUSH1, 0xaa, op::PUSH0, op::MSTORE8, op::PUSH1, 1, op::PUSH0, op::LOG0]);
        code.extend(call_code(child));
        code.extend([op::CALL, op::POP]);
        code.extend(create_code());
        code.extend([op::CREATE, op::POP, op::PUSH0, op::SLOAD, op::POP, op::STOP]);
        (db, code)
    }

    #[test]
    fn sparse_steps_match_full_inspector_at_those_opcodes() {
        let (db, code) = config_test_code();
        let (expected, expected_logs) = run_evm_uninspected(db.clone(), code.clone(), 1_000_000);
        let (result, full, _) = run_evm_with_inspector_db(
            db.clone(),
            code.clone(),
            &MessageExt::default(),
            1_000_000,
            ConfigInspector::new(OpcodeSet::ALL),
        );
        assert_eq!(result, expected);
        assert_eq!(full.logs, expected_logs);

        for opcodes in [
            vec![op::SLOAD, op::SSTORE, op::CALL, op::CREATE],
            vec![op::SLOAD],
            vec![op::CALL, op::CREATE, op::CREATE2],
            vec![op::POP, op::STOP, op::LOG0],
            vec![op::INVALID],
        ] {
            let steps = opcode_set(opcodes);
            let (result, sparse, _) = run_evm_with_inspector_db(
                db.clone(),
                code.clone(),
                &MessageExt::default(),
                1_000_000,
                ConfigInspector::new(steps),
            );
            let filter = |records: &[(u8, u64, Vec<Word>)]| {
                records
                    .iter()
                    .filter(|record| steps.contains(record.0))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(result, expected, "{steps:?}");
            assert_eq!(sparse.logs, expected_logs);
            assert_eq!(sparse.steps, filter(&full.steps));
            assert_eq!(sparse.step_ends, filter(&full.step_ends));
            assert_eq!((sparse.calls, sparse.creates), (full.calls, full.creates));
        }
    }

    #[test]
    fn message_hook_can_stop_the_parent_without_steps() {
        let (db, code) = config_test_code();
        let run = |steps| {
            let inspector =
                ConfigInspector { stop_after_child: true, ..ConfigInspector::new(steps) };
            run_evm_with_inspector_db(
                db.clone(),
                code.clone(),
                &MessageExt::default(),
                1_000_000,
                inspector,
            )
        };
        let (full, _, _) = run(OpcodeSet::ALL);
        assert_eq!(full.stop, InstrStop::Revert);
        for steps in [OpcodeSet::EMPTY, opcode_set([op::SLOAD]), opcode_set([op::CALL])] {
            let (result, inspector, _) = run(steps);
            assert_eq!(result, full);
            // The frame stopped at the CALL, before the later CREATE.
            assert_eq!(inspector.creates, 0);
        }
        // The stop wins over a reload for the changed steps.
        let inspector = ConfigInspector {
            stop_after_child: true,
            ..ConfigInspector::new(OpcodeSet::EMPTY).switching_to(OpcodeSet::ALL)
        };
        let (result, inspector, _) = run_evm_with_inspector_db(
            db.clone(),
            code.clone(),
            &MessageExt::default(),
            1_000_000,
            inspector,
        );
        assert_eq!(result, full);
        assert_eq!(inspector.creates, 0);
    }

    #[test]
    fn step_end_stop_wins_over_a_reload() {
        let (db, code) = config_test_code();
        let run = |inspector| {
            run_evm_with_inspector_db(
                db.clone(),
                code.clone(),
                &MessageExt::default(),
                1_000_000,
                inspector,
            )
        };
        let (full, _, _) = run(ConfigInspector {
            stop_in_step_end: Some(op::CALL),
            ..ConfigInspector::new(OpcodeSet::ALL)
        });
        assert_eq!(full.stop, InstrStop::Revert);
        // The child message of the stepped CALL changes the steps while its `step_end` stops.
        let (result, inspector, _) = run(ConfigInspector {
            stop_in_step_end: Some(op::CALL),
            ..ConfigInspector::new(opcode_set([op::CALL])).switching_to(opcode_set([op::SSTORE]))
        });
        assert_eq!(result, full);
        // The frame stopped at the CALL, before the later CREATE.
        assert_eq!(inspector.creates, 0);
    }

    #[test]
    fn message_hooks_see_the_message_instruction_whatever_the_steps() {
        let (db, code) = config_test_code();
        let views = |steps| {
            run_evm_with_inspector_db(
                db.clone(),
                code.clone(),
                &MessageExt::default(),
                1_000_000,
                ConfigInspector::new(steps),
            )
            .1
            .views
        };
        let full = views(OpcodeSet::ALL);
        assert_eq!(full.iter().map(|view| view.1).collect::<Vec<_>>(), [op::CALL, op::CREATE]);
        assert_eq!(full[0].2.len(), 7);
        assert_eq!(full[1].2.len(), 3);
        for steps in [OpcodeSet::EMPTY, opcode_set([op::SLOAD]), opcode_set([op::CALL])] {
            assert_eq!(views(steps), full, "{steps:?}");
        }
    }

    #[test]
    fn stop_set_before_the_run_ends_the_frame_without_steps() {
        let code = vec![op::PUSH1, 1, op::PUSH0, op::SSTORE, op::STOP];
        let run = |steps| {
            let inspector = ConfigInspector { stop_on_init: true, ..ConfigInspector::new(steps) };
            run_evm_with_inspector(code.clone(), &MessageExt::default(), 100_000, inspector).0
        };
        let full = run(OpcodeSet::ALL);
        assert_eq!((full.stop, full.gas.remaining()), (InstrStop::Revert, 100_000));
        for steps in [OpcodeSet::EMPTY, opcode_set([op::SLOAD]), opcode_set([op::SSTORE])] {
            assert_eq!(run(steps), full);
        }
    }

    /// The base config with `CALL` also installed at [`CUSTOM_CALL`].
    struct CustomCallConfig;

    const CUSTOM_CALL: u8 = 0xb0;

    impl EvmConfig<BaseEvmTypes> for CustomCallConfig {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<BaseEvmTypes> = &{
            let mut config =
                OpcodeConfig::<BaseEvmTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
            config.set_instruction::<instr::call<BaseEvmTypes>>(CUSTOM_CALL, 100);
            config
        };
    }

    /// The base config with `CALL` installed at `PUSH30`, an opcode with immediate bytes.
    struct PushSlotCallConfig;

    impl EvmConfig<BaseEvmTypes> for PushSlotCallConfig {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<BaseEvmTypes> = &{
            let mut config =
                OpcodeConfig::<BaseEvmTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
            config.set_instruction::<instr::call<BaseEvmTypes>>(op::PUSH30, 100);
            config
        };
    }

    /// A hooked call that pushes a word after `call_with`.
    #[instruction(no_stack_preamble, hooked, EvmTypes = BaseEvmTypes)]
    fn call_then_push(cx: _) -> Result {
        instr::call_with(
            stack.reborrow(),
            cx.gas,
            cx.state,
            MessageKind::Call,
            |_, _, _| Ok(()),
            |_, _, _, _| Ok(()),
        )?;
        stack.push_u64(0x77)?;
        Ok(())
    }

    /// The base config with the call that pushes a word installed at [`CUSTOM_CALL`].
    struct CallThenPushConfig;

    impl EvmConfig<BaseEvmTypes> for CallThenPushConfig {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<BaseEvmTypes> = &{
            let mut config =
                OpcodeConfig::<BaseEvmTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
            config.set_instruction::<call_then_push>(CUSTOM_CALL, 100);
            config
        };
    }

    /// A hooked call that runs its child message through the host instead of `call_with`.
    #[instruction(no_stack_preamble, hooked, EvmTypes = BaseEvmTypes)]
    fn host_call(cx: _) -> Result {
        let [_, to, _, _, _, _, _] = stack.popn::<7>()?;
        let to = to.to_address();
        let current = cx.state.message();
        let mut message = MessageExt {
            kind: MessageKind::Call,
            depth: current.depth + 1,
            gas_limit: 10_000,
            destination: to,
            call_target: to,
            caller: current.destination,
            code_address: to,
            ..Default::default()
        };
        let result =
            cx.state.host().execute_message(&mut message).map_err(|error| cx.state.fail(error))?;
        stack.push_u64(result.stop.is_success() as u64)
    }

    /// The base config with the call through the host installed at [`CUSTOM_CALL`].
    struct HostCallConfig;

    impl EvmConfig<BaseEvmTypes> for HostCallConfig {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<BaseEvmTypes> = &{
            let mut config =
                OpcodeConfig::<BaseEvmTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
            config.set_instruction::<host_call>(CUSTOM_CALL, 100);
            config
        };
    }

    /// A call whose `before` hook overwrites the caller's memory.
    #[instruction(no_stack_preamble, hooked, EvmTypes = BaseEvmTypes)]
    fn overwriting_call(cx: _) -> Result {
        instr::call_with(
            stack,
            cx.gas,
            cx.state,
            MessageKind::Call,
            |_, state, _| {
                state.memory().set(0, &[0xbb; 32]);
                Ok(())
            },
            |_, _, _, _| Ok(()),
        )
    }

    /// The base config with [`overwriting_call`] installed at [`CUSTOM_CALL`].
    struct OverwritingCallConfig;

    impl EvmConfig<BaseEvmTypes> for OverwritingCallConfig {
        const BASE_SPEC_ID: SpecId = SpecId::OSAKA;
        const OPCODE_CONFIG: &'static OpcodeConfig<BaseEvmTypes> = &{
            let mut config =
                OpcodeConfig::<BaseEvmTypes>::base::<BaseEvmConfig<{ SpecId::OSAKA as u32 }>>();
            config.set_instruction::<overwriting_call>(CUSTOM_CALL, 100);
            config
        };
    }

    /// Runs a call to a child through `message_op`, which `C` installs as a call, followed by
    /// `immediates`.
    fn run_custom_call<C: EvmConfig<BaseEvmTypes>>(
        inspector: ConfigInspector,
        message_op: u8,
        immediates: &[u8],
    ) -> (MessageResult<BaseEvmTypes>, ConfigInspector) {
        let mut code = call_code(Address::repeat_byte(0x66));
        code.push(message_op);
        code.extend(immediates);
        code.extend([op::GAS, op::POP, op::STOP]);
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_config::<C>(),
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(inspector);
        let mut message =
            MessageExt { gas_limit: 100_000, code: legacy_bytecode(code), ..Default::default() };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        (result, *evm.clear_inspector_as::<ConfigInspector>().unwrap())
    }

    /// Asserts that the frame steps the set that the child message of [`CUSTOM_CALL`] switches to,
    /// from the opcode after it.
    fn assert_custom_call_rereads_the_steps<C: EvmConfig<BaseEvmTypes>>() {
        let (expected, full) =
            run_custom_call::<C>(ConfigInspector::new(OpcodeSet::ALL), CUSTOM_CALL, &[]);

        // The child message of the custom opcode switches the steps to `GAS`, and no steps before.
        let (result, inspector) = run_custom_call::<C>(
            ConfigInspector::new(OpcodeSet::EMPTY).switching_to(opcode_set([op::GAS])),
            CUSTOM_CALL,
            &[],
        );
        assert_eq!(result, expected);
        let gas = full.steps.iter().position(|record| record.0 == op::GAS).unwrap();
        assert_eq!(inspector.steps, [full.steps[gas].clone()]);
    }

    #[test]
    fn custom_call_opcode_rereads_the_steps() {
        assert_custom_call_rereads_the_steps::<CustomCallConfig>();
    }

    #[test]
    fn hooked_opcode_that_runs_a_message_through_the_host_rereads_the_steps() {
        assert_custom_call_rereads_the_steps::<HostCallConfig>();
    }

    #[test]
    fn custom_call_opcode_with_immediates_resumes_after_them() {
        // The immediates are invalid opcodes that fail the frame if the reload resumes inside them.
        let immediates = [op::INVALID; 30];
        let (expected, _) = run_custom_call::<PushSlotCallConfig>(
            ConfigInspector::new(OpcodeSet::ALL),
            op::PUSH30,
            &immediates,
        );
        assert_eq!(expected.stop, InstrStop::Stop);

        let (result, inspector) = run_custom_call::<PushSlotCallConfig>(
            ConfigInspector::new(OpcodeSet::EMPTY).switching_to(opcode_set([op::GAS])),
            op::PUSH30,
            &immediates,
        );
        assert_eq!(result, expected);
        assert_eq!(inspector.steps.len(), 1);
    }

    #[test]
    fn empty_steps_skip_step_hooks_but_keep_message_and_log_hooks() {
        let (db, code) = config_test_code();
        let (expected, expected_logs) = run_evm_uninspected(db.clone(), code.clone(), 1_000_000);
        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt::default(),
            1_000_000,
            ConfigInspector::new(OpcodeSet::EMPTY),
        );

        assert_eq!(result, expected);
        assert!(inspector.steps.is_empty());
        assert!(inspector.step_ends.is_empty());
        // The top-level message and the child call.
        assert_eq!(inspector.calls, 2);
        assert_eq!(inspector.creates, 1);
        assert_eq!(inspector.logs, expected_logs);
        assert_eq!(inspector.logs.len(), 1);
    }

    /// Runs a `CALL` followed by `GAS`, `POP` and `STOP` through inspectors that switch their steps
    /// when the child message starts, with the call answered by `answer` in the hook if given.
    /// Asserts that the caller steps the new set from the opcode after the `CALL`, and returns the
    /// result and the inspector of the run that steps every opcode.
    fn assert_steps_follow_the_child(
        answer: Option<MessageResult<BaseEvmTypes>>,
    ) -> (MessageResult<BaseEvmTypes>, ConfigInspector) {
        let mut code = call_code(Address::repeat_byte(0x66));
        code.extend([op::CALL, op::GAS, op::POP, op::STOP]);
        let run = |inspector: ConfigInspector| {
            let inspector = match &answer {
                Some(answer) => inspector.answering_calls(answer.clone()),
                None => inspector,
            };
            run_evm_with_inspector(code.clone(), &MessageExt::default(), 100_000, inspector)
        };
        let (expected, full, _) = run(ConfigInspector::new(OpcodeSet::ALL));
        let call = full.steps.iter().position(|record| record.0 == op::CALL).unwrap();
        let end = full.steps.len();

        for (before, after, range, kept) in [
            // Starts stepping after the CALL.
            (OpcodeSet::EMPTY, OpcodeSet::ALL, call + 1..end, OpcodeSet::ALL),
            // Stops stepping after the CALL.
            (OpcodeSet::ALL, OpcodeSet::EMPTY, 0..call + 1, OpcodeSet::ALL),
            (
                OpcodeSet::EMPTY,
                opcode_set([op::GAS, op::STOP]),
                call + 1..end,
                opcode_set([op::GAS, op::STOP]),
            ),
            (
                opcode_set([op::CALL]),
                opcode_set([op::POP]),
                call..end,
                opcode_set([op::CALL, op::POP]),
            ),
        ] {
            let (result, inspector, _) = run(ConfigInspector::new(before).switching_to(after));
            assert_eq!(result, expected, "{before:?} {after:?}");
            let keep = |records: &[(u8, u64, Vec<Word>)]| {
                let records = records[range.clone()].iter();
                records.filter(|record| kept.contains(record.0)).cloned().collect::<Vec<_>>()
            };
            assert_eq!(inspector.steps, keep(&full.steps), "{before:?} {after:?}");
            assert_eq!(inspector.step_ends, keep(&full.step_ends), "{before:?} {after:?}");
        }
        // The first step after the CALL sees its success flag on the stack.
        let (_, inspector, _) =
            run(ConfigInspector::new(OpcodeSet::EMPTY).switching_to(OpcodeSet::ALL));
        assert_eq!(inspector.steps[0], (op::GAS, full.steps[call + 1].1, vec![Word::ONE]));
        (expected, *full)
    }

    #[test]
    fn child_message_can_change_the_parent_steps() {
        let mut code = call_code(Address::repeat_byte(0x66));
        code.extend([op::CALL, op::GAS, op::POP, op::STOP]);
        let (uninspected, _) = run_evm_uninspected(InMemoryDB::default(), code, 100_000);
        let (result, _) = assert_steps_follow_the_child(None);
        assert_eq!(result, uninspected);
    }

    #[test]
    fn answered_call_can_change_the_parent_steps() {
        let answer = MessageResultExt {
            stop: InstrStop::Return,
            output: Bytes::from_static(&[0xaa]),
            ..Default::default()
        };
        let (_, full) = assert_steps_follow_the_child(Some(answer));
        // The hook answers the call, so every step belongs to the caller.
        assert_eq!(full.calls, 2);
    }

    #[test]
    fn code_after_call_with_runs_when_the_steps_change() {
        let (expected, full) = run_custom_call::<CallThenPushConfig>(
            ConfigInspector::new(OpcodeSet::ALL),
            CUSTOM_CALL,
            &[],
        );
        let gas = full.steps.iter().find(|record| record.0 == op::GAS).unwrap();
        // The instruction pushes a word after `call_with`, below the one that `GAS` pushes.
        assert_eq!(gas.2, [Word::ONE, Word::from(0x77)]);

        let (result, inspector) = run_custom_call::<CallThenPushConfig>(
            ConfigInspector::new(OpcodeSet::EMPTY).switching_to(opcode_set([op::GAS])),
            CUSTOM_CALL,
            &[],
        );
        assert_eq!(result, expected);
        assert_eq!(inspector.steps, core::slice::from_ref(gas));
    }

    #[test]
    fn inspector_gas_edits_survive_dispatch() {
        #[derive(Default)]
        struct GasInspector {
            steps: usize,
            ends: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let mut gas = interp.gas();
                gas.set_remaining(100);
                gas.set_refund(7);
                interp.set_gas(gas);
            }

            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let expected = [100, 128, 156, 184, 212, 236][self.steps];
                assert_eq!(interp.gas().remaining(), expected);
                assert_eq!(interp.gas().refunded(), 7);
                interp.gas_mut().set_remaining(expected + 10);
                self.steps += 1;
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let expected = [108, 136, 164, 192, 216, 246][self.ends];
                assert_eq!(interp.gas().remaining(), expected);
                if self.ends == 1 {
                    assert_eq!(interp.stack().last(), Some(&Word::from(136)));
                }
                interp.gas_mut().set_remaining(expected + 20);
                self.ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH0, op::GAS, op::POP, op::PUSH0, op::MSTORE, op::STOP]),
            &MessageExt::default(),
            1000,
            GasInspector::default(),
        );
        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(result.gas.remaining(), 266);
        assert_eq!(result.gas.refunded(), 7);
        assert_eq!((inspector.steps, inspector.ends), (6, 6));
    }

    #[test]
    fn inspector_message_hooks_preserve_parent_gas_edits() {
        #[derive(Default)]
        struct GasInspector {
            opcode: u8,
            before: u64,
            calls: usize,
            ends: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                self.opcode = interp.opcode();
                self.before = interp.gas().remaining();
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if matches!(self.opcode, op::CALL | op::CREATE) {
                    assert_eq!(interp.gas().remaining(), 4100);
                } else if self.opcode == op::GAS {
                    assert_eq!(interp.stack().last(), Some(&Word::from(4098)));
                }
            }

            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    return None;
                }
                let expected = if self.opcode == op::CALL {
                    self.before - 2600 - message.gas_limit
                } else {
                    self.before - 32000 - message.gas_limit
                };
                assert_eq!(interp.gas().remaining(), expected);
                interp.gas_mut().set_remaining(5000);
                self.calls += 1;
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(100),
                    created_address: Some(Address::repeat_byte(0x77)),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                _result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.depth > 0 {
                    assert_eq!(interp.gas().remaining(), 5000);
                    interp.gas_mut().set_remaining(4000);
                    self.ends += 1;
                }
            }

            fn create(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                self.call(interp, message)
            }

            fn create_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                self.call_end(interp, message, result);
            }
        }

        for (mut code, opcode) in
            [(call_code(Address::repeat_byte(0x66)), op::CALL), (create_code(), op::CREATE)]
        {
            code.extend([opcode, op::GAS, op::STOP]);
            let (result, inspector, _) = run_evm_with_inspector(
                code,
                &MessageExt::default(),
                100_000,
                GasInspector::default(),
            );
            assert_eq!(result.stop, InstrStop::Stop);
            assert_eq!(result.gas.remaining(), 4098);
            assert_eq!((inspector.calls, inspector.ends), (1, 1));
        }
    }

    #[test]
    fn inspector_gas_is_valid_after_out_of_gas() {
        struct GasInspector {
            recover: bool,
            failures: usize,
        }

        impl Inspector<BaseEvmTypes> for GasInspector {
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if matches!(interp.result(), Err(InstrStop::OutOfGas | InstrStop::MemoryOOG)) {
                    assert_eq!(interp.gas().remaining(), 0);
                    self.failures += 1;
                    if self.recover {
                        interp.gas_mut().set_remaining(17);
                        interp.set_stop(InstrStop::Stop);
                    }
                }
            }
        }

        for (code, gas_limit, stop) in [
            (vec![op::PUSH0], 1, InstrStop::OutOfGas),
            (vec![op::PUSH0, op::PUSH0, op::MSTORE], 9, InstrStop::MemoryOOG),
            (vec![op::PUSH0, op::SLOAD], 2, InstrStop::OutOfGas),
        ] {
            for recover in [false, true] {
                let (result, inspector, _) = run_evm_with_inspector(
                    code.clone(),
                    &MessageExt::default(),
                    gas_limit,
                    GasInspector { recover, failures: 0 },
                );
                assert_eq!(inspector.failures, 1, "code={code:?}, result={result:?}");
                assert_eq!(result.stop, if recover { InstrStop::Stop } else { stop });
                assert_eq!(result.gas.remaining(), if recover { 17 } else { 0 });
            }
        }
    }

    #[test]
    fn step_can_stop_before_current_opcode_executes() {
        #[derive(Default)]
        struct StopOnStepInspector {
            opcode: u8,
            steps: usize,
            step_ends: usize,
            stack: Vec<Word>,
        }

        impl<T: EvmTypesHost> Inspector<T> for StopOnStepInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
                if interp.opcode() == self.opcode {
                    self.stack = interp.stack().to_vec();
                    interp.gas_mut().set_remaining(123);
                    interp.set_stop(InstrStop::Revert);
                }
            }

            fn step_end(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &MessageExt::default(),
            10_000,
            StopOnStepInspector { opcode: op::ADD, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::ONE, Word::from(2)]);
        assert_eq!(inspector.steps, 3);
        assert_eq!(inspector.step_ends, 2);
        assert_eq!(result.gas.remaining(), 123);
    }

    #[test]
    fn step_end_can_stop_before_next_opcode_executes() {
        #[derive(Default)]
        struct StopOnStepEndInspector {
            opcode: u8,
            last_opcode: Option<u8>,
            steps: usize,
            step_ends: usize,
            stack: Vec<Word>,
        }

        impl<T: EvmTypesHost> Inspector<T> for StopOnStepEndInspector {
            fn step(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
                self.last_opcode = Some(interp.opcode());
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                self.step_ends += 1;
                if self.last_opcode == Some(self.opcode) {
                    self.stack = interp.stack().to_vec();
                    interp.gas_mut().set_remaining(123);
                    interp.set_stop(InstrStop::Revert);
                }
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::PUSH1, 1, op::PUSH1, 2, op::ADD, op::STOP]),
            &MessageExt::default(),
            10_000,
            StopOnStepEndInspector { opcode: op::PUSH1, ..Default::default() },
        );

        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(inspector.stack, [Word::ONE]);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
        assert_eq!(result.gas.remaining(), 123);
    }

    #[test]
    fn call_too_deep_is_inspected_without_executing() {
        let target = Address::repeat_byte(0x22);
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        for initial_call_depth in [0, 7] {
            let mut version = Version::new(SpecId::OSAKA);
            version.initial_call_depth = initial_call_depth;
            let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
                ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
                SpecId::OSAKA,
                BlockEnvExt::default(),
                TxRegistry::new(),
                InMemoryDB::default(),
                Precompiles::base(SpecId::OSAKA),
            );
            evm.set_inspector(HookInspector::default());
            let depth = initial_call_depth + CALL_DEPTH_LIMIT;
            let mut message = MessageExt {
                depth,
                gas_limit: 50_000,
                code: legacy_bytecode(code.clone()),
                ..Default::default()
            };
            let result = Host::execute_message(&mut evm, &mut message).unwrap();
            let inspector = evm.clear_inspector_as::<HookInspector>().unwrap();
            assert_matches!(result.stop, InstrStop::Stop);
            assert_eq!(inspector.call_depths, [depth, depth + 1]);
            assert_eq!(inspector.call_end_stops, [InstrStop::CallTooDeep, InstrStop::Stop]);
        }
    }

    #[test]
    fn call_inspector_override_skips_execution_and_still_calls_end() {
        let target = Address::repeat_byte(0x22);
        let inspector = OverrideCallInspector {
            result: MessageResultExt {
                stop: InstrStop::Return,
                output: Bytes::from_static(&[0xaa, 0xbb, 0xcc]),
                ..Default::default()
            },
            min_depth: 1,
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL, op::POP, op::RETURNDATASIZE]);
        return_top_word(&mut code);

        let (result, inspector, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, inspector);

        assert_matches!(result.stop, InstrStop::Return);
        // The override output is observed by the parent frame's RETURNDATASIZE.
        assert_eq!(Word::from_be_slice(&result.output), Word::from(3));
        assert_eq!(inspector.call_depth, Some(1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn call_inspector_override_wins_at_max_depth() {
        let target = Address::repeat_byte(0x22);
        let inspector = OverrideCallInspector {
            result: MessageResultExt { stop: InstrStop::Return, ..Default::default() },
            min_depth: CALL_DEPTH_LIMIT + 1,
            call_depth: None,
            call_end_stop: None,
        };
        let mut code = call_code(target);
        code.extend([op::CALL]);
        return_top_word(&mut code);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            inspector,
        );

        assert_matches!(result.stop, InstrStop::Return);
        // The override wins over the call depth check: the call succeeds.
        assert_eq!(Word::from_be_slice(&result.output), Word::ONE);
        assert_eq!(inspector.call_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.call_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn call_inspector_can_mutate_message_before_execution() {
        struct MutateCallInspector {
            destination: Address,
        }

        impl Inspector<BaseEvmTypes> for MutateCallInspector {
            fn call(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth > 0 {
                    message.destination = self.destination;
                    message.code_address = self.destination;
                }
                None
            }
        }

        let target = Address::repeat_byte(0x22);
        let replacement = Address::repeat_byte(0x33);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &Address::ZERO,
            AccountInfo::default().with_balance(U256::from(100)),
        );
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(7),
                address_to_word(&target),
                Word::from(50_000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            db,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(MutateCallInspector { destination: replacement });
        let bytecode = legacy_bytecode(code);
        let mut message = MessageExt { gas_limit: 100_000, code: bytecode, ..Default::default() };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_matches!(result.stop, InstrStop::Stop);
        // The redirected call transferred the value to the replacement, not the target.
        let mut balance = |address| {
            evm.state_mut()
                .account_info_untracked(&address)
                .unwrap()
                .map_or(U256::ZERO, |info| info.balance)
        };
        assert_eq!(balance(replacement), U256::from(7));
        assert_eq!(balance(target), U256::ZERO);
    }

    #[test]
    fn call_end_can_mutate_result_before_caller_observes_it() {
        struct CallEndInspector;

        impl Inspector<BaseEvmTypes> for CallEndInspector {
            fn call(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    return None;
                }
                Some(MessageResultExt {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.depth > 0 {
                    result.stop = InstrStop::Return;
                    result.output = Bytes::from_static(&[0xaa, 0xbb]);
                }
            }
        }

        let target = Address::repeat_byte(0x22);
        let mut code = call_code(target);
        code.extend([op::CALL, op::POP, op::RETURNDATASIZE]);
        return_top_word(&mut code);

        let (result, _, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, CallEndInspector);

        assert_matches!(result.stop, InstrStop::Return);
        // `call_end` upgraded the override from a revert to a 2-byte return.
        assert_eq!(Word::from_be_slice(&result.output), Word::from(2));
    }

    /// An account that returns its call data in [`echo_db`].
    const ECHO: Address = Address::repeat_byte(0x22);

    /// Returns a database with an account at [`ECHO`] that returns its call data.
    fn echo_db() -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &ECHO,
            AccountInfo::default().with_code(legacy_bytecode(vec![
                op::CALLDATASIZE,
                op::PUSH0,
                op::PUSH0,
                op::CALLDATACOPY,
                op::CALLDATASIZE,
                op::PUSH0,
                op::RETURN,
            ])),
        );
        db
    }

    /// Calls `target` through `call_op` with memory 0..32 as input and returns its 32-byte output.
    fn memory_input_call_code(target: Address, call_op: u8) -> Vec<u8> {
        let mut code = Vec::new();
        push_all(&mut code, [Word::from_be_bytes([0xaa; 32]), Word::ZERO]);
        code.push(op::MSTORE);
        push_all(
            &mut code,
            [
                Word::from(32),
                Word::from(32),
                Word::from(32),
                Word::ZERO,
                Word::ZERO,
                address_to_word(&target),
                Word::from(50_000),
            ],
        );
        code.extend([call_op, op::POP]);
        push_all(&mut code, [Word::from(32), Word::from(32)]);
        code.push(op::RETURN);
        code
    }

    #[test]
    fn call_hook_overwriting_caller_memory_keeps_child_input() {
        struct OverwriteCallerMemory;

        impl Inspector<BaseEvmTypes> for OverwriteCallerMemory {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth > 0 {
                    interp.memory_mut().set(0, &[0xbb; 32]);
                }
                None
            }
        }

        for target in [ECHO, Address::with_last_byte(4)] {
            let (result, _, _) = run_evm_with_inspector_db(
                echo_db(),
                memory_input_call_code(target, op::CALL),
                &MessageExt::default(),
                100_000,
                OverwriteCallerMemory,
            );

            assert_matches!(result.stop, InstrStop::Return);
            assert_eq!(result.output[..], [0xaa; 32], "{target}");
        }
    }

    #[test]
    fn call_with_hook_overwriting_caller_memory_keeps_child_input() {
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_config::<OverwritingCallConfig>(),
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            echo_db(),
            Precompiles::base(SpecId::OSAKA),
        );
        let code = memory_input_call_code(ECHO, CUSTOM_CALL);
        let mut message =
            MessageExt { gas_limit: 100_000, code: legacy_bytecode(code), ..Default::default() };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(result.output[..], [0xaa; 32]);
    }

    #[test]
    fn create_too_deep_is_inspected_without_executing() {
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_depths, [CALL_DEPTH_LIMIT + 1]);
        assert_eq!(inspector.create_end_stops, [InstrStop::CallTooDeep]);
    }

    #[test]
    fn create_inspector_override_skips_execution_and_still_calls_end() {
        let created = Address::repeat_byte(0x77);
        let inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, inspector, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, inspector);

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
        assert_eq!(inspector.create_depth, Some(1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn create_inspector_sees_derived_destination() {
        let contract = Address::repeat_byte(0x11);
        let expected = contract.create(0);
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { destination: contract, ..Default::default() },
            50_000,
            HookInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.create_destinations, [expected]);
    }

    #[test]
    fn amsterdam_create_preaccess_failure_does_not_fire_create_hook() {
        let contract = Address::repeat_byte(0x11);
        for is_create2 in [false, true] {
            for (balance, nonce, value, depth) in [
                (Word::ONE, 0, Word::from(2), 0),
                (Word::MAX, u64::MAX, Word::ZERO, 0),
                (Word::MAX, 0, Word::ZERO, CALL_DEPTH_LIMIT),
            ] {
                let mut code = Vec::new();
                if is_create2 {
                    push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO, value]);
                } else {
                    push_all(&mut code, [Word::ZERO, Word::ZERO, value]);
                }
                code.push(if is_create2 { op::CREATE2 } else { op::CREATE });
                return_top_word(&mut code);

                let mut db = InMemoryDB::default();
                db.insert_account_info(
                    &contract,
                    AccountInfo { balance, nonce, ..Default::default() },
                );
                let (result, inspector, _) = run_evm_with_inspector_db_spec(
                    SpecId::AMSTERDAM,
                    db,
                    code,
                    &MessageExt { destination: contract, depth, ..Default::default() },
                    100_000,
                    HookInspector::default(),
                );

                assert_matches!(result.stop, InstrStop::Return);
                assert_eq!(Word::from_be_slice(&result.output), Word::ZERO);
                assert!(inspector.create_depths.is_empty());
                assert!(inspector.create_end_stops.is_empty());
            }
        }
    }

    #[test]
    fn create_inspector_override_wins_at_max_depth() {
        let created = Address::repeat_byte(0x77);
        let inspector =
            OverrideCreateInspector { created, create_depth: None, create_end_stop: None };
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, inspector, _) = run_evm_with_inspector(
            code,
            &MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() },
            50_000,
            inspector,
        );

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
        assert_eq!(inspector.create_depth, Some(CALL_DEPTH_LIMIT + 1));
        assert_eq!(inspector.create_end_stop, Some(InstrStop::Return));
    }

    #[test]
    fn create_end_can_mutate_result_before_caller_observes_it() {
        struct CreateEndInspector {
            created: Address,
        }

        impl Inspector<BaseEvmTypes> for CreateEndInspector {
            fn create(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                Some(MessageResultExt {
                    stop: InstrStop::Revert,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn create_end(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                result.stop = InstrStop::Return;
                result.created_address = Some(self.created);
            }
        }

        let created = Address::repeat_byte(0x88);
        let mut code = create_code();
        code.extend([op::CREATE]);
        return_top_word(&mut code);

        let (result, _, _) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            50_000,
            CreateEndInspector { created },
        );

        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), address_to_word(&created));
    }

    #[test]
    fn log_opcode_is_inspected_and_emitted_to_host() {
        let contract = Address::repeat_byte(0x11);
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (result, inspector, evm) = run_evm_with_inspector(
            code,
            &MessageExt { destination: contract, ..Default::default() },
            10_000,
            LogInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::Stop);
        assert_eq!(inspector.logs.len(), 1);
        assert_eq!(inspector.logs[0].address, contract);
        assert_eq!(evm.logs(), inspector.logs);
    }

    #[test]
    fn log_opcode_oog_is_not_inspected_or_emitted_to_host() {
        let code = Vec::from([op::PUSH1, 0, op::PUSH1, 0, op::LOG0, op::STOP]);

        let (result, inspector, evm) =
            run_evm_with_inspector(code, &MessageExt::default(), 6, LogInspector::default());

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(inspector.logs.is_empty());
        assert!(evm.logs().is_empty());
    }

    #[test]
    fn step_end_runs_for_failing_opcode_with_result_set() {
        #[derive(Default)]
        struct FailingStepInspector {
            steps: usize,
            step_ends: usize,
        }

        impl<T: EvmTypesHost> Inspector<T> for FailingStepInspector {
            fn step(&mut self, _interp: &mut Interpreter<'_, '_, T>) {
                self.steps += 1;
            }

            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, T>) {
                let _ = interp;
                self.step_ends += 1;
            }
        }

        let (result, inspector, _) = run_evm_with_inspector(
            Vec::from([op::INVALID]),
            &MessageExt::default(),
            10_000,
            FailingStepInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::InvalidFEOpcode);
        assert_eq!(inspector.steps, 1);
        assert_eq!(inspector.step_ends, 1);
    }

    #[test]
    fn selfdestruct_is_inspected_from_opcode() {
        let contract = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x99);
        let value = Word::from(0xbeef);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&contract, AccountInfo::default().with_balance(value));
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt { destination: contract, ..Default::default() },
            50_000,
            SelfdestructInspector::default(),
        );

        assert_matches!(result.stop, InstrStop::SelfDestruct);
        assert_eq!(inspector.selfdestruct, Some((contract, target, value)));
    }

    #[test]
    fn selfdestruct_dynamic_gas_oog_is_not_inspected() {
        let contract = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x99);
        let mut db = InMemoryDB::default();
        db.insert_account_info(&contract, AccountInfo::default());
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt { destination: contract, ..Default::default() },
            7_000,
            SelfdestructInspector::default(),
        );

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(inspector.selfdestruct, None);
    }

    #[test]
    fn selfdestruct_host_error_is_not_inspected() {
        // Host failures are injected through the mock host; this intentionally uses [`TestHost`].
        let target = Address::repeat_byte(0x99);
        let mut host = TestHost {
            selfdestruct_error: Some(
                DatabaseError::new(crate::AnyError::from("selfdestruct failed"), true).into(),
            ),
            ..Default::default()
        };
        let mut inspector = SelfdestructInspector::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let message = Message::<TestTypes> {
            gas_limit: 10_000,
            code: legacy_bytecode(code),
            ..Default::default()
        };
        let mut interp = Interpreter::<TestTypes>::new(&message);
        let stop = interp.run_inspect(&mut host, &mut inspector);

        assert_matches!(stop, Err(crate::ExecutionError::Database(error)) if error.is_fatal());
        assert_eq!(inspector.selfdestruct, None);
    }

    #[test]
    fn evm_transaction_inspects_interpreter_steps_and_logs() {
        let caller = Address::repeat_byte(0xaa);
        let contract = Address::repeat_byte(0xbb);
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::LOG0,
            op::STOP,
        ]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(contract),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.is_success());
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 4);
        assert_eq!(state.step_ends, 4);
        assert_eq!(state.logs.len(), 1);
        assert_eq!(state.logs[0].address, contract);
        assert_eq!(state.calls, 1);
        assert_eq!(state.creates, 0);
    }

    #[test]
    fn evm_transaction_inspects_eip7708_transfer_log() {
        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0xbb);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                value: U256::from(7),
                gas_limit: 300_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").detach();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.result.is_success());
        assert_eq!(result.result.logs.len(), 1);
        assert_eq!(state.logs, result.result.logs);
        assert_eq!(state.logs[0].address, SYSTEM_ADDRESS);
    }

    #[test]
    fn evm_create_transaction_initializes_interpreter_with_create_hook() {
        let caller = Address::repeat_byte(0xaa);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SharedE2eInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Create,
                input: Bytes::from_static(&[op::STOP]),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        let inspector = evm.inspector().unwrap().downcast_ref::<SharedE2eInspector>().unwrap();
        let state = &inspector.state;

        assert!(result.is_success());
        assert_eq!(state.initialized, 1);
        assert_eq!(state.steps, 1);
        assert_eq!(state.step_ends, 1);
        assert_eq!(state.calls, 0);
        assert_eq!(state.creates, 1);
    }

    #[test]
    fn frame_checkpoint_precedes_value_transfer_and_creation() {
        // Records, at a call from the running frame, the running frame's checkpoint and the
        // caller's balance and target's existence once a copy of the state rolls back to it.
        #[derive(Default)]
        struct CheckpointInspector {
            observed: Option<(bool, Option<U256>, bool)>,
        }
        impl Inspector<BaseEvmTypes> for CheckpointInspector {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.destination != Address::with_last_byte(0x44) {
                    return None;
                }
                let target = interp.message().destination;
                let caller = interp.message().caller;
                let evm = interp.host();
                let checkpoint = evm.frame_checkpoint().cloned().unwrap();
                let before_hook = checkpoint != evm.state().checkpoint();
                let features = evm.version().features;
                let mut state = evm.state().clone();
                state.rollback(checkpoint, features);
                let caller_balance =
                    state.account_info_untracked(&caller).unwrap().map(|a| a.balance);
                let target_exists = state.account_info_untracked(&target).unwrap().is_some();
                self.observed = Some((before_hook, caller_balance, target_exists));
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }
        }

        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0xbb);
        // CALL(gas, 0x44, 0, 0, 0, 0, 0), then STOP.
        let code = vec![
            op::PUSH0,
            op::PUSH0,
            op::PUSH0,
            op::PUSH0,
            op::PUSH0,
            op::PUSH1,
            0x44,
            op::PUSH2,
            0xff,
            0xff,
            op::CALL,
            op::STOP,
        ];
        for to in [TxKind::Call(target), TxKind::Create] {
            let mut database = InMemoryDB::default();
            database.insert_account_info(
                &caller,
                AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
            );
            database.insert_account_info(
                &target,
                AccountInfo::default().with_code(legacy_bytecode(code.clone())),
            );
            let mut evm = Evm::<BaseEvmTypes>::new(
                SpecId::OSAKA,
                BlockEnvExt::default(),
                ethereum_tx_registry(SpecId::OSAKA),
                database,
                Precompiles::base(SpecId::OSAKA),
            );
            evm.set_inspector(CheckpointInspector::default());
            let tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    to,
                    value: U256::from(5),
                    input: if to.is_create() { code.clone().into() } else { Bytes::new() },
                    gas_limit: 100_000,
                    ..Default::default()
                }),
                caller,
            );

            let result = evm.transact(&tx).expect("transaction should execute").discard();
            assert!(result.is_success());
            let inspector = evm.inspector().unwrap().downcast_ref::<CheckpointInspector>().unwrap();
            let (before_hook, caller_balance, target_exists) = inspector.observed.unwrap();
            // The value transfer, and for a create the account creation, follow the checkpoint.
            assert!(before_hook);
            assert_eq!(caller_balance, Some(U256::from(1_000_000_000_u64)));
            assert_eq!(target_exists, to.is_call());
        }
    }

    #[test]
    fn nested_create_frame_checkpoint_follows_nonce_bump_and_restores_parent() {
        // Records the top-level frame's checkpoint, the creator's nonce and the created account's
        // existence once a copy of the state rolls back to the create frame's checkpoint, and
        // whether the top-level frame's checkpoint is restored after the create frame.
        #[derive(Default)]
        struct CheckpointInspector {
            parent: Option<StateCheckpoint>,
            child: Option<(u64, bool)>,
            restored: Option<bool>,
        }
        impl Inspector<BaseEvmTypes> for CheckpointInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let depth = interp.message().depth;
                let creator = interp.message().caller;
                let created = interp.message().destination;
                let evm = interp.host();
                let checkpoint = evm.frame_checkpoint().cloned().unwrap();
                if depth == 0 {
                    self.parent = Some(checkpoint);
                    return;
                }
                let features = evm.version().features;
                let mut state = evm.state().clone();
                state.rollback(checkpoint, features);
                let nonce = state.account_info_untracked(&creator).unwrap().unwrap().nonce;
                let created_exists = state.account_info_untracked(&created).unwrap().is_some();
                self.child = Some((nonce, created_exists));
            }

            fn create_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &Message<BaseEvmTypes>,
                _result: &mut MessageResult<BaseEvmTypes>,
            ) {
                self.restored = Some(interp.host().frame_checkpoint() == self.parent.as_ref());
            }
        }

        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0xbb);
        // MSTORE8(0, 0), CREATE(0, 0, 1) with a one byte STOP initcode, then STOP.
        let code = vec![
            op::PUSH0,
            op::PUSH0,
            op::MSTORE8,
            op::PUSH1,
            1,
            op::PUSH0,
            op::PUSH0,
            op::CREATE,
            op::STOP,
        ];
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        database.insert_account_info(
            &target,
            AccountInfo::default().with_nonce(1).with_code(legacy_bytecode(code)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(CheckpointInspector::default());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                gas_limit: 200_000,
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        assert!(result.is_success());
        let inspector = evm.inspector().unwrap().downcast_ref::<CheckpointInspector>().unwrap();
        // The create frame's checkpoint follows the creator's nonce bump and precedes the
        // account creation.
        assert_eq!(inspector.child, Some((2, false)));
        // The top-level frame's checkpoint is restored once the create frame returns.
        assert_eq!(inspector.restored, Some(true));
    }

    #[test]
    fn runtime_oog_first_frame_fires_call_hooks() {
        #[derive(Default)]
        struct HaltedFrameInspector {
            calls: Vec<(u16, Address, u64)>,
            ends: Vec<(InstrStop, u64)>,
        }
        impl Inspector<BaseEvmTypes> for HaltedFrameInspector {
            fn call(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                assert!(message.code.is_empty());
                self.calls.push((message.depth, message.destination, message.gas_limit));
                // A returned result must not replace the halt.
                Some(MessageResultExt::default())
            }

            fn call_end(
                &mut self,
                _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                self.ends.push((result.stop, result.gas.spent()));
            }
        }

        let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let authorization = alloy_eips::eip7702::Authorization {
            chain_id: U256::from(1),
            address: Address::ZERO,
            nonce: 0,
        };
        let (signature, recovery_id) =
            key.sign_prehash_recoverable(authorization.signature_hash().as_slice());
        let authorization = alloy_eips::eip7702::SignedAuthorization::new_unchecked(
            authorization,
            recovery_id.to_byte(),
            U256::from_be_slice(&signature.r().to_bytes()),
            U256::from_be_slice(&signature.s().to_bytes()),
        );
        let caller = Address::from([0xaa; 20]);
        let target = Address::from([0xbb; 20]);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::AMSTERDAM),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        evm.set_inspector(HaltedFrameInspector::default());
        // The gas covers the intrinsic cost but not the new authority's state gas.
        let tx = Recovered::new_unchecked(
            TxEnvelope::from(alloy_consensus::TxEip7702 {
                chain_id: 1,
                gas_limit: 60_000,
                to: target,
                authorization_list: vec![authorization],
                ..Default::default()
            }),
            caller,
        );

        let result = evm.transact(&tx).expect("transaction should execute").discard();
        assert!(!result.status);
        assert_eq!(result.stop, InstrStop::OutOfGas);
        let inspector = evm.inspector().unwrap().downcast_ref::<HaltedFrameInspector>().unwrap();
        let [(0, destination, gas_limit)] = inspector.calls[..] else {
            panic!("expected one depth-0 call hook, got {:?}", inspector.calls)
        };
        assert_eq!(destination, target);
        // The halt burns the whole budget the frame was given.
        assert_eq!(inspector.ends, [(InstrStop::OutOfGas, gas_limit)]);
    }

    #[test]
    fn cooling_loaded_slot_respects_access_list() {
        struct CoolSlotInspector {
            prewarmed: bool,
            before: Option<u64>,
            costs: Vec<u64>,
        }
        impl Inspector<BaseEvmTypes> for CoolSlotInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let address = interp.message().destination;
                let state = interp.host().state_mut();
                state.storage_slot(&address, Word::ZERO).unwrap().set(Word::from(7));
                if self.prewarmed {
                    state.prewarm_storage_slot(&address, Word::ZERO);
                }
            }
            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if interp.opcode() == op::SLOAD {
                    if self.costs.len() == 1 {
                        let address = interp.message().destination;
                        interp.host().state_mut().set_storage_warm(&address, Word::ZERO, false);
                    }
                    self.before = Some(interp.gas().remaining());
                }
            }
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if let Some(before) = self.before.take() {
                    self.costs.push(before - interp.gas().remaining());
                }
            }
        }
        for prewarmed in [false, true] {
            let mut code = vec![
                op::PUSH0,
                op::SLOAD,
                op::POP,
                op::PUSH0,
                op::SLOAD,
                op::POP,
                op::PUSH0,
                op::SLOAD,
            ];
            return_top_word(&mut code);
            let (result, inspector, _) = run_evm_with_inspector(
                code,
                &MessageExt::default(),
                100_000,
                CoolSlotInspector { prewarmed, before: None, costs: Vec::new() },
            );
            assert_eq!(result.stop, InstrStop::Return);
            assert_eq!(Word::from_be_slice(&result.output), Word::from(7));
            assert_eq!(
                inspector.costs,
                [if prewarmed { 100 } else { 2100 }, if prewarmed { 100 } else { 2100 }, 100]
            );
        }
    }

    /// Reproduces Foundry restoring a snapshot from a still-running child call.
    #[test]
    fn restored_snapshot_allows_child_and_parent_revert() {
        #[derive(Default)]
        struct SnapshotInspector {
            snapshot: Option<State<'static>>,
            restored: bool,
            child_reverted: bool,
        }

        impl Inspector<BaseEvmTypes> for SnapshotInspector {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                let marker = message.destination;
                if marker == Address::with_last_byte(0x44) {
                    self.snapshot = Some(interp.host().state().clone_with(EmptyDB::default()));
                } else if marker == Address::with_last_byte(0x55) {
                    *interp.host().state_mut() = self.snapshot.as_ref().unwrap().clone();
                    self.restored = true;
                } else {
                    return None;
                }
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }

            fn call_end(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                if message.destination == Address::with_last_byte(0xbb) {
                    assert_eq!(result.stop, InstrStop::Revert);
                    assert_eq!(
                        interp
                            .host()
                            .state_mut()
                            .storage_slot_untracked(&Address::with_last_byte(0xaa), &Word::ZERO,)
                            .unwrap(),
                        Word::from(7),
                    );
                    self.child_reverted = true;
                }
            }
        }

        fn append_call(code: &mut Vec<u8>, target: Address) {
            push_all(
                code,
                [
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    Word::ZERO,
                    address_to_word(&target),
                    Word::from(200_000),
                ],
            );
            code.extend([op::CALL, op::POP]);
        }

        let parent = Address::with_last_byte(0xaa);
        let child = Address::with_last_byte(0xbb);
        let mut parent_code = vec![op::PUSH1, 7, op::PUSH0, op::SSTORE];
        append_call(&mut parent_code, Address::with_last_byte(0x44));
        parent_code.extend([op::PUSH1, 8, op::PUSH0, op::SSTORE]);
        append_call(&mut parent_code, child);
        parent_code.extend([op::PUSH0, op::PUSH0, op::REVERT]);
        let mut child_code = Vec::new();
        append_call(&mut child_code, Address::with_last_byte(0x55));
        child_code.extend([op::PUSH0, op::PUSH0, op::REVERT]);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &child,
            AccountInfo::default().with_code(legacy_bytecode(child_code)),
        );
        let (result, inspector, mut evm) = run_evm_with_inspector_db(
            db,
            parent_code,
            &MessageExt { destination: parent, ..Default::default() },
            1_000_000,
            SnapshotInspector::default(),
        );
        assert!(inspector.restored);
        assert!(inspector.child_reverted);
        assert_eq!(result.stop, InstrStop::Revert);
        assert_eq!(
            evm.state_mut().storage_slot_untracked(&parent, &Word::ZERO).unwrap(),
            Word::ZERO,
        );
    }

    #[test]
    fn snapshot_restore_preserves_logs_without_reinspection() {
        #[derive(Default)]
        struct SnapshotInspector {
            snapshot: Option<State<'static>>,
            logs: usize,
        }
        impl Inspector<BaseEvmTypes> for SnapshotInspector {
            fn log(&mut self, _log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
                self.logs += 1;
            }
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                match message.destination.as_slice()[19] {
                    0x44 => {
                        self.snapshot = Some(interp.host().state().clone_with(EmptyDB::default()))
                    }
                    0x55 => {
                        let state = interp.host().state_mut();
                        let logs = core::mem::take(state.logs_mut());
                        let db = core::mem::replace(
                            &mut state.overlay_db_mut().db,
                            Box::new(EmptyDB::default()),
                        );
                        *state = self.snapshot.as_ref().unwrap().clone_with(db);
                        *state.logs_mut() = logs;
                    }
                    _ => return None,
                }
                Some(MessageResultExt {
                    stop: InstrStop::Return,
                    gas: GasTracker::new(message.gas_limit),
                    ..Default::default()
                })
            }
        }
        let mut code = Vec::new();
        for marker in [0x44, 0x55] {
            code.extend([
                op::PUSH1,
                marker,
                op::PUSH0,
                op::MSTORE8,
                op::PUSH1,
                1,
                op::PUSH0,
                op::LOG0,
            ]);
            code.extend(call_code(Address::with_last_byte(marker)));
            code.extend([op::CALL, op::POP]);
            if marker == 0x44 {
                code.extend([op::PUSH1, 9, op::PUSH0, op::SSTORE]);
            }
        }
        code.push(op::STOP);
        let (result, inspector, mut evm) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            100_000,
            SnapshotInspector::default(),
        );
        assert_eq!(result.stop, InstrStop::Stop);
        assert_eq!(evm.logs().len(), 2);
        assert_eq!(evm.logs()[0].data.data.as_ref(), &[0x44]);
        assert_eq!(evm.logs()[1].data.data.as_ref(), &[0x55]);
        assert_eq!(inspector.logs, 2);
        assert_eq!(
            evm.state_mut().storage_slot_untracked(&Address::ZERO, &Word::ZERO).unwrap(),
            Word::ZERO
        );
    }

    #[test]
    fn inspector_gas_changes_survive_dispatch_errors() {
        struct GasEdit;
        impl Inspector<TestTypes> for GasEdit {
            fn step_end(&mut self, interp: &mut Interpreter<'_, '_, TestTypes>) {
                interp.gas_mut().set_remaining(1000);
            }
        }

        for (code, gas_limit, expected) in [
            (Vec::from([op::PUSH1, 0]), 2, InstrStop::OutOfGas),
            (Vec::from([op::INVALID]), 10_000, InstrStop::InvalidFEOpcode),
        ] {
            let message = Message::<TestTypes> {
                gas_limit,
                code: legacy_bytecode(code),
                ..Default::default()
            };
            let mut interp = Interpreter::<TestTypes>::new(&message);
            let stop = interp.run_inspect(&mut TestHost::default(), &mut GasEdit);

            assert_eq!(stop, Ok(expected));
            assert_eq!(interp.gas().remaining(), 1000);
        }
    }

    #[test]
    fn tuple_inspector_forwards_hooks_and_prefers_first_override() {
        let target = Address::repeat_byte(0x22);
        let mut code = call_code(target);
        code.extend([op::CALL, op::POP, op::RETURNDATASIZE]);
        return_top_word(&mut code);
        let override_inspector = |output: &'static [u8]| OverrideCallInspector {
            result: MessageResultExt {
                stop: InstrStop::Return,
                output: Bytes::from_static(output),
                ..Default::default()
            },
            min_depth: 1,
            call_depth: None,
            call_end_stop: None,
        };

        let (result, inspector, _) = run_evm_with_inspector(
            code.clone(),
            &MessageExt::default(),
            50_000,
            (HookInspector::default(), override_inspector(&[0xaa, 0xbb, 0xcc])),
        );
        assert_matches!(result.stop, InstrStop::Return);
        assert_eq!(Word::from_be_slice(&result.output), Word::from(3));
        assert_eq!(inspector.0.call_depths, [0, 1]);
        assert_eq!(inspector.0.call_end_stops, [InstrStop::Return, InstrStop::Return]);
        assert_eq!(inspector.1.call_depth, Some(1));
        assert_eq!(inspector.1.call_end_stop, Some(InstrStop::Return));

        let (result, ..) = run_evm_with_inspector(
            code,
            &MessageExt::default(),
            50_000,
            (override_inspector(&[0xaa]), override_inspector(&[0xaa, 0xbb, 0xcc])),
        );
        assert_eq!(Word::from_be_slice(&result.output), Word::ONE);
    }

    #[test]
    fn option_inspector_forwards_hooks_only_when_present() {
        let target = Address::repeat_byte(0x22);
        let mut code = call_code(target);
        code.extend([op::CALL, op::STOP]);

        let (_, inspector, _) = run_evm_with_inspector(
            code.clone(),
            &MessageExt::default(),
            50_000,
            Some(HookInspector::default()),
        );
        let inspector = inspector.unwrap();
        assert_eq!(inspector.call_depths, [0, 1]);
        assert_eq!(inspector.call_end_stops, [InstrStop::Stop, InstrStop::Stop]);

        let (result, inspector, _) =
            run_evm_with_inspector(code, &MessageExt::default(), 50_000, None::<HookInspector>);
        assert_matches!(result.stop, InstrStop::Stop);
        assert!(inspector.is_none());
    }

    #[test]
    fn tuple_and_option_inspectors_step_the_union_of_their_sets() {
        let (db, code) = config_test_code();
        let (expected, expected_logs) = run_evm_uninspected(db.clone(), code.clone(), 1_000_000);
        let config = |inspector: &dyn Inspector<BaseEvmTypes>| inspector.inspector_config().steps();
        let stepped = |inspector: &ConfigInspector| {
            let mut opcodes = inspector.steps.iter().map(|step| step.0).collect::<Vec<_>>();
            opcodes.sort_unstable();
            opcodes.dedup();
            opcodes
        };
        let sload = || ConfigInspector::new(opcode_set([op::SLOAD]));
        let call = || ConfigInspector::new(opcode_set([op::CALL]));

        assert_eq!(config(&(sload(), call())), opcode_set([op::SLOAD, op::CALL]));
        assert_eq!(
            config(&(sload(), ConfigInspector::new(OpcodeSet::EMPTY))),
            opcode_set([op::SLOAD])
        );
        assert_eq!(config(&(sload(), NoopInspector::default())), opcode_set([op::SLOAD]));
        assert_eq!(config(&(sload(), HookInspector::default())), OpcodeSet::ALL);
        assert_eq!(config(&Some(sload())), opcode_set([op::SLOAD]));
        assert_eq!(config(&None::<ConfigInspector>), OpcodeSet::EMPTY);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db.clone(),
            code.clone(),
            &MessageExt::default(),
            1_000_000,
            (sload(), call()),
        );
        assert_eq!(result, expected);
        assert_eq!(inspector.0.logs, expected_logs);
        // Each member also sees the steps of the other, and both see every message.
        assert_eq!(stepped(&inspector.0), [op::SLOAD, op::CALL]);
        assert_eq!(inspector.0.steps, inspector.1.steps);
        assert_eq!((inspector.0.calls, inspector.1.calls), (2, 2));

        let (result, inspector, _) = run_evm_with_inspector_db(
            db.clone(),
            code.clone(),
            &MessageExt::default(),
            1_000_000,
            Some(sload()),
        );
        assert_eq!(result, expected);
        assert_eq!(stepped(&inspector.unwrap()), [op::SLOAD]);

        let (result, inspector, _) = run_evm_with_inspector_db(
            db,
            code,
            &MessageExt::default(),
            1_000_000,
            None::<ConfigInspector>,
        );
        assert_eq!(result, expected);
        assert!(inspector.is_none());
    }
}
