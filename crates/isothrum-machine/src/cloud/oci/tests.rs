use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::{answered_by, fresh_dir};

#[test]
fn each_refusal_reads_as_the_oci_cli_says_it() {
    assert_eq!(refusal(include_str!("../fixtures/oci/3.50-not-authenticated.txt")), Refusal::CredentialsExpired);
    assert_eq!(refusal(include_str!("../fixtures/oci/3.50-not-authorized-or-not-found.txt")), Refusal::Denied);
    assert_eq!(refusal("ServiceError: TooManyRequests"), Refusal::Other);
}

#[tokio::test(start_paused = true)]
async fn a_compartments_name_and_the_sign_in_are_read_from_the_cli() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "oci iam compartment get --compartment-id ocid1.tenancy.oc1..a --query data.name --raw-output",
                Reply::ok("root\n"),
            )
            .expect(
                "oci iam region list --output json",
                Reply::fail(1, include_str!("../fixtures/oci/3.50-not-authenticated.txt")),
            )
            .build(),
    );
    assert_eq!(compartment_name(&cx, "ocid1.tenancy.oc1..a").await.unwrap(), "root");
    assert_eq!(signed_in(&cx).await.unwrap_err().refusal(), Some(Refusal::CredentialsExpired));
}

#[test]
fn the_config_files_default_profile_is_read() {
    let text = "[OTHER]\ntenancy=ocid1.tenancy.oc1..other\n\n[DEFAULT]\nuser=ocid1.user.oc1..u\n tenancy = ocid1.tenancy.oc1..main \nregion=eu-frankfurt-1\nnot a pair\n[LATER]\nregion=us-ashburn-1\n";
    assert_eq!(
        parse_config(text),
        Config {
            tenancy: "ocid1.tenancy.oc1..main".into(),
            region: "eu-frankfurt-1".into()
        }
    );
    assert_eq!(parse_config("[default]\nregion=x\n").region, "x", "the profile's name in any case");
    assert_eq!(parse_config("[PROD]\nregion=x\n"), Config::default());
}

#[test]
fn config_lines_at_the_edges() {
    // CRLF endings, a later DEFAULT overriding, a value holding `=`, spaces around a key, an empty
    // value, and a header with text after it (another section).
    let text = "[DEFAULT]\r\nregion=eu-paris-1\r\ntenancy = ocid1.tenancy.oc1..a=b \r\n[OTHER]\r\nregion=x\r\n[DEFAULT]\r\nregion=\r\n";
    let c = parse_config(text);
    assert_eq!((c.tenancy.as_str(), c.region.as_str()), ("ocid1.tenancy.oc1..a=b", ""));
    assert_eq!(parse_config("[DEFAULT] ; main\nregion=x\n").region, "");
}

#[test]
fn a_config_file_is_read_and_none_is_none() {
    let dir = fresh_dir("oci-config");
    let file = dir.join("config");
    assert_eq!(config(&file), None);
    std::fs::write(&file, "[DEFAULT]\ntenancy=ocid1.tenancy.oc1..t\nregion=eu-paris-1\n").unwrap();
    assert_eq!(config(&file).map(|c| c.region), Some("eu-paris-1".to_owned()));
    std::fs::remove_dir_all(dir).unwrap();
}
