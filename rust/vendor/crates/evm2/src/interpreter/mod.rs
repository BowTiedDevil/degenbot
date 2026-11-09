//! EVM interpreter.

pub mod gas;
pub use gas::{Gas, GasTracker, MemoryGas};

#[macro_use]
mod utils;

/// Base EVM instruction implementations.
///
/// This module is not part of the stable API and has no stability guarantees.
#[doc(hidden)]
pub mod instructions;
pub use instructions::i256;

pub(crate) mod dispatch;

#[doc(hidden)] // For macro only. Not public API.
pub mod private;

pub mod opcode;
pub use opcode::op;

mod ctrl;
pub use ctrl::{BytecodeRef, Pc};

mod stack;
pub(crate) use stack::{RawStack, StackBacking};
pub use stack::{Stack, StackMut, StackRef, Word};

mod memory;
pub use memory::Memory;

mod message;
pub use message::{
    Message, MessageExt, MessageInput, MessageKind, create_preaccess_failure,
    derive_create_destination,
};

mod host;
pub use host::{Host, MessageResult, MessageResultExt};

mod runtime;
pub(crate) use runtime::InterpreterPool;
pub use runtime::{Interpreter, InterpreterState};

/// Instruction result type.
pub type Result<T = (), E = InstrStop> = core::result::Result<T, E>;

/// Result of executing an EVM instruction.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum InstrStop {
    /// Encountered a `STOP` opcode
    #[default]
    Stop = 1, // Start at 1 so that `Result<(), _>::Ok(())` is 0.
    /// Return from the current call.
    Return,
    /// Self-destruct the current contract.
    SelfDestruct,

    // Revert Codes
    /// Revert the transaction.
    Revert = 0x10,
    /// Exceeded maximum call depth.
    CallTooDeep,
    /// Insufficient funds for transfer.
    OutOfFunds,
    /// Nonce overflow.
    NonceOverflow,

    // Halt Codes
    /// Out of gas error.
    OutOfGas = 0x20,
    /// Out of gas error encountered during memory expansion.
    MemoryOOG,
    /// The memory limit of the EVM has been exceeded.
    MemoryLimitOOG,
    /// Out of gas error encountered during the execution of a precompiled contract.
    PrecompileOOG,
    /// Out of gas error encountered while calling an invalid operand.
    InvalidOperandOOG,
    /// Out of gas error encountered while checking for reentrancy sentry.
    ReentrancySentryOOG,
    /// Invalid `CALL` with value transfer in static context.
    CallNotAllowedInsideStatic,
    /// Invalid state modification in static call.
    StateChangeDuringStaticCall,
    /// Undefined opcode.
    OpcodeNotFound,
    /// Encountered the designated invalid opcode (`0xFE`).
    InvalidFEOpcode,
    /// Invalid jump destination. Dynamic jumps points to invalid not jumpdest opcode.
    InvalidJump,
    /// The feature or opcode is not activated in this version of the EVM.
    NotActivated,
    /// Attempting to pop a value from an empty stack.
    StackUnderflow,
    /// Attempting to push a value onto a full stack.
    StackOverflow,
    /// Invalid memory or storage offset.
    OutOfOffset,
    /// Address collision during contract creation.
    CreateCollision,
    /// Error in precompiled contract execution.
    PrecompileError,
    /// Exceeded contract size limit during creation.
    CreateContractSizeLimit,
    /// Created contract starts with invalid bytes (`0xEF`).
    CreateContractStartingWithEF,
    /// Exceeded init code size limit (EIP-3860:  Limit and meter initcode).
    CreateInitCodeSizeLimit,
    /// Fatal external error. Returned by database.
    FatalExternalError,
    /// Invalid encoding of an instruction's immediate operand.
    InvalidImmediateEncoding,
    /// The inspector's config changed while a `hooked` instruction ran. The dispatch exits with
    /// this stop after the instruction, and the frame continues on the table for the new config. No
    /// instruction returns it, and a frame never reports it.
    #[doc(hidden)]
    Reload,
}

impl InstrStop {
    /// Returns whether execution completed successfully.
    #[doc(alias = "is_ok")]
    #[inline]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Stop | Self::Return | Self::SelfDestruct)
    }

    /// Returns whether execution ended with the `REVERT` opcode.
    #[inline]
    pub const fn is_revert(self) -> bool {
        matches!(self, Self::Revert)
    }

    /// Returns whether the frame failed without an exceptional halt, so its unused gas returns to
    /// the caller.
    ///
    /// This is the `REVERT` opcode and the failures of a call or create before its frame ran:
    /// [`Self::CallTooDeep`], [`Self::OutOfFunds`] and [`Self::NonceOverflow`].
    #[inline]
    pub const fn is_frame_revert(self) -> bool {
        matches!(self, Self::Revert | Self::CallTooDeep | Self::OutOfFunds | Self::NonceOverflow)
    }

    /// Returns whether execution halted exceptionally.
    #[inline]
    pub const fn is_halt(self) -> bool {
        !self.is_success() && !self.is_frame_revert()
    }

    /// Returns whether execution ran out of gas or exceeded the memory limit.
    #[inline]
    pub const fn is_out_of_gas(self) -> bool {
        matches!(
            self,
            Self::OutOfGas
                | Self::MemoryOOG
                | Self::MemoryLimitOOG
                | Self::PrecompileOOG
                | Self::InvalidOperandOOG
                | Self::ReentrancySentryOOG
        )
    }

    /// Returns whether execution hit a fatal host/extension boundary error.
    #[inline]
    pub const fn is_fatal(self) -> bool {
        matches!(self, Self::FatalExternalError)
    }
}
