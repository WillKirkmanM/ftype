use clap::Parser;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Inspect and compare saved Windows file type handlers"
)]
struct Arguments {
    #[arg(long, value_name = "PATH", help = "Read a saved ftype snapshot")]
    input: PathBuf,
    #[arg(
        long,
        value_name = "PATTERN",
        conflicts_with = "file_type",
        help = "Filter names using case-insensitive * and ? patterns"
    )]
    filter: Option<String>,
    #[arg(
        long = "type",
        value_name = "NAME",
        help = "Select a handler by its case-insensitive name"
    )]
    file_type: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Compare the input snapshot with a newer snapshot"
    )]
    compare: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
struct Handler {
    name: String,
    command: String,
    line: usize,
}

type Snapshot = BTreeMap<String, Handler>;

#[derive(Debug, PartialEq, Eq)]
struct RecordError {
    line: usize,
    message: String,
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

fn parse_snapshot(text: &str) -> Result<Snapshot, RecordError> {
    let mut handlers = Snapshot::new();
    for (index, line) in text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .enumerate()
    {
        let number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let fail = |message: String| RecordError {
            line: number,
            message,
        };
        let (name, command) = line
            .split_once('=')
            .ok_or_else(|| fail("missing '=' separator".into()))?;
        let name = name.trim();
        if name.is_empty() {
            return Err(fail("empty file type name".into()));
        }
        if command.trim().is_empty() {
            return Err(fail(format!("empty command for '{name}'")));
        }
        let key = name.to_ascii_lowercase();
        if let Some(previous) = handlers.get(&key) {
            return Err(fail(format!(
                "duplicate file type '{name}' (first recorded on line {})",
                previous.line
            )));
        }
        handlers.insert(
            key,
            Handler {
                name: name.into(),
                command: command.into(),
                line: number,
            },
        );
    }
    Ok(handlers)
}

fn read_snapshot(path: &Path) -> Result<Snapshot, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
    parse_snapshot(&text).map_err(|error| format!("'{}': {error}", path.display()))
}

fn matches_pattern(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let name: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    let mut restart = 0;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            restart = n;
        } else if let Some(position) = star {
            restart += 1;
            n = restart;
            p = position + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[derive(Debug, PartialEq, Eq)]
enum Change<'a> {
    Added(&'a Handler),
    Removed(&'a Handler),
    Changed(&'a Handler, &'a Handler),
}

fn changes<'a>(old: &'a Snapshot, new: &'a Snapshot) -> Vec<(&'a str, Change<'a>)> {
    let keys: BTreeSet<&str> = old.keys().chain(new.keys()).map(String::as_str).collect();
    keys.into_iter()
        .filter_map(|key| {
            let change = match (old.get(key), new.get(key)) {
                (None, Some(handler)) => Change::Added(handler),
                (Some(handler), None) => Change::Removed(handler),
                (Some(before), Some(after)) if before.command != after.command => {
                    Change::Changed(before, after)
                }
                _ => return None,
            };
            Some((key, change))
        })
        .collect()
}

fn inspect(arguments: &Arguments, output: &mut impl Write) -> Result<(), String> {
    let old = read_snapshot(&arguments.input)?;
    let new = arguments
        .compare
        .as_deref()
        .map(read_snapshot)
        .transpose()?;
    let selected = arguments
        .file_type
        .as_ref()
        .map(|name| name.to_ascii_lowercase());
    if let Some(key) = &selected
        && !old.contains_key(key)
        && !new
            .as_ref()
            .is_some_and(|snapshot| snapshot.contains_key(key))
    {
        return Err(format!(
            "file type '{}' was not found",
            arguments.file_type.as_deref().unwrap_or_default()
        ));
    }
    let includes = |key: &str| {
        selected.as_ref().is_none_or(|name| name == key)
            && arguments
                .filter
                .as_ref()
                .is_none_or(|pattern| matches_pattern(pattern, key))
    };
    let write_result = if let Some(new) = &new {
        let mut count = 0;
        for (key, change) in changes(&old, new) {
            if !includes(key) {
                continue;
            }
            count += 1;
            match change {
                Change::Added(handler) => {
                    writeln!(output, "Added {}\n  New: {}", handler.name, handler.command)
                }
                Change::Removed(handler) => writeln!(
                    output,
                    "Removed {}\n  Old: {}",
                    handler.name, handler.command
                ),
                Change::Changed(before, after) => writeln!(
                    output,
                    "Changed {}\n  Old: {}\n  New: {}",
                    after.name, before.command, after.command
                ),
            }
            .map_err(|error| format!("cannot write output: {error}"))?;
        }
        if count == 0 {
            writeln!(output, "No changes")
        } else {
            Ok(())
        }
    } else {
        for (key, handler) in &old {
            if includes(key) {
                writeln!(output, "{}={}", handler.name, handler.command)
                    .map_err(|error| format!("cannot write output: {error}"))?;
            }
        }
        Ok(())
    };
    write_result.map_err(|error| format!("cannot write output: {error}"))
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let mut output = io::BufWriter::new(io::stdout().lock());
    match inspect(&arguments, &mut output).and_then(|()| {
        output
            .flush()
            .map_err(|error| format!("cannot write output: {error}"))
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_commands_and_orders_names() {
        let snapshot = parse_snapshot("\u{feff}txtfile=\"C:\\Windows\\notepad.exe\" \"%1\" key=value  \r\n\nhtmlfile=browser %*\n").unwrap();
        assert_eq!(
            snapshot.keys().map(String::as_str).collect::<Vec<_>>(),
            ["htmlfile", "txtfile"]
        );
        assert_eq!(
            snapshot["txtfile"].command,
            "\"C:\\Windows\\notepad.exe\" \"%1\" key=value  "
        );
    }

    #[test]
    fn rejects_invalid_records_with_line_numbers() {
        for (text, line, message) in [
            ("\ninvalid", 2, "missing '=' separator"),
            (" =command", 1, "empty file type name"),
            ("txtfile=\t ", 1, "empty command for 'txtfile'"),
            (
                "TxtFile=first\n\ntxtfile=second",
                3,
                "duplicate file type 'txtfile' (first recorded on line 1)",
            ),
        ] {
            assert_eq!(
                parse_snapshot(text).unwrap_err(),
                RecordError {
                    line,
                    message: message.into()
                }
            );
        }
    }

    #[test]
    fn wildcard_matching_handles_backtracking_and_character_width() {
        for (pattern, name, expected) in [
            ("*FILE", "TxtFile", true),
            ("t?t*", "txtfile", true),
            ("*ab?c", "ababxc", true),
            ("?", "é", true),
            ("", "", true),
            ("*", "", true),
            ("txt?", "txt", false),
            ("*.txt", "txtfile", false),
        ] {
            assert_eq!(
                matches_pattern(pattern, name),
                expected,
                "{pattern}: {name}"
            );
        }
    }

    #[test]
    fn compares_commands_exactly_and_names_without_ascii_case_sensitivity() {
        let old = parse_snapshot("Same=unchanged\nChanged=old\nRemoved=gone\n").unwrap();
        let new = parse_snapshot("same=unchanged\nchanged=new\nAdded=fresh\n").unwrap();
        assert_eq!(
            changes(&old, &new),
            vec![
                ("added", Change::Added(&new["added"])),
                ("changed", Change::Changed(&old["changed"], &new["changed"])),
                ("removed", Change::Removed(&old["removed"])),
            ]
        );
    }
}
