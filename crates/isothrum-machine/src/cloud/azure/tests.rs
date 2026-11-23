use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};
use serde_json::json;

use super::*;
use crate::testing::{answered_by, shared};

const SIZE: &str = "Standard_D2als_v6";

/// A size in `location`, refused to the subscription there for `reason` when given.
fn size(name: &str, location: &str, reason: Option<&str>, vcpus: u32, memory: f64) -> Size {
    Size {
        name: name.into(),
        family: "f".into(),
        location: location.into(),
        restricted: reason.map(str::to_owned),
        vcpus: Some(vcpus),
        memory_gb: Some(memory),
    }
}

#[test]
fn each_refusal_reads_as_the_azure_cli_says_it() {
    assert_eq!(refusal(include_str!("../fixtures/az/2.67-not-logged-in.txt")), Refusal::CredentialsExpired);
    assert_eq!(
        refusal(include_str!("../fixtures/az/2.67-refresh-token-expired.txt")),
        Refusal::CredentialsExpired
    );
    assert_eq!(
        refusal("ERROR: Subscription '00000000-0000-0000-0000-000000000000' was not found."),
        Refusal::NotFound
    );
    assert_eq!(refusal("ERROR: (TooManyRequests) throttled"), Refusal::Other);
}

#[tokio::test(start_paused = true)]
async fn the_subscriptions_and_a_subscriptions_name_are_read_from_the_cli() {
    let tools = FakeProcesses::script()
        .expect("az account list", Reply::ok(include_str!("../fixtures/az/2.67-account-list.json")))
        .expect("az account show --subscription 0000", Reply::ok("Pay-As-You-Go\n"))
        .expect("az account get-access-token --output none", Reply::ok(""))
        .build();
    let cx = answered_by(tools);
    let subs = subscriptions(&cx).await.unwrap();
    assert_eq!(subs.len(), 2);
    assert_eq!((subs[0].name.as_str(), subs[0].is_default), ("Pay-As-You-Go", true));
    assert_eq!((subs[1].id.as_str(), subs[1].is_default), ("11111111-1111-1111-1111-111111111111", false));
    assert_eq!(subscription_name(&cx, "0000").await.unwrap(), "Pay-As-You-Go");
    assert_eq!(signed_in(&cx).await, Ok(()));
}

#[test]
fn a_subscription_list_that_isnt_one_is_unreadable_never_empty() {
    for out in ["{}", "null", "", "WARNING: update available\n[]", "[1, \"x\", null]", "Please run 'az login'"] {
        assert!(matches!(read_subscriptions(out), Err(Failure::Unreadable { .. })), "{out:?}");
    }
    let loose = read_subscriptions(r#"[{"name": "Dev", "id": "1111"}]"#).unwrap();
    assert!(!loose[0].is_default, "no default said: not the default");
}

#[tokio::test(start_paused = true)]
async fn a_regions_sizes_are_read_with_every_size_listed_and_only_a_location_restriction_refuses_one() {
    let tools = Arc::new(
        FakeProcesses::script()
            .expect("az vm list-skus", Reply::ok(include_str!("../fixtures/az/2.67-list-skus-swedencentral.json")))
            .build(),
    );
    let cx = shared(Arc::<FakeProcesses>::clone(&tools));
    let listed = sizes(&cx, "sub", Some("swedencentral"), None).await.unwrap();
    // `--all`: without it az leaves out the sizes refused to the subscription, which then read
    // as not offered.
    assert_eq!(
        tools.last("list-skus").unwrap().line(),
        "az vm list-skus --subscription sub --resource-type virtualMachines --all -o json -l swedencentral"
    );
    assert_eq!(in_region(&listed, SIZE), InRegion::Restricted("NotAvailableForSubscription".into()));
    // A zone restriction closes some zones only.
    assert_eq!(in_region(&listed, "standard_b2s"), InRegion::Usable("standardbsfamily".into()));
    assert_eq!(in_region(&listed, "Standard_E2s_v5"), InRegion::NotOffered);
    let d2s = listed.iter().find(|s| s.is("Standard_D2s_v5")).unwrap();
    assert_eq!((d2s.vcpus, d2s.memory_gb, d2s.location.as_str()), (Some(2), Some(8.0), "swedencentral"));
}

#[tokio::test(start_paused = true)]
async fn a_size_list_that_fails_or_isnt_one_is_an_error_never_no_sizes() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect("az vm list-skus", Reply::fail(1, include_str!("../fixtures/az/2.67-not-logged-in.txt")))
            .build(),
    );
    let failure = sizes(&cx, "sub", Some("westeurope"), Some(SIZE)).await.unwrap_err();
    assert_eq!(failure.refusal(), Some(Refusal::CredentialsExpired));
    assert!(failure.to_string().contains("az login"), "{failure}");
    let not_a_list = answered_by(FakeProcesses::script().expect("az vm list-skus", Reply::ok("{}")).build());
    assert!(matches!(sizes(&not_a_list, "sub", None, Some(SIZE)).await, Err(Failure::Unreadable { .. })));
}

#[test]
fn entries_with_missing_fields_read_without_a_panic() {
    let read: Vec<Size> = [json!({"name": SIZE, "restrictions": null}), json!({"no": "name"})]
        .iter()
        .filter_map(read_size)
        .collect();
    assert_eq!(read.len(), 1);
    assert_eq!(in_region(&read, SIZE), InRegion::Usable(String::new()));
    let elsewhere_only = read_size(&json!({"name": SIZE, "family": "F", "locations": ["WestEurope"], "restrictions": [{"type": "Location", "reasonCode": "NotAvailableForSubscription", "values": ["eastus"]}]})).unwrap();
    assert_eq!(
        (
            elsewhere_only.location.as_str(),
            elsewhere_only.restricted.as_deref(),
            elsewhere_only.family.as_str()
        ),
        ("westeurope", None, "f")
    );
}

#[test]
fn the_regions_elsewhere_never_name_the_region_asked() {
    let everywhere = [
        size(SIZE, "swedencentral", Some("NotAvailableForSubscription"), 2, 4.0),
        size(SIZE, "westeurope", None, 2, 4.0),
        size(SIZE, "northeurope", None, 2, 4.0),
        size(SIZE, "northeurope", None, 2, 4.0),
        size("Other", "uksouth", None, 2, 4.0),
    ];
    assert_eq!(elsewhere(&everywhere, SIZE, "swedencentral"), (vec!["northeurope", "westeurope"], 1));
    assert_eq!(elsewhere(&everywhere, SIZE, "westeurope"), (vec!["northeurope"], 1));
}

#[test]
fn alike_sizes_have_as_many_cores_and_as_much_memory_the_smallest_first() {
    let region = [
        size(SIZE, "r", Some("NotAvailableForSubscription"), 2, 4.0),
        size("Standard_D2s_v5", "r", None, 2, 8.0),
        size("Standard_B2s", "r", None, 2, 4.0),
        size("Standard_B1s", "r", None, 1, 1.0),
        size("Standard_D2as_v5", "r", Some("NotAvailableForSubscription"), 2, 8.0),
        size("Standard_D4s_v5", "r", None, 4, 16.0),
        size("Standard_E2s_v5", "r", None, 2, 16.0),
    ];
    let names: Vec<&str> = alike(SIZE, None, &region, 3).iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Standard_B2s", "Standard_D2s_v5", "Standard_E2s_v5"]);
    // A size the region doesn't list is known from the list across regions.
    let everywhere = [size("Big", "x", None, 4, 16.0)];
    let bigger: Vec<&str> = alike("Big", Some(&everywhere), &region, 3).iter().map(|s| s.name.as_str()).collect();
    assert_eq!(bigger, ["Standard_D4s_v5"]);
}

#[tokio::test(start_paused = true)]
async fn the_quotas_say_how_many_cores_are_free_whatever_their_numbers_are_written_as() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "az vm list-usage --subscription sub -l westeurope",
                Reply::ok(include_str!("../fixtures/az/2.67-list-usage.json")),
            )
            .build(),
    );
    let read = quotas(&cx, "sub", "westeurope").await.unwrap();
    assert_eq!(free_cores(&read, "standarddalsv6family"), Some(6));
    assert_eq!(free_cores(&read, "cores"), Some(20));
    assert_eq!(free_cores(&read, "otherfamily"), None);
    let loose = read_quotas(&[
        json!({"name": {"value": "Fam"}, "limit": "10"}),
        json!({"name": {"value": "odd"}, "limit": "ten", "currentValue": 4}),
        json!({"name": "not an object"}),
    ]);
    assert_eq!(free_cores(&loose, "fam"), Some(10), "no current use yet");
    assert_eq!(free_cores(&loose, "odd"), Some(-4), "unreadable numbers count as 0");
    assert_eq!(loose.len(), 2);
}
