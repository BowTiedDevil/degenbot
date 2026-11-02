//! `channel_lifecycle` — the one-way delivery channel's phase.
//!
//! One home for the question "where is this one-way channel?", which used to be
//! answered by `Option<Sender>` presence. An `Option` has **two** `None`s: never
//! attached, and closed. Those are different facts with different consequences —
//! a standalone consumer has nothing to end, while a closed channel's receiver
//! has already observed end-of-stream — and collapsing them is what let a
//! `set_channel`-after-`close` silently re-open a pipe whose receiver was
//! already gone.
//!
//! Here the phase **is** the sender: [`DeliveryChannel::Open`] carries it, so
//! [`DeliveryChannel::Detached`] and [`DeliveryChannel::Closed`] cannot collapse
//! into one value. [`SendOutcome`] names which way a send did not deliver,
//! instead of a `bool` that conflates three distinct failures.
//!
//! End-of-stream contract (owned by this module): after [`DeliveryChannel::close`]
//! the receiver observes a natural stream end **exactly once**; sends are
//! non-delivering after close and are reported as such.

use tokio::sync::mpsc;

/// Where a one-way channel is: the [`DeliveryChannel`] phase, as a cheap `Copy`
/// projection for readers that must not hold the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelPhase {
    /// No sender attached yet. Nothing can end: a standalone consumer never
    /// gets a receiver, so there is no stream to close.
    Detached,
    /// A sender is attached and delivering.
    Open,
    /// [`DeliveryChannel::close`] ran. The receiver has observed end-of-stream;
    /// this is absorbing and cannot return to [`Self::Open`].
    Closed,
}

/// Why a channel phase move was refused. Every arm is a caller error, never a
/// runtime condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChannelDecline {
    /// A sender is already attached. Re-attaching would strand the first
    /// consumer's stream — it would never observe end-of-stream.
    #[error("a channel is already attached; close it before attaching another")]
    AlreadyAttached,
    /// [`DeliveryChannel::attach`] after [`DeliveryChannel::close`]. The
    /// receiver already saw end-of-stream, so re-opening would deliver into a
    /// stream the consumer considers finished.
    #[error("the channel is closed and cannot be re-opened")]
    AttachedAfterClose,
}

/// What a [`DeliveryChannel::send`] did. Delivered is only one of four outcomes,
/// and a caller that needs to know *why* a batch did not go out gets the answer
/// here rather than inferring it from a `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendOutcome {
    /// The item was handed to the channel.
    Delivered,
    /// No sender is attached (a standalone consumer).
    NotAttached,
    /// [`DeliveryChannel::close`] already ran.
    Closed,
    /// The sender was attached but the receiver is gone. This latches the
    /// channel to [`ChannelPhase::Closed`]: the stream is over either way, and
    /// leaving it `Open` would re-report the same dead channel per send.
    ReceiverGone,
}

impl SendOutcome {
    /// Whether the item reached the channel.
    #[must_use]
    pub const fn delivered(self) -> bool {
        matches!(self, Self::Delivered)
    }
}

/// A one-way delivery channel whose phase carries its sender.
///
/// The payload lives in the variant so the phase cannot disagree with the
/// sender: there is no field to leave `None` for two different reasons. See the
/// module docs for the end-of-stream contract.
pub enum DeliveryChannel<T> {
    /// Never attached. Sends report [`SendOutcome::NotAttached`].
    Detached,
    /// Attached and delivering; the variant carries the sender.
    Open(mpsc::UnboundedSender<T>),
    /// Closed; absorbing.
    Closed,
}

impl<T> DeliveryChannel<T> {
    /// A channel that has never been attached.
    #[must_use]
    pub const fn detached() -> Self {
        Self::Detached
    }

    /// The [`ChannelPhase`] projection.
    #[must_use]
    pub const fn phase(&self) -> ChannelPhase {
        match self {
            Self::Detached => ChannelPhase::Detached,
            Self::Open(_) => ChannelPhase::Open,
            Self::Closed => ChannelPhase::Closed,
        }
    }

    /// Attach a sender — the **only** door in.
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`Self::close`].
    pub fn attach(&mut self, tx: mpsc::UnboundedSender<T>) -> Result<(), ChannelDecline> {
        match self {
            Self::Detached => {
                *self = Self::Open(tx);
                Ok(())
            }
            Self::Open(_) => Err(ChannelDecline::AlreadyAttached),
            Self::Closed => Err(ChannelDecline::AttachedAfterClose),
        }
    }

    /// Deliver one item.
    ///
    /// Takes `&mut self` so [`SendOutcome::ReceiverGone`] can latch the channel
    /// to [`ChannelPhase::Closed`]; a dead channel is terminal, not re-derivable
    /// per send.
    pub fn send(&mut self, item: T) -> SendOutcome {
        match self {
            Self::Detached => SendOutcome::NotAttached,
            Self::Closed => SendOutcome::Closed,
            Self::Open(tx) => {
                // Unbounded: `send` fails only when the receiver is gone.
                if tx.send(item).is_ok() {
                    SendOutcome::Delivered
                } else {
                    *self = Self::Closed;
                    SendOutcome::ReceiverGone
                }
            }
        }
    }

    /// Close the channel — the end-of-stream edge. Total and absorbing: it
    /// never declines and never moves out of [`ChannelPhase::Closed`]. Returns
    /// the phase for callers that assert on the edge; the answer is always
    /// [`ChannelPhase::Closed`], so it is not `#[must_use]`.
    pub fn close(&mut self) -> ChannelPhase {
        *self = Self::Closed;
        ChannelPhase::Closed
    }
}

// Hand-written: `#[derive(Default)]` bounds every type parameter, so it would
// demand `T: Default` — a requirement the phase does not have and that
// `DeliveryChannel<BlockNotification>` could not satisfy.
#[expect(
    clippy::derivable_impls,
    reason = "derive would add a spurious `T: Default` bound the phase does not need"
)]
impl<T> Default for DeliveryChannel<T> {
    fn default() -> Self {
        Self::Detached
    }
}

// Hand-written so a channel of a non-`Debug` payload still prints its phase
// (the payload is an opaque sender, not diagnostic state).
impl<T> std::fmt::Debug for DeliveryChannel<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryChannel")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    fn chan() -> (mpsc::UnboundedSender<u32>, mpsc::UnboundedReceiver<u32>) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn a_fresh_channel_is_detached_and_sends_report_not_attached() {
        let mut ch = DeliveryChannel::<u32>::default();
        assert_eq!(ch.phase(), ChannelPhase::Detached);
        assert_eq!(ch.send(1), SendOutcome::NotAttached);
    }

    #[test]
    fn attach_is_the_only_door_and_close_the_only_exit() {
        let (tx, _rx) = chan();
        let mut ch = DeliveryChannel::default();
        assert_eq!(ch.attach(tx), Ok(()));
        assert_eq!(ch.phase(), ChannelPhase::Open);
        assert_eq!(ch.close(), ChannelPhase::Closed);
    }

    #[test]
    fn attach_after_close_is_declined() {
        // The defect this module exists for: an `Option<Sender>` whose `None`
        // meant both "never attached" and "closed" let a re-attach silently
        // re-open a pipe whose receiver had already seen end-of-stream.
        let (tx, _rx) = chan();
        let (tx2, _rx2) = chan();
        let mut ch = DeliveryChannel::default();
        let _ = ch.attach(tx);
        ch.close();
        assert_eq!(ch.attach(tx2), Err(ChannelDecline::AttachedAfterClose));
        assert_eq!(ch.phase(), ChannelPhase::Closed, "closed is absorbing");
    }

    #[test]
    fn attach_twice_does_not_silently_end_the_first_consumer() {
        let (tx, _rx) = chan();
        let (tx2, _rx2) = chan();
        let mut ch = DeliveryChannel::default();
        let _ = ch.attach(tx);
        assert_eq!(ch.attach(tx2), Err(ChannelDecline::AlreadyAttached));
    }

    #[test]
    fn send_outcome_names_which_way_the_channel_was_absent() {
        let mut ch = DeliveryChannel::<u32>::default();
        assert_eq!(ch.send(1), SendOutcome::NotAttached);

        let (tx, rx) = chan();
        let _ = ch.attach(tx);
        assert_eq!(ch.send(2), SendOutcome::Delivered);

        drop(rx);
        assert_eq!(ch.send(3), SendOutcome::ReceiverGone);
        assert_eq!(
            ch.send(4),
            SendOutcome::Closed,
            "a gone receiver latches closed"
        );

        assert_eq!(ch.close(), ChannelPhase::Closed);
        assert_eq!(ch.send(5), SendOutcome::Closed);
    }

    #[test]
    fn receiver_gone_latches_the_channel_closed() {
        let (tx, rx) = chan();
        let mut ch = DeliveryChannel::default();
        let _ = ch.attach(tx);
        drop(rx);
        let _ = ch.send(1);
        assert_eq!(ch.phase(), ChannelPhase::Closed);
    }

    #[test]
    fn close_ends_the_stream_exactly_once() {
        let (tx, mut rx) = chan();
        let mut ch = DeliveryChannel::default();
        let _ = ch.attach(tx);
        assert_eq!(ch.send(1), SendOutcome::Delivered);
        assert_eq!(ch.close(), ChannelPhase::Closed);
        assert_eq!(rx.try_recv().expect("first item delivered"), 1);
        assert!(
            rx.try_recv().is_err(),
            "the receiver observes end-of-stream exactly once"
        );
        assert_eq!(ch.close(), ChannelPhase::Closed);
    }

    #[test]
    fn close_is_total_and_never_declines() {
        let mut ch = DeliveryChannel::<u32>::default();
        assert_eq!(ch.close(), ChannelPhase::Closed);
        assert_eq!(ch.close(), ChannelPhase::Closed);
    }

    #[test]
    fn delivered_is_true_only_for_a_real_send() {
        assert!(SendOutcome::Delivered.delivered());
        assert!(!SendOutcome::NotAttached.delivered());
        assert!(!SendOutcome::Closed.delivered());
        assert!(!SendOutcome::ReceiverGone.delivered());
    }

    #[test]
    fn every_state_answers_every_event() {
        // Totality: attach/send/close are defined from all three phases and
        // none of them panics or leaves an undefined arm.
        let (tx, _rx) = chan();
        let mut open = DeliveryChannel::<u32>::default();
        open.attach(tx).expect("a fresh channel attaches");

        let mut closed = DeliveryChannel::<u32>::default();
        let _ = closed.close();

        let mut detached = DeliveryChannel::<u32>::default();

        for ch in [&mut detached, &mut open, &mut closed] {
            let _ = ch.close();
            let _ = ch.send(0);
        }
    }
}
