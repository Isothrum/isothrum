//! Filling the manifest's `${…}` placeholders with one operation's values. A pure function of
//! the values bound so far: a placeholder with none is an error, never an empty string.

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{Part, Placeholder, RunVar, Template};

use crate::Result;
use crate::error::Error;

/// A published port: the machine, and the port's name on it.
pub(crate) type PortKey = (String, String);

/// The values an operation's placeholders take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Vars {
    run: BTreeMap<RunVar, String>,
    params: BTreeMap<String, String>,
    outputs: BTreeMap<String, String>,
    published: BTreeMap<PortKey, u16>,
    hosts: BTreeMap<String, String>,
    machine: Option<String>,
    /// The host ports a local VM run forwards, by `"<machine>/<port>"`: a free one the runner
    /// pinned, bound from the record so the Vagrant driver and its forwarded-port check agree.
    forwards: BTreeMap<String, u16>,
}

impl Vars {
    /// Binds `${run.<var>}`.
    pub(crate) fn set_run(&mut self, var: RunVar, value: impl Into<String>) {
        self.run.insert(var, value.into());
    }

    /// Binds every `${params.<name>}`.
    pub(crate) fn set_params(&mut self, params: BTreeMap<String, String>) {
        self.params = params;
    }

    /// The params bound.
    pub(crate) fn params(&self) -> &BTreeMap<String, String> {
        &self.params
    }

    /// Binds the host ports a local VM run forwards, by `"<machine>/<port>"`.
    pub(crate) fn set_forwards(&mut self, forwards: BTreeMap<String, u16>) {
        self.forwards = forwards;
    }

    /// The host ports a local VM run forwards, by `"<machine>/<port>"`.
    pub(crate) fn forwards(&self) -> &BTreeMap<String, u16> {
        &self.forwards
    }

    /// Binds `${published.…}` with what the engine reports.
    pub(crate) fn set_published(&mut self, published: BTreeMap<PortKey, u16>) {
        self.published = published;
    }

    /// Binds `${host.<machine>}`: where each machine's published ports answer, as the driver
    /// reports it.
    pub(crate) fn set_hosts(&mut self, hosts: BTreeMap<String, String>) {
        self.hosts = hosts;
    }

    /// The published ports bound.
    pub(crate) fn published(&self) -> &BTreeMap<PortKey, u16> {
        &self.published
    }

    /// Binds `${machine}`: the machine a `lifecycle.provision.machine` step sets up again.
    pub(crate) fn set_machine(&mut self, machine: &str) {
        self.machine = Some(machine.to_owned());
    }

    /// Binds `${outputs.<name>}`.
    pub(crate) fn set_output(&mut self, name: &str, value: String) {
        self.outputs.insert(name.to_owned(), value);
    }

    /// Whether `placeholder` is an output or a machine's host that has no value yet: what the
    /// driver's state gives once something made it (a server VM's address after `apply`).
    pub(crate) fn awaits_state(&self, placeholder: &Placeholder) -> bool {
        match placeholder {
            Placeholder::Output(name) => !self.outputs.contains_key(name.as_str()),
            Placeholder::Host(machine) => !self.hosts.contains_key(machine.as_str()),
            _ => false,
        }
    }

    /// Action argument `name` of `with` (a step's, at `at`), filled; none when it isn't given.
    pub(crate) fn with_arg(&self, with: &std::collections::BTreeMap<isoloom_manifest::SnakeName, Template>, at: &str, name: &str) -> Result<Option<String>> {
        with.iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, t)| self.render(t, &format!("{at}.with.{name}")))
            .transpose()
    }

    /// [`Vars::with_arg`], empty when it isn't given.
    pub(crate) fn with_text(&self, with: &std::collections::BTreeMap<isoloom_manifest::SnakeName, Template>, at: &str, name: &str) -> Result<String> {
        Ok(self.with_arg(with, at, name)?.unwrap_or_default())
    }

    /// `template` filled; `at` names where it is, for the error.
    pub(crate) fn render(&self, template: &Template, at: &str) -> Result<String> {
        self.fill(template, None, at)
    }

    /// `template` filled, `${value}` being `value` (an output's `format`).
    pub(crate) fn render_value(&self, template: &Template, value: &str, at: &str) -> Result<String> {
        self.fill(template, Some(value), at)
    }

    /// `template` filled where it can be; a placeholder with no value, or naming one of `hidden`'s
    /// outputs, stays as written (a run's message never shows a sensitive output).
    pub(crate) fn render_lenient(&self, template: &Template, hidden: &BTreeSet<String>) -> String {
        template
            .parts()
            .iter()
            .map(|part| match part {
                Part::Text(t) => t.clone(),
                Part::Placeholder(Placeholder::Output(name)) if hidden.contains(name.as_str()) => {
                    format!("${{{}}}", Placeholder::Output(name.clone()))
                }
                Part::Placeholder(p) => self.value(p, None).unwrap_or_else(|| format!("${{{p}}}")),
            })
            .collect()
    }

    fn fill(&self, template: &Template, value: Option<&str>, at: &str) -> Result<String> {
        let mut out = String::new();
        for part in template.parts() {
            match part {
                Part::Text(t) => out.push_str(t),
                Part::Placeholder(p) => {
                    let v = self.value(p, value).ok_or_else(|| Error::Unbound {
                        at: at.to_owned(),
                        placeholder: p.to_string(),
                    })?;
                    out.push_str(&v);
                }
            }
        }
        Ok(out)
    }

    fn value(&self, placeholder: &Placeholder, value: Option<&str>) -> Option<String> {
        match placeholder {
            Placeholder::Run(var) => self.run.get(var).cloned(),
            Placeholder::Param(name) => self.params.get(name.as_str()).cloned(),
            Placeholder::Output(name) => self.outputs.get(name.as_str()).cloned(),
            Placeholder::Published { machine, port } => self.published.get(&(machine.to_string(), port.to_string())).map(u16::to_string),
            Placeholder::Host(machine) => self.hosts.get(machine.as_str()).cloned(),
            Placeholder::Value => value.map(str::to_owned),
            Placeholder::Machine => self.machine.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Template {
        Template::parse(text).unwrap()
    }

    fn vars() -> Vars {
        let mut v = Vars::default();
        v.set_run(RunVar::Name, "web-portal");
        v.set_run(RunVar::StateDir, "/home/u/.isothrum/runs/web-portal");
        v.set_params(BTreeMap::from([("ISOLOOM_DERIVED".to_owned(), "1".to_owned())]));
        v.set_published(BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        v.set_hosts(BTreeMap::from([("web".to_owned(), "127.0.0.1".to_owned())]));
        v.set_output("secret_token", "s3cr3t".into());
        v
    }

    #[test]
    fn placeholders_take_the_operations_values() {
        let v = vars();
        assert_eq!(
            v.render(&t("${run.state_dir}/ports.yml"), "x").unwrap(),
            "/home/u/.isothrum/runs/web-portal/ports.yml"
        );
        assert_eq!(
            v.render(&t("${run.name}_lab cost $$5 ${params.ISOLOOM_DERIVED}"), "x").unwrap(),
            "web-portal_lab cost $5 1"
        );
        assert_eq!(
            v.render_value(&t("http://127.0.0.1:${value}/"), "41873", "x").unwrap(),
            "http://127.0.0.1:41873/"
        );
        assert_eq!(v.render(&t("${published.web.portal}"), "x").unwrap(), "41873");
        assert_eq!(
            v.render_value(&t("http://${host.web}:${value}/"), "41873", "x").unwrap(),
            "http://127.0.0.1:41873/"
        );
    }

    #[test]
    fn a_placeholder_without_a_value_is_an_error_naming_it() {
        let err = vars().render(&t("--provider ${run.provider}"), "lifecycle.up.up").unwrap_err();
        assert_eq!(err.to_string(), "lifecycle.up.up: `${run.provider}` has no value in this operation");
        assert!(vars().render(&t("${published.web.admin}"), "x").is_err());
        assert!(vars().render(&t("${value}"), "x").is_err());
        let unbound_host = vars().render(&t("http://${host.db}/"), "outputs.db_site.format").unwrap_err();
        assert_eq!(unbound_host.to_string(), "outputs.db_site.format: `${host.db}` has no value in this operation");
    }

    #[test]
    fn a_message_never_shows_a_sensitive_output_or_an_unknown_value() {
        let hidden = BTreeSet::from(["secret_token".to_owned()]);
        let msg = t("Open http://localhost:${published.web.portal}/ with ${outputs.secret_token}; ${published.db.mysql}");
        assert_eq!(
            vars().render_lenient(&msg, &hidden),
            "Open http://localhost:41873/ with ${outputs.secret_token}; ${published.db.mysql}"
        );
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn text_with_no_placeholder_renders_as_itself(text in "\\PC{0,80}") {
            let template = Template::parse(&text.replace('$', "$$")).unwrap();
            prop_assert_eq!(Vars::default().render(&template, "x").unwrap(), text);
        }

        #[test]
        fn a_bound_placeholder_is_replaced_wherever_it_sits(before in "[^$]{0,20}", after in "[^$]{0,20}", name in "[a-z0-9][a-z0-9_-]{0,30}") {
            let mut vars = Vars::default();
            vars.set_run(RunVar::Name, name.clone());
            let template = Template::parse(&format!("{before}${{run.name}}{after}")).unwrap();
            prop_assert_eq!(vars.render(&template, "x").unwrap(), format!("{before}{name}{after}"));
        }

        #[test]
        fn a_value_is_never_read_as_a_placeholder_itself(value in "\\PC{0,40}") {
            // A param holding `${run.name}` or a lone `$` is put in as it is, never filled again.
            let mut vars = Vars::default();
            vars.set_params(BTreeMap::from([("P".to_owned(), value.clone())]));
            let template = Template::parse("<${params.P}>").unwrap();
            prop_assert_eq!(vars.render(&template, "x").unwrap(), format!("<{value}>"));
        }

        #[test]
        fn a_message_is_always_filled_or_kept_as_written(text in "[a-z ]{0,20}", port in 1u16..) {
            let template = Template::parse(&format!("{text}${{published.web.portal}}")).unwrap();
            let mut vars = Vars::default();
            prop_assert_eq!(vars.render_lenient(&template, &BTreeSet::new()), format!("{text}${{published.web.portal}}"));
            vars.set_published(BTreeMap::from([(("web".to_owned(), "portal".to_owned()), port)]));
            prop_assert_eq!(vars.render_lenient(&template, &BTreeSet::new()), format!("{text}{port}"));
        }
    }
}
