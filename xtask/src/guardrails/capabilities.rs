//! A claimed capability must change what runs: every method of `FormatDriver` a driver of the
//! runner defines names the test that shows the call reach its tool (a scripted process sees the
//! command, or the file the method owns changes) and what in that test shows it; a method that
//! reaches no tool (a refusal, a query, a read of a file the tool wrote) says so. A claim may
//! instead be shown by a clause of the format contract (`formats/contract.rs`), which every
//! driver's model of its tool runs (`formats/<driver>/tests/contract.rs`): the model then answers
//! the call the claim makes. A claim with none fails, and so does a row whose claim the driver no
//! longer makes.

mod claims;
use super::source;
use claims::{CLAIMS, OWED};

/// How a claim is shown.
#[derive(Debug, Clone, Copy)]
enum Proof {
    /// The test `test` shows it: its text holds `evidence` (the command or the effect it asserts).
    Witness { test: &'static str, evidence: &'static str },
    /// The contract's clause `clause` shows it, run against the driver's model of its tool, whose
    /// file holds `evidence` (the call it answers).
    Contract { clause: &'static str, evidence: &'static str },
    /// The claim reaches no tool, and why.
    NoTool(&'static str),
}

use Proof::{Contract, NoTool, Witness};

/// The format contract's clauses.
const CONTRACT: &str = "crates/isothrum-runner/src/formats/contract.rs";

/// Driver `driver`'s model of its tool, which runs the contract.
fn model_of(driver: &str) -> String {
    format!("crates/isothrum-runner/src/formats/{driver}/tests/contract.rs")
}

/// What is wrong with the contract proof of `driver`'s `claim`: its clause missing from the
/// contract, the driver's model missing, not running the contract, or not answering `evidence`.
fn contract_unproven(tests: &[source::File], driver: &str, claim: &str, clause: &str, evidence: &str) -> Option<String> {
    let text = |path: &str| tests.iter().find(|f| f.path == path).map(|f| f.text.as_str());
    let model = model_of(driver);
    if !text(CONTRACT).is_some_and(|t| t.contains(&format!("async fn {clause}<"))) {
        return Some(format!("{driver} `{claim}`: `{clause}` is no clause of the contract ({CONTRACT})"));
    }
    match text(&model) {
        Some(t) if !t.contains("contract::suite!(") => Some(format!("{driver} `{claim}`: its model ({model}) doesn't run the contract")),
        Some(t) if !t.contains(evidence) => Some(format!("{driver} `{claim}`: its model ({model}) no longer answers `{evidence}`")),
        Some(_) => None,
        None => Some(format!("{driver} `{claim}`: it has no model of its tool ({model})")),
    }
}

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
        if let Some(Contract { clause, evidence }) = proof {
            found.extend(contract_unproven(tests, driver, claim, clause, evidence));
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
    fn a_contract_proof_needs_its_clause_and_a_model_that_runs_the_contract_and_answers_the_call() {
        let file = |path: &str, text: &str| source::File {
            path: path.to_owned(),
            text: text.to_owned(),
        };
        let contract = || file(CONTRACT, "pub(crate) async fn probe_reads_the_version<M: Model>() {}");
        let model = |text: &str| file(&model_of("compose"), text);
        let check = |files: &[source::File], clause: &str| contract_unproven(files, "compose", "probe", clause, "\"--short\"");
        let answering = model("contract::suite!(Docker);\n[\"compose\", \"version\", \"--short\"]");
        assert_eq!(check(&[contract(), answering], "probe_reads_the_version"), None);
        let said = [
            check(&[contract()], "probe_reads_the_version"),
            check(&[contract(), model("[\"--short\"]")], "probe_reads_the_version"),
            check(&[contract(), model("contract::suite!(Docker);")], "probe_reads_the_version"),
            check(&[contract()], "up_then_observe_reads_it_ready"),
        ];
        assert_eq!(
            said.map(Option::unwrap_or_default),
            [
                "compose `probe`: it has no model of its tool (crates/isothrum-runner/src/formats/compose/tests/contract.rs)",
                "compose `probe`: its model (crates/isothrum-runner/src/formats/compose/tests/contract.rs) doesn't run the contract",
                "compose `probe`: its model (crates/isothrum-runner/src/formats/compose/tests/contract.rs) no longer answers `\"--short\"`",
                "compose `probe`: `up_then_observe_reads_it_ready` is no clause of the contract (crates/isothrum-runner/src/formats/contract.rs)",
            ]
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
