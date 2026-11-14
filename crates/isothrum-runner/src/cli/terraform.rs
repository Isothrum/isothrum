//! The Terraform adapter: `init`, `apply` and `destroy` of a module with its variables file, what
//! `show -json` says of the state ([`Shown`]: managed resources apart from data sources), its
//! `output -json` ([`Output`]), a held state lock read as [`Failure::StateLocked`], and its
//! release.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "terraform";

/// A failed Terraform call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Another process holds the state's lock (`Error acquiring the state lock`), with the lock's
    /// id when it says one: `force-unlock` takes it.
    StateLocked {
        /// The lock's id.
        id: Option<String>,
    },
    /// Any other failure: its reason.
    Other(String),
}

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if !said_any(out, &["error acquiring the state lock"]) {
            return Failure::Other(out.reason());
        }
        // `  ID:        2f4b…`, in the lock's details (Terraform 1.x).
        let id = out.stdout.iter().chain(&out.stderr_tail).find_map(|l| {
            let l = l.trim_start_matches(['│', ' ', '\t']).trim();
            l.strip_prefix("ID:").map(|id| id.trim().to_owned()).filter(|id| !id.is_empty())
        });
        Failure::StateLocked { id }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::StateLocked { id: Some(id) } => write!(f, "another process holds the state's lock ({id})"),
            Failure::StateLocked { id: None } => f.write_str("another process holds the state's lock"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// `init -input=false`.
pub(crate) async fn init(call: &Call<'_>) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["init", "-input=false"]), Failure::of).await.map(drop)
}

/// `apply` (or `destroy`, when `destroy`) without asking, with `vars` as its variables file.
pub(crate) async fn apply(call: &Call<'_>, vars: Option<&Path>, destroy: bool) -> Result<(), CliError<Failure>> {
    let mut args = argv([if destroy { "destroy" } else { "apply" }, "-auto-approve", "-input=false"]);
    if let Some(vars) = vars {
        args.push(format!("-var-file={}", vars.display()));
    }
    call.ok(TOOL, args, Failure::of).await.map(drop)
}

/// `force-unlock -force <id>`.
pub(crate) async fn force_unlock(call: &Call<'_>, id: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["force-unlock", "-force", id]), Failure::of).await.map(drop)
}

/// One resource of the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    /// Its address (`module.web.aws_instance.vm`).
    pub(crate) address: String,
    /// Its type (`aws_instance`).
    pub(crate) kind: String,
    /// Its provider's id for it, when it has one.
    pub(crate) id: Option<String>,
}

/// What `show -json` says of the state: its managed resources (what the run made, what may cost)
/// apart from its data sources (what it only read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    /// `mode == "managed"`.
    pub(crate) managed: Vec<Resource>,
    /// `mode == "data"`: their addresses.
    pub(crate) data: Vec<String>,
}

#[derive(Deserialize)]
struct ShowDoc {
    #[serde(default)]
    values: Option<Values>,
}

#[derive(Deserialize)]
struct Values {
    #[serde(default)]
    root_module: Module,
}

#[derive(Default, Deserialize)]
struct Module {
    #[serde(default)]
    resources: Vec<ResourceDoc>,
    #[serde(default)]
    child_modules: Vec<Module>,
}

#[derive(Deserialize)]
struct ResourceDoc {
    address: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Option<IdOnly>,
}

#[derive(Deserialize)]
struct IdOnly {
    #[serde(default)]
    id: Option<serde_json::Value>,
}

/// `show -json`'s state, read: an empty state (no `values`) has nothing; a mode other than
/// `managed` or `data` is unexpected, never skipped.
pub(crate) fn shown(json: &str) -> Result<Shown, String> {
    let doc: ShowDoc = serde_json::from_str(json).map_err(|e| format!("`show -json` isn't the state's JSON: {e}"))?;
    let mut out = Shown::default();
    let mut pending = vec![doc.values.map(|v| v.root_module).unwrap_or_default()];
    while let Some(module) = pending.pop() {
        for r in module.resources {
            match r.mode.as_str() {
                "managed" => {
                    let id = r.values.and_then(|v| v.id).and_then(|id| match id {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    });
                    out.managed.push(Resource {
                        address: r.address,
                        kind: r.kind,
                        id,
                    });
                }
                "data" => out.data.push(r.address),
                other => return Err(format!("resource `{}` of mode `{other}`", r.address)),
            }
        }
        pending.extend(module.child_modules);
    }
    Ok(out)
}

/// The state as `show -json` gives it.
pub(crate) async fn show(call: &Call<'_>) -> Result<Shown, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["show", "-json"]), Failure::of).await?;
    shown(&out.stdout.join("\n")).map_err(|what| CliError::Unexpected { tool: TOOL, what })
}

/// One output of the module. Its value is the module's own shape (a string, a map, a list), so it
/// stays JSON.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Output {
    /// Its value.
    pub(crate) value: serde_json::Value,
    /// Whether the module marks it sensitive.
    #[serde(default)]
    pub(crate) sensitive: bool,
}

/// `output -json`'s outputs, by name.
pub(crate) async fn outputs(call: &Call<'_>) -> Result<BTreeMap<String, Output>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["output", "-json"]), Failure::of).await?;
    serde_json::from_str(&out.stdout.join("\n")).map_err(|e| CliError::Unexpected {
        tool: TOOL,
        what: format!("`output -json` isn't the outputs' JSON: {e}"),
    })
}

/// `terraform version -json`'s answer.
#[derive(Deserialize)]
struct VersionDoc {
    terraform_version: String,
}

/// `terraform version -json`.
pub(crate) async fn version(call: &Call<'_>) -> Result<isoloom_manifest::Version, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["version", "-json"]), Failure::of).await?;
    let doc: VersionDoc = serde_json::from_str(&out.stdout.join("\n")).map_err(|e| CliError::Unexpected {
        tool: TOOL,
        what: format!("`version -json` isn't its JSON: {e}"),
    })?;
    super::version(TOOL, &doc.terraform_version, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Exit;

    #[tokio::test]
    async fn the_version_is_read_from_its_json() {
        use crate::process::fake::{FakeProcesses, Reply};
        let fake = FakeProcesses::rules(&[(
            "terraform version",
            Reply::ok(include_str!("../tool_output/fixtures/terraform/1.9-version.json")),
        )]);
        let read = version(&crate::cli::testing::call(&fake)).await.unwrap();
        assert_eq!(read.as_str(), "1.9.8");
    }

    #[test]
    fn a_held_lock_is_read_with_its_id() {
        let stderr = include_str!("../tool_output/fixtures/terraform/1.9-state-locked.txt")
            .lines()
            .map(str::to_owned)
            .collect();
        let out = Captured::new(TOOL, Exit::new(Some(1)), Vec::new(), stderr);
        assert_eq!(
            Failure::of(&out),
            Failure::StateLocked {
                id: Some("9db590f1-b6fe-c5f2-2678-8804f089deba".into())
            }
        );
        let other = Captured::new(TOOL, Exit::new(Some(1)), Vec::new(), vec!["Error: Invalid provider configuration".into()]);
        assert!(matches!(Failure::of(&other), Failure::Other(_)));
    }

    #[test]
    fn show_reads_managed_resources_apart_from_data_sources() {
        let read = shown(include_str!("../tool_output/fixtures/terraform/1.9-show.json")).unwrap();
        assert_eq!(read.data, ["data.aws_ami.debian"]);
        assert_eq!(
            read.managed,
            [
                Resource {
                    address: "aws_security_group.lab".into(),
                    kind: "aws_security_group".into(),
                    id: Some("sg-0a1b2c3d".into())
                },
                Resource {
                    address: "module.web.aws_instance.vm".into(),
                    kind: "aws_instance".into(),
                    id: Some("i-0123456789abcdef0".into())
                },
            ]
        );
        assert_eq!(shown("{\"format_version\":\"1.0\"}").unwrap(), Shown::default(), "an empty state");
        assert!(
            shown("{\"values\":{\"root_module\":{\"resources\":[{\"address\":\"x\"").is_err(),
            "a cut answer"
        );
    }
}
