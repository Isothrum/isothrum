//! A forward's protocol, which `vagrant port` doesn't say (`forwarded_port,<guest>,<host>`, with
//! or without `--machine-readable`, on every provider): read from the Vagrantfile's own
//! `forwarded_port` lines, as Vagrant's syntax writes them.
//!
//! A line `<v>.vm.network "forwarded_port", guest: 53, …, protocol: "udp"` declares guest port 53
//! as UDP (`protocol` defaults to `tcp`, as in Vagrant), for the machine whose
//! `config.vm.define "<name>" do |<v>|` block `<v>` is, else for every machine (a line on the
//! `Vagrant.configure` block's own variable). A forward `disabled: true` forwards nothing.
//!
//! The Vagrantfile is Ruby and isn't run here: what isn't written literally is unknown, and makes
//! the protocol it could touch unknown, never guessed. A guest port, a protocol or a machine name
//! computed (`guest: base + 1`, `define "node#{i}"`) counts for every guest port, protocol or
//! machine; a guest port forwarded as both TCP and UDP to one machine has no protocol known,
//! since `vagrant port` doesn't say which host port is which.

use std::collections::BTreeMap;
use std::path::Path;

use crate::project::platform::Protocol;

/// The forwards a Vagrantfile declares, as far as its text says them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Declared(Vec<Forward>);

/// One `forwarded_port` line: `None` where the Vagrantfile computes the value.
#[derive(Debug, PartialEq, Eq)]
struct Forward {
    /// The machine it forwards for; `None` for every machine.
    machine: Option<String>,
    guest: Option<u16>,
    protocol: Option<Protocol>,
}

impl Declared {
    /// The forwards the Vagrantfile at `path` declares; none when it can't be read (the run's
    /// ports are then of unknown protocol, as before any was read).
    pub(super) fn of_file(path: &Path) -> Declared {
        std::fs::read_to_string(path).map(|text| Declared::read(&text)).unwrap_or_default()
    }

    /// The forwards the Vagrantfile `text` declares.
    pub(super) fn read(text: &str) -> Declared {
        // The machine each block variable stands for: `None` when its name is computed.
        let mut blocks: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut forwards = Vec::new();
        for statement in statements(text) {
            if let Some((var, name)) = define(&statement) {
                blocks.insert(var, name);
            } else if let Some((receiver, args)) = forwarded_port(&statement) {
                if arg(args, "disabled") == Some("true") {
                    continue;
                }
                forwards.push(Forward {
                    machine: blocks.get(receiver).cloned().flatten(),
                    guest: arg(args, "guest").and_then(|v| v.replace('_', "").parse().ok()),
                    protocol: arg(args, "protocol").map_or(Some(Protocol::Tcp), |v| Protocol::parse(literal(v)?)),
                });
            }
        }
        Declared(forwards)
    }

    /// The protocol `machine`'s forward of guest port `guest` has: the one every line that could
    /// declare it says, else unknown.
    pub(super) fn protocol(&self, machine: &str, guest: u16) -> Option<Protocol> {
        let mut said = self
            .0
            .iter()
            .filter(|f| f.machine.as_deref().is_none_or(|m| m == machine) && f.guest.is_none_or(|g| g == guest))
            .map(|f| f.protocol);
        let first = said.next()??;
        said.all(|p| p == Some(first)).then_some(first)
    }
}

/// The Vagrantfile's statements, one per line, comments left out (`# …`, `=begin` … `=end`), a
/// line ending in `,`, `(` or `\` joined with the next.
fn statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = String::new();
    let mut in_block_comment = false;
    for line in text.lines() {
        if in_block_comment {
            in_block_comment = !line.starts_with("=end");
            continue;
        }
        if line.starts_with("=begin") {
            in_block_comment = true;
            continue;
        }
        let code = uncommented(line).trim();
        let continued = code.ends_with([',', '(', '\\']);
        open.push_str(code.trim_end_matches('\\'));
        open.push(' ');
        if !continued {
            out.push(std::mem::take(&mut open).trim().to_owned());
        }
    }
    out.push(open.trim().to_owned());
    out.retain(|s| !s.is_empty());
    out
}

/// `line` up to its comment: a `#` outside a string.
fn uncommented(line: &str) -> &str {
    let end = code(line).find(|&(_, c)| c == '#').map_or(line.len(), |(i, _)| i);
    line.get(..end).unwrap_or(line)
}

/// The characters of `text` outside its strings (`"…"`, `'…'`, a `\` escaping the next), with
/// their byte offsets.
fn code(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let (mut quote, mut escaped) = (None, false);
    text.char_indices().filter(move |&(_, c)| {
        let Some(q) = quote else {
            if matches!(c, '"' | '\'') {
                quote = Some(c);
                return false;
            }
            return true;
        };
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == q {
            quote = None;
        }
        false
    })
}

/// A `<receiver>.vm.define <name> … do |<var>|` statement's variable and machine name, the name
/// `None` when it isn't written literally.
fn define(statement: &str) -> Option<(String, Option<String>)> {
    let (_, rest) = statement.split_once(".vm.define")?;
    let rest = rest.trim_start().trim_start_matches('(');
    let (_, var) = rest.rsplit_once('|').and_then(|(head, _)| head.rsplit_once('|'))?;
    let first = rest.split([',', ')']).next().unwrap_or_default().split(" do").next().unwrap_or_default();
    let name = literal(first.trim()).map(str::to_owned);
    Some((var.trim().to_owned(), name))
}

/// A `<receiver>.vm.network "forwarded_port", …` statement's receiver and the arguments after the
/// type.
fn forwarded_port(statement: &str) -> Option<(&str, &str)> {
    let (head, rest) = statement.split_once(".vm.network")?;
    let receiver = head.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next()?;
    let rest = rest.trim_start().trim_start_matches('(').trim_start();
    let (kind, args) = rest.split_once(',').unwrap_or((rest, ""));
    (literal(kind.trim()) == Some("forwarded_port")).then_some((receiver, args))
}

/// The value given to `key` among `args` (`key: v`, `"key": v`, `:key => v`, `"key" => v`), as
/// written.
fn arg<'a>(args: &'a str, key: &str) -> Option<&'a str> {
    split_args(args).into_iter().find_map(|a| {
        let a = a.trim();
        let (k, v) = match a.split_once("=>") {
            Some((k, v)) if a.starts_with([':', '"', '\'']) => (k.trim().trim_start_matches(':'), v),
            _ => a.split_once(':')?,
        };
        let k = literal(k).unwrap_or(k);
        (k == key).then(|| v.trim().trim_end_matches(')').trim_end())
    })
}

/// `args` split at the commas outside strings and brackets.
fn split_args(args: &str) -> Vec<&str> {
    let mut depth = 0_i32;
    let cuts = code(args).filter(|&(_, c)| {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        c == ',' && depth == 0
    });
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in cuts {
        out.extend(args.get(start..i));
        start = i + 1;
    }
    out.extend(args.get(start..));
    out
}

/// The word a literal string or symbol (`"udp"`, `'udp'`, `:udp`) writes; `None` for anything
/// computed (`"node#{i}"`, a variable, a call).
fn literal(value: &str) -> Option<&str> {
    let word = if let Some(s) = value.strip_prefix(':') {
        s
    } else {
        let q = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        value.strip_prefix(q)?.strip_suffix(q)?
    };
    let plain = !word.is_empty() && !word.contains(['"', '\'', '#', '\\']);
    plain.then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PER_MACHINE: &str = r#"
Vagrant.configure("2") do |config|
  config.vm.box = "debian/bookworm64" # a box
  config.vm.network "forwarded_port", guest: 8443, host: 8443
  config.vm.define "dns" do |m|
    m.vm.network "forwarded_port", guest: 53, host: ENV.fetch("HOST_PORT_DNS_53_UDP", 10053).to_i, host_ip: "127.0.0.1", protocol: "udp", auto_correct: true # UDP, not TCP
    m.vm.network :forwarded_port, guest: 80, host: 8080
  end
  config.vm.define :web, autostart: false do |m|
    m.vm.network("forwarded_port",
      :guest => 53, "protocol": :tcp,
      host: 5353)
    # m.vm.network "forwarded_port", guest: 80, host: 80, protocol: "udp"
    m.vm.network "forwarded_port", guest: 3389, host: 3389, id: "rdp", disabled: true, protocol: "udp"
  end
end
"#;

    #[test]
    fn a_forwards_protocol_is_the_one_its_machines_line_says() {
        let d = Declared::read(ONE_PER_MACHINE);
        assert_eq!(d.protocol("dns", 53), Some(Protocol::Udp));
        assert_eq!(d.protocol("dns", 80), Some(Protocol::Tcp), "tcp unless said");
        assert_eq!(d.protocol("web", 53), Some(Protocol::Tcp), "another machine's 53 is its own");
        assert_eq!(d.protocol("web", 8443), Some(Protocol::Tcp), "a line on the configure block is every machine's");
        assert_eq!(d.protocol("web", 80), None, "a commented line declares nothing");
        assert_eq!(d.protocol("web", 3389), None, "a disabled forward forwards nothing");
        assert_eq!(d.protocol("dns", 22), None, "Vagrant's own SSH forward isn't in the file");
    }

    #[test]
    fn what_the_vagrantfile_computes_leaves_the_protocol_unknown() {
        let both = "Vagrant.configure('2') do |c|\n  c.vm.define 'dns' do |m|\n    m.vm.network 'forwarded_port', guest: 53, host: 53\n    m.vm.network 'forwarded_port', guest: 53, host: 10053, protocol: 'udp'\n  end\nend\n";
        assert_eq!(
            Declared::read(both).protocol("dns", 53),
            None,
            "TCP and UDP: which host port is which is unknown"
        );

        let looped = "Vagrant.configure('2') do |c|\n  (1..2).each do |i|\n    c.vm.define \"node#{i}\" do |n|\n      n.vm.network 'forwarded_port', guest: 53, host: 53 + i, protocol: 'udp'\n    end\n  end\n  c.vm.define 'dns' do |m|\n    m.vm.network 'forwarded_port', guest: 53, host: 53\n  end\nend\n";
        let d = Declared::read(looped);
        assert_eq!(d.protocol("node1", 53), Some(Protocol::Udp), "the only line that could be node1's");
        assert_eq!(d.protocol("dns", 53), None, "a computed name could be dns too, so UDP or TCP");

        let computed = "Vagrant.configure('2') do |c|\n  c.vm.define 'a' do |m|\n    m.vm.network 'forwarded_port', guest: PORT, host: 1\n    m.vm.network 'forwarded_port', guest: 80, host: 2, protocol: PROTO\n    m.vm.network 'forwarded_port', guest: 81, host: 3, protocol: 'udp'\n  end\nend\n";
        let computed = Declared::read(computed);
        assert_eq!(computed.protocol("a", 80), None, "a computed protocol");
        assert_eq!(computed.protocol("a", 81), None, "a computed guest port could be 81, as TCP");
    }

    #[test]
    fn a_vagrantfile_that_cant_be_read_declares_nothing() {
        let d = Declared::of_file(Path::new("/nonexistent/Vagrantfile"));
        assert_eq!(d, Declared::default());
        assert_eq!(d.protocol("web", 80), None);
    }

    #[test]
    fn a_comment_starts_at_a_hash_outside_a_string() {
        assert_eq!(uncommented(r#"m.vm.define "a#b" # c"#), r#"m.vm.define "a#b" "#);
        assert_eq!(uncommented(r##"x = "a\"#" # c"##), r##"x = "a\"#" "##);
    }
}
