//! Host quirks of starting a tool.
//!
//! - **Windows wrappers.** Some tools ship as a `.cmd` wrapper, not an `.exe`: `CreateProcessW`
//!   only appends `.exe`, so they go through `cmd /C`. cmd.exe parses the rest of its command
//!   line, so an argument holding one of its operators is refused, on every OS, so a new call with
//!   an unchecked value fails in the tests instead of becoming an injection on Windows.
//! - **Docker's engine.** A process that started the runner can carry a stale `DOCKER_HOST` or
//!   `DOCKER_CONTEXT` (OrbStack's socket while the user works in Docker Desktop): every run would
//!   then go to an engine the user never sees. When the user picked a context of their own, those
//!   variables are dropped for `docker`, so it follows `docker context show`.
//! - **Terraform's ambient cloud.** A runner started from a shell carries that shell's cloud
//!   settings (`AWS_PROFILE`, `AWS_REGION`, `ARM_SUBSCRIPTION_ID`, `GOOGLE_PROJECT`, a stray
//!   `TF_VAR_region`): a destroy would then go wherever the shell points now, not where the run
//!   was applied. Terraform inherits none of them; the manifest's params and connection
//!   slots, delivered to its steps, are the only cloud settings it gets.

/// The tools run through `cmd /C` on Windows.
pub(super) const SHIMMED: &[&str] = &["az", "gcloud", "oci"];

/// What cmd.exe gives a meaning to outside quotes.
const CMD_OPERATORS: &[char] = &['&', '|', '<', '>', '^', '%', '"', '\n', '\r'];

/// cmd.exe's exit code when it finds no such command.
const CMD_NOT_FOUND: i32 = 9009;

/// The variables that redirect the Docker CLI to another engine than the current context.
pub(super) const ENGINE_OVERRIDES: [&str; 2] = ["DOCKER_HOST", "DOCKER_CONTEXT"];

/// Terraform's programs (OpenTofu's too): the tools that never inherit the runner's cloud
/// settings.
pub(crate) const TERRAFORM_PROGRAMS: [&str; 2] = ["terraform", "tofu"];

/// The prefixes of the cloud settings they don't inherit: each cloud CLI's and Terraform
/// provider's own, and Terraform's variables.
const CLOUD_SETTINGS: [&str; 11] = [
    "AWS_",
    "ARM_",
    "AZURE_",
    "GOOGLE_",
    "CLOUDSDK_",
    "GCLOUD_",
    "DIGITALOCEAN_",
    "LINODE_",
    "OCI_",
    "TF_VAR_",
    "TF_CLI_ARGS",
];

/// The names among `inherited` (the runner's own variables) that `program` doesn't inherit: the
/// cloud settings, for Terraform.
pub(super) fn withheld(program: &str, inherited: impl Iterator<Item = String>) -> Vec<String> {
    if !TERRAFORM_PROGRAMS.contains(&program) {
        return Vec::new();
    }
    inherited.filter(|name| CLOUD_SETTINGS.iter().any(|p| name.starts_with(p))).collect()
}

/// The first argument cmd.exe would interpret, for a tool that goes through it.
pub(super) fn refused_arg<'a>(program: &str, args: &'a [String]) -> Option<&'a str> {
    if !SHIMMED.contains(&program) {
        return None;
    }
    args.iter().find(|a| a.contains(CMD_OPERATORS)).map(String::as_str)
}

/// Whether `program`, run through cmd.exe, isn't installed: cmd started, then exited with its own
/// "not found" code.
pub(super) fn missing_behind_cmd(program: &str, code: Option<i32>) -> bool {
    SHIMMED.contains(&program) && code == Some(CMD_NOT_FOUND)
}

/// Whether `program` talks to the Docker engine (`docker`, `docker compose` included).
pub(super) fn uses_engine(program: &str) -> bool {
    program == "docker"
}

/// Whether the inherited engine overrides go: only when the user picked a context of their own
/// (`currentContext` in the CLI config, not `default`). Without one, an inherited `DOCKER_HOST` is
/// how the user reaches their engine (rootless Docker sets it), and stripping it pointed the CLI
/// at the root daemon.
pub(super) fn strips_engine_overrides(cli_config: Option<&str>) -> bool {
    cli_config
        .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
        .and_then(|v| v.get("currentContext").and_then(|c| c.as_str()).map(|c| !c.is_empty() && c != "default"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_cmd_operator_in_an_argument_is_refused() {
        assert_eq!(refused_arg("gcloud", &args(&["auth", "list", "--filter=status:ACTIVE"])), None);
        for bad in ["x & calc", "a|b", "out>f", "<in", "^x", "%PATH%", "say \"hi\"", "a\nb", "a\rb"] {
            assert_eq!(refused_arg("az", &args(&["group", "create", "--name", bad])), Some(bad));
        }
        assert_eq!(refused_arg("docker", &args(&["a & b"])), None, "only the wrapped tools");
    }

    #[test]
    fn a_wrapped_tool_cmd_cant_find_is_missing() {
        for tool in ["az", "gcloud", "oci"] {
            assert!(missing_behind_cmd(tool, Some(9009)) && !missing_behind_cmd(tool, Some(1)) && !missing_behind_cmd(tool, None));
        }
        assert!(!missing_behind_cmd("terraform", Some(9009)));
    }

    #[test]
    fn terraform_inherits_no_cloud_setting_of_the_runners() {
        // A destroy went where the shell's profile and region pointed, not where it applied.
        let inherited = [
            "PATH",
            "HOME",
            "AWS_PROFILE",
            "AWS_REGION",
            "ARM_SUBSCRIPTION_ID",
            "GOOGLE_PROJECT",
            "TF_VAR_region",
            "TF_CLI_ARGS_apply",
            "OCI_CLI_PROFILE",
            "LINODE_TOKEN",
        ];
        let names = || inherited.iter().map(|s| (*s).to_owned());
        assert_eq!(
            withheld("terraform", names()),
            [
                "AWS_PROFILE",
                "AWS_REGION",
                "ARM_SUBSCRIPTION_ID",
                "GOOGLE_PROJECT",
                "TF_VAR_region",
                "TF_CLI_ARGS_apply",
                "OCI_CLI_PROFILE",
                "LINODE_TOKEN"
            ]
        );
        assert_eq!(withheld("tofu", names()).len(), 8);
        assert!(withheld("aws", names()).is_empty(), "a cloud's own CLI reads its settings");
    }

    #[test]
    fn engine_overrides_go_only_with_a_context_the_user_picked() {
        assert!(strips_engine_overrides(Some(r#"{"currentContext":"desktop-linux"}"#)));
        assert!(!strips_engine_overrides(Some(r#"{"currentContext":"default"}"#)));
        assert!(!strips_engine_overrides(Some(r#"{"auths":{}}"#)));
        assert!(!strips_engine_overrides(Some("not json")));
        assert!(!strips_engine_overrides(None));
        assert!(uses_engine("docker") && !uses_engine("vagrant"));
    }
}
