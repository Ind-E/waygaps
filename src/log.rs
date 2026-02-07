use core::fmt;

use rustix::io::write;
use rustix::stdio::stdout;
use tracing::{Event, Level, Metadata, span, subscriber::Subscriber};

pub struct LinuxSubscriber {
    max_level: Level,
}

impl LinuxSubscriber {
    pub fn new(max_level: Level) -> Self {
        Self { max_level }
    }

    // Helper to write to Linux stdout via rustix
    fn write_to_stdout(&self, s: &str) {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            // rustix handles the raw syscall
            if let Ok(n) = write(unsafe { stdout() }, bytes) {
                bytes = &bytes[n..];
            } else {
                break;
            }
        }
    }
}

impl Subscriber for LinuxSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.max_level
    }

    fn event(&self, event: &Event<'_>) {
        let mut buf = alloc::string::String::new();
        let meta = event.metadata();

        // Format: [LEVEL] target:
        let _ = fmt::write(
            &mut buf,
            format_args!("[{}] {}: ", meta.level(), meta.target()),
        );

        // Use a visitor to extract fields (message, variables)
        struct Visitor<'a>(&'a mut alloc::string::String);
        impl<'a> tracing::field::Visit for Visitor<'a> {
            fn record_debug(
                &mut self,
                field: &tracing::field::Field,
                value: &dyn fmt::Debug,
            ) {
                let _ = match field.name() {
                    "message" => {
                        fmt::write(self.0, format_args!(" {:?}", value))
                    }
                    other => fmt::write(
                        self.0,
                        format_args!(" {}={:?}", other, value),
                    ),
                };
            }
        }

        let mut visitor = Visitor(&mut buf);
        event.record(&mut visitor);
        buf.push('\n');

        self.write_to_stdout(&buf);
    }

    // Minimal implementations required for the trait
    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}
