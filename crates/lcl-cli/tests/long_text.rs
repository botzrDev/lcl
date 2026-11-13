//! Long text from a file or stdin (prove-out D1 and D2).
//!
//! Inline text passes through the shell, which expands `$(...)` and
//! backticks inside double quotes, and through argv, which has a size limit.
//! So descriptions, comments and doc content can also come from a file:
//! `--description-file`, `--body-file`, `--content-file`. A PATH of `-` reads
//! stdin to EOF, and is refused when stdin is a terminal. The text is stored
//! byte for byte. Invalid UTF-8, an unreadable file, a file flag together
//! with its inline flag, an inline `-`, and a blank comment all exit 2 and
//! change nothing. stdin that stays silent (a pipe or socket that never
//! writes or closes) is refused after LCL_STDIN_WAIT seconds (default 10;
//! 0 waits forever); once it starts, the rest is read with no limit.

mod common;

use common::{dump, issue_row, Env, Out};
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// A word that appears once, at the very end of `long_body()`.
const END_WORD: &str = "zanzibarquux";

/// Just over 1 MiB of markdown that a shell would mangle: backticks,
/// `$(rm -rf /)`, both kinds of quote, a table, tabs, CRLF lines, non-ASCII,
/// and a trailing newline.
fn long_body() -> String {
    let block = concat!(
        "## Verify report: `cargo test` \"double\" 'single' $(rm -rf /) and `$(echo pwned)`\r\n",
        "| check | result | note |\r\n",
        "|-------|--------|------|\r\n",
        "| caf\u{e9}\tok | \u{65e5}\u{672c} | \u{1f980} shipped |\r\n",
        "\tindented with a tab; a backslash \\ and $HOME and ${PATH}\n",
        "\n",
    );
    let mut s = String::with_capacity((1 << 20) + 4096);
    while s.len() < 1 << 20 {
        s.push_str(block);
    }
    s.push_str(&format!("The last line names {END_WORD}.\n"));
    s
}

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    env
}

/// Writes `bytes` to a file under the env's HOME (not its cwd).
fn write(env: &Env, name: &str, bytes: &[u8]) -> PathBuf {
    let path = env.home.path().join(name);
    std::fs::write(&path, bytes).expect("write text file");
    path
}

fn s(p: &Path) -> &str {
    p.to_str().expect("utf-8 temp path")
}

/// Runs `args` with `{}` replaced by `path`, or by `-` with `text` piped to
/// stdin.
fn send(env: &Env, args: &[&str], text: &[u8], path: &Path, stdin: bool) -> Out {
    let p = if stdin { "-" } else { s(path) };
    let args: Vec<String> = args.iter().map(|a| a.replace("{}", p)).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if stdin { env.lcl_stdin(&args, text) } else { env.lcl(&args) }
}

/// Byte-exact comparison that reports sizes and the first difference
/// instead of printing a megabyte.
#[track_caller]
fn assert_same(got: &Value, want: &str, what: &str) {
    let got = got.as_str().unwrap_or_else(|| panic!("{what}: not a string: {got}"));
    if got != want {
        let at = got.bytes().zip(want.bytes()).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
        panic!("{what}: stored {} bytes, sent {}; first difference at byte {at}", got.len(), want.len());
    }
}

/// A doc's id, from `doc create --json`.
fn create_doc(env: &Env) -> String {
    let d = env.lcl(&["doc", "create", "--title", "d", "--content", "short", "--json"]).assert_ok().json();
    d["id"].as_str().expect("doc id").to_string()
}

#[test]
fn long_text_round_trips_byte_for_byte() {
    let env = workspace();
    let body = long_body();
    let body2 = format!("updated\r\n{body}");
    let (f1, f2) = (write(&env, "body.md", body.as_bytes()), write(&env, "body2.md", body2.as_bytes()));

    for (route, stdin) in [("file", false), ("stdin", true)] {
        let issue = send(&env, &["issue", "create", "--title", "long", "--description-file", "{}", "--json"], body.as_bytes(), &f1, stdin);
        let issue = issue.assert_ok().json();
        assert_same(&issue["description"], &body, &format!("issue create, {route}"));
        let n = issue["number"].as_i64().expect("number").to_string();
        let shown = env.lcl(&["issue", "show", &n, "--json"]).assert_ok().json();
        assert_same(&shown["issue"]["description"], &body, &format!("issue show after create, {route}"));

        let updated = send(&env, &["issue", "update", &n, "--description-file", "{}", "--json"], body2.as_bytes(), &f2, stdin);
        assert_same(&updated.assert_ok().json()["description"], &body2, &format!("issue update, {route}"));
        let shown = env.lcl(&["issue", "show", &n, "--json"]).assert_ok().json();
        assert_same(&shown["issue"]["description"], &body2, &format!("issue show after update, {route}"));

        let comment = send(&env, &["issue", "comment", &n, "--body-file", "{}", "--json"], body.as_bytes(), &f1, stdin);
        assert_same(&comment.assert_ok().json()["body"], &body, &format!("issue comment, {route}"));
        let comments = env.lcl(&["issue", "comments", &n, "--json"]).assert_ok().json();
        assert_same(&comments[0]["body"], &body, &format!("issue comments, {route}"));

        let doc = send(&env, &["doc", "create", "--title", "long", "--content-file", "{}", "--json"], body.as_bytes(), &f1, stdin);
        let doc = doc.assert_ok().json();
        assert_same(&doc["content"], &body, &format!("doc create, {route}"));
        let id = doc["id"].as_str().expect("doc id");
        assert_same(&env.lcl(&["doc", "show", id, "--json"]).assert_ok().json()["content"], &body, &format!("doc show after create, {route}"));
        let updated = send(&env, &["doc", "update", id, "--content-file", "{}", "--json"], body2.as_bytes(), &f2, stdin);
        assert_same(&updated.assert_ok().json()["content"], &body2, &format!("doc update, {route}"));
        assert_same(&env.lcl(&["doc", "show", id, "--json"]).assert_ok().json()["content"], &body2, &format!("doc show after update, {route}"));
    }
}

#[test]
fn search_finds_a_word_at_the_end_of_a_long_description() {
    let env = workspace();
    let body = long_body();
    let at = body.find(END_WORD).expect("end word");
    assert!(body.matches(END_WORD).count() == 1 && at > body.len() - 64, "the word must appear once, at the end");
    env.create("decoy");
    let f = write(&env, "body.md", body.as_bytes());
    env.lcl(&["issue", "create", "--title", "long", "--description-file", s(&f)]).assert_ok();

    let found = env.lcl(&["search", END_WORD, "--json"]).assert_ok().json();
    let ids: Vec<_> = found["issues"].as_array().expect("issues").iter().map(|i| i["identifier"].clone()).collect();
    assert_eq!(ids, ["DREA-2"]);
}

#[test]
fn file_flags_conflict_with_inline_flags() {
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let f = write(&env, "t.md", b"text\n");
    let f = s(&f);
    let db = env.db_path();
    let before = dump(&db);

    for args in [
        vec!["issue", "create", "--title", "x", "--description", "a", "--description-file", f],
        vec!["issue", "update", "1", "--description", "a", "--description-file", f],
        vec!["issue", "comment", "1", "a", "--body-file", f],
        vec!["doc", "create", "--title", "x", "--content", "a", "--content-file", f],
        vec!["doc", "update", &doc, "--content", "a", "--content-file", f],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains("cannot be used with"), "{args:?}: {out:#?}");
    }
    // A comment needs exactly one of BODY and --body-file.
    let out = env.lcl(&["issue", "comment", "1"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(out.stderr.contains("required arguments were not provided") && out.stderr.contains("<BODY|--body-file <PATH>>"), "{out:#?}");
    assert_eq!(dump(&db), before);
}

#[test]
fn invalid_utf8_exits_2_with_the_offset() {
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let bad = b"fine so far\xff\xfe and after\n";
    let f = write(&env, "bad.md", bad);
    let db = env.db_path();
    let before = dump(&db);

    for stdin in [false, true] {
        let name = if stdin { "stdin" } else { s(&f) };
        for args in [
            vec!["issue", "create", "--title", "x", "--description-file", "{}"],
            vec!["issue", "update", "1", "--description-file", "{}"],
            vec!["issue", "comment", "1", "--body-file", "{}"],
            vec!["doc", "create", "--title", "x", "--content-file", "{}"],
            vec!["doc", "update", &doc, "--content-file", "{}"],
        ] {
            let out = send(&env, &args, bad, &f, stdin);
            assert_eq!(out.code, 2, "{args:?} stdin={stdin}: {out:#?}");
            assert!(out.stderr.contains(&format!("{name} is not valid UTF-8: invalid byte at offset 11")), "{args:?}: {out:#?}");
        }
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn missing_or_unreadable_file_exits_2() {
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let missing = env.home.path().join("no such file.md");
    let dir = env.home.path().to_path_buf();
    let db = env.db_path();
    let before = dump(&db);

    for path in [s(&missing), s(&dir)] {
        for args in [
            vec!["issue", "create", "--title", "x", "--description-file", path],
            vec!["issue", "update", "1", "--description-file", path],
            vec!["issue", "comment", "1", "--body-file", path],
            vec!["doc", "create", "--title", "x", "--content-file", path],
            vec!["doc", "update", &doc, "--content-file", path],
        ] {
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&format!("can't read {path}: ")), "{args:?}: {out:#?}");
        }
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn an_inline_dash_exits_2() {
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let db = env.db_path();
    let before = dump(&db);

    for (args, hint) in [
        (vec!["issue", "create", "--title", "x", "--description", "-"], "--description-file -"),
        (vec!["issue", "update", "1", "--description", "-"], "--description-file -"),
        (vec!["issue", "update", "1", "--description=-"], "--description-file -"),
        (vec!["issue", "comment", "1", "-"], "--body-file -"),
        (vec!["doc", "create", "--title", "x", "--content", "-"], "--content-file -"),
        (vec!["doc", "update", &doc, "--content", "-"], "--content-file -"),
    ] {
        // Even with text on stdin: an inline `-` is never read as stdin.
        let out = env.lcl_stdin(&args, b"from stdin\n");
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(&format!("to read stdin, use {hint}")), "{args:?}: {out:#?}");
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn a_blank_comment_exits_2() {
    let env = workspace();
    env.create("one");
    let empty = write(&env, "empty.md", b"");
    let blank = write(&env, "blank.md", b"\r\n \t\n");
    let db = env.db_path();
    let before = dump(&db);

    let file_msg = |p: &Path| format!("{} is blank; a comment can't be blank", s(p));
    let stdin_msg = "stdin is blank; a comment can't be blank".to_string();
    let outs = [
        ("inline empty", env.lcl(&["issue", "comment", "1", ""]), "a comment can't be blank".to_string()),
        ("inline blank", env.lcl(&["issue", "comment", "1", " \n\t"]), "a comment can't be blank".to_string()),
        ("empty file", env.lcl(&["issue", "comment", "1", "--body-file", s(&empty)]), file_msg(&empty)),
        ("blank file", env.lcl(&["issue", "comment", "1", "--body-file", s(&blank)]), file_msg(&blank)),
        ("empty pipe", env.lcl_stdin(&["issue", "comment", "1", "--body-file", "-"], b""), stdin_msg.clone()),
        ("blank pipe", env.lcl_stdin(&["issue", "comment", "1", "--body-file", "-"], b"   \n"), stdin_msg.clone()),
        ("/dev/null", env.lcl_devnull(&["issue", "comment", "1", "--body-file", "-"]), stdin_msg),
    ];
    for (route, out, msg) in outs {
        assert_eq!(out.code, 2, "{route}: {out:#?}");
        assert!(out.stderr.contains(&msg), "{route}: {out:#?}");
    }
    assert_eq!(dump(&db), before);

    // Not blank: stored byte for byte, surrounding whitespace and all.
    for (route, out) in [
        ("inline", env.lcl(&["issue", "comment", "1", "\n  hi  \n", "--json"])),
        ("pipe", env.lcl_stdin(&["issue", "comment", "1", "--body-file", "-", "--json"], b"\n  hi  \n")),
    ] {
        assert_eq!(out.assert_ok().json()["body"], "\n  hi  \n", "{route}");
    }
}

#[test]
fn empty_input_clears_a_description_or_content() {
    let env = workspace();
    let db = env.db_path();
    let text = |n: i64| issue_row(&db, n)["description"].clone();

    // stdin from /dev/null: not a terminal, and empty.
    let created = env.lcl_devnull(&["issue", "create", "--title", "a", "--description-file", "-", "--json"]).assert_ok().json();
    assert_eq!(created["description"], "");
    env.lcl(&["issue", "create", "--title", "b", "--description", "was here"]).assert_ok();
    env.lcl_devnull(&["issue", "update", "2", "--description-file", "-"]).assert_ok();
    assert_eq!((text(1), text(2)), (SqlValue::Text(String::new()), SqlValue::Text(String::new())));

    // An empty file does the same, for docs too.
    let empty = write(&env, "empty.md", b"");
    env.lcl(&["issue", "update", "2", "--description", "again"]).assert_ok();
    env.lcl(&["issue", "update", "2", "--description-file", s(&empty)]).assert_ok();
    assert_eq!(text(2), SqlValue::Text(String::new()));
    let doc = create_doc(&env);
    let d = env.lcl(&["doc", "update", &doc, "--content-file", s(&empty), "--json"]).assert_ok().json();
    assert_eq!(d["content"], "");
    let d = env.lcl_devnull(&["doc", "create", "--title", "e", "--content-file", "-", "--json"]).assert_ok().json();
    assert_eq!(d["content"], "");
}

/// stdin on a terminal is refused at once rather than waited on. Needs
/// util-linux `script` to give lcl a pseudo-terminal.
#[test]
fn a_terminal_on_stdin_exits_2() {
    if std::process::Command::new("script").arg("--version").output().is_err() {
        eprintln!("SKIPPED: no `script` binary to provide a terminal");
        return;
    }
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let db = env.db_path();
    let before = dump(&db);
    let lcl = env!("CARGO_BIN_EXE_lcl");
    for args in [
        "issue create --title x --description-file -".to_string(),
        "issue comment 1 --body-file -".to_string(),
        format!("doc update {doc} --content-file -"),
    ] {
        // `timeout` turns a hang (lcl waiting on the terminal) into a failure.
        let out = env
            .command("timeout")
            .args(["20", "script", "-qec", &format!("'{lcl}' {args}"), "/dev/null"])
            .stdin(Stdio::null())
            .output()
            .expect("run script");
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(2), "{args}: {text}");
        assert!(text.contains("nothing is piped to stdin"), "{args}: {text}");
    }
    assert_eq!(dump(&db), before);
}

/// What `run_feeding` writes to lcl's stdin.
enum Feed {
    /// Keep the pipe open and never write, until lcl exits.
    Silent,
    /// After each delay, write the bytes; then close the pipe.
    Chunks(Vec<(Duration, Vec<u8>)>),
}

/// Runs lcl with LCL_STDIN_WAIT=`wait` and stdin a pipe fed by `feed`.
/// Kills lcl and fails after 15 s, so a hang can't stall the suite. Returns
/// the output and how long lcl ran.
fn run_feeding(env: &Env, args: &[&str], wait: &str, feed: Feed) -> (Out, Duration) {
    let started = Instant::now();
    let mut child = env
        .command(env!("CARGO_BIN_EXE_lcl"))
        .args(args)
        .env("LCL_STDIN_WAIT", wait)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lcl");
    let mut pipe = child.stdin.take();
    let (mut stdout, mut stderr) = (child.stdout.take().expect("stdout"), child.stderr.take().expect("stderr"));
    std::thread::scope(|scope| {
        if let Feed::Chunks(chunks) = feed {
            let mut pipe = pipe.take().expect("stdin pipe");
            scope.spawn(move || {
                for (delay, bytes) in chunks {
                    std::thread::sleep(delay);
                    if pipe.write_all(&bytes).is_err() {
                        return; // lcl exited early; its exit code says why
                    }
                }
            });
        }
        let out = scope.spawn(move || {
            let mut b = Vec::new();
            let _ = stdout.read_to_end(&mut b);
            b
        });
        let err = scope.spawn(move || {
            let mut b = Vec::new();
            let _ = stderr.read_to_end(&mut b);
            b
        });
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll lcl") {
                break status;
            }
            if started.elapsed() > Duration::from_secs(15) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("lcl {args:?} still running after 15 s (hung on stdin?); killed");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let elapsed = started.elapsed();
        drop(pipe); // a Silent pipe stayed open until here
        let text = |h: std::thread::ScopedJoinHandle<'_, Vec<u8>>| String::from_utf8_lossy(&h.join().expect("reader")).into_owned();
        let out = Out { code: status.code().expect("exit code"), stdout: text(out), stderr: text(err) };
        (out, elapsed)
    })
}

/// An agent's shell can hand lcl a stdin that never writes and never
/// closes. lcl gives up after LCL_STDIN_WAIT seconds instead of hanging.
#[test]
fn silent_stdin_exits_2_after_the_wait() {
    let env = workspace();
    env.create("one");
    let doc = create_doc(&env);
    let db = env.db_path();
    let before = dump(&db);
    for (args, flag) in [
        (vec!["issue", "update", "1", "--description-file", "-"], "--description-file"),
        (vec!["issue", "comment", "1", "--body-file", "-"], "--body-file"),
        (vec!["doc", "update", &doc, "--content-file", "-"], "--content-file"),
    ] {
        let (out, took) = run_feeding(&env, &args, "1", Feed::Silent);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        let msg = format!("nothing arrived on stdin within 1 s; pipe the text in or use a heredoc: … {flag} - <<'EOF'");
        assert!(out.stderr.contains(&msg), "{args:?}: {out:#?}");
        assert!(took < Duration::from_secs(5), "{args:?} took {took:?}");
    }
    let (out, _) = run_feeding(&env, &["issue", "update", "1", "--description-file", "-"], "soon", Feed::Silent);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(out.stderr.contains("LCL_STDIN_WAIT must be a whole number of seconds (0 waits forever), not \"soon\""), "{out:#?}");
    assert_eq!(dump(&db), before);
}

#[test]
fn a_late_first_byte_within_the_wait_is_read() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    let text = |n: i64| issue_row(&db, n)["description"].clone();
    let late = Feed::Chunks(vec![(Duration::from_millis(500), b"late text\n".to_vec())]);
    let (out, _) = run_feeding(&env, &["issue", "update", "1", "--description-file", "-"], "2", late);
    assert_eq!(out.code, 0, "{out:#?}");
    assert_eq!(text(1), SqlValue::Text("late text\n".into()));
    // 0 waits forever: 1.5 s of silence is fine.
    let later = Feed::Chunks(vec![(Duration::from_millis(1500), b"later\n".to_vec())]);
    let (out, _) = run_feeding(&env, &["issue", "update", "1", "--description-file", "-"], "0", later);
    assert_eq!(out.code, 0, "{out:#?}");
    assert_eq!(text(1), SqlValue::Text("later\n".into()));
}

/// The wait covers only the first byte: a producer may pause mid-stream
/// for longer than LCL_STDIN_WAIT.
#[test]
fn a_pause_after_the_first_chunk_is_waited_out() {
    let env = workspace();
    env.create("one");
    let body = long_body();
    // Split inside a multi-byte character, as a pipe may.
    let cut = (100_000..).find(|&i| !body.is_char_boundary(i)).expect("a split inside a character");
    let (a, b) = body.as_bytes().split_at(cut);
    let paused = Feed::Chunks(vec![(Duration::ZERO, a.to_vec()), (Duration::from_secs(3), b.to_vec())]);
    let (out, took) = run_feeding(&env, &["issue", "update", "1", "--description-file", "-"], "1", paused);
    assert_eq!(out.code, 0, "{out:#?}");
    assert!(took >= Duration::from_secs(3), "lcl finished before the rest was sent: {took:?}");
    let shown = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json();
    assert_same(&shown["issue"]["description"], &body, "description after a 3 s pause");
}
