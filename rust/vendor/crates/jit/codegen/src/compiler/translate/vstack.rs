//! Section-local virtual stack that tracks EVM stack values as SSA values.
//!
//! Instead of immediately storing/loading to the stack alloca, values are kept
//! in a logical cache. Physical stores are deferred until a "materialization"
//! point (builtin call, section boundary, branch, etc.).

use core::ops::Range;

/// State of a single virtual stack slot.
#[derive(Clone, Copy, Debug)]
pub(super) enum VSlot<T> {
    /// Physical memory already contains the canonical value.
    ///
    /// Holds the SSA value the translator last stored there or loaded from there, if no builtin
    /// may have written the slot since. It only identifies reloads of that value; codegen still
    /// reloads.
    Materialized(Option<T>),
    /// The canonical value exists only as an SSA value; memory may be stale.
    Virtual(T),
}

/// A section-local virtual stack indexed by section-relative offsets.
///
/// Offsets are relative to `section_start_sp`:
/// - Negative offsets (`-inputs .. 0`) represent values that existed on the stack before the
///   section started (the section's inputs).
/// - Non-negative offsets represent values pushed within the section.
#[derive(Clone, Debug)]
pub(super) struct VStack<T> {
    /// Lowest tracked offset (inclusive), relative to `section_start_sp`.
    base_offset: i32,
    /// One-past-top offset, relative to `section_start_sp`.
    /// Equivalent to `section_len_offset + len_offset` in `FunctionCx`.
    top_offset: i32,
    /// Slot states, indexed by `(offset - base_offset)`.
    slots: Vec<Option<VSlot<T>>>,
}

impl<T> Default for VStack<T> {
    fn default() -> Self {
        Self { base_offset: 0, top_offset: 0, slots: Vec::new() }
    }
}

impl<T: Copy> VStack<T> {
    /// Resets the virtual stack for a new section.
    ///
    /// `inputs` is the number of stack values consumed by the section (the section's
    /// minimum required stack depth). `max_growth` is the maximum number of slots the
    /// section pushes beyond the entry height.
    ///
    /// All input slots start as [`VSlot::Materialized`] because they were written by
    /// the previous section (or the function entry) and are already in memory.
    pub(super) fn reset(&mut self, inputs: usize, max_growth: usize) {
        self.base_offset = 0;
        self.top_offset = 0;
        self.slots.clear();
        self.extend(-(inputs as i32), max_growth as i32);
    }

    /// Extends the tracked range to cover `low..high` for a section that continues the
    /// current one.
    ///
    /// New slots below the range are inputs that no earlier part of the section touched, so
    /// they are already in memory.
    pub(super) fn extend(&mut self, low: i32, high: i32) {
        if low < self.base_offset {
            let n = (self.base_offset - low) as usize;
            self.slots.splice(0..0, core::iter::repeat_n(Some(VSlot::Materialized(None)), n));
            self.base_offset = low;
        }
        let end = self.base_offset + self.slots.len() as i32;
        if high > end {
            self.slots.resize(self.slots.len() + (high - end) as usize, None);
        }
    }

    /// Returns the current top offset (one-past-top, relative to section start).
    pub(super) const fn top_offset(&self) -> i32 {
        self.top_offset
    }

    /// Returns the section-relative offset for a given operand depth
    /// (0 = TOS, 1 = second from top, etc.).
    pub(super) const fn offset_at_depth(&self, depth: usize) -> i32 {
        self.top_offset - 1 - depth as i32
    }

    fn idx(&self, offset: i32) -> usize {
        let idx = offset - self.base_offset;
        debug_assert!(
            idx >= 0 && (idx as usize) < self.slots.len(),
            "VStack: offset {offset} out of range [{}..{}), base={}, top={}, cap={}",
            self.base_offset,
            self.base_offset + self.slots.len() as i32,
            self.base_offset,
            self.top_offset,
            self.slots.len(),
        );
        idx as usize
    }

    /// Returns the slot state at the given operand depth.
    pub(super) fn get(&self, depth: usize) -> VSlot<T> {
        let idx = self.idx(self.offset_at_depth(depth));
        self.slots[idx].unwrap()
    }

    /// Returns the slot state at the given section-relative offset.
    /// Returns `VSlot::Materialized` if the offset is outside the tracked range.
    pub(super) fn get_at_offset(&self, offset: i32) -> VSlot<T> {
        let idx = offset - self.base_offset;
        if idx < 0 || idx as usize >= self.slots.len() {
            return VSlot::Materialized(None);
        }
        self.slots[idx as usize].unwrap_or(VSlot::Materialized(None))
    }

    /// Sets a slot to virtual at the given operand depth.
    pub(super) fn set(&mut self, depth: usize, value: T) {
        self.set_slot(self.offset_at_depth(depth), VSlot::Virtual(value));
    }

    /// Sets the slot at the given section-relative offset.
    fn set_slot(&mut self, offset: i32, slot: VSlot<T>) {
        let idx = self.idx(offset);
        self.slots[idx] = Some(slot);
    }

    /// Records that memory at a section-relative offset holds `value`.
    pub(super) fn set_stored_at_offset(&mut self, offset: i32, value: T) {
        self.set_slot(offset, VSlot::Materialized(Some(value)));
    }

    /// Pushes a virtual value onto the top of the stack.
    pub(super) fn push(&mut self, value: T) {
        self.set_slot(self.top_offset, VSlot::Virtual(value));
        self.top_offset += 1;
    }

    /// Pushes a materialized marker onto the top of the stack.
    /// Used when a builtin writes directly to the physical stack slot.
    pub(super) fn push_mem(&mut self) {
        self.set_slot(self.top_offset, VSlot::Materialized(None));
        self.top_offset += 1;
    }

    /// Drops `n` elements from the top of the stack.
    pub(super) fn drop_top(&mut self, n: usize) {
        self.top_offset -= n as i32;
        debug_assert!(
            self.top_offset >= self.base_offset,
            "VStack::drop_top({n}): top={} dropped below base={}",
            self.top_offset,
            self.base_offset,
        );
    }

    /// Returns the live range of offsets (base_offset..top_offset).
    pub(super) const fn live_range(&self) -> Range<i32> {
        self.base_offset..self.top_offset
    }

    /// Returns the number of virtual (non-materialized) slots in the live range.
    pub(super) fn virtual_count(&self) -> usize {
        self.pending_stores(self.live_range()).count()
    }

    /// Returns pending virtual slots that need physical stores in the given range,
    /// yielding `(offset, value)` pairs.
    ///
    /// Offsets outside the tracked range are silently skipped (they are already materialized
    /// by the previous section or not yet live).
    pub(super) fn pending_stores(&self, range: Range<i32>) -> impl Iterator<Item = (i32, T)> + '_ {
        range.filter_map(|off| {
            let idx = off - self.base_offset;
            if idx < 0 || idx as usize >= self.slots.len() {
                return None;
            }
            match self.slots.get(idx as usize)?.as_ref()? {
                VSlot::Virtual(v) => Some((off, *v)),
                VSlot::Materialized(_) => None,
            }
        })
    }

    /// Marks all slots in the given range as materialized with unknown contents.
    ///
    /// Offsets outside the tracked range are silently skipped.
    pub(super) fn mark_materialized_range(&mut self, range: Range<i32>) {
        self.map_range(range, |_| VSlot::Materialized(None));
    }

    /// Marks all slots in the given range as materialized after their pending values have been
    /// stored.
    ///
    /// Offsets outside the tracked range are silently skipped.
    pub(super) fn mark_stored_range(&mut self, range: Range<i32>) {
        self.map_range(range, |slot| match slot {
            VSlot::Virtual(value) => VSlot::Materialized(Some(value)),
            slot => slot,
        });
    }

    /// Forgets the stored values of all slots from the given offset up, for when the physical
    /// stack there may have been overwritten.
    pub(super) fn forget_stored_from(&mut self, offset: i32) {
        self.map_range(offset..self.base_offset + self.slots.len() as i32, |slot| match slot {
            VSlot::Materialized(_) => VSlot::Materialized(None),
            slot => slot,
        });
    }

    fn map_range(&mut self, range: Range<i32>, f: impl Fn(VSlot<T>) -> VSlot<T>) {
        for off in range {
            let idx = off - self.base_offset;
            if idx < 0 || idx as usize >= self.slots.len() {
                continue;
            }
            if let Some(slot) = &mut self.slots[idx as usize] {
                *slot = f(*slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_basic() {
        let mut vs = VStack::<i32>::default();
        // Section with 0 inputs, max_growth 4.
        vs.reset(0, 4);
        assert_eq!(vs.top_offset(), 0);

        vs.push(10);
        vs.push(20);
        assert_eq!(vs.top_offset(), 2);

        // Depth 0 = TOS = 20, depth 1 = 10.
        match vs.get(0) {
            VSlot::Virtual(v) => assert_eq!(v, 20),
            _ => panic!("expected virtual"),
        }
        match vs.get(1) {
            VSlot::Virtual(v) => assert_eq!(v, 10),
            _ => panic!("expected virtual"),
        }

        vs.drop_top(1);
        assert_eq!(vs.top_offset(), 1);
        match vs.get(0) {
            VSlot::Virtual(v) => assert_eq!(v, 10),
            _ => panic!("expected virtual"),
        }
    }

    #[test]
    fn section_inputs_materialized() {
        let mut vs = VStack::<i32>::default();
        // Section with 2 inputs, max_growth 2.
        vs.reset(2, 2);
        assert_eq!(vs.top_offset(), 0);
        assert_eq!(vs.live_range(), -2..0);

        // Input slots (depth 1 and 2 from top of pre-existing stack) are materialized.
        // depth 0 is at offset -1, depth 1 is at offset -2.
        // But top_offset = 0, so get(depth) requires depth < 2 (2 elements exist: -2, -1).
        // Wait — these are below top, so we access them as "from TOS".
        // With top=0, depth 0 = offset -1, depth 1 = offset -2.
        // But top is the one-past-end; the values are at -2 and -1.
        // We can't use get() because top=0 and nothing is "on" the stack from the
        // virtual stack's perspective (inputs are below the section start).
        // Instead, verify via pending_stores.
        let pending: Vec<_> = vs.pending_stores(-2..0).collect();
        assert!(pending.is_empty(), "inputs should be materialized");
    }

    #[test]
    fn set_swap() {
        let mut vs = VStack::<i32>::default();
        vs.reset(0, 4);
        vs.push(10);
        vs.push(20);
        vs.push(30);

        // Simulate swap: read both, then set.
        let a = match vs.get(0) {
            VSlot::Virtual(v) => v,
            _ => panic!("expected virtual"),
        };
        let b = match vs.get(2) {
            VSlot::Virtual(v) => v,
            _ => panic!("expected virtual"),
        };
        vs.set(0, b);
        vs.set(2, a);

        match vs.get(0) {
            VSlot::Virtual(v) => assert_eq!(v, 10),
            _ => panic!("expected virtual"),
        }
        match vs.get(2) {
            VSlot::Virtual(v) => assert_eq!(v, 30),
            _ => panic!("expected virtual"),
        }
    }

    #[test]
    fn materialize_range() {
        let mut vs = VStack::<i32>::default();
        vs.reset(0, 4);
        vs.push(10);
        vs.push(20);
        vs.push(30);

        let pending: Vec<_> = vs.pending_stores(0..3).collect();
        assert_eq!(pending.len(), 3);

        vs.mark_materialized_range(0..3);
        let pending: Vec<_> = vs.pending_stores(0..3).collect();
        assert_eq!(pending.len(), 0);
    }
}
