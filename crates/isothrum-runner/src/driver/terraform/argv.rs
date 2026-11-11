//! What a manifest step's argv asks Terraform (or OpenTofu) to do: its subcommand, the one thing
//! of a step the driver reads, for a destroy whose resources are already gone ([`super::gone`])
//! and an apply a cloud's eventual consistency fails ([`applied`]).

use isoloom_manifest::{Argv, HostArgv, Run, Step};

use crate::error::Error;

/// The subcommand `argv` runs when it is Terraform's or OpenTofu's, with its index among the
/// arguments: the first argument that isn't an option (`apply` in `terraform -chdir=m apply`).
pub(crate) fn subcommand(argv: &Argv) -> Option<(usize, String)> {
    if !crate::process::TERRAFORM_PROGRAMS.contains(&argv.program().as_str()) {
        return None;
    }
    argv.args().iter().map(ToString::to_string).enumerate().find(|(_, w)| !w.starts_with('-'))
}

/// Whether `e` is the failure of a step of `up` that runs `terraform apply`.
pub(crate) fn applied(up: &[Step], e: &Error) -> bool {
    let Error::ToolFailed { step, .. } = e else { return false };
    up.iter().filter(|s| s.id.as_str() == step).any(|s| match &s.run {
        Run::Argv(argv) => argv_applies(argv),
        Run::Action { .. } => false,
    })
}

/// Whether `argv` (on any host) is `terraform apply …` (or `tofu`), global options such as
/// `-chdir=<module>` before the subcommand.
fn argv_applies(argv: &HostArgv) -> bool {
    let applies = |a: &Argv| subcommand(a).is_some_and(|(_, s)| s == "apply");
    crate::executor::plan::every_argv(argv).any(applies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(json: &str) -> HostArgv {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_apply_is_terraforms_apply_subcommand_wherever_its_module() {
        assert!(argv_applies(&argv(r#"["terraform", "apply", "-auto-approve"]"#)));
        assert!(argv_applies(&argv(
            r#"["terraform", "-chdir=${run.state_dir}/module", "apply", "-auto-approve"]"#
        )));
        assert!(argv_applies(&argv(r#"["tofu", "apply"]"#)));
        assert!(!argv_applies(&argv(r#"["terraform", "destroy", "-auto-approve"]"#)));
        assert!(
            !argv_applies(&argv(r#"["terraform", "-chdir=apply", "init"]"#)),
            "a folder named apply isn't the subcommand"
        );
        assert!(!argv_applies(&argv(r#"["ssh", "apply"]"#)));
        assert!(argv_applies(&argv(r#"{"default": ["terraform", "apply"], "windows": ["terraform", "apply"]}"#)));
    }
}
