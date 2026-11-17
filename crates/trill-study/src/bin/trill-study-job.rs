//! Studies a recording for a Trill that started it: the models run in a
//! process of their own, so that one that faults takes this process and
//! not Trill. `trill` and `trill-mcp` start themselves a second time for
//! it; this is for a program that cannot be. See [`trill_study::helper`].

fn main() -> std::process::ExitCode {
    trill_study::helper::run_if_requested();
    eprintln!("trill-study-job studies for a Trill that starts it, and does nothing when it is started by hand.");
    std::process::ExitCode::from(2)
}
