//! `Client::connect_exact` against services of other releases, each a process of its own: this
//! test binary, run again through `service_process` (by the test, as a fake service of another
//! release, or by the client, through a wrapper named as the `isothrum` it starts, as this
//! release's service, or the fake a `serve-as` file in the home names).

// In a test module: its helpers may unwrap and panic, as tests do.
#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command as Process, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    use interprocess::local_socket::traits::ListenerExt as _;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName as _};
    use isothrum_client::proto::{Command, PROTOCOL, Reply, Request, Target};
    use isothrum_client::{Client, ClientError, ServiceInfo, ServicePaths, VERSION};
    use isothrum_runner::store::{OperationId, Owner, Store};
    use isothrum_runner::{BoxFuture, ErrorCode, Op, RunName, Status};
    use isothrum_service::{Engine, Journal, Refusal, ServeOptions, Service};

    /// The role a `service_process` plays: `serve` (what the client starts), `fake`, `exit`, `sleep`.
    const ROLE: &str = "ISOTHRUM_TEST_ROLE";
    /// The fake's kind.
    const KIND: &str = "ISOTHRUM_TEST_KIND";
    /// The home.
    const HOME: &str = "ISOTHRUM_TEST_HOME";

    /// What an older release says it is.
    const OLD: &str = "2.9.0";

    /// A fake service of another release.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        /// Speaks another protocol, and stops when asked.
        Protocol,
        /// Another version of this protocol, and stops when asked.
        Version,
        /// Another version, with an operation running: refuses to stop.
        Busy,
        /// Holds the home and never answers.
        Hung,
        /// Holds the home and never answers, while the journal shows an operation of it unfinished.
        HungBusy,
        /// Holds the home and never answers, and its `service.json` names another process (one that
        /// started after it said it did: a pid reused).
        HungReused,
    }

    impl Kind {
        fn parse(text: &str) -> Kind {
            match text {
                "protocol" => Kind::Protocol,
                "version" => Kind::Version,
                "busy" => Kind::Busy,
                "hung" => Kind::Hung,
                "hung-busy" => Kind::HungBusy,
                "hung-reused" => Kind::HungReused,
                other => panic!("no fake {other}"),
            }
        }

        fn name(self) -> &'static str {
            match self {
                Kind::Protocol => "protocol",
                Kind::Version => "version",
                Kind::Busy => "busy",
                Kind::Hung => "hung",
                Kind::HungBusy => "hung-busy",
                Kind::HungReused => "hung-reused",
            }
        }

        fn hung(self) -> bool {
            matches!(self, Kind::Hung | Kind::HungBusy | Kind::HungReused)
        }
    }

    /// Not a test: the entry of the processes the tests start. In a test run (no role) it does
    /// nothing.
    #[test]
    fn service_process() {
        let Ok(role) = std::env::var(ROLE) else { return };
        let home = PathBuf::from(std::env::var(HOME).unwrap());
        match role.as_str() {
            "serve" => match std::fs::read_to_string(home.join("serve-as")) {
                Ok(kind) => fake(Kind::parse(kind.trim()), &home),
                Err(_) => serve(&home),
            },
            "fake" => fake(Kind::parse(&std::env::var(KIND).unwrap()), &home),
            "sleep" => std::thread::sleep(Duration::from_secs(120)),
            _ => {}
        }
        std::process::exit(0);
    }

    /// An engine that runs nothing.
    struct Idle;

    impl Engine for Idle {
        fn resolve(&self, _target: &Target) -> Result<RunName, Refusal> {
            Err(Refusal::new(ErrorCode::Unsupported, "nothing runs here"))
        }

        fn execute<'a>(&'a self, _run: &'a RunName, _command: &'a Command, _operation: OperationId, _journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
            Box::pin(async { false })
        }

        fn status<'a>(
            &'a self,
            _run: &'a RunName,
            _target: Option<&'a Target>,
            _options: &'a isothrum_client::proto::OpOptions,
        ) -> BoxFuture<'a, Result<Status, Refusal>> {
            Box::pin(async { Err(Refusal::new(ErrorCode::NotRunning, "nothing runs here")) })
        }
    }

    /// This release's service on `home`, until asked to stop.
    fn serve(home: &Path) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let service = Service::hold(home).unwrap();
            let options = ServeOptions::new(Arc::new(Idle), Box::new(|| Box::pin(std::future::pending())));
            service.serve(options).await.unwrap();
        });
    }

    /// A service of another release on `home`, as `kind` behaves, written without this release's
    /// service: its lock, its `service.json`, its endpoint.
    fn fake(kind: Kind, home: &Path) {
        let paths = ServicePaths::new(home);
        let lock = File::options().create(true).truncate(false).write(true).open(paths.lock()).unwrap();
        lock.try_lock().unwrap();
        let endpoint = if cfg!(windows) {
            format!(r"\\.\pipe\isothrum-fake-{}", std::process::id())
        } else {
            paths.socket().to_str().unwrap().to_owned()
        };
        drop(std::fs::remove_file(paths.socket()));
        let name = endpoint.as_str().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new().name(name).create_sync().unwrap();
        let mut info = ServiceInfo {
            pid: std::process::id(),
            started: isothrum_runner::rfc3339(SystemTime::now()),
            version: if kind == Kind::Protocol { "9.0.0" } else { OLD }.to_owned(),
            protocol: if kind == Kind::Protocol { PROTOCOL + 1 } else { PROTOCOL },
            endpoint,
            epoch: 1,
        };
        let mut sleeper = None;
        match kind {
            Kind::HungBusy => {
                let store = Store::open(home).unwrap();
                let run = RunName::new("web").unwrap();
                store.write(|tx| tx.queue_operation(&run, Op::Up, &Owner::this_process())).unwrap();
            }
            Kind::HungReused => {
                // It says it started a minute ago, and names a process started now.
                info.started = isothrum_runner::rfc3339(SystemTime::now() - Duration::from_secs(60));
                let child = helper("sleep", home);
                info.pid = child.id();
                sleeper = Some(child);
            }
            _ => {}
        }
        std::fs::write(paths.info(), serde_json::to_vec(&info).unwrap()).unwrap();
        if kind.hung() {
            // It listens, and never answers.
            std::thread::sleep(Duration::from_secs(120));
            drop(sleeper);
            return;
        }
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if answer(kind, &info, &stream) {
                drop(std::fs::remove_file(paths.info()));
                drop(std::fs::remove_file(paths.socket()));
                return;
            }
        }
    }

    /// Answers one connection as a service of `kind`; true when it agreed to stop.
    fn answer(kind: Kind, info: &ServiceInfo, stream: &interprocess::local_socket::Stream) -> bool {
        let mut reader = BufReader::new(stream);
        let mut writer = stream;
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            let id = request.id;
            let (reply, stops) = match request.command {
                Command::Hello { .. } => (
                    Reply::Hello {
                        id,
                        protocol: info.protocol,
                        version: info.version.clone(),
                    },
                    false,
                ),
                Command::ShutdownIfIdle if kind == Kind::Busy => (
                    Reply::Busy {
                        id,
                        operations: vec![OperationId(7)],
                    },
                    false,
                ),
                Command::ShutdownIfIdle => (Reply::Ok { id }, true),
                _ => (
                    Reply::Error {
                        id,
                        code: ErrorCode::Usage,
                        message: "not in this fake".into(),
                        detail: None,
                    },
                    false,
                ),
            };
            let mut text = serde_json::to_vec(&reply).unwrap();
            text.push(b'\n');
            writer.write_all(&text).unwrap();
            if stops {
                return true;
            }
        }
        false
    }

    /// This binary as a helper process of `role` on `home`.
    fn helper(role: &str, home: &Path) -> Child {
        helper_process(role, home).spawn().unwrap()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the other process of the test: this test binary as a service of another release"
    )]
    fn helper_process(role: &str, home: &Path) -> Process {
        let mut process = Process::new(std::env::current_exe().unwrap());
        process
            .args(["tests::service_process", "--exact", "--nocapture", "--test-threads=1"])
            .env(ROLE, role)
            .env(HOME, home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        process
    }

    /// A fake service of `kind` on `home`, once it said where it listens.
    fn start_fake(kind: Kind, home: &Path) -> Child {
        let mut process = helper_process("fake", home);
        let child = process.env(KIND, kind.name()).spawn().unwrap();
        let paths = ServicePaths::new(home);
        let deadline = Instant::now() + Duration::from_secs(10);
        while paths.read_info().is_none() {
            assert!(Instant::now() < deadline, "the fake didn't start");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// The `isothrum` the client starts: a wrapper that runs this binary as the service of the home
    /// it is given (`serve --home <home> --detach`).
    fn wrapper(dir: &Path) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let path = dir.join("isothrum");
            let script = format!(
                "#!/bin/sh\n{ROLE}=serve {HOME}=\"$3\" exec '{}' tests::service_process --exact --nocapture --test-threads=1\n",
                exe.display()
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        #[cfg(windows)]
        {
            let path = dir.join("isothrum.cmd");
            let script = format!(
                "@set {ROLE}=serve\r\n@set {HOME}=%~3\r\n@\"{}\" tests::service_process --exact --nocapture --test-threads=1\r\n",
                exe.display()
            );
            std::fs::write(&path, script).unwrap();
            path
        }
    }

    /// A home and the folder of its `isothrum` (kept while the test runs).
    struct Setup {
        home: tempfile::TempDir,
        _bin: tempfile::TempDir,
        isothrum: PathBuf,
    }

    fn setup() -> Setup {
        let (home, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let isothrum = wrapper(bin.path());
        Setup { home, _bin: bin, isothrum }
    }

    impl Setup {
        async fn connect_exact(&self) -> Result<Client, ClientError> {
            tokio::time::timeout(Duration::from_secs(60), Client::connect_exact(self.home.path(), Some(&self.isothrum)))
                .await
                .expect("connect_exact took over a minute")
        }

        /// Stops this release's service `client` connects to, and waits until it is gone.
        async fn stop(&self, client: Client) {
            client.shutdown().await.unwrap();
            drop(client);
            let paths = ServicePaths::new(self.home.path());
            let deadline = Instant::now() + Duration::from_secs(10);
            while paths.read_info().is_some() {
                assert!(Instant::now() < deadline, "the service didn't stop");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    /// Whether `child` ended within `wait`.
    fn ends(child: &mut Child, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Ends `child` (a helper a test left running).
    fn reap(mut child: Child) {
        drop(child.kill());
        drop(child.wait());
    }

    /// The client is this release's, and the service's process isn't `old`.
    fn is_this_release(client: &Client, old: u32) {
        assert_eq!(client.service().version, VERSION);
        assert_eq!(client.service().protocol, PROTOCOL);
        assert_ne!(client.service().pid, old);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_no_service_it_starts_this_releases_and_a_second_call_keeps_it() {
        let s = setup();
        let first = s.connect_exact().await.unwrap();
        is_this_release(&first, 0);
        let second = s.connect_exact().await.unwrap();
        assert_eq!(second.service().pid, first.service().pid, "the same service");
        drop(second);
        s.stop(first).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_service_of_another_protocol_is_stopped_and_replaced() {
        let s = setup();
        let mut old = start_fake(Kind::Protocol, s.home.path());
        let refused = Client::connect_running(s.home.path()).await.unwrap_err();
        assert!(matches!(&refused, ClientError::Protocol(m) if m.contains("9.0.0")), "{refused}");
        let client = s.connect_exact().await.unwrap();
        is_this_release(&client, old.id());
        assert!(ends(&mut old, Duration::from_secs(5)), "the old service stopped");
        s.stop(client).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_service_of_another_version_is_stopped_and_replaced() {
        let s = setup();
        let mut old = start_fake(Kind::Version, s.home.path());
        assert_eq!(Client::connect_running(s.home.path()).await.unwrap().service().version, OLD, "connect takes it");
        let client = s.connect_exact().await.unwrap();
        is_this_release(&client, old.id());
        assert!(ends(&mut old, Duration::from_secs(5)));
        s.stop(client).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_busy_service_of_another_version_is_left_running() {
        let s = setup();
        let mut old = start_fake(Kind::Busy, s.home.path());
        let busy = s.connect_exact().await.unwrap_err();
        let ClientError::Busy { version, pid, operations } = &busy else {
            panic!("{busy:?}")
        };
        assert_eq!((version.as_str(), *pid, operations.as_slice()), (OLD, old.id(), [OperationId(7)].as_slice()));
        assert_eq!(busy.code(), ErrorCode::Locked);
        assert!(!ends(&mut old, Duration::from_millis(300)), "it runs on");
        reap(old);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hung_service_is_ended_and_replaced() {
        let s = setup();
        let mut old = start_fake(Kind::Hung, s.home.path());
        let client = s.connect_exact().await.unwrap();
        is_this_release(&client, old.id());
        assert!(ends(&mut old, Duration::from_secs(5)), "the hung service was ended");
        s.stop(client).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hung_service_with_an_unfinished_operation_is_left_running() {
        let s = setup();
        let mut old = start_fake(Kind::HungBusy, s.home.path());
        let busy = s.connect_exact().await.unwrap_err();
        assert!(
            matches!(&busy, ClientError::Busy { pid, operations, .. } if *pid == old.id() && operations.len() == 1),
            "{busy:?}"
        );
        assert!(!ends(&mut old, Duration::from_millis(300)), "it runs on");
        reap(old);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pid_that_isnt_the_hung_service_is_never_ended() {
        let s = setup();
        let old = start_fake(Kind::HungReused, s.home.path());
        let named = ServicePaths::new(s.home.path()).read_info().unwrap().pid;
        let refused = s.connect_exact().await.unwrap_err();
        assert!(matches!(&refused, ClientError::Unavailable(m) if m.contains("nothing was ended")), "{refused}");
        assert!(
            isothrum_runner::process_started_at(named).await.is_some(),
            "the process its service.json names still runs"
        );
        // The fake's own sleeper goes with it: end both.
        #[cfg(unix)]
        assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(named).unwrap()), nix::sys::signal::Signal::SIGKILL).is_ok());
        reap(old);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stale_service_json_is_never_acted_on_whether_its_pid_is_gone_or_reused() {
        let s = setup();
        let paths = ServicePaths::new(s.home.path());
        let mut gone = helper("exit", s.home.path());
        gone.wait().unwrap();
        let mut reused = helper("sleep", s.home.path());
        for pid in [gone.id(), reused.id()] {
            let stale = ServiceInfo {
                pid,
                started: "2020-01-01T00:00:00Z".into(),
                version: OLD.into(),
                protocol: PROTOCOL,
                endpoint: paths.socket().to_str().unwrap().to_owned(),
                epoch: 1,
            };
            std::fs::write(paths.info(), serde_json::to_vec(&stale).unwrap()).unwrap();
            let client = s.connect_exact().await.unwrap();
            is_this_release(&client, pid);
            s.stop(client).await;
        }
        assert!(!ends(&mut reused, Duration::from_millis(100)), "the process with the stale pid runs on");
        reap(reused);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_binary_of_another_release_is_a_typed_error_and_is_left_stopped() {
        let s = setup();
        std::fs::write(s.home.path().join("serve-as"), "version").unwrap();
        let wrong = s.connect_exact().await.unwrap_err();
        assert!(
            matches!(&wrong, ClientError::Version { client, service } if client == VERSION && service == OLD),
            "{wrong:?}"
        );
        let paths = ServicePaths::new(s.home.path());
        let deadline = Instant::now() + Duration::from_secs(5);
        while paths.read_info().is_some() {
            assert!(Instant::now() < deadline, "the service of the wrong release was stopped");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn this_releases_service_answers_a_hello_of_any_protocol_then_takes_only_shutdown_if_idle() {
        use isothrum_client::wire::{connect, write_line};
        let s = setup();
        let client = s.connect_exact().await.unwrap();
        let (mut reader, mut writer) = connect(&client.service().endpoint).await.unwrap();
        let other = PROTOCOL + 1;
        write_line(
            &mut writer,
            &Request {
                id: 0,
                command: Command::Hello { protocol: other },
            },
        )
        .await
        .unwrap();
        let hello = reader.next::<Reply>().await.unwrap().unwrap();
        assert_eq!(
            hello,
            Reply::Hello {
                id: 0,
                protocol: PROTOCOL,
                version: VERSION.to_owned()
            }
        );
        write_line(&mut writer, &Request { id: 1, command: Command::List }).await.unwrap();
        let refused = reader.next::<Reply>().await.unwrap().unwrap();
        assert!(
            matches!(&refused, Reply::Error { id: 1, message, .. } if message.contains("shutdown_if_idle")),
            "{refused:?}"
        );
        s.stop(client).await;
    }
}
