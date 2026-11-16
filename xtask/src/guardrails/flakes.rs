//! What makes a test pass or fail by luck, read from the tests' own sources (CONTRIBUTING,
//! "How they are written"):
//!
//! - a test waits on a condition or an event, never on the clock: the `sleep`s in the tests are
//!   counted per file ([`SLEEPS`]), each count with why those are no wait for something to
//!   happen (a stand-in that hangs or answers late, Tokio's paused clock, the poll of a bounded
//!   wait), so a new one fails, and one taken out asks for its count to go down;
//! - a free port is held until it is used: a listener bound to port 0 then let go (`drop(l)` a
//!   few lines on, or a `.local_addr()` read off a listener the statement doesn't keep) hands its
//!   port to whichever process asks next ([`PORTS_LET_GO`]: the tests whose subject is a port let
//!   go).
//!
//! The tests' sources: a test file whole (`source::is_test_file`, and `isothrum-test-support`),
//! and any other file from its inline `#[cfg(test)] mod … {` on.

use super::source;

/// The `sleep`s each file's tests have, and why none of them waits for something to happen.
const SLEEPS: [(&str, usize, &str); 12] = [
    ("crates/isothrum-machine/src/testing.rs", 1, "`Stuck`, a stand-in tool that never answers"),
    (
        "crates/isothrum-runner/src/process/elevated/tests.rs",
        5,
        "the stand-in broker's polls of its session folder (one bounded to 3000 looks), and its `slow` request answered 600 ms late on purpose",
    ),
    (
        "crates/isothrum-runner/src/process/elevated/tests/cut_short.rs",
        1,
        "the stand-in broker's poll for the request it then cuts short",
    ),
    (
        "crates/isothrum-runner/src/process/fake.rs",
        2,
        "a scripted reply's `delay` and `pace`, which a test sets (on Tokio's paused clock)",
    ),
    (
        "crates/isothrum-runner/src/process/system/tests/stopping.rs",
        3,
        "polls for a real process's pid and its end, each bounded (1500 and 15000 looks, a 300 s timeout)",
    ),
    (
        "crates/isothrum-runner/src/run/tests/held.rs",
        2,
        "Tokio's paused clock: the app reading a run after the time a stand-in machine takes",
    ),
    (
        "crates/isothrum-runner/src/ssh/tests.rs",
        1,
        "Tokio's paused clock: a stand-in ssh-keygen that takes a second",
    ),
    (
        "crates/isothrum-runner/src/store/lock.rs",
        2,
        "Tokio's paused clock: a waiter seen still waiting 5 s on, and a lock held across a turn",
    ),
    (
        "crates/isothrum-service/tests/common/mod.rs",
        1,
        "the poll of `connect`'s bounded wait (PATIENCE) for the service to answer",
    ),
    (
        "crates/isothrum-service/tests/replace.rs",
        2,
        "stand-in services that hang: a `sleep` kind, and a hung one that listens and never answers",
    ),
    (
        "crates/isothrum-service/tests/service.rs",
        4,
        "three polls to the PATIENCE deadline; and 800 ms to see an idle service with a client NOT stop (an absence takes time to see), the one wall-clock wait left",
    ),
    ("crates/isothrum-test-support/src/lib.rs", 1, "`wait_until`'s poll, bounded by its deadline"),
];

/// The files whose tests let a bound port go on purpose, and why.
const PORTS_LET_GO: [(&str, usize, &str); 1] = [(
    "crates/isothrum-runner/src/net.rs",
    1,
    "`reachable` said no to a listener let go: the port let go is the subject",
)];

/// How many lines after the bind a port let go is looked for: bound, its port read, dropped.
const LET_GO_WITHIN: usize = 4;

/// One file's test code: its path and its lines, each with its number in the file.
struct Tests {
    path: String,
    lines: Vec<(usize, String)>,
}

/// The test code of every crate and of the xtask.
fn tests() -> Vec<Tests> {
    let mut files = source::files("crates");
    files.extend(source::files("xtask"));
    files
        .into_iter()
        .filter_map(|f| {
            let whole = source::is_test_file(&f.path) || f.path.starts_with("crates/isothrum-test-support/");
            let lines: Vec<&str> = f.text.lines().collect();
            let from = if whole {
                0
            } else {
                (0..lines.len()).find(|&i| {
                    let attr = lines.get(i).map_or("", |l| l.trim());
                    let next = lines.get(i + 1).map_or("", |l| l.trim());
                    (attr == "#[cfg(test)]" || attr.starts_with("#[cfg(all(test,")) && next.contains("mod ") && next.ends_with('{')
                })?
            };
            Some(Tests {
                lines: lines.iter().enumerate().skip(from).map(|(i, l)| (i + 1, (*l).to_owned())).collect(),
                path: f.path,
            })
        })
        .collect()
}

/// The line numbers of `lines` calling a `sleep` (`thread::sleep`, `tokio::time::sleep`, one
/// imported), comments and a function's own definition left out.
fn sleeps(lines: &[(usize, String)]) -> Vec<usize> {
    let calls = |l: &str| {
        l.match_indices("sleep(")
            .filter(|(at, _)| !l.get(..*at).is_some_and(|before| before.ends_with("fn ") || before.ends_with('_')))
            .count()
    };
    lines
        .iter()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .flat_map(|(n, l)| std::iter::repeat_n(*n, calls(l)))
        .collect()
}

/// Whether `line` binds a socket to port 0 (the system picks a free one).
fn binds_port_zero(line: &str) -> bool {
    line.contains("bind(") && (line.contains(":0\"") || line.contains(", 0))") || line.contains(",0))"))
}

/// The line numbers of `lines` letting a port the system picked go before it is used: a
/// `let <name> = …bind(…0…)` whose `<name>` is dropped within a few lines (its port read in
/// between), or a `.local_addr()` read off a bound listener the statement doesn't keep.
fn ports_let_go(lines: &[(usize, String)]) -> Vec<usize> {
    let mut found = Vec::new();
    for (i, (n, line)) in lines
        .iter()
        .enumerate()
        .filter(|(_, (_, l))| binds_port_zero(l) && !l.trim_start().starts_with("//"))
    {
        let name = line
            .trim_start()
            .strip_prefix("let ")
            .map(|rest| rest.trim_start_matches("mut "))
            .and_then(|rest| rest.split([' ', ':', '=']).next())
            .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'));
        let after = lines.get(i..).unwrap_or_default();
        // The lines a statement chained over a few lines takes.
        let statement: Vec<&str> = after.iter().take(LET_GO_WITHIN).map(|(_, l)| l.as_str()).collect();
        let statement = statement.join(" ");
        // From the bind to the statement's end (its `;`, or the block it opens).
        let read_off = statement
            .split_once("bind(")
            .is_some_and(|(_, rest)| rest.split([';', '{']).next().unwrap_or_default().contains(".local_addr()"));
        let dropped = name.is_some_and(|name| after.iter().skip(1).take(LET_GO_WITHIN).any(|(_, l)| l.contains(&format!("drop({name})"))));
        if read_off || dropped {
            found.push(*n);
        }
    }
    found
}

/// Each file whose tests `rule` finds more or fewer times than `allowed` counts for it.
fn counted(rule: fn(&[(usize, String)]) -> Vec<usize>, allowed: &[(&str, usize, &str)], list: &str, advice: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    for t in tests() {
        // The scans' own cases below are no test's.
        if t.path == "xtask/src/guardrails/flakes.rs" {
            continue;
        }
        let found = rule(&t.lines);
        let allowed = allowed.iter().find(|(p, ..)| *p == t.path).map_or(0, |(_, n, _)| *n);
        if found.len() != allowed {
            let at: Vec<String> = found.iter().map(ToString::to_string).collect();
            wrong.push(format!(
                "{}: {} found (lines {}), {allowed} allowed (xtask/src/guardrails/flakes.rs, {list}): {advice}, or lower the count when one went away",
                t.path,
                found.len(),
                at.join(", ")
            ));
        }
    }
    wrong
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(text: &str) -> Vec<(usize, String)> {
        text.lines().enumerate().map(|(i, l)| (i + 1, l.to_owned())).collect()
    }

    #[test]
    fn a_test_waits_on_a_condition_not_on_the_clock() {
        let wrong = counted(
            sleeps,
            &SLEEPS,
            "SLEEPS",
            "wait on the condition or event itself (`wait_until`, a channel, a child's exit), or on Tokio's paused clock",
        );
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_test_holds_the_free_port_it_picked_until_it_uses_it() {
        let wrong = counted(
            ports_let_go,
            &PORTS_LET_GO,
            "PORTS_LET_GO",
            "keep the listener and hand it (or its port, while it is held) to what uses it",
        );
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_flake_scans_see_what_they_look_for() {
        let sleep = "std::thread::sleep(Duration::from_secs(1));\n// thread::sleep(x)\ntokio::time::sleep(d).await;\nsleep(d).await;\nfn sleep(d: u64) {}\ntokio::time::sleep_until(t).await;\n";
        assert_eq!(sleeps(&numbered(sleep)), [1, 3, 4]);
        let ports = [
            (
                "let l = TcpListener::bind(\"127.0.0.1:0\")?;\nlet port = l.local_addr()?.port();\ndrop(l);\n",
                vec![1],
            ),
            (
                "let port = TcpListener::bind(\"127.0.0.1:0\")\n    .unwrap()\n    .local_addr()\n    .unwrap()\n    .port();\n",
                vec![1],
            ),
            ("let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;\nserve(l);\n", vec![]),
            ("let held = TcpListener::bind((\"127.0.0.1\", port))?;\ndrop(held);\n", vec![]),
            (
                "if let Ok(l) = TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)) {\n    let port = l.local_addr().unwrap().port();\n",
                vec![],
            ),
            (
                "let l = TcpListener::bind(\"127.0.0.1:0\")?;\nlet port = l.local_addr()?.port();\nlet c = connect(port);\nuse_it(c);\nserve(&l);\ndrop(l);\n",
                vec![],
            ),
        ];
        for (code, expected) in ports {
            assert_eq!(ports_let_go(&numbered(code)), expected, "{code}");
        }
    }
}
