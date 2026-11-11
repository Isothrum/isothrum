//! The values an operation runs with, given on the command line and parsed once: params
//! (`NAME=VALUE`) and the slots' values, read from the environment (`NAME[=VAR]`).

use std::collections::BTreeMap;
use std::ffi::OsString;

use isothrum_runner::{SecretValue, Secrets};

use crate::error::{Error, Result};

/// Reads the environment: variable `name`'s value, `None` when it isn't set.
pub(crate) type Var<'a> = dyn Fn(&str) -> Option<OsString> + 'a;

/// `NAME=VALUE` pairs, by name; a name given twice is refused, not the last one kept.
pub(crate) fn params(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for p in pairs {
        let (k, v) = p.split_once('=').ok_or_else(|| Error::Usage(format!("`--param {p}`: write NAME=VALUE")))?;
        if out.insert(k.to_owned(), v.to_owned()).is_some() {
            return Err(Error::Usage(format!("`--param {p}`: `{k}` is given twice")));
        }
    }
    Ok(out)
}

/// The slots' values, each `NAME[=VAR]` read from variable `VAR` (default `NAME`) through `var`;
/// a slot named twice is refused, not the last one kept.
pub(crate) fn secrets(specs: &[String], var: &Var<'_>) -> Result<Secrets> {
    let mut out = Secrets::new();
    for spec in specs {
        let (name, from) = spec.split_once('=').unwrap_or((spec, spec));
        let refused = |why: &str| Error::Usage(format!("`--secret-from-env {spec}`: `{from}` {why}"));
        if out.get(name).is_some() {
            return Err(Error::Usage(format!("`--secret-from-env {spec}`: `{name}` is given twice")));
        }
        let value = var(from).ok_or_else(|| refused("isn't set"))?;
        let value = value.into_string().map_err(|_| refused("isn't UTF-8"))?;
        out.insert(name, SecretValue::new(value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_parsed_once() {
        assert_eq!(
            params(&["A=1".into(), "B=x=y".into()]).unwrap(),
            BTreeMap::from([("A".into(), "1".into()), ("B".into(), "x=y".into())])
        );
        assert!(params(&["A".into()]).is_err());
        let env = |k: &str| (k == "SECRET_VAR").then(|| OsString::from("token-x"));
        let s = secrets(&["DEMO_SECRET=SECRET_VAR".into()], &env).unwrap();
        assert_eq!(s.get("DEMO_SECRET").map(SecretValue::expose), Some("token-x"));
        assert!(
            secrets(&["DEMO_SECRET".into()], &env)
                .unwrap_err()
                .to_string()
                .contains("`DEMO_SECRET` isn't set")
        );
    }

    #[test]
    fn a_slot_named_alone_is_read_from_its_own_variable() {
        let env = |k: &str| (k == "SECRET_VAR").then(|| OsString::from("token-y"));
        let s = secrets(&["SECRET_VAR".into()], &env).unwrap();
        assert_eq!(s.get("SECRET_VAR").map(SecretValue::expose), Some("token-y"));
    }

    /// Regression: a variable that isn't UTF-8 was read as unset, and refused as "isn't set".
    #[cfg(unix)]
    #[test]
    fn a_secret_variable_that_isnt_utf8_is_refused_as_such() {
        use std::os::unix::ffi::OsStringExt;
        let env = |k: &str| (k == "SECRET_VAR").then(|| OsString::from_vec(b"bad\xff".to_vec()));
        let refused = secrets(&["DEMO_SECRET=SECRET_VAR".into()], &env).unwrap_err().to_string();
        assert!(refused.contains("`SECRET_VAR` isn't UTF-8"), "{refused}");
    }

    proptest::proptest! {
        #[test]
        fn a_param_is_its_name_up_to_the_first_equals_sign_and_the_rest_its_value(name in "[A-Za-z_][A-Za-z0-9_]{0,12}", value in ".{0,20}") {
            let parsed = params(&[format!("{name}={value}")]).unwrap();
            proptest::prop_assert_eq!(parsed, BTreeMap::from([(name, value)]));
        }
    }

    /// Regression: a repeated `--param A=…` or `--secret-from-env A` silently kept the last value.
    #[test]
    fn a_value_given_twice_is_refused() {
        let twice = params(&["A=1".into(), "A=2".into()]).unwrap_err();
        assert_eq!(twice.to_string(), "`--param A=2`: `A` is given twice");
        let env = |_: &str| Some(OsString::from("v"));
        let again = secrets(&["S".into(), "S=OTHER".into()], &env).unwrap_err();
        assert_eq!(again.to_string(), "`--secret-from-env S=OTHER`: `S` is given twice");
    }
}
