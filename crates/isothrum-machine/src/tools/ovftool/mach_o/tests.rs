use super::*;
use crate::tools::ovftool::tests::{ARM64, Dirs, EXE, X86_64, download, thin};

#[test]
fn the_cpus_come_from_the_mach_o_header() {
    assert_eq!(cpus(&thin(X86_64)), [Cpu::X86_64]);
    assert_eq!(cpus(&thin(ARM64)), [Cpu::Arm64]);
    // A universal binary: big-endian, its count, then one 20-byte entry per CPU.
    let mut fat = FAT_MAGIC.to_be_bytes().to_vec();
    fat.extend(2u32.to_be_bytes());
    for cpu in [X86_64, ARM64] {
        let mut entry = cpu.to_be_bytes().to_vec();
        entry.resize(20, 0);
        fat.extend(entry);
    }
    assert_eq!(cpus(&fat), [Cpu::X86_64, Cpu::Arm64]);
    assert!(is_mach_o(&fat) && is_mach_o(&thin(X86_64)));
    assert_eq!(cpus(b"#!/bin/sh\n"), []);
    assert!(!is_mach_o(b"#!/bin/sh\n") && !is_mach_o(b"MZ") && !is_mach_o(b""));
    assert!(intel_only(&cpus(&thin(X86_64))));
    assert!(!intel_only(&cpus(&fat)) && !intel_only(&cpus(&thin(ARM64))) && !intel_only(&[]));
}

#[test]
fn only_mach_o_files_are_signature_checked() {
    let d = Dirs::new("macho");
    let dir = download(&d.picked());
    let found: Vec<String> = mach_o_files(&dir)
        .iter()
        .map(|f| f.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(found, ["lib/libvmomi.dylib".to_owned(), EXE.to_owned()]);
}
