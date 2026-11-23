//! The agent skill, `skill/SKILL.md`, ships with the binary it describes
//! (LOCL-9). It names the workspace version, which is the one the binary
//! prints, and every command in its examples is one this binary has, with
//! the flags the example gives it.

mod common;

use common::Env;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SKILL: &str = include_str!("../../../skill/SKILL.md");

/// The skill states the version in both places an agent reads it: the
/// description it is chosen by, and the line it checks the binary with.
#[test]
fn the_skill_names_the_workspace_version() {
    let description = SKILL
        .lines()
        .find(|l| l.starts_with("description: "))
        .expect("a description in the front matter");
    assert!(
        description.contains(&format!("CLI ({VERSION})")),
        "the skill's description doesn't name {VERSION}: {description}"
    );
    let check = format!("`lcl --version` prints `lcl {VERSION}`");
    assert!(
        SKILL.contains(&check),
        "the skill doesn't say {check}; bump skill/SKILL.md with the version"
    );
    let out = Env::new().lcl(&["--version"]);
    assert_eq!(out.assert_ok().stdout, format!("lcl {VERSION}\n"));
}

/// Splits a shell line into words: whitespace outside quotes separates,
/// and quotes are dropped. Enough for the skill's examples.
fn words(line: &str) -> Vec<String> {
    let (mut out, mut word, mut quote, mut any) = (vec![], String::new(), None, false);
    for c in line.chars() {
        match (quote, c) {
            (None, '"' | '\'') => (quote, any) = (Some(c), true),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if any {
                    out.push(std::mem::take(&mut word));
                }
                any = false;
            }
            (_, c) => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

/// Each `lcl` call in the skill's examples, as words after `lcl`.
fn example_calls() -> Vec<Vec<String>> {
    let mut calls = vec![];
    let mut in_bash = false;
    for line in SKILL.lines() {
        match line.trim() {
            "```bash" => in_bash = true,
            "```" => in_bash = false,
            text if in_bash => {
                let text = text.split(" #").next().unwrap_or(text);
                for part in text.split('|') {
                    let w = words(part);
                    if w.first().map(String::as_str) == Some("lcl") {
                        calls.push(w[1..].to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    calls
}

/// Every example names a command this binary has, and every flag it passes
/// is one that command's help lists. A renamed command or flag fails here
/// until the skill is updated.
#[test]
fn every_example_in_the_skill_is_a_command_this_binary_has() {
    let calls = example_calls();
    assert!(calls.len() > 40, "found only {} examples", calls.len());
    let env = Env::new();
    for call in &calls {
        let mut rest = call.iter().map(String::as_str).peekable();
        // Global options come first in the examples.
        let mut path = vec![];
        while let Some(&w) = rest.peek() {
            match w {
                "--json" => {
                    rest.next();
                }
                "-w" | "--workspace" => {
                    rest.next();
                    rest.next();
                }
                _ => break,
            }
        }
        while let Some(&w) = rest.peek() {
            if path.len() == 2 || !w.chars().all(|c| c.is_ascii_lowercase()) {
                break;
            }
            path.push(w);
            rest.next();
        }
        let mut args = path.clone();
        args.push("--help");
        let out = env.lcl(&args);
        // A word after the command, such as a search word, can be taken
        // for a subcommand; one word fewer must then be the command.
        let (out, path) = if out.code != 0 && path.len() == 2 {
            let one = vec![path[0], "--help"];
            (env.lcl(&one), vec![path[0]])
        } else {
            (out, path)
        };
        assert_eq!(
            out.code,
            0,
            "lcl {}: no such command\n{out:#?}",
            call.join(" ")
        );
        for flag in call
            .iter()
            .filter(|w| w.starts_with("--") && *w != "--json")
        {
            if flag == "--" {
                continue;
            }
            assert!(
                out.stdout
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .any(|w| w == flag),
                "lcl {}: {flag} is not in `lcl {} --help`",
                call.join(" "),
                path.join(" ")
            );
        }
    }
}
