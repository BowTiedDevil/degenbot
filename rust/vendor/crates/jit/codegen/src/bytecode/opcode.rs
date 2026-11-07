use crate::{OpcodeInfo, op_info_map};
use evm2::{
    SpecId,
    interpreter::{op, opcode::OPCODE_INFO},
};
use std::{fmt, slice};

pub use evm2::interpreter::instructions::{decode_pair, decode_single, encode_pair, encode_single};

/// A bytecode iterator that yields opcodes and their immediate data, alongside the program counter.
///
/// Created by calling [`OpcodesIter::with_pc`].
#[derive(Debug)]
pub struct OpcodesIterWithPc<'a> {
    iter: OpcodesIter<'a>,
    pc: usize,
}

impl OpcodesIterWithPc<'_> {
    /// Rewinds the iterator by `n` bytes so that the next call to [`Iterator::next`] re-yields
    /// them as fresh opcodes.
    ///
    /// # Safety
    ///
    /// `n` must not exceed the number of bytes already consumed.
    pub(crate) unsafe fn rewind(&mut self, n: usize) {
        // SAFETY: the caller guarantees n ≤ bytes consumed, so `remaining` was originally
        // part of a larger contiguous slice that started at least `n` bytes earlier.
        unsafe { self.iter.rewind(n) };
        self.pc -= n;
    }
}

impl<'a> Iterator for OpcodesIterWithPc<'a> {
    type Item = (usize, Opcode<'a>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let len = self.iter.inner().len();
        self.iter.next().map(|opcode| {
            let pc = self.pc;
            // Count the bytes consumed, including a truncated immediate at the end of the code.
            self.pc += len - self.iter.inner().len();
            (pc, opcode)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl std::iter::FusedIterator for OpcodesIterWithPc<'_> {}

/// An iterator that yields opcodes and their immediate data.
///
/// If the bytecode is not well-formed, the iterator will still yield opcodes, but the immediate
/// data may be incorrect. For example, if the bytecode is `PUSH2 0x69`, the iterator will yield
/// `PUSH2, None`.
#[derive(Clone, Debug)]
pub struct OpcodesIter<'a> {
    iter: slice::Iter<'a, u8>,
    info: &'a [OpcodeInfo; 256],
}

impl fmt::Display for OpcodesIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, op) in self.clone().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{op}")?;
        }
        Ok(())
    }
}

impl<'a> OpcodesIter<'a> {
    /// Create a new iterator over the given bytecode slice.
    #[inline]
    pub fn new(slice: &'a [u8], spec_id: SpecId) -> Self {
        Self::with_infos(slice, op_info_map(spec_id))
    }

    /// Create a new iterator over the given bytecode slice with the given opcode infos.
    #[inline]
    pub(crate) fn with_infos(slice: &'a [u8], info: &'a [OpcodeInfo; 256]) -> Self {
        Self { iter: slice.iter(), info }
    }

    /// Returns a new iterator that also yields the program counter alongside the opcode and
    /// immediate data.
    #[inline]
    pub const fn with_pc(self) -> OpcodesIterWithPc<'a> {
        OpcodesIterWithPc { iter: self, pc: 0 }
    }

    /// Returns the inner iterator.
    #[inline]
    pub const fn inner(&self) -> &slice::Iter<'a, u8> {
        &self.iter
    }

    /// Returns the inner iterator.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut slice::Iter<'a, u8> {
        &mut self.iter
    }

    /// Returns the inner iterator.
    #[inline]
    pub const fn into_inner(self) -> slice::Iter<'a, u8> {
        self.iter
    }

    /// Rewinds the iterator by `n` bytes so that the next call to [`Iterator::next`] re-yields
    /// them as fresh opcodes.
    ///
    /// # Safety
    ///
    /// `n` must not exceed the number of bytes already consumed.
    pub(crate) unsafe fn rewind(&mut self, n: usize) {
        let inner = self.inner_mut();
        let remaining = inner.as_slice();
        // SAFETY: the caller guarantees n ≤ bytes consumed, so `remaining` was originally
        // part of a larger contiguous slice that started at least `n` bytes earlier.
        let rewound =
            unsafe { std::slice::from_raw_parts(remaining.as_ptr().sub(n), remaining.len() + n) };
        *inner = rewound.iter();
    }
}

impl<'a> Iterator for OpcodesIter<'a> {
    type Item = Opcode<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|&opcode| {
            let info = self.info[opcode as usize];
            if info.is_unknown() || info.is_disabled() || info.is_unsupported_host() {
                return Opcode { opcode, immediate: None };
            }

            let len = min_imm_len(opcode) as usize;
            let immediate = if len > 0 {
                let r = self.iter.as_slice().get(..len);
                // TODO: Use `advance_by` when stable.
                self.iter.by_ref().take(len).for_each(drop);
                r
            } else {
                None
            };
            Opcode { opcode, immediate }
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.iter.len();
        ((len != 0) as usize, Some(len))
    }
}

impl std::iter::FusedIterator for OpcodesIter<'_> {}

/// An opcode and its immediate data. Returned by [`OpcodesIter`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Opcode<'a> {
    /// The opcode.
    pub opcode: u8,
    /// The immediate data, if any.
    pub immediate: Option<&'a [u8]>,
}

impl fmt::Debug for Opcode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Opcode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match OPCODE_INFO[self.opcode as usize] {
            Some(s) => f.write_str(s.name()),
            None => write!(f, "UNKNOWN(0x{:02x})", self.opcode),
        }?;
        match self.immediate {
            Some(imm) => write!(f, " {}", alloy_primitives::hex::encode_prefixed(imm)),
            None => Ok(()),
        }
    }
}

/// Returns the length of the immediate data for the given opcode, or `0` if none.
#[inline]
pub const fn min_imm_len(op: u8) -> u8 {
    if let Some(info) = &OPCODE_INFO[op as usize] { info.immediate_size() } else { 0 }
}

/// Returns the number of input and output stack elements of the given opcode.
#[inline]
pub const fn stack_io(op: u8) -> (u8, u8) {
    if let Some(info) = &OPCODE_INFO[op as usize] {
        (info.inputs(), info.outputs())
    } else {
        (0, 0)
    }
}

/// Returns the real `(inputs, outputs)` stack I/O for an opcode, decoding the immediate for
/// `DUPN`, `SWAPN`, and `EXCHANGE` whose opcode-table entries are placeholders.
///
/// Returns `None` if the immediate of one of those opcodes is invalid.
pub(crate) fn compute_stack_io(op: u8, immediate: Option<&[u8]>) -> Option<(u8, u8)> {
    let imm_u8 = || immediate.map_or(0, |b| b[0]);
    match op {
        op::DUPN => decode_single(imm_u8()).map(|n| (n, n + 1)),
        op::SWAPN => decode_single(imm_u8()).map(|n| (n + 1, n + 1)),
        op::EXCHANGE => decode_pair(imm_u8()).map(|(_n, m)| (m + 1, m + 1)),
        _ => Some(stack_io(op)),
    }
}

/// Returns a string representation of the given bytecode.
pub fn format_bytecode(bytecode: &[u8], spec_id: SpecId) -> String {
    let mut w = String::new();
    format_bytecode_to(bytecode, spec_id, &mut w).unwrap();
    w
}

/// Formats an EVM bytecode to the given writer.
pub fn format_bytecode_to<W: fmt::Write + ?Sized>(
    bytecode: &[u8],
    spec_id: SpecId,
    w: &mut W,
) -> fmt::Result {
    write!(w, "{}", OpcodesIter::new(bytecode, spec_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEF_SPEC: SpecId = SpecId::LONDON;

    #[test]
    fn iter_basic() {
        let bytecode = [0x01, 0x02, 0x03, 0x04, 0x05];
        let mut iter = OpcodesIter::new(&bytecode, DEF_SPEC);

        assert_eq!(iter.next(), Some(Opcode { opcode: 0x01, immediate: None }));
        assert_eq!(iter.next(), Some(Opcode { opcode: 0x02, immediate: None }));
        assert_eq!(iter.next(), Some(Opcode { opcode: 0x03, immediate: None }));
        assert_eq!(iter.next(), Some(Opcode { opcode: 0x04, immediate: None }));
        assert_eq!(iter.next(), Some(Opcode { opcode: 0x05, immediate: None }));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn iter_with_imm() {
        let bytecode = [op::PUSH0, op::PUSH1, 0x69, op::PUSH2, 0x01, 0x02];
        let mut iter = OpcodesIter::new(&bytecode, DEF_SPEC);

        assert_eq!(iter.next(), Some(Opcode { opcode: op::PUSH0, immediate: None }));
        assert_eq!(iter.next(), Some(Opcode { opcode: op::PUSH1, immediate: Some(&[0x69]) }));
        assert_eq!(iter.next(), Some(Opcode { opcode: op::PUSH2, immediate: Some(&[0x01, 0x02]) }));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn iter_with_imm_too_short() {
        let bytecode = [op::PUSH2, 0x69];
        let mut iter = OpcodesIter::new(&bytecode, DEF_SPEC);

        assert_eq!(iter.next(), Some(Opcode { opcode: op::PUSH2, immediate: None }));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn display() {
        let bytecode = [op::PUSH0, op::PUSH1, 0x69, op::PUSH2, 0x01, 0x02];
        let s = format_bytecode(&bytecode, DEF_SPEC);
        assert_eq!(s, "PUSH0 PUSH1 0x69 PUSH2 0x0102");
    }
}
