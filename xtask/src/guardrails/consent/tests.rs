use super::*;
use crate::guardrails::source::file;

#[test]
fn each_switch_that_accepts_for_the_user_is_named_with_its_line() {
    let files = [
        file("a.rs", "const A: &str = \"install --accept-license --user=x\";\n// never `--accept-license`\n"),
        file("b.rs", "step(\"winget\", &[\"install\", \"--Accept-Package-Agreements\"])\n"),
        file(
            "c.rs",
            "\"xcodebuild -license accept\"\n\"ACCEPT_EULA=Y apt-get install\"\n\"setup.exe /EULA\"\n",
        ),
        file(
            "d.rs",
            "\"apt-get install -y --no-install-recommends virtualbox\"\n\"winget install -e --id X --source winget\"\n",
        ),
    ];
    let found = accepted_for_the_user(&files);
    let at: Vec<&str> = found.iter().map(|f| f.split(": ").next().unwrap()).collect();
    assert_eq!(at, ["a.rs:1", "b.rs:1", "c.rs:1", "c.rs:2", "c.rs:3"], "{found:#?}");
}

/// Regression: Docker Desktop's macOS install ran `install --accept-license`, and winget's
/// installs `--accept-package-agreements --accept-source-agreements`.
#[test]
fn nothing_is_accepted_on_the_users_behalf() {
    let found = accepted_for_the_user(&super::super::scans::code_files());
    assert!(found.is_empty(), "{}", found.join("\n"));
}
