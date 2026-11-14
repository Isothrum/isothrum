//! A claimed capability must change what runs: every capability a driver of the runner claims
//! (each action of the manifest it takes, each hook of `Driver` it overrides) names the test
//! that shows the call reach its tool (a scripted process sees the command, or the file the
//! action owns changes) and what in that test shows it; a hook that reaches no tool (a refusal,
//! a nothing-to-do, the run's set-up) says so. A claim with neither fails, and so does a row
//! whose claim the driver no longer makes.

use super::source;

/// How a claim is shown.
#[derive(Debug, Clone, Copy)]
enum Proof {
    /// The test `test` shows it: its text holds `evidence` (the command or the effect it asserts).
    Witness { test: &'static str, evidence: &'static str },
    /// The claim reaches no tool, and why.
    NoTool(&'static str),
}

use Proof::{NoTool, Witness};

/// The drivers: their name and the file of their `impl Driver`.
const DRIVERS: [(&str, &str); 3] = [
    ("compose", "crates/isothrum-runner/src/driver/compose.rs"),
    ("vagrant", "crates/isothrum-runner/src/driver/vagrant.rs"),
    ("terraform", "crates/isothrum-runner/src/driver/terraform.rs"),
];

/// Each driver's claims (`action:<Action>` or the hook's name) and how each is shown.
const CLAIMS: [(&str, &str, Proof); 54] = [
    // Compose.
    (
        "compose",
        "action:ComposePinPorts",
        Witness {
            test: "a_run_comes_up_with_its_entry_and_message_then_goes",
            evidence: "the pinned port",
        },
    ),
    (
        "compose",
        "action:ComposeCheckPortsFree",
        Witness {
            test: "a_port_held_on_the_host_is_refused_before_anything_starts",
            evidence: "host port 8080/tcp is in use",
        },
    ),
    (
        "compose",
        "action:ComposeRemoveStaleNetworks",
        Witness {
            test: "a_stale_network_goes_and_a_defined_one_stays",
            evidence: "docker network rm web-portal_old",
        },
    ),
    (
        "compose",
        "action:ComposeRemoveContainers",
        Witness {
            test: "an_interrupted_down_retried_removes_everything_and_ends_absent",
            evidence: "ctrs.is_empty()",
        },
    ),
    (
        "compose",
        "action:ComposeRemoveNetworks",
        Witness {
            test: "an_interrupted_down_retried_removes_everything_and_ends_absent",
            evidence: "RunState::Absent",
        },
    ),
    (
        "compose",
        "before_step",
        Witness {
            test: "a_compose_up_pulls_its_services_with_its_project_files_and_policy",
            evidence: "pull --policy missing",
        },
    ),
    ("compose", "preflight", NoTool("nothing to check before a start: its tools are probed")),
    ("compose", "env", NoTool("no variable of its own")),
    ("compose", "prepare", NoTool("writes the pinned-ports file the calls name, before the first")),
    (
        "compose",
        "observe",
        Witness {
            test: "a_run_comes_up_with_its_entry_and_message_then_goes",
            evidence: "run.status().await",
        },
    ),
    (
        "compose",
        "before_change",
        Witness {
            test: "an_operation_cut_short_leaves_its_record_and_the_next_removes_what_it_orphaned",
            evidence: "volume rm --force",
        },
    ),
    (
        "compose",
        "after_change",
        Witness {
            test: "an_operation_cut_short_leaves_its_record_and_the_next_removes_what_it_orphaned",
            evidence: "volume rm --force",
        },
    ),
    (
        "compose",
        "after_down",
        Witness {
            test: "a_run_comes_up_with_its_entry_and_message_then_goes",
            evidence: "Containers::None",
        },
    ),
    (
        "compose",
        "restart",
        NoTool("nothing: a service carries no boot digest, up's steps recreate a changed one"),
    ),
    (
        "compose",
        "attach",
        Witness {
            test: "attach_brings_the_access_service_up_in_its_profile_with_the_picked_image",
            evidence: "--profile access up",
        },
    ),
    (
        "compose",
        "retire",
        Witness {
            test: "retiring_a_compose_service_removes_it_with_its_namespace",
            evidence: "--profile access rm -sfv user",
        },
    ),
    (
        "compose",
        "console_command",
        Witness {
            test: "a_console_into_a_compose_service_opens_as_the_account_asked",
            evidence: " exec --user user web sh -c ",
        },
    ),
    ("compose", "in_guest", NoTool("refused: a Compose run's containers run in no VM")),
    // Vagrant.
    (
        "vagrant",
        "action:VagrantReconcileProvider",
        Witness {
            test: "a_record_on_another_provider_is_cleared_before_the_start",
            evidence: "vagrant destroy --force",
        },
    ),
    (
        "vagrant",
        "action:VagrantDiscardAbortedSaved",
        Witness {
            test: "a_virtualbox_vm_whose_saved_state_was_cut_off_boots_fresh",
            evidence: "vagrant up --provider",
        },
    ),
    (
        "vagrant",
        "action:VagrantRemoveOrphans",
        Witness {
            test: "a_down_on_parallels_removes_the_runs_orphaned_vm_by_its_exact_name",
            evidence: "prlctl stop",
        },
    ),
    (
        "vagrant",
        "action:VagrantResetState",
        Witness {
            test: "vagrants_record_lives_in_the_runs_state_folder",
            evidence: "reset once the VMs are gone",
        },
    ),
    (
        "vagrant",
        "action:VagrantWakeController",
        Witness {
            test: "a_controller_still_halting_is_waited_for_before_it_boots",
            evidence: "asked_then_booted(&calls, 3)",
        },
    ),
    (
        "vagrant",
        "action:VagrantParkController",
        Witness {
            test: "a_controller_halts_with_its_lines_in_the_log",
            evidence: "Attempting graceful shutdown of VM",
        },
    ),
    (
        "vagrant",
        "action:VagrantCheckForwardedPorts",
        Witness {
            test: "a_forwarded_port_another_lab_holds_stops_the_start_before_the_vm_is_made",
            evidence: "ErrorCode::PortInUse",
        },
    ),
    (
        "vagrant",
        "pin_vm_ports",
        Witness {
            test: "a_fresh_local_start_pins_a_free_host_port_and_answers_on_it",
            evidence: "ISOLOOM_HOSTPORTS",
        },
    ),
    (
        "vagrant",
        "forwarded_port",
        Witness {
            test: "a_fresh_local_start_pins_a_free_host_port_and_answers_on_it",
            evidence: "http://127.0.0.1:41000/",
        },
    ),
    (
        "vagrant",
        "before_step",
        Witness {
            test: "a_start_downloads_the_boxes_its_vms_boot_on_its_provider_before_vagrant_up",
            evidence: "vagrant box add bento/debian-12 --provider virtualbox",
        },
    ),
    (
        "vagrant",
        "run_argv",
        Witness {
            test: "an_up_refused_by_a_machines_lock_runs_again_and_is_no_leftover",
            evidence: "vagrant up --provider virtualbox",
        },
    ),
    (
        "vagrant",
        "preflight",
        Witness {
            test: "vagrant_attach_boots_the_access_vm_with_the_picked_box",
            evidence: "plugin list",
        },
    ),
    ("vagrant", "prepare", NoTool("creates the run's own folder the dotfile goes in")),
    (
        "vagrant",
        "env",
        Witness {
            test: "vagrants_record_lives_in_the_runs_state_folder",
            evidence: "VAGRANT_DOTFILE_PATH",
        },
    ),
    (
        "vagrant",
        "observe",
        Witness {
            test: "a_vm_run_comes_up_is_checked_parked_resumed_and_goes",
            evidence: "RunState::Parked",
        },
    ),
    (
        "vagrant",
        "after_down",
        NoTool("nothing: the manifest's own `vagrant.reset_state` clears the state"),
    ),
    (
        "vagrant",
        "restart",
        Witness {
            test: "an_update_restarts_only_the_running_vm_whose_boot_settings_changed",
            evidence: "vagrant reload web --no-provision",
        },
    ),
    (
        "vagrant",
        "attach",
        Witness {
            test: "vagrant_attach_boots_the_access_vm_with_the_picked_box",
            evidence: "ACCESS_BOX",
        },
    ),
    (
        "vagrant",
        "retire",
        Witness {
            test: "retiring_a_vagrant_machine_destroys_only_its_define",
            evidence: "destroy -f user",
        },
    ),
    (
        "vagrant",
        "console_command",
        Witness {
            test: "a_console_opens_any_vagrant_define_by_name",
            evidence: "login_shell_as(\"user\")",
        },
    ),
    (
        "vagrant",
        "in_guest",
        Witness {
            test: "a_console_into_a_service_goes_through_the_vm_into_its_container",
            evidence: "[\"ssh\", \"default\", \"-c\"]",
        },
    ),
    // Terraform.
    (
        "terraform",
        "action:TerraformClearLock",
        Witness {
            test: "removing_what_isnt_there_is_fine",
            evidence: "LOCK_FILE",
        },
    ),
    (
        "terraform",
        "action:TerraformReadState",
        Witness {
            test: "a_state_that_cannot_be_read_is_an_error_not_an_empty_state",
            evidence: "read(dir.path())",
        },
    ),
    (
        "terraform",
        "action:TerraformCopyModule",
        Witness {
            test: "the_labs_own_module_is_copied_into_the_run_and_destroyed_as_applied",
            evidence: "the start copied the module",
        },
    ),
    (
        "terraform",
        "gone",
        Witness {
            test: "a_destroy_that_failed_on_a_resource_already_gone_drops_it_and_destroys_again",
            evidence: "\"state\", \"rm\"",
        },
    ),
    (
        "terraform",
        "forget_gone",
        Witness {
            test: "a_destroy_that_failed_on_a_resource_already_gone_drops_it_and_destroys_again",
            evidence: "terraform destroy",
        },
    ),
    (
        "terraform",
        "reapplies",
        Witness {
            test: "an_apply_that_converges_on_a_second_try_is_applied_again",
            evidence: "terraform apply",
        },
    ),
    ("terraform", "preflight", NoTool("checks the module has a `.tf` file, on disk")),
    ("terraform", "prepare", NoTool("creates the runner's plugin cache folder")),
    ("terraform", "env", NoTool("names the plugin cache and the run's expiry for the calls")),
    (
        "terraform",
        "observe",
        Witness {
            test: "an_applied_state_runs_every_machine_and_publishes_at_the_routers_address",
            evidence: "MachineState::Running",
        },
    ),
    ("terraform", "after_down", NoTool("forgets the run's state files, on disk")),
    ("terraform", "attach", NoTool("refused: the module makes every machine at up")),
    (
        "terraform",
        "retire",
        NoTool("nothing for a machine the module makes (the update's apply removes it); refused otherwise"),
    ),
    (
        "terraform",
        "console_command",
        Witness {
            test: "a_console_over_ssh_opens_as_the_account_asked",
            evidence: "admin@198.51.100.10",
        },
    ),
    (
        "terraform",
        "in_guest",
        Witness {
            test: "a_guest_step_waits_for_its_machine_to_answer_over_ssh",
            evidence: "doesn't answer over SSH yet",
        },
    ),
];

/// The claims whose witness is owed: each a test to write (a scripted process seeing the call),
/// and the list only shrinks. A claim added to a driver never goes here: it comes with its test.
const OWED: [(&str, &str, &str); 2] = [
    (
        "compose",
        "before_down",
        "a down records the project's anonymous volumes (`docker volume ls`) before its steps",
    ),
    ("vagrant", "elevates", "a Hyper-V run's management commands go through the elevated broker"),
];

/// What `impl Driver for …` in `text` claims: each method it defines, each action it matches.
fn claims_in(text: &str) -> Vec<String> {
    let Some(start) = text.find("impl Driver for ") else { return Vec::new() };
    let block = text.get(start..).unwrap_or("");
    let block = block.get(..block.find("\n}").unwrap_or(block.len())).unwrap_or("");
    let mut found = Vec::new();
    for line in block.lines() {
        let line = line.trim_start();
        let line = line.strip_prefix("async ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("fn ") {
            found.extend(rest.split(['(', '<']).next().map(str::to_owned));
        }
        if let Some(rest) = line.strip_prefix("Action::")
            && let Some((name, _)) = rest.split_once(" =>")
        {
            found.push(format!("action:{name}"));
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
        let text = "impl Driver for Compose {\n    async fn attach(&self) {}\n    fn shiny(&self) {}\n    fn action(&self, a: Action) {\n        match a {\n            Action::ComposePinPorts => x,\n            Action::ComposeNew => y,\n        }\n    }\n}\nfn outside() {}\n";
        let claims = claims_in(text);
        assert_eq!(claims, ["attach", "shiny", "action", "action:ComposePinPorts", "action:ComposeNew"]);
        let tests = [source::File {
            path: "t.rs".to_owned(),
            text: "#[test]\nfn attach_brings_the_access_service_up_in_its_profile_with_the_picked_image() { x(\"--profile access up\") }".to_owned(),
        }];
        let found = unproven(&[("compose", claims)], &tests);
        for want in ["claims `shiny`", "claims `action:ComposeNew`", "claims `action` with"] {
            assert!(found.iter().any(|f| f.contains(want)), "{want}: {found:#?}");
        }
        assert!(found.iter().any(|f| f.contains("no longer claims `retire`")), "{found:#?}");
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
                let mut claims = claims_in(&source::code(&text));
                // `action` is the dispatcher of the `action:` claims, not one of its own.
                claims.retain(|c| c != "action");
                (*name, claims)
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
