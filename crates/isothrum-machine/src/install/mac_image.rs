//! A macOS tool installed from its vendor's disk image or package, the one way every such tool
//! installs (Docker Desktop, Vagrant, VirtualBox, UTM, the AWS CLI): nothing is left for its user
//! to drag or finish in a window of its own, and nothing is built on this Mac.
//!
//! The step runs in two halves. The user's half downloads the vendor's file ([`Source`]): with
//! Homebrew (`brew fetch --cask`, which checks the cask's checksum), or from the vendor's own
//! address over HTTPS, where no cask carries it (the AWS CLI's package, whose signature is all
//! that is checked: its address always serves the latest release). It hands the file to the root
//! half through macOS's administrator prompt, or `sudo -n` where nobody is there to answer one
//! ([`Elevation`]). The
//! root half copies the file where only root writes, so nothing can swap it between the checks
//! and the install, mounts a disk image there read-only and out of Finder's sight (`hdiutil attach
//! -nobrowse -readonly`), checks what it installs as signed by the vendor's Developer ID team
//! (`codesign`, `pkgutil --check-signature`) and accepted by Gatekeeper (`spctl`), then installs
//! it: an app copied into Applications with `ditto` under a hidden name and moved into place once
//! whole, a package through `installer`, Docker Desktop through its own unattended installer. The
//! image is detached whatever happens. Last, the installed app is checked again (`codesign --verify
//! --deep --strict`, `spctl`), or a package without an app answers as its vendor's program
//! ([`Vendor::answers`]): the step fails unless it is whole and signed.
//!
//! A copied app counts as installed only once it is whole ([`complete`]): Finder's resumable-copy
//! marks on a bundle are a copy still under way, or one cut short, and an app whose signature
//! doesn't verify is damaged; macOS refuses to open either.

use std::path::Path;

use super::steps::{Licence, Note, Step};
use super::{Dependency, Elevation};
use crate::context::Context;

/// What a vendor's file holds, and how it is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Payload {
    /// An app bundle in the disk image, copied into Applications.
    App(&'static str),
    /// An installer package, in the disk image or the download itself, installed by `installer`;
    /// the app it puts in Applications, when it puts one there.
    Pkg(Option<&'static str>),
    /// Docker Desktop: `Docker.app` in the disk image, installed by its own unattended installer
    /// (`Contents/MacOS/install --accept-license --user <user>`), which copies it into
    /// Applications, sets up its privileged helper and links the `docker` CLI, so its first start
    /// asks its user nothing. It accepts Docker's licence on their behalf.
    DockerDesktop,
}

impl Payload {
    /// The app it puts in Applications, when it puts one there.
    fn app(self) -> Option<&'static str> {
        match self {
            Payload::App(app) | Payload::Pkg(Some(app)) => Some(app),
            Payload::DockerDesktop => Some("Docker.app"),
            Payload::Pkg(None) => None,
        }
    }
}

/// Where a vendor's file is downloaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The Homebrew cask that downloads it (Homebrew checks its checksum).
    Cask(&'static str),
    /// The vendor's own HTTPS address, where no cask carries it: its file name ends in `.pkg` or
    /// `.dmg`. Needs no Homebrew.
    Url(&'static str),
}

/// A tool installed from its vendor's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Vendor {
    /// Where its file is downloaded from.
    pub source: Source,
    /// The tool's name, as its vendor writes it.
    pub name: &'static str,
    /// Who signs it, as their Developer ID certificate names them.
    pub signer: &'static str,
    /// Their Apple Developer ID team.
    pub team: &'static str,
    /// What the file holds.
    pub payload: Payload,
    /// The licence its install accepts on its user's behalf, when it does.
    pub accepts: Option<Licence>,
    /// The command its install must answer, once done, where it puts no app in Applications to
    /// check (`/usr/local/aws-cli/aws --version`): the install fails unless it does.
    pub answers: Option<&'static str>,
}

/// Docker's Subscription Service Agreement, which Docker Desktop's unattended install accepts.
pub(crate) const DOCKER_LICENCE: Licence = Licence {
    name: "Docker Subscription Service Agreement",
    url: "https://www.docker.com/legal/docker-subscription-service-agreement/",
};

/// Docker Desktop, from Docker's disk image.
pub(crate) const DOCKER: Vendor = Vendor {
    source: Source::Cask("docker-desktop"),
    name: "Docker Desktop",
    signer: "Docker Inc",
    team: "9BNSXJN65R",
    payload: Payload::DockerDesktop,
    accepts: Some(DOCKER_LICENCE),
    answers: None,
};

/// Vagrant, from HashiCorp's package in its disk image.
pub(crate) const VAGRANT: Vendor = Vendor {
    source: Source::Cask("vagrant"),
    name: "Vagrant",
    signer: "Hashicorp, Inc.",
    team: "D38WU7D763",
    payload: Payload::Pkg(None),
    accepts: None,
    answers: None,
};

/// VirtualBox, from Oracle's package in its disk image.
pub(crate) const VIRTUALBOX: Vendor = Vendor {
    source: Source::Cask("virtualbox"),
    name: "VirtualBox",
    signer: "Oracle America, Inc.",
    team: "VB5E2TV963",
    payload: Payload::Pkg(Some("VirtualBox.app")),
    accepts: None,
    answers: None,
};

/// UTM, its app from its disk image.
pub(crate) const UTM: Vendor = Vendor {
    source: Source::Cask("utm"),
    name: "UTM",
    signer: "Turing Software, LLC",
    team: "WDNLXAD4W8",
    payload: Payload::App("UTM.app"),
    accepts: None,
    answers: None,
};

/// The AWS CLI, from Amazon's own package (one for both CPUs, notarized), never Homebrew's
/// formula: on an Intel Mac Homebrew has no bottle and builds its C libraries from source, which
/// fails (`aws-c-s3`).
pub(crate) const AWS_CLI: Vendor = Vendor {
    source: Source::Url("https://awscli.amazonaws.com/AWSCLIV2.pkg"),
    name: "AWS CLI",
    signer: "AMZN Mobile LLC",
    team: "94KV3E626L",
    payload: Payload::Pkg(None),
    accepts: None,
    answers: Some("/usr/local/aws-cli/aws --version"),
};

/// The install of `dep` from `vendor`'s file, downloaded by Homebrew (`brew`, when the file comes
/// from a cask) or from its vendor's address, its root half run `elevation`'s way: macOS's
/// administrator prompt, else `sudo -n` (or as is, when root).
///
/// # Errors
///
/// [`super::Refusal::NoHomebrew`] for a cask's file without Homebrew.
pub(crate) fn install(brew: Option<&str>, dep: Dependency, vendor: &Vendor, elevation: Elevation) -> Result<Step, super::Refusal> {
    let download = match vendor.source {
        Source::Cask(cask) => cask_download(brew.ok_or(super::Refusal::NoHomebrew)?, cask, vendor.name),
        Source::Url(url) => url_download(url, vendor.name),
    };
    let users = users_half(&download, vendor, elevation);
    let mut step = super::steps::step("sh", &["-c", &users, "sh", &root_half(vendor)]);
    step.note = Some(Note::InstallingFromVendor {
        dependency: dep,
        accepts: vendor.accepts,
    });
    Ok(step)
}

/// The user's half, run by `sh -c` with the root half as `$1`: `download` (which leaves the file's
/// path in `$f`), then the root half as an administrator, handed the file, this account's name
/// and the Applications folder as arguments (never spliced into it).
fn users_half(download: &str, vendor: &Vendor, elevation: Elevation) -> String {
    let name = vendor.name;
    let licence = vendor
        .accepts
        .map(|l| format!("echo \"Installing {name} accepts the {} ({}) for you.\"\n", l.name, l.url))
        .unwrap_or_default();
    let elevated = as_administrator(elevation, &format!("Installing {name}"), &[r#""$f""#, r#""$(id -un)""#, "/Applications"]);
    format!("set -e\n{download}\n{licence}{elevated}")
}

/// The vendor's file from its own address `url`, into a private folder removed when the user's
/// half ends (the root half has its own copy by then): `$f`.
fn url_download(url: &str, name: &str) -> String {
    let file = url.rsplit('/').next().unwrap_or_default();
    format!(
        r#"w="$(mktemp -d)"
trap 'rm -rf "$w"' EXIT
f="$w/{file}"
echo "Downloading {name} from {url}"
curl -fsSL --proto '=https' --tlsv1.2 '{url}' -o "$f" || {{ echo "{name} couldn't be downloaded from {url}. Nothing was installed." >&2; exit 1; }}"#
    )
}

/// The vendor's file from Homebrew's `cask` (an old Homebrew that crashes reading today's cask
/// data, `undefined method 'first' for nil` in `api/cask.rb`, is updated and the download tried
/// once more): `$f`, in Homebrew's cache.
fn cask_download(brew: &str, cask: &str, name: &str) -> String {
    format!(
        r#"if ! '{brew}' fetch --cask {cask}; then
  echo "Homebrew couldn't download {name}. Updating Homebrew and trying again (this can take a few minutes)…"
  '{brew}' update || true
  if ! '{brew}' fetch --cask {cask}; then
    echo "Homebrew still can't download {name}. Run 'brew update' in Terminal, or install {name} from its website, then try again." >&2
    exit 1
  fi
fi
f="$('{brew}' --cache --cask {cask})"
[ -f "$f" ] || {{ echo "Homebrew downloaded {name}, but its file isn't where Homebrew says ($f). Nothing was installed." >&2; exit 1; }}"#
    )
}

/// The lines of a user's script that run its root half (`$1`) as an administrator `elevation`'s
/// way, handed `args` (shell words, each one argument: never spliced into the root half), after
/// saying `doing` (`Installing UTM`). With someone there, macOS's administrator prompt (its own
/// dialog; cancelled, it says so); else `sudo -n`, which refuses at once rather than waits for a
/// password, or as is when root. An ssh session can't show macOS's prompt: it is refused there
/// (`-60007`), so a run with nobody at the Mac goes unattended.
pub(crate) fn as_administrator(elevation: Elevation, doing: &str, args: &[&str]) -> String {
    let words = args.join(" ");
    match elevation {
        Elevation::Prompt => {
            let items = (2..=args.len() + 1)
                .map(|i| format!(" & \" \" & quoted form of (item {i} of argv)"))
                .collect::<Vec<_>>()
                .concat();
            format!(
                r#"echo "{doing}: macOS asks for your password."
if ! out="$(osascript -e 'on run argv' -e 'do shell script "/bin/sh -c " & quoted form of (item 1 of argv) & " sh"{items} with administrator privileges' -e 'end run' "$1" {words} 2>&1)"; then
  case "$out" in
    *-128*) echo "The password prompt was cancelled, so nothing was installed. Try again and enter your password." >&2 ;;
    *) printf '%s\n' "$out" | tr '\r' '\n' >&2 ;;
  esac
  exit 1
fi
printf '%s\n' "$out" | tr '\r' '\n'"#
            )
        }
        Elevation::Unattended { root } => {
            let sudo = if root { "" } else { "sudo -n " };
            format!(
                r#"echo "{doing} as an administrator."
{sudo}/bin/sh -c "$1" sh {words}"#
            )
        }
    }
}

/// The code requirement of a bundle signed by Developer ID team `team`.
fn signed_by(team: &str) -> String {
    format!("-R=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"")
}

/// The root half's install, the vendor's file opened in `$src`: its payload checked as signed,
/// then installed (into the Applications folder, `$3`).
fn payload_install(vendor: &Vendor) -> String {
    let Vendor {
        name, signer, team, payload, ..
    } = *vendor;
    let signed = signed_by(team);
    let image_app = |app: &str| {
        format!(
            r#"a="$src/{app}"
[ -d "$a" ] || fail "{name}'s disk image has no {app}."
codesign --verify --deep --strict '{signed}' "$a" || fail "{app} in {name}'s disk image isn't signed by {signer} ({team})."
spctl --assess --type execute "$a" || fail "macOS doesn't accept {app} in {name}'s disk image (not notarized).""#
        )
    };
    match payload {
        Payload::App(app) => format!(
            r#"{check}
t="$3/.{app}.isothrum"
rm -rf "$t"
ditto "$a" "$t"
rm -rf "$3/{app}"
mv "$t" "$3/{app}""#,
            check = image_app(app)
        ),
        Payload::Pkg(_) => format!(
            r#"p="$(find "$src" -maxdepth 1 -name '*.pkg' | head -n 1)"
[ -n "$p" ] || fail "{name}'s download has no installer package."
pkgutil --check-signature "$p" | grep -q 'Developer ID Installer: .*({team})' || fail "{name}'s package isn't signed by {signer} ({team})."
spctl --assess --type install "$p" || fail "macOS doesn't accept {name}'s package (not notarized)."
installer -pkg "$p" -target /"#
        ),
        Payload::DockerDesktop => format!(
            r#"{check}
rm -rf "$3/Docker.app"
"$a/Contents/MacOS/install" --accept-license --user="$2""#,
            check = image_app("Docker.app")
        ),
    }
}

/// The root half's last check: the app the install put in the Applications folder (`$3`), when
/// it puts one there, whole and signed by the vendor, accepted by Gatekeeper; else the command the
/// install must answer, when there is one.
fn installed_check(vendor: &Vendor) -> String {
    let Vendor {
        name, team, payload, answers, ..
    } = *vendor;
    let signed = signed_by(team);
    let answered = answers.map_or_else(String::new, |command| {
        format!(
            r#"
{command} || {{ echo "{name} was installed, but \`{command}\` doesn't answer." >&2; exit 1; }}"#
        )
    });
    answered + &payload.app().map_or_else(String::new, |app| {
        format!(
            r#"
i="$3/{app}"
codesign --verify --deep --strict '{signed}' "$i" || {{ echo "{name} was installed, but {app} isn't whole: its signature doesn't verify. Try again." >&2; exit 1; }}
spctl --assess --type execute "$i" || {{ echo "{name} was installed, but macOS doesn't accept {app}." >&2; exit 1; }}
xattr -dr com.apple.quarantine "$i" 2>/dev/null || true"#
        )
    })
}

/// The root half, run by `/bin/sh -c` with the downloaded file as `$1`, the user's account name as
/// `$2` and the Applications folder as `$3`.
fn root_half(vendor: &Vendor) -> String {
    let Vendor { name, signer, .. } = *vendor;
    let (install, verify) = (payload_install(vendor), installed_check(vendor));
    format!(
        r#"set -e
fail() {{ echo "$1 Nothing was installed." >&2; exit 1; }}
d="$(mktemp -d /tmp/isothrum-install.XXXXXX)"
m=""
trap '[ -z "$m" ] || hdiutil detach -quiet "$m" || hdiutil detach -quiet -force "$m"; rm -rf "$d"' EXIT
case "$1" in
  *.dmg)
    cp "$1" "$d/image.dmg"
    m="$(hdiutil attach -nobrowse -noautoopen -readonly -mountrandom "$d" "$d/image.dmg" </dev/null | awk -F'\t' '$NF ~ /^\// {{print $NF; exit}}')"
    [ -n "$m" ] || fail "{name}'s disk image couldn't be opened."
    src="$m"
    ;;
  *.pkg)
    mkdir "$d/package"
    cp "$1" "$d/package/"
    src="$d/package"
    ;;
  *) fail "The download of {name} is neither a disk image nor a package ($1)." ;;
esac
{install}{verify}
echo "{name} is installed, signed by {signer}.""#
    )
}

/// Whether one of a bundle's extended attributes (`xattr`'s listing line) marks a copy Finder
/// hasn't finished: its resumable-copy marks.
fn copy_under_way(attribute: &str) -> bool {
    let attribute = attribute.trim();
    attribute.contains("kMDItemResumableCopy") || attribute.starts_with("com.apple.finder.copy")
}

/// Whether the macOS app bundle `app` is installed whole: there, without Finder's marks of a copy
/// under way (or cut short), and its signature verifying (`codesign --verify --strict`, its
/// resources included). A bundle that isn't is installed again: macOS says it is damaged.
pub(crate) async fn complete(cx: &Context, app: &Path) -> bool {
    if !app.is_dir() {
        return false;
    }
    let at = app.display().to_string();
    if let Ok(attributes) = cx.read_ok("xattr", &[&at]).await
        && attributes.lines().any(copy_under_way)
    {
        return false;
    }
    cx.read_ok("codesign", &["--verify", "--strict", &at]).await.is_ok()
}

#[cfg(test)]
mod tests;
