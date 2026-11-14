//! What the tools the runner runs print, read in one place per tool, so no driver matches a
//! tool's words where it acts on them: each reader returns a typed value (an enum with an
//! explicit `Other` or `Unknown`, an `Option` or a `Result` that says why it couldn't read), and
//! each wording it knows is backed by a fixture of the text as a named version of the tool prints
//! it (`tool_output/fixtures/<tool>/<version>-<case>`): a release that rewords a line then fails
//! one test, not a run. The tools are spawned with their messages in the C locale
//! (`process::locale`), so what they say is in the words these readers know.
//!
//! - [`vagrant`]: `status --machine-readable`, `ssh-config`, `plugin list`, the `error-exit`
//!   record, a held lock and ESXi's refused halt ([`vagrant::Failure`]).
//! - [`vboxmanage`]: `list vms`, `list systemproperties`, `showvminfo --machinereadable`.
//! - [`virsh`]: `domdisplay`. [`qemu`]: the monitor's `info vnc`, vagrant-qemu's `options.yml`.
//! - [`docker`]: the networks `docker inspect` says a container is on.
//! - [`download`]: a failed download's network error that passes, as Docker, Vagrant and curl
//!   print it. [`sign_in`]: a server refusing the run's sign-in, in each server tool's words.
//! - [`ruby`]: a Ruby tool's backtrace frames and fog's warnings.
//!
//! Tools whose whole answer is a document are read beside their driver, each in one module of
//! its own: Compose's `ps` and `config` (`driver::compose::{ps, config}`), Terraform's state
//! (`driver::terraform::state`), the clouds' CLIs (`reap::tags`) and Proxmox's API
//! (`graphical::proxmox::answer`).

pub(crate) mod docker;
pub(crate) mod download;
pub(crate) mod leftover;
pub(crate) mod qemu;
pub(crate) mod ruby;
pub(crate) mod sign_in;
pub(crate) mod vagrant;
pub(crate) mod vboxmanage;
pub(crate) mod virsh;

pub use vagrant::plugins as vagrant_plugins;

/// The failure `program` printed as a record of its own among its standard output (`stdout`),
/// for the tools that print one: Vagrant's `error-exit` (with `--machine-readable`, Vagrant prints
/// its error there, and nothing of it on standard error). What a failed tool's reason leads with
/// ([`Captured::reason`](crate::Captured::reason)).
pub(crate) fn failure_record(program: &str, stdout: &[String]) -> Option<String> {
    let name = std::path::Path::new(program).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase());
    match name.as_deref() {
        Some("vagrant") => vagrant::error(stdout),
        _ => None,
    }
}

use isoloom_manifest::Version;

/// The version `text` says (what a tool prints: `Vagrant 2.4.1`, `v2.29.1`, `1.0.0-rc.1`), as
/// Isoloom's [`Version`], so versions compare in its order (a pre-release before its release):
/// its first word that is one, else its first `<major>.<minor>[.<patch>]` (`2.3.8.dev` is 2.3.8, a
/// missing patch 0). `None` when it says none: nothing is held back then. The one reading of a
/// version for the runner's tool probes and `isothrum-machine`'s minimums.
#[must_use]
pub fn version_found(text: &str) -> Option<Version> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        .map(|w| w.strip_prefix('v').unwrap_or(w))
        .find_map(|w| Version::parse(w).ok())
        .or_else(|| {
            // Isoloom's Version is made from its text alone.
            let (major, minor, patch) = numbers_in(text)?;
            Version::parse(&format!("{major}.{minor}.{patch}")).ok()
        })
}

/// The first `<major>.<minor>[.<patch>]` in `text` (`Docker Compose version 2.17`,
/// `2.3.8.dev`), its suffix dropped and a missing patch 0.
fn numbers_in(text: &str) -> Option<(u64, u64, u64)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.')).find_map(|word| {
        let mut parts = word.split('.').filter(|p| !p.is_empty()).map(str::parse::<u64>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().and_then(Result::ok).unwrap_or(0);
        Some((major, minor, patch))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version as each tool prints it, compared in Isoloom's order: by its numbers (a missing
    /// patch 0), a pre-release before its release.
    #[test]
    fn versions_are_read_from_what_a_tool_prints_in_isoloom_s_order() {
        let v = |text: &str| version_found(text).unwrap();
        let parsed = |text: &str| Version::parse(text).unwrap();
        assert_eq!(v("5.5.1"), parsed("5.5.1"));
        assert_eq!(v("Vagrant 2.4.1"), parsed("2.4.1"));
        assert_eq!(v("v2.29.1-desktop.1"), parsed("2.29.1-desktop.1"));
        assert_eq!(v("Docker Compose version 2.17"), parsed("2.17.0"));
        assert_eq!(v("2.3.8.dev"), parsed("2.3.8"), "a suffix that isn't a pre-release is dropped");
        assert_eq!(version_found("no version here"), None);
        assert!(v("2.16.9") < v("2.17.0"));
        assert!(v("4.7") > v("4.6.5"), "a missing part is 0");
        assert!(v("4.7.5") > v("4.6.5") && v("4.6.5") >= v("4.6.5"));
        assert!(v("0.1.6") < v("0.2.0"));
        assert!(v("0.2.0-beta") < v("0.2.0"), "a pre-release comes before its release");
    }
}
