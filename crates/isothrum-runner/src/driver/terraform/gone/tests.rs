use super::*;

/// The state a Proxmox host's reset left (a cut-short apply's IP set, gone from the server), as
/// the real run kept it.
const LEFT: &str = r#"{"version":4,"resources":[{"mode":"managed","type":"proxmox_virtual_environment_firewall_ipset","name":"test","provider":"provider[\"registry.terraform.io/bpg/proxmox\"]","instances":[{"schema_version":0,"attributes":{"cidr":[{"comment":"","name":"10.254.250.0/24","nomatch":false}],"comment":"cyberctf-server-selftest: network test","container_id":null,"id":"iso1n0","name":"iso1n0","node_name":null,"vm_id":null}}]}]}"#;

fn errors(named: &[(&str, &str)]) -> Vec<GoneError> {
    named
        .iter()
        .map(|(resource, error)| GoneError {
            resource: isoloom_manifest::SnakeName::parse(resource).unwrap(),
            error: isoloom_manifest::GoneText::parse(error).unwrap(),
        })
        .collect()
}

fn ipset() -> Vec<GoneError> {
    errors(&[("proxmox_virtual_environment_firewall_ipset", "no such IPSet '{id}'")])
}

#[test]
fn an_instance_of_a_named_type_gets_its_error_filled_with_its_attributes() {
    let state: Value = serde_json::from_str(LEFT).unwrap();
    assert_eq!(
        in_state(&state, &ipset()),
        [Gone {
            address: "proxmox_virtual_environment_firewall_ipset.test".into(),
            text: "no such IPSet 'iso1n0'".into(),
        }]
    );
    assert_eq!(in_state(&state, &[]), Vec::<Gone>::new(), "a type no error names");
    let vm = errors(&[(
        "proxmox_virtual_environment_vm",
        "Configuration file 'nodes/{node_name}/qemu-server/{id}.conf' does not exist",
    )]);
    assert_eq!(in_state(&state, &vm), Vec::<Gone>::new());
}

#[test]
fn an_instance_the_error_cant_name_is_left_out() {
    // `node_name` is null: the text would name no node, so no instance.
    let state: Value = serde_json::from_str(LEFT).unwrap();
    let by_node = errors(&[("proxmox_virtual_environment_firewall_ipset", "no such IPSet '{name}' on {node_name}")]);
    assert_eq!(in_state(&state, &by_node), Vec::<Gone>::new());
    let empty: Value = serde_json::from_str(&LEFT.replace(r#""id":"iso1n0""#, r#""id":"""#)).unwrap();
    assert_eq!(in_state(&empty, &ipset()), Vec::<Gone>::new(), "an empty id names nothing");
    let data: Value = serde_json::from_str(&LEFT.replace(r#""mode":"managed""#, r#""mode":"data""#)).unwrap();
    assert_eq!(in_state(&data, &ipset()), Vec::<Gone>::new(), "a data source is never destroyed");
}

#[test]
fn an_address_has_its_module_and_its_key() {
    let state = serde_json::json!({ "resources": [
        { "mode": "managed", "module": "module.lab", "type": "proxmox_virtual_environment_vm", "name": "m",
          "instances": [{ "index_key": 0, "attributes": { "id": "101", "node_name": "pve" } },
                        { "index_key": "web", "attributes": { "id": "102", "node_name": "pve" } },
                        { "index_key": true, "attributes": { "id": "103", "node_name": "pve" } }] }
    ]});
    let vm = errors(&[(
        "proxmox_virtual_environment_vm",
        "Configuration file 'nodes/{node_name}/qemu-server/{id}.conf' does not exist",
    )]);
    let found: Vec<(String, String)> = in_state(&state, &vm).into_iter().map(|g| (g.address, g.text)).collect();
    assert_eq!(
        found,
        [
            (
                "module.lab.proxmox_virtual_environment_vm.m[0]".to_owned(),
                "Configuration file 'nodes/pve/qemu-server/101.conf' does not exist".to_owned()
            ),
            (
                r#"module.lab.proxmox_virtual_environment_vm.m["web"]"#.to_owned(),
                "Configuration file 'nodes/pve/qemu-server/102.conf' does not exist".to_owned()
            ),
        ]
    );
}

fn step(json: &str) -> Step {
    serde_json::from_str(json).unwrap()
}

fn gone(address: &str) -> Gone {
    Gone {
        address: address.into(),
        text: String::new(),
    }
}

#[test]
fn the_state_rm_runs_as_the_destroy_ran_with_its_global_options_only() {
    let destroy = step(
        r#"{"id": "destroy", "argv": ["terraform", "-chdir=${run.state_dir}/m", "destroy", "-auto-approve", "-refresh=false"], "env": {"TF_DATA_DIR": "${run.state_dir}/.terraform"}, "cwd": "mod", "timeout_s": 1800, "verdict": "Error: .*"}"#,
    );
    let rm = state_rm(&destroy, HostOs::Linux, &[gone("a.b"), gone(r#"c.d["e"]"#)]).unwrap();
    let Run::Argv(HostArgv::Every(argv)) = &rm.run else { panic!("{rm:?}") };
    let words: Vec<String> = argv.args().iter().map(ToString::to_string).collect();
    assert_eq!(argv.program().as_str(), "terraform");
    assert_eq!(words, ["-chdir=${run.state_dir}/m", "state", "rm", "a.b", r#"c.d["e"]"#]);
    assert_eq!(
        (rm.id.as_str(), rm.env.clone(), rm.cwd.clone(), rm.timeout_s),
        ("destroy", destroy.env.clone(), destroy.cwd.clone(), destroy.timeout_s)
    );
    assert_eq!(rm.verdict, None);
}

#[test]
fn only_a_destroy_with_something_gone_drops_anything() {
    let a = [gone("a.b")];
    let apply = step(r#"{"id": "apply", "argv": ["terraform", "apply", "-auto-approve"]}"#);
    assert!(state_rm(&apply, HostOs::Linux, &a).is_none());
    let ssh = step(r#"{"id": "x", "argv": ["ssh", "destroy"]}"#);
    assert!(state_rm(&ssh, HostOs::Linux, &a).is_none());
    let destroy = step(r#"{"id": "destroy", "argv": ["tofu", "destroy"]}"#);
    assert!(state_rm(&destroy, HostOs::Linux, &[]).is_none(), "nothing gone");
    assert!(state_rm(&destroy, HostOs::Linux, &a).is_some(), "OpenTofu's");
    let elsewhere = step(r#"{"id": "destroy", "argv": {"windows": ["terraform", "destroy"]}}"#);
    assert!(state_rm(&elsewhere, HostOs::Linux, &a).is_none(), "no argv on this host");
}
