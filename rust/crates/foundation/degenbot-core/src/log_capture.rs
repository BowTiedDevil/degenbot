//! Structured-log `message` capture — one home for the field visitation that
//! capture layers previously re-implemented per crate (test capture
//! subscribers, the Python log forwarder, the retry-log probe).

use tracing::field::{Field, Visit};

/// A [`tracing::field::Visit`] impl capturing an event's `message` field.
///
/// `record_str` captures the string verbatim; `record_debug` captures the
/// `Debug` formatting, which wraps strings in quotes.
/// [`MessageVisitor::into_unquoted`] strips those quotes (a no-op for
/// `record_str` capture), which is the form line-oriented consumers want.
#[derive(Default)]
pub struct MessageVisitor {
    message: Option<String>,
    quoted: bool,
}

impl MessageVisitor {
    /// Visit `event` and return its `message`, Debug-quotes stripped.
    #[must_use]
    pub fn capture(event: &tracing::Event<'_>) -> Option<String> {
        let mut visitor = Self::default();
        event.record(&mut visitor);
        visitor.into_unquoted()
    }

    /// The captured `message` as the field was recorded: verbatim for
    /// `record_str`, `Debug`-formatted (quoted) for `record_debug`.
    #[must_use]
    pub fn into_message(self) -> Option<String> {
        self.message
    }

    /// The captured `message` with the surrounding quotes a `Debug`-recorded
    /// string carries removed.
    #[must_use]
    pub fn into_unquoted(self) -> Option<String> {
        if !self.quoted {
            return self.message;
        }
        self.message
            .as_deref()
            .and_then(|s| s.strip_prefix('"'))
            .and_then(|s| s.strip_suffix('"'))
            .map(str::to_owned)
            .or(self.message)
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
            self.quoted = false;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
            self.quoted = true;
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::MessageVisitor;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::Registry;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    /// Captures each event's `message` through `extract`, so both the raw and
    /// the unquoted accessors are exercised against real emissions.
    struct CaptureLayer<F: Fn(MessageVisitor) -> Option<String>>(Capture, F);

    impl<S: tracing::Subscriber, F: Fn(MessageVisitor) -> Option<String> + 'static> Layer<S>
        for CaptureLayer<F>
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            if let Some(message) = (self.1)(visitor) {
                self.0 .0.lock().unwrap().push(message);
            }
        }
    }

    fn captured() -> (Capture, impl FnOnce() -> Vec<String>) {
        let capture = Capture(Arc::new(Mutex::new(Vec::new())));
        let reader = capture.clone();
        (capture, move || reader.0.lock().unwrap().clone())
    }

    #[test]
    fn debug_recorded_message_is_captured_unquoted() {
        let (capture, read) = captured();
        let subscriber =
            Registry::default().with(CaptureLayer(capture, MessageVisitor::into_unquoted));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("plain {}", "text");
        });
        assert_eq!(read(), vec!["plain text".to_string()]);
    }

    #[test]
    fn str_recorded_message_keeps_embedded_quotes() {
        let (capture, read) = captured();
        let subscriber =
            Registry::default().with(CaptureLayer(capture, MessageVisitor::into_unquoted));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(message = "quoted \"ends\"");
        });
        assert_eq!(read(), vec!["quoted \"ends\"".to_string()]);
    }

    #[test]
    fn debug_wrapped_string_stays_quoted_under_into_message() {
        let (capture, read) = captured();
        let subscriber =
            Registry::default().with(CaptureLayer(capture, MessageVisitor::into_message));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(message = tracing::field::debug("wrapped".to_string()));
        });
        assert_eq!(read(), vec!["\"wrapped\"".to_string()]);
    }

    #[test]
    fn capture_strips_the_quotes_of_a_debug_wrapped_string() {
        let (capture, read) = captured();
        let subscriber =
            Registry::default().with(CaptureLayer(capture, MessageVisitor::into_unquoted));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(message = tracing::field::debug("wrapped".to_string()));
        });
        assert_eq!(read(), vec!["wrapped".to_string()]);
    }

    #[test]
    fn non_message_fields_are_ignored_and_missing_message_yields_none() {
        let (capture, read) = captured();
        let subscriber =
            Registry::default().with(CaptureLayer(capture, MessageVisitor::into_unquoted));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(attempt = 1u64);
        });
        assert!(read().is_empty());
    }
}
