use std::process::{Command, Output};

use open_computer_use::VERSION;

#[test]
fn cli_help_version_and_errors_are_truthful() {
    let help = run(&["help"]);
    assert!(help.status.success());
    let help_text = text(&help.stdout);
    assert!(help_text.contains("Open Computer Use for Linux Wayland"));
    assert!(help_text.contains("list-apps"));
    assert!(help_text.contains("snapshot APP"));
    assert!(help_text.contains("init"));
    assert!(help_text.contains("call FILE"));
    assert!(help_text.contains("requests KDE approval at startup"));

    let version = run(&["version"]);
    assert!(version.status.success());
    assert_eq!(text(&version.stdout).trim(), VERSION);

    let unknown = run(&["not-a-command"]);
    assert!(!unknown.status.success());
    assert!(text(&unknown.stderr).contains("unknown command"));

    let missing_app = run(&["snapshot"]);
    assert!(!missing_app.status.success());
    assert!(text(&missing_app.stderr).contains("requires exactly one"));
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_open-computer-use"))
        .args(arguments)
        .output()
        .expect("run open-computer-use")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
