//! Gives lcl's main thread 8 MiB of stack on Windows, as Linux and macOS do.
//!
//! Windows gives a program's main thread 1 MiB. A debug build of lcl needs
//! most of that just to build its command line (clap's derive code keeps
//! every argument in one frame), and a usage error from inside a command
//! builds it a second time: `lcl search ""` overflowed. A release build needs
//! about a tenth as much, but the limit is the platform's, not lcl's.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    match (os.as_str(), env.as_str()) {
        ("windows", "msvc") => println!("cargo::rustc-link-arg-bins=/STACK:8388608"),
        ("windows", "gnu") => println!("cargo::rustc-link-arg-bins=-Wl,--stack,8388608"),
        _ => {}
    }
}
