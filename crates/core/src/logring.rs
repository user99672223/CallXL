//! `tracing` ring buffer feeding `export_diagnostics` (§10 Logging).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

const CAPACITY: usize = 4000;

static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn ring() -> &'static Mutex<VecDeque<String>> {
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

pub fn lines() -> Vec<String> {
    ring().lock().map(|r| r.iter().cloned().collect()).unwrap_or_default()
}

pub struct RingLayer;

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut f = Fields(String::new());
        event.record(&mut f);
        let line = format!("{} {} {}: {}", callxl_signal::now_ms(), meta.level(), meta.target(), f.0);
        if let Ok(mut r) = ring().lock() {
            if r.len() == CAPACITY {
                r.pop_front();
            }
            r.push_back(line);
        }
    }
}

/// Installs a global subscriber (stderr + ring) unless the embedder already did.
pub fn init(filter: &str) {
    use tracing_subscriber::prelude::*;
    let env = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter));
    let _ = tracing_subscriber::registry()
        .with(env)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_target(false))
        .with(RingLayer)
        .try_init();
}
