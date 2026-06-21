// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::fmt;
use std::io::Write;

use tracing::info;

use crate::crypto::RustCryptoSha256;
use crate::decoder_platform::DecoderPlatform;
use crate::sonde_platform::SondePlatform;
use prevail::crab::ebpf_domain::DomainContext;
use prevail::crab::var_registry::VariableRegistry;
use prevail::elf_loader::ElfObject;
use prevail::fwd_analyzer;
use prevail::ir::program::Program as PrevailProgram;
use prevail::ir::unmarshal;
use prevail::printing;
use prevail::spec::config::EbpfVerifierOptions;
use prevail::spec::type_descriptors::EbpfMapDescriptor;
use sonde_protocol::{MapDef, ProgramImage, Sha256Provider};

/// Program verification profile.
#[derive(Debug, Clone, PartialEq)]
pub enum VerificationProfile {
    /// Resident programs are stored persistently on the node.
    Resident,
    /// Ephemeral programs are run once and discarded.
    Ephemeral,
}

/// A stored program record: the CBOR-encoded image plus metadata.
#[derive(Debug, Clone)]
pub struct ProgramRecord {
    /// SHA-256 of the CBOR-encoded node program image.
    pub hash: Vec<u8>,
    /// CBOR-encoded node program image (bytecode + map definitions).
    pub image: Vec<u8>,
    /// Byte length of the CBOR node image.
    pub size: u32,
    /// Verification profile used at ingestion time.
    pub verification_profile: VerificationProfile,
    /// ABI version this program was compiled for (`None` = any ABI).
    pub abi_version: Option<u32>,
    /// Original filename passed at ingestion time (operator-supplied metadata).
    pub source_filename: Option<String>,
    /// CBOR-encoded decoder `ProgramImage` extracted from `SEC("decoder")` (GW-1902).
    ///
    /// `None` for ELFs without a decoder section. Never sent to nodes — used
    /// only by the gateway for APP_DATA enrichment (GW-1903).
    pub decoder_image: Option<Vec<u8>>,
}

/// Errors from program library operations.
#[derive(Debug, Clone)]
pub enum ProgramError {
    /// Image is empty or invalid.
    InvalidImage,
    /// Image exceeds the size limit for its profile.
    ImageTooLarge { size: u32, limit: u32 },
    /// Program not found by hash.
    NotFound,
    /// ELF parsing failed.
    ElfParseError(String),
    /// Prevail verification failed.
    VerificationFailed(String),
    /// Generic error.
    Internal(String),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::InvalidImage => write!(f, "image is empty or invalid"),
            ProgramError::ImageTooLarge { size, limit } => {
                write!(f, "image size {} exceeds limit {}", size, limit)
            }
            ProgramError::NotFound => write!(f, "program not found"),
            ProgramError::ElfParseError(msg) => write!(f, "ELF parse error: {}", msg),
            ProgramError::VerificationFailed(msg) => {
                write!(f, "verification failed: {}", msg)
            }
            ProgramError::Internal(msg) => write!(f, "program error: {}", msg),
        }
    }
}

impl std::error::Error for ProgramError {}

/// Maximum CBOR image sizes per profile (GW-0403).
const MAX_RESIDENT_SIZE: u32 = 4096;
/// Maximum CBOR image size for ephemeral programs (GW-0202 AC3).
pub(crate) const MAX_EPHEMERAL_SIZE: u32 = 2048;

/// Section names that BPF loaders treat as map-backed.
///
/// Includes explicit map definition sections (`.maps`/`maps`) and global
/// data sections (`.rodata`, `.data`, `.bss`) which libbpf/prevail promote
/// to array maps.
const MAP_SECTION_NAMES: &[&str] = &[".maps", "maps", ".rodata", ".data", ".bss"];

/// Section name prefixes that indicate map-backed sections.
///
/// Covers explicit map sections (`maps/foo`) and global variable section
/// variants that Prevail promotes to maps (`.rodata.str1.1`, `.data.rel.ro`,
/// etc.).
const MAP_SECTION_PREFIXES: &[&str] = &[".maps/", "maps/", ".rodata.", ".data.", ".bss."];

/// Section names corresponding to global variable maps.
///
/// Prevail promotes these to array maps (one entry, `value_size` = section
/// size). Unlike `.maps`/`maps`, they carry initial data — the ELF section
/// content — that must be serialized into the program image.
const GLOBAL_DATA_SECTION_NAMES: &[&str] = &[".rodata", ".data", ".bss"];

/// ELF section type for sections with data (SHT_PROGBITS).
const SHT_PROGBITS: u32 = 1;
/// ELF section type for symbol tables (SHT_SYMTAB).
const SHT_SYMTAB: u32 = 2;
/// ELF section type for RELA relocation tables.
const SHT_RELA: u32 = 4;
/// ELF section type for REL relocation tables.
const SHT_REL: u32 = 9;
/// eBPF `LD_DW_IMM` opcode.
const BPF_LD_DW_IMM: u8 = 0x18;

/// Lightweight check for ELF64 LE sections that produce BPF maps.
///
/// Scans section headers and the section-name string table without invoking
/// the full prevail loader, so it is safe to call on any platform.
/// Returns `false` (rather than panicking) on any malformed input.
fn elf_has_map_sections(data: &[u8]) -> bool {
    // ELF64 header is 64 bytes; bail out on anything too short.
    if data.len() < 64 {
        return false;
    }

    // Validate ELF magic, 64-bit class, and little-endian encoding.
    if data[0..4] != [0x7f, b'E', b'L', b'F'] || data[4] != 2 || data[5] != 1 {
        return false;
    }

    // Only inspect BPF object files — non-BPF ELFs may have `.data`/`.bss`
    // sections that are unrelated to BPF maps.
    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    if e_machine != 0x00F7 {
        return false;
    }

    let read_u16 = |off: usize| u16::from_le_bytes([data[off], data[off + 1]]);
    let read_u64 = |off: usize| {
        u64::from_le_bytes([
            data[off],
            data[off + 1],
            data[off + 2],
            data[off + 3],
            data[off + 4],
            data[off + 5],
            data[off + 6],
            data[off + 7],
        ])
    };

    let sh_off = read_u64(40) as usize; // e_shoff
    let sh_entsize = read_u16(58) as usize; // e_shentsize
    let sh_num = read_u16(60) as usize; // e_shnum
    let sh_strndx = read_u16(62) as usize; // e_shstrndx

    if sh_strndx >= sh_num || sh_entsize < 64 {
        return false;
    }

    // Locate the section-name string table (.shstrtab).
    let str_sh = match sh_strndx
        .checked_mul(sh_entsize)
        .and_then(|offset| sh_off.checked_add(offset))
    {
        Some(v) => v,
        None => return false,
    };
    if str_sh > data.len().saturating_sub(40) {
        return false;
    }
    let strtab_off = read_u64(str_sh + 24) as usize;
    let strtab_size = read_u64(str_sh + 32) as usize;
    let strtab_end = match strtab_off.checked_add(strtab_size) {
        Some(end) => end,
        None => return false,
    };
    if strtab_end > data.len() {
        return false;
    }
    let strtab = &data[strtab_off..strtab_end];

    // Scan each section header looking for a map-backed section name.
    for i in 0..sh_num {
        let hdr = match i
            .checked_mul(sh_entsize)
            .and_then(|offset| sh_off.checked_add(offset))
        {
            Some(v) => v,
            None => return false,
        };
        if hdr > data.len().saturating_sub(4) {
            break;
        }
        let name_off =
            u32::from_le_bytes([data[hdr], data[hdr + 1], data[hdr + 2], data[hdr + 3]]) as usize;
        if name_off >= strtab.len() {
            continue;
        }
        // Extract the NUL-terminated section name.
        let name_end = strtab[name_off..]
            .iter()
            .position(|&b| b == 0)
            .map_or(strtab.len(), |p| name_off + p);
        if let Ok(name) = std::str::from_utf8(&strtab[name_off..name_end]) {
            if MAP_SECTION_NAMES.contains(&name)
                || MAP_SECTION_PREFIXES.iter().any(|&p| name.starts_with(p))
            {
                return true;
            }
        }
    }

    false
}

/// Check whether a BPF ELF contains a section with the given exact name.
///
/// Lightweight scan of section headers — does not invoke the full prevail
/// loader. Returns `false` on any malformed input.
fn elf_has_section(data: &[u8], target: &str) -> bool {
    if data.len() < 64 {
        return false;
    }
    if data[0..4] != [0x7f, b'E', b'L', b'F'] || data[4] != 2 || data[5] != 1 {
        return false;
    }
    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    if e_machine != 0x00F7 {
        return false;
    }

    let read_u16 = |off: usize| u16::from_le_bytes([data[off], data[off + 1]]);
    let read_u64 = |off: usize| {
        u64::from_le_bytes([
            data[off],
            data[off + 1],
            data[off + 2],
            data[off + 3],
            data[off + 4],
            data[off + 5],
            data[off + 6],
            data[off + 7],
        ])
    };

    let sh_off = read_u64(40) as usize;
    let sh_entsize = read_u16(58) as usize;
    let sh_num = read_u16(60) as usize;
    let sh_strndx = read_u16(62) as usize;

    if sh_strndx >= sh_num || sh_entsize < 64 {
        return false;
    }
    let str_sh = match sh_strndx
        .checked_mul(sh_entsize)
        .and_then(|offset| sh_off.checked_add(offset))
    {
        Some(v) => v,
        None => return false,
    };
    if str_sh > data.len().saturating_sub(40) {
        return false;
    }
    let strtab_off = read_u64(str_sh + 24) as usize;
    let strtab_size = read_u64(str_sh + 32) as usize;
    let strtab_end = match strtab_off.checked_add(strtab_size) {
        Some(end) => end,
        None => return false,
    };
    if strtab_end > data.len() {
        return false;
    }
    let strtab = &data[strtab_off..strtab_end];

    for i in 0..sh_num {
        let hdr = match i
            .checked_mul(sh_entsize)
            .and_then(|offset| sh_off.checked_add(offset))
        {
            Some(v) => v,
            None => return false,
        };
        if hdr > data.len().saturating_sub(4) {
            break;
        }
        let name_off =
            u32::from_le_bytes([data[hdr], data[hdr + 1], data[hdr + 2], data[hdr + 3]]) as usize;
        if name_off >= strtab.len() {
            continue;
        }
        let name_end = strtab[name_off..]
            .iter()
            .position(|&b| b == 0)
            .map_or(strtab.len(), |p| name_off + p);
        if let Ok(name) = std::str::from_utf8(&strtab[name_off..name_end]) {
            if name == target {
                // Also check that the section has non-zero size (GW-1900 AC-8:
                // empty decoder section is treated as no decoder).
                if hdr + 40 <= data.len() {
                    let sec_size = u64::from_le_bytes([
                        data[hdr + 32],
                        data[hdr + 33],
                        data[hdr + 34],
                        data[hdr + 35],
                        data[hdr + 36],
                        data[hdr + 37],
                        data[hdr + 38],
                        data[hdr + 39],
                    ]);
                    if sec_size > 0 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Extract initial data for global variable sections from a BPF ELF.
///
/// Returns section content for `.rodata`, `.data`, and `.bss` sections in
/// the same order they appear in the ELF section header table. Prevail's
/// `add_global_variable_maps()` iterates sections in this same order, so
/// the returned entries correspond 1:1 to the `map_type == 0` descriptors
/// in `RawProgram::info::map_descriptors`.
///
/// `.bss` sections (SHT_NOBITS) have no file data — an empty Vec is
/// returned for them since map storage is already zero-initialized.
///
/// Returns an empty Vec if the ELF is malformed.
fn extract_global_section_data(data: &[u8]) -> Vec<(Vec<u8>, bool)> {
    if data.len() < 64 {
        return Vec::new();
    }
    if data[0..4] != [0x7f, b'E', b'L', b'F'] || data[4] != 2 || data[5] != 1 {
        return Vec::new();
    }
    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    if e_machine != 0x00F7 {
        return Vec::new();
    }

    let read_u16 = |off: usize| u16::from_le_bytes([data[off], data[off + 1]]);
    let read_u32 =
        |off: usize| u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let read_u64 = |off: usize| {
        u64::from_le_bytes([
            data[off],
            data[off + 1],
            data[off + 2],
            data[off + 3],
            data[off + 4],
            data[off + 5],
            data[off + 6],
            data[off + 7],
        ])
    };

    let sh_off = read_u64(40) as usize;
    let sh_entsize = read_u16(58) as usize;
    let sh_num = read_u16(60) as usize;
    let sh_strndx = read_u16(62) as usize;

    if sh_strndx >= sh_num || sh_entsize < 64 {
        return Vec::new();
    }

    let str_sh = match sh_strndx
        .checked_mul(sh_entsize)
        .and_then(|offset| sh_off.checked_add(offset))
    {
        Some(v) => v,
        None => return Vec::new(),
    };
    if str_sh > data.len().saturating_sub(40) {
        return Vec::new();
    }
    let strtab_off = read_u64(str_sh + 24) as usize;
    let strtab_size = read_u64(str_sh + 32) as usize;
    let strtab_end = match strtab_off.checked_add(strtab_size) {
        Some(end) => end,
        None => return Vec::new(),
    };
    if strtab_end > data.len() {
        return Vec::new();
    }
    let strtab = &data[strtab_off..strtab_end];

    let mut sections = Vec::new();
    for i in 0..sh_num {
        let hdr = match i
            .checked_mul(sh_entsize)
            .and_then(|offset| sh_off.checked_add(offset))
        {
            Some(v) => v,
            None => return Vec::new(),
        };
        if hdr + sh_entsize > data.len() {
            break;
        }
        let name_off = read_u32(hdr) as usize;
        if name_off >= strtab.len() {
            continue;
        }
        let name_end = strtab[name_off..]
            .iter()
            .position(|&b| b == 0)
            .map_or(strtab.len(), |p| name_off + p);
        let name = match std::str::from_utf8(&strtab[name_off..name_end]) {
            Ok(n) => n,
            Err(_) => continue,
        };
        // Match global data sections by prefix (Prevail promotes .rodata.str1.1,
        // .data.rel.ro, etc. — not just exact .rodata/.data/.bss).
        let is_global = GLOBAL_DATA_SECTION_NAMES.iter().any(|prefix| {
            name == *prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        });
        if !is_global {
            continue;
        }
        let sh_type = read_u32(hdr + 4);
        let sec_off = read_u64(hdr + 24) as usize;
        let sec_size = read_u64(hdr + 32) as usize;

        let is_readonly = name == ".rodata"
            || name
                .strip_prefix(".rodata")
                .is_some_and(|rest| rest.starts_with('.'));

        if sh_type == SHT_PROGBITS {
            // .rodata / .data — extract file content.
            let sec_end = match sec_off.checked_add(sec_size) {
                Some(end) if end <= data.len() => end,
                _ => continue,
            };
            sections.push((data[sec_off..sec_end].to_vec(), is_readonly));
        } else {
            // .bss (SHT_NOBITS) — no file data; maps are zero-initialized.
            sections.push((Vec::new(), false));
        }
    }
    sections
}

/// Rewrite ELF global-data relocations into `LD_DW_IMM src=6` map-value loads.
///
/// The CBOR `ProgramImage` stores only raw bytecode, not ELF relocation
/// metadata. Any `.rodata`, `.data`, or `.bss` reference therefore has to be
/// converted from an ELF relocation into the runtime form expected by the node
/// and decoder interpreters: `LD_DW_IMM` with `src=6` and `imm=<map index>`.
fn rewrite_global_data_relocations(
    elf_bytes: &[u8],
    target_section_name: &str,
    bytecode: &mut [u8],
    map_descriptors: &[EbpfMapDescriptor],
) -> Result<(), ProgramError> {
    if elf_bytes.len() < 64 {
        return Err(ProgramError::ElfParseError("ELF too short".into()));
    }
    if elf_bytes[0..4] != [0x7f, b'E', b'L', b'F'] || elf_bytes[4] != 2 || elf_bytes[5] != 1 {
        return Err(ProgramError::ElfParseError("invalid ELF header".into()));
    }
    let e_machine = u16::from_le_bytes([elf_bytes[18], elf_bytes[19]]);
    if e_machine != 0x00F7 {
        return Err(ProgramError::ElfParseError("ELF is not EM_BPF".into()));
    }

    let read_u16 = |off: usize| u16::from_le_bytes([elf_bytes[off], elf_bytes[off + 1]]);
    let read_u32 = |off: usize| {
        u32::from_le_bytes([
            elf_bytes[off],
            elf_bytes[off + 1],
            elf_bytes[off + 2],
            elf_bytes[off + 3],
        ])
    };
    let read_u64 = |off: usize| {
        u64::from_le_bytes([
            elf_bytes[off],
            elf_bytes[off + 1],
            elf_bytes[off + 2],
            elf_bytes[off + 3],
            elf_bytes[off + 4],
            elf_bytes[off + 5],
            elf_bytes[off + 6],
            elf_bytes[off + 7],
        ])
    };

    let sh_off = read_u64(40) as usize;
    let sh_entsize = read_u16(58) as usize;
    let sh_num = read_u16(60) as usize;
    let sh_strndx = read_u16(62) as usize;
    if sh_strndx >= sh_num || sh_entsize < 64 {
        return Err(ProgramError::ElfParseError(
            "invalid section header table".into(),
        ));
    }

    let str_sh = sh_off
        .checked_add(
            sh_strndx
                .checked_mul(sh_entsize)
                .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?,
        )
        .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?;
    if str_sh + 40 > elf_bytes.len() {
        return Err(ProgramError::ElfParseError(
            "section name table header out of bounds".into(),
        ));
    }
    let strtab_off = read_u64(str_sh + 24) as usize;
    let strtab_size = read_u64(str_sh + 32) as usize;
    let strtab_end = strtab_off
        .checked_add(strtab_size)
        .ok_or_else(|| ProgramError::ElfParseError("section name table overflow".into()))?;
    if strtab_end > elf_bytes.len() {
        return Err(ProgramError::ElfParseError(
            "section name table out of bounds".into(),
        ));
    }
    let shstrtab = &elf_bytes[strtab_off..strtab_end];

    let section_name = |hdr: usize| -> Result<&str, ProgramError> {
        let name_off = read_u32(hdr) as usize;
        if name_off >= shstrtab.len() {
            return Err(ProgramError::ElfParseError(
                "section name offset out of bounds".into(),
            ));
        }
        let name_end = shstrtab[name_off..]
            .iter()
            .position(|&b| b == 0)
            .map_or(shstrtab.len(), |p| name_off + p);
        std::str::from_utf8(&shstrtab[name_off..name_end])
            .map_err(|_| ProgramError::ElfParseError("section name is not valid UTF-8".into()))
    };

    let global_map_indices: Vec<u32> = map_descriptors
        .iter()
        .enumerate()
        .filter_map(|(i, md)| (md.map_type == 0).then_some(i as u32))
        .collect();
    let mut next_global_map = 0usize;
    let mut target_section_index = None;
    let mut section_to_global_map: HashMap<u16, u32> = HashMap::new();

    for i in 0..sh_num {
        let hdr = sh_off
            .checked_add(
                i.checked_mul(sh_entsize)
                    .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?,
            )
            .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?;
        if hdr + sh_entsize > elf_bytes.len() {
            return Err(ProgramError::ElfParseError(
                "section header out of bounds".into(),
            ));
        }
        let name = section_name(hdr)?;
        if name == target_section_name {
            target_section_index = Some(i);
        }
        let is_global = GLOBAL_DATA_SECTION_NAMES.iter().any(|prefix| {
            name == *prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        });
        if is_global {
            let Some(&map_index) = global_map_indices.get(next_global_map) else {
                return Err(ProgramError::ElfParseError(format!(
                    "missing map descriptor for global section `{name}`"
                )));
            };
            section_to_global_map.insert(i as u16, map_index);
            next_global_map += 1;
        }
    }

    if next_global_map != global_map_indices.len() {
        return Err(ProgramError::ElfParseError(format!(
            "global section count mismatch while rewriting relocations: ELF has {} sections but image has {} global maps",
            next_global_map,
            global_map_indices.len()
        )));
    }

    let Some(target_section_index) = target_section_index else {
        return Ok(());
    };

    for i in 0..sh_num {
        let hdr = sh_off
            .checked_add(
                i.checked_mul(sh_entsize)
                    .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?,
            )
            .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?;
        if hdr + sh_entsize > elf_bytes.len() {
            return Err(ProgramError::ElfParseError(
                "section header out of bounds".into(),
            ));
        }

        let sh_type = read_u32(hdr + 4);
        if sh_type != SHT_REL && sh_type != SHT_RELA {
            continue;
        }
        let rel_target_index = read_u32(hdr + 44) as usize;
        if rel_target_index != target_section_index {
            continue;
        }

        let rel_off = read_u64(hdr + 24) as usize;
        let rel_size = read_u64(hdr + 32) as usize;
        let rel_entsize = read_u64(hdr + 56) as usize;
        let rel_end = rel_off
            .checked_add(rel_size)
            .ok_or_else(|| ProgramError::ElfParseError("relocation table overflow".into()))?;
        if rel_end > elf_bytes.len() {
            return Err(ProgramError::ElfParseError(
                "relocation table out of bounds".into(),
            ));
        }
        let entry_size = match sh_type {
            SHT_REL => 16,
            SHT_RELA => 24,
            _ => unreachable!(),
        };
        let stride = if rel_entsize == 0 {
            entry_size
        } else if rel_entsize >= entry_size {
            rel_entsize
        } else {
            return Err(ProgramError::ElfParseError(
                "relocation entry size is too small".into(),
            ));
        };

        let symtab_index = read_u32(hdr + 40) as usize;
        if symtab_index >= sh_num {
            return Err(ProgramError::ElfParseError(
                "relocation section has invalid symtab link".into(),
            ));
        }
        let sym_hdr = sh_off
            .checked_add(
                symtab_index
                    .checked_mul(sh_entsize)
                    .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?,
            )
            .ok_or_else(|| ProgramError::ElfParseError("section offset overflow".into()))?;
        if sym_hdr + sh_entsize > elf_bytes.len() {
            return Err(ProgramError::ElfParseError(
                "symbol table header out of bounds".into(),
            ));
        }
        if read_u32(sym_hdr + 4) != SHT_SYMTAB {
            return Err(ProgramError::ElfParseError(
                "relocation section does not link to a symbol table".into(),
            ));
        }
        let sym_off = read_u64(sym_hdr + 24) as usize;
        let sym_size = read_u64(sym_hdr + 32) as usize;
        let sym_entsize = read_u64(sym_hdr + 56) as usize;
        if sym_entsize < 24 {
            return Err(ProgramError::ElfParseError(
                "symbol table entry size is too small".into(),
            ));
        }
        let sym_end = sym_off
            .checked_add(sym_size)
            .ok_or_else(|| ProgramError::ElfParseError("symbol table overflow".into()))?;
        if sym_end > elf_bytes.len() {
            return Err(ProgramError::ElfParseError(
                "symbol table out of bounds".into(),
            ));
        }

        let rel_data = &elf_bytes[rel_off..rel_end];
        for entry_off in (0..rel_data.len()).step_by(stride) {
            if entry_off + entry_size > rel_data.len() {
                break;
            }
            let r_offset = u64::from_le_bytes([
                rel_data[entry_off],
                rel_data[entry_off + 1],
                rel_data[entry_off + 2],
                rel_data[entry_off + 3],
                rel_data[entry_off + 4],
                rel_data[entry_off + 5],
                rel_data[entry_off + 6],
                rel_data[entry_off + 7],
            ]) as usize;
            let r_info = u64::from_le_bytes([
                rel_data[entry_off + 8],
                rel_data[entry_off + 9],
                rel_data[entry_off + 10],
                rel_data[entry_off + 11],
                rel_data[entry_off + 12],
                rel_data[entry_off + 13],
                rel_data[entry_off + 14],
                rel_data[entry_off + 15],
            ]);
            let sym_index = (r_info >> 32) as usize;
            let sym_entry =
                sym_off
                    .checked_add(sym_index.checked_mul(sym_entsize).ok_or_else(|| {
                        ProgramError::ElfParseError("symbol offset overflow".into())
                    })?)
                    .ok_or_else(|| ProgramError::ElfParseError("symbol offset overflow".into()))?;
            if sym_entry + 8 > sym_end {
                return Err(ProgramError::ElfParseError(
                    "relocation symbol out of bounds".into(),
                ));
            }
            let st_shndx = u16::from_le_bytes([elf_bytes[sym_entry + 6], elf_bytes[sym_entry + 7]]);
            let Some(&map_index) = section_to_global_map.get(&st_shndx) else {
                continue;
            };

            if r_offset + 16 > bytecode.len() {
                return Err(ProgramError::ElfParseError(format!(
                    "relocation at byte offset {} exceeds section `{}` size {}",
                    r_offset,
                    target_section_name,
                    bytecode.len()
                )));
            }
            if bytecode[r_offset] != BPF_LD_DW_IMM {
                return Err(ProgramError::ElfParseError(format!(
                    "relocation at byte offset {} in section `{}` does not target LD_DW_IMM",
                    r_offset, target_section_name
                )));
            }

            bytecode[r_offset + 1] = (bytecode[r_offset + 1] & 0x0f) | 0x60;
            bytecode[r_offset + 4..r_offset + 8].copy_from_slice(&map_index.to_le_bytes());
        }
    }

    Ok(())
}

/// Program library: stores verified programs and serves chunks.
#[derive(Clone)]
pub struct ProgramLibrary {
    sha256: RustCryptoSha256,
}

impl ProgramLibrary {
    pub fn new() -> Self {
        Self {
            sha256: RustCryptoSha256,
        }
    }

    /// Ingest a CBOR-encoded program image **without** BPF verification.
    ///
    /// **Warning:** This bypasses Prevail verification. Use `ingest_elf()`
    /// for production ingestion of BPF programs. This method exists for
    /// testing and for internal use by `ingest_elf()` after verification.
    pub fn ingest_unverified(
        &self,
        image: Vec<u8>,
        profile: VerificationProfile,
    ) -> Result<ProgramRecord, ProgramError> {
        if image.is_empty() {
            return Err(ProgramError::InvalidImage);
        }

        let size = image.len() as u32;
        let limit = match profile {
            VerificationProfile::Resident => MAX_RESIDENT_SIZE,
            VerificationProfile::Ephemeral => MAX_EPHEMERAL_SIZE,
        };
        if size > limit {
            return Err(ProgramError::ImageTooLarge { size, limit });
        }

        let hash = self.sha256.hash(&image).to_vec();

        Ok(ProgramRecord {
            hash,
            image,
            size,
            verification_profile: profile,
            abi_version: None,
            source_filename: None,
            decoder_image: None,
        })
    }

    /// Ingest a raw ELF binary with prevail verification (GW-0401).
    ///
    /// Steps:
    ///   1. Reject ephemeral programs that declare map-backed sections (GW-0401 criterion 5).
    ///   2. Write ELF bytes to a temp file for prevail.
    ///   3. Parse the ELF with `ElfObject`.
    ///   4. Extract programs and run prevail verification on each extracted program.
    ///   5. Reject ELF files containing multiple programs (ambiguous selection).
    ///   6. Serialize bytecode and map definitions into a `ProgramImage`.
    ///   7. Delegate size-limit enforcement and hashing to `ingest()`.
    pub fn ingest_elf(
        &self,
        elf_bytes: &[u8],
        profile: VerificationProfile,
    ) -> Result<ProgramRecord, ProgramError> {
        if elf_bytes.is_empty() {
            return Err(ProgramError::InvalidImage);
        }

        // Ephemeral programs are stateless — reject early if the ELF declares
        // map sections (GW-0401 criterion 5).
        if profile == VerificationProfile::Ephemeral && elf_has_map_sections(elf_bytes) {
            return Err(ProgramError::VerificationFailed(
                "ephemeral programs must not declare maps".into(),
            ));
        }

        // Write ELF bytes to a temp file for prevail's file-based parser.
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProgramError::Internal(format!("failed to create temp file: {e}")))?;
        tmp.write_all(elf_bytes)
            .map_err(|e| ProgramError::Internal(format!("failed to write temp file: {e}")))?;
        let tmp_path = tmp.path().to_str().ok_or_else(|| {
            ProgramError::Internal("temporary file path is not valid UTF-8".into())
        })?;

        // Configure prevail verifier options based on profile.
        let mut opts = EbpfVerifierOptions::default();
        match profile {
            VerificationProfile::Resident => {
                opts.cfg_opts.check_for_termination = false;
            }
            VerificationProfile::Ephemeral => {
                opts.cfg_opts.check_for_termination = true;
            }
        }

        // Parse the ELF.
        let mut elf = ElfObject::new(tmp_path, opts)
            .map_err(|e| ProgramError::ElfParseError(e.to_string()))?;

        // Extract programs from the `sonde` ELF section only (GW-0401 criterion 6).
        // The section filter ensures helper functions in other sections (e.g. `.text`)
        // are ignored, matching the `SEC("sonde")` annotation used by all sonde BPF programs.
        let mut platform = SondePlatform::new();
        let raw_programs = elf
            .get_programs("sonde", "", &mut platform)
            .map_err(|e| ProgramError::ElfParseError(e.to_string()))?;

        if raw_programs.is_empty() {
            return Err(ProgramError::ElfParseError(
                "no programs found in ELF; ensure the BPF program uses \
                 SEC(\"sonde\") section annotation"
                    .into(),
            ));
        }
        if raw_programs.len() > 1 {
            return Err(ProgramError::ElfParseError(format!(
                "ELF contains {} programs; expected exactly one",
                raw_programs.len()
            )));
        }

        // Sync map descriptors from the ELF loader into the platform so
        // that get_map_descriptor() can find global variable maps (.rodata,
        // .data) which are not passed through parse_maps_section.
        platform.sync_map_descriptors(&raw_programs[0].info.map_descriptors);

        // Verify each program with prevail.
        for raw_prog in &raw_programs {
            let mut notes: Vec<Vec<String>> = Vec::new();
            let inst_seq =
                unmarshal::unmarshal(&raw_prog.prog, &mut notes, &raw_prog.info, &platform, &opts)
                    .map_err(|e| ProgramError::VerificationFailed(format!("unmarshal: {e}")))?;

            let program =
                PrevailProgram::from_sequence(&inst_seq, &raw_prog.info, &platform, &opts)
                    .map_err(|e| {
                        ProgramError::VerificationFailed(format!("invalid control flow: {e}"))
                    })?;

            let ctx = DomainContext {
                program_info: &raw_prog.info,
                options: &opts,
                platform: &platform,
            };
            let mut registry = VariableRegistry::new();
            let result = fwd_analyzer::analyze(&program, &ctx, &mut registry);

            if result.failed {
                // Build diagnostics matching the Prevail CLI output format
                // (GW-1305).
                //
                // Line 1: summary (always present).
                // Line 2: first error from `find_first_error()` — clients
                //         rely on this being the very first line after the
                //         summary for non-verbose display (GW-1305 criterion 3).
                // Lines 3+: unmarshal-stage notes, then full invariant state
                //           from `print_invariants()`.

                let mut diag = String::new();

                // First forward-analysis error via `find_first_error()`
                // (GW-1305 criterion 1). Placed first so clients can
                // reliably extract it as the line immediately after the
                // summary.
                //
                // The admin client relies on this line always being present.
                // If `find_first_error()` does not yield a usable string,
                // fall back to a placeholder to preserve the contract.
                let first_error_line = result
                    .find_first_error()
                    .and_then(|first_error| {
                        let mut buf = Vec::new();
                        let _ = printing::print_error(&mut buf, &first_error);
                        String::from_utf8(buf).ok().map(|s| s.trim_end().to_owned())
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        "verifier failed but no primary error was reported; see diagnostics below"
                            .to_owned()
                    });

                diag.push('\n');
                diag.push_str(&first_error_line);

                // Unmarshal-stage notes (instruction-level parsing
                // diagnostics). Placed after find_first_error() so they
                // don't displace the primary verifier error.
                let unmarshal_notes: Vec<String> = notes.into_iter().flatten().collect();
                for note in &unmarshal_notes {
                    diag.push('\n');
                    diag.push_str(note);
                }

                // Full invariant state (equivalent to Prevail's `-v` flag,
                // GW-1305 criterion 2).
                //
                // gRPC status messages are carried in HTTP/2 trailers whose
                // total size is typically capped at 8–16 KiB.  To avoid
                // turning a verification failure into a transport-level error
                // we cap the diagnostics string and indicate truncation.
                {
                    let mut buf = Vec::new();
                    let _ = printing::print_invariants(
                        &mut buf,
                        &program,
                        &raw_prog.info,
                        false,
                        &result,
                        &registry,
                    );
                    if let Ok(s) = String::from_utf8(buf) {
                        let s = s.trim_end();
                        if !s.is_empty() {
                            diag.push('\n');
                            diag.push_str(s);
                        }
                    }
                }

                // Cap total message length to stay within gRPC metadata
                // limits.  The summary + first-error line are always
                // preserved; only the verbose invariant tail is truncated.
                const MAX_DIAG_BYTES: usize = 7 * 1024; // 7 KiB — well within the 8-16 KiB gRPC trailer limit
                if diag.len() > MAX_DIAG_BYTES {
                    // Truncate on a char boundary.
                    let mut end = MAX_DIAG_BYTES;
                    while end > 0 && !diag.is_char_boundary(end) {
                        end -= 1;
                    }
                    diag.truncate(end);
                    diag.push_str("\n... [diagnostics truncated]");
                }

                return Err(ProgramError::VerificationFailed(format!(
                    "program `{}` failed verification; check that the BPF program \
                     compiles correctly for the sonde target{}",
                    raw_prog.function_name, diag
                )));
            }
        }

        // Build the ProgramImage from the first program's bytecode and maps.
        let first = &raw_programs[0];

        let mut bytecode = Vec::with_capacity(first.prog.len() * 8);
        for inst in &first.prog {
            bytecode.push(inst.opcode);
            bytecode.push(inst.dst_src);
            bytecode.extend_from_slice(&inst.offset.to_le_bytes());
            bytecode.extend_from_slice(&inst.imm.to_le_bytes());
        }

        let maps: Vec<MapDef> = first
            .info
            .map_descriptors
            .iter()
            .map(|md| MapDef {
                map_type: md.map_type,
                key_size: md.key_size,
                value_size: md.value_size,
                max_entries: md.max_entries,
            })
            .collect();

        // Extract initial data for global variable maps (.rodata, .data).
        // Prevail promotes these sections to map descriptors with map_type == 0
        // in section header order (GW-0405).  `extract_global_section_data`
        // returns data in the same order, so we can match them 1:1.
        let global_sections = extract_global_section_data(elf_bytes);
        let global_count = global_sections.len();
        let mut global_iter = global_sections.into_iter();
        let mut map_initial_data: Vec<Vec<u8>> = Vec::with_capacity(maps.len());
        let mut map_readonly: Vec<bool> = Vec::with_capacity(maps.len());
        for md in &first.info.map_descriptors {
            if md.map_type == 0 {
                let (data, readonly) = global_iter.next().unwrap_or_default();
                map_initial_data.push(data);
                map_readonly.push(readonly);
            } else {
                map_initial_data.push(Vec::new());
                map_readonly.push(false);
            }
        }

        // Verify 1:1 correspondence between ELF global sections and
        // Prevail map_type==0 descriptors (GW-0405 criterion 4).
        let type0_count = first
            .info
            .map_descriptors
            .iter()
            .filter(|md| md.map_type == 0)
            .count();
        if type0_count != global_count {
            return Err(ProgramError::ElfParseError(format!(
                "global section count mismatch: ELF has {} sections but Prevail reports {} map_type==0 descriptors",
                global_count, type0_count
            )));
        }

        rewrite_global_data_relocations(
            elf_bytes,
            "sonde",
            &mut bytecode,
            &first.info.map_descriptors,
        )?;

        let image = ProgramImage {
            bytecode,
            maps,
            map_initial_data,
            map_readonly,
        };
        let cbor = image
            .encode_deterministic()
            .map_err(|e| ProgramError::Internal(format!("CBOR encoding failed: {e}")))?;

        // Delegate size-limit and hashing logic to the canonical ingest path.
        let mut record = self.ingest_unverified(cbor, profile)?;

        // ── Decoder extraction (GW-1900, GW-1901, GW-1906) ─────────────
        //
        // Check for an optional `SEC("decoder")` section. The decoder image
        // is stored alongside the node image but does NOT affect the node
        // program hash.
        let decoder_image = self.extract_decoder(elf_bytes, tmp_path, &opts)?;
        if decoder_image.is_some() {
            info!("decoder section extracted and verified");
        }
        record.decoder_image = decoder_image;

        Ok(record)
    }

    /// Extract and verify an optional decoder section from a BPF ELF (GW-1900).
    ///
    /// Returns `Ok(Some(cbor_bytes))` if a valid decoder section is found,
    /// `Ok(None)` if no decoder section exists (or it is empty), and
    /// `Err(...)` if the decoder section is invalid (verification failure,
    /// multiple sections, etc.).
    fn extract_decoder(
        &self,
        elf_bytes: &[u8],
        tmp_path: &str,
        opts: &EbpfVerifierOptions,
    ) -> Result<Option<Vec<u8>>, ProgramError> {
        // Lightweight check: scan ELF section headers for a "decoder" section
        // before invoking Prevail. This avoids relying on error message strings.
        if !elf_has_section(elf_bytes, "decoder") {
            return Ok(None);
        }

        // Parse the ELF again with a DecoderPlatform to extract the decoder
        // section. We re-parse rather than reusing the ElfObject because
        // get_programs() mutates internal state (map descriptors).
        let mut elf = ElfObject::new(tmp_path, *opts)
            .map_err(|e| ProgramError::ElfParseError(e.to_string()))?;

        let mut decoder_platform = DecoderPlatform::new();
        let decoder_programs = elf
            .get_programs("decoder", "", &mut decoder_platform)
            .map_err(|e| {
                ProgramError::ElfParseError(format!("decoder section extraction failed: {e}"))
            })?;

        if decoder_programs.is_empty() {
            return Ok(None);
        }

        // GW-1900 AC-7: reject ELFs with multiple decoder sections.
        if decoder_programs.len() > 1 {
            return Err(ProgramError::ElfParseError(format!(
                "ELF contains {} decoder programs; expected at most one",
                decoder_programs.len()
            )));
        }

        let decoder_prog = &decoder_programs[0];

        // GW-1900 AC-8: empty decoder section (zero bytecode) → no decoder.
        if decoder_prog.prog.is_empty() {
            return Ok(None);
        }

        // Sync map descriptors for the decoder platform.
        decoder_platform.sync_map_descriptors(&decoder_prog.info.map_descriptors);

        // GW-1901: verify the decoder program with DecoderPlatform.
        let mut notes: Vec<Vec<String>> = Vec::new();
        let inst_seq = unmarshal::unmarshal(
            &decoder_prog.prog,
            &mut notes,
            &decoder_prog.info,
            &decoder_platform,
            opts,
        )
        .map_err(|e| ProgramError::VerificationFailed(format!("decoder unmarshal: {e}")))?;

        let program = PrevailProgram::from_sequence(
            &inst_seq,
            &decoder_prog.info,
            &decoder_platform,
            opts,
        )
        .map_err(|e| ProgramError::VerificationFailed(format!("decoder control flow: {e}")))?;

        let ctx = DomainContext {
            program_info: &decoder_prog.info,
            options: opts,
            platform: &decoder_platform,
        };
        let mut registry = VariableRegistry::new();
        let result = fwd_analyzer::analyze(&program, &ctx, &mut registry);

        if result.failed {
            let mut diag = String::new();
            if let Some(first_error) = result.find_first_error() {
                let mut buf = Vec::new();
                let _ = printing::print_error(&mut buf, &first_error);
                if let Ok(s) = String::from_utf8(buf) {
                    diag.push_str(s.trim_end());
                }
            }
            return Err(ProgramError::VerificationFailed(format!(
                "decoder program failed verification: {diag}"
            )));
        }

        // Build the decoder ProgramImage.
        let mut bytecode = Vec::with_capacity(decoder_prog.prog.len() * 8);
        for inst in &decoder_prog.prog {
            bytecode.push(inst.opcode);
            bytecode.push(inst.dst_src);
            bytecode.extend_from_slice(&inst.offset.to_le_bytes());
            bytecode.extend_from_slice(&inst.imm.to_le_bytes());
        }

        let maps: Vec<MapDef> = decoder_prog
            .info
            .map_descriptors
            .iter()
            .map(|md| MapDef {
                map_type: md.map_type,
                key_size: md.key_size,
                value_size: md.value_size,
                max_entries: md.max_entries,
            })
            .collect();

        // Extract initial data for decoder global variable maps.
        let global_sections = extract_global_section_data(elf_bytes);
        let global_count = global_sections.len();
        let mut global_iter = global_sections.into_iter();
        let mut map_initial_data: Vec<Vec<u8>> = Vec::with_capacity(maps.len());
        let mut map_readonly: Vec<bool> = Vec::with_capacity(maps.len());
        for md in &decoder_prog.info.map_descriptors {
            if md.map_type == 0 {
                let (data, readonly) = global_iter.next().unwrap_or_default();
                map_initial_data.push(data);
                map_readonly.push(readonly);
            } else {
                map_initial_data.push(Vec::new());
                map_readonly.push(false);
            }
        }

        // Validate 1:1 correspondence between ELF global sections and
        // map_type==0 descriptors (same check as the node image path).
        let type0_count = decoder_prog
            .info
            .map_descriptors
            .iter()
            .filter(|md| md.map_type == 0)
            .count();
        if type0_count != global_count {
            return Err(ProgramError::ElfParseError(format!(
                "decoder global section count mismatch: ELF has {global_count} sections \
                 but Prevail reports {type0_count} map_type==0 descriptors"
            )));
        }

        rewrite_global_data_relocations(
            elf_bytes,
            "decoder",
            &mut bytecode,
            &decoder_prog.info.map_descriptors,
        )?;

        let decoder_image = ProgramImage {
            bytecode,
            maps,
            map_initial_data,
            map_readonly,
        };

        // Enforce size limit: decoder image same as ephemeral (GW-1904).
        let cbor = decoder_image
            .encode_deterministic()
            .map_err(|e| ProgramError::Internal(format!("decoder CBOR encoding failed: {e}")))?;

        let decoder_size = cbor.len() as u32;
        if decoder_size > MAX_EPHEMERAL_SIZE {
            return Err(ProgramError::ImageTooLarge {
                size: decoder_size,
                limit: MAX_EPHEMERAL_SIZE,
            });
        }

        Ok(Some(cbor))
    }

    /// Look up a program by its hash in the given storage snapshot.
    pub fn get_by_hash<'a>(
        &self,
        records: &'a [ProgramRecord],
        hash: &[u8],
    ) -> Option<&'a ProgramRecord> {
        records.iter().find(|r| r.hash == hash)
    }

    /// Serve a chunk from a program image using `sonde_protocol::get_chunk()`.
    pub fn get_chunk<'a>(
        &self,
        image: &'a [u8],
        chunk_index: u32,
        chunk_size: u32,
    ) -> Option<&'a [u8]> {
        sonde_protocol::get_chunk(image, chunk_index, chunk_size)
    }

    /// Compute the chunk count for a given image size and chunk size.
    pub fn chunk_count(&self, image_size: usize, chunk_size: usize) -> Option<u32> {
        sonde_protocol::chunk_count(image_size, chunk_size)
    }
}

impl Default for ProgramLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_elf_empty_bytes_rejected() {
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&[], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::InvalidImage));
    }

    #[test]
    fn ingest_elf_invalid_bytes_rejected() {
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&[0xDE, 0xAD, 0xBE, 0xEF], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::ElfParseError(_)));
    }

    #[test]
    fn ingest_elf_truncated_elf_header_rejected() {
        let lib = ProgramLibrary::new();
        // Valid ELF magic but truncated
        let err = lib
            .ingest_elf(&[0x7f, b'E', b'L', b'F'], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::ElfParseError(_)));
    }

    #[test]
    fn ingest_elf_valid_minimal_program() {
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        let elf = make_sonde_elf(&bpf_code);
        let lib = ProgramLibrary::new();
        let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

        assert!(!record.hash.is_empty());
        assert!(record.size > 0);
        assert_eq!(record.verification_profile, VerificationProfile::Resident);

        let image = ProgramImage::decode(&record.image).unwrap();
        assert_eq!(image.bytecode.len(), 16);
        assert!(image.maps.is_empty());
    }

    #[test]
    fn ingest_elf_content_hash_is_deterministic() {
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        let elf = make_sonde_elf(&bpf_code);
        let lib = ProgramLibrary::new();
        let r1 = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
        let r2 = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
        assert_eq!(r1.hash, r2.hash);
    }

    /// Build a minimal BPF ELF that declares one `.maps` entry (an ARRAY map)
    /// while still containing a trivially valid program (`mov r0, 0; exit`).
    /// The program does *not* reference the map — the map is present only so
    /// that this ELF exercises the pre-Prevail section-scan rejection for
    /// ephemeral programs that declare maps.
    fn make_minimal_bpf_elf_with_maps() -> Vec<u8> {
        // Layout:
        //   ELF header          64 B
        //   .text               16 B  (mov r0, 0; exit)
        //   .maps               28 B  (one BpfLoadMapDef: 7 × u32)
        //   .strtab             13 B  ("\0counter_map\0")
        //   .shstrtab           39 B
        //   .symtab             48 B  (null + 1 GLOBAL OBJECT in .maps)
        //   Section headers    384 B  (6 × 64)

        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        // One BPF_MAP_TYPE_ARRAY map (7 × u32 LE).
        let mut map_def = Vec::with_capacity(28);
        map_def.extend_from_slice(&1u32.to_le_bytes()); // map_type = ARRAY (sonde ABI)
        map_def.extend_from_slice(&4u32.to_le_bytes()); // key_size
        map_def.extend_from_slice(&4u32.to_le_bytes()); // value_size
        map_def.extend_from_slice(&1u32.to_le_bytes()); // max_entries
        map_def.extend_from_slice(&0u32.to_le_bytes()); // map_flags
        map_def.extend_from_slice(&0u32.to_le_bytes()); // inner_map_idx
        map_def.extend_from_slice(&0u32.to_le_bytes()); // numa_node

        let strtab: &[u8] = b"\0counter_map\0"; // 13 bytes
                                                // shstrtab: "\0.text\0.maps\0.strtab\0.symtab\0.shstrtab\0"
                                                //   offsets:  0  1     7     13      21      29
        let shstrtab: &[u8] = b"\0.text\0.maps\0.strtab\0.symtab\0.shstrtab\0"; // 39 bytes

        let text_offset: u64 = 64;
        let maps_offset: u64 = text_offset + bpf_code.len() as u64; // 80
        let strtab_offset: u64 = maps_offset + map_def.len() as u64; // 108
        let shstrtab_offset: u64 = strtab_offset + strtab.len() as u64; // 121
        let symtab_offset: u64 = shstrtab_offset + shstrtab.len() as u64; // 160
        let shdr_offset: u64 = symtab_offset + 48; // 208

        let mut elf = Vec::new();

        // ── ELF header (64 bytes) ──
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']); // magic
        elf.push(2); // EI_CLASS = ELFCLASS64
        elf.push(1); // EI_DATA = ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]); // padding
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&6u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&5u16.to_le_bytes()); // e_shstrndx = 5
        assert_eq!(elf.len(), 64);

        // ── .text section data ──
        elf.extend_from_slice(&bpf_code);

        // ── .maps section data ──
        elf.extend_from_slice(&map_def);

        // ── .strtab section data ──
        elf.extend_from_slice(strtab);

        // ── .shstrtab section data ──
        elf.extend_from_slice(shstrtab);

        // ── .symtab section data (2 × 24 bytes) ──
        // [0] Null symbol
        elf.extend_from_slice(&[0u8; 24]);

        // [1] Symbol for counter_map in .maps section
        let mut sym = [0u8; 24];
        sym[0..4].copy_from_slice(&1u32.to_le_bytes()); // st_name = 1
        sym[4] = 0x11; // st_info = STB_GLOBAL | STT_OBJECT
        sym[5] = 0; // st_other
        sym[6..8].copy_from_slice(&2u16.to_le_bytes()); // st_shndx = 2 (.maps)
        sym[8..16].copy_from_slice(&0u64.to_le_bytes()); // st_value = 0
        sym[16..24].copy_from_slice(&28u64.to_le_bytes()); // st_size = 28
        elf.extend_from_slice(&sym);

        // ── Section headers (6 × 64 bytes) ──

        // [0] Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes()); // SHF_ALLOC | SHF_EXECINSTR
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [2] .maps
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&maps_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(map_def.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [3] .strtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&13u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh[24..32].copy_from_slice(&strtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(strtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [4] .symtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&21u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&2u32.to_le_bytes()); // sh_type = SHT_SYMTAB
        sh[24..32].copy_from_slice(&symtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&48u64.to_le_bytes()); // sh_size = 2 entries
        sh[40..44].copy_from_slice(&3u32.to_le_bytes()); // sh_link = .strtab index
        sh[44..48].copy_from_slice(&1u32.to_le_bytes()); // sh_info = first non-local
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        sh[56..64].copy_from_slice(&24u64.to_le_bytes()); // sh_entsize
        elf.extend_from_slice(&sh);

        // [5] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&29u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        elf
    }

    /// Build a minimal BPF ELF with arbitrary bytecode in a `sonde` section.
    fn make_sonde_elf(bpf_code: &[u8]) -> Vec<u8> {
        let shstrtab: &[u8] = b"\0sonde\0.shstrtab\0"; // 17 bytes

        let text_offset: u64 = 64;
        let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ── ELF header (64 bytes) ──
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2); // EI_CLASS = ELFCLASS64
        elf.push(1); // EI_DATA = ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]); // padding
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx = 2
        assert_eq!(elf.len(), 64);

        // ── sonde section data ──
        elf.extend_from_slice(bpf_code);

        // ── .shstrtab section data ──
        elf.extend_from_slice(shstrtab);

        // ── Section headers (3 entries × 64 bytes each) ──

        // [0] Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] sonde section header
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name = offset of "sonde"
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
        sh[8..16].copy_from_slice(&flags.to_le_bytes());
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [2] .shstrtab section header
        let mut sh2 = [0u8; 64];
        sh2[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name = offset of ".shstrtab"
        sh2[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh2[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh2[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh2[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh2);

        elf
    }

    #[test]
    fn ingest_elf_ephemeral_with_maps_rejected() {
        let elf = make_minimal_bpf_elf_with_maps();
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&elf, VerificationProfile::Ephemeral)
            .unwrap_err();
        match &err {
            ProgramError::VerificationFailed(msg) => {
                assert!(
                    msg.contains("ephemeral programs must not declare maps"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected VerificationFailed, got: {other:?}"),
        }
    }

    /// Proves `SondePlatform` recognises helper 3 (`i2c_write_read`), which
    /// has no Linux equivalent and would fail without the custom platform.
    #[test]
    fn ingest_elf_with_sonde_helper_succeeds() {
        // i2c_write_read(handle, *write_ptr, write_len, *read_ptr, read_len) → i32
        #[rustfmt::skip]
        let bpf_code: [u8; 88] = [
            0x62, 0x0a, 0xf8, 0xff, 0x00, 0x00, 0x00, 0x00, // *(u32*)(r10 - 8) = 0  (init write buf)
            0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r1 = 0                 (handle)
            0xbf, 0xa2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r2 = r10
            0x07, 0x02, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, // r2 += -8               (write_ptr)
            0xb7, 0x03, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, // r3 = 4                 (write_len)
            0xbf, 0xa4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r4 = r10
            0x07, 0x04, 0x00, 0x00, 0xf0, 0xff, 0xff, 0xff, // r4 += -16              (read_ptr)
            0xb7, 0x05, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, // r5 = 4                 (read_len)
            0x85, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, // call 3                 (i2c_write_read)
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r0 = 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        let elf = make_sonde_elf(&bpf_code);
        let lib = ProgramLibrary::new();
        let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

        assert!(!record.hash.is_empty());
        assert!(record.size > 0);

        let image = ProgramImage::decode(&record.image).unwrap();
        assert_eq!(image.bytecode.len(), bpf_code.len());
    }

    /// Proves `SondePlatform` recognises helper 8 (`send`), the primary
    /// transmission helper already tested on hardware.
    #[test]
    fn ingest_elf_with_sonde_helper_8_succeeds() {
        // send(*ptr, len) → i32
        // Initialize stack memory so the readable-pointer arg passes verification.
        #[rustfmt::skip]
        let bpf_code: [u8; 56] = [
            0x62, 0x0a, 0xf8, 0xff, 0x00, 0x00, 0x00, 0x00, // *(u32*)(r10 - 8) = 0
            0xbf, 0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r1 = r10
            0x07, 0x01, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, // r1 += -8  (blob_ptr)
            0xb7, 0x02, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, // r2 = 4   (blob_len)
            0x85, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, // call 8   (send)
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r0 = 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        let elf = make_sonde_elf(&bpf_code);
        let lib = ProgramLibrary::new();
        let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

        assert!(!record.hash.is_empty());
        assert!(record.size > 0);

        let image = ProgramImage::decode(&record.image).unwrap();
        assert_eq!(image.bytecode.len(), bpf_code.len());
    }

    /// E2E verification: a BPF program calling sonde helper `gpio_read` (ID 5)
    /// must pass verification when using `SondePlatform` (T-0408, GW-0404).
    #[test]
    fn ingest_elf_sonde_helper_call_verified() {
        // BPF: mov r1, 0; call 5 (gpio_read); exit
        let elf = make_sonde_elf(&[
            0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r1, 0
            0x85, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // call 5 (gpio_read)
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ]);
        let lib = ProgramLibrary::new();
        let record = lib
            .ingest_elf(&elf, VerificationProfile::Resident)
            .expect("ELF calling sonde helper gpio_read should pass verification");
        assert!(!record.hash.is_empty());
        let image = ProgramImage::decode(&record.image).unwrap();
        // 3 instructions × 8 bytes = 24 bytes of bytecode
        assert_eq!(image.bytecode.len(), 24);
    }

    /// Negative case: a BPF program calling an unsupported helper (ID 0)
    /// must be rejected by verification (GW-0404).
    #[test]
    fn ingest_elf_unsupported_helper_rejected() {
        // BPF: call 0 (unsupported); exit
        let elf = make_sonde_elf(&[
            0x85, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // call 0 (unsupported)
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ]);
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&elf, VerificationProfile::Resident)
            .unwrap_err();
        assert!(
            matches!(err, ProgramError::VerificationFailed(_)),
            "unsupported helper call should fail verification, got: {err}"
        );
    }

    /// Verification failures must include per-instruction diagnostic notes from
    /// Prevail's forward analysis (GW-1305).
    #[test]
    fn ingest_elf_verification_failure_includes_diagnostics() {
        // BPF program that fails forward analysis (not control flow):
        // overwrite r1 (context pointer) with 0, then dereference it.
        #[rustfmt::skip]
        let bpf_code: [u8; 24] = [
            0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r1, 0
            0x79, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r0 = *(u64*)(r1 + 0)
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        let elf = make_sonde_elf(&bpf_code);
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&elf, VerificationProfile::Resident)
            .unwrap_err();
        match &err {
            ProgramError::VerificationFailed(msg) => {
                // The message must contain the summary AND per-instruction notes.
                assert!(
                    msg.contains("failed verification"),
                    "should contain summary: {msg}"
                );
                // Assert structurally that we have multi-line diagnostics
                // and that at least one subsequent line looks like a
                // per-instruction verifier diagnostic with an instruction
                // label (e.g. "0: ..."). This avoids brittleness on
                // Prevail's exact wording while still verifying GW-1305.
                let lines: Vec<&str> = msg.lines().collect();
                assert!(
                    lines.len() >= 2,
                    "expected multi-line verifier diagnostics, got: {msg}"
                );
                let has_instruction_label = lines.iter().skip(1).any(|line| {
                    let trimmed = line.trim_start();
                    trimmed
                        .split_once(':')
                        .map(|(idx, _)| !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(false)
                });
                assert!(
                    has_instruction_label,
                    "expected per-instruction verifier diagnostic with an \
                     instruction label, got: {msg}"
                );
            }
            other => panic!("expected VerificationFailed, got: {other:?}"),
        }
    }

    /// Build a BPF ELF that includes a `.rodata` section with the given
    /// content. Used to test `extract_global_section_data` (GW-0405).
    fn make_bpf_elf_with_rodata(rodata: &[u8]) -> Vec<u8> {
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        // shstrtab: "\0.text\0.rodata\0.shstrtab\0"
        //   offsets:  0  1     7       15
        let shstrtab: &[u8] = b"\0.text\0.rodata\0.shstrtab\0"; // 25 bytes

        let text_offset: u64 = 64;
        let rodata_offset: u64 = text_offset + bpf_code.len() as u64;
        let shstrtab_offset: u64 = rodata_offset + rodata.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ── ELF header (64 bytes) ──
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2); // EI_CLASS = ELFCLASS64
        elf.push(1); // EI_DATA = ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]); // padding
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&4u16.to_le_bytes()); // e_shnum (null + .text + .rodata + .shstrtab)
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shstrndx = 3
        assert_eq!(elf.len(), 64);

        // ── .text section data ──
        elf.extend_from_slice(&bpf_code);

        // ── .rodata section data ──
        elf.extend_from_slice(rodata);

        // ── .shstrtab section data ──
        elf.extend_from_slice(shstrtab);

        // ── Section headers (4 × 64 bytes) ──

        // [0] Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes()); // SHF_ALLOC | SHF_EXECINSTR
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] .rodata
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name = offset of ".rodata"
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&rodata_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(rodata.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [3] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&15u32.to_le_bytes()); // sh_name = offset of ".shstrtab"
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        elf
    }

    /// T-0411: ELF .rodata section data is extracted correctly (GW-0405).
    #[test]
    fn extract_global_section_data_rodata() {
        let rodata_content = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let elf = make_bpf_elf_with_rodata(&rodata_content);

        let sections = extract_global_section_data(&elf);
        assert_eq!(
            sections.len(),
            1,
            "expected one global data section (.rodata)"
        );
        assert_eq!(sections[0].0, rodata_content);
        assert!(
            sections[0].1,
            "expected .rodata section to be marked read-only"
        );
    }

    /// T-0412: ELF .bss section (SHT_NOBITS) produces empty data (GW-0405).
    #[test]
    fn extract_global_section_data_bss() {
        // Build ELF with a .bss section (SHT_NOBITS, type 8).
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let shstrtab: &[u8] = b"\0.text\0.bss\0.shstrtab\0"; // 22 bytes

        let text_offset: u64 = 64;
        let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ELF header
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2);
        elf.push(1);
        elf.push(1);
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes());
        elf.extend_from_slice(&247u16.to_le_bytes());
        elf.extend_from_slice(&1u32.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&shdr_offset.to_le_bytes());
        elf.extend_from_slice(&0u32.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&4u16.to_le_bytes()); // 4 sections
        elf.extend_from_slice(&3u16.to_le_bytes()); // shstrndx = 3
        assert_eq!(elf.len(), 64);

        elf.extend_from_slice(&bpf_code);
        elf.extend_from_slice(shstrtab);

        // [0] Null
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&16u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] .bss (SHT_NOBITS = 8)
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&7u32.to_le_bytes()); // offset of ".bss"
        sh[4..8].copy_from_slice(&8u32.to_le_bytes()); // SHT_NOBITS
        sh[8..16].copy_from_slice(&0x3u64.to_le_bytes()); // SHF_WRITE | SHF_ALLOC
        sh[32..40].copy_from_slice(&64u64.to_le_bytes()); // sh_size = 64
        elf.extend_from_slice(&sh);

        // [3] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&12u32.to_le_bytes()); // offset of ".shstrtab"
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        elf.extend_from_slice(&sh);

        let sections = extract_global_section_data(&elf);
        assert_eq!(sections.len(), 1, "expected one global data section (.bss)");
        assert!(
            sections[0].0.is_empty(),
            ".bss section should produce empty data"
        );
        assert!(!sections[0].1, ".bss section should not be read-only");
    }

    /// Non-BPF ELF (wrong e_machine) returns no sections.
    #[test]
    fn extract_global_section_data_non_bpf() {
        let sections = extract_global_section_data(&[
            0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, // e_type
            0, 0, // e_machine = 0 (not BPF)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert!(sections.is_empty());
    }

    /// GW-0405 criterion 5: explicit `.maps` sections have no initial data.
    ///
    /// `extract_global_section_data` only captures global variable sections
    /// (`.rodata`, `.data`, `.bss`).  An explicit `.maps` section MUST NOT
    /// appear in the result — map definitions carry structure, not initial
    /// data.
    #[test]
    fn extract_global_section_data_maps_no_initial_data() {
        // Build an ELF with both `.maps` and `.rodata` sections.
        // Only `.rodata` should appear in the extracted initial data.
        let rodata_content = vec![0xDE, 0xAD];
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        // One BPF_MAP_TYPE_ARRAY map definition (7 × u32 LE = 28 bytes).
        let mut map_def = Vec::with_capacity(28);
        map_def.extend_from_slice(&1u32.to_le_bytes()); // map_type = ARRAY
        map_def.extend_from_slice(&4u32.to_le_bytes()); // key_size
        map_def.extend_from_slice(&4u32.to_le_bytes()); // value_size
        map_def.extend_from_slice(&1u32.to_le_bytes()); // max_entries
        map_def.extend_from_slice(&0u32.to_le_bytes()); // map_flags
        map_def.extend_from_slice(&0u32.to_le_bytes()); // inner_map_idx
        map_def.extend_from_slice(&0u32.to_le_bytes()); // numa_node

        // shstrtab: "\0.text\0.maps\0.rodata\0.shstrtab\0"
        //   offsets:  0  1     7     13      21
        let shstrtab: &[u8] = b"\0.text\0.maps\0.rodata\0.shstrtab\0"; // 31 bytes

        let text_offset: u64 = 64;
        let maps_offset: u64 = text_offset + bpf_code.len() as u64;
        let rodata_offset: u64 = maps_offset + map_def.len() as u64;
        let shstrtab_offset: u64 = rodata_offset + rodata_content.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ELF header (64 bytes)
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2);
        elf.push(1);
        elf.push(1);
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&shdr_offset.to_le_bytes());
        elf.extend_from_slice(&0u32.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&5u16.to_le_bytes()); // 5 sections
        elf.extend_from_slice(&4u16.to_le_bytes()); // shstrndx = 4
        assert_eq!(elf.len(), 64);

        elf.extend_from_slice(&bpf_code);
        elf.extend_from_slice(&map_def);
        elf.extend_from_slice(&rodata_content);
        elf.extend_from_slice(shstrtab);

        // [0] Null
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes());
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] .maps (SHT_PROGBITS — explicit map definition, not global data)
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&7u32.to_le_bytes()); // offset of ".maps"
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&maps_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(map_def.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [3] .rodata (SHT_PROGBITS — global data section with initial data)
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&13u32.to_le_bytes()); // offset of ".rodata"
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&rodata_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(rodata_content.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [4] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&21u32.to_le_bytes()); // offset of ".shstrtab"
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        let sections = extract_global_section_data(&elf);
        assert_eq!(
            sections.len(),
            1,
            "only .rodata should produce initial data; .maps must be excluded"
        );
        assert_eq!(
            sections[0].0, rodata_content,
            "the single initial data entry should match .rodata content"
        );
        assert!(sections[0].1, ".rodata section should be marked read-only");
    }

    /// GW-0405 criterion 6: prefix matching captures `.rodata.str1.1` etc.
    #[test]
    fn extract_global_section_data_rodata_prefix() {
        let rodata_content = vec![0xCA, 0xFE];
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];

        // shstrtab with prefixed name: ".rodata.str1.1"
        // "\0.text\0.rodata.str1.1\0.shstrtab\0"
        //   0  1     7              22
        let shstrtab: &[u8] = b"\0.text\0.rodata.str1.1\0.shstrtab\0"; // 32 bytes

        let text_offset: u64 = 64;
        let rodata_offset: u64 = text_offset + bpf_code.len() as u64;
        let shstrtab_offset: u64 = rodata_offset + rodata_content.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ELF header (64 bytes)
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2);
        elf.push(1);
        elf.push(1);
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&shdr_offset.to_le_bytes());
        elf.extend_from_slice(&0u32.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&4u16.to_le_bytes()); // 4 sections
        elf.extend_from_slice(&3u16.to_le_bytes()); // shstrndx = 3
        assert_eq!(elf.len(), 64);

        elf.extend_from_slice(&bpf_code);
        elf.extend_from_slice(&rodata_content);
        elf.extend_from_slice(shstrtab);

        // [0] Null
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes());
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] .rodata.str1.1
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name offset
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&rodata_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(rodata_content.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [3] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&22u32.to_le_bytes()); // offset of ".shstrtab"
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        let sections = extract_global_section_data(&elf);
        assert_eq!(
            sections.len(),
            1,
            "prefixed .rodata.str1.1 should be matched"
        );
        assert_eq!(sections[0].0, rodata_content);
        assert!(sections[0].1, ".rodata.str1.1 should be marked read-only");
    }

    /// Build a minimal BPF ELF whose only non-`.text` section has the given
    /// name. Used to test `elf_has_map_sections` against arbitrary section
    /// names without invoking the full Prevail loader.
    fn make_bpf_elf_with_named_section(section_name: &str) -> Vec<u8> {
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        // Build shstrtab: "\0.text\0<section_name>\0.shstrtab\0"
        let mut shstrtab = Vec::new();
        shstrtab.push(0); // index 0: empty
        shstrtab.extend_from_slice(b".text");
        shstrtab.push(0); // index 1..6
        let sec_name_off = shstrtab.len();
        shstrtab.extend_from_slice(section_name.as_bytes());
        shstrtab.push(0);
        let shstrtab_name_off = shstrtab.len();
        shstrtab.extend_from_slice(b".shstrtab");
        shstrtab.push(0);

        // Section data (the named section carries 2 bytes of dummy data).
        let sec_data: [u8; 2] = [0xAB, 0xCD];

        let text_offset: u64 = 64;
        let sec_data_offset: u64 = text_offset + bpf_code.len() as u64;
        let shstrtab_offset: u64 = sec_data_offset + sec_data.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ELF header (64 bytes)
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2); // EI_CLASS = ELFCLASS64
        elf.push(1); // EI_DATA = ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&4u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shstrndx = 3
        assert_eq!(elf.len(), 64);

        elf.extend_from_slice(&bpf_code);
        elf.extend_from_slice(&sec_data);
        elf.extend_from_slice(&shstrtab);

        // [0] Null
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes()); // SHF_ALLOC | SHF_EXECINSTR
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] named section
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&(sec_name_off as u32).to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes()); // SHF_ALLOC
        sh[24..32].copy_from_slice(&sec_data_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(sec_data.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [3] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&(shstrtab_name_off as u32).to_le_bytes());
        sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        elf
    }

    /// `elf_has_map_sections` detects prefixed global data section names
    /// (e.g. `.rodata.str1.1`), consistent with `extract_global_section_data`.
    #[test]
    fn elf_has_map_sections_detects_prefixed_global_data() {
        for name in &[".rodata.str1.1", ".data.rel.ro", ".bss.my_var"] {
            let elf = make_bpf_elf_with_named_section(name);
            assert!(
                elf_has_map_sections(&elf),
                "`elf_has_map_sections` should detect `{name}` as a map section"
            );
        }
    }

    /// `elf_has_map_sections` does not match unrelated sections whose names
    /// happen to share a character prefix (e.g. `.rodataXYZ` without a dot
    /// separator is NOT `.rodata.<suffix>`).
    #[test]
    fn elf_has_map_sections_ignores_non_dot_prefix() {
        let elf = make_bpf_elf_with_named_section(".rodataXYZ");
        assert!(
            !elf_has_map_sections(&elf),
            "`.rodataXYZ` should not match (no dot separator)"
        );
    }

    /// Build a BPF ELF with two executable sections: `sonde` (with `sonde_code`)
    /// and `.text` (with `text_code`). Used to test that `ingest_elf` filters
    /// to the `sonde` section (GW-0401 criterion 6).
    fn make_multi_section_bpf_elf(sonde_code: &[u8], text_code: &[u8]) -> Vec<u8> {
        assert!(sonde_code.len().is_multiple_of(8));
        assert!(text_code.len().is_multiple_of(8));

        // shstrtab: "\0sonde\0.text\0.shstrtab\0"
        //   offsets:  0  1     7     13
        let shstrtab: &[u8] = b"\0sonde\0.text\0.shstrtab\0"; // 23 bytes

        let sonde_offset: u64 = 64; // right after ELF header
        let text_offset: u64 = sonde_offset + sonde_code.len() as u64;
        let shstrtab_offset: u64 = text_offset + text_code.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ── ELF header (64 bytes) ──
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']); // e_ident magic
        elf.push(2); // EI_CLASS = ELFCLASS64
        elf.push(1); // EI_DATA = ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]); // padding
        elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
        elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
        elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&4u16.to_le_bytes()); // e_shnum (null + sonde + .text + .shstrtab)
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shstrndx = 3
        assert_eq!(elf.len(), 64);

        // ── sonde section data ──
        elf.extend_from_slice(sonde_code);

        // ── .text section data ──
        elf.extend_from_slice(text_code);

        // ── .shstrtab section data ──
        elf.extend_from_slice(shstrtab);

        // ── Section headers (4 entries × 64 bytes each) ──

        // [0] Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] sonde section header
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name = offset of "sonde"
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
        sh[8..16].copy_from_slice(&flags.to_le_bytes());
        sh[24..32].copy_from_slice(&sonde_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(sonde_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [2] .text section header
        let mut sh2 = [0u8; 64];
        sh2[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name = offset of ".text"
        sh2[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        sh2[8..16].copy_from_slice(&flags.to_le_bytes());
        sh2[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh2[32..40].copy_from_slice(&(text_code.len() as u64).to_le_bytes());
        sh2[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh2);

        // [3] .shstrtab section header
        let mut sh3 = [0u8; 64];
        sh3[0..4].copy_from_slice(&13u32.to_le_bytes()); // sh_name = offset of ".shstrtab"
        sh3[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh3[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh3[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh3[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh3);

        elf
    }

    /// Multi-section ELF with both `sonde` and `.text` sections should succeed,
    /// extracting only the `sonde` section program (GW-0401 criterion 6, T-0413).
    #[test]
    fn ingest_elf_multi_section_filters_to_sonde() {
        #[rustfmt::skip]
        let sonde_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        #[rustfmt::skip]
        let text_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // mov r0, 1
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];

        let elf = make_multi_section_bpf_elf(&sonde_code, &text_code);
        let lib = ProgramLibrary::new();
        let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

        // Verify the stored bytecode is from the sonde section (mov r0, 0),
        // not the .text section (mov r0, 1).
        let image = ProgramImage::decode(&record.image).unwrap();
        assert_eq!(image.bytecode, sonde_code);
    }

    /// Build a small ELF with one executable section, one global data section,
    /// and a relocation that references that global section from the first
    /// `LD_DW_IMM` in the executable section.
    fn make_bpf_elf_with_global_data_relocation(
        target_section_name: &str,
        global_section_name: &str,
    ) -> Vec<u8> {
        let section_code: [u8; 24] = [
            0x18, 0x01, 0x00, 0x00, 0, 0, 0, 0, // ldimm64 r1, 0 (to be relocated)
            0, 0, 0, 0, 0, 0, 0, 0, 0x95, 0x00, 0x00, 0x00, 0, 0, 0, 0, // exit
        ];
        let global_data: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
        let strtab: &[u8] = b"\0";
        let rel_name = format!(".rel{target_section_name}");
        let shstrtab = format!(
            "\0{target}\0{global}\0{rel}\0.strtab\0.symtab\0.shstrtab\0",
            target = target_section_name,
            global = global_section_name,
            rel = rel_name
        )
        .into_bytes();

        let target_name_off = 1u32;
        let global_name_off = target_name_off + target_section_name.len() as u32 + 1;
        let rel_name_off = global_name_off + global_section_name.len() as u32 + 1;
        let strtab_name_off = rel_name_off + rel_name.len() as u32 + 1;
        let symtab_name_off = strtab_name_off + ".strtab".len() as u32 + 1;
        let shstrtab_name_off = symtab_name_off + ".symtab".len() as u32 + 1;

        let target_offset: u64 = 64;
        let global_offset: u64 = target_offset + section_code.len() as u64;
        let rel_offset: u64 = global_offset + global_data.len() as u64;
        let strtab_offset: u64 = rel_offset + 16;
        let symtab_offset: u64 = strtab_offset + strtab.len() as u64;
        let shstrtab_offset: u64 = symtab_offset + 48;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2);
        elf.push(1);
        elf.push(1);
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes());
        elf.extend_from_slice(&247u16.to_le_bytes());
        elf.extend_from_slice(&1u32.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&shdr_offset.to_le_bytes());
        elf.extend_from_slice(&0u32.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes());
        elf.extend_from_slice(&7u16.to_le_bytes());
        elf.extend_from_slice(&6u16.to_le_bytes());

        elf.extend_from_slice(&section_code);
        elf.extend_from_slice(&global_data);

        // One ELF64_Rel entry: offset=0, symbol=1, type=1.
        elf.extend_from_slice(&0u64.to_le_bytes());
        elf.extend_from_slice(&((1u64 << 32) | 1u64).to_le_bytes());

        elf.extend_from_slice(strtab);

        // [0] null symbol
        elf.extend_from_slice(&[0u8; 24]);
        // [1] section symbol for the global data section
        let mut sym = [0u8; 24];
        sym[4] = 0x03; // STT_SECTION
        sym[6..8].copy_from_slice(&2u16.to_le_bytes()); // target the global data section
        elf.extend_from_slice(&sym);

        elf.extend_from_slice(&shstrtab);

        // [0] null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] target executable section
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&target_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes());
        sh[8..16].copy_from_slice(&0x6u64.to_le_bytes());
        sh[24..32].copy_from_slice(&target_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(section_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [2] global data section
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&global_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes());
        sh[8..16].copy_from_slice(&0x2u64.to_le_bytes());
        sh[24..32].copy_from_slice(&global_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(global_data.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&4u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [3] relocation section
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&rel_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&SHT_REL.to_le_bytes());
        sh[24..32].copy_from_slice(&rel_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&16u64.to_le_bytes());
        sh[40..44].copy_from_slice(&5u32.to_le_bytes()); // linked .symtab section
        sh[44..48].copy_from_slice(&1u32.to_le_bytes()); // relocates section [1]
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        sh[56..64].copy_from_slice(&16u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [4] .strtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&strtab_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&3u32.to_le_bytes());
        sh[24..32].copy_from_slice(&strtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(strtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [5] .symtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&symtab_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&SHT_SYMTAB.to_le_bytes());
        sh[24..32].copy_from_slice(&symtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&48u64.to_le_bytes());
        sh[40..44].copy_from_slice(&4u32.to_le_bytes()); // linked .strtab section
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        sh[56..64].copy_from_slice(&24u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // [6] .shstrtab
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&shstrtab_name_off.to_le_bytes());
        sh[4..8].copy_from_slice(&3u32.to_le_bytes());
        sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        elf
    }

    #[test]
    fn rewrite_global_data_relocations_rewrites_lddw_to_map_value_index() {
        let elf = make_bpf_elf_with_global_data_relocation("sonde", ".data");
        let mut bytecode = vec![
            0x18, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x95, 0x00, 0x00, 0x00, 0,
            0, 0, 0,
        ];
        let map_descriptors = vec![
            EbpfMapDescriptor {
                original_fd: 1,
                map_type: 0,
                key_size: 4,
                value_size: 4,
                max_entries: 1,
                inner_map_fd: 0,
            },
            EbpfMapDescriptor {
                original_fd: 2,
                map_type: 1,
                key_size: 4,
                value_size: 4,
                max_entries: 1,
                inner_map_fd: 0,
            },
        ];

        rewrite_global_data_relocations(&elf, "sonde", &mut bytecode, &map_descriptors).unwrap();

        assert_eq!(bytecode[0], BPF_LD_DW_IMM);
        assert_eq!(bytecode[1], 0x61, "src nibble should be rewritten to 6");
        assert_eq!(
            u32::from_le_bytes([bytecode[4], bytecode[5], bytecode[6], bytecode[7]]),
            0,
            "the first global section should map to map index 0"
        );
        assert_eq!(
            u32::from_le_bytes([bytecode[12], bytecode[13], bytecode[14], bytecode[15]]),
            0
        );
    }
}
