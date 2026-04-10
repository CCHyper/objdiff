//! Watcom C++ name demangler.
//!
//! Implements the Watcom / Open Watcom C++ name mangling scheme used by
//! DOS and Windows compilers (Watcom C++ 10.x, 11.x, Open Watcom 1.x).
//!
//! # Mangled name format
//!
//! ```text
//! W?<name>$[:<class>$]<quals>(<params>)<return>
//! ```
//!
//! | Part       | Meaning                                                   |
//! |-----------|-----------------------------------------------------------|
//! | `W?`      | Mandatory prefix (identifies a Watcom-mangled name)       |
//! | `<name>`  | Identifier or special operator token (e.g. `$ct`, `$nw`) |
//! | `$`       | Name terminator (after a plain identifier)               |
//! | `:<class>$` | Optional enclosing class name                           |
//! | `<quals>` | Calling convention (`n`/`f`/`z`/`e`) + method cv (`x`/`b`) separated by `.` |
//! | `(<params>)` | Parameter type list (see type-encoding table)          |
//! | `<return>` | Return type (or `_` for constructors/destructors)        |
//!
//! # Examples
//!
//! | Mangled                                              | Demangled                                          |
//! |------------------------------------------------------|----------------------------------------------------|
//! | `W?fn_init$n()v`                                     | `void fn_init()`                                   |
//! | `W?$ct:WeaponTypeClass$n(pnxa)_`                     | `WeaponTypeClass::WeaponTypeClass(const char *)`   |
//! | `W?$dt:WeaponTypeClass$n()_`                         | `WeaponTypeClass::~WeaponTypeClass()`              |
//! | `W?$nw:WeaponTypeClass$n(ui)pnv`                     | `void * WeaponTypeClass::operator new(unsigned int)` |
//! | `W?Allowed_Threats$:WeaponTypeClass$n.x()$ThreatType$` | `ThreatType WeaponTypeClass::Allowed_Threats() const` |

use alloc::{format, string::String, vec::Vec};

/// Attempt to demangle a Watcom C++ mangled name.
///
/// Returns `None` if `name` does not begin with `W?` or cannot be parsed.
pub fn demangle(name: &str) -> Option<String> {
    let s = name.strip_prefix("W?")?;
    if s.is_empty() {
        return None;
    }

    let mut user_types: Vec<String> = Vec::new();

    // ── 1. Parse function / operator name ────────────────────────────────────
    // Special operator names start with `$` (e.g. `$ct`, `$nw`).
    // Regular identifiers end at the first `$`.
    let (func_token, s) = if s.starts_with('$') {
        if let Some(result) = parse_special_name(s) {
            result
        } else {
            // Unknown `$`-prefixed token: treat as raw name up to next `$`.
            let end = s[1..].bytes().position(|b| b == b'$').map(|p| p + 1)?;
            let token = &s[..end];
            let rest = &s[end + 1..]; // skip closing `$`
            (token, rest)
        }
    } else {
        let end = dollar_pos(s)?;
        (&s[..end], &s[end + 1..])
    };

    // ── 2. Parse optional class qualifier   :ClassName$ ──────────────────────
    // The class can be a simple name (`ClassName$`) or a templated name
    // (`ClassName$::1n$Arg$`).  We parse it by consuming the class name
    // as a user-defined type token (which handles template suffixes).
    // For `$W` internal tokens, the class may also appear without `:` prefix
    // as an embedded `ClassName$` directly after the token name.
    let (class_name, s) = if let Some(inner) = s.strip_prefix(':') {
        parse_class_qualifier(inner, &mut user_types)?
    } else if func_token.starts_with("$W") && !s.is_empty() && s.as_bytes()[0].is_ascii_uppercase() {
        // `$W` internal token with embedded class: `ClassName$[::tmpl]$$...`
        // Use the full class qualifier parser to handle templates.
        let (class_name, rest) = parse_class_qualifier(s, &mut user_types)?;
        // Strip extra `$` separator between class info and type data.
        let rest = rest.strip_prefix('$').unwrap_or(rest);
        (class_name, rest)
    } else {
        (None, s)
    };

    // ── 3. Parse qualifiers ───────────────────────────────────────────────────
    // Everything up to the opening `(`.
    // Calling conventions: n = near/__cdecl, f = far/__pascal, z = __stdcall, e = __fastcall.
    // Method cv-qualifiers are dot-separated: `.x` = const, `.b` = volatile.
    let paren = s.bytes().position(|b| b == b'(').unwrap_or(s.len());
    let qualifiers = &s[..paren];
    let s = if paren < s.len() { &s[paren..] } else { "" };

    let is_const_method    = qualifiers.contains(".x");
    let is_volatile_method = qualifiers.contains(".b");

    // ── 4. Parse parameter list ───────────────────────────────────────────────
    if !s.starts_with('(') {
        // Data symbol or unrecognised form.
        // For Watcom C++ data symbols the mangled suffix after the name is
        // the variable's type code (e.g. `W?Count$ui` → `unsigned int Count`).
        // Try to parse `qualifiers` as a single type; if it succeeds, prepend it.
        let data_type = parse_single_type(qualifiers, &mut user_types)
            .map(|(t, _)| t)
            .filter(|t| !t.is_empty());
        return build_simple(func_token, class_name.as_deref(), data_type.as_deref());
    }
    let close = s.bytes().position(|b| b == b')')?;
    let param_data = &s[1..close];
    let after_params = &s[close + 1..];

    // In Watcom C++, `()` (empty params) means `(void)` — display explicitly.
    let params = if param_data.is_empty() {
        vec!["void".to_string()]
    } else {
        parse_type_list(param_data, &mut user_types)
    };

    // ── 5. Parse return type ──────────────────────────────────────────────────
    let return_str = parse_single_type(after_params, &mut user_types)
        .map(|(t, _)| t)
        .unwrap_or_default();

    // ── 6. Assemble output ────────────────────────────────────────────────────
    build_output(func_token, class_name.as_deref(), is_const_method, is_volatile_method, &params, &return_str)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Return the index of the first `$` byte in `s`, or `None`.
fn dollar_pos(s: &str) -> Option<usize> {
    s.bytes().position(|b| b == b'$')
}

/// Parse the class qualifier section after the leading `:`.
///
/// Handles simple class names (`ClassName$`), templated classes
/// (`ClassName$::1n$Arg$`), and chained nested scopes
/// (`:InnerClass$:OuterClass$` → `OuterClass::InnerClass`).
///
/// Returns `(Some(qualified_name), remaining_input)`.
fn parse_class_qualifier<'a>(
    mut s: &'a str,
    user_types: &mut Vec<String>,
) -> Option<(Option<String>, &'a str)> {
    let mut scopes: Vec<String> = Vec::new();
    loop {
        let end = dollar_pos(s)?;
        let class_name = &s[..end];
        let after_dollar = &s[end + 1..]; // skip closing `$`

        // Register BASE name for back-references (before template).
        if !user_types.iter().any(|t| t == class_name) {
            user_types.push(class_name.to_string());
        }

        // Check for template suffix `::` followed by arg markers.
        let (class_str, rest) = if let Some(tmpl) = after_dollar.strip_prefix("::") {
            parse_template_suffix(class_name, tmpl, user_types)
                .unwrap_or_else(|| (class_name.to_string(), after_dollar))
        } else {
            (class_name.to_string(), after_dollar)
        };

        scopes.push(class_str);

        // Check for another chained scope `:NextClass$`.
        if rest.starts_with(':') {
            s = &rest[1..];
            continue;
        }

        // After the last scope, there should be a terminating `$` before quals.
        // But it may have already been consumed as the closing `$` of the last
        // template arg.  If `rest` starts with `$`, skip it.
        let rest = rest.strip_prefix('$').unwrap_or(rest);

        // Build the full qualified name: outermost first.
        scopes.reverse();
        let full = scopes.join("::");
        return Some((Some(full), rest));
    }
}

/// Try to match a known special-name token at the start of `s`.
/// Returns `(token, remaining)` on success.
///
/// Tokens are tried longest-first so that `$nwa` beats `$nw`, etc.
fn parse_special_name(s: &str) -> Option<(&str, &str)> {
    debug_assert!(s.starts_with('$'));
    // Sorted longest → shortest within each prefix group.
    const SPECIALS: &[&str] = &[
        // Compound assignment / new[] / delete[]
        "$nwa", "$dla",
        "$apl", "$ami", "$aml", "$adv", "$amd",
        "$als", "$ars",
        "$aan", "$aor", "$aer",
        // Ctor / dtor / allocation
        "$ct", "$dt", "$nw", "$dl",
        // Arithmetic
        "$pl", "$mi", "$ml", "$dv", "$md",
        // Shift
        "$ls", "$rs",
        // Comparison
        "$eq", "$ne", "$le", "$lt", "$ge", "$gt",
        // Bitwise
        "$an", "$or", "$er",
        // Unary / misc
        "$nt", "$co",
        "$as", "$pp", "$mm", "$cm",
        "$cl", "$vc", "$rf",
        "$cn",
        // Type conversion operator
        "$op",
        // Additional operators
        "$od",
    ];
    for &sp in SPECIALS {
        if let Some(rest) = s.strip_prefix(sp) {
            return Some((sp, rest));
        }
    }

    // Watcom compiler-generated internal names: `$W<tag><digits>`.
    // Examples: `$Wvf0mo12` (vtable thunk), `$Wda018` (default-arg thunk),
    //           `$Wsi0hg` (static-init wrapper).
    // Consume everything up to the next `$` or `:`, then skip the `$`
    // name terminator if present (`:` is kept for class parsing).
    if s.starts_with("$W") {
        let end = s[2..]
            .bytes()
            .position(|b| b == b'$' || b == b':')
            .map(|p| p + 2)
            .unwrap_or(s.len());
        let token = &s[..end];
        let rest = &s[end..];
        // Skip trailing `$` name terminator (but not `:` which starts class).
        let rest = rest.strip_prefix('$').unwrap_or(rest);
        return Some((token, rest));
    }

    None
}

/// Map a special operator token to its C++ spelling.
/// Returns `None` for `$ct` / `$dt` which require the class name.
fn operator_name(token: &str) -> Option<&'static str> {
    match token {
        "$nw"  => Some("operator new"),
        "$nwa" => Some("operator new[]"),
        "$dl"  => Some("operator delete"),
        "$dla" => Some("operator delete[]"),
        "$pl"  => Some("operator+"),
        "$mi"  => Some("operator-"),
        "$ml"  => Some("operator*"),
        "$dv"  => Some("operator/"),
        "$md"  => Some("operator%"),
        "$ls"  => Some("operator<<"),
        "$rs"  => Some("operator>>"),
        "$eq"  => Some("operator=="),
        "$ne"  => Some("operator!="),
        "$lt"  => Some("operator<"),
        "$le"  => Some("operator<="),
        "$gt"  => Some("operator>"),
        "$ge"  => Some("operator>="),
        "$an"  => Some("operator&"),
        "$or"  => Some("operator|"),
        "$er"  => Some("operator^"),
        "$nt"  => Some("operator!"),
        "$co"  => Some("operator~"),
        "$as"  => Some("operator="),
        "$pp"  => Some("operator++"),
        "$mm"  => Some("operator--"),
        "$cm"  => Some("operator,"),
        "$cl"  => Some("operator()"),
        "$vc"  => Some("operator[]"),
        "$rf"  => Some("operator->"),
        "$cn"  => Some("operator cast"),
        "$op"  => Some("operator cast"),
        "$od"  => Some("operator[]"),
        _ => None,
    }
}

/// Map a `$W` compiler-internal token to its readable name.
///
/// Watcom generates internal symbols with names like `$Wsi0hg`, `$Wts0en`.
/// The two characters after `$W` identify the kind; trailing digits are an
/// index that makes each instance unique.  Only tokens whose readable form
/// is more useful than the raw indexed name are mapped here.
fn watcom_internal_name(token: &str) -> Option<&'static str> {
    if !token.starts_with("$W") || token.len() < 4 {
        return None;
    }
    match &token[2..4] {
        "si" => Some("__staticinit"),
        _ => None,
    }
}

/// Returns `true` if `c` is the first byte of a valid primitive-type encoding.
///
/// Used to distinguish primitive-type groups (`$ipnxa$` → `int, const char *`)
/// from user-defined-type tokens (`$UnitType$`) inside a parameter list.
fn is_primitive_type_start(c: u8) -> bool {
    matches!(
        c,
        b'a' | b'v' | b'b' | b'c' | b'f' | b'd' | b'e' | b'i' | b'u' | b's' | b'l'
            | b'x' | b'y' | b'p' | b'r' | b'm' | b'_'
    )
}

/// Parse every consecutive type token in `s` and return them as a `Vec<String>`.
///
/// `void` (`v`) appearing as a top-level parameter type is preserved as
/// `"void"`, so that `(v)` produces `["void"]` → displayed as `(void)`.
/// An empty parameter list `()` still produces `[]` → displayed as `()`.
///
/// Handles two special `$`-prefixed forms that can appear between types:
///
/// * **`$$`** — bare separator; the leading `$` is skipped so that the
///   following `$TypeName$` or primitive group is parsed normally.
/// * **`$<prims>$`** — a Watcom *primitive-type group*: a `$`-delimited
///   sequence of primitive-type codes (e.g. `$ipnxa$` encodes `int,
///   const char *`).  The inner string is expanded in-place.
fn parse_type_list(mut s: &str, user_types: &mut Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    loop {
        if s.is_empty() {
            break;
        }

        // Handle `$`-prefixed group tokens before delegating to parse_single_type.
        if let Some(b'$') = s.as_bytes().first() {
            let c1 = s.as_bytes().get(1).copied();

            if c1 == Some(b'$') {
                // `$$` → bare separator: skip one `$` and continue.
                s = &s[1..];
                continue;
            }

            if c1.is_some_and(is_primitive_type_start) {
                // `$<prims>$` → primitive-type group.
                // Find the closing `$`, parse the inner string as a nested type list.
                let inner_start = &s[1..]; // skip opening `$`
                if let Some(close) = inner_start.bytes().position(|x| x == b'$') {
                    let inner = &inner_start[..close];
                    // Keep the closing `$` — in Watcom mangling it doubles as the
                    // opening `$` of the next token (shared delimiter).
                    // e.g. `$ipnxa$AnimType$` → inner="ipnxa", next="$AnimType$".
                    let after_group = &inner_start[close..];
                    let sub = parse_type_list(inner, user_types);
                    result.extend(sub);
                    s = after_group;
                    continue;
                }
                // No closing `$` found — fall through and let parse_single_type
                // handle (or fail on) the leading `$`.
            }
        }

        match parse_single_type(s, user_types) {
            Some((t, rest)) => {
                if rest.len() == s.len() {
                    break; // guard against infinite loop
                }
                // Push every non-empty type, including "void".
                // A lone `v` in the param list means the function was
                // declared as `(void)` in the source; preserve that.
                if !t.is_empty() {
                    result.push(t);
                }
                s = rest;
            }
            None => break,
        }
    }
    result
}

/// Parse one type token from the front of `s`.
///
/// Returns `(type_string, remaining_input)` on success, or `None` if the
/// leading character is not a recognised type code.
///
/// # Type-encoding table
///
/// | Code      | C++ type              |
/// |-----------|-----------------------|
/// | `_`       | *(no type — ctor/dtor)* |
/// | `a`       | `char`                |
/// | `v`       | `void`                |
/// | `b`       | `bool`                |
/// | `c`       | `char`                |
/// | `sc`      | `signed char`         |
/// | `uc`      | `unsigned char`       |
/// | `i`       | `int`                 |
/// | `s`       | `short`               |
/// | `l`       | `long`                |
/// | `ui`      | `unsigned int`        |
/// | `us`      | `unsigned short`      |
/// | `ul`      | `unsigned long`       |
/// | `f`       | `float`               |
/// | `d`       | `double`              |
/// | `ld`      | `long double`         |
/// | `e`       | `...`                 |
/// | `xa`      | `const char`          |
/// | `x`+*T*   | `const` *T*           |
/// | `pn`+*T*  | *T* `*`               |
/// | `pf`+*T*  | *T* `*`  (far ptr)    |
/// | `rn`+*T*  | *T* `&`               |
/// | `rf`+*T*  | *T* `&`  (far ref)    |
/// | `$`*Name*`$` | user-defined type  |
/// | `$`*N*    | back-reference (*N* = 1-based digit) |
fn parse_single_type<'a>(s: &'a str, user_types: &mut Vec<String>) -> Option<(String, &'a str)> {
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();

    match b[0] {
        // ── No-type (constructor / destructor return) ─────────────────────────
        b'_' => Some(("".to_string(), &s[1..])),

        // ── Single-char primitives ────────────────────────────────────────────
        b'a' => Some(("char".to_string(),   &s[1..])),
        b'v' => Some(("void".to_string(),   &s[1..])),
        b'b' => Some(("bool".to_string(),   &s[1..])),
        b'c' => Some(("char".to_string(),   &s[1..])),
        b'f' => Some(("float".to_string(),  &s[1..])),
        b'd' => Some(("double".to_string(), &s[1..])),
        b'e' => Some(("...".to_string(),    &s[1..])),
        b'i' => Some(("int".to_string(),    &s[1..])),

        // ── Unsigned types: `u` is a prefix modifier that takes exactly
        //    ONE following type code ─────────────────────────────────────────
        // `u` is NOT greedy past the single type that follows it — any extra
        // primitive letters belong to the NEXT parameter.  For example
        // `(usi)` is `us` (unsigned short) + `i` (int), not a single
        // "unsigned short int" that swallows the trailing `i`.
        b'u' => match b.get(1) {
            Some(b'i') => Some(("unsigned int".to_string(),   &s[2..])),
            Some(b'c') => Some(("unsigned char".to_string(),  &s[2..])),
            Some(b's') => Some(("unsigned short".to_string(), &s[2..])),
            Some(b'l') => Some(("unsigned long".to_string(),  &s[2..])),
            _ => None,
        },

        // ── short / signed char: s* ───────────────────────────────────────────
        // Bare `s` = short.  `sc` is a combined code for "signed char"
        // (distinct from plain `c` = char).  `s` does NOT swallow a following
        // `i`/`ui`/etc. — `(si)` is short+int, `(sui)` is short+unsigned int.
        b's' => match b.get(1) {
            Some(b'c') => Some(("signed char".to_string(), &s[2..])),
            _ => Some(("short".to_string(), &s[1..])),
        },

        // ── long types: l* ────────────────────────────────────────────────────
        // Bare `l` = long.  `ld` is a combined code for "long double".  `l`
        // does NOT swallow a following `i`/`ui`/etc. — `(li)` is long+int,
        // `(lui)` is long+unsigned int.
        b'l' => match b.get(1) {
            Some(b'd') => Some(("long double".to_string(), &s[2..])),
            _ => Some(("long".to_string(), &s[1..])),
        },

        // ── const qualifier: x ────────────────────────────────────────────────
        // Watcom uses east-const (post-fix) style: `T const` not `const T`.
        // `xa` is a special combined encoding for `char const`.
        b'x' => match b.get(1) {
            Some(b'a') => Some(("char const".to_string(), &s[2..])),
            _ => {
                let (inner, rest) = parse_single_type(&s[1..], user_types)?;
                if inner.is_empty() {
                    Some(("const".to_string(), rest))
                } else if inner.ends_with(']') {
                    // `const` qualifies the element type, not the array.
                    // Move `const` before the brackets: `T[]` → `T const[]`.
                    if let Some(bracket_pos) = inner.rfind('[') {
                        let elem = &inner[..bracket_pos];
                        let brackets = &inner[bracket_pos..];
                        Some((format!("{elem} const{brackets}"), rest))
                    } else {
                        Some((format!("{inner} const"), rest))
                    }
                } else {
                    Some((format!("{inner} const"), rest))
                }
            }
        },

        // ── volatile qualifier: y ─────────────────────────────────────────────
        b'y' => {
            let (inner, rest) = parse_single_type(&s[1..], user_types)?;
            if inner.is_empty() {
                Some(("volatile".to_string(), rest))
            } else {
                Some((format!("{inner} volatile"), rest))
            }
        },

        // ── Pointer: p + modifier + type ─────────────────────────────────────
        // Modifiers: n = near, f = far, h = huge — shown explicitly in the
        // demangled output to match Watcom's own demangler style.
        b'p' => {
            let (qual, rest) = match b.get(1) {
                Some(b'n') => ("near", &s[2..]),
                Some(b'f') => ("far",  &s[2..]),
                Some(b'h') => ("huge", &s[2..]),
                _ => return None,
            };
            let (inner, rest2) = parse_single_type(rest, user_types)?;
            if inner.is_empty() {
                Some((format!("void {qual} *"), rest2))
            } else {
                Some((format!("{inner} {qual} *"), rest2))
            }
        },

        // ── Reference: r + modifier + type ───────────────────────────────────
        b'r' => {
            let (qual, rest) = match b.get(1) {
                Some(b'n') => ("near", &s[2..]),
                Some(b'f') => ("far",  &s[2..]),
                _ => return None,
            };
            let (inner, rest2) = parse_single_type(rest, user_types)?;
            Some((format!("{inner} {qual} &"), rest2))
        },

        // ── Member pointer: m + class_type + field_type ───────────────────────
        // Simplified: skip the class type and render as `TYPE *`.
        b'm' => {
            let (_, s1) = parse_single_type(&s[1..], user_types)?;
            let (inner, s2) = parse_single_type(s1, user_types)?;
            Some((format!("{inner} *"), s2))
        },

        // ── Near / huge memory-model qualifier: n, h ─────────────────────────
        // In Watcom C++ data-symbol type suffixes, `n` (near) and `h` (huge)
        // are the *addressing-mode* qualifiers for the variable — they are NOT
        // pointer prefixes.  Actual pointer types use `pn`/`pf`/`ph` (handled
        // by the `b'p'` case above).
        //
        // `ni`          → int           (near int, no `*`)
        // `nx$Type$`    → const Type    (near const Type, no `*`)
        // `n[]xul`      → const unsigned long[]  (near array, no `*`)
        //
        // Simply consume the qualifier and delegate to the inner type.
        b'n' | b'h' => parse_single_type(&s[1..], user_types),

        // ── Array type: [dim]elem ─────────────────────────────────────────────
        // Watcom encodes array dimensions as `[N]` (with a decimal number) or
        // `[]` for unbounded arrays. The element type follows the closing `]`.
        // Produces `elem[N]` or `elem[]` to mirror C++ declaration syntax.
        b'[' => {
            let end = s.bytes().position(|x| x == b']')?;
            let dim = &s[1..end];       // content between `[` and `]`
            let after = &s[end + 1..]; // everything after `]`
            let (elem, rest) = parse_single_type(after, user_types)?;
            if dim.is_empty() {
                Some((format!("{elem}[]"), rest))
            } else {
                Some((format!("{elem}[{dim}]"), rest))
            }
        },

        // ── User-defined type or back-reference: $ ────────────────────────────
        b'$' => {
            let c1 = b.get(1).copied();
            if c1.is_some_and(|c| c.is_ascii_digit()) {
                // Back-reference: `$1` = first registered user type, `$2` = second, etc.
                // `$0` is a self-reference to the enclosing class (also user_types[0]).
                let digit = (c1.unwrap() - b'0') as usize;
                let idx = if digit == 0 { 0 } else { digit - 1 };
                let base = user_types.get(idx)?.clone();
                let after = &s[2..];

                // Back-references can have their own template suffix.
                let (type_str, after) =
                    if let Some(tmpl) = after.strip_prefix("::") {
                        parse_template_suffix(&base, tmpl, user_types)
                            .unwrap_or_else(|| (base.clone(), after))
                    } else {
                        (base, after)
                    };
                Some((type_str, after))
            } else if c1.is_some() {
                // Named user type: `$TypeName$`
                let name_slice = &s[1..];
                let end = name_slice.bytes().position(|x| x == b'$')?;
                let type_name = &name_slice[..end];
                let after = &name_slice[end + 1..]; // skip closing `$`

                // Register BASE name for back-references (before template).
                if !user_types.iter().any(|t| t == type_name) {
                    user_types.push(type_name.to_string());
                }

                // Try to parse a template suffix.
                let (type_str, after) =
                    if let Some(tmpl) = after.strip_prefix("::") {
                        parse_template_suffix(type_name, tmpl, user_types)
                            .unwrap_or_else(|| (type_name.to_string(), after))
                    } else {
                        (type_name.to_string(), after)
                    };

                Some((type_str, after))
            } else {
                None
            }
        },

        _ => None,
    }
}

/// Parse a Watcom template suffix that follows `::` after a user-type name.
///
/// Template arguments are self-describing, each prefixed by a marker:
///
/// * `0` — **integer** template parameter: followed by a base-32 encoded
///   value and a sign character (`Z`/`z` = positive, `Y`/`y` = negative).
/// * `1` — **type** template parameter: followed by a type encoding
///   (parsed by `parse_single_type`).
///
/// Arguments repeat until neither `0` nor `1` is found. A trailing `$`
/// (template terminator) is consumed if present.
///
/// Returns `Some((full_type, remaining))` on success, `None` on failure.
fn parse_template_suffix<'a>(
    base_name: &str,
    tmpl: &'a str,
    user_types: &mut Vec<String>,
) -> Option<(String, &'a str)> {
    let mut rest = tmpl;
    let mut args: Vec<String> = Vec::new();
    loop {
        match rest.as_bytes().first() {
            Some(b'0') => {
                // Integer template parameter.
                rest = &rest[1..];
                let (value, new_rest) = parse_base32_int(rest)?;
                args.push(format!("{value}"));
                rest = new_rest;
            }
            Some(b'1') => {
                // Type template parameter.
                rest = &rest[1..];
                let (arg, new_rest) = parse_single_type(rest, user_types)?;
                if !arg.is_empty() {
                    args.push(arg);
                }
                rest = new_rest;
            }
            _ => break,
        }
    }
    // Consume trailing `$` template terminator if present.
    if rest.starts_with('$') {
        rest = &rest[1..];
    }
    let type_str = if args.is_empty() {
        base_name.to_string()
    } else {
        format!("{}<{}>", base_name, args.join(", "))
    };
    Some((type_str, rest))
}

/// Parse a base-32 encoded integer with a sign suffix.
///
/// Format: `<base32_digits><sign_char>` where:
/// * Base-32 digits use `0-9` (values 0–9) and `a-v` / `A-V` (values 10–31).
///   Characters with digit value ≥ 32 terminate the number.
/// * Sign char: `Z`/`z` = positive, `Y`/`y` = negative.
///
/// Returns `(value, remaining_input)`.
fn parse_base32_int(s: &str) -> Option<(i64, &str)> {
    let mut val: i64 = 0;
    let mut consumed = 0;
    for &b in s.as_bytes() {
        let dig = match b {
            b'0'..=b'9' => (b - b'0') as i64,
            b'A'..=b'Z' => (b - b'A' + 10) as i64,
            b'a'..=b'z' => (b - b'a' + 10) as i64,
            _ => break,
        };
        if dig >= 32 {
            break;
        }
        val = val * 32 + dig;
        consumed += 1;
    }
    if consumed == 0 {
        return None;
    }
    let rest = &s[consumed..];
    let sign_char = rest.as_bytes().first()?;
    match sign_char {
        b'Z' | b'z' => Some((val, &rest[1..])),
        b'Y' | b'y' => Some((-val, &rest[1..])),
        _ => None,
    }
}

/// Build a simple demangled name for data symbols (no parameter list).
///
/// `data_type` is the parsed type prefix (e.g. `"int"`, `"unsigned int *"`).
/// When present it is prepended as `"type name"` or `"type Class::name"`.
fn build_simple(func_token: &str, class_name: Option<&str>, data_type: Option<&str>) -> Option<String> {
    let mut result = String::new();

    // In C/C++, array brackets bind to the *declarator* (the name), not to
    // the type specifier.  `const int arr[]` is correct; `const int[] arr`
    // is not valid C++.  Split any trailing `[…]` off the type string so we
    // can append it after the full name instead.
    let (type_prefix, array_suffix): (&str, &str) = match data_type {
        Some(ty) if ty.ends_with(']') => {
            match ty.rfind('[') {
                Some(pos) => (&ty[..pos], &ty[pos..]),
                None      => (ty, ""),
            }
        }
        Some(ty) => (ty, ""),
        None     => ("", ""),
    };

    if !type_prefix.is_empty() {
        result.push_str(type_prefix);
        result.push(' ');
    }
    if let Some(cls) = class_name {
        result.push_str(cls);
        result.push_str("::");
    }
    if let Some(op) = operator_name(func_token) {
        result.push_str(op);
    } else if let Some(wname) = watcom_internal_name(func_token) {
        result.push_str(wname);
    } else {
        result.push_str(func_token);
    }
    result.push_str(array_suffix);
    Some(result)
}

/// Assemble the final demangled string from the parsed components.
fn build_output(
    func_token: &str,
    class_name: Option<&str>,
    is_const_method: bool,
    is_volatile_method: bool,
    params: &[String],
    return_type: &str,
) -> Option<String> {
    let is_ctor = func_token == "$ct";
    let is_dtor = func_token == "$dt";
    let is_conv = func_token == "$op";

    let mut result = String::new();

    // Return type (omitted for constructors, destructors, and conversion operators).
    if !is_ctor && !is_dtor && !is_conv && !return_type.is_empty() {
        result.push_str(return_type);
        result.push(' ');
    }

    // Class qualifier.
    if let Some(cls) = class_name {
        result.push_str(cls);
        result.push_str("::");
    }

    // Function / operator name.
    if is_ctor {
        result.push_str(class_name.unwrap_or(func_token));
    } else if is_dtor {
        result.push('~');
        result.push_str(class_name.unwrap_or(func_token));
    } else if is_conv {
        // Conversion operator: `operator <return_type>()`
        result.push_str("operator ");
        result.push_str(return_type);
    } else if let Some(op) = operator_name(func_token) {
        result.push_str(op);
    } else if let Some(wname) = watcom_internal_name(func_token) {
        result.push_str(wname);
    } else {
        result.push_str(func_token);
    }

    // Parameter list.
    result.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            result.push_str(", ");
        }
        result.push_str(p);
    }
    result.push(')');

    // Method cv-qualifiers.
    if is_const_method {
        result.push_str(" const");
    }
    if is_volatile_method {
        result.push_str(" volatile");
    }

    Some(result)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! demangle_eq {
        ($mangled:expr, $expected:expr) => {
            assert_eq!(
                demangle($mangled).as_deref(),
                Some($expected),
                "demangling {:?}",
                $mangled
            );
        };
    }

    macro_rules! demangle_none {
        ($mangled:expr) => {
            assert_eq!(demangle($mangled), None, "expected None for {:?}", $mangled);
        };
    }

    #[test]
    fn test_plain_function() {
        demangle_eq!("W?fn_init$n()v", "void fn_init(void)");
    }

    #[test]
    fn test_constructor() {
        demangle_eq!(
            "W?$ct:WeaponTypeClass$n(pnxa)_",
            "WeaponTypeClass::WeaponTypeClass(char const near *)"
        );
    }

    #[test]
    fn test_destructor() {
        demangle_eq!(
            "W?$dt:WeaponTypeClass$n()_",
            "WeaponTypeClass::~WeaponTypeClass(void)"
        );
    }

    #[test]
    fn test_operator_new() {
        demangle_eq!(
            "W?$nw:WeaponTypeClass$n(ui)pnv",
            "void near * WeaponTypeClass::operator new(unsigned int)"
        );
    }

    #[test]
    fn test_const_method_user_return() {
        demangle_eq!(
            "W?Allowed_Threats$:WeaponTypeClass$n.x()$ThreatType$",
            "ThreatType WeaponTypeClass::Allowed_Threats(void) const"
        );
    }

    #[test]
    fn test_no_class() {
        demangle_eq!("W?SomeFunc$n(i)v", "void SomeFunc(int)");
    }

    #[test]
    fn test_multiple_params() {
        demangle_eq!("W?Add$n(ii)i", "int Add(int, int)");
    }

    #[test]
    fn test_unsigned_int_param() {
        demangle_eq!("W?Foo$n(ui)v", "void Foo(unsigned int)");
    }

    #[test]
    fn test_pointer_param() {
        demangle_eq!("W?Bar$n(pnv)v", "void Bar(void near *)");
    }

    #[test]
    fn test_not_watcom() {
        demangle_none!("_SomeFunction");
        demangle_none!("?SomeFunction@@YAHXZ");
        demangle_none!("");
    }

    #[test]
    fn test_data_symbol_no_parens() {
        // Data symbols with no type suffix — just the name.
        demangle_eq!("W?gSomeGlobal$", "gSomeGlobal");
    }

    #[test]
    fn test_data_symbol_int_type() {
        // `i` suffix → `int` prefix.
        demangle_eq!("W?Count$i", "int Count");
    }

    #[test]
    fn test_data_symbol_ptr_type() {
        // `pni` → near pointer to int → `int near *`.
        demangle_eq!("W?pTable$pni", "int near * pTable");
    }

    #[test]
    fn test_stdcall() {
        // z = __stdcall — calling convention not shown in output.
        // `(v)` means the source was declared `(void)` — preserve it.
        demangle_eq!("W?Init$z(v)v", "void Init(void)");
    }

    #[test]
    fn test_module_init() {
        // Names starting with '.' are internal module-init helpers.
        demangle_eq!("W?.mod_init$npn()v", "void .mod_init(void)");
    }

    #[test]
    fn test_primitive_group_in_params() {
        // `$ipnxa$` is a primitive-type group: int + const char *
        demangle_eq!(
            "W?Foo$n($ipnxa$)v",
            "void Foo(int, char const near *)"
        );
    }

    #[test]
    fn test_double_dollar_separator() {
        // A lone `$` between two user-type tokens is a bare separator.
        // In the raw string, closing-`$` of `Foo` + separator-`$` + opening-`$`
        // of `Bar` looks like `$Foo$` + `$` + `$Bar$` = `$Foo$$Bar$`.
        // But the closing `$` of Foo and the opening `$` of Bar already share
        // the boundary, so a *real* bare separator produces the triple `$$$`.
        // The simpler observable form: a double-`$` where the FIRST is surplus.
        demangle_eq!(
            "W?Baz$n($$Foo$$Bar$)v",
            "void Baz(Foo, Bar)"
        );
    }

    #[test]
    fn test_unittype_constructor() {
        // Variant with bare primitive tokens (no $..$ group around ipnxa).
        demangle_eq!(
            "W?$ct:UnitTypeClass$n($UnitType$ipnxa$AnimType$$RemapType$iiiiiiiiiiiiiiiiiiiii$MissionType$)_",
            "UnitTypeClass::UnitTypeClass(UnitType, int, char const near *, AnimType, RemapType, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, MissionType)"
        );
    }

    #[test]
    fn test_unittype_constructor_with_primitive_group() {
        // The actual mangled symbol from UDATA.OBJ uses $ipnxa$ (a primitive-type
        // group) where the closing $ of the group doubles as the opening $ of the
        // next user-type token $AnimType$.  This "shared delimiter" pattern must
        // be handled by NOT consuming the closing $ of the primitive group.
        demangle_eq!(
            "W?$ct:UnitTypeClass$n($UnitType$$ipnxa$AnimType$$$RemapType$$iiiiiiiiiiiiiiiiiiiii$MissionType$$)_",
            "UnitTypeClass::UnitTypeClass(UnitType, int, char const near *, AnimType, RemapType, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, int, MissionType)"
        );
    }

    #[test]
    fn test_data_symbol_near_const_user_type() {
        // W?UnitJeep$nx$UnitTypeClass$:
        //   `n` = near memory-model qualifier (NOT a pointer prefix).
        //   `x$UnitTypeClass$` = UnitTypeClass const (Watcom post-fix const style).
        //   These are const instances (e.g. `extern const UnitTypeClass UnitJeep`).
        demangle_eq!(
            "W?UnitJeep$nx$UnitTypeClass$",
            "UnitTypeClass const UnitJeep"
        );
    }

    #[test]
    fn test_data_symbol_near_template_type() {
        // W?Weapons$n$TFixedIHeapClass$::1n$WeaponTypeClass$:
        //   `n` = near memory-model qualifier.
        //   `$TFixedIHeapClass$::1n$WeaponTypeClass$` = TFixedIHeapClass<WeaponTypeClass>.
        //   A static heap object, not a pointer.
        demangle_eq!(
            "W?Weapons$n$TFixedIHeapClass$::1n$WeaponTypeClass$",
            "TFixedIHeapClass<WeaponTypeClass> Weapons"
        );
    }

    #[test]
    fn test_data_symbol_near_memory_model_int() {
        // `ni` = near int: `n` is the addressing-mode qualifier, not a pointer.
        // A global named `pInt` with type `ni` is just `int pInt`.
        demangle_eq!("W?pInt$ni", "int pInt");
    }

    #[test]
    fn test_data_symbol_array_type() {
        // W?CenterOffset$:BuildingClass$n[]xul:
        //   `n` = near (followed by `[` → array, not pointer: no `*` added)
        //   `[]` = unbounded array
        //   `xul` = unsigned long const (Watcom post-fix const style)
        //   Expected: unsigned long const BuildingClass::CenterOffset[]
        demangle_eq!(
            "W?CenterOffset$:BuildingClass$n[]xul",
            "unsigned long const BuildingClass::CenterOffset[]"
        );
    }

    #[test]
    fn test_data_symbol_array_type_bounded() {
        // n[4]i = near bounded array of 4 ints → int[4]
        demangle_eq!("W?Arr$n[4]i", "int Arr[4]");
    }

    #[test]
    fn test_data_symbol_near_is_memory_model_not_pointer() {
        // `ni` = near int: `n` is the MEMORY-MODEL qualifier, not a pointer
        // prefix. Watcom uses `pn`/`pf`/`ph` for pointer types; bare `n` or
        // `h` after the name separator just indicate the addressing mode.
        // W?IsNewAllowed$:BuildingClass$ni → int BuildingClass::IsNewAllowed
        demangle_eq!(
            "W?IsNewAllowed$:BuildingClass$ni",
            "int BuildingClass::IsNewAllowed"
        );
    }

    #[test]
    fn test_near_ptr_const_char_shows_qualifier() {
        // `pnxa` = near pointer to const char.
        // Watcom's own demangler renders this as `char const near *`:
        //   - base type first: `char`
        //   - const qualifier after the type (east-const / post-fix style)
        //   - `near` addressing-mode qualifier shown explicitly
        //   - `*` at the end
        // W?Bar$n(pnxa)v → void Bar(char const near *)
        demangle_eq!("W?Bar$n(pnxa)v", "void Bar(char const near *)");
    }

    #[test]
    fn test_array_brackets_after_name_not_type() {
        // In C/C++ declarations, array brackets bind to the *declarator* (name),
        // not the type specifier. So `const int arr[]` is correct; `const int[] arr`
        // is not valid C++. The demangler must place `[]` after the full name.
        // W?Harvester_Dump_List$:UnitTypeClass$n[]xi
        //   n = near (memory-model passthrough)
        //   []xi = array of const int
        // Expected: int const UnitTypeClass::Harvester_Dump_List[]
        demangle_eq!(
            "W?Harvester_Dump_List$:UnitTypeClass$n[]xi",
            "int const UnitTypeClass::Harvester_Dump_List[]"
        );
    }

    // ── UDATA.OBJ symbols ─────────────────────────────────────────────────────

    #[test]
    fn test_integer_template_param() {
        // Int<64> using base-32 integer template param: 0x20 base32 = 64, z = positive
        demangle_eq!(
            "W?Gcd$n(rnx$Int$::020z$rnx$1::020z$)$1::020z$",
            "Int<64> Gcd(Int<64> const near &, Int<64> const near &)"
        );
    }

    #[test]
    fn test_bare_short() {
        demangle_eq!("W?MAX$n(ss)s", "short MAX(short, short)");
    }

    #[test]
    fn test_bare_long() {
        demangle_eq!("W?ABS$n(l)l", "long ABS(long)");
    }

    /// Regression: adjacent single-letter primitives in a parameter list must
    /// be treated as SEPARATE types.  `l` (long) followed by `i` (int) used to
    /// be eagerly swallowed as a single "long int", collapsing two parameters
    /// into one.  Real-world example from a Watcom-built TimerClass::Set.
    #[test]
    fn test_long_int_two_params() {
        demangle_eq!(
            "W?Set$:TimerClass$n(li)l",
            "long TimerClass::Set(long, int)"
        );
    }

    /// `(si)` — short, int — same adjacency bug as `(li)`.
    #[test]
    fn test_short_int_two_params() {
        demangle_eq!("W?Foo$n(si)v", "void Foo(short, int)");
    }

    /// `(lui)` — long, unsigned int — `l` followed by `ui`.
    #[test]
    fn test_long_unsigned_int_two_params() {
        demangle_eq!("W?Foo$n(lui)v", "void Foo(long, unsigned int)");
    }

    /// `(sui)` — short, unsigned int — `s` followed by `ui`.
    #[test]
    fn test_short_unsigned_int_two_params() {
        demangle_eq!("W?Foo$n(sui)v", "void Foo(short, unsigned int)");
    }

    /// `(usi)` — unsigned short, int — `us` followed by `i`.
    #[test]
    fn test_unsigned_short_int_two_params() {
        demangle_eq!("W?Foo$n(usi)v", "void Foo(unsigned short, int)");
    }

    /// Guard: `ld` must stay a single "long double" (distinct C++ type that
    /// can't be spelt as the concatenation of `l` + `d`).
    #[test]
    fn test_long_double_single_param() {
        demangle_eq!("W?Foo$n(ld)v", "void Foo(long double)");
    }

    /// Guard: `sc` must stay a single "signed char" (distinct from plain `c` =
    /// `char`, which is unsigned-or-signed-implementation-defined).
    #[test]
    fn test_signed_char_single_param() {
        demangle_eq!("W?Foo$n(sc)v", "void Foo(signed char)");
    }

    #[test]
    fn test_conversion_operator() {
        // $op = type conversion operator; return type is the target type
        demangle_eq!(
            "W?$op:FrameTimerClass$n.x()l",
            "FrameTimerClass::operator long(void) const"
        );
    }

    #[test]
    fn test_destructor_templated_class() {
        demangle_eq!(
            "W?$dt:BasicTimerClass$::1n$FrameTimerClass$$n()_",
            "BasicTimerClass<FrameTimerClass>::~BasicTimerClass<FrameTimerClass>(void)"
        );
    }

    #[test]
    fn test_method_templated_class() {
        demangle_eq!(
            "W?Stop$:TTimerClass$::1n$FrameTimerClass$$n()v",
            "void TTimerClass<FrameTimerClass>::Stop(void)"
        );
    }

    #[test]
    fn test_nested_scoped_class_with_template() {
        // IndexClass<INIEntry near *> nested inside INIClass
        demangle_eq!(
            "W?Is_Present$:IndexClass$::1npn$INIEntry$:INIClass$$n.x(i)i",
            "int INIClass::IndexClass<INIEntry near *>::Is_Present(int) const"
        );
    }

    #[test]
    fn test_watcom_internal_wvf() {
        // $Wvf0mo12 = compiler-generated vtable thunk
        demangle_eq!(
            "W?$Wvf0mo12:AbstractTypeClass$$nx[]pn()v",
            "void AbstractTypeClass::$Wvf0mo12(void)"
        );
    }

    #[test]
    fn test_operator_subscript_od() {
        demangle_eq!(
            "W?$od:VectorClass$::1npnxan(ui)rnpnxa",
            "char const near * near & VectorClass<char const near *>::operator[](unsigned int)"
        );
    }

    #[test]
    fn test_template_backref_with_template() {
        // Delete method on DynamicVectorClass<PerCellPacketData near *>
        // $2$ is back-ref to "PerCellPacketData" (registered during class parsing)
        demangle_eq!(
            "W?Delete$:DynamicVectorClass$::1npn$PerCellPacketData$$n(rnxpn$2$)i",
            "int DynamicVectorClass<PerCellPacketData near *>::Delete(PerCellPacketData near * const near &)"
        );
    }

    #[test]
    fn test_watcom_internal_wdf_thunk() {
        // $Wdf thunks are compiler-generated; partial demangling is acceptable
        demangle_eq!(
            "W?$Wdf0kResize$n(uipnxpnv)i",
            "int $Wdf0kResize(unsigned int, void near * const near *)"
        );
    }

    #[test]
    fn test_watcom_internal_wts() {
        // $Wts = compiler-generated type-sig, class embedded without `:` prefix
        demangle_eq!(
            "W?$Wts0en$SuperClass$$nx[]uc",
            "unsigned char const SuperClass::$Wts0en[]"
        );
    }

    #[test]
    fn test_self_ref_copy_ctor() {
        // $0$ = self-reference to enclosing class type
        demangle_eq!(
            "W?$ct:UnitTypeClass$n(rnx$0$)_",
            "UnitTypeClass::UnitTypeClass(UnitTypeClass const near &)"
        );
    }

    #[test]
    fn test_watcom_internal_wda() {
        // $Wda018 = compiler-generated default-arg thunk
        demangle_eq!(
            "W?$Wda018$:SuperClass$n(ui)pnv",
            "void near * SuperClass::$Wda018(unsigned int)"
        );
    }

    #[test]
    fn test_watcom_internal_wsi() {
        // $Wsi = static initializer.  The full scope chain for this symbol
        // includes embedded function signatures which aren't fully supported.
        // We verify at least the __staticinit name mapping works.
        demangle_eq!(
            "W?$Wsi0hg$n[]$Int$::020z$:.0$:?Gcd$n(rnx$Int$::020z$rnx$2::020z$)$2::020z$::1n$2::020z$nx[]$2::020z$",
            "__staticinit(Int<64> const near &)"
        );
    }

    #[test]
    fn test_dynamic_vector_char_constructor() {
        // DynamicVectorClass<char near *> — `a` = char type code
        demangle_eq!(
            "W?$ct:DynamicVectorClass$::1npnan(uipnxpna)_",
            "DynamicVectorClass<char near *>::DynamicVectorClass<char near *>(unsigned int, char near * const near *)"
        );
    }

    #[test]
    fn test_dynamic_vector_char_method() {
        demangle_eq!(
            "W?Delete$:DynamicVectorClass$::1npnan(rnxpna)i",
            "int DynamicVectorClass<char near *>::Delete(char near * const near &)"
        );
    }

    #[test]
    fn test_wts_templated_class() {
        // $Wts with templated class DynamicVectorClass<NewDeletePacketData near *>
        demangle_eq!(
            "W?$Wts1en$DynamicVectorClass$::1npn$NewDeletePacketData$$$$nx[]uc",
            "unsigned char const DynamicVectorClass<NewDeletePacketData near *>::$Wts1en[]"
        );
    }
}
