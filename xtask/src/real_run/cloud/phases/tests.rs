use std::cell::Cell;
use std::path::PathBuf;

use super::*;
use isothrum_runner::isoloom_manifest::{Cloud, Target};

fn args(cloud: Cloud) -> CloudArgs {
    CloudArgs {
        cloud,
        target: Target::CloudVm,
        budget_minutes: 45,
        lifetime_minutes: 60,
        host: None,
    }
}

/// A cloud manifest Isoloom generated (AWS's `solo-web`), with `params` (`name`, required) and
/// slots `slots` too: one it has is marked as asked, one it lacks added as a copy of its first
/// one, renamed.
fn declared(params: &[(&str, bool)], slots: &[&str]) -> String {
    let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../crates/isothrum-runner/tests/fixtures/solo-web.cloud-vm-aws.run.json"
    ))
    .unwrap();
    for (key, name, required) in params
        .iter()
        .map(|(n, r)| ("params", *n, Some(*r)))
        .chain(slots.iter().map(|n| ("secrets", *n, None)))
    {
        let list = manifest[key].as_array_mut().unwrap();
        let at = list.iter().position(|e| e["name"] == name).unwrap_or_else(|| {
            let mut copy = list[0].clone();
            copy["name"] = name.into();
            copy.as_object_mut().unwrap().remove("default");
            list.push(copy);
            list.len() - 1
        });
        if let Some(required) = required {
            list[at]["required"] = required.into();
        }
    }
    manifest.to_string()
}

fn aws() -> Account {
    Account {
        secrets: vec!["AWS_ACCESS_KEY_ID".into(), "AWS_SECRET_ACCESS_KEY".into()],
        params: vec![("AWS_PROFILE".into(), "demo".into()), ("region".into(), "eu-west-3".into())],
        place: vec!["--region".into(), "eu-west-3".into()],
    }
}

fn plan() -> Vec<Phase> {
    let declared = declared(
        &[("region", false), ("allowed_cidr", true), ("AWS_PROFILE", false), ("instance_type", false)],
        &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"],
    );
    let (run_dir, home) = (PathBuf::from("/r/solo-web"), PathBuf::from("/r/home"));
    phases(&args(Cloud::Aws), &aws(), &declared, "203.0.113.7/32", (&run_dir, &home)).unwrap()
}

#[test]
fn the_start_takes_the_accounts_place_the_smallest_size_this_machines_address_and_a_short_life() {
    let p = plan();
    assert_eq!(
        p.iter().map(|p| p.step).collect::<Vec<_>>(),
        [Step::Up, Step::Status, Step::Check, Step::Down, Step::Reap]
    );
    let up = p[0].args.join(" ");
    assert!(
        up.starts_with("up /r/solo-web --target cloud-vm/aws --name isothrum-real-run --home /r/home --json"),
        "{up}"
    );
    for part in [
        "--param allowed_cidr=203.0.113.7/32",
        "--param instance_type=t3.micro",
        "--param AWS_PROFILE=demo",
        "--param region=eu-west-3",
        "--secret-from-env AWS_ACCESS_KEY_ID",
        "--lifetime 60",
    ] {
        assert!(up.contains(part), "{part}: {up}");
    }
    assert!(!up.contains("AWS_SESSION_TOKEN"), "only what the environment has");
    assert_eq!(p[4].args[..5], ["reap", "aws", "--home", "/r/home", "--json"]);
    assert!(p[4].args.join(" ").contains("--region eu-west-3 --secret-from-env AWS_ACCESS_KEY_ID"));
}

#[test]
fn a_module_without_a_size_param_keeps_its_own_sizes() {
    // cloud-vm modules size each machine from the spec: solo-web is the smallest already.
    let declared = declared(&[("allowed_cidr", true), ("subscription_id", true)], &[]);
    let (run_dir, home) = (PathBuf::from("/l"), PathBuf::from("/h"));
    let account = Account {
        params: vec![("subscription_id".into(), "sub-1".into())],
        ..Account::default()
    };
    let p = phases(&args(Cloud::Azure), &account, &declared, "1.2.3.4/32", (&run_dir, &home)).unwrap();
    let up = p[0].args.join(" ");
    assert!(
        up.contains("--param subscription_id=sub-1") && up.contains("--param allowed_cidr=1.2.3.4/32"),
        "{up}"
    );
    assert!(!up.contains("Standard_B1s"), "no size param to set: {up}");
}

/// Regression: the manifest was read as untyped JSON, so one Isoloom's contract doesn't read (a
/// field renamed) passed no params and no slots instead of failing.
#[test]
fn a_manifest_the_contract_doesnt_read_refuses_the_run() {
    let renamed = declared(&[("allowed_cidr", true)], &[]).replace("\"params\"", "\"parameters\"");
    let (run_dir, home) = (PathBuf::from("/l"), PathBuf::from("/h"));
    assert!(phases(&args(Cloud::Aws), &aws(), &renamed, "1.2.3.4/32", (&run_dir, &home)).is_err());
}

#[test]
fn a_param_the_manifest_requires_and_the_account_lacks_refuses_the_run() {
    let declared = declared(&[("allowed_cidr", true), ("compartment_id", true)], &[]);
    let (run_dir, home) = (PathBuf::from("/l"), PathBuf::from("/h"));
    let err = phases(&args(Cloud::Oci), &Account::default(), &declared, "1.2.3.4/32", (&run_dir, &home)).unwrap_err();
    assert!(err.to_string().contains("needs `compartment_id`"), "{err}");
}

/// A phase that failed.
fn failed() -> Ran {
    Ran::Failed("exit status: 1".into())
}

/// A runner that answers each step from `outcomes` and notes what ran.
struct Scripted {
    outcomes: Vec<(Step, Ran)>,
    ran: Vec<Step>,
}

impl Runs for Scripted {
    fn run(&mut self, args: &[String]) -> Ran {
        let step = [Step::Up, Step::Status, Step::Check, Step::Down, Step::Reap]
            .into_iter()
            .find(|s| args.first().map(String::as_str) == Some(s.name()))
            .unwrap();
        self.ran.push(step);
        self.outcomes.iter().find(|(s, _)| *s == step).map_or(Ran::Ok, |(_, r)| r.clone())
    }
}

fn scripted(outcomes: &[(Step, Ran)]) -> Scripted {
    Scripted {
        outcomes: outcomes.to_vec(),
        ran: Vec::new(),
    }
}

#[test]
fn a_whole_cycle_runs_every_phase_in_order() {
    let mut s = scripted(&[]);
    let out = execute(&plan(), Duration::from_secs(60), &mut s, &|| Duration::ZERO);
    assert_eq!(s.ran, [Step::Up, Step::Status, Step::Check, Step::Down, Step::Reap]);
    assert!(out.iter().all(|(_, r)| *r == Ran::Ok));
}

#[test]
fn a_failed_start_is_still_destroyed() {
    let mut s = scripted(&[(Step::Up, failed())]);
    let out = execute(&plan(), Duration::from_secs(60), &mut s, &|| Duration::ZERO);
    assert_eq!(s.ran, [Step::Up, Step::Down, Step::Reap]);
    assert_eq!(
        out.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(),
        [failed(), Ran::Skipped, Ran::Skipped, Ran::Ok, Ran::Ok]
    );
}

#[test]
fn a_failed_check_or_destroy_still_reaps() {
    let mut s = scripted(&[(Step::Check, failed()), (Step::Down, failed())]);
    let out = execute(&plan(), Duration::from_secs(60), &mut s, &|| Duration::ZERO);
    assert_eq!(s.ran, [Step::Up, Step::Status, Step::Check, Step::Down, Step::Reap]);
    assert!(out.iter().any(|(_, r)| *r == failed()));
}

#[test]
fn past_the_budget_nothing_but_the_destroy_starts() {
    let clock = Cell::new(Duration::ZERO);
    let mut s = scripted(&[]);
    // The start took the whole budget.
    let elapsed = || {
        let now = clock.get();
        clock.set(now + Duration::from_secs(3600));
        now
    };
    let out = execute(&plan(), Duration::from_secs(45 * 60), &mut s, &elapsed);
    assert_eq!(s.ran, [Step::Up, Step::Down, Step::Reap]);
    assert_eq!(out[1].1, Ran::OverBudget);
}
