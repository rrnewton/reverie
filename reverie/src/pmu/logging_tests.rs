use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use tracing::Event;
use tracing::Level;
use tracing::Subscriber;
use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

#[derive(Debug, PartialEq)]
pub(super) struct RecordedEvent {
    pub target: &'static str,
    pub module_path: &'static str,
    pub level: Level,
    pub fields: BTreeMap<String, String>,
}

impl Visit for RecordedEvent {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

struct Recorder(Arc<Mutex<Vec<RecordedEvent>>>);

impl<Observed: Subscriber> Layer<Observed> for Recorder {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, Observed>) {
        let metadata = event.metadata();
        let mut recorded = RecordedEvent {
            target: metadata.target(),
            module_path: metadata.module_path().unwrap(),
            level: *metadata.level(),
            fields: BTreeMap::new(),
        };
        event.record(&mut recorded);
        self.0.lock().unwrap().push(recorded);
    }
}

pub(super) fn capture(filter: &str, action: impl FnOnce()) -> Vec<RecordedEvent> {
    let records = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::try_new(filter).unwrap())
        .with(Recorder(Arc::clone(&records)));
    tracing::subscriber::with_default(subscriber, action);
    std::mem::take(&mut *records.lock().unwrap())
}
