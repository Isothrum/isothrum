use proptest::prelude::*;

use super::*;

#[test]
fn only_ssh_failing_to_connect_with_nothing_printed_is_unreachable() {
    assert!(unreachable(Exit::new(Some(UNREACHABLE)), false));
    assert!(!unreachable(Exit::new(Some(UNREACHABLE)), true), "the command ran: its own 255");
    assert!(!unreachable(Exit::new(Some(1)), false));
    assert!(!unreachable(Exit::new(None), false));
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_host_is_tried_again_every_retry_until_the_deadline() {
    let start = tokio::time::Instant::now();
    let wait = |limit| SshWait {
        limit,
        login: "root@web".to_owned(),
    };
    let two = wait(RETRY * 2);
    let mut retrying = Retrying::new(&two);
    let mut said = 0;
    assert!(retrying.again(true, || said += 1).await);
    assert!(retrying.again(true, || said += 1).await);
    assert_eq!(start.elapsed(), RETRY * 2, "each retry after RETRY");
    assert!(!retrying.again(true, || said += 1).await, "not past the deadline");
    assert_eq!(said, 1, "said once, before the first retry");
    let ten = wait(RETRY * 10);
    let mut connected = Retrying::new(&ten);
    assert!(
        !connected.again(false, || panic!("nothing to say")).await,
        "a try that connected is the outcome"
    );
    assert_eq!(start.elapsed(), RETRY * 2, "and isn't waited for");
}

/// Regression: a public key that couldn't be read was no key, and `ssh-keygen` was run over
/// the existing one, failing on its overwrite prompt.
#[tokio::test]
async fn a_public_key_that_cant_be_read_is_an_error_and_no_new_key_is_made() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(key_path(home.path()).with_extension("pub")).unwrap();
    // Nothing is scripted: running `ssh-keygen` fails the test.
    let fake = crate::process::fake::FakeProcesses::script().build();
    let err = ensure_key(&fake, home.path()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
    assert!(err.to_string().contains("id_ed25519.pub"), "{err}");
    assert_eq!(public_key(&home.path().join("elsewhere")).unwrap(), None, "a missing key is none");
}

/// `ssh-keygen` as it runs: a while to make the key, then the private half, then the public
/// one, each pair its own; over a key already there it fails on its overwrite prompt.
struct SlowKeygen {
    home: std::path::PathBuf,
    made: std::sync::atomic::AtomicUsize,
}

impl ProcessRunner for SlowKeygen {
    fn run<'a>(
        &'a self,
        _spec: &'a ProcessSpec,
        _on_line: crate::process::OnLine<'a>,
    ) -> crate::BoxFuture<'a, std::result::Result<crate::process::Exit, crate::process::ProcessError>> {
        Box::pin(async move {
            let key = key_path(&self.home);
            if key.exists() {
                return Ok(crate::process::Exit::new(Some(1)));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let n = self.made.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::write(&key, format!("private {n}")).unwrap();
            std::fs::write(key.with_extension("pub"), format!("ssh-ed25519 AAAA{n} isothrum\n")).unwrap();
            Ok(crate::process::Exit::new(Some(0)))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn two_operations_needing_the_key_at_once_make_one_and_both_get_it() {
    // Regression: each saw no key and ran `ssh-keygen`; the second wrote its key over the
    // first's, which had already handed its public half on.
    let home = tempfile::tempdir().unwrap();
    let keygen = SlowKeygen {
        home: home.path().to_path_buf(),
        made: std::sync::atomic::AtomicUsize::new(0),
    };
    let (a, b) = tokio::join!(ensure_key(&keygen, home.path()), ensure_key(&keygen, home.path()));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(keygen.made.load(std::sync::atomic::Ordering::SeqCst), 1, "one key made");
    assert_eq!(a, b);
    assert_eq!(
        public_key(home.path()).unwrap().as_deref(),
        Some(a.as_str()),
        "the key handed on is the one kept"
    );
}

#[test]
fn a_machines_login_is_its_address_as_its_user_else_the_runs_else_root() {
    let outputs = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned());
    let own = outputs(&[("address_web_1", "198.51.100.7"), ("ssh_user_web_1", "admin"), ("ssh_user", "ubuntu")]);
    let login = machine_login(own, "web-1", Some("me@jump")).unwrap().unwrap();
    assert_eq!(login.target, "admin@198.51.100.7");
    assert_eq!(login.jumps, ["me@jump"]);
    let run_wide = outputs(&[("address_web_1", "198.51.100.7"), ("ssh_user", "ubuntu")]);
    assert_eq!(machine_login(run_wide, "web-1", None).unwrap().unwrap().target, "ubuntu@198.51.100.7");
    let none = outputs(&[("address_web_1", "198.51.100.7")]);
    assert_eq!(machine_login(none, "web-1", None).unwrap().unwrap().target, "root@198.51.100.7");
    assert_eq!(machine_login(outputs(&[("address_web_1", "")]), "web-1", None).unwrap(), None, "no address yet");
    assert_eq!(machine_login(outputs(&[]), "web-1", None).unwrap(), None);
}

/// The names are Isoloom's: `-` and `.` as `_`, in lower case.
#[test]
fn a_machines_output_names_are_its_terraform_identifier_in_lower_case() {
    assert_eq!(address_output("web-1"), "address_web_1");
    assert_eq!(address_output("Web.1"), "address_web_1");
    assert_eq!(ssh_user_output("db.Primary-2"), "ssh_user_db_primary_2");
}

/// The rule held to what Isoloom generates: every Linux VM of each cloud's `cloud-vm`
/// manifest has the address and user outputs the runner reads, and the guest delivery to it
/// finds its login there.
#[test]
fn every_linux_vm_has_the_outputs_isoloom_generates_for_it() {
    let spec = "version: 1\nname: pair\nnetworks:\n  app: { cidr: 10.80.0.0/24, internet: true }\n\
                    machines:\n  web-1: { os: debian@12, networks: { app: 10 } }\n  db-main-2: { os: debian@12, networks: { app: 11 } }\n";
    let lab = isoloom_targets::Lab::parse(spec).unwrap();
    let (files, _) = isoloom_targets::generate_all(&lab);
    let clouds: Vec<_> = files
        .iter()
        .filter(|f| f.path.starts_with(".isoloom/cloud-vm/") && f.path.ends_with("/run.json"))
        .collect();
    assert!(!clouds.is_empty(), "{:?}", files.iter().map(|f| &f.path).collect::<Vec<_>>());
    for file in clouds {
        let manifest = isoloom_manifest::parse(&file.contents).unwrap();
        let names: Vec<String> = manifest.outputs.iter().map(|o| o.name.to_string()).collect();
        let output = |name: &str| names.iter().any(|n| n == name).then(|| "198.51.100.7".to_owned());
        for machine in ["web-1", "db-main-2"] {
            assert!(names.contains(&address_output(machine)), "{}: {names:?}", file.path);
            assert!(names.contains(&ssh_user_output(machine)), "{}: {names:?}", file.path);
            assert!(machine_login(output, machine, None).unwrap().is_some(), "{}", file.path);
        }
    }
}

#[test]
fn a_login_goes_through_the_runs_jump_then_the_manifests() {
    let l = Login::new("isoloom", "10.60.0.10", Some("root@pve.lan"), ",isoloom@10.10.10.5").unwrap();
    assert_eq!(l.jumps, ["root@pve.lan", "isoloom@10.10.10.5"]);
    assert_eq!(
        l.args(&[], "sh -s"),
        ["-F", "ssh_config", "-J", "root@pve.lan,isoloom@10.10.10.5", "isoloom@10.60.0.10", "sh -s"]
    );
    let direct = Login::new("isoloom", "10.0.0.9", None, "").unwrap();
    assert_eq!(direct.args(&[], "true"), ["-F", "ssh_config", "isoloom@10.0.0.9", "true"]);
}

#[test]
fn the_variables_sent_are_named_before_the_host_their_values_never_in_the_arguments() {
    let l = Login::new("isoloom", "10.0.0.9", None, "").unwrap();
    let sent = ["DEMO_SECRET".to_owned(), "TOKEN".to_owned()];
    assert_eq!(
        l.args(&sent, "sudo -E docker compose up"),
        [
            "-F",
            "ssh_config",
            "-o",
            "SendEnv=DEMO_SECRET",
            "-o",
            "SendEnv=TOKEN",
            "isoloom@10.0.0.9",
            "sudo -E docker compose up"
        ]
    );
}

/// Regression: a `ready_file` condition whose `user` filled empty logged in as `@host`, which
/// ssh takes as this computer's user; the fallback was applied by the callers, not by `Login`.
#[test]
fn a_login_with_no_user_is_root() {
    assert_eq!(Login::new("", "10.0.0.9", None, "").unwrap().target, "root@10.0.0.9");
    assert_eq!(Login::new("admin", "10.0.0.9", None, "").unwrap().target, "admin@10.0.0.9");
}

#[test]
fn an_odd_login_is_refused() {
    for (user, host, via) in [
        ("isoloom", "10.0.0.5;rm -rf /", ""),
        ("-oProxyCommand=x", "h", ""),
        ("u", "h", "a@b@c"),
        ("u", "h", "-J x"),
    ] {
        assert!(Login::new(user, host, None, via).is_err(), "{user}@{host} via {via}");
    }
}

#[test]
fn the_config_carries_the_key_and_the_runs_known_hosts_to_every_host() {
    let c = config(Path::new("/home/a b/.isothrum/ssh/id_ed25519"), Path::new("/r/known_hosts")).unwrap();
    assert!(c.contains("Host *\n"), "{c}");
    assert!(c.contains("IdentityFile \"/home/a b/.isothrum/ssh/id_ed25519\"\n"), "{c}");
    assert!(c.contains("UserKnownHostsFile \"/r/known_hosts\"\n"), "{c}");
    assert!(c.contains("StrictHostKeyChecking accept-new\n") && c.contains("BatchMode yes\n"), "{c}");
}

/// Quoted, a path reads the same to OpenSSH before 8.7 (no escapes) and after (`\` escapes).
#[test]
fn a_path_is_quoted_for_every_openssh_and_its_tokens_doubled() {
    assert_eq!(config_path("/home/a b/x 50%", PathStyle::Unix).as_deref(), Some("\"/home/a b/x 50%%\""));
    assert_eq!(config_path("it's", PathStyle::Unix).as_deref(), Some("\"it's\""));
    assert_eq!(
        config_path("C:\\Users\\a b\\.isothrum\\ssh\\id_ed25519", PathStyle::Windows).as_deref(),
        Some("\"C:/Users/a b/.isothrum/ssh/id_ed25519\"")
    );
    assert_eq!(
        config_path("\\\\server\\share\\h", PathStyle::Windows).as_deref(),
        Some("\"//server/share/h\""),
        "a network path"
    );
}

#[test]
fn a_path_no_openssh_reads_as_written_is_refused() {
    for path in ["/a\"b", "/a\\b", "/a/${HOME}", "/a\nb"] {
        assert_eq!(config_path(path, PathStyle::Unix), None, "{path:?}");
    }
    assert_eq!(config_path("C:\\a\"b", PathStyle::Windows), None);
}

#[test]
fn a_login_refused_says_what_a_login_takes() {
    let refused = Login::new("u", "h;x", None, "").unwrap_err().to_string();
    assert!(
        refused.ends_with("a login takes letters, digits, `.`, `-`, `_`, `:` and one `@` only"),
        "{refused}"
    );
    assert!(Login::new("u", "fd00::10", None, "").is_ok(), "`:` is one of them");
}

proptest! {
    #[test]
    fn a_plain_login_never_starts_an_option(user in "[a-z][a-z0-9_]{0,8}", host in "[a-z0-9.-]{1,20}") {
        let l = Login::new(&user, &host, None, "");
        prop_assert!(l.is_ok());
        let args = l.unwrap().args(&[], "true");
        prop_assert!(!args[2].starts_with('-'));
    }
}

/// Regression (#125): a private key left without its public half (an empty `.pub` from a
/// disk that filled up) ran `ssh-keygen` over it, which stopped at its overwrite prompt and
/// failed every later run. Its public half is derived from it again.
#[tokio::test]
async fn a_key_left_without_its_public_half_gets_it_back_from_the_private_one() {
    let home = tempfile::tempdir().unwrap();
    let key = key_path(home.path());
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "private").unwrap();
    std::fs::write(key.with_extension("pub"), "").unwrap();
    let fake = crate::process::fake::FakeProcesses::new(|call| {
        if call.line().contains(" -y ") {
            crate::process::fake::Reply::ok("ssh-ed25519 AAAAkept")
        } else {
            crate::process::fake::Reply::fail(1, "id_ed25519 already exists.\nOverwrite (y/n)? ")
        }
    });
    let got = ensure_key(&fake, home.path()).await.unwrap();
    assert_eq!(got, "ssh-ed25519 AAAAkept isothrum");
    assert_eq!(public_key(home.path()).unwrap().as_deref(), Some(got.as_str()));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "private", "the private key is kept");
    assert_eq!(fake.calls().len(), 1, "no new key: {:?}", fake.lines());
}

/// A private key that isn't one (a truncated write) is set aside and a new key made.
#[tokio::test]
async fn a_private_key_that_cant_be_read_is_set_aside_and_a_new_one_made() {
    let home = tempfile::tempdir().unwrap();
    let key = key_path(home.path());
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "trunc").unwrap();
    let made = key.clone();
    let fake = crate::process::fake::FakeProcesses::new(move |call| {
        if call.line().contains(" -y ") {
            return crate::process::fake::Reply::fail(255, "Load key: invalid format");
        }
        if made.exists() {
            return crate::process::fake::Reply::fail(1, "Overwrite (y/n)? ");
        }
        std::fs::write(&made, "new private").unwrap();
        std::fs::write(made.with_extension("pub"), "ssh-ed25519 AAAAnew isothrum\n").unwrap();
        crate::process::fake::Reply::ok("")
    });
    assert_eq!(ensure_key(&fake, home.path()).await.unwrap(), "ssh-ed25519 AAAAnew isothrum");
    assert_eq!(std::fs::read_to_string(key.with_extension("unusable")).unwrap(), "trunc", "set aside, not lost");
    assert_eq!(fake.calls().len(), 2, "{:?}", fake.lines());
}
