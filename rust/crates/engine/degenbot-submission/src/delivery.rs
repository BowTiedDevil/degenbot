use degenbot_core::errors::ProviderError;

/// The class of one failed delivery attempt.
///
/// The transport and refusal classes are the provider's own taxonomy
/// ([`ProviderError`]) plus the vocabulary the bundle arms' channels answer
/// with. A class says what the failure is *evidence of*, which is what the
/// policy reads: [`is_liveness_signal`](Self::is_liveness_signal),
/// [`is_definitive`](Self::is_definitive), or
/// [`is_ambiguous`](Self::is_ambiguous) — every class is exactly one of the
/// three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryFailureClass {
    /// The node already holds these exact signed bytes. A live-transaction
    /// signal, not a rejection: the submission exists on the wire.
    AlreadyKnown,
    /// The node refused the nonce because a lower nonce is still unmined: the
    /// bytes can never be included at that nonce.
    NonceTooLow,
    /// The signer's account cannot cover gas plus value: the bytes can never be
    /// included by that account as it stands.
    InsufficientFunds,
    /// The nonce is held by a live transaction whose fees these fees do not
    /// dominate. The bytes are admissible only as a replacement, which this
    /// submission is not.
    ReplacementUnderpriced,
    /// The node could not accept the signed payload's own encoding: the bytes
    /// can never be included as they stand.
    Malformed,
    /// A delivery channel refused to admit the submission for the slot it was
    /// pinned to, so no channel holds it. This is the bundle arms' definitive
    /// class: a bid is valid for exactly the block it was pinned to, and the
    /// auction books a bid only by acknowledging it — so a round trip that ends
    /// without an acknowledgement leaves no bid for that block, and no later
    /// block can include a bid pinned to a passed one. The pin is the bid's
    /// whole liveness window, which is why these failures are non-liveness
    /// rather than transport ambiguity.
    NotAdmitted,
    /// The attempt timed out: the body may have reached the channel.
    Timeout,
    /// The connection failed: the body may have reached the channel.
    Connection,
    /// The caller's retry budget ran out before the attempt resolved: the last
    /// attempt's fate is unknown.
    RetryBudget,
    /// The channel rate-limited the request. The endpoint refused to serve it,
    /// but a fan-out's other attempts and the provider's own retry loop may
    /// still have delivered the body, so the submission's fate is unknown.
    RateLimited,
    /// A failure this taxonomy does not name. It is evidence of nothing about
    /// the submission's liveness, so it reads as ambiguity: a wrongly held
    /// lease costs time, a wrongly released one can double-issue a nonce.
    Unclassified,
}

impl DeliveryFailureClass {
    /// Every class, in declaration order.
    ///
    /// Exhaustive on purpose: the classifier's tests walk this list, so a new
    /// class cannot land without a truth-table verdict.
    pub const ALL: &'static [Self] = &[
        Self::AlreadyKnown,
        Self::NonceTooLow,
        Self::InsufficientFunds,
        Self::ReplacementUnderpriced,
        Self::Malformed,
        Self::NotAdmitted,
        Self::Timeout,
        Self::Connection,
        Self::RetryBudget,
        Self::RateLimited,
        Self::Unclassified,
    ];

    /// The class's census label: the closed value set the delivery telemetry
    /// counts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyKnown => "already_known",
            Self::NonceTooLow => "nonce_too_low",
            Self::InsufficientFunds => "insufficient_funds",
            Self::ReplacementUnderpriced => "replacement_underpriced",
            Self::Malformed => "malformed",
            Self::NotAdmitted => "not_admitted",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::RetryBudget => "retry_budget",
            Self::RateLimited => "rate_limited",
            Self::Unclassified => "unclassified",
        }
    }

    /// Whether the class proves the signed bytes are already live.
    #[must_use]
    pub const fn is_liveness_signal(self) -> bool {
        matches!(self, Self::AlreadyKnown)
    }

    /// Whether the class is a definitive verdict on the signed bytes: a channel
    /// examined them and refused to admit them, so they can never be included.
    #[must_use]
    pub const fn is_definitive(self) -> bool {
        matches!(
            self,
            Self::NonceTooLow
                | Self::InsufficientFunds
                | Self::ReplacementUnderpriced
                | Self::Malformed
                | Self::NotAdmitted
        )
    }

    /// Whether the class is a failed attempt whose outcome is unknown: the body
    /// may or may not have reached a channel that could include it.
    #[must_use]
    pub const fn is_ambiguous(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Connection
                | Self::RetryBudget
                | Self::RateLimited
                | Self::Unclassified
        )
    }

    /// The class of one failed [`AlloyProvider::eth_send_raw_transaction`]
    /// attempt.
    ///
    /// [`AlloyProvider::eth_send_raw_transaction`]:
    /// degenbot_rpc::provider::AlloyProvider::eth_send_raw_transaction
    #[must_use]
    pub fn from_provider_error(error: &ProviderError) -> Self {
        match error {
            ProviderError::Timeout { .. } => Self::Timeout,
            ProviderError::ConnectionFailed { .. } => Self::Connection,
            ProviderError::RetryBudgetExhausted { .. } => Self::RetryBudget,
            ProviderError::RateLimited { .. } => Self::RateLimited,
            ProviderError::InvalidParams { .. } => Self::Malformed,
            ProviderError::RpcError { message, .. } => Self::from_refusal_message(message),
            // A failure outside the taxonomy: it proves nothing about whether
            // the bytes are live, which is ambiguity, not a refusal.
            _ => Self::Unclassified,
        }
    }

    /// The class of a node's refusal message.
    ///
    /// A node answers a rejected `eth_sendRawTransaction` with a free-form
    /// `RpcError` message, so the classes the policy distinguishes are read
    /// from the markers the mainstream clients emit. An unrecognized refusal is
    /// still a refusal — the node examined the bytes and declined to admit them
    /// — so it lands in [`Self::NotAdmitted`], never in ambiguity.
    fn from_refusal_message(message: &str) -> Self {
        let message = message.to_ascii_lowercase();
        let markers: &[(&[&str], Self)] = &[
            (&["already known"], Self::AlreadyKnown),
            (&["nonce too low"], Self::NonceTooLow),
            (
                &["insufficient funds", "lack of funds"],
                Self::InsufficientFunds,
            ),
            (
                &[
                    "replacement transaction underpriced",
                    "replacement underpriced",
                ],
                Self::ReplacementUnderpriced,
            ),
            (
                &[
                    "malformed",
                    "rlp",
                    "failed to decode",
                    "transaction type not supported",
                ],
                Self::Malformed,
            ),
        ];
        for (markers, class) in markers {
            if markers.iter().any(|marker| message.contains(marker)) {
                return *class;
            }
        }
        Self::NotAdmitted
    }
}

/// What a failed delivery attempt implies for the nonce its bytes were signed
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryVerdict {
    /// Definitive non-liveness: no channel admitted the bytes and none can, so
    /// the lease goes back and the nonce is reusable.
    Release,
    /// Ambiguity: the bytes may be live. The lease stays held and the ledger
    /// record moves to `DeliveryUnknown` until chain evidence or the bounded
    /// hold settles it.
    Hold,
    /// Liveness: a channel already holds these exact bytes. The submission is
    /// tracked as a broadcast, exactly as an acknowledged delivery is.
    Promote,
}

impl DeliveryVerdict {
    /// The verdict's census label: the closed value set the delivery telemetry
    /// counts, in the join's own vocabulary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Hold => "hold",
            Self::Promote => "promote",
        }
    }

    /// The rank the join orders by: definitive non-liveness below ambiguity
    /// below liveness.
    const fn rank(self) -> u8 {
        match self {
            Self::Release => 0,
            Self::Hold => 1,
            Self::Promote => 2,
        }
    }

    /// The least upper bound of two verdicts — the more evidential one.
    const fn join(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// What one failure class is evidence of.
const fn evidence_of(failure: DeliveryFailureClass) -> DeliveryVerdict {
    if failure.is_liveness_signal() {
        DeliveryVerdict::Promote
    } else if failure.is_ambiguous() {
        DeliveryVerdict::Hold
    } else {
        DeliveryVerdict::Release
    }
}

/// The one delivery-failure policy: what a set of failed attempts implies for
/// the lease.
///
/// Every attempt's class is recorded, and the set's verdict is their join: a
/// liveness signal anywhere wins (`already known` means the bytes are on the
/// wire), ambiguity beats a definitive refusal (one transport failure leaves
/// the submission's fate open even next to a node that refused it), and only a
/// unanimously definitive set releases the lease.
///
/// The join is monotone — adding evidence can raise a verdict but never lower
/// it — so an arm that gains a new failure class cannot silently turn a hold
/// into a release.
#[must_use]
pub fn classify_delivery_failure(failures: &[DeliveryFailureClass]) -> DeliveryVerdict {
    let mut verdict = DeliveryVerdict::Release;
    for failure in failures {
        verdict = verdict.join(evidence_of(*failure));
    }
    verdict
}

/// The class that decided the verdict: the most evidential member of the set,
/// in the join's ranking (liveness above ambiguity above a definite refusal).
///
/// Telemetry reads it so a verdict is attributable — "which attempt told us" —
/// without a second policy helper. `None` for an empty set, whose verdict is the
/// join's identity rather than any attempt's evidence.
#[must_use]
pub fn deciding_failure(failures: &[DeliveryFailureClass]) -> Option<DeliveryFailureClass> {
    failures
        .iter()
        .copied()
        .max_by_key(|class| evidence_of(*class).rank())
}

#[cfg(test)]
mod tests {
    use degenbot_core::errors::ProviderError;
    use proptest::prelude::*;

    use super::*;

    /// The hand-written truth table: every class, and the verdict that class
    /// alone must produce. Deliberately independent of the classifier's own
    /// predicates so a mis-signed predicate cannot license itself.
    const TRUTH_TABLE: &[(DeliveryFailureClass, DeliveryVerdict)] = &[
        (DeliveryFailureClass::AlreadyKnown, DeliveryVerdict::Promote),
        (DeliveryFailureClass::NonceTooLow, DeliveryVerdict::Release),
        (
            DeliveryFailureClass::InsufficientFunds,
            DeliveryVerdict::Release,
        ),
        (
            DeliveryFailureClass::ReplacementUnderpriced,
            DeliveryVerdict::Release,
        ),
        (DeliveryFailureClass::Malformed, DeliveryVerdict::Release),
        (DeliveryFailureClass::NotAdmitted, DeliveryVerdict::Release),
        (DeliveryFailureClass::Timeout, DeliveryVerdict::Hold),
        (DeliveryFailureClass::Connection, DeliveryVerdict::Hold),
        (DeliveryFailureClass::RetryBudget, DeliveryVerdict::Hold),
        (DeliveryFailureClass::RateLimited, DeliveryVerdict::Hold),
        (DeliveryFailureClass::Unclassified, DeliveryVerdict::Hold),
    ];

    /// The oracle join: liveness beats ambiguity, ambiguity beats a definitive
    /// refusal. Written as a match so it cannot borrow the implementation's
    /// ranking.
    fn oracle_join(left: DeliveryVerdict, right: DeliveryVerdict) -> DeliveryVerdict {
        use DeliveryVerdict::{Hold, Promote, Release};
        match (left, right) {
            (Promote, _) | (_, Promote) => Promote,
            (Hold, _) | (_, Hold) => Hold,
            (Release, Release) => Release,
        }
    }

    fn rank(verdict: DeliveryVerdict) -> u8 {
        match verdict {
            DeliveryVerdict::Release => 0,
            DeliveryVerdict::Hold => 1,
            DeliveryVerdict::Promote => 2,
        }
    }

    fn each_class() -> impl Strategy<Value = DeliveryFailureClass> {
        proptest::sample::select(DeliveryFailureClass::ALL)
    }

    #[test]
    fn the_truth_table_covers_every_class_exactly_once() {
        let listed: Vec<DeliveryFailureClass> =
            TRUTH_TABLE.iter().map(|(class, _)| *class).collect();
        assert_eq!(listed.len(), DeliveryFailureClass::ALL.len());
        for class in DeliveryFailureClass::ALL {
            assert_eq!(
                listed.iter().filter(|other| *other == class).count(),
                1,
                "{class:?} needs exactly one truth-table row"
            );
        }
    }

    #[test]
    fn a_unanimous_set_takes_its_one_class_verdict() {
        for &(class, expected) in TRUTH_TABLE {
            assert_eq!(
                classify_delivery_failure(&[class]),
                expected,
                "a unanimous {class:?} set"
            );
            assert_eq!(
                classify_delivery_failure(&[class, class, class]),
                expected,
                "repetition cannot change a verdict"
            );
        }
    }

    #[test]
    fn a_mixed_set_takes_its_most_evidential_member() {
        for &(left, left_verdict) in TRUTH_TABLE {
            for &(right, right_verdict) in TRUTH_TABLE {
                let expected = oracle_join(left_verdict, right_verdict);
                assert_eq!(
                    classify_delivery_failure(&[left, right]),
                    expected,
                    "{left:?} + {right:?}"
                );
                assert_eq!(
                    classify_delivery_failure(&[right, left]),
                    expected,
                    "attempt order cannot change a verdict ({right:?} + {left:?})"
                );
            }
        }
    }

    #[test]
    fn an_already_known_attempt_promotes_whatever_else_failed() {
        for &(class, _) in TRUTH_TABLE {
            let already = DeliveryFailureClass::AlreadyKnown;
            assert_eq!(
                classify_delivery_failure(&[class, already]),
                DeliveryVerdict::Promote,
                "{class:?} alongside a liveness signal"
            );
            assert_eq!(
                classify_delivery_failure(&[already, class]),
                DeliveryVerdict::Promote,
                "the liveness signal short-circuits in either position ({class:?})"
            );
        }
    }

    #[test]
    fn an_ambiguous_failure_holds_a_fully_definitive_set() {
        for &(definitive, _) in TRUTH_TABLE {
            if definitive.is_ambiguous() || definitive.is_liveness_signal() {
                continue;
            }
            for &(transport, _) in TRUTH_TABLE {
                if !transport.is_ambiguous() {
                    continue;
                }
                assert_eq!(
                    classify_delivery_failure(&[definitive, transport]),
                    DeliveryVerdict::Hold,
                    "ambiguity wins over definitiveness ({definitive:?} + {transport:?})"
                );
            }
        }
    }

    #[test]
    fn no_recorded_failure_is_the_lattice_identity() {
        // The arms reach the classifier only after a failed attempt and always
        // record what failed; an empty set is the join's identity, not a fourth
        // policy.
        assert_eq!(classify_delivery_failure(&[]), DeliveryVerdict::Release);
    }

    #[test]
    fn every_class_is_exactly_one_kind_of_evidence() {
        for class in DeliveryFailureClass::ALL {
            let kinds = [
                class.is_liveness_signal(),
                class.is_definitive(),
                class.is_ambiguous(),
            ];
            assert_eq!(
                kinds.iter().filter(|kind| **kind).count(),
                1,
                "{class:?} must be exactly one of liveness, definitive, or ambiguous"
            );
        }
    }

    #[test]
    fn provider_transport_failures_read_as_ambiguity() {
        let cases = [
            (
                ProviderError::Timeout {
                    message: "read timeout".to_string(),
                },
                DeliveryFailureClass::Timeout,
            ),
            (
                ProviderError::ConnectionFailed {
                    message: "connection reset".to_string(),
                },
                DeliveryFailureClass::Connection,
            ),
            (
                ProviderError::RetryBudgetExhausted {
                    attempt: 3,
                    elapsed_ms: 900,
                    budget_ms: 800,
                    last_error: "timeout".to_string(),
                },
                DeliveryFailureClass::RetryBudget,
            ),
            (
                ProviderError::RateLimited {
                    message: "429 Too Many Requests".to_string(),
                },
                DeliveryFailureClass::RateLimited,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(
                DeliveryFailureClass::from_provider_error(&error),
                expected,
                "{error:?}"
            );
            assert!(expected.is_ambiguous(), "{expected:?}");
        }
    }

    #[test]
    fn node_refusal_messages_take_their_named_class() {
        let cases = [
            (
                "eth_sendRawTransaction failed: error code -32000: already known",
                DeliveryFailureClass::AlreadyKnown,
            ),
            (
                "eth_sendRawTransaction failed: error code -32000: nonce too low: next nonce 8, tx nonce 7",
                DeliveryFailureClass::NonceTooLow,
            ),
            (
                "eth_sendRawTransaction failed: error code -32000: insufficient funds for gas * price + value",
                DeliveryFailureClass::InsufficientFunds,
            ),
            (
                "eth_sendRawTransaction failed: error code -32000: replacement transaction underpriced",
                DeliveryFailureClass::ReplacementUnderpriced,
            ),
            (
                "eth_sendRawTransaction failed: error code -32000: malformed transaction: rlp: expected input list",
                DeliveryFailureClass::Malformed,
            ),
        ];
        for (message, expected) in cases {
            assert_eq!(
                DeliveryFailureClass::from_provider_error(&ProviderError::RpcError {
                    code: -32000,
                    message: message.to_string(),
                }),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn a_refusal_the_taxonomy_does_not_name_is_still_definitive() {
        // The node examined the signed bytes and refused to admit them. The
        // class is not one this taxonomy names individually, but a refusal is
        // evidence of non-liveness all the same.
        let error = ProviderError::RpcError {
            code: -32000,
            message: "eth_sendRawTransaction failed: error code -32000: intrinsic gas too low"
                .to_string(),
        };
        let class = DeliveryFailureClass::from_provider_error(&error);
        assert_eq!(class, DeliveryFailureClass::NotAdmitted);
        assert!(class.is_definitive());
    }

    #[test]
    fn malformed_parameters_are_a_definitive_payload_refusal() {
        assert_eq!(
            DeliveryFailureClass::from_provider_error(&ProviderError::InvalidParams {
                message: "bad nonce field".to_string(),
            }),
            DeliveryFailureClass::Malformed
        );
    }

    #[test]
    fn a_failure_outside_the_taxonomy_reads_as_ambiguity() {
        let error = ProviderError::InvalidResponse {
            message: "garbled relay answer".to_string(),
        };
        let class = DeliveryFailureClass::from_provider_error(&error);
        assert_eq!(class, DeliveryFailureClass::Unclassified);
        assert!(
            class.is_ambiguous(),
            "nothing about an unclassifiable failure proves the bytes are not live"
        );
    }

    #[test]
    fn the_deciding_class_is_the_most_evidential_member() {
        use DeliveryFailureClass::{AlreadyKnown, NotAdmitted, Timeout};

        assert_eq!(deciding_failure(&[]), None);
        assert_eq!(deciding_failure(&[NotAdmitted]), Some(NotAdmitted));
        assert_eq!(
            deciding_failure(&[NotAdmitted, Timeout]),
            Some(Timeout),
            "ambiguity outranks a refusal"
        );
        assert_eq!(
            deciding_failure(&[Timeout, NotAdmitted, AlreadyKnown]),
            Some(AlreadyKnown),
            "a liveness signal outranks everything"
        );
        // The attribution and the verdict agree on the evidence class.
        for &(class, verdict) in TRUTH_TABLE {
            assert_eq!(
                verdict.as_str(),
                classify_delivery_failure(&[class]).as_str()
            );
            assert_eq!(deciding_failure(&[class]), Some(class));
        }
    }

    #[test]
    fn every_class_and_verdict_has_a_distinct_census_label() {
        let mut labels: Vec<&'static str> = DeliveryFailureClass::ALL
            .iter()
            .map(|class| class.as_str())
            .collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "class labels must be unique");

        let mut verdict_labels = vec![
            DeliveryVerdict::Release.as_str(),
            DeliveryVerdict::Hold.as_str(),
            DeliveryVerdict::Promote.as_str(),
        ];
        verdict_labels.sort_unstable();
        verdict_labels.dedup();
        assert_eq!(verdict_labels.len(), 3, "verdict labels must be unique");
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(256))]

        /// Monotone by construction: adding evidence to a failed attempt can
        /// raise the verdict, never lower it — so a later arm cannot quietly
        /// downgrade a hold into a release.
        #[test]
        fn adding_evidence_never_lowers_the_verdict(
            base in proptest::collection::vec(each_class(), 1..5),
            extra in each_class(),
        ) {
            let before = classify_delivery_failure(&base);
            let mut extended = base.clone();
            extended.push(extra);
            let after = classify_delivery_failure(&extended);
            prop_assert!(
                rank(after) >= rank(before),
                "{:?} + {:?}: {:?} -> {:?}",
                base,
                extra,
                before,
                after
            );
        }
    }
}
