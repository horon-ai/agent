use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Result, ensure};

pub(super) fn verify_native_target(mut file: File, target: &str, path: &Path) -> Result<()> {
    let arm64 = target.starts_with("aarch64-");
    if target.ends_with("-darwin") {
        verify_mach(&mut file, arm64)
    } else if target.ends_with("-gnu") {
        verify_elf(&mut file, arm64)
    } else {
        ensure!(
            path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
            "Windows package executable needs an .exe suffix"
        );
        verify_pe(&mut file, arm64)
    }
}

fn read_at<const N: usize>(file: &mut File, position: u64) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    file.seek(SeekFrom::Start(position))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn verify_mach(file: &mut File, arm64: bool) -> Result<()> {
    // One thin 64-bit image per target. The package can declare several binaries.
    let header = read_at::<32>(file, 0)?;
    ensure!(
        header[..4] == [0xcf, 0xfa, 0xed, 0xfe],
        "expected a thin 64-bit Mach-O executable"
    );
    let cpu = if arm64 { 0x0100_000c } else { 0x0100_0007 };
    ensure!(
        u32::from_le_bytes(header[4..8].try_into()?) == cpu,
        "Mach-O CPU does not match declared target"
    );
    ensure!(
        u32::from_le_bytes(header[12..16].try_into()?) == 2,
        "Mach-O file is not executable"
    );
    let count = u32::from_le_bytes(header[16..20].try_into()?);
    let bytes = u64::from(u32::from_le_bytes(header[20..24].try_into()?));
    ensure!(
        count <= 4096 && 32 + bytes <= file.metadata()?.len(),
        "invalid Mach-O load commands"
    );
    let mut offset = 32;
    let mut macos = false;
    for _ in 0..count {
        ensure!(offset + 8 <= 32 + bytes, "truncated Mach-O load command");
        let command = read_at::<8>(file, offset)?;
        let kind = u32::from_le_bytes(command[..4].try_into()?);
        let len = u64::from(u32::from_le_bytes(command[4..8].try_into()?));
        ensure!(
            len >= 8 && len.is_multiple_of(8) && offset + len <= 32 + bytes,
            "invalid Mach-O load command length"
        );
        match kind {
            0x32 => {
                // LC_BUILD_VERSION
                ensure!(
                    len >= 24 && u32::from_le_bytes(read_at::<4>(file, offset + 8)?) == 1,
                    "Mach-O platform is not macOS"
                );
                macos = true;
            }
            0x24 => {
                // LC_VERSION_MIN_MACOSX
                ensure!(len == 16, "invalid Mach-O macOS version command");
                macos = true;
            }
            0x25 | 0x2f | 0x30 => anyhow::bail!("Mach-O platform is not macOS"),
            _ => {}
        }
        offset += len;
    }
    ensure!(macos && offset == 32 + bytes, "expected macOS Mach-O load commands");
    Ok(())
}

fn verify_elf(file: &mut File, arm64: bool) -> Result<()> {
    let header = read_at::<64>(file, 0)?;
    ensure!(
        header[..7] == *b"\x7fELF\x02\x01\x01" && matches!(header[7], 0 | 3) && header[20..24] == [1, 0, 0, 0],
        "expected a little-endian 64-bit Linux ELF executable"
    );
    let kind = u16::from_le_bytes(header[16..18].try_into()?);
    let entry = u64::from_le_bytes(header[24..32].try_into()?);
    ensure!(matches!(kind, 2 | 3) && entry != 0, "ELF file is not executable");
    ensure!(
        u16::from_le_bytes(header[18..20].try_into()?) == if arm64 { 183 } else { 62 },
        "ELF CPU does not match declared target"
    );
    let start = u64::from_le_bytes(header[32..40].try_into()?);
    let count = u64::from(u16::from_le_bytes(header[56..58].try_into()?));
    let stride = 56;
    let len = file.metadata()?.len();
    ensure!(
        u16::from_le_bytes(header[52..54].try_into()?) == 64
            && u64::from(u16::from_le_bytes(header[54..56].try_into()?)) == stride
            && (1..=256).contains(&count)
            && start >= 64
            && start.checked_add(count * stride).is_some_and(|end| end <= len),
        "invalid ELF program headers"
    );
    let mut has_interpreter = false;
    for index in 0..count {
        let program = read_at::<56>(file, start + index * stride)?;
        if program[..4] != [3, 0, 0, 0] {
            continue;
        } // PT_INTERP
        ensure!(!has_interpreter, "duplicate ELF interpreter");
        has_interpreter = true;
        let position = u64::from_le_bytes(program[8..16].try_into()?);
        let size = u64::from_le_bytes(program[32..40].try_into()?);
        let mut name = [0; 4096];
        ensure!(
            (1..=name.len() as u64).contains(&size),
            "invalid ELF interpreter length"
        );
        let value = &mut name[..size as usize];
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(value)?;
        let loader: &[u8] = if arm64 {
            b"ld-linux-aarch64.so.1"
        } else {
            b"ld-linux-x86-64.so.2"
        };
        let interpreter = value.strip_suffix(b"\0").unwrap_or_default();
        ensure!(
            interpreter.first() == Some(&b'/')
                && !interpreter.contains(&0)
                && interpreter.rsplit(|b| *b == b'/').next() == Some(loader),
            "ELF interpreter does not match GNU target"
        );
    }
    Ok(())
}

fn verify_pe(file: &mut File, arm64: bool) -> Result<()> {
    ensure!(read_at::<2>(file, 0)? == *b"MZ", "expected a Windows PE executable");
    let offset = u64::from(u32::from_le_bytes(read_at::<4>(file, 60)?));
    ensure!(offset >= 64, "invalid PE header offset");
    let header = read_at::<26>(file, offset)?;
    ensure!(header[..4] == *b"PE\0\0", "invalid PE header");
    ensure!(
        u16::from_le_bytes(header[4..6].try_into()?) == if arm64 { 0xaa64 } else { 0x8664 },
        "PE CPU does not match declared target"
    );
    let flags = u16::from_le_bytes(header[22..24].try_into()?);
    let optional = u64::from(u16::from_le_bytes(header[20..22].try_into()?));
    let sections = u64::from(u16::from_le_bytes(header[6..8].try_into()?));
    ensure!(
        header[24..26] == [0x0b, 0x02]
            && flags & 0x2002 == 2
            && optional >= 112
            && sections > 0
            && offset + 24 + optional + sections * 40 <= file.metadata()?.len(),
        "PE file is not a 64-bit executable"
    );
    Ok(())
}
