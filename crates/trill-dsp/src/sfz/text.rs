//! The text of an SFZ file, taken apart: headers and `name=value` pairs,
//! with comments gone, `#define`d names filled in and `#include`d files in
//! their place.
//!
//! The format has no quoting. A value runs to the next `name=`, to the next
//! header, or to the end of its line, so a path may have spaces in it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files included deeper than this are not read: a file that includes
/// itself would otherwise never end.
const MAX_DEPTH: usize = 16;
/// The most files one instrument may be made of.
const MAX_FILES: usize = 512;

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
    /// Every file that was read, the first being the one asked for.
    pub files: Vec<PathBuf>,
}

/// Reads the SFZ file at `root`, and every file it includes, through
/// `read`. An error is the main file not being readable; everything else
/// is a problem that is noted and passed over.
pub fn tokens(root: &Path, read: &dyn Fn(&Path) -> Result<String, String>) -> Result<Text, String> {
    let source = read(root)?;
    let mut text = Text { files: vec![root.to_path_buf()], ..Text::default() };
    let mut defines = BTreeMap::new();
    file(&source, root, root, read, &mut defines, &mut text, 0);
    Ok(text)
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
            let Some(equals) = rest.find('=') else {
                text.problems.push(format!("'{}' is neither a header nor a name=value", rest.trim()));
                break;
            };
            let name = &rest[..equals];
            if let Some(space) = name.rfind(char::is_whitespace) {
                // Words before the name belong to nothing.
                text.problems.push(format!("'{}' is neither a header nor a name=value", name[..space].trim()));
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
    if depth >= MAX_DEPTH || text.files.len() >= MAX_FILES {
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
            text.files.push(path.clone());
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
        assert!(text.files.len() > 3);
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
        assert_eq!(text.problems.len(), 5, "{:?}", text.problems);
        assert!(text.problems[0].contains("'stray words' is neither"));
    }
}
