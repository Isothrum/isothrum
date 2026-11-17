//! Reading a Mach-O file's first bytes (macOS's programs and libraries): whether it is one, and
//! the CPUs it is built for.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Every Mach-O file (a program or library) under `dir`, by its first bytes.
pub(super) fn mach_o_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            found.extend(mach_o_files(&entry.path()));
        } else if kind.is_file() && head(&entry.path()).is_some_and(|h| is_mach_o(&h)) {
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// The CPU kinds a Mach-O file is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cpu {
    X86_64,
    Arm64,
    Other,
}

/// The first bytes of `file`, enough for a Mach-O header and a universal binary's list.
pub(super) fn head(file: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut buf = Vec::with_capacity(512);
    std::fs::File::open(file).ok()?.take(512).read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub(super) fn word(bytes: &[u8], at: usize, big_endian: bool) -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

pub(super) const MH_MAGIC_64: u32 = 0xfeed_facf;
pub(super) const MH_MAGIC: u32 = 0xfeed_face;
pub(super) const FAT_MAGIC: u32 = 0xcafe_babe;
pub(super) const FAT_MAGIC_64: u32 = 0xcafe_babf;

pub(super) fn is_mach_o(head: &[u8]) -> bool {
    word(head, 0, false).is_some_and(|m| m == MH_MAGIC_64 || m == MH_MAGIC) || word(head, 0, true).is_some_and(|m| m == FAT_MAGIC || m == FAT_MAGIC_64)
}

pub(super) fn cpu(kind: u32) -> Cpu {
    match kind {
        0x0100_0007 => Cpu::X86_64,
        0x0100_000c => Cpu::Arm64,
        _ => Cpu::Other,
    }
}

/// The CPUs the Mach-O file whose first bytes are `head` runs on: one for a thin file, each of a
/// universal binary's. Empty for anything else.
pub(super) fn cpus(head: &[u8]) -> Vec<Cpu> {
    match (word(head, 0, false), word(head, 0, true)) {
        (Some(MH_MAGIC_64 | MH_MAGIC), _) => word(head, 4, false).map(cpu).into_iter().collect(),
        (_, Some(magic @ (FAT_MAGIC | FAT_MAGIC_64))) => {
            let entry = if magic == FAT_MAGIC { 20 } else { 32 };
            let count = word(head, 4, true).map_or(0, |c| usize::try_from(c).unwrap_or(0));
            (0..count.min(16)).filter_map(|i| word(head, 8 + i * entry, true)).map(cpu).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether a program built for `cpus` runs on an Apple silicon Mac only through Rosetta.
pub(super) fn intel_only(cpus: &[Cpu]) -> bool {
    !cpus.is_empty() && cpus.iter().all(|c| *c == Cpu::X86_64)
}
