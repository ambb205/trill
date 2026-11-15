//! Logging for the DAW's binaries.

/// Logs to stderr. With the stdio transport stdout belongs to the protocol,
/// so nothing else may be printed there.
struct StderrLogger;

/// Crates of this workspace all start with this; their messages are shown
/// from info level up.
const OWN_PREFIX: &str = "trill";

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // Dependencies (the GPU and windowing stack especially) are chatty
        // at info and warn about things nobody here can act on.
        let limit = if metadata.target().starts_with(OWN_PREFIX) { log::Level::Info } else { log::Level::Error };
        metadata.level() <= limit
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Sends this workspace's `log` output (info and above) and other crates'
/// errors to stderr. Does nothing if a logger is already installed.
pub fn init_stderr() {
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
