use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use isoloom_manifest::{Driver, HostOs, Target};
use proptest::prelude::*;

use super::*;
use crate::name::RunName;
use crate::process::fake::{FakeProcesses, Reply};
use crate::testing::{TEST_TIME, no_ports, processes, runner_config};

/// A runner whose tools answer with `answer`, its home a temporary folder.
fn runner(answer: impl Fn(&crate::process::fake::Call) -> Reply + Send + Sync + 'static) -> (tempfile::TempDir, Runner) {
    let home = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeProcesses::new(answer));
    let r = Runner::new(runner_config(home.path(), processes(&fake), no_ports(), HostOs::Linux)).unwrap();
    (home, r)
}

/// A recorded run of `cloud` in `state`, expiring at `expires_at`, its Terraform state `tfstate`.
fn record(r: &Runner, name: &str, state: RunState, expires_at: Option<u64>, tfstate: Option<&str>) {
    let mut rec = RunRecord::fresh(RunName::new(name).unwrap(), "/l/run.json".into(), Target::CloudVm, Driver::Terraform);
    rec.cloud = Some(Cloud::Aws);
    rec.state = state;
    rec.expires_at = expires_at;
    r.store.save(&rec).unwrap();
    if let Some(text) = tfstate {
        std::fs::write(r.store.run_dir(&rec.name).unwrap().join("terraform.tfstate"), text).unwrap();
    }
}

/// Every cloud.
const CLOUDS: [Cloud; 6] = [Cloud::Aws, Cloud::Azure, Cloud::Gcp, Cloud::Digitalocean, Cloud::Linode, Cloud::Oci];

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(TEST_TIME)
}

#[test]
fn a_run_is_expired_once_its_time_passed_while_something_of_it_is_left() {
    let (_home, r) = runner(|_| Reply::ok(""));
    record(&r, "past", RunState::Ready, Some(TEST_TIME - 1), None);
    record(&r, "failed-past", RunState::Failed, Some(TEST_TIME), None);
    record(&r, "future", RunState::Ready, Some(TEST_TIME + 60), None);
    record(&r, "gone", RunState::Absent, Some(TEST_TIME - 600), None);
    record(&r, "no-expiry", RunState::Ready, None, None);
    let names: Vec<String> = r.expired(now()).unwrap().iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names, ["failed-past", "past"]);
}

/// What `aws resourcegroupstaggingapi get-resources` answers: an expired instance no run covers,
/// one a recorded run's state names, one not expired yet, and two whose expiry isn't a time (one
/// no run covers, one a recorded run's state names).
const AWS: &str = r#"{"ResourceTagMappingList": [
  {"ResourceARN": "arn:aws:ec2:eu-west-3:123456789012:instance/i-0abandoned", "Tags": [
    {"Key": "isoloom-expires-at", "Value": "1792000000"}, {"Key": "isoloom-instance", "Value": "isoloom-solo-web-dead0000"}]},
  {"ResourceARN": "arn:aws:ec2:eu-west-3:123456789012:instance/i-0recorded", "Tags": [
    {"Key": "isoloom-expires-at", "Value": "1792000000"}, {"Key": "isoloom-instance", "Value": "isoloom-solo-web-ab12cd34"}]},
  {"ResourceARN": "arn:aws:ec2:eu-west-3:123456789012:vpc/vpc-0later", "Tags": [
    {"Key": "isoloom-expires-at", "Value": "1999999999"}, {"Key": "isoloom-instance", "Value": "isoloom-other-0000aaaa"}]},
  {"ResourceARN": "arn:aws:ec2:eu-west-3:123456789012:subnet/subnet-0none", "Tags": [{"Key": "isoloom-expires-at", "Value": ""}]},
  {"ResourceARN": "arn:aws:ec2:eu-west-3:123456789012:subnet/subnet-0covered", "Tags": [
    {"Key": "isoloom-expires-at", "Value": "2026-12-01"}, {"Key": "isoloom-instance", "Value": "isoloom-solo-web-ab12cd34"}]}
]}"#;

/// A recorded run's state that exists but doesn't parse fails the sweep: what it holds is unknown,
/// and skipping it would list its live resources as orphans.
#[tokio::test]
async fn a_recorded_state_that_doesnt_parse_fails_the_sweep() {
    let (_home, r) = runner(|call| if call.program == "aws" { Reply::ok(AWS) } else { Reply::missing() });
    record(&r, "live", RunState::Ready, Some(TEST_TIME + 60), Some("{ cut short"));
    let place = Place::new(vec!["eu-west-3".into()], None);
    let err = r.orphans(Cloud::Aws, &place, now()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Corrupt, "{err}");
    assert!(err.to_string().contains("terraform.tfstate"), "{err}");
}

#[tokio::test]
async fn an_orphan_is_tagged_past_its_expiry_with_no_recorded_run_holding_it() {
    let (_home, r) = runner(|call| if call.program == "aws" { Reply::ok(AWS) } else { Reply::missing() });
    let state = r#"{"resources":[{"instances":[{"attributes":{"tags":{"isoloom-instance":"isoloom-solo-web-ab12cd34"}}}]}]}"#;
    record(&r, "live", RunState::Ready, Some(TEST_TIME + 60), Some(state));
    let place = Place::new(vec!["eu-west-3".into()], None);
    let found = r.orphans(Cloud::Aws, &place, now()).await.unwrap();
    let ids: Vec<&str> = found.resources.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["arn:aws:ec2:eu-west-3:123456789012:instance/i-0abandoned"]);
    assert_eq!(found.resources[0].kind, "ec2 instance");
    assert_eq!(
        found.unread,
        ["`arn:aws:ec2:eu-west-3:123456789012:subnet/subnet-0none`: its expiry tag `` isn't a time"],
        "an expiry that isn't a time is said, unless a recorded run covers the resource"
    );
}

#[tokio::test]
async fn a_cloud_whose_command_line_is_missing_is_said_unchecked_not_a_failure() {
    let (_home, r) = runner(|_| Reply::missing());
    let found = r.orphans(Cloud::Gcp, &Place::default(), now()).await.unwrap();
    assert_eq!(found.resources, []);
    assert_eq!(found.unread.len(), 1);
    assert!(found.unread[0].starts_with("`gcloud compute instances list"), "{found:?}");
}

/// A query counts its limit as every tool call does: the time it goes without progress.
#[tokio::test(start_paused = true)]
async fn a_cloud_query_that_prints_nothing_is_given_up_after_its_limit() {
    let (_home, r) = runner(|_| Reply::ok("[]").after(Duration::from_secs(3600)));
    let found = r.orphans(Cloud::Gcp, &Place::default(), now()).await.unwrap();
    assert_eq!(found.unread.len(), 1, "{found:?}");
    assert!(found.unread[0].ends_with("printed nothing for 120s and was stopped"), "{found:?}");
}

#[test]
fn each_cloud_asks_its_own_command_line_read_only() {
    let place = Place::new(vec!["eu-west-3".into(), "us-east-1".into()], Some("demo-123".into()));
    let lines: Vec<Vec<String>> = CLOUDS
        .iter()
        .map(|c| tags::queries(*c, &place).iter().map(ProcessSpec::line).collect())
        .collect();
    assert_eq!(lines[0].len(), 2, "one query per AWS region");
    assert!(lines[0][1].ends_with("--region us-east-1"));
    assert!(lines[2][0].ends_with("--project=demo-123"));
    for (cloud, l) in CLOUDS.iter().zip(&lines) {
        assert!(l[0].starts_with(tags::cloud_cli(*cloud)), "{l:?}");
        for word in [" delete", " remove", " destroy", " rm "] {
            assert!(!l[0].contains(word), "{l:?}");
        }
    }
}

#[test]
fn every_clouds_answer_reads_as_tagged_resources() {
    let azure = r#"[{"id": "/subscriptions/s/resourceGroups/isoloom-x/providers/Microsoft.Compute/virtualMachines/web", "type": "Microsoft.Compute/virtualMachines", "tags": {"isoloom-expires-at": "1792000000", "isoloom-instance": "isoloom-x-1"}}]"#;
    let gcp = r#"[{"name": "isoloom-x-1-web", "selfLink": "https://www.googleapis.com/compute/v1/projects/p/zones/z/instances/isoloom-x-1-web", "labels": {"isoloom-expires-at": "1792000000", "isoloom-instance": "isoloom-x-1"}}]"#;
    let droplets =
        r#"[{"id": 101, "name": "web", "tags": ["isoloom", "x", "isoloom-x-1", "isoloom-expires-1792000000"]}, {"id": 102, "name": "mine", "tags": ["blog"]}]"#;
    let linodes = r#"[{"id": 7, "label": "web", "tags": ["isoloom", "x", "isoloom-x-1", "isoloom-expires-1792000000"]}]"#;
    let oci = r#"{"data": {"items": [{"identifier": "ocid1.instance.oc1..aaa", "resource-type": "Instance", "freeform-tags": {"isoloom-expires-at": "1792000000", "isoloom-instance": "isoloom-x-1"}}]}}"#;
    for (cloud, text, id, kind) in [
        (
            Cloud::Azure,
            azure,
            "/subscriptions/s/resourceGroups/isoloom-x/providers/Microsoft.Compute/virtualMachines/web",
            "Microsoft.Compute/virtualMachines",
        ),
        (
            Cloud::Gcp,
            gcp,
            "https://www.googleapis.com/compute/v1/projects/p/zones/z/instances/isoloom-x-1-web",
            "compute instance",
        ),
        (Cloud::Digitalocean, droplets, "101", "droplet"),
        (Cloud::Linode, linodes, "7", "linode"),
        (Cloud::Oci, oci, "ocid1.instance.oc1..aaa", "Instance"),
    ] {
        let found: Vec<Tagged> = tags::parse(cloud, text).unwrap().into_iter().map(Result::unwrap).collect();
        assert_eq!(found.len(), 1, "{cloud:?}: {found:?}");
        assert_eq!((found[0].id.as_str(), found[0].kind.as_str()), (id, kind), "{cloud:?}");
        assert_eq!(found[0].expires_at, Some(1_792_000_000), "{cloud:?}");
        assert_eq!(found[0].instance.as_deref(), Some("isoloom-x-1"), "{cloud:?}");
    }
    assert!(tags::parse(Cloud::Aws, "[]").unwrap_err().contains("not the shape aws gives"));
}

#[test]
fn an_expiry_that_isnt_a_time_is_said_on_every_kind_of_tag() {
    let keyed = r#"[{"id": "/r/web", "type": "vm", "tags": {"isoloom-expires-at": "soon", "isoloom-instance": "isoloom-x-1"}}]"#;
    let words = r#"[{"id": 7, "tags": ["isoloom-x-1", "isoloom-expires-tomorrow"]}]"#;
    for (cloud, text, id, value) in [(Cloud::Azure, keyed, "/r/web", "soon"), (Cloud::Linode, words, "7", "tomorrow")] {
        let found = tags::parse(cloud, text).unwrap();
        let expected = tags::Unreadable {
            id: id.into(),
            instance: Some("isoloom-x-1".into()),
            value: value.into(),
        };
        assert_eq!(found, [Err(expected)], "{cloud:?}");
    }
}

#[test]
fn a_state_names_its_environment_by_tag_or_by_word() {
    let state: serde_json::Value = serde_json::from_str(
        r#"{"resources":[{"instances":[{"attributes":{"tags":{"isoloom-instance":"isoloom-a-1"}}},{"attributes":{"tags":["isoloom","a","isoloom-b-2","isoloom-expires-1"]}},{"attributes":{"tags":["isoloom-not-ours"]}}]}]}"#,
    )
    .unwrap();
    let mut found = BTreeSet::new();
    tags::instances_in_state(&state, &mut found);
    assert_eq!(found.into_iter().collect::<Vec<_>>(), ["isoloom-a-1", "isoloom-b-2"]);
}

proptest! {
    #[test]
    fn any_answer_reads_or_is_refused_without_panicking(text in ".{0,300}", cloud in 0usize..6) {
        let cloud = CLOUDS[cloud];
        // Read or refused; an expiry read is one the answer holds.
        if let Ok(found) = tags::parse(cloud, &text) {
            prop_assert!(found.iter().flatten().all(|t| t.expires_at.is_none_or(|at| text.contains(&at.to_string()))));
        }
    }

    #[test]
    fn a_word_tags_expiry_reads_back(at in 0u64..4_000_000_000, instance in "isoloom-[a-df-z0-9][a-z0-9-]{0,19}") {
        let text = serde_json::json!([{"id": 1, "tags": ["isoloom", instance.clone(), format!("isoloom-expires-{at}")]}]).to_string();
        let found = tags::parse(Cloud::Linode, &text).unwrap().remove(0).unwrap();
        prop_assert_eq!(found.expires_at, Some(at));
        prop_assert_eq!(found.instance.as_deref(), Some(instance.as_str()));
    }
}
