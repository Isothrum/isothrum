//! A Terraform module's input variables, read from its own files: each `variable` block of its
//! `*.tf` files (HCL) and each `variable` object of its `*.tf.json` files, as Terraform reads
//! them. Nothing runs: a caller lists what a folder asks for before it gives the values
//! ([`crate::RunOptions::with_env`] for a plain one, a secret for a sensitive one, which the run's
//! record then never keeps).

use std::path::Path;

use hcl::eval::{Context, Evaluate};
use serde::{Deserialize, Serialize};

use super::{Format, Project};
use crate::Result;
use crate::error::Error;

/// One input variable of a Terraform module, as its `variable` block declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct InputVariable {
    /// Its name (the block's label).
    pub name: String,
    /// Its type constraint as written (`string`, `list(string)`, `object({ name = string })`),
    /// when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub type_constraint: Option<String>,
    /// Its default value, as JSON; none when the module needs it given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Whether Terraform keeps it out of its output (`sensitive = true`): give it as a secret.
    #[serde(default)]
    pub sensitive: bool,
    /// Its description, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl InputVariable {
    /// Whether the module needs it given: it has no default.
    #[must_use]
    pub fn required(&self) -> bool {
        self.default.is_none()
    }
}

impl Project {
    /// The input variables of this Terraform module, by name, read from its files.
    ///
    /// # Errors
    ///
    /// `unsupported` for a project of another format; `project_invalid` for a file that isn't
    /// HCL or JSON as Terraform reads it, or a `variable` that isn't one; `io` when a file can't
    /// be read.
    pub fn variables(&self) -> Result<Vec<InputVariable>> {
        if self.format != Format::Terraform {
            return Err(Error::Unsupported {
                what: format!("input variables of a {} project (only a Terraform module declares them)", self.format),
            });
        }
        let mut found = Vec::new();
        for file in &self.files {
            let text = std::fs::read_to_string(file).map_err(Error::io(file, "read"))?;
            let refused = |reason: String| Error::Project {
                dir: self.dir.clone(),
                reason: format!("{}: {reason}", file.display()),
            };
            if file.extension().is_some_and(|e| e == "json") {
                found.extend(from_json(&text).map_err(refused)?);
            } else if file.extension().is_some_and(|e| e == "tf") {
                found.extend(from_hcl(&text).map_err(refused)?);
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }
}

/// The `variable` blocks of a `*.tf` file.
fn from_hcl(text: &str) -> std::result::Result<Vec<InputVariable>, String> {
    let body = hcl::parse(text).map_err(|e| format!("not HCL Terraform reads: {e}"))?;
    body.blocks()
        .filter(|b| b.identifier() == "variable")
        .map(|block| {
            let [label] = block.labels() else {
                return Err("a `variable` block names one variable".to_owned());
            };
            let name = label.as_str().to_owned();
            let mut variable = InputVariable {
                name: name.clone(),
                type_constraint: None,
                default: None,
                sensitive: false,
                description: None,
            };
            for attribute in block.body().attributes() {
                let value = || constant(&name, attribute.key(), attribute.expr());
                match attribute.key() {
                    "type" => {
                        let written = hcl::format::to_string(attribute.expr()).map_err(|e| format!("variable `{name}`: its type: {e}"))?;
                        variable.type_constraint = Some(written);
                    }
                    "default" => variable.default = Some(value()?),
                    "sensitive" => variable.sensitive = value()? == serde_json::Value::Bool(true),
                    "description" => variable.description = value()?.as_str().map(str::to_owned),
                    _ => {}
                }
            }
            Ok(variable)
        })
        .collect()
}

/// The value of `variable`'s attribute `key`, a constant (Terraform allows no reference there).
fn constant(variable: &str, key: &str, expr: &hcl::Expression) -> std::result::Result<serde_json::Value, String> {
    let value = expr
        .evaluate(&Context::new())
        .map_err(|e| format!("variable `{variable}`: its `{key}` isn't a constant: {e}"))?;
    serde_json::to_value(value).map_err(|e| format!("variable `{variable}`: its `{key}`: {e}"))
}

/// One `variable` of a `*.tf.json` file, as Terraform's JSON syntax writes it.
#[derive(Deserialize)]
struct JsonVariable {
    #[serde(default, rename = "type")]
    type_constraint: Option<String>,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    sensitive: bool,
    #[serde(default)]
    description: Option<String>,
}

/// The `variable` objects of a `*.tf.json` file.
fn from_json(text: &str) -> std::result::Result<Vec<InputVariable>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        variable: std::collections::BTreeMap<String, JsonVariable>,
    }
    let file: File = serde_json::from_str(text).map_err(|e| format!("not JSON Terraform reads: {e}"))?;
    Ok(file
        .variable
        .into_iter()
        .map(|(name, v)| InputVariable {
            name,
            type_constraint: v.type_constraint,
            default: v.default,
            sensitive: v.sensitive,
            description: v.description,
        })
        .collect())
}

/// The input variables of the Terraform module in `dir`.
///
/// # Errors
///
/// As [`Project::open`] and [`Project::variables`] refuse.
pub fn variables_of(dir: &Path) -> Result<Vec<InputVariable>> {
    Project::open(dir, Some(Format::Terraform))?.variables()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(files: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        let project = Project::open(dir.path(), Some(Format::Terraform)).unwrap();
        (dir, project)
    }

    #[test]
    fn each_variable_block_is_read_with_its_type_default_and_sensitivity() {
        let (_dir, project) = module(&[(
            "main.tf",
            r#"
variable "region" {
  type    = string
  default = "us-east-1"
}
variable "token" {
  type      = string
  sensitive = true
  description = "The API token."
}
variable "sizes" {
  type    = map(number)
  default = { small = 1, large = 4 }
}
resource "null_resource" "x" {}
"#,
        )]);
        let vars = project.variables().unwrap();
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["region", "sizes", "token"]);
        assert_eq!(vars[0].type_constraint.as_deref(), Some("string"));
        assert_eq!(vars[0].default, Some(serde_json::json!("us-east-1")));
        assert!(!vars[0].sensitive && !vars[0].required());
        assert_eq!(vars[1].type_constraint.as_deref(), Some("map(number)"));
        assert_eq!(vars[1].default, Some(serde_json::json!({"small": 1, "large": 4})));
        assert!(vars[2].sensitive && vars[2].required());
        assert_eq!(vars[2].description.as_deref(), Some("The API token."));
    }

    #[test]
    fn a_json_file_declares_variables_too() {
        let (_dir, project) = module(&[(
            "vars.tf.json",
            r#"{"variable": {"zone": {"type": "string", "default": "a"}, "key": {"sensitive": true}}}"#,
        )]);
        let vars = project.variables().unwrap();
        assert_eq!(
            vars.iter().map(|v| (v.name.as_str(), v.sensitive, v.required())).collect::<Vec<_>>(),
            [("key", true, true), ("zone", false, false)]
        );
    }

    #[test]
    fn a_module_without_variables_has_none_and_a_broken_file_is_project_invalid() {
        let (_empty, none) = module(&[("main.tf", "output \"x\" { value = 1 }\n")]);
        assert_eq!(none.variables().unwrap(), []);
        let (_broken, broken) = module(&[("main.tf", "variable \"x\" {\n")]);
        assert_eq!(broken.variables().unwrap_err().code(), crate::ErrorCode::ProjectInvalid);
    }

    #[test]
    fn another_format_has_no_variables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("compose.yaml"), "services: {}\n").unwrap();
        let project = Project::open(dir.path(), None).unwrap();
        assert_eq!(project.variables().unwrap_err().code(), crate::ErrorCode::Unsupported);
    }
}
