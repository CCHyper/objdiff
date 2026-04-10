use alloc::string::String;

use crate::diff::Demangler;

#[cfg(feature = "demangler")]
impl Demangler {
    pub fn demangle(&self, name: &str) -> Option<String> {
        match self {
            Demangler::None => None,
            Demangler::Codewarrior => Self::demangle_codewarrior(name),
            Demangler::Msvc => Self::demangle_msvc(name),
            Demangler::Itanium => Self::demangle_itanium(name),
            Demangler::GnuLegacy => Self::demangle_gnu_legacy(name),
            Demangler::Watcom => Self::demangle_watcom(name),
            Demangler::Borland => Self::demangle_borland(name),
            Demangler::Auto => {
                // Try to guess the mangling scheme from the name prefix.
                if name.starts_with("W?") {
                    Self::demangle_watcom(name)
                } else if name.starts_with('?') {
                    Self::demangle_msvc(name)
                } else if name.starts_with('@') {
                    // Borland member functions start with @ClassName@.
                    // Fall through to other demanglers if Borland parse fails.
                    Self::demangle_borland(name)
                        .or_else(|| Self::demangle_codewarrior(name))
                } else {
                    Self::demangle_codewarrior(name)
                        .or_else(|| Self::demangle_gnu_legacy(name))
                        .or_else(|| Self::demangle_itanium(name))
                        .or_else(|| Self::demangle_borland(name))
                }
            }
        }
    }

    fn demangle_codewarrior(name: &str) -> Option<String> {
        cwdemangle::demangle(name, &cwdemangle::DemangleOptions::default())
    }

    fn demangle_msvc(name: &str) -> Option<String> {
        msvc_demangler::demangle(name, msvc_demangler::DemangleFlags::llvm()).ok()
    }

    fn demangle_itanium(name: &str) -> Option<String> {
        let name = name.trim_start_matches('.');
        cpp_demangle::Symbol::new(name).ok().and_then(|s| s.demangle().ok())
    }

    fn demangle_gnu_legacy(name: &str) -> Option<String> {
        let name = name.trim_start_matches('.');
        gnuv2_demangle::demangle(name, &gnuv2_demangle::DemangleConfig::new()).ok()
    }

    fn demangle_watcom(name: &str) -> Option<String> {
        crate::diff::watcom::demangle(name)
    }

    fn demangle_borland(name: &str) -> Option<String> {
        crate::diff::borland::demangle(name)
    }
}

#[cfg(not(feature = "demangler"))]
impl Demangler {
    pub fn demangle(&self, _name: &str) -> Option<String> { None }
}
