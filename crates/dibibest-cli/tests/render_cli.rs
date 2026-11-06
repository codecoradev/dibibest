//! `dibibest render` draws the ERD as SVG on stdout.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn render(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dibibest"))
        .arg("render")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn renders_svg_from_stdin() {
    let out = render(
        &["--theme", "light"],
        "CREATE TABLE users (id int PRIMARY KEY);\nCREATE TABLE posts (id int PRIMARY KEY, user_id int REFERENCES users(id));\n",
    );
    assert!(out.status.success());
    let svg = String::from_utf8(out.stdout).unwrap();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.contains(">users<") && svg.contains(">posts<"));
    assert!(svg.contains("fill=\"#ffffff\""));
    assert!(svg.contains("marker-end"));
}

#[test]
fn bad_theme_or_source_fails() {
    assert_eq!(render(&["--theme", "pink"], "").status.code(), Some(2));
    assert!(!render(&[], "hello").status.success());
}
