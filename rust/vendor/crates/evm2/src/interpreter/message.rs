use crate::{
    BaseEvmTypes, EvmTypesHost,
    bytecode::Bytecode,
    constants::CALL_DEPTH_LIMIT,
    interpreter::{InstrStop, Memory, Word},
};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use core::ops::Range;

/// EVM message kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessageKind {
    /// Regular `CALL` message.
    #[default]
    Call,
    /// `DELEGATECALL` message.
    DelegateCall,
    /// `CALLCODE` message.
    CallCode,
    /// `CREATE` message.
    Create,
    /// `CREATE2` message.
    Create2,
    /// `STATICCALL` message.
    StaticCall,
}

impl MessageKind {
    /// Returns `true` if the message is CREATE or CREATE2.
    #[inline]
    pub const fn is_create(&self) -> bool {
        matches!(self, Self::Create | Self::Create2)
    }
}

/// Frame-local EVM call/create message executed by the interpreter for an EVM type family.
pub type Message<T = BaseEvmTypes> = MessageExt<<T as EvmTypesHost>::MessageExt>;

/// Frame-local EVM call/create message, parameterized by extension data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageExt<E = ()> {
    /// Message kind.
    pub kind: MessageKind,
    /// Inspector-visible call depth, starting at
    /// [`Host::initial_call_depth`](crate::interpreter::Host::initial_call_depth).
    pub depth: u16,
    /// Gas available to this message.
    pub gas_limit: u64,
    /// EIP-8037 state-gas reservoir inherited from the parent frame.
    ///
    /// The reservoir is a shared pool threaded down into child frames and
    /// reconciled back on return by
    /// [`GasTracker::merge_child_gas`](crate::interpreter::GasTracker::merge_child_gas).
    /// Zero for non-Amsterdam execution.
    pub reservoir: u64,
    /// The account this message targets.
    ///
    /// Its meaning depends on [`MessageExt::kind`]: for call-family messages it is the account
    /// whose context is executed; for `CREATE`/`CREATE2` it is the address of the
    /// yet-to-be-created contract, derived when the message is constructed (from the creator
    /// and nonce, or from the salt and init-code hash).
    pub destination: Address,
    /// Address requested by the call opcode or transaction, before EIP-7702 code resolution.
    ///
    /// Unlike `destination`, this remains the requested callee for `CALLCODE` and
    /// `DELEGATECALL`. Native precompile dispatch uses this address before delegated bytecode.
    /// For create messages it is the derived contract address.
    pub call_target: Address,
    /// Immediate caller.
    pub caller: Address,
    /// Call input data, or initcode for create messages.
    pub input: MessageInput,
    /// Value transferred with the message.
    pub value: U256,
    /// Bytecode resolved for this frame: the code at [`MessageExt::code_address`] (the resolved
    /// delegate's code for an EIP-7702 delegated call), or the initcode for create messages.
    ///
    /// Resolved by the message's producer when it is constructed, so frames never load accounts
    /// for code. Native precompile dispatch at [`MessageExt::call_target`] takes precedence.
    pub code: Bytecode,
    /// Address from which bytecode was resolved. This can differ from `destination` for
    /// `CALLCODE`, `DELEGATECALL`, and EIP-7702 delegated-code execution.
    pub code_address: Address,
    /// Whether the immediate caller frame is executing in a static context. Combined with a
    /// `STATICCALL` kind, this determines whether the new frame is static.
    pub caller_is_static: bool,
    /// CREATE2 salt. Ignored for other message kinds.
    pub salt: B256,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> MessageExt<E> {
    /// Replaces the extension, preserving all other message fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> MessageExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other message fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> MessageExt<F> {
        let Self {
            kind,
            depth,
            gas_limit,
            reservoir,
            destination,
            call_target,
            caller,
            input,
            value,
            code,
            code_address,
            caller_is_static,
            salt,
            ext,
            _non_exhaustive,
        } = self;
        MessageExt {
            kind,
            depth,
            gas_limit,
            reservoir,
            destination,
            call_target,
            caller,
            input,
            value,
            code,
            code_address,
            caller_is_static,
            salt,
            ext: f(ext),
            _non_exhaustive,
        }
    }

    /// Returns `true` if the frame executing this message is static: the caller is static or the
    /// message is a `STATICCALL`.
    #[inline]
    pub fn is_static(&self) -> bool {
        self.caller_is_static || self.kind == MessageKind::StaticCall
    }

    /// Returns `true` if the account whose context the message runs in is the requested
    /// [`call_target`](Self::call_target), i.e. it is not a `DELEGATECALL` or `CALLCODE` into
    /// another account.
    ///
    /// This compares addresses, so a `DELEGATECALL` or `CALLCODE` to the executing account's own
    /// address also counts as direct, and so does an EIP-7702 delegated `CALL`. Create messages
    /// are always direct. Native precompiles can use this to reject delegated calls.
    #[inline]
    pub fn is_direct_call(&self) -> bool {
        self.destination == self.call_target
    }
}

/// Derives the contract address a create message deploys to, i.e. its
/// [`destination`](MessageExt::destination).
///
/// `nonce` is the creator's (pre-bump) nonce and is only used by the `CREATE` scheme; `CREATE2`
/// derives the address from `salt` and the initcode hash instead.
#[inline]
pub fn derive_create_destination(
    kind: MessageKind,
    caller: &Address,
    salt: &B256,
    init_code: &[u8],
    nonce: u64,
) -> Address {
    match kind {
        MessageKind::Create2 => caller.create2(salt, keccak256(init_code)),
        _ => caller.create(nonce),
    }
}

/// Returns the reason a create message fails before its destination is accessed, if any.
///
/// `balance` and `nonce` are the creator's, and are zero for an absent account. Failures are
/// checked in order: call depth (more than [`CALL_DEPTH_LIMIT`] below `initial_call_depth`),
/// insufficient balance for `value`, then, for a nested create (`depth` above
/// `initial_call_depth`), a creator nonce at `u64::MAX` (EIP-2681). The transaction itself bumps
/// the nonce of a top-level create, so that one skips the nonce check.
#[inline]
pub fn create_preaccess_failure(
    balance: Word,
    nonce: u64,
    value: Word,
    depth: u16,
    initial_call_depth: u16,
) -> Option<InstrStop> {
    if depth.saturating_sub(initial_call_depth) > CALL_DEPTH_LIMIT {
        Some(InstrStop::CallTooDeep)
    } else if balance < value {
        Some(InstrStop::OutOfFunds)
    } else if depth > initial_call_depth && nonce == u64::MAX {
        Some(InstrStop::NonceOverflow)
    } else {
        None
    }
}

/// The input of a message: call data, or initcode for create messages.
///
/// The input of a call instruction's message is a range of the caller frame's memory instead of a
/// copy, since the caller waits for the call to return. The child frame reads it from there, and
/// the inspector hooks that hold the message read it with [`as_bytes`](Self::as_bytes) and the
/// caller's memory, which is the `interp.memory()` of the hook, and a precompile reads it with
/// [`Evm::message_input`](crate::Evm::message_input). To change an input, copy it out and assign
/// an owned one, as in `message.input = bytes.into()`. A clone of such an input is only the range,
/// and does not record whose memory it names: copy the bytes out with
/// [`to_bytes`](Self::to_bytes) to keep or forward them.
///
/// A running frame reads its own input with
/// [`Interpreter::input`](crate::interpreter::Interpreter::input), and only [`Evm`](crate::Evm)
/// resolves a borrowed input when it enters a frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageInput(Repr);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Repr {
    /// The input's bytes.
    Owned(Bytes),
    /// The input's range in the memory of the caller frame with the entry number `frame`.
    Borrowed { range: Range<usize>, frame: u64 },
}

impl Default for Repr {
    #[inline]
    fn default() -> Self {
        Self::Owned(Bytes::new())
    }
}

impl MessageInput {
    /// Creates an input that is `range` of the memory of the caller frame with the entry number
    /// `frame`.
    #[inline]
    pub(crate) const fn borrowed(range: Range<usize>, frame: u64) -> Self {
        Self(Repr::Borrowed { range, frame })
    }

    /// Returns the range and the caller frame's entry number of a borrowed input.
    #[inline]
    pub(crate) const fn borrowed_range(&self) -> Option<(&Range<usize>, u64)> {
        match &self.0 {
            Repr::Owned(_) => None,
            Repr::Borrowed { range, frame } => Some((range, *frame)),
        }
    }

    /// Returns the length of the input.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Owned(bytes) => bytes.len(),
            Repr::Borrowed { range, .. } => range.len(),
        }
    }

    /// Returns whether the input is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the bytes of an input that owns them, and `None` for a range of the caller's
    /// memory.
    #[inline]
    pub const fn as_owned(&self) -> Option<&Bytes> {
        match &self.0 {
            Repr::Owned(bytes) => Some(bytes),
            Repr::Borrowed { .. } => None,
        }
    }

    /// Returns the input bytes, reading them from `memory`, the caller frame's memory, unless the
    /// input owns them.
    ///
    /// # Panics
    ///
    /// Panics if the input's range is out of bounds of `memory`.
    #[inline]
    pub fn as_bytes<'a>(&'a self, memory: &'a Memory) -> &'a [u8] {
        match &self.0 {
            Repr::Owned(bytes) => bytes,
            Repr::Borrowed { range, .. } => &memory.as_slice()[range.clone()],
        }
    }

    /// Returns the input as owned bytes, copying them from `memory`, the caller frame's memory,
    /// unless the input owns them.
    ///
    /// # Panics
    ///
    /// Panics if the input's range is out of bounds of `memory`.
    #[inline]
    pub fn to_bytes(&self, memory: &Memory) -> Bytes {
        match &self.0 {
            Repr::Owned(bytes) => bytes.clone(),
            Repr::Borrowed { .. } => Bytes::copy_from_slice(self.as_bytes(memory)),
        }
    }
}

impl From<Bytes> for MessageInput {
    #[inline]
    fn from(bytes: Bytes) -> Self {
        Self(Repr::Owned(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_preaccess_failure_order_and_nesting() {
        let initial_call_depth = 5;
        let max = initial_call_depth + CALL_DEPTH_LIMIT;
        let check = |balance: u64, nonce: u64, value: u64, depth: u16| {
            create_preaccess_failure(
                Word::from(balance),
                nonce,
                Word::from(value),
                depth,
                initial_call_depth,
            )
        };

        assert_eq!(check(0, u64::MAX, 1, max + 1), Some(InstrStop::CallTooDeep));
        assert_eq!(check(0, u64::MAX, 1, max), Some(InstrStop::OutOfFunds));
        assert_eq!(check(1, u64::MAX, 1, max), Some(InstrStop::NonceOverflow));
        assert_eq!(check(1, u64::MAX, 1, initial_call_depth), None);
        assert_eq!(check(0, 0, 0, max), None);

        // A host-supplied initial depth near `u16::MAX` must not overflow the limit.
        let check_at = |initial_call_depth: u16, depth: u16| {
            create_preaccess_failure(Word::ZERO, 0, Word::ZERO, depth, initial_call_depth)
        };
        assert_eq!(check_at(u16::MAX, u16::MAX), None);
        assert_eq!(check_at(u16::MAX - CALL_DEPTH_LIMIT, u16::MAX), None);
        assert_eq!(
            check_at(u16::MAX - CALL_DEPTH_LIMIT - 1, u16::MAX),
            Some(InstrStop::CallTooDeep)
        );
    }

    #[test]
    fn is_static_follows_caller_or_staticcall_kind() {
        let message = |kind, caller_is_static| MessageExt::<()> {
            kind,
            caller_is_static,
            ..MessageExt::default()
        };

        assert!(!message(MessageKind::Call, false).is_static());
        assert!(!message(MessageKind::DelegateCall, false).is_static());
        assert!(message(MessageKind::Call, true).is_static());
        assert!(message(MessageKind::DelegateCall, true).is_static());
        assert!(message(MessageKind::StaticCall, false).is_static());
        assert!(message(MessageKind::StaticCall, true).is_static());
    }

    #[test]
    fn is_direct_call_rejects_delegated_code() {
        let target = Address::with_last_byte(1);
        let executing = Address::with_last_byte(2);
        let caller = Address::with_last_byte(3);
        let call = MessageExt::<()> {
            kind: MessageKind::Call,
            destination: target,
            call_target: target,
            caller,
            ..MessageExt::default()
        };
        let delegated = |kind, destination| MessageExt { kind, destination, ..call.clone() };

        assert!(call.is_direct_call());
        assert!(delegated(MessageKind::StaticCall, target).is_direct_call());
        assert!(delegated(MessageKind::Create, target).is_direct_call());
        assert!(!delegated(MessageKind::DelegateCall, executing).is_direct_call());
        assert!(!delegated(MessageKind::CallCode, executing).is_direct_call());
        // Address-based: a delegate call to the executing account's own address is direct.
        assert!(delegated(MessageKind::DelegateCall, target).is_direct_call());
    }

    #[test]
    fn caller_memory_input() {
        let mut memory = Memory::new();
        memory.resize(0, 3).unwrap();
        memory.set(0, &[1, 2, 3]);
        let input = MessageInput::borrowed(1..3, 0);
        let owned = MessageInput::from(Bytes::from_static(&[9]));
        let kept = input.clone();
        memory.set(1, &[7]);

        assert_eq!((input.len(), owned.len()), (2, 1));
        assert_eq!(input.as_owned(), None);
        assert_eq!(owned.as_owned(), Some(&Bytes::from_static(&[9])));
        // A borrowed input, and a clone of it, read the caller's memory.
        assert_eq!(input.as_bytes(&memory), [7, 3]);
        assert_eq!(kept.as_bytes(&memory), [7, 3]);
        assert_eq!(input.to_bytes(&memory)[..], [7, 3]);
        assert_eq!(owned.as_bytes(&memory), [9]);
        assert_eq!(MessageInput::default().len(), 0);
    }

    #[test]
    #[should_panic = "range end index 3 out of range for slice of length 2"]
    fn caller_memory_input_out_of_bounds_panics() {
        let mut memory = Memory::new();
        memory.resize(0, 2).unwrap();

        MessageInput::borrowed(1..3, 0).as_bytes(&memory);
    }
}
