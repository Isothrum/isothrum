//! A module's required variables (no default) that have no value, refused before Terraform runs:
//! `secret_missing` for a sensitive one, `input_invalid` for a plain one, naming it, rather than
//! Terraform's "No value for required variable" once the apply began. Never a module that would
//! have run: a value counts from every place Terraform reads one, and a place that can't be read
//! counts as giving it.
//!
//! The places: the run's values and secrets (`TF_VAR_<name>` in the tool's environment), the
//! runner's own environment (which the tools inherit), and the module folder's
//! `terraform.tfvars`, `terraform.tfvars.json` and `*.auto.tfvars` / `*.auto.tfvars.json`, read
//! for the names they set only. `TF_CLI_ARGS` in the runner's environment may add `-var` and
//! `-var-file`: nothing is refused then.

use std::path::Path;

use crate::Result;
use crate::error::Error;
use crate::formats::ToolCx;
use crate::host_env::TF_VAR_PREFIX;
use crate::project::variables::InputVariable;

/// Refuses the first required variable of `cx`'s module that nothing gives a value.
///
/// # Errors
///
/// `secret_missing` for a sensitive variable, `input_invalid` for a plain one.
pub(super) fn refuse_unset(cx: &ToolCx<'_>) -> Result<()> {
    if cx.host_env.sets_terraform_args() {
        return Ok(());
    }
    // Variables that can't be read are Terraform's to refuse: nothing is refused here.
    let Ok(variables) = cx.project.variables() else { return Ok(()) };
    let given = |var: &str| cx.env.iter().any(|(k, _)| k == var) || cx.host_env.sets_terraform(var);
    unset(&variables, given, &cx.project.dir).map_or(Ok(()), Err)
}

/// The refusal of the first of `variables` that is required (no default) and has no value: not
/// in the environment (`given`, by its `TF_VAR_<name>`), nor set by a variable file of `dir`.
fn unset(variables: &[InputVariable], given: impl Fn(&str) -> bool, dir: &Path) -> Option<Error> {
    let mut from_files: Option<Option<Vec<String>>> = None;
    for v in variables.iter().filter(|v| v.default.is_none()) {
        if given(&format!("{TF_VAR_PREFIX}{}", v.name)) {
            continue;
        }
        // A folder whose files can't be read may set any of them: none is refused.
        match from_files.get_or_insert_with(|| names_set(dir)) {
            Some(set) if !set.contains(&v.name) => {}
            _ => continue,
        }
        return Some(if v.sensitive {
            Error::SecretMissing { name: v.name.clone() }
        } else {
            Error::Input {
                name: v.name.clone(),
                reason: "the module requires it (no default) and nothing gives it a value".to_owned(),
            }
        });
    }
    None
}

/// The variable names the module folder `dir`'s variable files set; `None` when one of them can't
/// be read (it may set any).
fn names_set(dir: &Path) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return None };
    for entry in entries {
        let path = entry.ok()?.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let json = file == "terraform.tfvars.json" || file.ends_with(".auto.tfvars.json");
        let hcl = file == "terraform.tfvars" || file.ends_with(".auto.tfvars");
        if !json && !hcl {
            continue;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        names.extend(if json { json_names(&text)? } else { hcl_names(&text) });
    }
    Some(names)
}

/// The top-level keys of a `.tfvars.json`; `None` when it isn't a JSON object.
fn json_names(text: &str) -> Option<Vec<String>> {
    let doc: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(doc.as_object()?.keys().cloned().collect())
}

/// The names a `.tfvars` file assigns: each line's word before `=`. A word inside a value (a map's
/// key, a heredoc) is taken too: that can only keep a module from being refused.
fn hcl_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim().trim_matches('"').to_owned())
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module declaring a sensitive `token` and a plain `region`, both required, and `size`
    /// with a default; its variable files `files`.
    fn module(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<InputVariable>) {
        let dir = tempfile::tempdir().expect("a module");
        let main = "variable \"token\" {\n  sensitive = true\n}\nvariable \"region\" {}\nvariable \"size\" {\n  default = 1\n}\n";
        std::fs::write(dir.path().join("main.tf"), main).expect("main.tf");
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).expect("a variable file");
        }
        let variables = crate::variables_of(dir.path()).expect("its variables");
        (dir, variables)
    }

    /// What `unset` refuses, its code and the variable it names.
    fn refused(variables: &[InputVariable], given: &[&str], dir: &Path) -> Option<(crate::ErrorCode, String)> {
        unset(variables, |var| given.contains(&var), dir).map(|e| {
            let name = match &e {
                Error::SecretMissing { name } | Error::Input { name, .. } => name.clone(),
                other => format!("{other:?}"),
            };
            (e.code(), name)
        })
    }

    #[test]
    fn a_required_variable_with_no_value_from_any_place_is_refused_by_its_kind() {
        let (dir, vars) = module(&[]);
        assert_eq!(refused(&vars, &[], dir.path()), Some((crate::ErrorCode::InputInvalid, "region".to_owned())));
        let region = ["TF_VAR_region"];
        assert_eq!(refused(&vars, &region, dir.path()), Some((crate::ErrorCode::SecretMissing, "token".to_owned())));
        assert_eq!(refused(&vars, &["TF_VAR_token", "TF_VAR_region"], dir.path()), None, "`size` has its default");
    }

    #[test]
    fn a_value_from_a_variable_file_or_one_that_cant_be_read_is_never_refused() {
        for files in [
            &[("terraform.tfvars", "token = \"x\"\nregion = \"eu\"\n")][..],
            &[("terraform.tfvars.json", r#"{"token": "x"}"#), ("lab.auto.tfvars", "region = \"eu\"\n")][..],
            &[("prod.auto.tfvars.json", r#"{"token": "x", "region": "eu"}"#)][..],
            // A file that doesn't parse may set anything.
            &[("terraform.tfvars.json", "{ not json")][..],
        ] {
            let (dir, vars) = module(files);
            assert_eq!(refused(&vars, &[], dir.path()), None, "{files:?}");
        }
        let (dir, vars) = module(&[("other.tfvars", "token = \"x\"\nregion = \"eu\"\n")]);
        assert_eq!(
            refused(&vars, &[], dir.path()),
            Some((crate::ErrorCode::InputInvalid, "region".to_owned())),
            "a file Terraform doesn't read by itself gives nothing"
        );
    }

    #[test]
    fn a_tfvars_files_names_are_read_and_a_json_one_that_doesnt_parse_reads_as_none() {
        assert_eq!(
            hcl_names("region = \"eu\"\n# token = x\n  \"size\" = 3\ntags = {\n  owner = \"me\"\n}\n"),
            ["region", "size", "tags", "owner"]
        );
        let mut names = json_names(r#"{"token": "x", "region": "eu"}"#).expect("an object");
        names.sort();
        assert_eq!(names, ["region", "token"]);
        assert_eq!((json_names("[1, 2]"), json_names("not json")), (None, None));
    }
}
