//! `lcl tui` reads the issues before it touches the terminal, so a
//! database error ends it with that error (exit 1, as any error), not an
//! empty list. Key handling, drawing and restoring the terminal are tested
//! in lcl-tui itself.
//!
//! lcl runs under `setsid -w`, with no controlling terminal: if it ever
//! went on to the terminal it would fail to open /dev/tty rather than take
//! over the one running the tests. `setsid` is util-linux, so this runs on
//! Linux only.
#![cfg(target_os = "linux")]

mod common;

use common::{Env, Out, exec};
use serde_json::json;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// `lcl ARGS` under `setsid -w`, killed if it runs for 30 s.
fn detached(env: &Env, args: &[&str]) -> Out {
    let mut child = env
        .command("setsid")
        .arg("-w")
        .arg(env!("CARGO_BIN_EXE_lcl"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn setsid");
    let start = Instant::now();
    while child.try_wait().expect("wait").is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("lcl {args:?} still running after 30 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("output");
    Out {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn a_database_error_ends_the_tui_with_it() {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.create("one");
    exec(
        &env.db_path(),
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'Weird' WHERE number = 1;",
    );
    let message = "issue 1 has status \"Weird\", which this lcl doesn't know";
    let out = detached(&env, &["tui"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", format!("Error: {message}\n").as_str())
    );
    let out = detached(&env, &["--json", "tui"]);
    let want = format!(
        "{}\n",
        json!({"error": {"kind": "schema", "message": message}})
    );
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", want.as_str())
    );
}
