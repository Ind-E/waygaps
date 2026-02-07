use alloc::format;
use core::fmt;

use nu_ansi_term::Color;
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

    fn write_to_stdout(&self, s: &str) {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
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

        let level = match *meta.level() {
            Level::TRACE => Color::Purple.paint("TRACE"),
            Level::DEBUG => Color::Blue.paint("DEBUG"),
            Level::INFO => Color::Green.paint("INFO"),
            Level::WARN => Color::Yellow.paint("WARN"),
            Level::ERROR => Color::Red.paint("ERROR"),
        };
        let _ = fmt::write(
            &mut buf,
            format_args!(
                "{} {} ",
                level,
                Color::Fixed(245).paint(format!(
                    "{}:{}:",
                    meta.target(),
                    meta.line().unwrap_or_default()
                )),
            ),
        );

        struct Visitor<'a>(&'a mut alloc::string::String);
        impl<'a> tracing::field::Visit for Visitor<'a> {
            fn record_debug(
                &mut self,
                field: &tracing::field::Field,
                value: &dyn fmt::Debug,
            ) {
                let _ = match field.name() {
                    "message" => {
                        fmt::write(self.0, format_args!("{:?}", value))
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

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}
