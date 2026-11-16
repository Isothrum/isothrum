use super::*;

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
    let fake = crate::process::fake::FakeProcesses::script()
        .expect("ssh-keygen -y", crate::process::fake::Reply::ok("ssh-ed25519 AAAAkept"))
        .build();
    let got = ensure_key(&fake, home.path()).await.unwrap();
    assert_eq!(got, "ssh-ed25519 AAAAkept isothrum");
    assert_eq!(public_key(home.path()).unwrap().as_deref(), Some(got.as_str()));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "private", "the private key is kept");
    // No new key: the script has no `ssh-keygen -q`, which would fail the test.
}

/// A private key that isn't one (a truncated write) is set aside and a new key made.
#[tokio::test]
async fn a_private_key_that_cant_be_read_is_set_aside_and_a_new_one_made() {
    let home = tempfile::tempdir().unwrap();
    let key = key_path(home.path());
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "trunc").unwrap();
    let made = key.clone();
    // A model of `ssh-keygen`: `-y` can't read the key, a new key is written as it writes one.
    let fake = crate::process::fake::FakeProcesses::model(move |call| {
        if call.args.iter().any(|a| a == "-y") {
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
    let derived: Vec<bool> = fake.calls().iter().map(|c| c.args.iter().any(|a| a == "-y")).collect();
    assert_eq!(derived, [true, false], "derived, then made once: {:?}", fake.lines());
}

/// Regression: a login with no user went out as `@host`, which ssh reads as a host named that.
/// A user or host that isn't a plain word (none, an option's dash) is refused.
#[test]
fn a_login_with_no_user_or_a_dashed_word_is_refused_and_a_plain_one_is_user_at_host() {
    assert!(Login::new("", "10.0.0.5").is_none(), "no user");
    assert!(Login::new("root", "-oProxyCommand=x").is_none(), "an option, not a host");
    assert!(Login::new("-l", "10.0.0.5").is_none(), "an option, not a user");
    assert_eq!(Login::new("kali", "10.0.0.5").map(|l| l.target()).as_deref(), Some("kali@10.0.0.5"));
}
