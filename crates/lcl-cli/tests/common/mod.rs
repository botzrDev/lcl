//! Integration-test harness: runs the real `lcl` binary against a throwaway
//! HOME and working directory, so no test can read or write the real `~/.lcl`.
// Each test binary compiles its own copy of this module and uses a subset of it.
#![allow(dead_code)]

use std::process::Command;
use tempfile::TempDir;

/// What one `lcl` invocation produced.
#[derive(Debug)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Panics with the full output unless the process exited 0.
    #[track_caller]
    pub fn assert_ok(&self) -> &Self {
        assert_eq!(self.code, 0, "lcl exited {}\n{self:#?}", self.code);
        self
    }

    /// Parses stdout as JSON, panicking with the full output if it isn't.
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e})\n{self:#?}"))
    }
}

/// An isolated environment: a fresh HOME and a fresh cwd, both deleted on drop.
pub struct Env {
    pub home: TempDir,
    pub cwd: TempDir,
}

impl Env {
    pub fn new() -> Self {
        let env = Self {
            home: TempDir::new().expect("create temp HOME"),
            cwd: TempDir::new().expect("create temp cwd"),
        };
        // lcl discovers a project by walking up from cwd looking for
        // `.lcl/data.sqlite`. A stray one above the temp dir would leak into
        // every test that runs before `init`, so refuse to run rather than
        // test against it.
        for dir in env.cwd.path().ancestors().skip(1) {
            let stray = dir.join(".lcl").join("data.sqlite");
            assert!(!stray.exists(), "stray lcl database at {} would leak into tests", stray.display());
        }
        env
    }

    /// Runs `lcl <args>` with HOME and cwd pointed at this environment.
    pub fn lcl(&self, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_lcl"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", self.home.path())
            // `--project` reads LCL_PROJECT; an exported value must not select
            // a project behind the test's back.
            .env_remove("LCL_PROJECT")
            .output()
            .expect("spawn lcl");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output
            .status
            .code()
            .unwrap_or_else(|| panic!("lcl {args:?} killed by a signal\nstdout: {stdout}\nstderr: {stderr}"));
        Out { code, stdout, stderr }
    }
}
