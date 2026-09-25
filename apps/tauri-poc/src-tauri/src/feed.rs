use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use degenbot::config::{resolve_node_ws_uri, ProcessEnv};
use degenbot::eip_1559;
use degenbot_ingestion::{IngestEvent, WsIngestor};
use degenbot_tauri_feed_model::{BlockFeedModel, BlockSnapshot, LogSnapshot};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const MAINNET_CHAIN_ID: u64 = 1;

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FeedStatus {
    #[default]
    Stopped,
    Connecting,
    Running,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FeedEvent {
    Status { status: FeedStatus },
    Block { block: BlockSnapshot },
    Log { log: LogSnapshot },
    Error { message: String },
}

impl FeedEvent {
    fn status(status: FeedStatus) -> Self {
        Self::Status { status }
    }

    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

#[derive(Default)]
struct FeedInner {
    running: AtomicBool,
    shutdown: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Clone, Default)]
pub struct FeedState {
    inner: Arc<FeedInner>,
}

impl FeedState {
    pub fn status(&self) -> FeedStatus {
        if self.inner.running.load(Ordering::Acquire) {
            FeedStatus::Running
        } else {
            FeedStatus::Stopped
        }
    }

    pub fn stop(&self) {
        if let Some(shutdown) = self
            .inner
            .shutdown
            .lock()
            .ok()
            .and_then(|value| value.clone())
        {
            shutdown.store(true, Ordering::Release);
        }
    }
}

pub fn configured_ws_url() -> Result<String, String> {
    resolve_node_ws_uri(&ProcessEnv, MAINNET_CHAIN_ID, None)
        .map(|resolved| resolved.value)
        .map_err(|error| error.to_string())
}

pub fn start_feed(app: AppHandle, state: FeedState, url: String) {
    if state
        .inner
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    if let Ok(mut current_shutdown) = state.inner.shutdown.lock() {
        *current_shutdown = Some(shutdown.clone());
    }

    let thread_state = state.clone();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                thread_state.inner.running.store(false, Ordering::Release);
                let _ = app.emit("feed-event", FeedEvent::error(error.to_string()));
                return;
            }
        };
        runtime.block_on(run_feed(app.clone(), url, shutdown));
        thread_state.inner.running.store(false, Ordering::Release);
    });
}

async fn run_feed(app: AppHandle, url: String, shutdown: Arc<AtomicBool>) {
    let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Connecting));
    let ingestor = match WsIngestor::connect(&url).await {
        Ok(ingestor) => ingestor,
        Err(error) => {
            let _ = app.emit("feed-event", FeedEvent::error(error));
            let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Error));
            return;
        }
    };
    let mut stream = match ingestor.subscribe_events().await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = app.emit("feed-event", FeedEvent::error(error));
            let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Error));
            return;
        }
    };
    let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Running));
    let mut model = BlockFeedModel::default();

    loop {
        if shutdown.load(Ordering::Acquire) {
            let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Stopped));
            return;
        }
        tokio::select! {
            event = stream.next() => {
                match event {
                    Some(IngestEvent::BlockHeader {
                        number,
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    }) => {
                        let transaction_count = match ingestor.get_block(number).await {
                            Ok(Some(block)) => Some(block.transactions.len()),
                            Ok(None) => None,
                            Err(error) => {
                                let _ = app.emit("feed-event", FeedEvent::error(format!(
                                    "block {number} transaction count: {error}"
                                )));
                                None
                            }
                        };
                        let next_base_fee_wei = base_fee_per_gas.map(|base_fee| {
                            u64::try_from(eip_1559::next_base_fee(
                                u128::from(base_fee),
                                u128::from(gas_used),
                                u128::from(gas_limit),
                                None,
                                8,
                                2,
                            ))
                            .unwrap_or(u64::MAX)
                        });
                        let block = model.on_header(
                            number,
                            timestamp,
                            transaction_count,
                            base_fee_per_gas,
                            next_base_fee_wei,
                            gas_used,
                            gas_limit,
                        );
                        let _ = app.emit("feed-event", FeedEvent::Block { block });
                    }
                    Some(IngestEvent::Pool(pool_event)) => {
                        let log = LogSnapshot {
                            block_number: pool_event.payload.block_number,
                            log_index: pool_event.payload.log_index,
                            address: format!("{:#x}", pool_event.payload.address()),
                            topic0: pool_event
                                .payload
                                .topics()
                                .first()
                                .map(|topic| format!("{topic:#x}")),
                            removed: pool_event.payload.removed,
                        };
                        if let Some(block) = model.on_log(&log) {
                            let _ = app.emit("feed-event", FeedEvent::Block { block });
                        }
                        let _ = app.emit("feed-event", FeedEvent::Log { log });
                    }
                    None => {
                        let _ = app.emit("feed-event", FeedEvent::error(
                            "newHeads/logs subscriptions ended",
                        ));
                        let _ = app.emit("feed-event", FeedEvent::status(FeedStatus::Error));
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}
