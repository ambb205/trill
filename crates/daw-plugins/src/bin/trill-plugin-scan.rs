//! The plugin scanner: reads the descriptors of one plugin file and prints
//! them as JSON.
//!
//! The DAW runs this as a child process for every plugin file it has not
//! seen before, so that a plugin which crashes or hangs while being loaded
//! takes down this process instead of the DAW. See [`daw_plugins::scan`].

fn main() -> std::process::ExitCode {
    daw_plugins::scan::scanner_main()
}
