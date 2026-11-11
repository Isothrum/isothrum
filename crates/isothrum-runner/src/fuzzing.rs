//! The pure cores the fuzz targets drive (`fuzz/`): filling a template's placeholders, and planning
//! a manifest's steps. Not an API: only the `fuzzing` feature builds this module, and only `fuzz/`
//! turns that feature on.

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{HostOs, Manifest, Provider, Run, RunVar, Step, Template};

use crate::executor::plan;
use crate::scope::Scope;
use crate::vars::Vars;

/// The values an operation's placeholders take, bound one at a time.
#[derive(Debug, Default)]
pub struct Values {
    vars: Vars,
    params: BTreeMap<String, String>,
    published: BTreeMap<(String, String), u16>,
}

impl Values {
    /// Binds `${run.<var>}`.
    pub fn run(&mut self, var: RunVar, value: &str) {
        self.vars.set_run(var, value);
    }

    /// Binds `${params.<name>}`.
    pub fn param(&mut self, name: &str, value: &str) {
        self.params.insert(name.to_owned(), value.to_owned());
        self.vars.set_params(self.params.clone());
    }

    /// Binds `${outputs.<name>}`.
    pub fn output(&mut self, name: &str, value: &str) {
        self.vars.set_output(name, value.to_owned());
    }

    /// Binds `${published.<machine>.<port>}`.
    pub fn published(&mut self, machine: &str, port: &str, host_port: u16) {
        self.published.insert((machine.to_owned(), port.to_owned()), host_port);
        self.vars.set_published(self.published.clone());
    }

    /// Binds `${machine}`.
    pub fn machine(&mut self, name: &str) {
        self.vars.set_machine(name);
    }

    /// `template` filled, as a step's argument is.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Unbound`] for a placeholder with no value.
    pub fn render(&self, template: &Template) -> crate::Result<String> {
        self.vars.render(template, "fuzz")
    }

    /// `template` filled, `${value}` being `value`, as an output's `format` is.
    ///
    /// # Errors
    ///
    /// As [`Values::render`].
    pub fn render_value(&self, template: &Template, value: &str) -> crate::Result<String> {
        self.vars.render_value(template, value, "fuzz")
    }

    /// `template` filled where it can be, as a run's message is: `hidden` outputs never shown.
    #[must_use]
    pub fn render_lenient(&self, template: &Template, hidden: &BTreeSet<String>) -> String {
        self.vars.render_lenient(template, hidden)
    }
}

/// One step, planned.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Planned {
    /// For another host or provider: it counts as done.
    Elsewhere,
    /// A command, its program and filled arguments.
    Command {
        /// The program.
        program: String,
        /// Its arguments, filled.
        args: Vec<String>,
    },
    /// An action, run by the driver.
    Action,
    /// Refused, with why (an unbound placeholder, no argv for this host).
    Refused(String),
}

/// Every step of `manifest` (its lifecycle's and its checks' lists, in order), each with the list
/// it is in, planned on `host` with the run's `provider` and `values`.
#[must_use]
pub fn plan(manifest: &Manifest, host: HostOs, provider: Option<Provider>, values: &Values) -> Vec<(String, Planned)> {
    let scope = Scope::new(host, provider);
    lists(manifest)
        .into_iter()
        .flat_map(|(list, steps)| steps.into_iter().map(move |step| (list, step)))
        .map(|(list, step)| (plan::at(list, step), planned(&scope, &values.vars, list, step)))
        .collect()
}

fn planned(scope: &Scope, vars: &Vars, list: &str, step: &Step) -> Planned {
    if !plan::applies(scope, step) {
        return Planned::Elsewhere;
    }
    match &step.run {
        Run::Argv(argv) => match plan::command(argv, scope.host, vars, &plan::at(list, step)) {
            Ok(command) => Planned::Command {
                program: command.program,
                args: command.args,
            },
            Err(e) => Planned::Refused(e.to_string()),
        },
        Run::Action { .. } => Planned::Action,
    }
}

/// The manifest's step lists, by the name the executor gives them.
fn lists(manifest: &Manifest) -> Vec<(&'static str, Vec<&Step>)> {
    let lifecycle = &manifest.lifecycle;
    let park = lifecycle.park.as_ref();
    let provision = lifecycle.provision.as_ref();
    let checks = &manifest.checks;
    vec![
        ("lifecycle.up", lifecycle.up.as_slice().iter().collect()),
        ("lifecycle.down", lifecycle.down.as_slice().iter().collect()),
        ("lifecycle.resume", lifecycle.resume.iter().collect()),
        ("lifecycle.status", vec![&lifecycle.status]),
        (
            "lifecycle.park",
            park.into_iter().flat_map(|p| p.pause.iter().chain(p.shutdown.iter()).flatten()).collect(),
        ),
        ("lifecycle.provision.all", provision.into_iter().flat_map(|p| &p.all).collect()),
        ("lifecycle.provision.machine", provision.into_iter().flat_map(|p| &p.machine).collect()),
        ("checks.setup", checks.setup.iter().collect()),
        ("checks.runners", checks.runners.iter().map(|r| &r.step).collect()),
        ("checks.on_timeout", checks.runners.iter().flat_map(|r| &r.on_timeout).collect()),
        ("checks.teardown", checks.teardown.iter().collect()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_of_a_manifest_is_planned_once() {
        let manifest = crate::testing::docker_manifest();
        let mut values = Values::default();
        values.run(RunVar::Name, "web-portal");
        values.run(RunVar::StateDir, "/runs/web-portal");
        let planned = plan(&manifest, HostOs::Linux, None, &values);
        let steps = lists(&manifest).iter().map(|(_, s)| s.len()).sum::<usize>();
        assert_eq!(planned.len(), steps);
        assert!(
            planned
                .iter()
                .any(|(at, p)| at.starts_with("lifecycle.up.") && matches!(p, Planned::Command { program, .. } if program == "docker"))
        );
        assert!(planned.iter().any(|(_, p)| *p == Planned::Action));
        let unbound = plan(&manifest, HostOs::Linux, None, &Values::default());
        assert!(
            unbound.iter().any(|(_, p)| matches!(p, Planned::Refused(why) if why.contains("${run.name}"))),
            "{unbound:?}"
        );
    }
}
