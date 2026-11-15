//! Where an app installs its tool outside `PATH`, for a tool that isn't on it: UTM's `utmctl`
//! inside its app (`/Applications/UTM.app/Contents/MacOS/utmctl`, or the user's own
//! `~/Applications`), VirtualBox's `VBoxManage` in its installer's folder on Windows
//! (`VBOX_MSI_INSTALL_PATH`, else `%ProgramFiles%\Oracle\VirtualBox`). Read through the host's
//! environment and root ([`HostEnv`]), never this machine's own.

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

/// Each place an app installs `tool`: the app's folder, and the tool in it.
fn places(tool: &str, env: &HostEnv) -> Vec<(PathBuf, PathBuf)> {
    match tool {
        "utmctl" => {
            let app = |base: &Path| base.join("Applications").join("UTM.app");
            let mut apps = vec![app(env.root())];
            apps.extend(env.get("HOME").map(|home| app(Path::new(home))));
            apps.into_iter().map(|a| (a.clone(), a.join("Contents").join("MacOS").join("utmctl"))).collect()
        }
        "VBoxManage" => {
            let mut folders: Vec<PathBuf> = env.get("VBOX_MSI_INSTALL_PATH").map(PathBuf::from).into_iter().collect();
            folders.extend(env.get("ProgramFiles").map(|p| Path::new(p).join("Oracle").join("VirtualBox")));
            folders.into_iter().map(|f| (f.clone(), f.join("VBoxManage.exe"))).collect()
        }
        _ => Vec::new(),
    }
}

/// Where `tool` is installed outside `PATH`, or why its app is there without it.
pub(crate) fn find(tool: &str, env: &HostEnv) -> Installed {
    let places = places(tool, env);
    if let Some((_, at)) = places.iter().find(|(_, t)| t.is_file()) {
        return Installed::At(at.clone());
    }
    if !places.iter().any(|(app, _)| app.is_dir()) {
        return Installed::Nothing;
    }
    let app = if tool == "utmctl" { "UTM" } else { "VirtualBox" };
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
        assert_eq!(fake.calls().last().map(|c| c.program.clone()), Some(utmctl.display().to_string()));
    }
}
