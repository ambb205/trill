//! The text of an SFZ file, taken apart: headers and `name=value` pairs,
//! with comments gone, `#define`d names filled in and `#include`d files in
//! their place. A name may also be defined by the bank file some makers
//! put beside their instruments (`bank_defines`): the one thing here that
//! looks at a folder, and not only at the files it is handed.
//!
//! The format has no quoting. A value runs to the next `name=`, to the next
//! header, or to the end of its line, so a path may have spaces in it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files included deeper than this are not read: a file that includes
/// itself would otherwise never end.
const MAX_DEPTH: usize = 16;
/// The most times one instrument may include a file. A kit recorded
/// through sixteen microphones includes the same few files hundreds of
/// times; this only bounds a file made to include without end.
const MAX_INCLUDES: usize = 20_000;

/// One piece of an SFZ file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// `<region>`, `<group>`...: the word between the brackets, lower case.
    Header(String),
    /// `name=value`: the name lower case, the value as written.
    Opcode(String, String),
}

/// What reading the text came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub tokens: Vec<Token>,
    /// What could not be made sense of, in words: a file that is not
    /// there, a line that is neither a header nor a pair.
    pub problems: Vec<String>,
    /// Every file that was read, each once, the first being the one
    /// asked for.
    pub files: Vec<PathBuf>,
    /// How many times a file was included.
    includes: usize,
}

/// Reads the SFZ file at `root`, and every file it includes, through
/// `read`. An error is the main file not being readable; everything else
/// is a problem that is noted and passed over.
pub fn tokens(root: &Path, read: &dyn Fn(&Path) -> Result<String, String>) -> Result<Text, String> {
    let source = read(root)?;
    let mut text = Text { files: vec![root.to_path_buf()], ..Text::default() };
    let mut defines = bank_defines(root, read, &mut text);
    file(&source, root, root, read, &mut defines, &mut text, 0);
    Ok(text)
}

/// What a maker's bank file defines before the instrument is read. Sets
/// made for the ARIA engine say where they keep their recordings in a
/// `*.bank.xml` beside their programs (`<Define name="$sample_dir"
/// value="../Samples"/>`), and an instrument of such a set that uses the
/// name without defining it names no file that is there. The bank files
/// in the instrument's folder and in the one above it are read; what the
/// instrument defines itself comes after, and wins.
fn bank_defines(
    root: &Path,
    read: &dyn Fn(&Path) -> Result<String, String>,
    text: &mut Text,
) -> BTreeMap<String, String> {
    let mut defines = BTreeMap::new();
    let beside = root.parent();
    for folder in [beside, beside.and_then(Path::parent)].into_iter().flatten() {
        let Ok(listing) = std::fs::read_dir(folder) else { continue };
        let mut banks: Vec<PathBuf> = listing
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                name.to_ascii_lowercase().ends_with(".bank.xml")
            })
            .collect();
        banks.sort();
        for bank in banks {
            let Ok(source) = read(&bank) else { continue };
            let found = defined_in(&source);
            if found.is_empty() {
                continue;
            }
            for (name, value) in found {
                defines.entry(name).or_insert(value);
            }
            text.files.push(bank);
        }
    }
    defines
}

/// The `<Define name="$name" value="value"/>` elements of a bank file, as
/// `(name, value)` without the `$`.
fn defined_in(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("<Define") {
        let after = &rest[at + "<Define".len()..];
        let end = after.find('>').unwrap_or(after.len());
        let element = &after[..end];
        let attribute = |name: &str| {
            let from = element.find(&format!("{name}=\""))? + name.len() + 2;
            let length = element[from..].find('"')?;
            Some(&element[from..from + length])
        };
        if let (Some(name), Some(value)) = (attribute("name"), attribute("value"))
            && let Some(name) = name.strip_prefix('$')
            && !name.is_empty()
        {
            found.push((name.to_string(), value.to_string()));
        }
        rest = &after[end..];
    }
    found
}

fn file(
    source: &str,
    path: &Path,
    root: &Path,
    read: &dyn Fn(&Path) -> Result<String, String>,
    defines: &mut BTreeMap<String, String>,
    text: &mut Text,
    depth: usize,
) {
    let mut in_comment = false;
    for line in source.lines() {
        let line = without_comments(line, &mut in_comment);
        let mut rest = line.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("#define") {
                // `#define $name value`: the value is one word.
                let mut words = after.split_whitespace();
                match (words.next(), words.next()) {
                    (Some(name), Some(value)) if name.starts_with('$') => {
                        defines.insert(name[1..].to_string(), filled(value, defines));
                        // Past the name and then the value: the value may
                        // be a part of the name too (`$TUNE10 0`).
                        let past_name = after.find(name).map_or(0, |at| at + name.len());
                        let used =
                            after[past_name..].find(value).map_or(after.len(), |at| past_name + at + value.len());
                        rest = after[used..].trim_start();
                    }
                    _ => {
                        text.problems.push(format!("'{}' defines nothing", rest.trim()));
                        break;
                    }
                }
                continue;
            }
            if let Some(after) = rest.strip_prefix("#include") {
                let after = after.trim_start();
                let Some((name, following)) = after.strip_prefix('"').and_then(|quoted| quoted.split_once('"')) else {
                    text.problems.push(format!("'{}' names no file to include", rest.trim()));
                    break;
                };
                include(&filled(name, defines), path, root, read, defines, text, depth);
                rest = following.trim_start();
                continue;
            }
            if let Some(after) = rest.strip_prefix('<') {
                let Some((name, following)) = after.split_once('>') else {
                    text.problems.push(format!("'{}' is a header that never closes", rest.trim()));
                    break;
                };
                text.tokens.push(Token::Header(name.trim().to_lowercase()));
                rest = following.trim_start();
                continue;
            }
            // A pair: the name is the word the `=` ends.
            // Words that are neither are passed over: a note somebody
            // left in the file without marking it as one.
            let Some(equals) = rest.find('=') else { break };
            let name = &rest[..equals];
            if let Some(space) = name.rfind(char::is_whitespace) {
                // Words before the name belong to nothing.
                rest = rest[space..].trim_start();
                continue;
            }
            let after = &rest[equals + 1..];
            let end = value_end(after);
            let value = after[..end].trim();
            text.tokens.push(Token::Opcode(filled(name.trim(), defines).to_lowercase(), filled(value, defines)));
            rest = after[end..].trim_start();
        }
    }
}

/// Where the value that `after` begins with ends: before the name of the
/// next pair, before a header, or at the end.
fn value_end(after: &str) -> usize {
    let header = after.find('<');
    let next = after.find('=').map(|equals| {
        // The next name is the last word before its `=`.
        after[..equals].rfind(char::is_whitespace).map_or(0, |space| space + 1)
    });
    let directive = ["#include", "#define"].iter().filter_map(|word| after.find(word)).min();
    [header, next, directive].into_iter().flatten().min().unwrap_or(after.len())
}

#[allow(clippy::too_many_arguments)]
fn include(
    name: &str,
    from: &Path,
    root: &Path,
    read: &dyn Fn(&Path) -> Result<String, String>,
    defines: &mut BTreeMap<String, String>,
    text: &mut Text,
    depth: usize,
) {
    text.includes += 1;
    if depth >= MAX_DEPTH || text.includes > MAX_INCLUDES {
        text.problems.push(format!("'{name}' is included too deep, or by itself; it was left out"));
        return;
    }
    let name = name.replace('\\', "/");
    // From the folder of the instrument's own file, as the format has it;
    // failing that from the folder of the file that asks.
    let folders = [root.parent(), from.parent()];
    let mut tried: Vec<PathBuf> = Vec::new();
    for folder in folders.into_iter().flatten() {
        let path = folder.join(&name);
        if tried.contains(&path) {
            continue;
        }
        if let Ok(source) = read(&path) {
            if !text.files.contains(&path) {
                text.files.push(path.clone());
            }
            file(&source, &path, root, read, defines, text, depth + 1);
            return;
        }
        tried.push(path);
    }
    text.problems.push(format!("the file '{name}' it includes is not there"));
}

/// `line` without what is commented out: from `//` to its end, and
/// between `/*` and `*/`, which may be lines apart.
fn without_comments(line: &str, in_comment: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        if *in_comment {
            match rest.find("*/") {
                Some(end) => {
                    rest = &rest[end + 2..];
                    *in_comment = false;
                }
                None => return out,
            }
        }
        let (line_comment, block) = (rest.find("//"), rest.find("/*"));
        match (line_comment, block) {
            (Some(line_comment), block) if block.is_none_or(|block| line_comment < block) => {
                out.push_str(&rest[..line_comment]);
                return out;
            }
            (_, Some(block)) => {
                out.push_str(&rest[..block]);
                out.push(' ');
                rest = &rest[block + 2..];
                *in_comment = true;
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// `text` with every `$name` that was defined replaced by its value. A
/// name is as many letters, digits and underscores as make a defined one.
fn filled(text: &str, defines: &BTreeMap<String, String>) -> String {
    if defines.is_empty() || !text.contains('$') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];
        let word = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
        // The longest defined name the word begins with.
        let found = (1..=word)
            .rev()
            .filter(|end| after.is_char_boundary(*end))
            .find_map(|end| defines.get(&after[..end]).map(|value| (end, value)));
        match found {
            Some((end, value)) => {
                out.push_str(value);
                rest = &after[end..];
            }
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &'static str) -> Text {
        tokens(Path::new("/i/main.sfz"), &|path| {
            if path == Path::new("/i/main.sfz") { Ok(source.to_string()) } else { Err("no such file".into()) }
        })
        .unwrap()
    }

    fn pairs(text: &Text) -> Vec<(&str, &str)> {
        text.tokens
            .iter()
            .map(|token| match token {
                Token::Header(name) => ("<>", name.as_str()),
                Token::Opcode(name, value) => (name.as_str(), value.as_str()),
            })
            .collect()
    }

    #[test]
    fn headers_and_pairs_are_read_wherever_they_stand() {
        let text = one(
            "<group> lokey=c2 hikey=48 // the low end\n<region>sample=Grand Piano/C 2 soft.wav LoVel=1\n\n  hivel=40<region> sample=b.wav",
        );
        assert_eq!(
            pairs(&text),
            [
                ("<>", "group"),
                ("lokey", "c2"),
                ("hikey", "48"),
                ("<>", "region"),
                ("sample", "Grand Piano/C 2 soft.wav"),
                ("lovel", "1"),
                ("hivel", "40"),
                ("<>", "region"),
                ("sample", "b.wav"),
            ]
        );
        assert!(text.problems.is_empty(), "{:?}", text.problems);
    }

    #[test]
    fn comments_are_gone_whether_they_end_a_line_or_span_several() {
        let text = one(
            "<region> /* not\nthis key=1\nat all */ key=60 /* nor this */ sample=a.wav // c4\n// <region>\nvolume=-3",
        );
        assert_eq!(pairs(&text), [("<>", "region"), ("key", "60"), ("sample", "a.wav"), ("volume", "-3")]);
    }

    #[test]
    fn defined_names_are_filled_in_by_the_longest_that_fits() {
        let text = one(
            "#define $KEY 36\n#define $KEY2 38 #define $DIR Drums\n#define $T10 0\n#define $T01 0\n<region> key=$KEY sample=$DIR/kick_$KEY.wav\n<region> key=$KEY2 locc$KEY=1 sample=$NOPE.wav",
        );
        assert!(text.problems.is_empty(), "a value that is part of its name is still the value: {:?}", text.problems);
        assert_eq!(
            pairs(&text),
            [
                ("<>", "region"),
                ("key", "36"),
                ("sample", "Drums/kick_36.wav"),
                ("<>", "region"),
                ("key", "38"),
                ("locc36", "1"),
                ("sample", "$NOPE.wav"),
            ]
        );
    }

    #[test]
    fn included_files_stand_where_they_are_included() {
        let read = |path: &Path| match path.to_string_lossy().replace('\\', "/").as_str() {
            "/i/main.sfz" => {
                Ok("<control> default_path=samples/\n#include \"parts/low.sfz\"\n<region> key=72 #include \"gone.sfz\""
                    .to_string())
            }
            "/i/parts/low.sfz" => {
                Ok("#define $ROOT 36\n<region> key=$ROOT\n#include \"parts/deep.sfz\" #include \"near.sfz\""
                    .to_string())
            }
            "/i/parts/deep.sfz" => Ok("<region> key=24 #include \"parts/deep.sfz\"".to_string()),
            "/i/parts/near.sfz" => Ok("<region> key=25".to_string()),
            _ => Err("no such file".to_string()),
        };
        let text = tokens(Path::new("/i/main.sfz"), &read).unwrap();
        let keys: Vec<&str> =
            pairs(&text).into_iter().filter(|(name, _)| *name == "key").map(|(_, value)| value).collect();
        // From the folder of the main file, and failing that from the folder of the file that asks.
        assert_eq!(keys[..3], ["36", "24", "24"]);
        assert_eq!(*keys.last().unwrap(), "72");
        assert!(keys.contains(&"25"));
        assert!(text.problems.iter().any(|problem| problem.contains("'gone.sfz' it includes is not there")));
        assert!(text.problems.iter().any(|problem| problem.contains("included too deep")), "{:?}", text.problems);
        assert_eq!(text.files[0], Path::new("/i/main.sfz"));
        assert_eq!(text.files.len(), 4, "each file once, however often it is included");
        assert!(tokens(Path::new("/i/none.sfz"), &read).is_err());
    }

    #[test]
    fn what_makes_no_sense_is_said_and_passed_over() {
        let text = one(
            "<region> stray words key=60 sample=a.wav\njust words\n<group\n#include nothing\n#define $X\n<region> key=61",
        );
        let keys: Vec<&str> =
            pairs(&text).into_iter().filter(|(name, _)| *name == "key").map(|(_, value)| value).collect();
        assert_eq!(keys, ["60", "61"]);
        // Words that are in the way are passed over without a word; what is broken is said.
        assert_eq!(text.problems.len(), 3, "{:?}", text.problems);
        assert!(text.problems[0].contains("is a header that never closes"));
    }

    #[test]
    fn a_name_the_maker_s_bank_file_defines_is_filled_in_and_the_instrument_s_own_wins() {
        let bank = "<?xml version=\"1.0\" ?>\n<AriaBank id=\"1\" name=\"Set\">\n\t<Define name=\"$sample_dir\" \
                    value=\"../Samples\"/>\n  <Define value=\"8\" name=\"$voices\" />\n<Define name=\"plain\" \
                    value=\"x\"/>\n<AriaProgram name=\"Set\"><AriaElement path=\"Programs/main.sfz\"/></AriaProgram>\n\
                    </AriaBank>\n";
        assert_eq!(
            defined_in(bank),
            [("sample_dir".to_string(), "../Samples".to_string()), ("voices".to_string(), "8".to_string())]
        );
        assert!(defined_in("<AriaBank><Define name=\"$broken\"").is_empty());

        // On a disk, as a set lies: the bank beside the folder of its programs.
        let folder = tempfile::tempdir().unwrap();
        let programs = folder.path().join("Set").join("Programs");
        std::fs::create_dir_all(&programs).unwrap();
        let bank_file = folder.path().join("Set").join("Set.bank.xml");
        std::fs::write(&bank_file, bank).unwrap();
        std::fs::write(folder.path().join("Set").join("notes.xml"), "<Define name=\"$sample_dir\" value=\"no\"/>")
            .unwrap();
        let main = programs.join("main.sfz");
        std::fs::write(&main, "<control> default_path=$sample_dir/\n<region> sample=a.wav polyphony=$voices").unwrap();
        let own = programs.join("own.sfz");
        std::fs::write(&own, "#define $sample_dir Mine\n<control> default_path=$sample_dir/\n<region> sample=a.wav")
            .unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).map_err(|error| error.to_string());

        let text = tokens(&main, &read).unwrap();
        assert_eq!(
            pairs(&text),
            [
                ("<>", "control"),
                ("default_path", "../Samples/"),
                ("<>", "region"),
                ("sample", "a.wav"),
                ("polyphony", "8")
            ]
        );
        assert_eq!(text.files, [main.clone(), bank_file.clone()], "the bank is one of the files it is made of");
        assert!(text.problems.is_empty(), "{:?}", text.problems);
        // What the instrument defines itself is what it means.
        let text = tokens(&own, &read).unwrap();
        assert!(pairs(&text).contains(&("default_path", "Mine/")), "{:?}", pairs(&text));
        // A set without a bank is read as it always was, and a name nobody defined stays as it is written.
        std::fs::remove_file(&bank_file).unwrap();
        let text = tokens(&main, &read).unwrap();
        assert!(pairs(&text).contains(&("default_path", "$sample_dir/")), "{:?}", pairs(&text));
        assert_eq!(text.files, [main]);
    }
}
