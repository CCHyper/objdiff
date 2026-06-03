# OMF Object File Support — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement complete, production-quality OMF object file diffing for Watcom and Borland 16-bit and 32-bit `.OBJ` files.

**Architecture:** Expose the `use32` segment flag from the object fork's OMF section reader, plumb `FixupLocation`/`FixupMode` through `RelocationFlags::Omf`, implement correct relocation sizing and naming in `ArchX86`, and add per-section bitness tracking to support 16-bit decoding.

**Tech Stack:** Rust, `object` crate fork at `E:/CCHyper_object` (local path), `iced-x86` for x86 decoding, `flagset` for section flags, `anyhow` for errors.

**Design doc:** `docs/plans/2026-03-06-omf-support-design.md`

---

## Before You Start

- Working directory: `E:/CCHyper_objdiff`
- Object fork: `E:/CCHyper_object`
- Branch: `encounter_omf`
- Build command: `cargo build -p objdiff-core`
- Test command: `cargo test -p objdiff-core`
- Do NOT commit — the user handles all commits

---

## Task 1: Object fork — expose USE32 bit via SectionFlags

**Goal:** Make the per-segment `use32` flag (from the OMF SEGDEF record) accessible through the generic `SectionFlags` API so objdiff can detect 16-bit vs 32-bit code sections.

**Files:**
- Modify: `E:/CCHyper_object/src/common.rs` (after line ~606, inside `SectionFlags` enum)
- Modify: `E:/CCHyper_object/src/read/omf/section.rs` (line ~211, `flags()` impl)

---

### Step 1: Add `Omf` variant to `SectionFlags` in the object fork

Open `E:/CCHyper_object/src/common.rs`. Find the `SectionFlags` enum (around line 583). It currently ends with:

```rust
    /// XCOFF section flags.
    Xcoff {
        /// `s_flags` field in the XCOFF symbol.
        s_flags: u32,
    },
}
```

Add a new variant after `Xcoff`:

```rust
    /// XCOFF section flags.
    Xcoff {
        /// `s_flags` field in the XCOFF symbol.
        s_flags: u32,
    },
    #[cfg(feature = "omf")]
    /// OMF section flags.
    Omf {
        /// Whether this segment was declared with the USE32 attribute.
        /// `false` means USE16 (16-bit addressing), `true` means USE32 (32-bit).
        use32: bool,
    },
}
```

---

### Step 2: Implement `flags()` in `OmfSection`

Open `E:/CCHyper_object/src/read/omf/section.rs`. Find the `flags()` method (line ~211):

```rust
    fn flags(&self) -> SectionFlags {
        // OMF does not map to a common SectionFlags variant.
        // Use RelocationFlags::Omf on individual relocations for OMF-specific metadata.
        SectionFlags::None
    }
```

Replace it with:

```rust
    fn flags(&self) -> SectionFlags {
        SectionFlags::Omf { use32: self.segment().use32 }
    }
```

---

### Step 3: Build the object fork to confirm it compiles

```bash
cargo build --manifest-path /e/CCHyper_object/Cargo.toml --features omf
```

Expected: compiles with no errors. (Warnings are fine.)

---

## Task 2: Core data model — fix `RelocationFlags::Omf` and add `SectionFlag::Code16Bit`

**Goal:** Replace the empty `Omf(/* TODO */)` variant with a proper struct variant carrying the two fields needed by downstream code. Add a section flag for 16-bit detection.

**Files:**
- Modify: `objdiff-core/src/obj/mod.rs` (lines ~57-63 for `SectionFlag`, line ~373 for `RelocationFlags`)

---

### Step 1: Add import for OMF types in `obj/mod.rs`

Open `objdiff-core/src/obj/mod.rs`. Find the existing imports at the top of the file. After the `use flagset::{FlagSet, flags};` line, add:

```rust
use object::omf::{FixupLocation, FixupMode};
```

---

### Step 2: Add `Code16Bit` to `SectionFlag`

Find the `flags!` block for `SectionFlag` (lines ~57-63):

```rust
flags! {
    #[derive(Hash)]
    pub enum SectionFlag: u8 {
        /// Section combined from multiple input sections
        Combined,
    }
}
```

Replace it with:

```rust
flags! {
    #[derive(Hash)]
    pub enum SectionFlag: u8 {
        /// Section combined from multiple input sections
        Combined,
        /// Section is a 16-bit code segment (OMF USE16).
        /// When set, the x86 instruction decoder will use 16-bit mode for this section.
        Code16Bit,
    }
}
```

---

### Step 3: Fix `RelocationFlags::Omf`

Find the `RelocationFlags` enum (lines ~369-374):

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RelocationFlags {
    Elf(u32),
    Coff(u16),
    Omf(/* TODO */),
}
```

Replace it with:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RelocationFlags {
    Elf(u32),
    Coff(u16),
    /// OMF fixup. `location` determines the byte size and addressing mode.
    /// `mode` distinguishes PC-relative (SelfRelative) from absolute (SegmentRelative).
    Omf { location: FixupLocation, mode: FixupMode },
}
```

---

### Step 4: Build to find all compilation errors caused by the changed variant

```bash
cargo build -p objdiff-core 2>&1 | grep "error\|Omf"
```

Expected: errors about `Omf()` (old unit variant) at locations in `x86.rs`. These will be fixed in Task 4.

---

### Step 5: Quick-fix the two stub `Omf()` arms in `x86.rs` to compile

Open `objdiff-core/src/arch/x86.rs`. Find lines 75 and 88 (the two `RelocationFlags::Omf()` arms):

```rust
RelocationFlags::Omf() => Some(4), // TODO
...
RelocationFlags::Omf() => None, // TODO
```

Change both to use the struct pattern (temporary stubs — Task 4 will implement properly):

```rust
RelocationFlags::Omf { .. } => Some(4), // TODO: use location field
...
RelocationFlags::Omf { .. } => None, // TODO: use location field
```

---

### Step 6: Build again to confirm it compiles

```bash
cargo build -p objdiff-core
```

Expected: clean build with no errors.

---

## Task 3: Read pipeline — extract OMF reloc fields and detect USE16 sections

**Goal:** Stop discarding the OMF relocation data and start storing `location`+`mode`. Also detect 16-bit sections via the new `SectionFlag::Code16Bit`.

**Files:**
- Modify: `objdiff-core/src/obj/read.rs` (line ~513 for reloc, lines ~332-344 for section)

---

### Step 1: Fix `map_section_relocations` to extract OMF fields

Open `objdiff-core/src/obj/read.rs`. Find line ~513 inside `map_section_relocations`:

```rust
        let flags = match reloc.flags() {
            object::RelocationFlags::Elf { r_type } => RelocationFlags::Elf(r_type),
            object::RelocationFlags::Coff { typ } => RelocationFlags::Coff(typ),
            object::RelocationFlags::Omf { .. } => RelocationFlags::Omf(/* TODO */),
            flags => bail!("Unhandled relocation flags: {:?}", flags),
        };
```

Replace the `Omf` arm:

```rust
        let flags = match reloc.flags() {
            object::RelocationFlags::Elf { r_type } => RelocationFlags::Elf(r_type),
            object::RelocationFlags::Coff { typ } => RelocationFlags::Coff(typ),
            object::RelocationFlags::Omf { location, mode, .. } => {
                RelocationFlags::Omf { location, mode }
            }
            flags => bail!("Unhandled relocation flags: {:?}", flags),
        };
```

---

### Step 2: Detect USE16 sections in `map_sections`

Find `map_sections` (around line 295). Find the `result.push(Section { ... })` block (line ~332). The section is pushed with `flags: Default::default()`. We need to conditionally set `Code16Bit` for OMF USE16 code sections.

Add the following import at the top of `read.rs`, with the other imports from `obj`:

```rust
use crate::obj::{
    // ... existing imports ...
    SectionFlag,
};
```

(Check what is already imported from `crate::obj` and add `SectionFlag` to the existing list.)

Then, just before the `result.push(Section { ... })` block, insert:

```rust
        // Detect 16-bit OMF code sections (USE16 attribute in SEGDEF record).
        // This drives the x86 instruction decoder to use 16-bit mode for this section.
        let mut section_flags = SectionFlagSet::default();
        if kind == SectionKind::Code {
            if let object::SectionFlags::Omf { use32 } = section.flags() {
                if !use32 {
                    section_flags |= SectionFlag::Code16Bit;
                }
            }
        }
```

Then change the `flags:` field inside `Section { ... }`:

```rust
        result.push(Section {
            id,
            name: name.to_string(),
            address: section.address(),
            size: section.size(),
            kind,
            data: SectionData(data),
            flags: section_flags,          // <-- was Default::default()
            align: NonZeroU64::new(section.align()),
            relocations: Default::default(),
            virtual_address,
            line_info: Default::default(),
        });
```

---

### Step 3: Build to confirm

```bash
cargo build -p objdiff-core
```

Expected: clean build. Check that `SectionFlag` and `SectionFlagSet` are in scope (fix any import errors).

---

## Task 4: x86 architecture — reloc sizes, names, and 16-bit decoder

**Goal:** Implement correct OMF relocation size/name lookup in `ArchX86`, and add per-section bitness tracking so 16-bit OMF code is decoded with iced-x86 in 16-bit mode.

**Files:**
- Modify: `objdiff-core/src/arch/x86.rs`

---

### Step 1: Add imports for OMF types and `BTreeMap`

Open `objdiff-core/src/arch/x86.rs`. At the top, add to existing imports:

```rust
use alloc::collections::BTreeMap;
use object::omf::{FixupLocation, FixupMode};

use crate::obj::SectionFlag;
```

(Insert after the existing `use crate::{...}` block. Check what already exists and avoid duplicates.)

---

### Step 2: Add `section_bitness` field to `ArchX86`

Find the `ArchX86` struct (line ~17):

```rust
#[derive(Debug)]
pub struct ArchX86 {
    arch: Architecture,
    endianness: object::Endianness,
}
```

Replace with:

```rust
#[derive(Debug)]
pub struct ArchX86 {
    arch: Architecture,
    endianness: object::Endianness,
    /// Maps section index → decoder bitness (16, 32, or 64).
    /// Populated in `post_init` from `SectionFlag::Code16Bit`.
    section_bitness: BTreeMap<usize, u32>,
}
```

---

### Step 3: Update `new()` to initialise the new field

Find `ArchX86::new()` (line ~30):

```rust
        Ok(Self { arch, endianness: object.endianness() })
```

Replace with:

```rust
        Ok(Self { arch, endianness: object.endianness(), section_bitness: BTreeMap::new() })
```

---

### Step 4: Add `default_bitness()` helper and refactor `decoder()`

Find `decoder()` (line ~39):

```rust
    fn decoder<'a>(&self, code: &'a [u8], address: u64) -> Decoder<'a> {
        Decoder::with_ip(
            match self.arch {
                Architecture::X86 => 32,
                Architecture::X86_64 => 64,
            },
            code,
            address,
            DecoderOptions::NONE,
        )
    }
```

Replace with:

```rust
    fn default_bitness(&self) -> u32 {
        match self.arch {
            Architecture::X86 => 32,
            Architecture::X86_64 => 64,
        }
    }

    fn decoder<'a>(&self, code: &'a [u8], address: u64, bitness: u32) -> Decoder<'a> {
        Decoder::with_ip(bitness, code, address, DecoderOptions::NONE)
    }
```

---

### Step 5: Fix all call sites of `decoder()` (3 locations)

**Location 1** — `scan_instructions_internal` (line ~104):

```rust
        let mut decoder = self.decoder(code, address);
```

Replace with:

```rust
        let bitness = self.section_bitness.get(&section_index).copied()
            .unwrap_or_else(|| self.default_bitness());
        let mut decoder = self.decoder(code, address, bitness);
```

Also change `_section_index: usize` to `section_index: usize` (remove the underscore):

```rust
    fn scan_instructions_internal(
        &self,
        address: u64,
        code: &[u8],
        section_index: usize,   // <-- remove leading underscore
        relocations: &[Relocation],
        _diff_config: &DiffObjConfig,
    ) -> Result<Vec<InstructionRef>> {
```

**Location 2** — `display_instruction` (line ~206):

```rust
        let mut decoder = self.decoder(resolved.code, resolved.ins_ref.address);
```

Replace with:

```rust
        let bitness = self.section_bitness.get(&resolved.section_index).copied()
            .unwrap_or_else(|| self.default_bitness());
        let mut decoder = self.decoder(resolved.code, resolved.ins_ref.address, bitness);
```

**Location 3** — `infer_function_size` (line ~344):

```rust
        let mut decoder = self.decoder(code, symbol.address);
```

Replace with:

```rust
        let bitness = if section.flags.contains(SectionFlag::Code16Bit) {
            16
        } else {
            self.default_bitness()
        };
        let mut decoder = self.decoder(code, symbol.address, bitness);
```

---

### Step 6: Implement `post_init()` to populate `section_bitness`

Add this method to the `impl Arch for ArchX86` block (before `scan_instructions_internal`):

```rust
    fn post_init(&mut self, sections: &[Section], _symbols: &[Symbol]) {
        self.section_bitness.clear();
        for (index, section) in sections.iter().enumerate() {
            let bitness = if section.flags.contains(SectionFlag::Code16Bit) {
                16
            } else {
                self.default_bitness()
            };
            self.section_bitness.insert(index, bitness);
        }
    }
```

---

### Step 7: Fix `reloc_size()` for OMF — use the `location` field

Find `reloc_size()` (line ~62). It currently looks like:

```rust
    fn reloc_size(&self, flags: RelocationFlags) -> Option<usize> {
        match self.arch {
            Architecture::X86 => match flags {
                ...
                RelocationFlags::Omf { .. } => Some(4), // TODO: use location field
            },
            Architecture::X86_64 => match flags {
                ...
                RelocationFlags::Omf { .. } => None, // TODO: use location field
            },
        }
    }
```

OMF relocation size depends only on `location`, not on the CPU mode. Pull it out before the arch dispatch. Replace the entire function body:

```rust
    fn reloc_size(&self, flags: RelocationFlags) -> Option<usize> {
        // OMF relocation size is determined entirely by the fixup location type,
        // independent of CPU word size.
        if let RelocationFlags::Omf { location, .. } = flags {
            return Some(match location {
                FixupLocation::LowByte | FixupLocation::HighByte => 1,
                FixupLocation::Offset
                | FixupLocation::LoaderOffset
                | FixupLocation::Base => 2,
                FixupLocation::Pointer
                | FixupLocation::Offset32
                | FixupLocation::LoaderOffset32 => 4,
                FixupLocation::Pointer48 => 6,
            });
        }
        match self.arch {
            Architecture::X86 => match flags {
                RelocationFlags::Coff(typ) => match typ {
                    pe::IMAGE_REL_I386_DIR16 | pe::IMAGE_REL_I386_REL16 => Some(2),
                    pe::IMAGE_REL_I386_DIR32 | pe::IMAGE_REL_I386_REL32 => Some(4),
                    _ => None,
                },
                RelocationFlags::Elf(typ) => match typ {
                    elf::R_386_32 | elf::R_386_PC32 => Some(4),
                    elf::R_386_16 => Some(2),
                    _ => None,
                },
                RelocationFlags::Omf { .. } => unreachable!(),
            },
            Architecture::X86_64 => match flags {
                RelocationFlags::Coff(typ) => match typ {
                    pe::IMAGE_REL_AMD64_ADDR32NB | pe::IMAGE_REL_AMD64_REL32 => Some(4),
                    pe::IMAGE_REL_AMD64_ADDR64 => Some(8),
                    _ => None,
                },
                RelocationFlags::Elf(typ) => match typ {
                    elf::R_X86_64_PC32 => Some(4),
                    elf::R_X86_64_64 => Some(8),
                    _ => None,
                },
                RelocationFlags::Omf { .. } => unreachable!(),
            },
        }
    }
```

---

### Step 8: Fix `reloc_name()` for OMF

Find `reloc_name()` (line ~305). Add an OMF early-return before the existing `match self.arch`:

```rust
    fn reloc_name(&self, flags: RelocationFlags) -> Option<&'static str> {
        // OMF reloc names are arch-independent.
        if let RelocationFlags::Omf { location, mode } = flags {
            return Some(match location {
                FixupLocation::LowByte => "OMF_LOW_BYTE",
                FixupLocation::HighByte => "OMF_HIGH_BYTE",
                FixupLocation::Offset => match mode {
                    FixupMode::SelfRelative => "OMF_REL_OFFSET",
                    FixupMode::SegmentRelative => "OMF_OFFSET",
                },
                FixupLocation::LoaderOffset => "OMF_LOADER_OFFSET",
                FixupLocation::Base => "OMF_BASE",
                FixupLocation::Pointer => "OMF_PTR",
                FixupLocation::Offset32 => match mode {
                    FixupMode::SelfRelative => "OMF_REL_OFFSET32",
                    FixupMode::SegmentRelative => "OMF_OFFSET32",
                },
                FixupLocation::LoaderOffset32 => "OMF_LOADER_OFFSET32",
                FixupLocation::Pointer48 => "OMF_PTR48",
            });
        }
        match self.arch {
            Architecture::X86 => match flags {
                RelocationFlags::Coff(typ) => match typ {
                    pe::IMAGE_REL_I386_DIR32 => Some("IMAGE_REL_I386_DIR32"),
                    pe::IMAGE_REL_I386_REL32 => Some("IMAGE_REL_I386_REL32"),
                    _ => None,
                },
                _ => None,
            },
            Architecture::X86_64 => match flags {
                RelocationFlags::Coff(typ) => match typ {
                    pe::IMAGE_REL_AMD64_ADDR64 => Some("IMAGE_REL_AMD64_ADDR64"),
                    pe::IMAGE_REL_AMD64_ADDR32NB => Some("IMAGE_REL_AMD64_ADDR32NB"),
                    pe::IMAGE_REL_AMD64_REL32 => Some("IMAGE_REL_AMD64_REL32"),
                    _ => None,
                },
                _ => None,
            },
        }
    }
```

---

### Step 9: Handle 6-byte inline data in `display_instruction`

`reloc_size` now returns `Some(6)` for `Pointer48`. The inline data display path handles sizes 1, 2, 4 and bails on unknown sizes. Add the 6-byte (48-bit far pointer) case.

Find the inline data display block in `display_instruction` (line ~191):

```rust
            let (mnemonic, imm) = match resolved.ins_ref.size {
                1 => (".byte", resolved.code[0] as u64),
                2 => (".word", self.endianness.read_u16_bytes(resolved.code.try_into()?) as u64),
                4 => (".dword", self.endianness.read_u32_bytes(resolved.code.try_into()?) as u64),
                _ => bail!("Unsupported x86 inline data size {}", resolved.ins_ref.size),
            };
```

Replace with:

```rust
            let (mnemonic, imm) = match resolved.ins_ref.size {
                1 => (".byte", resolved.code[0] as u64),
                2 => (".word", self.endianness.read_u16_bytes(resolved.code.try_into()?) as u64),
                4 => (".dword", self.endianness.read_u32_bytes(resolved.code.try_into()?) as u64),
                6 => {
                    // 48-bit OMF far pointer: 32-bit offset + 16-bit segment selector.
                    let buf: [u8; 6] = resolved.code.try_into()?;
                    let lo = self.endianness.read_u32_bytes(buf[..4].try_into()?);
                    let hi = self.endianness.read_u16_bytes(buf[4..6].try_into()?);
                    (".ptr48", ((hi as u64) << 32) | lo as u64)
                }
                _ => bail!("Unsupported x86 inline data size {}", resolved.ins_ref.size),
            };
```

---

### Step 10: Build and verify

```bash
cargo build -p objdiff-core
```

Expected: clean build, no errors or warnings about the new code.

---

## Task 5: Other arch modules — replace wildcards with explicit OMF arms

**Goal:** Replace silent `_ => None` / `_ => 1` wildcard arms that absorb `RelocationFlags::Omf` with explicit named arms. This makes missing OMF handling visible and grep-able.

**Files:**
- Modify: `objdiff-core/src/arch/ppc/mod.rs`
- Modify: `objdiff-core/src/arch/arm.rs`
- Modify: `objdiff-core/src/arch/arm64.rs`
- Modify: `objdiff-core/src/arch/mips.rs`
- Modify: `objdiff-core/src/arch/superh/mod.rs`

For each file, apply the same pattern. Instructions shown for `ppc/mod.rs`; repeat identically for the others.

---

### Step 1: Fix `ppc/mod.rs` — `reloc_name`

Find `reloc_name` in `arch/ppc/mod.rs` (line ~311). It ends with:

```rust
            _ => None,
        }
    }
```

Replace the final `_ => None` with an explicit arm:

```rust
            RelocationFlags::Omf { .. } => None,
        }
    }
```

---

### Step 2: Fix `ppc/mod.rs` — `data_reloc_size`

Find `data_reloc_size` in `ppc/mod.rs` (line ~339). It ends with:

```rust
            _ => 1,
        }
    }
```

Replace the final `_ => 1` with:

```rust
            RelocationFlags::Omf { .. } => 1,
        }
    }
```

---

### Step 3: Repeat for `arm.rs`

In `arm.rs`:
- `reloc_name` (line ~422): replace `_ => None` with `RelocationFlags::Omf { .. } => None`
- `data_reloc_size` (line ~441): find the final `_ => 1` arm and replace with `RelocationFlags::Omf { .. } => 1`

---

### Step 4: Repeat for `arm64.rs`

In `arm64.rs`:
- `reloc_name` (line ~107): replace `_ => None` with `RelocationFlags::Omf { .. } => None`
- `data_reloc_size` (line ~130): replace final `_ => ...` with `RelocationFlags::Omf { .. } => ...` matching the existing fallback value

---

### Step 5: Repeat for `mips.rs`

In `mips.rs`:
- `reloc_name` (line ~307): replace `_ => None` with `RelocationFlags::Omf { .. } => None`
- `data_reloc_size` (line ~328): replace final `_ => ...` with `RelocationFlags::Omf { .. } => ...`

---

### Step 6: Repeat for `superh/mod.rs`

In `superh/mod.rs`:
- `reloc_name` (line ~135): replace `_ => None` with `RelocationFlags::Omf { .. } => None`
- `data_reloc_size` (line ~181): replace final `_ => ...` with `RelocationFlags::Omf { .. } => ...`

---

### Step 7: Build

```bash
cargo build -p objdiff-core
```

Expected: clean build. All wildcard arms are now named.

---

## Task 6: Unit tests for OMF reloc size and reloc name

**Goal:** Confirm the new OMF logic in `ArchX86` is correct for all `FixupLocation` variants.

**Files:**
- Modify: `objdiff-core/tests/arch_x86.rs`

---

### Step 1: Add the OMF reloc_size tests

Open `objdiff-core/tests/arch_x86.rs`. Add the following at the end of the file (before any closing `}`):

```rust
#[cfg(test)]
mod omf_tests {
    use object::omf::{FixupLocation, FixupMode};
    use objdiff_core::{
        arch::x86::ArchX86,
        diff::DiffObjConfig,
        obj::RelocationFlags,
    };

    fn x86_arch() -> ArchX86 {
        // Create a minimal x86 ArchX86 via its public constructor
        // We need an object::File — use a minimal ELF stub to construct it,
        // or call via test_helpers if available.
        // Workaround: test reloc_size and reloc_name through a trait object.
        use object::{Architecture, Endianness};
        ArchX86::new_for_test(Architecture::I386, Endianness::Little)
    }

    #[test]
    fn omf_reloc_size_low_byte() {
        let arch = x86_arch();
        let flags = RelocationFlags::Omf {
            location: FixupLocation::LowByte,
            mode: FixupMode::SegmentRelative,
        };
        assert_eq!(arch.reloc_size_pub(flags), Some(1));
    }

    #[test]
    fn omf_reloc_size_high_byte() {
        let arch = x86_arch();
        let flags = RelocationFlags::Omf {
            location: FixupLocation::HighByte,
            mode: FixupMode::SegmentRelative,
        };
        assert_eq!(arch.reloc_size_pub(flags), Some(1));
    }

    #[test]
    fn omf_reloc_size_offset_16() {
        let arch = x86_arch();
        for loc in [FixupLocation::Offset, FixupLocation::LoaderOffset, FixupLocation::Base] {
            let flags = RelocationFlags::Omf { location: loc, mode: FixupMode::SegmentRelative };
            assert_eq!(arch.reloc_size_pub(flags), Some(2), "location={loc:?}");
        }
    }

    #[test]
    fn omf_reloc_size_offset_32() {
        let arch = x86_arch();
        for loc in [FixupLocation::Pointer, FixupLocation::Offset32, FixupLocation::LoaderOffset32] {
            let flags = RelocationFlags::Omf { location: loc, mode: FixupMode::SegmentRelative };
            assert_eq!(arch.reloc_size_pub(flags), Some(4), "location={loc:?}");
        }
    }

    #[test]
    fn omf_reloc_size_pointer48() {
        let arch = x86_arch();
        let flags = RelocationFlags::Omf {
            location: FixupLocation::Pointer48,
            mode: FixupMode::SegmentRelative,
        };
        assert_eq!(arch.reloc_size_pub(flags), Some(6));
    }

    #[test]
    fn omf_reloc_name_segment_relative() {
        let arch = x86_arch();
        let cases = [
            (FixupLocation::LowByte,         FixupMode::SegmentRelative, "OMF_LOW_BYTE"),
            (FixupLocation::HighByte,        FixupMode::SegmentRelative, "OMF_HIGH_BYTE"),
            (FixupLocation::Offset,          FixupMode::SegmentRelative, "OMF_OFFSET"),
            (FixupLocation::LoaderOffset,    FixupMode::SegmentRelative, "OMF_LOADER_OFFSET"),
            (FixupLocation::Base,            FixupMode::SegmentRelative, "OMF_BASE"),
            (FixupLocation::Pointer,         FixupMode::SegmentRelative, "OMF_PTR"),
            (FixupLocation::Offset32,        FixupMode::SegmentRelative, "OMF_OFFSET32"),
            (FixupLocation::LoaderOffset32,  FixupMode::SegmentRelative, "OMF_LOADER_OFFSET32"),
            (FixupLocation::Pointer48,       FixupMode::SegmentRelative, "OMF_PTR48"),
        ];
        for (loc, mode, expected) in cases {
            let flags = RelocationFlags::Omf { location: loc, mode };
            assert_eq!(
                arch.reloc_name_pub(flags),
                Some(expected),
                "location={loc:?}, mode={mode:?}"
            );
        }
    }

    #[test]
    fn omf_reloc_name_self_relative() {
        let arch = x86_arch();
        let cases = [
            (FixupLocation::Offset,   FixupMode::SelfRelative, "OMF_REL_OFFSET"),
            (FixupLocation::Offset32, FixupMode::SelfRelative, "OMF_REL_OFFSET32"),
        ];
        for (loc, mode, expected) in cases {
            let flags = RelocationFlags::Omf { location: loc, mode };
            assert_eq!(
                arch.reloc_name_pub(flags),
                Some(expected),
                "location={loc:?}, mode={mode:?}"
            );
        }
    }
}
```

> **Note on test helper methods:** The tests above call `arch.reloc_size_pub()` and `arch.reloc_name_pub()` and `ArchX86::new_for_test()`. These are thin wrappers to be added to `ArchX86` in `arch/x86.rs` (see Step 2). If the existing tests in `arch_x86.rs` already construct `ArchX86` from an `object::File`, follow that pattern instead and remove the `new_for_test` approach.

---

### Step 2: Add test helper methods to `ArchX86`

In `objdiff-core/src/arch/x86.rs`, look for the existing test module at the bottom of the file. Find how existing tests construct `ArchX86`. The existing tests use an actual `object::File` (they parse real ELF bytes).

Add these `#[cfg(test)]` helpers to the `impl ArchX86` block (not inside the trait impl):

```rust
#[cfg(test)]
impl ArchX86 {
    /// Test-only constructor that bypasses object file parsing.
    pub fn new_for_test(architecture: object::Architecture, endianness: object::Endianness) -> Self {
        let arch = match architecture {
            object::Architecture::I386 => Architecture::X86,
            object::Architecture::X86_64 => Architecture::X86_64,
            _ => panic!("unsupported arch in test"),
        };
        Self { arch, endianness, section_bitness: BTreeMap::new() }
    }

    /// Test-only wrapper for `reloc_size` (normally private).
    pub fn reloc_size_pub(&self, flags: RelocationFlags) -> Option<usize> {
        self.reloc_size(flags)
    }

    /// Test-only wrapper for `reloc_name` (normally behind trait).
    pub fn reloc_name_pub(&self, flags: RelocationFlags) -> Option<&'static str> {
        use crate::arch::Arch;
        self.reloc_name(flags)
    }
}
```

---

### Step 3: Run the new tests

```bash
cargo test -p objdiff-core omf_tests 2>&1
```

Expected: all 7 tests pass.

If compilation fails because `ArchX86` is not pub or `reloc_size` is private, adjust the pub visibility of `reloc_size` in `ArchX86` for tests (inside `#[cfg(test)]` helper method is sufficient).

---

### Step 4: Run the full test suite

```bash
cargo test -p objdiff-core
```

Expected: all existing tests pass; no regressions.

---

## Task 7: Integration test — load a real OMF object file

**Goal:** Confirm that `obj::read::parse()` can successfully load a Watcom or Borland `.OBJ` file and produce correct symbols, sections, and relocations.

**Files:**
- Create: `objdiff-core/tests/arch_omf.rs`
- Create: `objdiff-core/tests/` (fixture directory, with a `.OBJ` file)

> **Prerequisite:** You need a real Watcom or Borland 32-bit OMF `.OBJ` file. Place it at `objdiff-core/tests/inputs/test_watcom.obj` (or similar path) before running this task.

---

### Step 1: Create the integration test file

Create `objdiff-core/tests/arch_omf.rs` with:

```rust
//! Integration tests for OMF object file loading.
//!
//! These tests require real OMF .OBJ files in tests/inputs/.
//! If the fixture files are not present, the tests are skipped.

use std::path::Path;

use objdiff_core::{
    diff::DiffObjConfig,
    obj::{RelocationFlags, SectionKind},
};

/// Load an OMF .OBJ file. Returns None if the file doesn't exist (skips test).
fn load_omf(name: &str) -> Option<objdiff_core::obj::Object> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/inputs")
        .join(name);
    if !path.exists() {
        eprintln!("Skipping: fixture not found at {}", path.display());
        return None;
    }
    let config = DiffObjConfig::default();
    let obj = objdiff_core::obj::read::read(path.as_ref(), &config)
        .expect("Failed to parse OMF file");
    Some(obj)
}

/// Sanity-check the loaded object:
/// - Has at least one section
/// - Has at least one symbol
/// - CODE sections have a valid section kind
fn sanity_check(obj: &objdiff_core::obj::Object) {
    assert!(!obj.sections.is_empty(), "No sections found");
    assert!(!obj.symbols.is_empty(), "No symbols found");
    for section in &obj.sections {
        assert!(
            !section.name.is_empty(),
            "Section has empty name"
        );
    }
}

#[test]
fn test_load_watcom_32bit() {
    let Some(obj) = load_omf("test_watcom32.obj") else { return; };
    sanity_check(&obj);

    // Verify that no relocations have the old empty-tuple Omf variant.
    // All OMF relocations must carry a location field.
    for section in &obj.sections {
        for reloc in &section.relocations {
            if let RelocationFlags::Omf { location, mode } = reloc.flags {
                // location and mode must be valid (they always are if deserialized correctly)
                let _ = format!("{location:?} {mode:?}"); // Forces the fields to be used
            }
        }
    }
}

#[test]
fn test_load_watcom_16bit() {
    let Some(obj) = load_omf("test_watcom16.obj") else { return; };
    sanity_check(&obj);

    // At least one CODE section must be marked as 16-bit.
    use objdiff_core::obj::{SectionFlag, SectionKind};
    let has_16bit_code = obj.sections.iter().any(|s| {
        s.kind == SectionKind::Code && s.flags.contains(SectionFlag::Code16Bit)
    });
    assert!(has_16bit_code, "Expected at least one 16-bit code section in a Watcom 16-bit .OBJ");
}

#[test]
fn test_load_borland_32bit() {
    let Some(obj) = load_omf("test_borland32.obj") else { return; };
    sanity_check(&obj);
}
```

---

### Step 2: Add the test to `Cargo.toml`

Open `objdiff-core/Cargo.toml`. Find the `[[test]]` entries (or the `[lib]` section). Add:

```toml
[[test]]
name = "arch_omf"
path = "tests/arch_omf.rs"
```

If no `[[test]]` blocks exist (tests are discovered automatically via `tests/` directory), skip this step — Cargo will find it automatically.

---

### Step 3: Run (will skip if fixtures not present)

```bash
cargo test -p objdiff-core arch_omf
```

Expected: tests run and either pass (if fixtures present) or print skip messages and pass trivially.

---

### Step 4: Add fixtures (manual step)

Copy a Watcom-generated 32-bit `.OBJ` file to `objdiff-core/tests/inputs/test_watcom32.obj`.
Copy a Watcom 16-bit `.OBJ` file to `objdiff-core/tests/inputs/test_watcom16.obj`.
Copy a Borland-generated 32-bit `.OBJ` file to `objdiff-core/tests/inputs/test_borland32.obj`.

Re-run the tests and confirm they pass.

---

## Troubleshooting

### "cannot find value `FixupLocation` in scope"
Add `use object::omf::{FixupLocation, FixupMode};` to the file.

### "mismatched types: expected `RelocationFlags::Omf`, found `RelocationFlags::Omf()`"
There is a stale `Omf()` unit-tuple arm somewhere. Search for `Omf()` and replace with `Omf { .. }`.

```bash
grep -rn "Omf()" objdiff-core/src/
```

### Build fails with "feature `omf` is not a dependency of package `object`"
The local `Cargo.toml` must point to the fork at `E:/CCHyper_object`, not the crates.io `object`. Check `objdiff-core/Cargo.toml` line ~135.

### 16-bit tests fail (16-bit sections not detected)
- Confirm Task 1 is complete (the object fork's `OmfSection::flags()` was updated)
- Confirm Task 3 Step 2 was applied (`map_sections` sets `Code16Bit`)
- Run `cargo build --manifest-path /e/CCHyper_object/Cargo.toml --features omf` to re-build the fork first

### `test_load_watcom_16bit` fails with "Expected at least one 16-bit code section"
The `.OBJ` fixture may be a 32-bit file. Use a file compiled with Watcom's `-ms` (small model) or `-mc` (compact) switch, or with explicit 16-bit segments.

---

## Summary of All Changed Files

| Repo | File | Change |
|---|---|---|
| object fork | `src/common.rs` | Add `SectionFlags::Omf { use32: bool }` |
| object fork | `src/read/omf/section.rs` | Implement `flags()` to return `SectionFlags::Omf` |
| objdiff-core | `src/obj/mod.rs` | Fix `RelocationFlags::Omf`, add `SectionFlag::Code16Bit`, add import |
| objdiff-core | `src/obj/read.rs` | Extract `location/mode` from OMF reloc; set `Code16Bit` flag |
| objdiff-core | `src/arch/x86.rs` | `reloc_size`, `reloc_name`, per-section bitness, 16-bit decoder, `.ptr48` display |
| objdiff-core | `src/arch/ppc/mod.rs` | Explicit `Omf { .. }` arm in `reloc_name`, `data_reloc_size` |
| objdiff-core | `src/arch/arm.rs` | Same |
| objdiff-core | `src/arch/arm64.rs` | Same |
| objdiff-core | `src/arch/mips.rs` | Same |
| objdiff-core | `src/arch/superh/mod.rs` | Same |
| objdiff-core | `tests/arch_x86.rs` | Unit tests for OMF reloc size/name |
| objdiff-core | `tests/arch_omf.rs` | Integration tests for loading real `.OBJ` files |
