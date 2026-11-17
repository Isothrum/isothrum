//! HashiCorp's Vagrant VMware Utility: the system service the `vagrant-vmware-desktop` plugin
//! drives VMware Fusion and Workstation through. It isn't a Vagrant plugin, nor in winget or
//! Homebrew's core taps in a form fit for Apple silicon, so it is installed from HashiCorp's own
//! signed package, pinned here with its checksum:
//!
//! - macOS: the disk image downloaded as the user, then, with macOS's administrator prompt,
//!   copied into a folder only root can write, its SHA-256 checked, its package checked as signed
//!   by HashiCorp's Developer ID and accepted by Gatekeeper (notarized), and installed with
//!   `installer`. The package's own script generates the certificates and starts the service.
//! - Linux: HashiCorp's apt repository, its key checked against its pinned fingerprint
//!   ([`super::linux::scripts::vmware_utility_script`]), as root.
//!
//! Every path ends in a real result: a declined prompt, a checksum or a signature that doesn't
//! match, or an installer error fails the step, and the machine check then says whether the
//! service answers. Nothing reports success on the strength of an installer merely opened.

use super::steps::{Note, Step};
use crate::host::Arch;

/// The release installed. Its files and checksums are HashiCorp's published ones
/// (`releases.hashicorp.com/vagrant-vmware-utility/<version>/…_SHA256SUMS`, signed with HashiCorp's
/// release key); a new release changes the version and every checksum below together.
const VERSION: &str = "1.0.24";

/// Where HashiCorp publishes the release's files.
fn url(file: &str) -> String {
    format!("https://releases.hashicorp.com/vagrant-vmware-utility/{VERSION}/{file}")
}

/// The macOS disk image for CPU `arch`, with its SHA-256. HashiCorp
/// publishes one universal image under both names.
fn mac_image(arch: Arch) -> (String, &'static str) {
    let arch = match arch {
        Arch::Amd64 => "amd64",
        Arch::Arm64 => "arm64",
    };
    (
        format!("vagrant-vmware-utility_{VERSION}_darwin_{arch}.dmg"),
        "5f42cb017dd07270409eaee7c3b4a164ffa7c0f21d85c65840c4f81aab21d457",
    )
}

/// The team of HashiCorp's Apple Developer ID, which signs the package (and is notarized).
const HASHICORP_TEAM: &str = "D38WU7D763";

/// The root half of the macOS install, run by `/bin/sh -c` with the downloaded image as `$1`:
/// copied where only root writes (so nothing can swap it between the check and the install),
/// checked, mounted there, its package checked as HashiCorp's and installed.
fn mac_root_script(sha256: &str) -> String {
    format!(
        r#"set -e
d="$(mktemp -d /private/tmp/isothrum-vmware-utility.XXXXXX)"
m=""
trap '[ -z "$m" ] || hdiutil detach -quiet "$m"; rm -rf "$d"' EXIT
cp "$1" "$d/utility.dmg"
[ "$(shasum -a 256 "$d/utility.dmg" | awk '{{print $1}}')" = "{sha256}" ] || {{ echo "The download isn't HashiCorp's Vagrant VMware Utility {VERSION} (its SHA-256 doesn't match). Nothing was installed." >&2; exit 1; }}
m="$(hdiutil attach -nobrowse -noautoopen -readonly -mountrandom "$d" "$d/utility.dmg" | awk -F'\t' '$NF ~ /^\// {{print $NF; exit}}')"
p="$m/VagrantVMwareUtility.pkg"
pkgutil --check-signature "$p" | grep -qF "Developer ID Installer: Hashicorp, Inc. ({HASHICORP_TEAM})" || {{ echo "The package isn't signed by HashiCorp. Nothing was installed." >&2; exit 1; }}
spctl --assess --type install "$p" || {{ echo "macOS doesn't accept the package (not notarized). Nothing was installed." >&2; exit 1; }}
installer -pkg "$p" -target /"#
    )
}

/// The user's half, run by `sh -c` with the root half as `$1`: the download into a private
/// folder, its checksum (a broken download stops before any prompt), then the root half through
/// macOS's administrator prompt, the image's path handed over as an argument (never spliced into
/// the script).
fn mac_script(file: &str, sha256: &str) -> String {
    let url = url(file);
    format!(
        r#"set -e
d="$(mktemp -d)"
trap 'rm -rf "$d"' EXIT
curl -fsSL --proto '=https' --tlsv1.2 "{url}" -o "$d/utility.dmg"
[ "$(shasum -a 256 "$d/utility.dmg" | awk '{{print $1}}')" = "{sha256}" ] || {{ echo "The download isn't HashiCorp's Vagrant VMware Utility {VERSION} (its SHA-256 doesn't match). Nothing was installed." >&2; exit 1; }}
echo "Downloaded and checked. Installing it: macOS asks for your password."
if ! out="$(osascript -e 'on run argv' -e 'do shell script "/bin/sh -c " & quoted form of (item 1 of argv) & " sh " & quoted form of (item 2 of argv) with administrator privileges' -e 'end run' "$1" "$d/utility.dmg" 2>&1)"; then
  case "$out" in
    *-128*) echo "The password prompt was cancelled, so nothing was installed. Try again and enter your password." >&2 ;;
    *) echo "$out" >&2 ;;
  esac
  exit 1
fi
echo "$out""#
    )
}

/// The macOS install: one step, as the user, raising macOS's administrator prompt itself.
#[must_use]
pub(crate) fn mac_install(arch: Arch) -> Step {
    let (file, sha256) = mac_image(arch);
    let mut step = super::steps::step("sh", &["-c", &mac_script(&file, sha256), "sh", &mac_root_script(sha256)]);
    step.note = Some(Note::DownloadingVmwareUtility { version: VERSION });
    step
}

#[cfg(test)]
mod tests;
