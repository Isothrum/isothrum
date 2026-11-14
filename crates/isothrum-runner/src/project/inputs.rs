//! The values a run is given ([`crate::RunOptions`]'s `env` and `secrets`), handed to its tool
//! the way that tool reads values. Every tool gets them in its process environment: Compose
//! interpolates `${NAME}` from it, and a Vagrantfile reads `ENV.fetch("NAME", ...)`. Terraform also
//! gets each one as the module variable of the same name (`TF_VAR_<NAME>`). Nothing is written to
//! a file, so a secret never lands on disk, and Isothrum checks no names: a project that needs a
//! value it wasn't given says so itself (Compose's `${NAME:?}`, Terraform's variable without a
//! default).

use std::collections::BTreeMap;

use super::Format;
use crate::Result;
use crate::error::Error;
use crate::secrets::Secrets;

/// The prefix Terraform reads a module variable's value from in its environment.
const TF_VAR: &str = "TF_VAR_";

/// The environment the tool of a `format` project gets for `values` and `secrets`, in order: the
/// values, then the secrets (a secret given under a value's name wins). Terraform's are followed
/// by their `TF_VAR_` copies.
///
/// # Errors
///
/// `param_invalid` for a name that can't be an environment variable's (empty, or holding `=` or
/// a NUL), or a value holding a NUL.
pub(crate) fn environment(format: Format, values: &BTreeMap<String, String>, secrets: &Secrets) -> Result<Vec<(String, String)>> {
    let mut env = Vec::new();
    for (name, value) in values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(secrets.iter().map(|(k, v)| (k, v.expose())))
    {
        check(name, value)?;
        env.push((name.to_owned(), value.to_owned()));
    }
    if format == Format::Terraform {
        let vars: Vec<(String, String)> = env.iter().map(|(k, v)| (format!("{TF_VAR}{k}"), v.clone())).collect();
        env.extend(vars);
    }
    Ok(env)
}

/// Refuses a name or value no environment can carry. The value isn't quoted in the error: it may
/// be a secret.
fn check(name: &str, value: &str) -> Result<()> {
    let reason = if name.is_empty() {
        Some("an empty name")
    } else if name.contains(['=', '\0']) {
        Some("a name holding `=` or a NUL")
    } else if value.contains('\0') {
        Some("a value holding a NUL")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(Error::Param {
            name: name.to_owned(),
            reason: format!("{reason} can't be passed to the tool's environment"),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretValue;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn values_then_secrets_reach_every_tool_and_terraform_gets_its_variables_too() {
        let mut secrets = Secrets::new();
        secrets.insert("TOKEN", SecretValue::new("s3cret"));
        let given = values(&[("ISOLOOM_KEY", "k3y9ab")]);
        let compose = environment(Format::Compose, &given, &secrets).expect("environment");
        assert_eq!(compose, vec![("ISOLOOM_KEY".into(), "k3y9ab".into()), ("TOKEN".into(), "s3cret".into())]);
        assert_eq!(environment(Format::Vagrant, &given, &secrets).expect("environment"), compose);
        let terraform = environment(Format::Terraform, &given, &secrets).expect("environment");
        assert_eq!(&terraform[..2], &compose[..]);
        assert_eq!(
            &terraform[2..],
            &[("TF_VAR_ISOLOOM_KEY".into(), "k3y9ab".into()), ("TF_VAR_TOKEN".into(), "s3cret".into())]
        );
    }

    #[test]
    fn a_name_no_environment_carries_is_refused_without_its_value() {
        for (name, value) in [("", "x"), ("A=B", "x"), ("A", "x\0y")] {
            let e = environment(Format::Compose, &values(&[(name, value)]), &Secrets::new()).expect_err("refused");
            assert_eq!(e.code(), crate::ErrorCode::ParamInvalid, "{name:?}");
        }
        let mut secrets = Secrets::new();
        secrets.insert("BAD\0NAME", SecretValue::new("hidden-value"));
        let e = environment(Format::Compose, &BTreeMap::new(), &secrets).expect_err("refused");
        assert!(!e.to_string().contains("hidden-value"), "{e}");
    }
}
