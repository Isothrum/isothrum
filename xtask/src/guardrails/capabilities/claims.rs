//! Each driver's claims and how each is shown: the table the capabilities guardrail reads.

use super::Proof::{self, Contract, NoTool, Witness};

/// Each driver's claims (the `FormatDriver` methods it defines) and how each is shown.
pub(super) const CLAIMS: [(&str, &str, Proof); 54] = [
    (
        "compose",
        "probe",
        Contract {
            clause: "probe_reads_the_version",
            evidence: r#"["compose", "version", "--short"]"#,
        },
    ),
    (
        "compose",
        "up",
        Witness {
            test: "up_waits_for_the_services_and_names_the_project_after_the_run",
            evidence: "expect_times(UP, 1",
        },
    ),
    (
        "compose",
        "down",
        Witness {
            test: "down_takes_the_volumes_then_whatever_still_carries_the_label",
            evidence: "down --volumes --remove-orphans",
        },
    ),
    (
        "compose",
        "park",
        Witness {
            test: "park_pauses_or_stops_and_resume_unpauses_what_was_paused_then_starts_without_remaking",
            evidence: "docker compose -p lab -f * pause",
        },
    ),
    (
        "compose",
        "resume",
        Witness {
            test: "park_pauses_or_stops_and_resume_unpauses_what_was_paused_then_starts_without_remaking",
            evidence: "unpause",
        },
    ),
    ("compose", "update", NoTool("the same `up`, which its witness shows (`Compose::up`)")),
    (
        "compose",
        "machines",
        Witness {
            test: "machines_are_the_services_the_files_declare_and_reapply_is_one_up",
            evidence: "config --services",
        },
    ),
    (
        "compose",
        "reapply",
        Witness {
            test: "machines_are_the_services_the_files_declare_and_reapply_is_one_up",
            evidence: "reapplied",
        },
    ),
    ("compose", "provision", NoTool("refused: a Compose project has no provisioning to run again")),
    (
        "compose",
        "attach",
        Witness {
            test: "attach_names_the_service_so_its_profile_starts_it_and_retire_removes_it_with_its_volumes",
            evidence: "up --detach --wait attackbox",
        },
    ),
    (
        "compose",
        "retire",
        Witness {
            test: "attach_names_the_service_so_its_profile_starts_it_and_retire_removes_it_with_its_volumes",
            evidence: "rm --stop --force --volumes attackbox",
        },
    ),
    (
        "compose",
        "exec",
        Witness {
            test: "exec_runs_in_the_running_container_or_as_a_one_off_and_says_how_it_ended",
            evidence: "exec -T web cat /flag",
        },
    ),
    (
        "compose",
        "console",
        NoTool("a query: the command an embedder runs with a terminal, nothing runs"),
    ),
    (
        "compose",
        "observe",
        Witness {
            test: "observe_reads_each_service_and_its_published_ports_and_derives_the_runs_state",
            evidence: "ps --all --format json",
        },
    ),
    (
        "compose",
        "found",
        Witness {
            test: "the_resources_are_what_the_project_label_marks_by_id",
            evidence: "found",
        },
    ),
    (
        "compose",
        "destroy_by_id",
        Witness {
            test: "the_resources_are_what_the_project_label_marks_by_id",
            evidence: "docker volume rm v9",
        },
    ),
    (
        "vagrant",
        "elevates",
        NoTool("a query: whether a provider's commands go through the elevated session (hyper_v_takes_its_commands_elevated)"),
    ),
    (
        "vagrant",
        "probe",
        Witness {
            test: "probe_refuses_a_provider_this_host_doesnt_run_a_missing_plugin_and_an_old_vagrant",
            evidence: "vagrant plugin list --machine-readable",
        },
    ),
    (
        "vagrant",
        "up",
        Witness {
            test: "up_runs_on_the_provider_asked_with_the_dotfile_in_the_state_folder_and_the_key_in_the_environment",
            evidence: "vagrant up --provider libvirt",
        },
    ),
    (
        "vagrant",
        "down",
        Witness {
            test: "down_destroys_then_resets_the_dotfile_and_a_failed_destroy_keeps_it",
            evidence: "vagrant destroy -f",
        },
    ),
    (
        "vagrant",
        "park",
        Witness {
            test: "park_suspends_or_halts_and_esxi_refusing_a_vm_already_off_is_halted",
            evidence: "vagrant suspend",
        },
    ),
    (
        "vagrant",
        "resume",
        Witness {
            test: "resume_boots_without_provisioning_and_update_and_provision_provision",
            evidence: "--no-provision",
        },
    ),
    (
        "vagrant",
        "update",
        Witness {
            test: "resume_boots_without_provisioning_and_update_and_provision_provision",
            evidence: "vagrant up --provider libvirt --provision",
        },
    ),
    (
        "vagrant",
        "machines",
        Witness {
            test: "reapply_adds_removes_and_provisions_machine_by_machine",
            evidence: "vagrant status --machine-readable",
        },
    ),
    (
        "vagrant",
        "reapply",
        Witness {
            test: "reapply_adds_removes_and_provisions_machine_by_machine",
            evidence: "vagrant destroy -f old",
        },
    ),
    (
        "vagrant",
        "provision",
        Witness {
            test: "resume_boots_without_provisioning_and_update_and_provision_provision",
            evidence: "provision",
        },
    ),
    (
        "vagrant",
        "attach",
        Witness {
            test: "attach_boots_one_machine_and_retire_destroys_it",
            evidence: "vagrant up --provider libvirt attack",
        },
    ),
    (
        "vagrant",
        "retire",
        Witness {
            test: "attach_boots_one_machine_and_retire_destroys_it",
            evidence: "vagrant destroy -f attack",
        },
    ),
    (
        "vagrant",
        "exec",
        Witness {
            test: "exec_runs_the_command_over_ssh_quoted_and_a_failing_command_is_its_result",
            evidence: "vagrant ssh web -c",
        },
    ),
    (
        "vagrant",
        "console",
        NoTool("a query: the command an embedder runs with a terminal, nothing runs"),
    ),
    (
        "vagrant",
        "observe",
        Witness {
            test: "observe_reads_each_machine_and_the_ports_of_those_running_without_guessing_a_protocol",
            evidence: "vagrant port web --machine-readable",
        },
    ),
    (
        "vagrant",
        "found",
        NoTool("reads the run's dotfile, a file Vagrant wrote: no tool runs (found_is_the_dotfiles_machine_ids_by_machine)"),
    ),
    (
        "vagrant",
        "destroy_by_id",
        Witness {
            test: "destroy_by_id_of_a_machine_no_longer_defined_goes_by_id_at_virtualbox_and_gone_is_fine",
            evidence: "VBoxManage unregistervm uuid-old --delete",
        },
    ),
    (
        "vagrant",
        "graphical",
        Witness {
            test: "each_provider_has_its_own_app_or_none",
            evidence: "Provider::Virtualbox",
        },
    ),
    ("vagrant", "native_app", NoTool("a query: the app a provider's VMs open in, nothing runs")),
    (
        "terraform",
        "ready_by_observation",
        NoTool(
            "a query: whether a status may find a failed run ready (a_failed_apply_leaves_the_run_failed_whatever_its_state_holds_and_the_next_up_applies_again)",
        ),
    ),
    (
        "terraform",
        "probe",
        Contract {
            clause: "probe_reads_the_version",
            evidence: r#"["version", "-json"]"#,
        },
    ),
    (
        "terraform",
        "up",
        Witness {
            test: "up_inits_then_applies_on_the_runs_own_state_with_inputs_in_the_environment_only",
            evidence: "terraform init -input=false -no-color",
        },
    ),
    (
        "terraform",
        "down",
        Witness {
            test: "a_destroy_failing_for_another_reason_fails_and_keeps_the_state",
            evidence: "terraform destroy",
        },
    ),
    (
        "terraform",
        "park",
        NoTool("refused: a module has no stopped state Terraform keeps (what_a_module_cant_do_is_refused_and_nothing_runs)"),
    ),
    (
        "terraform",
        "resume",
        NoTool("refused: a module has no stopped state Terraform keeps (what_a_module_cant_do_is_refused_and_nothing_runs)"),
    ),
    (
        "terraform",
        "update",
        Witness {
            test: "update_is_the_same_apply",
            evidence: "terraform apply -auto-approve",
        },
    ),
    (
        "terraform",
        "machines",
        Witness {
            test: "machines_are_the_vms_the_state_keeps_and_those_the_plan_makes",
            evidence: "terraform show -json",
        },
    ),
    (
        "terraform",
        "reapply",
        Witness {
            test: "reapply_plans_then_applies_and_writes_no_plan_file",
            evidence: "terraform plan -input=false -json",
        },
    ),
    (
        "terraform",
        "provision",
        NoTool("refused: Terraform has no provisioning of its own to run again"),
    ),
    ("terraform", "attach", NoTool("refused: a module's machines are all applied together")),
    ("terraform", "retire", NoTool("refused: a module's machines are all applied together")),
    ("terraform", "exec", NoTool("refused: Terraform reaches no machine to run a command in")),
    (
        "terraform",
        "console",
        Witness {
            test: "a_vms_console_is_ssh_as_the_user_the_outputs_name_with_the_runs_key_and_its_host_pinned",
            evidence: "admin@203.0.113.7",
        },
    ),
    (
        "terraform",
        "observe",
        Witness {
            test: "observe_reads_the_vms_and_the_outputs_sensitivity_kept",
            evidence: "terraform show -json -no-color",
        },
    ),
    (
        "terraform",
        "found",
        Witness {
            test: "found_is_every_managed_instance_by_address_and_id",
            evidence: "terraform show -json",
        },
    ),
    (
        "terraform",
        "destroy_by_id",
        Contract {
            clause: "found_ids_are_destroyed_by_id_and_gone_is_done",
            evidence: r#"strip_prefix("-target=")"#,
        },
    ),
    (
        "terraform",
        "graphical",
        Witness {
            test: "only_a_proxmox_vm_offers_a_screen",
            evidence: "proxmox_virtual_environment_vm",
        },
    ),
    ("terraform", "native_app", NoTool("a query: the app a module's VMs open in, nothing runs")),
];

/// The claims whose witness is owed: each a test to write (a scripted process seeing the call),
/// and the list only shrinks. A claim added to a driver never goes here: it comes with its test.
pub(super) const OWED: [(&str, &str, &str); 4] = [
    (
        "vagrant",
        "reach_graphical",
        "a VM's screen read from its provider (VBoxManage's remote display, virsh domdisplay) by a scripted process",
    ),
    ("vagrant", "native_console", "a VM's own window opened through its provider's app"),
    (
        "terraform",
        "reach_graphical",
        "a Proxmox VE VM's VNC websocket asked of its API (curl's vncproxy) from the module's state",
    ),
    ("terraform", "native_console", "a Proxmox VE VM's web console address from the module's state"),
];
