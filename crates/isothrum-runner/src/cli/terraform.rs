//! The Terraform adapter: a module's `init`, its `apply`, `destroy` and `plan` through Terraform's
//! machine-readable UI (`-json`: each line an event, shown as its message, and each error a typed
//! [`Diagnostic`] naming its resource), the state as `show -json` says it ([`Shown`]: managed
//! resources apart from data sources), the module's `output -json` ([`Output`]), `state rm`,
//! `import`, a held state lock read as [`Failure::StateLocked`] and its release, and the backend
//! `init` chose ([`backend`]).
//!
//! Where the state is ([`StateAt`]): a file of the run's own (`-state=<file>`, the local backend's
//! path, so two copies of one module never share a state), or the module's own backend when it
//! declares one that isn't local. No variable is ever passed on the command line or in a file: a
//! module's inputs reach it as `TF_VAR_<name>` in its environment only.

mod state;
pub(crate) use state::{Resource, Shown, shown};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Call, CliError, argv, said_any};
use crate::events::Stream;
use crate::process::Captured;

/// The tool.
const TOOL: &str = "terraform";

/// The oldest Terraform the adapter drives: the first whose machine-readable UI (`-json` on
/// `apply`, `destroy` and `plan`) and `show -json` are covered by its compatibility promise.
const MIN_VERSION: &str = "1.0.0";

/// What Terraform says when another process holds the state's lock, in any backend.
const LOCKED: &str = "error acquiring the state lock";

/// What `import` says when the object to import doesn't exist.
const NO_SUCH_OBJECT: &str = "cannot import non-existent remote object";

/// How a provider's error says the object it was asked to act on doesn't exist (any case): the
/// wordings of the providers' delete and read errors (`no such IPSet`, `Configuration file …
/// does not exist`, `404 Not Found`, `InvalidInstanceID.NotFound`).
const MISSING: [&str; 5] = ["no such ", "does not exist", "doesn't exist", "not found", "notfound"];

/// What a provisioner's error says in its summary (`local-exec provisioner error`): its command's
/// words, never its provider's, so it never reads as an object gone.
const PROVISIONER: &str = "provisioner";

/// The file `init` keeps the module's chosen backend in, in Terraform's data folder.
const BACKEND_FILE: &str = "terraform.tfstate";

/// A failed Terraform call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Another process holds the state's lock (`Error acquiring the state lock`), with the lock's
    /// id when it says one: `force-unlock` takes it.
    StateLocked {
        /// The lock's id.
        id: Option<String>,
    },
    /// `import` found no object of that id.
    NoSuchObject,
    /// The errors a `-json` call reported, each naming its resource when it has one.
    Errors(Vec<Diagnostic>),
    /// Any other failure: its reason.
    Other(String),
}

/// One error Terraform reported in its machine-readable UI.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Diagnostic {
    /// Its one-line summary.
    #[serde(default)]
    pub(crate) summary: String,
    /// Its detail (several lines, or none).
    #[serde(default)]
    pub(crate) detail: String,
    /// The resource instance it is about (`module.lab.aws_instance.vm["web"]`), when it is about
    /// one.
    #[serde(default)]
    pub(crate) address: Option<String>,
}

impl Diagnostic {
    /// Whether it says the object its resource stands for doesn't exist any more (a provider's
    /// own words, never a provisioner's): what a destroy that can't finish because the object is
    /// already gone fails with.
    pub(crate) fn says_missing(&self) -> bool {
        let (summary, detail) = (self.summary.to_lowercase(), self.detail.to_lowercase());
        self.address.is_some() && !summary.contains(PROVISIONER) && MISSING.iter().any(|w| summary.contains(w) || detail.contains(w))
    }
}

impl Failure {
    /// What a failed call that printed `out` (text) failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if said_any(out, &[NO_SUCH_OBJECT]) {
            return Failure::NoSuchObject;
        }
        if !said_any(out, &[LOCKED]) {
            return Failure::Other(out.reason());
        }
        Failure::StateLocked {
            id: lock_id(out.stdout.iter().chain(&out.stderr_tail).map(String::as_str)),
        }
    }

    /// What a failed `-json` call that printed `out` failed with: its error diagnostics, a held
    /// lock when one of them says so; the text's reading ([`Failure::of`]) when it printed none
    /// (it failed before its UI started: an unknown flag, a crash).
    pub(crate) fn of_events(out: &Captured) -> Failure {
        let errors: Vec<Diagnostic> = out.stdout.iter().filter_map(|l| error_of(l)).collect();
        if errors.is_empty() {
            return Failure::of(out);
        }
        if let Some(locked) = errors.iter().find(|d| d.summary.to_lowercase().contains(LOCKED)) {
            return Failure::StateLocked {
                id: lock_id(locked.detail.lines()),
            };
        }
        Failure::Errors(errors)
    }
}

/// The lock's id among `lines` (`  ID:        2f4b…`, in the lock's details).
fn lock_id<'l>(lines: impl IntoIterator<Item = &'l str>) -> Option<String> {
    lines.into_iter().find_map(|l| {
        let l = l.trim_start_matches(['│', ' ', '\t']).trim();
        l.strip_prefix("ID:").map(|id| id.trim().to_owned()).filter(|id| !id.is_empty())
    })
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::StateLocked { id: Some(id) } => write!(f, "another process holds the state's lock ({id})"),
            Failure::StateLocked { id: None } => f.write_str("another process holds the state's lock"),
            Failure::NoSuchObject => f.write_str("there is no object of that id to import"),
            Failure::Errors(errors) => {
                let each: Vec<String> = errors
                    .iter()
                    .map(|d| {
                        let first = d
                            .detail
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .map(|l| format!(": {}", l.trim()))
                            .unwrap_or_default();
                        match &d.address {
                            Some(at) => format!("{} ({at}){first}", d.summary),
                            None => format!("{}{first}", d.summary),
                        }
                    })
                    .collect();
                f.write_str(&each.join("; "))
            }
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// One line of Terraform's machine-readable UI, as far as the adapter reads it.
#[derive(Deserialize)]
struct Event {
    #[serde(rename = "@message", default)]
    message: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    diagnostic: Option<DiagnosticDoc>,
    #[serde(default)]
    change: Option<ChangeDoc>,
}

#[derive(Deserialize)]
struct DiagnosticDoc {
    #[serde(default)]
    severity: String,
    #[serde(flatten)]
    diagnostic: Diagnostic,
}

#[derive(Deserialize)]
struct ChangeDoc {
    resource: ChangedResource,
    action: String,
}

#[derive(Deserialize)]
struct ChangedResource {
    addr: String,
    resource_type: String,
}

/// The error diagnostic `line` reports, if it is one.
fn error_of(line: &str) -> Option<Diagnostic> {
    let event: Event = serde_json::from_str(line).ok()?;
    let d = event.diagnostic.filter(|_| event.kind == "diagnostic")?;
    (d.severity == "error").then_some(d.diagnostic)
}

/// What a person reads of `line`, a line of the machine-readable UI: its message, and for a
/// diagnostic its detail and the resource it names; a line that isn't an event, as it is.
fn shown_line(line: &str) -> Vec<String> {
    let Ok(event) = serde_json::from_str::<Event>(line) else {
        return vec![line.to_owned()];
    };
    let mut out = vec![event.message];
    if let Some(d) = event.diagnostic {
        if let Some(at) = d.diagnostic.address {
            out.push(format!("  with {at}"));
        }
        out.extend(d.diagnostic.detail.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("  {l}")));
    }
    out
}

/// Runs `args` (a `-json` call) through `call`, its lines shown to `call`'s sink as a person
/// reads them ([`shown_line`]), failing unless it exits 0 ([`Failure::of_events`]).
async fn run_events(call: &Call<'_>, args: Vec<String>) -> Result<Captured, CliError<Failure>> {
    let sink = call.sink;
    let say = move |stream: Stream, line: &str| shown_line(line).iter().for_each(|l| sink(stream, l));
    let shown = Call {
        runner: call.runner,
        cwd: call.cwd.clone(),
        env: call.env.clone(),
        limit: call.limit,
        sink: &say,
    };
    shown.ok(TOOL, args, Failure::of_events).await
}

/// Where a run's state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateAt<'a> {
    /// A file of the run's own: the local backend's state, at this path.
    File(&'a Path),
    /// Wherever the module's own backend keeps it (a backend that isn't local).
    Backend,
}

impl StateAt<'_> {
    /// The option that points a call at the state (`-state=<file>`); none for the backend's.
    fn option(self) -> Option<String> {
        match self {
            StateAt::File(path) => Some(format!("-state={}", path.display())),
            StateAt::Backend => None,
        }
    }
}

/// The backend a module's `init` chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Backend {
    /// The local backend: the one a module without a `backend` block gets, or `backend "local"`.
    Local,
    /// Another (`s3`, `remote`, `http`…), by its name: the state is the backend's.
    Other(String),
}

#[derive(Deserialize)]
struct BackendDoc {
    #[serde(default)]
    backend: Option<BackendType>,
}

#[derive(Deserialize)]
struct BackendType {
    #[serde(rename = "type")]
    kind: String,
}

/// The backend `init` chose, read from what it keeps in `data_dir` (Terraform's data folder,
/// `TF_DATA_DIR`): no file, or no backend in it, is the local backend (a module without a
/// `backend` block).
///
/// # Errors
///
/// The file can't be read, or isn't what `init` writes.
pub(crate) fn backend(data_dir: &Path) -> Result<Backend, String> {
    let path = data_dir.join(BACKEND_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Backend::Local),
        Err(e) => return Err(format!("{} can't be read: {e}", path.display())),
    };
    backend_of(&text).map_err(|e| format!("{} isn't what `init` writes: {e}", path.display()))
}

/// The backend `text` (`init`'s backend file) names.
fn backend_of(text: &str) -> Result<Backend, serde_json::Error> {
    let doc: BackendDoc = serde_json::from_str(text)?;
    Ok(match doc.backend {
        Some(b) if b.kind != "local" => Backend::Other(b.kind),
        _ => Backend::Local,
    })
}

/// `init -input=false -no-color`.
pub(crate) async fn init(call: &Call<'_>) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["init", "-input=false", "-no-color"]), Failure::of).await.map(drop)
}

/// What an [`apply`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change<'a> {
    /// Brings everything to what the module says (`apply`).
    Apply,
    /// Removes everything (`destroy`).
    Destroy,
    /// Removes the one resource instance at this address (`destroy -target=<address>`).
    DestroyOne(&'a str),
}

/// `apply` or `destroy`, as `change` says, without asking, on the state at `state`.
pub(crate) async fn apply(call: &Call<'_>, state: StateAt<'_>, change: Change<'_>) -> Result<(), CliError<Failure>> {
    let verb = if change == Change::Apply { "apply" } else { "destroy" };
    let mut args = argv([verb, "-auto-approve", "-input=false", "-json"]);
    args.extend(state.option());
    if let Change::DestroyOne(address) = change {
        args.push(format!("-target={address}"));
    }
    run_events(call, args).await.map(drop)
}

/// What a plan would do to one resource instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    /// Its address.
    pub(crate) address: String,
    /// Its type (`aws_instance`).
    pub(crate) kind: String,
    /// What would happen to it.
    pub(crate) action: Action,
}

/// What a plan would do to a resource instance (`planned_change`'s `action`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Made.
    Create,
    /// Changed in place.
    Update,
    /// Removed.
    Delete,
    /// Removed and made again.
    Replace,
    /// Anything else Terraform says (`read`, `move`, `import`, `noop`, or a later word).
    Other(String),
}

impl Action {
    fn of(word: &str) -> Action {
        match word {
            "create" => Action::Create,
            "update" => Action::Update,
            "delete" => Action::Delete,
            "replace" => Action::Replace,
            other => Action::Other(other.to_owned()),
        }
    }
}

/// The planned changes among `lines`, a `plan -json`'s.
fn planned(lines: &[String]) -> Vec<Planned> {
    lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .filter(|e| e.kind == "planned_change")
        .filter_map(|e| e.change)
        .map(|c| Planned {
            address: c.resource.addr,
            kind: c.resource.resource_type,
            action: Action::of(&c.action),
        })
        .collect()
}

/// `plan -json` on the state at `state` (no plan file is written: a saved plan holds the
/// variables' values), and what it would change.
pub(crate) async fn plan(call: &Call<'_>, state: StateAt<'_>) -> Result<Vec<Planned>, CliError<Failure>> {
    let mut args = argv(["plan", "-input=false", "-json"]);
    args.extend(state.option());
    let out = run_events(call, args).await?;
    Ok(planned(&out.stdout))
}

/// `state rm` of each of `addresses` from the state at `state`.
pub(crate) async fn state_rm(call: &Call<'_>, state: StateAt<'_>, addresses: &[String]) -> Result<(), CliError<Failure>> {
    let mut args = argv(["state", "rm"]);
    args.extend(state.option());
    args.extend(addresses.iter().cloned());
    call.ok(TOOL, args, Failure::of).await.map(drop)
}

/// `import` of the object of `id` as `address` into the state at `state`
/// ([`Failure::NoSuchObject`] when there is no such object).
pub(crate) async fn import(call: &Call<'_>, state: StateAt<'_>, address: &str, id: &str) -> Result<(), CliError<Failure>> {
    let mut args = argv(["import", "-input=false", "-no-color"]);
    args.extend(state.option());
    args.extend([address.to_owned(), id.to_owned()]);
    call.ok(TOOL, args, Failure::of).await.map(drop)
}

/// `force-unlock -force <id>`.
pub(crate) async fn force_unlock(call: &Call<'_>, id: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["force-unlock", "-force", id]), Failure::of).await.map(drop)
}

/// The state at `state` as `show -json` gives it. A run's own state file not written yet is an
/// empty state (`show` refuses a path that doesn't exist).
pub(crate) async fn show(call: &Call<'_>, state: StateAt<'_>) -> Result<Shown, CliError<Failure>> {
    let mut args = argv(["show", "-json", "-no-color"]);
    if let StateAt::File(path) = state {
        if !path.exists() {
            return Ok(Shown::default());
        }
        args.push(path.display().to_string());
    }
    let out = call.ok(TOOL, args, Failure::of).await?;
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

/// `output -json`'s outputs of the state at `state`, by name.
pub(crate) async fn outputs(call: &Call<'_>, state: StateAt<'_>) -> Result<BTreeMap<String, Output>, CliError<Failure>> {
    let mut args = argv(["output", "-json", "-no-color"]);
    args.extend(state.option());
    let out = call.ok(TOOL, args, Failure::of).await?;
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

/// `terraform version -json`: `TooOld` before [`MIN_VERSION`].
pub(crate) async fn version(call: &Call<'_>) -> Result<crate::project::platform::Version, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["version", "-json"]), Failure::of).await?;
    let doc: VersionDoc = serde_json::from_str(&out.stdout.join("\n")).map_err(|e| CliError::Unexpected {
        tool: TOOL,
        what: format!("`version -json` isn't its JSON: {e}"),
    })?;
    super::version(TOOL, &doc.terraform_version, Some(MIN_VERSION))
}

#[cfg(test)]
mod tests {
    use super::state::VmFacts;
    use super::*;
    use crate::process::Exit;
    use crate::process::fake::{FakeProcesses, Reply};

    /// What a call that printed `stdout` and exited 1 captured.
    fn failed(stdout: &str) -> Captured {
        Captured::new(TOOL, Exit::new(Some(1)), stdout.lines().map(str::to_owned).collect(), Vec::new())
    }

    #[tokio::test]
    async fn the_version_is_read_from_its_json_and_one_too_old_refused() {
        let fake = FakeProcesses::script()
            .expect(
                "terraform version -json",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.9-version.json")),
            )
            .build();
        let read = version(&crate::cli::testing::call(&fake)).await.unwrap();
        assert_eq!(read.to_string(), "1.9.8");
        let old = FakeProcesses::script()
            .expect("terraform version -json", Reply::ok(r#"{"terraform_version":"0.14.11"}"#))
            .build();
        let refused = version(&crate::cli::testing::call(&old)).await.unwrap_err();
        assert!(matches!(refused, CliError::TooOld { .. }), "{refused}");
    }

    #[test]
    fn a_held_lock_is_read_with_its_id_in_text_and_in_events() {
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
        let events = failed(include_str!("../tool_output/fixtures/terraform/1.16-apply-locked.jsonl"));
        assert_eq!(
            Failure::of_events(&events),
            Failure::StateLocked {
                id: Some("ee74a1fa-ed2e-e1a4-161e-513a943007dd".into())
            }
        );
        let other = Captured::new(TOOL, Exit::new(Some(1)), Vec::new(), vec!["Error: Invalid provider configuration".into()]);
        assert!(matches!(Failure::of(&other), Failure::Other(_)));
        assert!(matches!(Failure::of_events(&other), Failure::Other(_)), "no event: read as text");
    }

    #[test]
    fn a_destroy_that_finds_its_object_gone_says_so_and_a_provisioners_failure_doesnt() {
        let gone = Failure::of_events(&failed(include_str!("../tool_output/fixtures/terraform/1.16-destroy-gone.jsonl")));
        let Failure::Errors(errors) = &gone else { panic!("{gone:?}") };
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].address.as_deref(), Some("terraform_data.ipset"));
        assert!(errors[0].says_missing());
        assert_eq!(
            gone.to_string(),
            "deleting IPSet: no such IPSet 'iso1n0' (terraform_data.ipset): received an HTTP 500 response - Reason: no such IPSet 'iso1n0'"
        );
        // The recorded one: a destroy-time provisioner printing the same words.
        let provisioner = Failure::of_events(&failed(include_str!("../tool_output/fixtures/terraform/1.16-destroy-provisioner-failed.jsonl")));
        let Failure::Errors(said) = &provisioner else { panic!("{provisioner:?}") };
        assert_eq!(said[0].address.as_deref(), Some("terraform_data.ipset"));
        assert!(!said[0].says_missing(), "a provisioner's words are its command's");
        let unaddressed = Diagnostic {
            summary: "Provider not found".into(),
            detail: String::new(),
            address: None,
        };
        assert!(!unaddressed.says_missing(), "about no resource");
    }

    #[test]
    fn an_import_of_nothing_is_told_apart() {
        let out = Captured::new(
            TOOL,
            Exit::new(Some(1)),
            Vec::new(),
            vec!["Error: Cannot import non-existent remote object".into()],
        );
        assert_eq!(Failure::of(&out), Failure::NoSuchObject);
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
                    id: Some("sg-0a1b2c3d".into()),
                    tainted: false,
                    vm: VmFacts::default(),
                },
                Resource {
                    address: "module.web.aws_instance.vm".into(),
                    kind: "aws_instance".into(),
                    id: Some("i-0123456789abcdef0".into()),
                    tainted: false,
                    vm: VmFacts::default(),
                },
            ]
        );
        assert_eq!(shown("{\"format_version\":\"1.0\"}").unwrap(), Shown::default(), "an empty state");
        assert!(
            shown("{\"values\":{\"root_module\":{\"resources\":[{\"address\":\"x\"").is_err(),
            "a cut answer"
        );
    }

    #[test]
    fn show_of_1_16_reads_every_instance_of_every_module_and_taint() {
        let read = shown(include_str!("../tool_output/fixtures/terraform/1.16-show.json")).unwrap();
        assert_eq!(read.data, ["data.terraform_remote_state.none"]);
        let addresses: Vec<&str> = read.managed.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(
            addresses,
            [
                "terraform_data.ipset",
                "terraform_data.pool[0]",
                "terraform_data.pool[1]",
                "terraform_data.web",
                "module.lab.terraform_data.vm[\"a\"]",
                "module.lab.terraform_data.vm[\"b\"]",
            ]
        );
        assert!(read.managed.iter().all(|r| r.id.is_some() && !r.tainted));
        let tainted = shown(include_str!("../tool_output/fixtures/terraform/1.16-show-tainted.json")).unwrap();
        assert!(tainted.managed[0].tainted);
    }

    /// A Proxmox VM as each provider's state writes it: bpg's `node_name`, `vm_id` and
    /// `ipv4_addresses` by interface, Telmate's `target_node`, `vmid` and `default_ipv4_address`.
    #[test]
    fn a_proxmox_vm_says_its_node_id_and_address() {
        let json = r#"{"values":{"root_module":{"resources":[
            {"address":"proxmox_virtual_environment_vm.web","mode":"managed","type":"proxmox_virtual_environment_vm","values":{"id":"101","node_name":"pve","vm_id":101,"ipv4_addresses":[["127.0.0.1"],["10.0.0.5"]]}},
            {"address":"proxmox_vm_qemu.db","mode":"managed","type":"proxmox_vm_qemu","values":{"id":"pve2/qemu/102","target_node":"pve2","vmid":"102","default_ipv4_address":"10.0.0.6"}},
            {"address":"terraform_data.x","mode":"managed","type":"terraform_data","values":{"id":"x","node_name":""}}
        ]}}}"#;
        let read = shown(json).unwrap();
        let facts: Vec<(Option<&str>, Option<&str>, Option<&str>)> = read
            .managed
            .iter()
            .map(|r| (r.vm.node.as_deref(), r.vm.vm_id.as_deref(), r.vm.address.as_deref()))
            .collect();
        assert_eq!(
            facts,
            [
                (Some("pve"), Some("101"), Some("10.0.0.5")),
                (Some("pve2"), Some("102"), Some("10.0.0.6")),
                (None, None, None)
            ]
        );
    }

    /// Each provider's own shape: ESXi's and AWS's attributes, libvirt's and Google's interfaces;
    /// an attribute of another shape under one of those names is skipped, never a state unread.
    #[test]
    fn a_vms_address_is_read_where_its_provider_writes_it() {
        let address = |values: &str| {
            let json = format!(r#"{{"values":{{"root_module":{{"resources":[{{"address":"x.y","mode":"managed","type":"x","values":{values}}}]}}}}}}"#);
            shown(&json).unwrap().managed.remove(0).vm.address
        };
        assert_eq!(address(r#"{"ip_address":"172.16.1.53"}"#).as_deref(), Some("172.16.1.53"));
        assert_eq!(address(r#"{"public_ip":"","private_ip":"10.1.0.4"}"#).as_deref(), Some("10.1.0.4"));
        assert_eq!(
            address(r#"{"network_interface":[{"addresses":["fe80::1","192.168.121.10"]}]}"#).as_deref(),
            Some("192.168.121.10")
        );
        assert_eq!(
            address(r#"{"network_interface":[{"network_ip":"10.128.0.2","access_config":[{"nat_ip":"34.1.2.3"}]}]}"#).as_deref(),
            Some("34.1.2.3")
        );
        assert_eq!(address(r#"{"ipv4_addresses":["10.2.0.9"]}"#).as_deref(), Some("10.2.0.9"));
        assert_eq!(address(r#"{"network_interface":[{"device_index":0,"addresses":7}],"ip_address":"none"}"#), None);
    }

    #[tokio::test]
    async fn show_of_a_state_file_not_written_yet_runs_nothing() {
        let fake = FakeProcesses::script().build();
        let dir = tempfile::tempdir().unwrap();
        let none = dir.path().join("terraform.tfstate");
        assert_eq!(show(&crate::cli::testing::call(&fake), StateAt::File(&none)).await.unwrap(), Shown::default());
    }

    #[tokio::test]
    async fn outputs_keep_their_sensitivity() {
        let fake = FakeProcesses::script()
            .expect(
                "terraform output -json -no-color -state=/runs/lab/terraform.tfstate",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.16-output.json")),
            )
            .build();
        let state = Path::new("/runs/lab/terraform.tfstate");
        let read = outputs(&crate::cli::testing::call(&fake), StateAt::File(state)).await.unwrap();
        assert_eq!(read["ip"].value, serde_json::json!("10.0.0.5"));
        assert_eq!(read["machines"].value, serde_json::json!({ "web": "10.0.0.10" }));
        assert!(read["token"].sensitive && !read["ip"].sensitive);
    }

    #[tokio::test]
    async fn apply_destroy_and_a_targeted_destroy_run_as_events_on_the_runs_state() {
        let fake = FakeProcesses::script()
            .expect(
                "terraform apply -auto-approve -input=false -json -state=/s/terraform.tfstate",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.16-apply.jsonl")),
            )
            .expect(
                "terraform destroy -auto-approve -input=false -json -state=/s/terraform.tfstate -target=terraform_data.web",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.16-destroy-target.jsonl")),
            )
            .expect("terraform destroy -auto-approve -input=false -json", Reply::ok(""))
            .build();
        let lines = std::sync::Mutex::new(Vec::new());
        let keep = |_: Stream, l: &str| lines.lock().unwrap().push(l.to_owned());
        let call = Call {
            sink: &keep,
            ..crate::cli::testing::call(&fake)
        };
        let state = StateAt::File(Path::new("/s/terraform.tfstate"));
        apply(&call, state, Change::Apply).await.unwrap();
        apply(&call, state, Change::DestroyOne("terraform_data.web")).await.unwrap();
        apply(&call, StateAt::Backend, Change::Destroy).await.unwrap();
        let lines = lines.into_inner().unwrap();
        assert!(
            lines.iter().any(|l| l == "Apply complete! Resources: 6 added, 0 changed, 0 destroyed."),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l == "terraform_data.web: Destruction complete after 0s"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with('{')), "shown as messages, never as JSON: {lines:?}");
    }

    #[tokio::test]
    async fn a_plan_says_what_it_would_change() {
        let fake = FakeProcesses::script()
            .expect(
                "terraform plan -input=false -json -state=/s/terraform.tfstate",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.16-plan.jsonl")),
            )
            .build();
        let read = plan(&crate::cli::testing::call(&fake), StateAt::File(Path::new("/s/terraform.tfstate")))
            .await
            .unwrap();
        assert_eq!(
            read,
            [
                Planned {
                    address: "terraform_data.db".into(),
                    kind: "terraform_data".into(),
                    action: Action::Create
                },
                Planned {
                    address: "terraform_data.web".into(),
                    kind: "terraform_data".into(),
                    action: Action::Update
                },
            ]
        );
    }

    #[tokio::test]
    async fn state_rm_import_and_init_name_their_state_and_never_a_variable() {
        let fake = FakeProcesses::script()
            .expect("terraform init -input=false -no-color", Reply::ok(""))
            .expect("terraform state rm -state=/s/t a.b c.d[\"e\"]", Reply::ok(""))
            .expect(
                "terraform import -input=false -no-color -state=/s/t a.b id-1",
                Reply::ok(include_str!("../tool_output/fixtures/terraform/1.16-import.txt")),
            )
            .build();
        let call = crate::cli::testing::call(&fake);
        let state = StateAt::File(Path::new("/s/t"));
        init(&call).await.unwrap();
        state_rm(&call, state, &["a.b".into(), "c.d[\"e\"]".into()]).await.unwrap();
        import(&call, state, "a.b", "id-1").await.unwrap();
        let calls = fake.calls();
        assert!(
            !calls.is_empty() && calls.iter().all(|c| !c.args.iter().any(|a| a.starts_with("-var"))),
            "{:?}",
            fake.lines()
        );
    }

    #[test]
    fn the_backend_is_read_from_inits_file() {
        assert_eq!(
            backend_of(r#"{"version":3,"backend":{"type":"local","config":{"path":null}}}"#).unwrap(),
            Backend::Local
        );
        assert_eq!(
            backend_of(r#"{"version":3,"backend":{"type":"s3","config":{}}}"#).unwrap(),
            Backend::Other("s3".into())
        );
        assert_eq!(backend_of(r#"{"version":3}"#).unwrap(), Backend::Local);
        assert!(backend_of("{").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(backend(dir.path()).unwrap(), Backend::Local, "no backend block: no file");
    }
}
