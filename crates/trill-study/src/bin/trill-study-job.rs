//! Studies a recording for a Trill that started it: the models run in a
//! process of their own, so that one that faults takes this process and
//! not Trill. `trill` and `trill-mcp` start themselves a second time for
//! it; this is for a program that cannot be (a test). See
//! [`trill_study::helper`].

use std::process::ExitCode;

/// For tests of a study of a link, which ask nothing of a network: given
/// what the program that fetches a link is given, and a link to
/// `recordings.test`, this "fetches" a file of this machine in its place.
/// The link is `https://recordings.test/<its length in seconds>/<the
/// file's path, each byte as two hexadecimal digits>`, or
/// `https://recordings.test/gone` for a page that is not there.
fn stand_in_fetcher(arguments: &[String]) -> Option<ExitCode> {
    let asked = arguments.last()?.strip_prefix("https://recordings.test/")?;
    let template = arguments.iter().position(|argument| argument == "-o").and_then(|at| arguments.get(at + 1))?;
    let Some((seconds, name)) = asked.split_once('/') else {
        eprintln!(
            "WARNING: this is a stand-in\nERROR: [generic] Unable to download webpage: HTTP Error 404: Not Found"
        );
        return Some(ExitCode::from(1));
    };
    let bytes: Vec<u8> = (0..name.len() / 2)
        .filter_map(|index| u8::from_str_radix(name.get(index * 2..index * 2 + 2)?, 16).ok())
        .collect();
    let from = std::path::PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
    let ending = from.extension().map(|ending| ending.to_string_lossy().into_owned()).unwrap_or_default();
    println!("some line of the program's own");
    println!("@trill@title=A stand-in song\n@trill@by=Nobody\n@trill@seconds={seconds}\n@trill@got=1/2");
    if let Err(error) = std::fs::copy(&from, template.replace("%(ext)s", &ending)) {
        eprintln!("ERROR: {}: {error}", from.display());
        return Some(ExitCode::from(1));
    }
    println!("@trill@got=2/2");
    Some(ExitCode::SUCCESS)
}

/// Asked which version it is, as the program that fetches a link is where
/// it failed, the stand-in says the one its own file is named for
/// (`fetcher-of-2001.01.01`): a test copies it under the name it wants.
/// Under its own name it has none to say.
fn stand_in_version(arguments: &[String]) -> Option<ExitCode> {
    if arguments != ["--version"] {
        return None;
    }
    let name = std::env::current_exe().ok()?.file_name()?.to_string_lossy().into_owned();
    println!("{}", name.trim_end_matches(".exe").split_once("-of-")?.1);
    Some(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    trill_study::helper::run_if_requested();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Some(ended) = stand_in_version(&arguments).or_else(|| stand_in_fetcher(&arguments)) {
        return ended;
    }
    eprintln!("trill-study-job studies for a Trill that starts it, and does nothing when it is started by hand.");
    ExitCode::from(2)
}
