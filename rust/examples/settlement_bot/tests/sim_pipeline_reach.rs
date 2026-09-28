//! Umbrella reach proof for the core bounded-concurrency + ordered-submit
//! pipeline (`degenbot::submission::SimSubmitPipeline`).
//!
//! The example is the `cargo add degenbot` consumer: this test constructs the
//! core pipeline through the umbrella re-export with trivial local leaves and
//! asserts the submit lane preserves arrival order, so ledger row 16 is a
//! compile-and-run reach claim rather than a prose assertion. The mechanism
//! itself is unit-tested in `degenbot-submission`.

use std::sync::Arc;

use degenbot::submission::{SimFuture, SimLeaf, SimSubmitPipeline, SubmitFuture, SubmitLeaf};
use tokio::sync::Mutex;

struct EchoSim;

impl SimLeaf<u64, u64> for EchoSim {
    fn simulate<'a>(&'a self, work: &'a u64) -> SimFuture<'a, u64> {
        Box::pin(async move { Ok(Some(*work)) })
    }
}

struct RecordingSubmit {
    order: Arc<Mutex<Vec<u64>>>,
}

impl SubmitLeaf<u64, u64> for RecordingSubmit {
    fn submit<'a>(&'a self, _work: &'a u64, outcome: &'a u64) -> SubmitFuture<'a> {
        let order = Arc::clone(&self.order);
        let id = *outcome;
        Box::pin(async move {
            order.lock().await.push(id);
            Ok(())
        })
    }
}

#[tokio::test]
async fn core_pipeline_is_reachable_through_the_umbrella() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let pipeline = SimSubmitPipeline::new(
        2,
        Arc::new(EchoSim),
        Arc::new(RecordingSubmit {
            order: Arc::clone(&order),
        }),
    );
    assert_eq!(pipeline.concurrency(), 2);
    for id in [1_u64, 2, 3] {
        pipeline.enqueue(id);
    }
    assert!(pipeline.shutdown().await.is_ok(), "no leaf failure");
    assert_eq!(*order.lock().await, vec![1, 2, 3]);
    assert_eq!(pipeline.enqueued(), 3);
    assert_eq!(pipeline.submitted(), 3);
}
