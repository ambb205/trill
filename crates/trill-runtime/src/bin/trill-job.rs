//! Does renders, measurements and probes for a Trill that started it: a
//! song with plugins is rendered in a process of its own, so that a
//! plugin that takes its process down takes this one. `trill` and
//! `trill-mcp` start themselves a second time for it; this is for a
//! program that cannot be. See [`trill_runtime::jobs`].

fn main() -> std::process::ExitCode {
    trill_runtime::jobs::run_as_job_if_requested();
    eprintln!("trill-job renders for a Trill that starts it, and does nothing when it is started by hand.");
    std::process::ExitCode::from(2)
}
