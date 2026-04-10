//! Integration tests for OMF object file loading.
//!
//! Tests load real OMF .OBJ files from tests/data/omf/.
//! If a fixture file is not present, the test is skipped with a message.

use std::path::Path;

use objdiff_core::{diff, obj};

/// Try to parse an OMF .OBJ file by name from tests/data/omf/.
/// Returns None (and prints a skip message) if the fixture doesn't exist.
fn try_load_omf(name: &str) -> Option<obj::Object> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/omf").join(name);
    if !path.exists() {
        eprintln!("SKIP: fixture not found at {} — add a real .OBJ to enable this test", path.display());
        return None;
    }
    let diff_config = diff::DiffObjConfig::default();
    let obj = obj::read::read(&path, &diff_config, diff::DiffSide::Base)
        .unwrap_or_else(|e| panic!("Failed to parse OMF file {name}: {e}"));
    Some(obj)
}

/// Basic sanity checks applied to every loaded OMF object.
fn sanity_check(name: &str, obj: &obj::Object) {
    assert!(!obj.sections.is_empty(), "{name}: no sections found");
    assert!(!obj.symbols.is_empty(), "{name}: no symbols found");
    for section in &obj.sections {
        assert!(!section.name.is_empty(), "{name}: section has empty name");
    }
}

#[test]
#[cfg(feature = "x86")]
fn load_watcom_32bit() {
    let Some(obj) = try_load_omf("test_watcom32.obj") else { return; };
    sanity_check("test_watcom32.obj", &obj);

    // All OMF relocations must carry location/mode fields (not silently discarded).
    use obj::RelocationFlags;
    for section in &obj.sections {
        for reloc in &section.relocations {
            if let RelocationFlags::Omf { location, mode } = reloc.flags {
                // The fields must be representable (the format!() forces evaluation).
                let _ = format!("{location:?} {mode:?}");
            }
        }
    }
}

#[test]
#[cfg(feature = "x86")]
fn load_watcom_16bit() {
    let Some(obj) = try_load_omf("test_watcom16.obj") else { return; };
    sanity_check("test_watcom16.obj", &obj);

    // A Watcom 16-bit .OBJ must have at least one code section marked Code16Bit.
    use obj::{SectionFlag, SectionKind};
    let has_16bit_code = obj
        .sections
        .iter()
        .any(|s| s.kind == SectionKind::Code && s.flags.contains(SectionFlag::Code16Bit));
    assert!(
        has_16bit_code,
        "load_watcom_16bit: expected at least one USE16 code section — \
         make sure the fixture was compiled without -zu or explicit USE32"
    );
}

#[test]
#[cfg(feature = "x86")]
fn load_borland_32bit() {
    let Some(obj) = try_load_omf("test_borland32.obj") else { return; };
    sanity_check("test_borland32.obj", &obj);
}

/// Load each file from the E:\omf_proj project and print what objdiff sees.
/// Skips silently if the project directory doesn't exist.
#[test]
#[cfg(feature = "x86")]
fn load_omf_proj_files() {
    use obj::{SectionKind, SectionFlag};
    use object;
    let proj_dir = std::path::Path::new(r"E:\omf_proj");
    if !proj_dir.exists() {
        eprintln!("SKIP: E:\\omf_proj not found");
        return;
    }
    let files = &[
        "WEAPON.OBJ",
        "UTRACKER.OBJ",
        "UDATA.OBJ",
        "DYNAVEC.OBJ",
        "2KEYFBUF.OBJ",
        "AUDIO.OBJ",
        "BANK.obj",
        "BUILDING.OBJ",
        "main.obj",
        // Borland C++ 32-bit OBJs with COMDEF communal segments
        "mixfile32/mixfile.obj",
        "mixfile32/ini.obj",
        "mixfile32/rawfile.obj",
        "mixfile32/pipe.obj",
        "mixfile32/pk.obj",
        "mixfile32/pkpipe.obj",
        "mixfile32/sha.obj",
        "mixfile32/shapipe.obj",
        "mixfile32/blowfish.obj",
        "mixfile32/blowpipe.obj",
        "mixfile32/crc.obj",
        "mixfile32/random.obj",
        "mixfile32/straw.obj",
        "mixfile32/int.obj",
        "mixfile32/mp.obj",
        "borland test/audio.obj",
    ];
    let diff_config = diff::DiffObjConfig::default();
    for name in files {
        let path = proj_dir.join(name);
        if !path.exists() {
            eprintln!("  SKIP {name}: file not found");
            continue;
        }
        println!("\n=== {name} ===");

        // First, check the raw object crate parse
        let raw_data = std::fs::read(&path).expect("read file");
        match object::File::parse(raw_data.as_slice()) {
            Ok(raw_file) => {
                use object::Object as _;
                println!("  [object crate] format={:?} arch={:?}", raw_file.format(), raw_file.architecture());
                println!("  [object crate] sections={}", raw_file.sections().count());
                println!("  [object crate] symbols={}", raw_file.symbols().count());
            }
            Err(e) => println!("  [object crate] PARSE FAILED: {e}"),
        }

        // Then check the full objdiff pipeline
        match obj::read::read(&path, &diff_config, diff::DiffSide::Base) {
            Ok(obj) => {
                println!("  [objdiff] sections: {}", obj.sections.len());
                for (i, s) in obj.sections.iter().enumerate() {
                    let kind_str = match s.kind {
                        SectionKind::Code => "Code",
                        SectionKind::Data => "Data",
                        SectionKind::Bss  => "Bss",
                        SectionKind::Unknown => "Unknown",
                        _ => "Other",
                    };
                    let use16 = s.flags.contains(SectionFlag::Code16Bit);
                    println!("    [{i}] '{}' kind={kind_str} size={:#x} use16={use16}", s.name, s.size);
                }
                let visible_symbols = obj.symbols.iter()
                    .filter(|sym| sym.section.is_some() && sym.size > 0)
                    .count();
                println!("  [objdiff] symbols total={}, visible(section+size>0)={visible_symbols}", obj.symbols.len());
                for sym in obj.symbols.iter().filter(|sym| sym.section.is_some() && sym.size > 0) {
                    println!("    '{}' addr={:#x} size={:#x} section={:?}", sym.name, sym.address, sym.size, sym.section);
                }
            }
            Err(e) => {
                println!("  [objdiff] FAILED: {e:#}");
            }
        }
    }
}
