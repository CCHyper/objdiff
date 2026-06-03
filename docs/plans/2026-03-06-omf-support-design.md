# OMF Object File Support — Design Document

**Date:** 2026-03-06
**Branch:** `encounter_omf`
**Repos:** `objdiff-core` (E:/CCHyper_objdiff), `object` fork (E:/CCHyper_object)
**Goal:** Production-quality diffing of OMF `.OBJ` files from Watcom and Borland, both 16-bit and 32-bit.

---

## Background

OMF (Object Module Format) is the object file format used by DOS and early Windows compilers:
- **Watcom C/C++** — 16-bit and 32-bit flat model
- **Borland C/C++ / Turbo C** — 16-bit and 32-bit
- **Microsoft MASM/LINK** — older toolchains

An `object` crate fork (`encounter/object`, branch `omf`) already parses OMF files. A WIP commit (`9f37d99`) wired in the dependency and added stubs, but the feature is incomplete and non-functional.

---

## Current State (WIP stubs)

| File | State |
|---|---|
| `Cargo.toml` | Points to local fork at `E:/CCHyper_object` with `"omf"` feature enabled |
| `obj/mod.rs:373` | `RelocationFlags::Omf(/* TODO */)` — empty variant, no fields |
| `obj/read.rs:513` | Discards all OMF relocation data via `{ .. }` wildcard |
| `arch/x86.rs:75,88` | Stub returns `Some(4)` / `None` with `// TODO` |
| `arch/ppc/mod.rs:335` | Added `_ => None` catch-all to fix non-exhaustive match |
| Other archs | Silent wildcard fallbacks, no explicit OMF handling |

---

## Design Decisions

### `RelocationFlags::Omf` — Option A: store `location` + `mode` only

```rust
pub enum RelocationFlags {
    Elf(u32),
    Coff(u16),
    Omf { location: object::omf::FixupLocation, mode: object::omf::FixupMode },
}
```

`FixupLocation` determines byte-size and whether the fixup targets a segment or offset.
`FixupMode` distinguishes PC-relative (`SelfRelative`) from absolute (`SegmentRelative`).

The `frame` and `target` fields from the object crate are already resolved into
`Relocation.target_symbol` and `Relocation.addend` by the object fork — no need to store
them again. Both `FixupLocation` and `FixupMode` are `Copy + Eq + Hash`, so they fit cleanly
into `RelocationFlags`.

**Rejected alternatives:**
- Option B (store all four fields): `frame`/`target` are redundant with `target_symbol`/`addend`
- Option C (flatten to `u8, u8`): loses type safety with no benefit (object crate is already a dep)

### OMF relocation size table

Derived from `FixupLocation` discriminant:

| FixupLocation | Bytes | Notes |
|---|---|---|
| `LowByte` | 1 | Low byte of 16-bit offset |
| `HighByte` | 1 | High byte of 16-bit offset |
| `Offset` | 2 | 16-bit offset |
| `LoaderOffset` | 2 | 16-bit loader-resolved offset |
| `Base` | 2 | 16-bit segment selector |
| `Pointer` | 4 | 16:16 far pointer |
| `Offset32` | 4 | 32-bit offset |
| `LoaderOffset32` | 4 | 32-bit loader-resolved offset |
| `Pointer48` | 6 | 16:32 far pointer |

### OMF relocation names

Human-readable names for `reloc_name()` in `ArchX86`:

| FixupLocation | FixupMode | Name |
|---|---|---|
| `LowByte` | `SegmentRelative` | `"OMF_LOW_BYTE"` |
| `HighByte` | `SegmentRelative` | `"OMF_HIGH_BYTE"` |
| `Offset` | `SegmentRelative` | `"OMF_OFFSET"` |
| `Offset` | `SelfRelative` | `"OMF_REL_OFFSET"` |
| `LoaderOffset` | any | `"OMF_LOADER_OFFSET"` |
| `Base` | any | `"OMF_BASE"` |
| `Pointer` | any | `"OMF_PTR"` |
| `Offset32` | `SegmentRelative` | `"OMF_OFFSET32"` |
| `Offset32` | `SelfRelative` | `"OMF_REL_OFFSET32"` |
| `LoaderOffset32` | any | `"OMF_LOADER_OFFSET32"` |
| `Pointer48` | any | `"OMF_PTR48"` |

### 16-bit OMF support

The `object` fork's `OmfFile::architecture()` always returns `Architecture::I386` regardless of
segment bitness. Per-segment bitness is stored in `OmfSegment::use32: bool` (from the SEGDEF
record's USE32 bit) but is currently not exposed through the generic `SectionFlags` API
(marked `#[allow(unused)]`).

**Detection mechanism:**

1. **Object fork**: Expose `use32` via `SectionFlags::Omf { use32: bool }` in the fork's
   `OmfSection::flags()` implementation.
2. **`obj/mod.rs`**: Add `SectionFlag::Code16Bit` to the `SectionFlagSet`.
3. **`obj/read.rs`**: In `map_sections()`, when `object::SectionFlags::Omf { use32: false }` and
   the section is a code section, set `SectionFlag::Code16Bit`.
4. **`arch/x86.rs`**: `ArchX86` stores `section_bitness: BTreeMap<usize, u32>`.
   - `post_init()` iterates sections, checks `SectionFlag::Code16Bit`, inserts `16` for those
     sections, `32` for all others (when arch is `X86`), `64` for `X86_64`.
   - `decoder()` is refactored to take `bitness: u32` as a parameter.
   - `scan_instructions_internal()` and `display_instruction()` look up bitness from
     `self.section_bitness` using `section_index`.
   - `infer_function_size()` checks `section.flags.contains(SectionFlag::Code16Bit)` directly
     (since it receives `section: &Section`).

This supports mixed 16/32-bit objects (e.g. a single `.OBJ` with both a CODE16 and a CODE32
segment), which is rare but valid in OMF.

### OMF implicit addends — no action needed

The object fork sets `implicit_addend: false` on all OMF relocations (the addend is pre-computed
from `target_displacement + base_addend` and stored in `Relocation.addend`). The guard at the
top of `relocation_override()` in `x86.rs` returns early when `has_implicit_addend()` is false,
so OMF will never reach the `bail!()` path. No changes to `relocation_override()` are needed.

### Non-x86 architectures

OMF is an x86-only format in practice. All non-x86 arch modules currently absorb
`RelocationFlags::Omf` via `_ => None` / `_ => 1` wildcards. These will be replaced with
explicit `RelocationFlags::Omf { .. } => None` / `=> 1` arms to make the gap visible and
grep-able. These stubs are permanent — OMF on non-x86 is not a real use case.

### Symbol visibility for OMF

The generic `map_symbol()` logic handles visibility via `is_global()`, `is_local()`, etc.
The object fork maps OMF symbol types correctly:
- PUBDEF → global public symbol
- LPUBDEF / LEXTDEF → local (file-scoped)
- EXTDEF → external reference

No OMF-specific symbol flag logic is needed beyond what the object crate already provides.
A `BinaryFormat::Omf` branch may be added to the format-check guards in `map_symbol()` if
testing reveals incorrect flag mapping, but this is deferred until integration tests run.

---

## Files to Change

### Object fork (`E:/CCHyper_object`)

| File | Change |
|---|---|
| `src/common.rs` | Add `Omf { use32: bool }` variant to `SectionFlags` enum |
| `src/read/omf/section.rs` | Implement `OmfSection::flags()` to return `SectionFlags::Omf { use32: self.file.segments[self.index].use32 }` |

### `objdiff-core` (`E:/CCHyper_objdiff/objdiff-core/src/`)

| File | Change |
|---|---|
| `obj/mod.rs` | Fix `RelocationFlags::Omf` fields; add `SectionFlag::Code16Bit` |
| `obj/read.rs` | Extract `location, mode` from OMF reloc; set `Code16Bit` for USE16 code sections |
| `arch/x86.rs` | `reloc_size`, `reloc_name`, `data_reloc_size` with OMF; per-section bitness; 16-bit decoder |
| `arch/ppc/mod.rs` | Replace `_ => None` with explicit `RelocationFlags::Omf { .. } => None` |
| `arch/arm.rs` | Replace `_ => None` / `_ => 1` with explicit `RelocationFlags::Omf { .. }` arms |
| `arch/arm64.rs` | Same as arm.rs |
| `arch/mips.rs` | Same as arm.rs |
| `arch/superh/mod.rs` | Same as arm.rs |

### Tests

| File | Change |
|---|---|
| `objdiff-core/tests/arch_x86.rs` | Add unit tests for OMF `reloc_size`, `reloc_name`, `data_reloc_size` |
| `objdiff-core/tests/arch_omf.rs` | New: integration tests loading real Watcom/Borland `.OBJ` files |

---

## Staged Implementation Plan

### Stage 1 — Object fork: expose USE32 bit

**Files:** `E:/CCHyper_object/src/common.rs`, `E:/CCHyper_object/src/read/omf/section.rs`

Add `SectionFlags::Omf { use32: bool }` to the fork and implement it in `OmfSection::flags()`.
This unblocks the 16-bit detection pipeline in stages 2 and 4.

### Stage 2 — Core data model

**Files:** `objdiff-core/src/obj/mod.rs`

- Change `Omf(/* TODO */)` to `Omf { location: object::omf::FixupLocation, mode: object::omf::FixupMode }`
- Add `SectionFlag::Code16Bit` to the `SectionFlag` flags enum

### Stage 3 — Read pipeline

**Files:** `objdiff-core/src/obj/read.rs`

- `map_section_relocations`: change `object::RelocationFlags::Omf { .. }` wildcard to
  `object::RelocationFlags::Omf { location, mode, .. }` and populate the internal variant
- `map_sections`: when iterating sections, check `object::SectionFlags::Omf { use32: false }`
  on code sections and set `SectionFlag::Code16Bit`

### Stage 4 — x86 architecture implementation

**File:** `objdiff-core/src/arch/x86.rs`

- Add `section_bitness: BTreeMap<usize, u32>` field to `ArchX86`
- Refactor `decoder()` to take `bitness: u32` parameter
- Implement `post_init()` to populate `section_bitness` from `SectionFlag::Code16Bit`
- Update `scan_instructions_internal()` and `display_instruction()` to look up bitness
- Update `infer_function_size()` to check section flags directly
- Fix `reloc_size()`: replace `Some(4) // TODO` with size table derived from `location`
- Add `reloc_name()`: return OMF names from the name table above
- Fix `data_reloc_size()`: derive from `location` field

### Stage 5 — Other arch modules

**Files:** `arch/ppc/mod.rs`, `arch/arm.rs`, `arch/arm64.rs`, `arch/mips.rs`, `arch/superh/mod.rs`

Replace all silent wildcard OMF fallbacks with explicit named arms:
```rust
// reloc_name
RelocationFlags::Omf { .. } => None,

// data_reloc_size
RelocationFlags::Omf { .. } => 1,
```

### Stage 6 — Tests

**Files:** `objdiff-core/tests/arch_x86.rs`, `objdiff-core/tests/arch_omf.rs`

Unit tests for all new reloc_size / reloc_name / data_reloc_size combinations.
Integration tests: place test `.OBJ` files from Watcom and Borland in
`objdiff-core/tests/` and load them via `obj::read::parse()`, asserting correct
symbol count, section names, and relocation data.

---

## Key Invariants

- `RelocationFlags::Omf` is `Copy + Eq + Hash` (required by the enum derive)
- `FixupLocation` and `FixupMode` from the object fork are already `Copy + Eq + Hash`
- All OMF relocations have `implicit_addend: false` — `relocation_override` is untouched
- `SectionFlag::Code16Bit` is only set for OMF code sections; ELF/COFF sections are unaffected
- Non-x86 archs return `None`/`1` for OMF relocations — they will never encounter OMF in practice
- The local path `E:/CCHyper_object` is used in both dev and committed `Cargo.toml` files;
  no commit squashing to GitHub URL is needed

---

## Reference: `FixupLocation` → size mapping (Rust)

```rust
fn omf_reloc_size(location: FixupLocation) -> usize {
    match location {
        FixupLocation::LowByte | FixupLocation::HighByte => 1,
        FixupLocation::Offset | FixupLocation::LoaderOffset | FixupLocation::Base => 2,
        FixupLocation::Pointer
        | FixupLocation::Offset32
        | FixupLocation::LoaderOffset32 => 4,
        FixupLocation::Pointer48 => 6,
    }
}
```
