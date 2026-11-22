//! `trill-meet`: a meeting point, run by whoever keeps a server for
//! their friends. See the crate's own page, and `trill-meet help`.

use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use trill_meet::{Keys, Limits, PORT, Server, keys};

/// The file its keys are in unless it is told another.
const KEYS: &str = "trill-meet.keys";

const HELP: &str = "\
trill-meet: a meeting point for Trill.

Two Trills that cannot reach each other directly both connect to it, and it
passes what each says to the other. It can neither read nor join a song:
the two prove the invitation to each other through it and encrypt
everything after. Only someone you gave a key can open a meeting on it;
whoever has an invitation can join one.

  trill-meet serve [--port N] [--listen ADDRESS]... [--keys FILE] [limits]
      Runs the meeting point. Without --listen it takes IPv6 and IPv4 on
      the port (8745 unless --port says another; 443 passes most networks).
      --listen names an address and port to take instead, and may be given
      more than once: 0.0.0.0:443, [::]:443, 127.0.0.1:8745.
      It writes its log to the standard output, a line a happening.

  trill-meet key NAME [--keys FILE]
      Makes a key for someone and shows it, once. The file keeps what
      proves the key and whose it is, not the key.

  trill-meet keys [--keys FILE]
      Lists whose keys there are.

  trill-meet forget NAME|KEY-ID [--keys FILE]
      Takes a key away. A meeting point that is running reads the file
      again by itself, and ends what was held with that key within a few
      seconds: its meetings, and the people in them.

The keys are in trill-meet.keys in the folder it is started in, unless
--keys says another file.

Limits, each with what it is unless you say otherwise:
  --connections N                  connections at once, of every kind
  --from-one-address N             of them from one address
  --a-minute-from-one-address N    new connections one address may make in a minute
  --meetings N                     meetings held at once
  --meetings-of-one-key N          of them by one key
  --people N                       people in a meeting, its holder among them
  --hello SECONDS                  to say what a connection came for
  --answer SECONDS                 for a meeting's holder to take a knock up
  --silent SECONDS                 a connection that passes nothing is kept
  --away SECONDS                   a meeting is remembered after its holder's
                                   connection was lost
  --megabytes-a-day N              the meetings of one key may pass in a day
";

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(sentence) = run(&arguments) {
        eprintln!("trill-meet: {sentence}");
        std::process::exit(2);
    }
}

/// What was asked for: the words, and what each `--flag` was given.
struct Asked {
    words: Vec<String>,
    flags: Vec<(String, String)>,
}

impl Asked {
    fn read(arguments: &[String]) -> Result<Asked, String> {
        let (mut words, mut flags) = (Vec::new(), Vec::new());
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.strip_prefix("--") {
                Some(flag) => match flag.split_once('=') {
                    Some((flag, value)) => flags.push((flag.to_owned(), value.to_owned())),
                    None => match arguments.next() {
                        Some(value) => flags.push((flag.to_owned(), value.clone())),
                        None => return Err(format!("--{flag} is given something: see trill-meet help")),
                    },
                },
                None => words.push(argument.clone()),
            }
        }
        Ok(Asked { words, flags })
    }

    /// The file of keys, where no other flag is taken.
    fn keys_only(&self) -> Result<PathBuf, String> {
        let mut keys = PathBuf::from(KEYS);
        for (flag, value) in &self.flags {
            match flag.as_str() {
                "keys" => keys = value.into(),
                _ => return Err(format!("--{flag} is nothing this takes: see trill-meet help")),
            }
        }
        Ok(keys)
    }
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("--{flag} takes a number, and {value} is none"))
}

fn seconds(flag: &str, value: &str) -> Result<Duration, String> {
    let seconds: f64 = number(flag, value)?;
    match seconds.is_finite() && (0.001..=31_536_000.0).contains(&seconds) {
        true => Ok(Duration::from_secs_f64(seconds)),
        false => Err(format!("--{flag} takes a number of seconds, and {value} is none")),
    }
}

fn run(arguments: &[String]) -> Result<(), String> {
    if arguments.iter().any(|argument| argument == "--help" || argument == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let asked = Asked::read(arguments)?;
    let words: Vec<&str> = asked.words.iter().map(String::as_str).collect();
    match words[..] {
        ["serve"] => serve(&asked),
        ["key", name] => {
            let path = asked.keys_only()?;
            let key = keys::give(&path, name).map_err(|error| told(&path, &error))?;
            println!("A key for {}. It is shown this once: the file keeps what proves it, not the key.", name.trim());
            println!();
            println!("    {key}");
            println!();
            println!("In Trill it is given where a song is shared, with this meeting point's address.");
            Ok(())
        }
        ["keys"] => {
            let path = asked.keys_only()?;
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(told(&path, &error)),
            };
            let (given, wrong) = keys::read(&text);
            for key in &given {
                println!("{}  {}", key.id, key.name);
            }
            if given.is_empty() {
                println!("No keys are given in {}: trill-meet key NAME makes one.", path.display());
            }
            if !wrong.is_empty() {
                let lines: Vec<String> = wrong.iter().map(usize::to_string).collect();
                println!("Lines that are no key and are passed over: {}.", lines.join(", "));
            }
            Ok(())
        }
        ["forget", who] => {
            let path = asked.keys_only()?;
            let gone = keys::forget(&path, who).map_err(|error| told(&path, &error))?;
            match gone.is_empty() {
                true => Err(format!("no key in {} is {who}'s: trill-meet keys lists them", path.display())),
                false => {
                    println!("Taken away: the key of {}.", gone.join(", "));
                    println!("A meeting point that is running ends what it held within a few seconds.");
                    Ok(())
                }
            }
        }
        [] | ["help"] => {
            print!("{HELP}");
            Ok(())
        }
        _ => Err("that is nothing it does: see trill-meet help".to_owned()),
    }
}

fn told(path: &Path, error: &std::io::Error) -> String {
    format!("{}: {error}", path.display())
}

/// Both kinds of address on one port.
fn both(port: u16) -> Result<(Vec<TcpListener>, &'static str), String> {
    let six = TcpListener::bind((Ipv6Addr::UNSPECIFIED, port));
    let four = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port));
    match (six, four) {
        (Ok(six), Ok(four)) => Ok((vec![six, four], "")),
        // One socket takes both, as Linux has it unless it is told
        // otherwise: the second is then refused the port.
        (Ok(six), Err(_)) => Ok((vec![six], " (which takes IPv4 too, unless another program has the port for it)")),
        (Err(_), Ok(four)) => Ok((vec![four], " (this machine has no IPv6)")),
        (Err(error), Err(_)) => Err(format!("port {port} could not be listened on: {error}")),
    }
}

fn serve(asked: &Asked) -> Result<(), String> {
    let mut limits = Limits::default();
    let (mut listen, mut port, mut keys_at) = (Vec::new(), PORT, PathBuf::from(KEYS));
    for (flag, value) in &asked.flags {
        match flag.as_str() {
            "listen" => listen.push(value.clone()),
            "port" => port = number(flag, value)?,
            "keys" => keys_at = value.into(),
            "connections" => limits.connections = number(flag, value)?,
            "from-one-address" => limits.from_one_address = number(flag, value)?,
            "a-minute-from-one-address" => limits.a_minute_from_one_address = number(flag, value)?,
            "meetings" => limits.meetings = number(flag, value)?,
            "meetings-of-one-key" => limits.meetings_of_one_key = number(flag, value)?,
            "people" => limits.people = number(flag, value)?,
            "hello" => limits.hello = seconds(flag, value)?,
            "answer" => limits.answer = seconds(flag, value)?,
            "silent" => limits.silent = seconds(flag, value)?,
            "away" => limits.away = seconds(flag, value)?,
            "megabytes-a-day" => {
                let megabytes: u64 = number(flag, value)?;
                limits.bytes_a_day = megabytes.saturating_mul(1 << 20);
            }
            _ => return Err(format!("--{flag} is nothing serve takes: see trill-meet help")),
        }
    }
    limits.held_to()?;
    let (listeners, remark) = match listen.is_empty() {
        true => both(port)?,
        false => {
            let bound: Result<Vec<TcpListener>, String> = (listen.iter())
                .map(|address| {
                    let bound = TcpListener::bind(address.as_str());
                    bound.map_err(|error| format!("{address} could not be listened on: {error}"))
                })
                .collect();
            (bound?, "")
        }
    };
    let log: trill_meet::Log = Arc::new(|line: &str| println!("{} {line}", stamp()));
    let on: Vec<String> =
        listeners.iter().filter_map(|listener| Some(listener.local_addr().ok()?.to_string())).collect();
    log(&format!("trill-meet {}: a meeting point on {}{remark}", env!("CARGO_PKG_VERSION"), on.join(" and ")));
    log(&format!("its limits: {}", limits.said()));
    let given = std::fs::read_to_string(&keys_at).map(|text| keys::read(&text).0.len()).unwrap_or(0);
    match given {
        0 => log(&format!(
            "no keys are given in {}: nobody can open a meeting until trill-meet key NAME has made one",
            keys_at.display()
        )),
        1 => log(&format!("1 key is given in {}", keys_at.display())),
        given => log(&format!("{given} keys are given in {}", keys_at.display())),
    }
    if let Some(files) = open_files()
        && files < limits.files()
    {
        log(&format!(
            "the system lets this program have {files} files open, and its limits can come to {}: connections are \
             turned away before the limits are reached (LimitNOFILE in a unit, ulimit -n in a shell)",
            limits.files()
        ));
    }
    let server = Server::start(listeners, Keys::at(keys_at), limits, log).map_err(|error| error.to_string())?;
    server.run()
}

/// How many files the system lets this program have open, where it says.
fn open_files() -> Option<usize> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits.lines().find(|line| line.starts_with("Max open files"))?;
    line.split_whitespace().nth(3)?.parse().ok()
}

/// The moment, as a log's line begins: the date and the time of day, in
/// universal time.
fn stamp() -> String {
    stamp_of(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs()))
}

/// The moment `seconds` after the beginning of 1970.
fn stamp_of(seconds: u64) -> String {
    let (days, of_day) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Days since 1970 as a date of the calendar (Howard Hinnant's
    // `civil_from_days`, which is in the public domain).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", of_day / 3600, of_day / 60 % 60, of_day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_the_log_begins_with_the_day_and_the_time_it_was() {
        assert_eq!(stamp_of(0), "1970-01-01 00:00:00Z");
        assert_eq!(stamp_of(951_782_400 + 12 * 3600 + 34 * 60 + 56), "2000-02-29 12:34:56Z");
        assert_eq!(stamp_of(951_782_400 + 86_400), "2000-03-01 00:00:00Z");
        assert_eq!(stamp_of(1_795_219_199), "2026-11-20 23:59:59Z");
        assert_eq!(stamp_of(4_102_444_800), "2100-01-01 00:00:00Z");
    }

    #[test]
    fn what_is_asked_for_is_read_as_words_and_what_each_flag_was_given() {
        let asked = |words: &[&str]| Asked::read(&words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>());
        let read = asked(&["serve", "--port", "443", "--keys=/var/lib/trill-meet/keys"]).unwrap();
        assert_eq!(read.words, ["serve"]);
        let flags = [("port".to_owned(), "443".to_owned()), ("keys".to_owned(), "/var/lib/trill-meet/keys".to_owned())];
        assert_eq!(read.flags, flags);
        assert!(asked(&["serve", "--port"]).is_err(), "a flag is given something");
        assert_eq!(asked(&["key", "Ada"]).unwrap().keys_only().unwrap(), PathBuf::from(KEYS));
        assert!(asked(&["key", "Ada", "--port", "9"]).unwrap().keys_only().is_err());
        assert!(seconds("hello", "0.5").is_ok() && seconds("hello", "0").is_err() && seconds("hello", "soon").is_err());
        // Limits nobody could be served under are said before anything listens.
        let refused = |limits: Limits| limits.held_to().unwrap_err();
        assert!(refused(Limits { people: 1, ..Limits::default() }).contains("at least two people"));
        assert!(refused(Limits { silent: Duration::from_secs(5), ..Limits::default() }).contains("--silent"));
        assert!(refused(Limits { answer: Duration::from_secs(60), ..Limits::default() }).contains("--answer"));
        assert!(refused(Limits { connections: 2, ..Limits::default() }).contains("three connections"));
        assert!(Limits::default().held_to().is_ok());
        assert!(Limits::default().files() < 1024, "the default limits fit the files a program has by default");
    }
}
