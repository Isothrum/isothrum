//! What `VBoxManage` prints, read: `list vms`, `list systemproperties`'s machine folder and
//! `showvminfo --machinereadable`'s remote display. Its fixtures are in `fixtures/vboxmanage/`.

use std::net::SocketAddr;
use std::path::PathBuf;

/// `(name, uuid)` of each VM `VBoxManage list vms` prints (`"<name>" {<uuid>}` per line).
pub(crate) fn vms(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (head, tail) = line.rsplit_once('{')?;
            let uuid = tail.strip_suffix('}')?;
            Some((head.trim().trim_matches('"').to_owned(), uuid.to_owned()))
        })
        .collect()
}

/// The `Default machine folder:` of `VBoxManage list systemproperties`.
pub(crate) fn machine_folder(out: &str) -> Option<PathBuf> {
    out.lines()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix("Default machine folder:"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// What `showvminfo --machinereadable` says of the VM's remote display (VRDE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Vrde {
    /// Off (no extension pack when the VM booted).
    Off,
    /// On, but bound to no port (the VM is off).
    NotListening,
    /// Listening at an address that isn't an IP address, as said.
    Unreadable(String),
    /// Listening there.
    At(SocketAddr),
}

/// Reads `vrde="on"`, `vrdeport=<port in use>` and `vrdeaddress="<address>"`. An empty address
/// is VirtualBox's default, every interface (`0.0.0.0`), which the runner then refuses as off
/// its loopback.
pub(crate) fn remote_display(lines: &[String]) -> Vrde {
    let value = |key: &str| {
        lines.iter().find_map(|l| {
            l.trim_end_matches('\r')
                .strip_prefix(key)?
                .strip_prefix('=')
                .map(|v| v.trim().trim_matches('"').to_owned())
        })
    };
    if value("vrde").as_deref() != Some("on") {
        return Vrde::Off;
    }
    let Some(port) = value("vrdeport")
        .and_then(|p| p.parse::<i32>().ok())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
    else {
        return Vrde::NotListening;
    };
    let address = value("vrdeaddress").unwrap_or_default();
    if address.is_empty() {
        return Vrde::At(SocketAddr::new(std::net::IpAddr::from([0, 0, 0, 0]), port));
    }
    match address.parse() {
        Ok(ip) => Vrde::At(SocketAddr::new(ip, port)),
        Err(_) => Vrde::Unreadable(address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtualbox_listings_keep_braces_and_unicode_inside_a_name() {
        let out = "\"run · {odd} name\" {1111-aaaa}\r\n\"日本語 · dc01\" {2222}\ngarbage\n\"<inaccessible>\" {3333}\n";
        assert_eq!(
            vms(out),
            [
                ("run · {odd} name".to_owned(), "1111-aaaa".to_owned()),
                ("日本語 · dc01".to_owned(), "2222".to_owned()),
                ("<inaccessible>".to_owned(), "3333".to_owned())
            ]
        );
    }

    #[test]
    fn virtualboxs_machine_folder_is_read_and_an_empty_one_is_none() {
        let out = "API version: 7_0\nDefault machine folder:          /Users/a/VirtualBox VMs\nVRDE auth library: x\n";
        assert_eq!(machine_folder(out), Some(PathBuf::from("/Users/a/VirtualBox VMs")));
        assert_eq!(machine_folder("Default machine folder:  D:\\VMs\r\n"), Some(PathBuf::from("D:\\VMs")));
        assert_eq!(machine_folder("Default machine folder:   \n"), None);
        assert_eq!(machine_folder(""), None);
    }

    #[test]
    fn fixtures_read_as_vboxmanage_prints_them() {
        let names = |text: &str| vms(text).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(
            names(include_str!("fixtures/vboxmanage/7.0.20-list-vms.txt")),
            ["stack · web", "stack · db", "<inaccessible>"]
        );
        assert_eq!(
            vms(include_str!("fixtures/vboxmanage/7.1.4-windows-list-vms.txt")),
            vms(include_str!("fixtures/vboxmanage/7.0.20-list-vms.txt"))[..2],
            "the same VMs, whatever the line ends"
        );
        assert_eq!(
            names(include_str!("fixtures/vboxmanage/7.0.20-list-vms-cut.txt")),
            ["stack · web"],
            "a cut line names no VM"
        );
        assert_eq!(vms(""), Vec::<(String, String)>::new());
        assert_eq!(
            machine_folder(include_str!("fixtures/vboxmanage/7.1.4-windows-systemproperties.txt")),
            Some(PathBuf::from("C:\\Users\\ada\\VirtualBox VMs"))
        );
        let lines = |text: &str| text.lines().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            remote_display(&lines(include_str!("fixtures/vboxmanage/7.0.20-showvminfo-vrde-on.txt"))),
            Vrde::At("127.0.0.1:3390".parse().unwrap())
        );
        assert_eq!(
            remote_display(&lines(include_str!("fixtures/vboxmanage/7.0.20-showvminfo-vrde-off.txt"))),
            Vrde::Off
        );
    }

    #[test]
    fn virtualbox_says_whether_its_remote_display_is_on_and_where() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let on = lines(&[
            "name=\"web\"",
            "vrde=\"on\"",
            "vrdeport=3390",
            "vrdeports=\"3390-3489\"",
            "vrdeaddress=\"127.0.0.1\"",
        ]);
        assert_eq!(remote_display(&on), Vrde::At("127.0.0.1:3390".parse().unwrap()));
        assert_eq!(remote_display(&lines(&["vrde=\"off\""])), Vrde::Off);
        assert_eq!(remote_display(&lines(&[])), Vrde::Off);
        assert_eq!(remote_display(&lines(&["vrde=\"on\"", "vrdeport=-1"])), Vrde::NotListening);
        assert_eq!(
            remote_display(&lines(&["vrde=\"on\"", "vrdeport=3389", "vrdeaddress=\"\""])),
            Vrde::At("0.0.0.0:3389".parse().unwrap()),
            "every interface: refused later"
        );
        assert_eq!(
            remote_display(&lines(&["vrde=\"on\"", "vrdeport=3389", "vrdeaddress=\"vbox-host\""])),
            Vrde::Unreadable("vbox-host".into())
        );
    }
}
