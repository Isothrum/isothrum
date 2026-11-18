//! No licence, agreement or terms is accepted on the user's behalf: no code of the crates (their
//! tests and comments aside) holds a switch that accepts one for them. Where a vendor asks for
//! it, its own window shows it and the user accepts it there; an install that would need it
//! accepted to go on unattended says so instead (`install::Interaction::VendorAgreement`, winget's
//! `Known::AgreementToAccept`).
//!
//! Read case aside: Docker's and VirtualBox's Extension Pack's `--accept-license`, winget's
//! `--accept-package-agreements` and `--accept-source-agreements`, Xcode's `-license accept`,
//! debconf's `accepted-…-license` answers and `select true` to a licence question, Microsoft's
//! `ACCEPT_EULA`, the EULA switches of installers (`/EULA`, `--accept-eula`, `AcceptEula`), and
//! `--agree-tos`.

use super::source::File;

/// What accepts a licence, an agreement or terms on its user's behalf, in lower case.
const ACCEPTS: [&str; 13] = [
    "--accept-license",
    "--accept-licence",
    "--accept-package-agreements",
    "--accept-source-agreements",
    "--accept-eula",
    "--accept-terms",
    "--agree-tos",
    "-license accept",
    "accepted-license",
    "license select true",
    "accept_eula",
    "accepteula",
    "/eula",
];

/// Each line of `files` (its comments aside) that holds one of [`ACCEPTS`], with its file and line.
fn accepted_for_the_user(files: &[File]) -> Vec<String> {
    files
        .iter()
        .flat_map(|f| {
            f.text
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim_start().starts_with("//"))
                .filter_map(|(n, line)| {
                    let lower = line.to_ascii_lowercase();
                    let flag = ACCEPTS.iter().find(|a| lower.contains(*a))?;
                    Some(format!(
                        "{}:{}: `{flag}` accepts a licence, an agreement or terms for the user: never; the vendor's own window shows it and they accept it there",
                        f.path,
                        n + 1
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
