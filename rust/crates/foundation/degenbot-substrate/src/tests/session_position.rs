//! Session positions — canonical position identity, and a read that refuses
//! instead of fabricating.
//!
//! Seam: `crate::session_registry::{Freshness, HealthFactor,
//! PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
//! SessionObjectRegistry}` — the position seam. A position is NOT a canonical
//! session object: only its IDENTITY (`chain`, `market`, `account`) is
//! session-canonical, and the VALUE is a fresh read-model projection reached
//! through an observer that can refuse. The registry holds no position store,
//! so there is nothing here for a failed read to fall back on.
//!
//! Terminology is the settled set in GLOSSARY.md § Session objects; the
//! identity-vs-projection split and the layer reasoning are in
//! `docs/architecture/session-object-registry.md`.

#![expect(clippy::expect_used)]

use crate::session_registry::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
    SessionObjectRegistry,
};
use alloy::primitives::{Address, U256};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::Context as TracingContext;
use tracing_subscriber::layer::{Layer, SubscriberExt};

/// Canonical identity is session-scoped, so the value only has to be a stable
/// per-test constant.
const CHAIN_ID: u64 = 1;

fn market() -> Address {
    Address::from([0x51u8; 20])
}

fn other_market() -> Address {
    Address::from([0x61u8; 20])
}

fn account() -> Address {
    Address::from([0x52u8; 20])
}

fn other_account() -> Address {
    Address::from([0x62u8; 20])
}

fn healthy(block: u64) -> Scripted {
    Scripted::Reading {
        observed_block: block,
        health: HealthFactor::Ratio(U256::from(2_500_000_000_000_000_000u64)),
    }
}

/// What the scripted observer answers with, per call. One answer per call, so
/// a test can prove the session asks the observer EVERY time instead of
/// serving a value (or a default) of its own.
#[derive(Clone, Copy, Debug)]
enum Scripted {
    Reading {
        observed_block: u64,
        health: HealthFactor,
    },
    Transient,
    MarketNotServed,
    UnknownPosition,
    Unreadable,
}

struct ScriptedObserver {
    answers: Mutex<VecDeque<Scripted>>,
    calls: AtomicUsize,
}

impl ScriptedObserver {
    fn new(answers: impl IntoIterator<Item = Scripted>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PositionObserver for ScriptedObserver {
    fn read_position(
        &self,
        identity: &PositionIdentity,
        _freshness: &Freshness,
    ) -> Result<PositionReading, PositionRefusal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let scripted = self
            .answers
            .lock()
            .expect("script mutex")
            .pop_front()
            .expect("a scripted answer per call");
        match scripted {
            Scripted::Reading {
                observed_block,
                health,
            } => Ok(PositionReading::new(*identity, observed_block, health)),
            Scripted::Transient => Err(PositionRefusal::TransientRead {
                identity: *identity,
                reason: String::from("scripted transport fault"),
            }),
            Scripted::MarketNotServed => Err(PositionRefusal::MarketNotServed {
                identity: *identity,
            }),
            Scripted::UnknownPosition => Err(PositionRefusal::UnknownPosition {
                identity: *identity,
            }),
            Scripted::Unreadable => Err(PositionRefusal::UnreadablePosition {
                identity: *identity,
                reason: String::from("scripted decode fault"),
            }),
        }
    }
}

/// A scripted refusal and the refusal it must produce, as a pair the test can
/// iterate: the point is that each cause is TYPED, so the expectation is built
/// from the identity under test rather than hard-coded.
type RefusalCase = (Scripted, fn(&PositionIdentity) -> PositionRefusal);

fn session_with(
    answers: impl IntoIterator<Item = Scripted>,
) -> (SessionObjectRegistry, Arc<ScriptedObserver>) {
    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let observer = ScriptedObserver::new(answers);
    assert!(
        registry
            .install_position_observer(Arc::clone(&observer) as Arc<dyn PositionObserver>)
            .is_ok(),
        "the first position observer installed wins"
    );
    (registry, observer)
}

/// The canonical key is `(chain, market, account)`: two consumers naming the
/// same position agree on it, a different market or account is a different
/// position, and identity is chain-scoped — a session for another chain names a
/// different position for the same contracts and account.
#[test]
fn a_position_identity_is_the_chain_scoped_market_and_account() {
    let settlement_session = SessionObjectRegistry::new(CHAIN_ID);
    let backrun_session = SessionObjectRegistry::new(CHAIN_ID);

    let from_settlement = settlement_session.position_identity(market(), account());
    let from_backrun = backrun_session.position_identity(market(), account());

    assert_eq!(from_settlement, from_backrun);
    assert_eq!(from_settlement.chain_id(), CHAIN_ID);
    assert_eq!(from_settlement.market(), market());
    assert_eq!(from_settlement.account(), account());

    assert_ne!(
        from_settlement,
        settlement_session.position_identity(market(), other_account())
    );
    assert_ne!(
        from_settlement,
        settlement_session.position_identity(other_market(), account())
    );

    // Chain-scoped: the same market and account on another chain is another
    // position, and the registry stamps the scope rather than trusting a
    // caller-supplied chain.
    let other_chain_session = SessionObjectRegistry::new(CHAIN_ID + 1);
    assert_ne!(
        from_settlement,
        other_chain_session.position_identity(market(), account())
    );
    assert_eq!(
        other_chain_session
            .position_identity(market(), account())
            .chain_id(),
        CHAIN_ID + 1
    );
}

/// The session's chain is part of the key, so a read for another chain is
/// refused at the session boundary — before the observer is asked, because an
/// observer on this chain cannot answer for one and a default would be a
/// fabricated position.
#[test]
fn a_position_read_for_another_chain_is_refused_before_the_observer_is_asked() {
    let (registry, observer) = session_with([healthy(100), healthy(100)]);
    let foreign = PositionIdentity::new(CHAIN_ID + 7, market(), account());

    let refusal = registry
        .read_position(&foreign, &Freshness::Any)
        .expect_err("a chain this session is not scoped to");

    assert_eq!(
        refusal,
        PositionRefusal::ChainScopeMismatch {
            identity: foreign,
            session_chain_id: CHAIN_ID,
        }
    );
    assert_eq!(
        observer.calls(),
        0,
        "the observer is never asked about a foreign chain"
    );
}

/// A session with no observer installed holds no positions and has nothing to
/// default to: the read is a typed refusal, and it is a DISTINCT one from an
/// observer that is installed but does not serve the market.
#[test]
fn an_unwired_session_refuses_rather_than_defaulting() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);
    assert!(!registry.has_position_observer());
    assert_eq!(
        registry
            .read_position(
                &registry.position_identity(market(), account()),
                &Freshness::Any
            )
            .expect_err("no observer installed"),
        PositionRefusal::NoPositionOwner
    );
    // Even the loosest freshness requirement cannot conjure a value: the
    // refusal is about the missing owner, not about staleness.
    assert_eq!(
        registry
            .read_position(
                &registry.position_identity(market(), account()),
                &Freshness::AtOrAfter { block: 0 },
            )
            .expect_err("no observer installed"),
        PositionRefusal::NoPositionOwner
    );
}

/// An operator-log capture, so a test can assert a refusal is LOUD rather than
/// merely returned. A plain `Layer` over the registry subscriber: it records
/// each event's level and rendered fields, which is all the assertions below
/// read, and it installs for the current thread only.
type LogCapture = Arc<Mutex<Vec<(Level, String)>>>;

struct RecordingLayer(LogCapture);

impl<S: Subscriber> Layer<S> for RecordingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _context: TracingContext<'_, S>) {
        let mut rendered = String::new();
        event.record(&mut FieldRenderer(&mut rendered));
        self.0
            .lock()
            .expect("log capture mutex")
            .push((*event.metadata().level(), rendered));
    }
}

struct FieldRenderer<'a>(&'a mut String);

impl Visit for FieldRenderer<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

/// Install the capture on this thread and hand the recorded events back.
fn capture_operator_log() -> (LogCapture, tracing::subscriber::DefaultGuard) {
    let capture: LogCapture = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(RecordingLayer(Arc::clone(&capture)));
    let guard = tracing::subscriber::set_default(subscriber);
    (capture, guard)
}

/// A second position source over one session is a CORRECTNESS FORK, not a
/// no-op: the two could answer the same identity from different state, which is
/// the drift the registry exists to remove. The first install therefore keeps
/// the session, the second is refused and handed back, and the fork is REPORTED
/// at ERROR — an operator must be able to see a session whose positions are
/// served by a source nobody chose deliberately.
#[test]
fn a_second_position_observer_is_refused_and_the_fork_is_reported() {
    let (capture, _guard) = capture_operator_log();

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let first = ScriptedObserver::new([healthy(100)]);
    let second = ScriptedObserver::new([healthy(999)]);

    assert!(registry
        .install_position_observer(Arc::clone(&first) as Arc<dyn PositionObserver>)
        .is_ok());
    assert!(
        capture.lock().expect("log capture mutex").is_empty(),
        "a clean install is not a fork and says nothing"
    );

    let refused = registry
        .install_position_observer(Arc::clone(&second) as Arc<dyn PositionObserver>)
        .expect_err("two sources over one identity is a fork, not a no-op");
    drop(refused);

    // Loud: the refusal reaches the operator log rather than living only in the
    // returned `Err`, because the composition site that caused it may be a
    // consumer's own code.
    let errors: Vec<String> = capture
        .lock()
        .expect("log capture mutex")
        .iter()
        .filter(|(level, _)| *level == Level::ERROR)
        .map(|(_, rendered)| rendered.clone())
        .collect();
    assert_eq!(errors.len(), 1, "one refused install reports one fork");
    assert!(
        errors[0].contains("a second position observer was refused"),
        "the report must name the fork: {}",
        errors[0]
    );

    // And the fork did not become a merge: the session still answers from the
    // FIRST source, and the refused one is never asked.
    assert!(registry.has_position_observer());
    let identity = registry.position_identity(market(), account());
    let reading = registry
        .read_position(&identity, &Freshness::Any)
        .expect("the first observer keeps serving the session");
    assert_eq!(reading.observed_block(), 100);
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 0, "a refused observer is never asked");
}

/// A caller states the freshness it requires, and an observation older than
/// that is refused as stale rather than handed back as if it were current. The
/// requirement is enforced by the session, so a lax observer cannot pass a
/// decaying snapshot through as fresh.
#[test]
fn a_read_older_than_the_required_freshness_is_refused_as_stale() {
    let (registry, observer) =
        session_with([healthy(100), healthy(100), healthy(100), healthy(100)]);
    let identity = registry.position_identity(market(), account());

    let stale = registry
        .read_position(&identity, &Freshness::AtOrAfter { block: 101 })
        .expect_err("a reading from block 100 cannot satisfy a block-101 requirement");
    assert_eq!(
        stale,
        PositionRefusal::StaleObservation {
            identity,
            observed_block: Some(100),
            required: Freshness::AtOrAfter { block: 101 },
        }
    );
    assert!(
        !stale.is_retryable(),
        "a stale reading is not a fault; re-reading is"
    );

    assert_eq!(
        registry
            .read_position(
                &identity,
                &Freshness::AtMost {
                    max_age: 5,
                    head: 200
                }
            )
            .expect_err("block 100 is 100 blocks behind head 200"),
        PositionRefusal::StaleObservation {
            identity,
            observed_block: Some(100),
            required: Freshness::AtMost {
                max_age: 5,
                head: 200
            },
        }
    );

    // The same observation is fresh enough for a caller that asks for less, so
    // the refusal is about the REQUIREMENT and never about the read itself.
    let fresh_enough = registry
        .read_position(
            &identity,
            &Freshness::AtMost {
                max_age: 200,
                head: 200,
            },
        )
        .expect("within the required age");
    assert_eq!(fresh_enough.observed_block(), 100);
    assert_eq!(
        fresh_enough.health_factor(),
        HealthFactor::Ratio(U256::from(2_500_000_000_000_000_000u64))
    );

    let any = registry
        .read_position(&identity, &Freshness::Any)
        .expect("a caller that does not care about age still gets the observation");
    assert_eq!(any.observed_block(), 100);
    assert_eq!(any.identity(), &identity);
    assert_eq!(observer.calls(), 4, "every read reaches the observer");
}

/// A transient read failure is a TYPED, retryable refusal and never a value:
/// the two failure modes this seam exists to prevent (a hidden freshness loss
/// and a fabricated position) are the same mistake here, and the seam refuses
/// both.
#[test]
fn a_transient_read_failure_is_a_typed_retryable_refusal_and_never_a_value() {
    let (registry, observer) = session_with([Scripted::Transient, Scripted::Transient]);
    let identity = registry.position_identity(market(), account());

    let refusal = registry
        .read_position(&identity, &Freshness::Any)
        .expect_err("a failed read produces no position");

    assert_eq!(
        refusal,
        PositionRefusal::TransientRead {
            identity,
            reason: String::from("scripted transport fault"),
        }
    );
    assert!(refusal.is_retryable(), "a transport fault is retryable");

    // Re-reading asks the observer again rather than falling back to a
    // remembered or default value: the registry holds no position store, so
    // there is nothing to fall back ON.
    let again = registry
        .read_position(&identity, &Freshness::Any)
        .expect_err("still failing, still no value");
    assert!(again.is_retryable());
    assert_eq!(observer.calls(), 2);
}

/// Each way a read can fail has its OWN refusal, so a caller can tell an
/// unwired market from an absent position from a transport fault from rows it
/// cannot interpret — and none of them carries a reading.
#[test]
fn every_refusal_is_typed_and_none_of_them_carries_a_reading() {
    let cases: [RefusalCase; 4] = [
        (Scripted::MarketNotServed, |identity| {
            PositionRefusal::MarketNotServed {
                identity: *identity,
            }
        }),
        (Scripted::UnknownPosition, |identity| {
            PositionRefusal::UnknownPosition {
                identity: *identity,
            }
        }),
        (Scripted::Transient, |identity| {
            PositionRefusal::TransientRead {
                identity: *identity,
                reason: String::from("scripted transport fault"),
            }
        }),
        (Scripted::Unreadable, |identity| {
            PositionRefusal::UnreadablePosition {
                identity: *identity,
                reason: String::from("scripted decode fault"),
            }
        }),
    ];

    for (scripted, expected) in cases {
        let (registry, observer) = session_with([scripted, scripted]);
        let identity = registry.position_identity(market(), account());

        let refusal = registry
            .read_position(&identity, &Freshness::Any)
            .expect_err("a refused read produces no position");
        assert_eq!(refusal, expected(&identity));
        assert_eq!(
            refusal.is_retryable(),
            matches!(refusal, PositionRefusal::TransientRead { .. })
        );

        // The loosest possible requirement does not launder a refusal into a
        // value, and the observer is still the only source.
        assert!(registry.read_position(&identity, &Freshness::Any).is_err());
        assert_eq!(observer.calls(), 2);
    }
}

/// Two consumers reading the same identity agree on the identity and on
/// NOTHING else: each read is a fresh projection, so a second read may report
/// a different block. That is the observable difference between an
/// identity-only registry and one that cached positions.
#[test]
fn two_consumers_agree_on_one_identity_and_never_share_a_reading() {
    let (registry, observer) = session_with([
        Scripted::Reading {
            observed_block: 100,
            health: HealthFactor::Ratio(U256::from(2u64)),
        },
        Scripted::Reading {
            observed_block: 101,
            health: HealthFactor::NoDebt,
        },
    ]);
    let settlement_identity = registry.position_identity(market(), account());
    let backrun_identity = registry.position_identity(market(), account());

    let from_settlement = registry
        .read_position(&settlement_identity, &Freshness::Any)
        .expect("first read");
    let from_backrun = registry
        .read_position(&backrun_identity, &Freshness::Any)
        .expect("second read");

    assert_eq!(from_settlement.identity(), from_backrun.identity());
    assert_ne!(
        from_settlement, from_backrun,
        "each read is a fresh projection"
    );
    assert_eq!(from_settlement.observed_block(), 100);
    assert_eq!(from_backrun.observed_block(), 101);
    assert_eq!(from_backrun.health_factor(), HealthFactor::NoDebt);
    assert_eq!(
        observer.calls(),
        2,
        "no read is served from a session-side store"
    );
}
