//! A run's Docker calls pinned to one engine: the Docker context the run was started with
//! ([`crate::RunOptions::docker_context`], recorded with the run), put before the arguments of
//! every `docker` call of its tools (`docker --context <context> …`). The flag wins over the
//! current context and over `DOCKER_HOST`, so the run keeps reaching the engine it started on
//! however the user's current context changes; a call that names a context already is left as it
//! is.

use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec};
use crate::BoxFuture;

/// `inner`'s tools, their `docker` calls pinned to `context` when there is one.
pub(crate) struct DockerContext<'a> {
    pub(crate) inner: &'a dyn ProcessRunner,
    pub(crate) context: Option<&'a str>,
}

impl ProcessRunner for DockerContext<'_> {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        match self.context.and_then(|context| pinned(spec, context)) {
            None => self.inner.run(spec, on_line),
            Some(pinned) => Box::pin(async move { self.inner.run(&pinned, on_line).await }),
        }
    }
}

/// `spec` with `--context <context>` before its arguments, when it is a `docker` call that names
/// no context; none when it is left as it is.
fn pinned(spec: &ProcessSpec, context: &str) -> Option<ProcessSpec> {
    if spec.program != "docker" || spec.args.first().is_some_and(|a| a == "--context") {
        return None;
    }
    let mut pinned = spec.clone();
    pinned.args.splice(0..0, ["--context".to_owned(), context.to_owned()]);
    Some(pinned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_s_docker_calls_go_to_its_context_and_nothing_else_changes() {
        let compose = ProcessSpec::new("docker", ["compose", "-p", "web", "up"]);
        assert_eq!(
            pinned(&compose, "orbstack").map(|s| s.args),
            Some(vec![
                "--context".into(),
                "orbstack".into(),
                "compose".into(),
                "-p".into(),
                "web".into(),
                "up".into()
            ])
        );
        assert!(pinned(&ProcessSpec::new("vagrant", ["up"]), "orbstack").is_none(), "another tool");
        assert!(
            pinned(&ProcessSpec::new("docker", ["--context", "colima", "ps"]), "orbstack").is_none(),
            "never two contexts"
        );
    }
}
