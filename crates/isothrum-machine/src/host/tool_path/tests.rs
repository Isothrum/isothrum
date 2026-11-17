use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

fn dirs(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

fn env(path: &[&str], home: Option<&str>, vars: &[(&str, &str)]) -> Env {
    Env {
        path: dirs(path),
        home: home.map(PathBuf::from),
        vars: vars.iter().map(|(k, v)| ((*k).to_owned(), OsString::from(v))).collect(),
    }
}

/// The variables of a usual Windows install.
fn windows_env(path: &[&str]) -> Env {
    env(
        path,
        Some(r"C:\Users\Ada"),
        &[
            ("SystemRoot", r"C:\Windows"),
            ("SystemDrive", "D:"),
            ("USERPROFILE", r"C:\Users\Ada"),
            ("VBOX_MSI_INSTALL_PATH", r"E:\VBox\"),
            ("ProgramFiles", r"C:\Program Files"),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ],
    )
}

/// A context on Windows whose `reg query`s `runner` answers.
fn on_windows(runner: FakeProcesses) -> Context {
    let mut host = linux_host(Path::new("/nonexistent/isothrum-machine-test"));
    host.os = HostOs::Windows;
    Context::new(Arc::new(runner), host)
}

fn shown(found: &[PathBuf]) -> Vec<String> {
    found.iter().map(|d| d.to_string_lossy().replace('/', r"\")).collect()
}

#[test]
fn appending_keeps_existing_entries_first_and_adds_each_new_dir_once() {
    let out = appended(HostOs::Linux, &dirs(&["/a", "/b"]), dirs(&["/b", "/c", "/c", ""])).unwrap();
    assert_eq!(out, dirs(&["/a", "/b", "/c"]));
    assert_eq!(appended(HostOs::Linux, &dirs(&["/a"]), dirs(&["/a"])), None);
    assert_eq!(appended(HostOs::Linux, &[], []), None);
}

#[test]
fn windows_entries_match_regardless_of_case_and_trailing_separator() {
    assert!(same_dir(
        HostOs::Windows,
        Path::new(r"C:\Program Files\Vagrant\bin\"),
        Path::new(r"c:\program files\vagrant\BIN")
    ));
    assert!(!same_dir(HostOs::Linux, Path::new("/Tools"), Path::new("/tools")));
    let current = dirs(&[r"C:\Windows\system32", r"C:\Tools\"]);
    assert_eq!(appended(HostOs::Windows, &current, dirs(&[r"c:\tools", r"C:\WINDOWS\System32"])), None);
}

/// Regression: VirtualBox read as not installed right after its installer ran, VMware
/// Workstation's `vmrun` was never found, and QEMU's winget install was offered again each pass.
#[test]
fn windows_finds_the_hypervisors_that_never_join_path() {
    let found = shown(&tool_dirs(HostOs::Windows, &windows_env(&[])));
    let has = |p: &str| found.iter().any(|d| d == p);
    assert_eq!(found.first().map(String::as_str), Some(r"E:\VBox\"), "the installer's own answer first");
    for dir in [
        r"C:\Program Files\Oracle\VirtualBox",
        r"C:\Program Files\Vagrant\bin",
        r"C:\Program Files\Docker\Docker\resources\bin",
        r"C:\Program Files\qemu",
        r"C:\Program Files (x86)\VMware\VMware Workstation",
        r"C:\Program Files (x86)\VMware\VMware Workstation\OVFTool",
        r"C:\Program Files\VMware\VMware Workstation\OVFTool",
    ] {
        assert!(has(dir), "{dir} missing from {found:?}");
    }
    assert!(found.iter().all(|d| !d.starts_with('/')), "no Unix folder on Windows: {found:?}");
    assert_eq!(tool_dirs(HostOs::Windows, &Env::default()), Vec::<PathBuf>::new());
}

/// Regression: a scan asks `utmctl` by name, and UTM keeps it inside its bundle, so an app
/// opened from Finder never found it. Docker's CLI and Fusion's OVF Tool live in bundles too.
#[test]
fn macos_finds_the_tools_that_ship_inside_app_bundles() {
    let found = tool_dirs(HostOs::Macos, &env(&[], Some("/Users/ada"), &[]));
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/Users/ada/.docker/bin",
        "/Applications/UTM.app/Contents/MacOS",
        "/Users/ada/Applications/UTM.app/Contents/MacOS",
        "/Applications/Docker.app/Contents/Resources/bin",
        "/Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool",
    ] {
        assert!(found.iter().any(|d| d == Path::new(dir)), "{dir} missing from {found:?}");
    }
    let linux = tool_dirs(HostOs::Linux, &env(&[], Some("/home/ada"), &[]));
    assert!(linux.iter().all(|d| !d.starts_with("/Applications")), "{linux:?}");
}

#[test]
fn the_future_folders_are_the_per_user_sdk_and_vagrants_under_the_system_drive() {
    assert_eq!(
        future_dirs(HostOs::Linux, &env(&[], Some("/home/ada"), &[])),
        dirs(&["/home/ada/google-cloud-sdk/bin"])
    );
    assert_eq!(shown(&future_dirs(HostOs::Windows, &windows_env(&[]))), [r"D:\HashiCorp\Vagrant\bin"]);
    assert_eq!(shown(&future_dirs(HostOs::Windows, &Env::default())), [r"C:\HashiCorp\Vagrant\bin"]);
    assert_eq!(future_dirs(HostOs::Macos, &env(&[], Some("/Users/ada"), &[])), Vec::<PathBuf>::new());
}

/// The caller's own tool folders go last, even before they exist: after a hypervisor's (Fusion's
/// OVF Tool first), and only the tool folders that exist are added.
#[test]
fn existing_tool_folders_then_the_callers_own_go_after_the_path() {
    let own = PathBuf::from("/data/tools/ovftool");
    let fusion = Path::new("/Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool");
    let exists = |d: &Path| d == Path::new("/opt/homebrew/bin") || d == fusion || d == Path::new("/usr/bin");
    let out = augmented_where(
        HostOs::Macos,
        &env(&["/usr/bin", "/bin"], Some("/Users/ada"), &[]),
        std::slice::from_ref(&own),
        exists,
    )
    .unwrap();
    assert_eq!(
        out,
        [PathBuf::from("/usr/bin"), "/bin".into(), "/opt/homebrew/bin".into(), fusion.to_path_buf(), own]
    );
    // Nothing exists, nothing is placed, no future folder on macOS: nothing to change.
    assert_eq!(augmented_where(HostOs::Macos, &env(&["/usr/bin"], None, &[]), &[], |_| false), None);
}

#[test]
fn a_registry_value_is_read_after_its_type_spaces_kept() {
    let out = "\r\nHKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment\r\n    Path    REG_EXPAND_SZ    %SystemRoot%\\system32;C:\\Program Files\\Docker\\Docker\\resources\\bin\r\n";
    assert_eq!(
        registry_value(out).as_deref(),
        Some(r"%SystemRoot%\system32;C:\Program Files\Docker\Docker\resources\bin")
    );
    assert_eq!(registry_value("    Path    REG_SZ    C:\\Tools").as_deref(), Some(r"C:\Tools"));
    assert_eq!(registry_value("    PathExt    REG_SZ    .COM;.EXE"), None);
    assert_eq!(
        registry_value("ERROR: The system was unable to find the specified registry key or value."),
        None
    );
}

#[test]
fn variables_are_expanded_as_windows_does_and_unknown_ones_kept() {
    let vars = windows_env(&[]);
    assert_eq!(expanded(r"%SystemRoot%\system32;%systemroot%", &vars), r"C:\Windows\system32;C:\Windows");
    assert_eq!(expanded(r"%USERPROFILE%\.local\bin;%NOPE%\x", &vars), r"C:\Users\Ada\.local\bin;%NOPE%\x");
    assert_eq!(expanded("100% sure", &vars), "100% sure");
    assert_eq!(expanded("%%", &vars), "%%");
}

/// An installer's new `PATH` (in the registry, unexpanded) is merged in without a restart, the
/// machine's before the user's, only folders that exist, each once.
#[tokio::test(start_paused = true)]
async fn windows_merges_the_registry_path_installers_wrote() {
    let runner = FakeProcesses::model(|call| {
        let key = call.args.get(1).map_or("", String::as_str);
        if key.starts_with("HKLM") {
            Reply::ok(
                "\r\nHKEY_LOCAL_MACHINE\\...\r\n    Path    REG_EXPAND_SZ    %SystemRoot%\\system32;C:\\Program Files\\Docker\\Docker\\resources\\bin;C:\\Gone\r\n",
            )
        } else {
            Reply::ok(
                "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    %USERPROFILE%\\AppData\\Local\\Programs\\Tool;C:\\WINDOWS\\SYSTEM32\\\r\n",
            )
        }
    });
    let cx = on_windows(runner);
    let exists = |d: &Path| !d.to_string_lossy().contains("Gone");
    let out = refreshed_where(&cx, &windows_env(&[r"C:\Windows\system32"]), exists).await.unwrap();
    let out = shown(&out);
    assert_eq!(out.first().map(String::as_str), Some(r"C:\Windows\system32"));
    assert_eq!(out.get(1).map(String::as_str), Some(r"C:\Program Files\Docker\Docker\resources\bin"));
    assert_eq!(out.get(2).map(String::as_str), Some(r"C:\Users\Ada\AppData\Local\Programs\Tool"));
    assert!(out.iter().all(|d| !d.contains("Gone")), "{out:?}");
    assert_eq!(out.iter().filter(|d| d.eq_ignore_ascii_case(r"C:\Windows\system32")).count(), 1, "{out:?}");
    assert!(out.iter().any(|d| d == r"C:\Program Files\qemu"), "the tool folders too: {out:?}");
}

#[tokio::test(start_paused = true)]
async fn a_registry_reg_cant_read_is_left_out_and_no_other_os_refreshes() {
    let failing = FakeProcesses::model(|_| Reply::fail(1, "ERROR: The system was unable to find the specified registry key or value."));
    assert_eq!(refreshed_where(&on_windows(failing), &Env::default(), |_| true).await, None);
    let missing = FakeProcesses::model(|_| Reply::missing());
    let only_tools = refreshed_where(&on_windows(missing), &windows_env(&[]), |_| true).await.unwrap();
    assert!(shown(&only_tools).iter().any(|d| d == r"C:\Program Files\Oracle\VirtualBox"), "{only_tools:?}");
    let untouched = crate::testing::answered_by(FakeProcesses::script().build());
    assert_eq!(refreshed(&untouched, &windows_env(&[])).await, None);
}

#[test]
fn a_variable_is_found_whatever_its_case_and_an_empty_one_is_none() {
    let e = env(&[], None, &[("Path", "x"), ("EMPTY", "")]);
    assert_eq!(e.var("PATH"), Some(OsStr::new("x")));
    assert_eq!(e.var("EMPTY"), None);
    assert_eq!(e.var("nope"), None);
}
