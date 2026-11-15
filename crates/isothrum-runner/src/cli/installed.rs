//! Where an app installs its tool outside `PATH`, for a tool that isn't on it, read through the
//! host's environment and root ([`HostEnv`]), never this machine's own:
//!
//! | Tool | App | Where |
//! |---|---|---|
//! | `utmctl` | UTM | `/Applications/UTM.app/Contents/MacOS/utmctl`, or the user's own `~/Applications` |
//! | `prlctl` | Parallels Desktop | `/Applications/Parallels Desktop.app/Contents/MacOS/prlctl`, or `~/Applications` |
//! | `vmrun` | VMware Fusion | `/Applications/VMware Fusion.app/Contents/Public/vmrun`, or `~/Applications` |
//! | `vmrun` | VMware Workstation | `%ProgramFiles(x86)%\VMware\VMware Workstation\vmrun.exe`, else `%ProgramFiles%`'s |
//! | `VBoxManage` | VirtualBox | its installer's folder on Windows (`VBOX_MSI_INSTALL_PATH`), else `%ProgramFiles%\Oracle\VirtualBox` |

use std::path::{Path, PathBuf};

use crate::host_env::HostEnv;

/// What an app's install says of `tool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Installed {
    /// The tool, at this path.
    At(PathBuf),
    /// The app is there, its tool isn't: why, naming where it was looked for.
    AppWithout(String),
    /// No app of the tool's is installed where it installs.
    Nothing,
}

/// The macOS app bundle `bundle` (`UTM.app`) in `/Applications` under the host's root, then in
/// the user's own `~/Applications`, each with the tool at `inside` it.
fn mac_app(env: &HostEnv, bundle: &str, inside: &[&str]) -> Vec<(PathBuf, PathBuf)> {
    let mut bases = vec![env.root().to_path_buf()];
    bases.extend(env.get("HOME").map(PathBuf::from));
    bases
        .into_iter()
        .map(|base| {
            let app = base.join("Applications").join(bundle);
            let tool = inside.iter().fold(app.clone(), |at, part| at.join(part));
            (app, tool)
        })
        .collect()
}

/// The Windows install folders `folder` under each of `variables` (`ProgramFiles`), in order,
/// each with the tool `exe` in it.
fn windows_folders(env: &HostEnv, variables: &[&str], folder: &[&str], exe: &str) -> Vec<(PathBuf, PathBuf)> {
    variables
        .iter()
        .filter_map(|v| env.get(v))
        .map(|base| {
            let at = folder.iter().fold(PathBuf::from(base), |at, part| at.join(part));
            (at.clone(), at.join(exe))
        })
        .collect()
}

/// Each place an app installs `tool`: the app's folder, and the tool in it; and the app's name.
fn places(tool: &str, env: &HostEnv) -> (&'static str, Vec<(PathBuf, PathBuf)>) {
    match tool {
        "utmctl" => ("UTM", mac_app(env, "UTM.app", &["Contents", "MacOS", "utmctl"])),
        "prlctl" => ("Parallels Desktop", mac_app(env, "Parallels Desktop.app", &["Contents", "MacOS", "prlctl"])),
        "vmrun" => {
            let mut at = mac_app(env, "VMware Fusion.app", &["Contents", "Public", "vmrun"]);
            at.extend(windows_folders(
                env,
                &["ProgramFiles(x86)", "ProgramFiles"],
                &["VMware", "VMware Workstation"],
                "vmrun.exe",
            ));
            ("VMware", at)
        }
        "VBoxManage" => {
            let mut folders: Vec<PathBuf> = env.get("VBOX_MSI_INSTALL_PATH").map(PathBuf::from).into_iter().collect();
            folders.extend(env.get("ProgramFiles").map(|p| Path::new(p).join("Oracle").join("VirtualBox")));
            ("VirtualBox", folders.into_iter().map(|f| (f.clone(), f.join("VBoxManage.exe"))).collect())
        }
        _ => ("", Vec::new()),
    }
}

/// Where `tool` is installed outside `PATH`, or why its app is there without it.
pub(crate) fn find(tool: &str, env: &HostEnv) -> Installed {
    let (app, places) = places(tool, env);
    if let Some((_, at)) = places.iter().find(|(_, t)| t.is_file()) {
        return Installed::At(at.clone());
    }
    if !places.iter().any(|(folder, _)| folder.is_dir()) {
        return Installed::Nothing;
    }
    let tried: Vec<String> = places.iter().map(|(_, t)| t.display().to_string()).collect();
    Installed::AppWithout(format!("{app} is installed but {tool} wasn't found at {}", tried.join(" or ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an empty file at `path`, its folders made.
    fn file(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("its folder");
        std::fs::write(path, "").expect("the file");
    }

    #[test]
    fn utmctl_is_found_in_the_app_under_the_root_then_in_the_users_own_applications() {
        let root = tempfile::tempdir().expect("a root");
        let home = tempfile::tempdir().expect("a home");
        let env = HostEnv::of([("HOME", home.path().as_os_str())]).rooted(root.path());
        assert_eq!(find("utmctl", &env), Installed::Nothing, "no UTM");
        let user_app = home.path().join("Applications/UTM.app");
        std::fs::create_dir_all(&user_app).expect("the user's UTM");
        let Installed::AppWithout(why) = find("utmctl", &env) else {
            panic!("UTM without utmctl")
        };
        assert!(why.starts_with("UTM is installed but utmctl wasn't found at "), "{why}");
        let users = user_app.join("Contents/MacOS/utmctl");
        file(&users);
        assert_eq!(find("utmctl", &env), Installed::At(users));
        let systems = root.path().join("Applications/UTM.app/Contents/MacOS/utmctl");
        file(&systems);
        assert_eq!(find("utmctl", &env), Installed::At(systems), "/Applications first");
    }

    #[test]
    fn vboxmanage_is_found_in_the_installers_folder_then_in_program_files() {
        let msi = tempfile::tempdir().expect("an installer's folder");
        let program_files = tempfile::tempdir().expect("ProgramFiles");
        let env = HostEnv::of([
            ("VBOX_MSI_INSTALL_PATH", msi.path().as_os_str()),
            ("ProgramFiles", program_files.path().as_os_str()),
        ]);
        let Installed::AppWithout(why) = find("VBoxManage", &env) else {
            panic!("an installer's folder without VBoxManage")
        };
        assert!(why.starts_with("VirtualBox is installed but VBoxManage wasn't found at "), "{why}");
        let in_program_files = program_files.path().join("Oracle/VirtualBox/VBoxManage.exe");
        file(&in_program_files);
        assert_eq!(find("VBoxManage", &env), Installed::At(in_program_files));
        let in_msi = msi.path().join("VBoxManage.exe");
        file(&in_msi);
        assert_eq!(find("VBoxManage", &env), Installed::At(in_msi), "the installer's own folder first");
        assert_eq!(find("vagrant", &env), Installed::Nothing, "only the tools an app installs off PATH");
    }

    #[test]
    fn prlctl_is_found_in_parallels_desktop_under_the_root_then_in_the_users_own_applications() {
        let root = tempfile::tempdir().expect("a root");
        let home = tempfile::tempdir().expect("a home");
        let env = HostEnv::of([("HOME", home.path().as_os_str())]).rooted(root.path());
        assert_eq!(find("prlctl", &env), Installed::Nothing, "no Parallels Desktop");
        let app = root.path().join("Applications/Parallels Desktop.app");
        std::fs::create_dir_all(&app).expect("Parallels Desktop");
        let Installed::AppWithout(why) = find("prlctl", &env) else {
            panic!("Parallels Desktop without prlctl")
        };
        assert!(why.starts_with("Parallels Desktop is installed but prlctl wasn't found at "), "{why}");
        let users = home.path().join("Applications/Parallels Desktop.app/Contents/MacOS/prlctl");
        file(&users);
        assert_eq!(find("prlctl", &env), Installed::At(users));
        let systems = app.join("Contents/MacOS/prlctl");
        file(&systems);
        assert_eq!(find("prlctl", &env), Installed::At(systems), "/Applications first");
    }

    #[test]
    fn vmrun_is_found_in_vmware_fusion_on_macos_and_in_workstations_folder_on_windows() {
        let root = tempfile::tempdir().expect("a root");
        let home = tempfile::tempdir().expect("a home");
        let mac = HostEnv::of([("HOME", home.path().as_os_str())]).rooted(root.path());
        assert_eq!(find("vmrun", &mac), Installed::Nothing, "no VMware");
        let users = home.path().join("Applications/VMware Fusion.app/Contents/Public/vmrun");
        file(&users);
        assert_eq!(find("vmrun", &mac), Installed::At(users), "the user's own Fusion");
        let systems = root.path().join("Applications/VMware Fusion.app/Contents/Public/vmrun");
        file(&systems);
        assert_eq!(find("vmrun", &mac), Installed::At(systems), "/Applications first");

        let (x86, x64) = (tempfile::tempdir().expect("ProgramFiles(x86)"), tempfile::tempdir().expect("ProgramFiles"));
        let windows = HostEnv::of([("ProgramFiles(x86)", x86.path().as_os_str()), ("ProgramFiles", x64.path().as_os_str())]);
        let in_x64 = x64.path().join("VMware/VMware Workstation/vmrun.exe");
        file(&in_x64);
        assert_eq!(find("vmrun", &windows), Installed::At(in_x64));
        std::fs::create_dir_all(x86.path().join("VMware/VMware Workstation")).expect("an install without vmrun");
        let in_x86 = x86.path().join("VMware/VMware Workstation/vmrun.exe");
        file(&in_x86);
        assert_eq!(
            find("vmrun", &windows),
            Installed::At(in_x86),
            "the 32-bit folder Workstation installs in first"
        );
    }

    /// A call of `utmctl list` that isn't on PATH runs the app's own, and says which app lacks it.
    #[tokio::test]
    async fn a_tool_not_on_path_runs_from_its_apps_folder_and_an_app_without_it_says_where_it_looked() {
        use crate::process::fake::{FakeProcesses, Reply};
        let root = tempfile::tempdir().expect("a root");
        let app = root.path().join("Applications/UTM.app");
        std::fs::create_dir_all(&app).expect("UTM");
        let env = HostEnv::of(std::iter::empty::<(&str, &str)>()).rooted(root.path());
        let fake = FakeProcesses::model(|call| {
            if call.program == "utmctl" {
                Reply::missing()
            } else {
                Reply::ok("1f0e  started  lab-web")
            }
        });
        let call = crate::cli::Call {
            host_env: &env,
            ..crate::cli::testing::call(&fake)
        };
        let without = crate::cli::utmctl::name_of(&call, "1f0e").await.expect_err("no utmctl in the app");
        assert!(
            matches!(&without, crate::cli::CliError::Spawn { why, .. } if why.starts_with("UTM is installed but utmctl wasn't found at ")),
            "{without}"
        );
        let utmctl = app.join("Contents/MacOS/utmctl");
        file(&utmctl);
        let name = crate::cli::utmctl::name_of(&call, "1f0e").await.expect("the app's utmctl");
        assert_eq!(name.as_deref(), Some("lab-web"));
        // As a path: the one joined for a Mac has `/` where Windows would write `\\`.
        let ran = fake.calls().last().map(|c| std::path::PathBuf::from(&c.program));
        assert_eq!(ran.as_deref(), Some(utmctl.as_path()));
    }
}
