//! Records all trace events and span fields, including dependency diagnostics.

use std::{
    fmt::Write as _,
    sync::{Arc, Mutex, atomic::AtomicU64},
};

use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

#[derive(Clone, Default)]
pub(crate) struct Capture(pub(crate) Arc<Mutex<String>>, Arc<AtomicU64>);

struct Record<'a>(&'a mut String);

impl Visit for Record<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attributes: &span::Attributes<'_>) -> span::Id {
        let mut output = self.0.lock().unwrap();
        let _ = write!(
            output,
            "span={} target={} ",
            attributes.metadata().name(),
            attributes.metadata().target()
        );
        attributes.record(&mut Record(&mut output));
        span::Id::from_u64(self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
    }

    fn record(&self, _: &span::Id, record: &span::Record<'_>) {
        record.record(&mut Record(&mut self.0.lock().unwrap()));
    }

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        event.record(&mut Record(&mut self.0.lock().unwrap()));
    }

    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}
