//! A claimed capability must change what runs: every method of `FormatDriver` a driver of the
//! runner defines names the test that shows the call reach its tool (a scripted process sees the
//! command, or the file the method owns changes) and what in that test shows it; a method that
//! reaches no tool (a refusal, a query, a read of a file the tool wrote) says so. A claim with
//! neither fails, and so does a row whose claim the driver no longer makes.

mod claims;
use super::source;
use claims::{CLAIMS, OWED};

/// How a claim is shown.
#[derive(Debug, Clone, Copy)]
enum Proof {
    /// The test `test` shows it: its text holds `evidence` (the command or the effect it asserts).
    Witness { test: &'static str, evidence: &'static str },
    /// The claim reaches no tool, and why.
    NoTool(&'static str),
}

use Proof::{NoTool, Witness};

/// The drivers: their name and the file of their `impl FormatDriver`.
const DRIVERS: [(&str, &str); 3] = [
    ("compose", "crates/isothrum-runner/src/formats/compose.rs"),
    ("vagrant", "crates/isothrum-runner/src/formats/vagrant.rs"),
    ("terraform", "crates/isothrum-runner/src/formats/terraform.rs"),
];

/// What `impl FormatDriver for …` in `text` claims: each method it defines.
fn claims_in(text: &str) -> Vec<String> {
    let Some(start) = text.find("impl FormatDriver for ") else {
        return Vec::new();
    };
    let block = text.get(start..).unwrap_or("");
    let block = block.get(..block.find("\n}").unwrap_or(block.len())).unwrap_or("");
    let mut found = Vec::new();
    for line in block.lines() {
        let line = line.trim_start();
        let line = line.strip_prefix("async ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("fn ") {
            found.extend(rest.split(['(', '<']).next().map(str::to_owned));
        }
    }
    found
}

/// Every claim without a proof, every proof whose claim is gone, and every witness that isn't
/// a test of the runner or doesn't hold its evidence.
fn unproven(drivers: &[(&str, Vec<String>)], tests: &[source::File]) -> Vec<String> {
    let mut found = Vec::new();
    for (driver, claims) in drivers {
        for claim in claims {
            let listed = CLAIMS.iter().any(|(d, c, _)| d == driver && c == claim) || OWED.iter().any(|(d, c, _)| d == driver && c == claim);
            if !listed {
                found.push(format!(
                    "{driver} claims `{claim}` with no test showing it reach its tool: add the claim and its witness to CLAIMS (xtask/src/guardrails/capabilities.rs)"
                ));
            }
        }
    }
    for (driver, claim, proof) in CLAIMS
        .iter()
        .map(|(d, c, p)| (*d, *c, Some(*p)))
        .chain(OWED.iter().map(|(d, c, _)| (*d, *c, None)))
    {
        let claimed = drivers.iter().any(|(d, claims)| *d == driver && claims.iter().any(|c| c == claim));
        if !claimed {
            found.push(format!(
                "{driver} no longer claims `{claim}`: take its row out (xtask/src/guardrails/capabilities.rs)"
            ));
        }
        if let Some(NoTool(why)) = proof
            && why.trim().is_empty()
        {
            found.push(format!("{driver} `{claim}`: a claim that reaches no tool says why"));
        }
        if let Some(Witness { test, evidence }) = proof {
            match source::test_body(tests, test) {
                None => found.push(format!("{driver} `{claim}`: its witness `{test}` is no test of the runner")),
                Some(body) if !body.contains(evidence) => {
                    found.push(format!("{driver} `{claim}`: its witness `{test}` no longer shows `{evidence}`"));
                }
                Some(_) => {}
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_without_a_witness_and_a_witness_without_its_claim_are_named() {
        let text = "impl FormatDriver for Compose {\n    async fn attach(&self) {}\n    fn shiny(&self) {}\n}\nfn outside() {}\n";
        let claims = claims_in(text);
        assert_eq!(claims, ["attach", "shiny"]);
        let tests = [source::File {
            path: "t.rs".to_owned(),
            text:
                "#[test]\nfn attach_names_the_service_so_its_profile_starts_it_and_retire_removes_it_with_its_volumes() { x(\"up --detach --wait attackbox\") }"
                    .to_owned(),
        }];
        let found = unproven(&[("compose", claims)], &tests);
        assert!(found.iter().any(|f| f.starts_with("compose claims `shiny` with no test")), "{found:#?}");
        assert!(
            found
                .iter()
                .any(|f| f == "compose no longer claims `retire`: take its row out (xtask/src/guardrails/capabilities.rs)"),
            "{found:#?}"
        );
        assert!(
            !found.iter().any(|f| f.starts_with("compose") && f.contains("`attach`")),
            "attach has its witness: {found:#?}"
        );
    }

    #[test]
    fn every_capability_a_driver_claims_has_a_test_that_reaches_its_tool() {
        let drivers: Vec<(&str, Vec<String>)> = DRIVERS
            .iter()
            .map(|(name, path)| {
                let text = std::fs::read_to_string(crate::repo::path_in(&crate::repo::root(), path)).unwrap_or_default();
                (*name, claims_in(&source::code(&text)))
            })
            .collect();
        assert!(
            drivers.iter().all(|(_, claims)| !claims.is_empty()),
            "every driver's impl was read: {drivers:?}"
        );
        let tests = source::files("crates/isothrum-runner");
        let found = unproven(&drivers, &tests);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
