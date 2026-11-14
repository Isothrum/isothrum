//! What `VBoxManage` prints, read: `list vms` and `list systemproperties`'s machine folder. Its
//! fixtures are in `fixtures/vboxmanage/`.

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
    fn reads_virtualboxs_machine_folder() {
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
            ["lab · web", "lab · db", "<inaccessible>"]
        );
        assert_eq!(
            vms(include_str!("fixtures/vboxmanage/7.1.4-windows-list-vms.txt")),
            vms(include_str!("fixtures/vboxmanage/7.0.20-list-vms.txt"))[..2],
            "the same VMs, whatever the line ends"
        );
        assert_eq!(
            names(include_str!("fixtures/vboxmanage/7.0.20-list-vms-cut.txt")),
            ["lab · web"],
            "a cut line names no VM"
        );
        assert_eq!(vms(""), Vec::<(String, String)>::new());
        assert_eq!(
            machine_folder(include_str!("fixtures/vboxmanage/7.1.4-windows-systemproperties.txt")),
            Some(PathBuf::from("C:\\Users\\ada\\VirtualBox VMs"))
        );
    }
}
