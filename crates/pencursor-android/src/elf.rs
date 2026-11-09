//! ELF64 `.dynsym` and `.dynstr` dynamic symbol table inspector.
//!
//! Scans 64-bit ELF shared libraries (such as `/system/lib64/libgui.so`) at runtime to locate
//! Itanium C++ mangled symbol names across Android 12–17 and OEM vendor builds.

use std::fs;

const SHT_DYNSYM: u32 = 11;

/// Scans the `.dynsym` section of the 64-bit ELF binary at `elf_path` and returns all exported
/// symbol names matching `predicate`.
pub fn find_elf64_dynsym_names<F>(elf_path: &str, mut predicate: F) -> Vec<String>
where
    F: FnMut(&str) -> bool,
{
    let mut matches = Vec::new();
    let Ok(bytes) = fs::read(elf_path) else {
        return matches;
    };
    if bytes.len() < 64 || &bytes[0..4] != b"\x7FELF" || bytes[4] != 2 {
        return matches;
    }

    let e_shoff = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
    let e_shentsize = u16::from_le_bytes(bytes[58..60].try_into().unwrap()) as usize;
    let e_shnum = u16::from_le_bytes(bytes[60..62].try_into().unwrap()) as usize;

    if e_shoff == 0
        || e_shentsize < 64
        || e_shoff.saturating_add(e_shentsize * e_shnum) > bytes.len()
    {
        return matches;
    }

    for i in 0..e_shnum {
        let sh = e_shoff + i * e_shentsize;
        let sh_type = u32::from_le_bytes(bytes[sh + 4..sh + 8].try_into().unwrap());
        if sh_type != SHT_DYNSYM {
            continue;
        }

        let sym_off = u64::from_le_bytes(bytes[sh + 24..sh + 32].try_into().unwrap()) as usize;
        let sym_sz = u64::from_le_bytes(bytes[sh + 32..sh + 40].try_into().unwrap()) as usize;
        let str_idx = u32::from_le_bytes(bytes[sh + 40..sh + 44].try_into().unwrap()) as usize;
        let entsize = u64::from_le_bytes(bytes[sh + 56..sh + 64].try_into().unwrap()) as usize;

        if str_idx >= e_shnum || entsize < 24 || sym_off.saturating_add(sym_sz) > bytes.len() {
            continue;
        }

        let str_sh = e_shoff + str_idx * e_shentsize;
        let str_off =
            u64::from_le_bytes(bytes[str_sh + 24..str_sh + 32].try_into().unwrap()) as usize;
        let str_sz =
            u64::from_le_bytes(bytes[str_sh + 32..str_sh + 40].try_into().unwrap()) as usize;
        if str_off.saturating_add(str_sz) > bytes.len() {
            continue;
        }
        let dynstr = &bytes[str_off..str_off + str_sz];

        let count = sym_sz / entsize;
        for s_idx in 0..count {
            let sym = sym_off + s_idx * entsize;
            let st_name = u32::from_le_bytes(bytes[sym..sym + 4].try_into().unwrap()) as usize;
            let st_shndx = u16::from_le_bytes(bytes[sym + 6..sym + 8].try_into().unwrap());
            if st_shndx == 0 || st_name >= dynstr.len() {
                continue;
            }
            let tail = &dynstr[st_name..];
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            if let Ok(name) = core::str::from_utf8(&tail[..end])
                && predicate(name)
            {
                matches.push(name.to_string());
            }
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_sniffer_missing_file() {
        let names = find_elf64_dynsym_names("/nonexistent/libgui.so", |_| true);
        assert!(names.is_empty());
    }
}
