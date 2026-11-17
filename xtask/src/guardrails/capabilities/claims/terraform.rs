//! Terraform's claims and how each is shown, beside Compose's and Vagrant's ([`super::CLAIMS`]).

use super::super::Proof::{self, Contract, NoTool, Witness};

/// Terraform's claims (the `FormatDriver` methods its driver defines) and how each is shown.
pub(in super::super) const TERRAFORM: [(&str, &str, Proof); 20] = [
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
        NoTool("refused: a module has no stopped state Terraform keeps (the contract's what_it_refuses_is_unsupported)"),
    ),
    (
        "terraform",
        "resume",
        NoTool("refused: a module has no stopped state Terraform keeps (the contract's what_it_refuses_is_unsupported)"),
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
    (
        "terraform",
        "exec",
        Witness {
            test: "a_command_runs_in_a_vm_behind_the_modules_router_through_the_runs_jump_first",
            evidence: "ssh -F none",
        },
    ),
    (
        "terraform",
        "published",
        Witness {
            test: "without_a_jump_host_each_endpoint_is_where_the_deployment_publishes_it",
            evidence: "203.0.113.7:8080",
        },
    ),
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
