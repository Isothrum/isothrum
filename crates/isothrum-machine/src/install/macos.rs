//! Installs and removals on macOS: the vendors' own disk images and packages for the apps and the
//! AWS CLI (Homebrew downloads the apps', the AWS CLI's comes from Amazon; then they are checked
//! and installed as an administrator, with nothing left for the user to finish:
//! [`super::mac_image`]), HashiCorp's signed package for VMware's utility, or Homebrew's formulae.
//! A removal of what came from a vendor's package runs as its uninstaller does, through macOS's
//! administrator prompt.
//!
//! A tool whose vendor ships a signed build installs from it rather than from a formula Homebrew
//! may build here: on an Intel Mac Homebrew has no bottles any more (its Tier 3), so every formula
//! builds from source there, which takes long and can fail (the AWS CLI's `aws-c-s3` did).
//! Per tool:
//!
//! - Docker Desktop, Vagrant, VirtualBox, UTM: their vendor's disk image or package.
//! - The AWS CLI: Amazon's package ([`mac_image::AWS_CLI`]), on both CPUs.
//! - VMware's utility: HashiCorp's package.
//! - Terraform: HashiCorp's tap, whose formula installs HashiCorp's own build (its checksum
//!   pinned): nothing is built on either CPU.
//! - QEMU: Homebrew's formula, as no vendor ships a macOS build. An Intel Mac builds it from
//!   source, which its step says ([`Note::BuildsFromSource`]); a failed build fails the install.
//! - The Azure CLI, the Google Cloud CLI: Homebrew's casks of their vendor's builds (their only
//!   macOS ones). Their Python is a formula, built from source on an Intel Mac that lacks it.

use crate::context::Context;
use crate::host::{Arch, HostOs, Packages};
use crate::install::steps::{Note, Step, step};
use crate::install::{Dependency, Elevation, Refusal, mac_image, vmware_utility};

/// Homebrew's path, when the host has it; why not, as `refusal` says, else.
fn homebrew(packages: Option<Packages>, refusal: Refusal) -> Result<&'static str, Refusal> {
    match packages {
        Some(Packages::Homebrew(brew)) => Ok(brew),
        _ => Err(refusal),
    }
}

/// A step that runs `script` with `sh -c`, `note` said before it.
fn sh(script: &str, note: Note) -> Step {
    let mut step = step("sh", &["-c", script]);
    step.note = Some(note);
    step
}

/// The install of `dep` on macOS: Homebrew's formulae, or a vendor's disk image or package
/// (Homebrew downloads it), installed as an administrator `elevation`'s way (macOS's password
/// prompt, else `sudo -n`); VMware's utility from HashiCorp's own package, on this Mac's CPU
/// `arch`.
///
/// # Errors
///
/// [`Refusal::NotHere`] for what doesn't run on macOS, [`Refusal::NoHomebrew`] without Homebrew.
pub fn install(dep: Dependency, packages: Option<Packages>, arch: Arch, elevation: Elevation) -> Result<Vec<Step>, Refusal> {
    if !dep.runs_on(HostOs::Macos) {
        return Err(Refusal::NotHere {
            dependency: dep,
            os: HostOs::Macos,
        });
    }
    // The vendors' own packages, downloaded from them: need no Homebrew.
    match dep {
        Dependency::VmwareUtility => return Ok(vec![vmware_utility::mac_install(arch, elevation)]),
        Dependency::Awscli => return Ok(vec![mac_image::install(None, dep, &mac_image::AWS_CLI, elevation)?]),
        _ => {}
    }
    let brew = homebrew(packages, Refusal::NoHomebrew)?;
    let vendor = |vendor: &mac_image::Vendor| mac_image::install(Some(brew), dep, vendor, elevation).map(|step| vec![step]);
    Ok(match dep {
        // The apps and Vagrant: their vendor's disk image or package, as an administrator (Docker
        // Desktop's brings the docker CLI and Compose).
        Dependency::Docker => vendor(&mac_image::DOCKER)?,
        Dependency::Vagrant => vendor(&mac_image::VAGRANT)?,
        Dependency::Virtualbox => vendor(&mac_image::VIRTUALBOX)?,
        Dependency::Utm => vendor(&mac_image::UTM)?,
        // A formula (no administrator); its tap's full name since core dropped it.
        Dependency::Terraform => vec![step(brew, &["install", "hashicorp/tap/terraform"])],
        // A formula. Homebrew is updated first so an outdated one learns this macOS (it otherwise
        // refuses every install with "unknown or unsupported macOS version: :dunno"), but that
        // update never blocks the install: it runs best-effort, then the install with auto-update
        // off. A real problem (an outdated Homebrew or Xcode, a root-owned lock) surfaces from the
        // install as `StepFailed::Homebrew`. On an Intel Mac, which Homebrew has no bottles for,
        // it builds from source: its note says so.
        Dependency::Qemu => vec![sh(
            &format!("'{brew}' update || echo 'Homebrew was not updated; continuing with the install.'\nHOMEBREW_NO_AUTO_UPDATE=1 '{brew}' install qemu"),
            match arch {
                Arch::Amd64 => Note::BuildsFromSource(dep),
                Arch::Arm64 => Note::QemuUpdatingHomebrewFirst,
            },
        )],
        Dependency::Libvirt | Dependency::Wsl | Dependency::Hyperv | Dependency::VmwareUtility | Dependency::Awscli => {
            return Err(Refusal::NotHere {
                dependency: dep,
                os: HostOs::Macos,
            });
        }
        // Microsoft's prebuilt cask instead of the `azure-cli` formula: on recent macOS the
        // formula has no bottle and builds its whole tree (llvm, rust) from source, which takes
        // about an hour. The cask is a signed download; its third-party tap needs trusting.
        Dependency::Azurecli => {
            let mut cask = step(brew, &["install", "--cask", "azure-cli-preview"]);
            cask.note = Some(Note::AzureCliCask);
            vec![
                step(brew, &["tap", "azure/azure-cli"]),
                // `brew trust` exists from Homebrew 7 only; an older Homebrew needs no trust and
                // must not fail here ("Unknown command"). On 7, an untrusted tap still fails at
                // the install step with Homebrew's own message.
                sh(&format!("'{brew}' trust azure/azure-cli 2>/dev/null || true"), Note::TrustingAzureTap),
                cask,
            ]
        }
        Dependency::Gcloud => vec![step(brew, &["install", "--cask", "google-cloud-sdk"])],
    })
}

/// A shell script run with macOS's administrator prompt (the same dialog an installer shows).
fn admin(script: &str, dep: Dependency) -> Step {
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    let mut step = step("osascript", &["-e", &format!("do shell script \"{escaped}\" with administrator privileges")]);
    step.note = Some(Note::Removing(dep));
    step
}

/// VirtualBox's own uninstaller, run as an administrator, only ever from the installer disk image
/// Homebrew downloaded (`image`, attached here, then detached). Never from whatever volume happens
/// to be mounted as `/Volumes/VirtualBox*`: anyone can mount a disk image with that name and a
/// `VirtualBox_Uninstall.tool` of their own, which would then run as root.
fn virtualbox_uninstall(image: &str) -> String {
    format!(
        "m=$(hdiutil attach -nobrowse -noautoopen '{image}' | awk -F'\\t' '/\\/Volumes\\//{{print $NF; exit}}'); [ -n \"$m\" ] || exit 3; \
         \"$m/VirtualBox_Uninstall.tool\" --unattended; s=$?; hdiutil detach -quiet \"$m\"; exit $s"
    )
}

/// The macOS removal of HashiCorp's VMware utility, as the package's own `uninstall.tool` does it:
/// the service, then its folder.
const VMWARE_UTILITY_REMOVAL: &str = "/opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility service uninstall; rm -rf /opt/vagrant-vmware-desktop && (pkgutil --forget com.vagrant.vagrant-vmware-utility || true)";

/// The macOS removal of the AWS CLI Amazon's package installed, as Amazon's guide has it: its
/// folder, and its links in `/usr/local/bin` only where they point into it (never another `aws`),
/// then the package's receipt.
const AWS_CLI_REMOVAL: &str = "for l in /usr/local/bin/aws /usr/local/bin/aws_completer; do case \"$(readlink \"$l\")\" in /usr/local/aws-cli/*) rm -f \"$l\" ;; esac; done; rm -rf /usr/local/aws-cli && (pkgutil --forget com.amazon.aws.cli2 || true)";

/// The removal of `dep` on macOS: Homebrew's own packages, or the vendor's uninstaller. Only
/// Homebrew's packages (and VirtualBox's installer image, which Homebrew keeps:
/// [`virtualbox_image`]) need Homebrew: a tool installed from its vendor's package is removed
/// without it.
///
/// # Errors
///
/// [`Refusal::NotHere`], [`Refusal::HomebrewNeeded`], or [`Refusal::NoVirtualboxImage`] for
/// VirtualBox without its installer image.
pub fn uninstall(dep: Dependency, packages: Option<Packages>, virtualbox: Option<&str>) -> Result<Vec<Step>, Refusal> {
    let brew = || homebrew(packages, Refusal::HomebrewNeeded);
    Ok(match dep {
        Dependency::Terraform => vec![step(brew()?, &["uninstall", "hashicorp/tap/terraform"])],
        Dependency::Qemu => vec![step(brew()?, &["uninstall", "qemu"])],
        Dependency::Awscli => vec![admin(AWS_CLI_REMOVAL, dep)],
        Dependency::Gcloud => vec![step(brew()?, &["uninstall", "--cask", "google-cloud-sdk"])],
        Dependency::Azurecli => vec![step(brew()?, &["uninstall", "--cask", "azure-cli-preview"])],
        // Installed from the vendors' own packages: removed the way their uninstallers do.
        Dependency::Vagrant => vec![admin(
            "rm -rf /opt/vagrant /usr/local/bin/vagrant && pkgutil --forget com.vagrant.vagrant || true",
            dep,
        )],
        Dependency::Virtualbox => {
            brew()?;
            let image = virtualbox.ok_or(Refusal::NoVirtualboxImage)?;
            vec![admin(&virtualbox_uninstall(image), dep)]
        }
        Dependency::Docker => {
            let mut uninstaller = step("/Applications/Docker.app/Contents/MacOS/uninstall", &[]);
            uninstaller.note = Some(Note::Removing(dep));
            vec![uninstaller]
        }
        Dependency::Utm => vec![step(
            "osascript",
            &["-e", "tell application \"Finder\" to delete POSIX file \"/Applications/UTM.app\""],
        )],
        Dependency::VmwareUtility => vec![admin(VMWARE_UTILITY_REMOVAL, dep)],
        Dependency::Libvirt | Dependency::Wsl | Dependency::Hyperv => {
            return Err(Refusal::NotHere {
                dependency: dep,
                os: HostOs::Macos,
            });
        }
    })
}

/// Why VirtualBox's installer image can't be had.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageMissing {
    /// Homebrew didn't say where it is (or named no `.dmg` a script can quote).
    Unsaid,
    /// Homebrew named this path, and nothing is there.
    NotThere(String),
    /// Homebrew's `fetch` or `--cache` failed: why.
    Failed(super::StepFailed),
}

/// The VirtualBox installer image, from `brew --cache --cask virtualbox`'s output: a `.dmg` path
/// the administrator's script can quote.
fn virtualbox_image_from(out: &str) -> Option<String> {
    let path = out.trim();
    // Homebrew names it `….dmg`, in lower case: anything else isn't its installer image.
    let is_dmg = std::path::Path::new(path).extension().is_some_and(|e| e == "dmg");
    (!path.is_empty() && is_dmg && !path.contains(['\'', '"', '\n'])).then(|| path.to_owned())
}

/// The VirtualBox installer image Homebrew (`brew`) has, fetched again first as the user: Homebrew
/// drops the download once the cask moves on, which it does often. Run before the administrator's
/// step: as root, `~` isn't the user's home.
///
/// # Errors
///
/// [`ImageMissing`]: why there is none.
pub async fn virtualbox_image(cx: &Context, brew: &str, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<String, ImageMissing> {
    super::run_steps(cx, &[step(brew, &["fetch", "--cask", "virtualbox"])], on_line)
        .await
        .map_err(ImageMissing::Failed)?;
    let out = cx
        .read_ok(brew, &["--cache", "--cask", "virtualbox"])
        .await
        .map_err(|why| ImageMissing::Failed(super::StepFailed::NotStarted(why)))?;
    let image = virtualbox_image_from(&out).ok_or(ImageMissing::Unsaid)?;
    if !std::path::Path::new(&image).is_file() {
        return Err(ImageMissing::NotThere(image));
    }
    Ok(image)
}

#[cfg(test)]
mod tests;
