//! Borland C++ name demangler.
//!
//! Implements the Borland C++ name mangling scheme used by Borland C++
//! (3.x, 4.x, 5.x) and Turbo C++ for DOS and Windows.
//!
//! # Mangled name format
//!
//! ```text
//! [@Class1[@Class2]@]FuncName$[x]<cc>ParamTypes
//! ```
//!
//! | Part             | Meaning                                                    |
//! |------------------|------------------------------------------------------------|
//! | `@Class1@`       | Optional class scope (member functions only)               |
//! | `@Class2@`       | Optional nested class (e.g. `Outer::Inner`)                |
//! | `FuncName`       | Function name, or `$bctr`/`$bdtr`/`$bcall`/... for special |
//! | `$`              | Separator before calling convention                        |
//! | `x`              | Optional const qualifier on member function                |
//! | `<cc>`           | Calling convention (see table below)                       |
//! | `ParamTypes`     | Concatenated parameter type codes (see table below)        |
//!
//! # Calling convention codes
//!
//! | Code   | Convention   |
//! |--------|--------------|
//! | `qqr`  | __fastcall   |
//! | `qqs`  | __stdcall    |
//! | `q`    | __cdecl      |
//! | `Q`    | __pascal     |
//!
//! # Parameter type encoding
//!
//! | Code             | C++ Type          |
//! |------------------|-------------------|
//! | `v`              | `void`            |
//! | `i`              | `int`             |
//! | `l`              | `long`            |
//! | `s`              | `short`           |
//! | `c`              | `char`            |
//! | `f`              | `float`           |
//! | `d`              | `double`          |
//! | `ld`             | `long double`     |
//! | `b`              | `bool`            |
//! | `e`              | `...`             |
//! | `ui`             | `unsigned int`    |
//! | `ul`             | `unsigned long`   |
//! | `uc`             | `unsigned char`   |
//! | `us`             | `unsigned short`  |
//! | `p` + *T*        | *T* `*`           |
//! | `r` + *T*        | *T* `&`           |
//! | `x` + *T*        | `const` *T*       |
//! | `t` + *N*        | Same as param *N* (1-based back-reference) |
//! | *N* `TypeName`   | Named type (*N* decimal digits = length of name) |
//!
//! # Operator / special function tokens
//!
//! | Token    | C++                |
//! |----------|--------------------|
//! | `$bctr`  | constructor        |
//! | `$bdtr`  | destructor         |
//! | `$bcall` | `operator()`       |
//! | `$bsubs` | `operator[]`       |
//! | `$badd`  | `operator+`        |
//! | `$bsub`  | `operator-`        |
//! | `$bmul`  | `operator*`        |
//! | `$bdiv`  | `operator/`        |
//! | `$bmod`  | `operator%`        |
//! | `$basg`  | `operator=`        |
//! | `$beql`  | `operator==`       |
//! | `$bneq`  | `operator!=`       |
//! | `$bgtr`  | `operator>`        |
//! | `$blss`  | `operator<`        |
//! | `$bgeq`  | `operator>=`       |
//! | `$bleq`  | `operator<=`       |
//! | `$bnot`  | `operator!`        |
//! | `$bcmp`  | `operator~`        |
//! | `$bland` | `operator&&`       |
//! | `$blor`  | `operator\|\|`     |
//! | `$band`  | `operator&`        |
//! | `$bor`   | `operator\|`       |
//! | `$bxor`  | `operator^`        |
//! | `$blsh`  | `operator<<`       |
//! | `$brsh`  | `operator>>`       |
//! | `$binc`  | `operator++`       |
//! | `$bdec`  | `operator--`       |
//! | `$barow` | `operator->`       |
//! | `$bcoma` | `operator,`        |
//! | `$bnew`  | `operator new`     |
//! | `$bnwa`  | `operator new[]`   |
//! | `$bdele` | `operator delete`  |
//! | `$bdla`  | `operator delete[]` |

use alloc::{format, string::String, vec::Vec};

/// Attempt to demangle a Borland C++ mangled name.
///
/// Returns `None` if `name` does not look like a Borland-mangled symbol or
/// cannot be fully parsed.
///
/// Accepts member functions (`@ClassName@FuncName$qParams`), free functions
/// with a leading `@` (`@FuncName$qParams`), free functions without
/// (`FuncName$qParams`), data symbols (`@ClassName@DataMember`), and
/// local-scope symbols with `@@N@@` prefix (switch tables, static locals).
pub fn demangle(name: &str) -> Option<String> {
    // Detect @@N@ local scope prefix (compiler-generated local symbols:
    // switch tables, static locals, exception handler tables, etc.).
    // Turbo Dump renders these as "::N::::Scope::Func(params)".
    //
    // Two forms:
    //   @@N@@Class@Func$params  — member function (rest starts with '@')
    //   @@N@_main@suffix        — free/C function  (rest is plain name)
    if let Some((scope_idx, rest)) = strip_local_scope_prefix(name) {
        if let Some(demangled) = demangle(rest) {
            return Some(format!("::{scope_idx}::::{demangled}"));
        }
        // Plain C function or unrecognized — extract name before suffix metadata.
        let func_name = rest.split('@').next().unwrap_or(rest);
        if !func_name.is_empty() {
            return Some(format!("::{scope_idx}::::{func_name}"));
        }
        return None;
    }

    // Borland compiler-generated exception handling symbols.
    // @$xt$<len><TypeName> — exception type descriptor for throw/catch matching.
    if let Some(rest) = name.strip_prefix("@$xt$") {
        return parse_compiler_type_symbol(rest, "exception type");
    }

    // '%' delimiters indicate Borland template syntax.  Try data-symbol
    // demangling first; if it fails, fall through to template-aware function
    // parsing (e.g. @%List$t17INIClass@INIEntry%@$bdtr$qqrv).
    if name.contains('%') {
        if let Some(result) = demangle_data_symbol(name) {
            return Some(result);
        }
        // Fall through — may be a member function on a template class.
    }

    if !name.contains('$') {
        // No '$' — might be a Borland data symbol: @Class@Member
        return demangle_data_symbol(name);
    }

    // ── 1. Class prefix: @Class1[@Class2]@ ──────────────────────────────────
    let (scope, rest) = parse_scope(name)?;

    // ── 2. Function token ───────────────────────────────────────────────────
    let (func_token, rest) = parse_func_name(rest)?;

    // ── 3. Calling-convention separator: $[x]<cc> ───────────────────────────
    let rest = rest.strip_prefix('$')?;
    if rest.is_empty() {
        return None;
    }

    // Optional const qualifier on member function (appears before CC code).
    let (is_const, rest) = if rest.starts_with('x') {
        (true, &rest[1..])
    } else {
        (false, rest)
    };

    // Consume calling-convention code (longest match first).
    let params_str = if let Some(r) = rest.strip_prefix("qqr") {
        r
    } else if let Some(r) = rest.strip_prefix("qqs") {
        r
    } else if let Some(r) = rest.strip_prefix('q') {
        r
    } else if let Some(r) = rest.strip_prefix('Q') {
        r
    } else {
        // Unknown CC — skip one character as a fallback.
        if rest.is_empty() {
            return None;
        }
        &rest[1..]
    };

    // Borland parameter strings are purely concatenated type codes — they
    // never contain '(' or ')'.  Names like "W?foo$n()v" (Watcom) can
    // pass the '$' pre-filter; the parenthesis check rejects them cleanly.
    if params_str.contains('(') || params_str.contains(')') {
        return None;
    }

    // ── 4. Parameter list ───────────────────────────────────────────────────
    let params = parse_param_list(params_str);

    // ── 5. Assemble output ──────────────────────────────────────────────────
    build_output(func_token, scope.as_deref(), &params, is_const)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Try to demangle a Borland data symbol.
///
/// Borland C++ encodes static/global data members as `@Class@Member` with
/// no calling-convention or parameter encoding.  Nested classes use additional
/// `@` separators: `@Outer@Inner@Member`.
///
/// Template instantiations are wrapped in `%…%` delimiters with `$`-prefixed
/// template parameters inside:
///   `@%Int$ii$64%@Borrow` → `Int<(int)64>::Borrow`
///
/// Returns `Some("Class::Member")` or `None` if the name doesn't match.
fn demangle_data_symbol(name: &str) -> Option<String> {
    // Must start with '@' and have at least two '@'-separated segments.
    let inner = name.strip_prefix('@')?;
    if inner.is_empty() {
        return None;
    }

    // Split on '@' but keep %…% template blocks intact.
    let segments = split_scope_segments(inner)?;
    if segments.len() < 2 {
        return None;
    }

    let mut parts = Vec::new();
    for seg in &segments {
        if let Some(tmpl) = seg.strip_prefix('%').and_then(|s| s.strip_suffix('%')) {
            // Template instantiation: %Name$params%
            let rendered = parse_template_instantiation(tmpl)?;
            parts.push(rendered);
        } else if seg.is_empty() || seg.contains('$') {
            // Empty segment or contains '$' (likely a function token, not data).
            return None;
        } else {
            parts.push((*seg).to_string());
        }
    }

    Some(parts.join("::"))
}

/// Split `s` on `@` but treat `%…%` template blocks as atomic units.
///
/// Returns `None` if `%` delimiters are unbalanced.
fn split_scope_segments(s: &str) -> Option<Vec<&str>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_template = false;
    let bytes = s.as_bytes();

    for i in 0..bytes.len() {
        if bytes[i] == b'%' {
            in_template = !in_template;
        } else if bytes[i] == b'@' && !in_template {
            segments.push(&s[start..i]);
            start = i + 1;
        }
    }

    if in_template {
        return None; // unbalanced '%'
    }

    // Last segment (after the final '@' or the whole string if no '@').
    segments.push(&s[start..]);
    Some(segments)
}

/// Parse a template instantiation body (without the outer `%` delimiters).
///
/// E.g. `"Int$ii$64"` → `"Int<(int)64>"`
fn parse_template_instantiation(s: &str) -> Option<String> {
    let dollar = s.find('$')?;
    let name = &s[..dollar];
    if name.is_empty() {
        return None;
    }

    let mut params = Vec::new();
    let mut rest = &s[dollar..];

    while !rest.is_empty() {
        let (param, remaining) = parse_template_param(rest)?;
        params.push(param);
        rest = remaining;
    }

    if params.is_empty() {
        return None;
    }

    Some(format!("{}<{}>", name, params.join(", ")))
}

/// Parse one template parameter starting with `$`.
///
/// Currently handles integer (non-type) template parameters:
///   `$i` + type_code + `$` + value_digits
///
/// E.g. `$ii$64` → `(int)64`
fn parse_template_param(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('$')?;

    if let Some(after_t) = rest.strip_prefix('t') {
        // Type template parameter: $t + type_encoding
        // The type is encoded the same way as function parameters (length-
        // prefixed class names, pointer/reference qualifiers, etc.).
        let (type_name, remaining) = parse_type(after_t, &[])?;
        Some((type_name, remaining))
    } else if let Some(after_i) = rest.strip_prefix('i') {
        // Integer template parameter: $i + type_code + $ + digits
        let (type_name, after_type) = parse_int_type_code(after_i)?;
        let after_dollar = after_type.strip_prefix('$')?;
        // Value: consume ASCII digits until next '$' or end.
        let val_end = after_dollar
            .bytes()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(after_dollar.len());
        if val_end == 0 {
            return None; // no digits
        }
        let value = &after_dollar[..val_end];
        Some((format!("({type_name}){value}"), &after_dollar[val_end..]))
    } else {
        None // unknown template parameter kind
    }
}

/// Match a Borland type code used in integer template parameter types.
fn parse_int_type_code(s: &str) -> Option<(&'static str, &str)> {
    // Longest match first for multi-char codes.
    if let Some(r) = s.strip_prefix("ui") { return Some(("unsigned int", r)); }
    if let Some(r) = s.strip_prefix("ul") { return Some(("unsigned long", r)); }
    if let Some(r) = s.strip_prefix("us") { return Some(("unsigned short", r)); }
    if let Some(r) = s.strip_prefix("uc") { return Some(("unsigned char", r)); }
    if let Some(r) = s.strip_prefix('i')  { return Some(("int", r)); }
    if let Some(r) = s.strip_prefix('l')  { return Some(("long", r)); }
    if let Some(r) = s.strip_prefix('s')  { return Some(("short", r)); }
    if let Some(r) = s.strip_prefix('c')  { return Some(("char", r)); }
    None
}

/// Detect and strip Borland's `@@N@` local scope prefix.
///
/// Compiler-generated local symbols (switch tables, static locals, etc.) use:
///   `@@N@@Class@Func$params…`  — member function (rest starts with `@`)
///   `@@N@func_name@suffix`     — free / C function
///
/// The format is always `@@` + digits + `@` + remainder.  For member
/// functions the remainder naturally starts with `@` (the class prefix).
///
/// Returns `(N_str, "rest…")` if matched.
fn strip_local_scope_prefix(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("@@")?;

    // Consume the numeric scope index.
    let idx_end = rest.bytes().position(|b| !b.is_ascii_digit())?;
    if idx_end == 0 {
        return None; // no digits (e.g. "@@@@...")
    }
    let n = &rest[..idx_end];

    // Expect a single '@' separator after the index.
    let after_digits = &rest[idx_end..];
    let after = after_digits.strip_prefix('@')?;
    if after.is_empty() {
        return None;
    }

    Some((n, after))
}

/// Parse a Borland compiler-generated symbol that encodes a type followed
/// by an annotation.
///
/// Two forms:
///   1. Length-prefixed class name: `"10UsageError"` → `"UsageError 'exception type'"`
///   2. Inline type code: `"pc"` (char *) → `"char * 'exception type'"`
fn parse_compiler_type_symbol(s: &str, annotation: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }

    // Try length-prefixed class name first (starts with digit).
    if s.as_bytes()[0].is_ascii_digit() {
        let len_end = s.bytes().position(|c| !c.is_ascii_digit()).unwrap_or(s.len());
        let len: usize = s[..len_end].parse().ok()?;
        let name_start = &s[len_end..];
        if name_start.len() < len {
            return None;
        }
        let type_name = &name_start[..len];
        // Template instantiation: %Name$params%
        let display_name = if let Some(tmpl) =
            type_name.strip_prefix('%').and_then(|s| s.strip_suffix('%'))
        {
            parse_template_instantiation(tmpl)?
        } else if type_name.contains('@') {
            // Borland encodes nested scopes within type names using '@'.
            type_name.replace('@', "::")
        } else {
            type_name.to_string()
        };
        return Some(format!("{display_name} `{annotation}'"));
    }

    // Fall back to inline type code (e.g. "pc" = char *, "i" = int).
    let (type_str, _rest) = parse_type(s, &[])?;
    Some(format!("{type_str} `{annotation}'"))
}

/// Parse the scope prefix, handling nested classes, templates, and free-function `@` prefix.
///
/// Borland member functions: `@Class1@Class2@FuncName$...`
/// Borland template member:  `@%List$t17INIClass@INIEntry%@$bdtr$qqrv`
/// Borland free functions:   `@FuncName$...` or `FuncName$...`
///
/// Returns `(Some("Class1::Class2"), rest)` for member functions,
/// `(None, rest)` for free functions.
fn parse_scope(name: &str) -> Option<(Option<String>, &str)> {
    if !name.starts_with('@') {
        return Some((None, name));
    }

    let inner = &name[1..]; // skip leading '@'

    // Find the first '$' that is NOT inside a %…% template block.
    // Template names like %List$t17INIClass@INIEntry% contain internal '$' and
    // '@' characters that must not be treated as delimiters.
    let dollar_pos = find_outside_template(inner, b'$')?;
    let before_dollar = &inner[..dollar_pos];

    // Split on '@' but keep %…% template blocks intact.
    let segments = split_scope_segments(before_dollar)?;
    if segments.is_empty() {
        return None;
    }

    // Demangle any %…% template segments into display form.
    let demangle_seg = |seg: &str| -> Option<String> {
        if let Some(tmpl) = seg.strip_prefix('%').and_then(|s| s.strip_suffix('%')) {
            parse_template_instantiation(tmpl)
        } else {
            Some(seg.to_string())
        }
    };

    // When the function name starts with '$' (e.g. `$bctr`, `$bcall`), the
    // text immediately before that '$' is an '@', so the last segment after
    // splitting is empty.  Example: "@BankClass@$bctr$qv"
    //   inner      = "BankClass@$bctr$qv"
    //   before '$' = "BankClass@"
    //   segments   = ["BankClass", ""]
    // In this case, all non-empty segments form the class scope and the rest
    // starts at the '$' (the special function token).
    if segments.last().map_or(false, |s| s.is_empty()) {
        let scope_parts: Vec<String> = segments
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| demangle_seg(s))
            .collect::<Option<Vec<_>>>()?;
        if scope_parts.is_empty() {
            if before_dollar.is_empty() {
                // @$bdele$qpv — free function with special operator token, no class scope.
                return Some((None, inner));
            }
            return None; // "@@$..." — no class name
        }
        // Check that no interior segment is empty (e.g. "@A@@$bctr$qv").
        if scope_parts.len() != segments.len() - 1 {
            return None;
        }
        let scope = scope_parts.join("::");
        // rest starts at the '$' which begins the function token.
        Some((Some(scope), &inner[dollar_pos..]))
    } else if segments.iter().any(|s| s.is_empty()) {
        None // empty interior segment (e.g. "@A@@Func$qv")
    } else if segments.len() == 1 {
        // Single segment after '@': free function with linkage decoration.
        // e.g. @FuncName$qParams → no class scope.
        Some((None, inner))
    } else {
        // Multiple segments: all but last form the scope chain.
        let scope_parts: Vec<String> = segments[..segments.len() - 1]
            .iter()
            .map(|s| demangle_seg(s))
            .collect::<Option<Vec<_>>>()?;
        let scope = scope_parts.join("::");
        // Return rest starting at the last segment (the function name).
        // Find the position after the last template-aware '@' separator.
        let last_seg = segments.last().unwrap();
        let func_start = before_dollar.len() - last_seg.len();
        Some((Some(scope), &inner[func_start..]))
    }
}

/// Return the innermost class name from a scope string, respecting `<>` nesting.
///
/// For `"Outer::Inner"` returns `"Inner"`.
/// For `"List<INIClass::INIEntry>"` returns `"List<INIClass::INIEntry>"` (the
/// `::` is inside `<>` so the whole thing is one class name).
fn innermost_class_name(scope: &str) -> &str {
    let bytes = scope.as_bytes();
    let mut depth: u32 = 0;
    let mut last_sep = 0; // byte index after the last top-level `::`
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b':' => {
                last_sep = i + 2;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    &scope[last_sep..]
}

/// Find the position of `needle` in `s`, ignoring occurrences inside `%…%` template blocks.
fn find_outside_template(s: &str, needle: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_template = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'%' {
            in_template = !in_template;
        } else if b == needle && !in_template {
            return Some(i);
        }
    }
    None
}

/// Parse the function name token.
///
/// Handles Borland special names (`$bctr`, `$bdtr`, `$bcall`, etc.) as well
/// as plain identifiers.
fn parse_func_name(s: &str) -> Option<(&str, &str)> {
    // Try all known special-name tokens (longest first within each prefix).
    static SPECIAL_TOKENS: &[&str] = &[
        "$bctr", "$bdtr",
        "$bcall", "$bcoma", "$bcmp",
        "$badd", "$basg", "$barow", "$barwm",
        "$band", "$bland", "$blor", "$blsh", "$bleq", "$blss",
        "$bsubs", "$bsub",
        "$bmul", "$bmod",
        "$bdiv", "$bdec", "$bdele", "$bdla",
        "$beql",
        "$bgeq", "$bgtr",
        "$binc", "$bind",
        "$bneq", "$bnot", "$bnew", "$bnwa",
        "$bor",
        "$brsh", "$brplu", "$brmin", "$brmul", "$brdiv", "$brmod",
        "$brand", "$bror", "$brxor", "$brlsh", "$brrsh",
        "$bfnc",
        "$bxor",
    ];

    for &tok in SPECIAL_TOKENS {
        if let Some(rest) = s.strip_prefix(tok) {
            return Some((tok, rest));
        }
    }

    if let Some(inner) = s.strip_prefix('$') {
        // Unknown special token — consume up to the next '$'.
        let end = inner.bytes().position(|b| b == b'$')?;
        Some((&s[..end + 1], &s[end + 1..]))
    } else {
        // Plain identifier ending at the first '$'.
        let dollar = s.bytes().position(|b| b == b'$')?;
        Some((&s[..dollar], &s[dollar..]))
    }
}

/// Parse every parameter type code in `s` and return them as a `Vec<String>`.
///
/// Uses `parsed` to support Borland back-references (`t1`, `t2`, ...) which
/// repeat a previously parsed parameter type.
fn parse_param_list(mut s: &str) -> Vec<String> {
    let mut result = Vec::new();
    loop {
        if s.is_empty() {
            break;
        }
        match parse_type(s, &result) {
            Some((ty, rest)) => {
                if rest.len() == s.len() {
                    break; // guard against infinite loop
                }
                if !ty.is_empty() {
                    result.push(ty);
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
/// `prev_params` allows resolving `t<N>` back-references.
fn parse_type<'a>(s: &'a str, prev_params: &[String]) -> Option<(String, &'a str)> {
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    match b[0] {
        // ── Primitive / keyword types ────────────────────────────────────────
        b'v' => Some(("void".to_string(),   &s[1..])),
        b'i' => Some(("int".to_string(),    &s[1..])),
        b'l' => match b.get(1) {
            Some(b'd') => Some(("long double".to_string(), &s[2..])),
            _          => Some(("long".to_string(),        &s[1..])),
        },
        b's' => Some(("short".to_string(),  &s[1..])),
        b'c' => Some(("char".to_string(),   &s[1..])),
        b'f' => Some(("float".to_string(),  &s[1..])),
        b'd' => Some(("double".to_string(), &s[1..])),
        b'b' => Some(("bool".to_string(),   &s[1..])),
        b'e' => Some(("...".to_string(),    &s[1..])),

        // ── unsigned types: u + base type ───────────────────────────────────
        b'u' => match b.get(1) {
            Some(b'i') => Some(("unsigned int".to_string(),   &s[2..])),
            Some(b'l') => Some(("unsigned long".to_string(),  &s[2..])),
            Some(b'c') => Some(("unsigned char".to_string(),  &s[2..])),
            Some(b's') => Some(("unsigned short".to_string(), &s[2..])),
            _ => None,
        },

        // ── Back-reference: t + digit ───────────────────────────────────────
        // Borland uses `t1`, `t2`, ... to repeat a previously seen parameter.
        b't' => {
            if b.len() < 2 || !b[1].is_ascii_digit() {
                return None;
            }
            let idx = (b[1] - b'0') as usize;
            if idx == 0 || idx > prev_params.len() {
                return None;
            }
            Some((prev_params[idx - 1].clone(), &s[2..]))
        }

        // ── Pointer: p + type ───────────────────────────────────────────────
        b'p' => {
            let (inner, rest) = parse_type(&s[1..], prev_params)?;
            if inner.is_empty() || inner == "void" {
                Some(("void *".to_string(), rest))
            } else {
                Some((format!("{inner} *"), rest))
            }
        }

        // ── Reference: r + type ─────────────────────────────────────────────
        b'r' => {
            let (inner, rest) = parse_type(&s[1..], prev_params)?;
            Some((format!("{inner} &"), rest))
        }

        // ── const: x + type ─────────────────────────────────────────────────
        b'x' => {
            let (inner, rest) = parse_type(&s[1..], prev_params)?;
            if inner.is_empty() {
                Some(("const".to_string(), rest))
            } else {
                Some((format!("const {inner}"), rest))
            }
        }

        // ── Length-prefixed class/struct name: <N><TypeName> ─────────────────
        b'0'..=b'9' => {
            let len_end = s.bytes().position(|c| !c.is_ascii_digit())?;
            let len: usize = s[..len_end].parse().ok()?;
            let name_start = &s[len_end..];
            if name_start.len() < len {
                return None; // truncated
            }
            let type_name = &name_start[..len];
            // Template instantiation: %Name$params%
            let display_name = if let Some(tmpl) =
                type_name.strip_prefix('%').and_then(|s| s.strip_suffix('%'))
            {
                parse_template_instantiation(tmpl)?
            } else if type_name.contains('@') {
                // Borland encodes nested scopes within type names using '@'.
                // e.g. "19PKPipe@CryptControl" → "PKPipe::CryptControl"
                type_name.replace('@', "::")
            } else {
                type_name.to_string()
            };
            Some((display_name, &name_start[len..]))
        }

        _ => None,
    }
}

/// Map a special-function token to its C++ display name.
fn special_func_display(token: &str) -> Option<&'static str> {
    Some(match token {
        "$bcall" | "$bfnc" => "operator()",
        "$bsubs" => "operator[]",
        "$badd"  => "operator+",
        "$bsub"  => "operator-",
        "$bmul"  => "operator*",
        "$bdiv"  => "operator/",
        "$bmod"  => "operator%",
        "$basg"  => "operator=",
        "$beql"  => "operator==",
        "$bneq"  => "operator!=",
        "$bgtr"  => "operator>",
        "$blss"  => "operator<",
        "$bgeq"  => "operator>=",
        "$bleq"  => "operator<=",
        "$bnot"  => "operator!",
        "$bcmp"  => "operator~",
        "$bland" => "operator&&",
        "$blor"  => "operator||",
        "$band"  => "operator&",
        "$bor"   => "operator|",
        "$bxor"  => "operator^",
        "$blsh"  => "operator<<",
        "$brsh"  => "operator>>",
        "$binc"  => "operator++",
        "$bdec"  => "operator--",
        "$bind"  => "operator*",
        "$badr"  => "operator&",
        "$barow" => "operator->",
        "$barwm" => "operator->*",
        "$bcoma" => "operator,",
        "$brplu" => "operator+=",
        "$brmin" => "operator-=",
        "$brmul" => "operator*=",
        "$brdiv" => "operator/=",
        "$brmod" => "operator%=",
        "$brand" => "operator&=",
        "$bror"  => "operator|=",
        "$brxor" => "operator^=",
        "$brlsh" => "operator<<=",
        "$brrsh" => "operator>>=",
        "$bnew"  => "operator new",
        "$bnwa"  => "operator new[]",
        "$bdele" => "operator delete",
        "$bdla"  => "operator delete[]",
        _ => return None,
    })
}

/// Assemble the final demangled string.
fn build_output(
    func_token: &str,
    scope: Option<&str>,
    params: &[String],
    is_const: bool,
) -> Option<String> {
    let is_ctor = func_token == "$bctr";
    let is_dtor = func_token == "$bdtr";

    let mut result = String::new();

    // Class qualifier.
    if let Some(s) = scope {
        result.push_str(s);
        result.push_str("::");
    }

    // Function name.
    if is_ctor {
        // Constructor — use the innermost class name.
        let cls = scope.map(innermost_class_name).unwrap_or(func_token);
        result.push_str(cls);
    } else if is_dtor {
        result.push('~');
        let cls = scope.map(innermost_class_name).unwrap_or(func_token);
        result.push_str(cls);
    } else if let Some(display) = special_func_display(func_token) {
        result.push_str(display);
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

    if is_const {
        result.push_str(" const");
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

    // ── Negative tests ───────────────────────────────────────────────────────

    #[test]
    fn test_not_borland() {
        demangle_none!("W?fn_init$n()v");       // Watcom
        demangle_none!("?SomeFunction@@YAHXZ"); // MSVC
        demangle_none!("_SomeFunction");        // plain C
        demangle_none!("");                     // empty
        demangle_none!("@");                    // lone @
        demangle_none!("@@");                   // empty class
    }

    // ── Constructor / destructor ─────────────────────────────────────────────

    #[test]
    fn test_constructor() {
        demangle_eq!(
            "@BankClass@$bctr$qv",
            "BankClass::BankClass(void)"
        );
    }

    #[test]
    fn test_destructor() {
        demangle_eq!(
            "@BankClass@$bdtr$qv",
            "BankClass::~BankClass(void)"
        );
    }

    // ── Methods with primitive params ────────────────────────────────────────

    #[test]
    fn test_void_method() {
        demangle_eq!(
            "@BankClass@New_Game_Reset$qv",
            "BankClass::New_Game_Reset(void)"
        );
    }

    #[test]
    fn test_int_param() {
        demangle_eq!(
            "@BankClass@Set_Houses$qi",
            "BankClass::Set_Houses(int)"
        );
    }

    #[test]
    fn test_unsigned_int_param() {
        demangle_eq!(
            "@SomeClass@Method$qui",
            "SomeClass::Method(unsigned int)"
        );
    }

    #[test]
    fn test_multiple_params() {
        demangle_eq!(
            "@SomeClass@Add$qii",
            "SomeClass::Add(int, int)"
        );
    }

    #[test]
    fn test_pointer_param() {
        demangle_eq!(
            "@SomeClass@Method$qpi",
            "SomeClass::Method(int *)"
        );
    }

    #[test]
    fn test_void_pointer_param() {
        demangle_eq!(
            "@SomeClass@Method$qpv",
            "SomeClass::Method(void *)"
        );
    }

    #[test]
    fn test_const_int_param() {
        demangle_eq!(
            "@SomeClass@Method$qxi",
            "SomeClass::Method(const int)"
        );
    }

    #[test]
    fn test_reference_param() {
        demangle_eq!(
            "@SomeClass@Method$qri",
            "SomeClass::Method(int &)"
        );
    }

    // ── Length-prefixed class type ────────────────────────────────────────────

    #[test]
    fn test_length_prefixed_class_param() {
        demangle_eq!(
            "@BankClass@Write_Save_Game_Data_To_Memory$q20MemorySaveBufferType",
            "BankClass::Write_Save_Game_Data_To_Memory(MemorySaveBufferType)"
        );
    }

    #[test]
    fn test_short_length_prefixed_type() {
        demangle_eq!(
            "@MyClass@Method$q3Foo",
            "MyClass::Method(Foo)"
        );
    }

    // ── Free functions (no class prefix) ─────────────────────────────────────

    #[test]
    fn test_free_function_void() {
        demangle_eq!("Init$qv", "Init(void)");
    }

    #[test]
    fn test_free_function_int_param() {
        demangle_eq!("Process$qi", "Process(int)");
    }

    // ── Additional types ─────────────────────────────────────────────────────

    #[test]
    fn test_long_param() {
        demangle_eq!("@Cls@Method$ql", "Cls::Method(long)");
    }

    #[test]
    fn test_long_double_param() {
        demangle_eq!("@Cls@Method$qld", "Cls::Method(long double)");
    }

    #[test]
    fn test_float_param() {
        demangle_eq!("@Cls@Method$qf", "Cls::Method(float)");
    }

    #[test]
    fn test_double_param() {
        demangle_eq!("@Cls@Method$qd", "Cls::Method(double)");
    }

    // ── Full BANK.obj symbol set ─────────────────────────────────────────────

    #[test]
    fn test_bank_obj_symbols() {
        demangle_eq!("@BankClass@$bctr$qv", "BankClass::BankClass(void)");
        demangle_eq!("@BankClass@New_Game_Reset$qv", "BankClass::New_Game_Reset(void)");
        demangle_eq!("@BankClass@Set_Houses$qi", "BankClass::Set_Houses(int)");
        demangle_eq!("@BankClass@Get_Houses$qv", "BankClass::Get_Houses(void)");
        demangle_eq!("@BankClass@Set_Hotels$qi", "BankClass::Set_Hotels(int)");
        demangle_eq!("@BankClass@Get_Hotels$qv", "BankClass::Get_Hotels(void)");
        demangle_eq!("@BankClass@Write_Save_Game_Data$qi", "BankClass::Write_Save_Game_Data(int)");
        demangle_eq!("@BankClass@Read_Save_Game_Data$qi", "BankClass::Read_Save_Game_Data(int)");
        demangle_eq!(
            "@BankClass@Write_Save_Game_Data_To_Memory$q20MemorySaveBufferType",
            "BankClass::Write_Save_Game_Data_To_Memory(MemorySaveBufferType)"
        );
        demangle_eq!(
            "@BankClass@Read_Save_Game_Data_From_Memory$q20MemorySaveBufferType",
            "BankClass::Read_Save_Game_Data_From_Memory(MemorySaveBufferType)"
        );
    }

    // ── Operator overloads ───────────────────────────────────────────────────

    #[test]
    fn test_operator_call() {
        demangle_eq!(
            "@RandomClass@$bcall$qqrii",
            "RandomClass::operator()(int, int)"
        );
    }

    #[test]
    fn test_operator_subscript() {
        demangle_eq!(
            "@Cls@$bsubs$qi",
            "Cls::operator[](int)"
        );
    }

    #[test]
    fn test_operator_add() {
        demangle_eq!(
            "@Cls@$badd$qri",
            "Cls::operator+(int &)"
        );
    }

    // ── Multi-character calling conventions ──────────────────────────────────

    #[test]
    fn test_fastcall_cc() {
        // qqr = __fastcall
        demangle_eq!(
            "@Calc_CRC$qqrpxvl",
            "Calc_CRC(const void *, long)"
        );
    }

    #[test]
    fn test_stdcall_cc() {
        // qqs = __stdcall
        demangle_eq!(
            "@Foo@Bar$qqsi",
            "Foo::Bar(int)"
        );
    }

    // ── Const member functions ───────────────────────────────────────────────

    #[test]
    fn test_const_member_function() {
        // $x before CC = const member function
        // $bfnc = operator() (function call operator)
        demangle_eq!(
            "@RandomClass@$bfnc$xqqrv",
            "RandomClass::operator()(void) const"
        );
    }

    #[test]
    fn test_const_member_cdecl() {
        demangle_eq!(
            "@Cls@Get$xqv",
            "Cls::Get(void) const"
        );
    }

    // ── Free functions with @ prefix ─────────────────────────────────────────

    #[test]
    fn test_free_function_with_at_prefix() {
        demangle_eq!(
            "@Calc_CRC$qqrpxvl",
            "Calc_CRC(const void *, long)"
        );
    }

    #[test]
    fn test_free_function_at_prefix_cdecl() {
        demangle_eq!(
            "@compfunc$qpxvt1",
            "compfunc(const void *, const void *)"
        );
    }

    // ── Nested class scopes ──────────────────────────────────────────────────

    #[test]
    fn test_nested_class_scope() {
        demangle_eq!(
            "@INIClass@INISection@Find_Entry$xqqrpxc",
            "INIClass::INISection::Find_Entry(const char *) const"
        );
    }

    // ── Back-references ──────────────────────────────────────────────────────

    #[test]
    fn test_backref_t1() {
        // t1 = repeat of first param
        demangle_eq!(
            "@compfunc$qpxvt1",
            "compfunc(const void *, const void *)"
        );
    }

    #[test]
    fn test_backref_t2() {
        demangle_eq!(
            "@Foo@Bar$qict2",
            "Foo::Bar(int, char, char)"
        );
    }

    // ── Base64_Decode free function with fastcall ────────────────────────────

    #[test]
    fn test_base64_decode() {
        demangle_eq!(
            "@Base64_Decode$qqrpxvipvi",
            "Base64_Decode(const void *, int, void *, int)"
        );
    }

    // ── Data symbols (no $ separator) ───────────────────────────────────────

    #[test]
    fn test_data_symbol_class_member() {
        demangle_eq!(
            "@BlowfishEngine@S_Init",
            "BlowfishEngine::S_Init"
        );
    }

    #[test]
    fn test_data_symbol_short_names() {
        demangle_eq!("@p_4@s", "p_4::s");
    }

    #[test]
    fn test_data_symbol_nested() {
        demangle_eq!(
            "@DataClass@AltPath",
            "DataClass::AltPath"
        );
    }

    #[test]
    fn test_data_symbol_not_valid() {
        // Single segment — not a data symbol, just a linkage-decorated name.
        demangle_none!("@JustAName");
        // Leading @@ — empty segment.
        demangle_none!("@@Bad");
    }

    // ── Scoped type names in parameters ─────────────────────────────────────

    #[test]
    fn test_scoped_type_in_param() {
        // PKPipe constructor with PKPipe::CryptControl nested type (by value)
        // and RandomStraw reference.
        demangle_eq!(
            "@PKPipe@$bctr$qqr19PKPipe@CryptControlr11RandomStraw",
            "PKPipe::PKPipe(PKPipe::CryptControl, RandomStraw &)"
        );
    }

    // ── Template data symbols (%…% template syntax) ────────────────────────

    #[test]
    fn test_template_data_symbol() {
        demangle_eq!("@%Int$ii$64%@Borrow", "Int<(int)64>::Borrow");
        demangle_eq!("@%Int$ii$64%@Error", "Int<(int)64>::Error");
        demangle_eq!("@%Int$ii$64%@Carry", "Int<(int)64>::Carry");
    }

    #[test]
    fn test_template_data_symbol_not_valid() {
        // Unbalanced '%'.
        demangle_none!("@%Int$ii$64@Borrow");
        // Single segment (template only, no member).
        demangle_none!("@%Int$ii$64%");
    }

    // ── $bfnc = operator() (function call operator) ────────────────────────

    #[test]
    fn test_bfnc_operator() {
        demangle_eq!(
            "@RandomClass@$bfnc$qqrv",
            "RandomClass::operator()(void)"
        );
    }

    // ── @@N@@ local scope prefix (switch tables, static locals) ────────────

    #[test]
    fn test_local_scope_member_function() {
        // @@N@@ form: member function (rest starts with '@' for class scope).
        // Turbo Dump: "::1::::INIClass::Get_Bool(const char*,const char*,bool) const __fastcall"
        demangle_eq!(
            "@@1@@INIClass@Get_Bool$xqqrpxct14bool@TPKT@0",
            "::1::::INIClass::Get_Bool(const char *, const char *, bool) const"
        );
    }

    #[test]
    fn test_local_scope_free_function() {
        // @@N@ form: free/C function (no class scope prefix).
        demangle_eq!(
            "@@1@_main@TPKT@0",
            "::1::::_main"
        );
    }

    // ── Free-function special operators (no class scope) ──────────────────

    #[test]
    fn test_free_operator_delete() {
        demangle_eq!("@$bdele$qpv", "operator delete(void *)");
    }

    #[test]
    fn test_free_operator_new() {
        demangle_eq!("@$bnew$qui", "operator new(unsigned int)");
    }

    #[test]
    fn test_free_operator_new_array() {
        demangle_eq!("@$bnwa$qui", "operator new[](unsigned int)");
    }

    // ── Compiler-generated exception type symbols ─────────────────────────

    #[test]
    fn test_exception_type_symbol() {
        demangle_eq!("@$xt$10UsageError", "UsageError `exception type'");
    }

    #[test]
    fn test_exception_type_inline_type_code() {
        // Inline type code: @$xt$pc = char * exception type
        demangle_eq!("@$xt$pc", "char * `exception type'");
    }

    #[test]
    fn test_exception_type_scoped() {
        // Nested scope: @$xt$19PKPipe@CryptControl
        demangle_eq!(
            "@$xt$19PKPipe@CryptControl",
            "PKPipe::CryptControl `exception type'"
        );
    }

    #[test]
    fn test_local_scope_prefix_not_valid() {
        // Not a local scope — only single '@'.
        demangle_none!("@1@@Foo@Bar$qv");
        // Empty index.
        demangle_none!("@@@@Foo@Bar$qv");
        // Non-numeric index.
        demangle_none!("@@abc@@Foo@Bar$qv");
    }

    // ── Template type parameters ($t) ───────────────────────────────────────

    #[test]
    fn test_template_type_param() {
        // %List$t17INIClass@INIEntry% → List<INIClass::INIEntry>
        demangle_eq!(
            "@%List$t17INIClass@INIEntry%@Count",
            "List<INIClass::INIEntry>::Count"
        );
    }

    #[test]
    fn test_template_type_param_simple() {
        // %Node$t11GenericList% → Node<GenericList>
        demangle_eq!(
            "@%Node$t11GenericList%@Next",
            "Node<GenericList>::Next"
        );
    }

    // ── Exception type with template ────────────────────────────────────────

    #[test]
    fn test_exception_type_template() {
        demangle_eq!(
            "@$xt$27%List$t17INIClass@INIEntry%",
            "List<INIClass::INIEntry> `exception type'"
        );
    }

    #[test]
    fn test_exception_type_template_node() {
        demangle_eq!(
            "@$xt$29%Node$t19INIClass@INISection%",
            "Node<INIClass::INISection> `exception type'"
        );
    }

    #[test]
    fn test_exception_type_template_comment_entry() {
        demangle_eq!(
            "@$xt$31%List$t21INIClass@CommentEntry%",
            "List<INIClass::CommentEntry> `exception type'"
        );
    }

    // ── Template class member functions ─────────────────────────────────────

    #[test]
    fn test_template_class_destructor() {
        demangle_eq!(
            "@%List$t17INIClass@INIEntry%@$bdtr$qqrv",
            "List<INIClass::INIEntry>::~List<INIClass::INIEntry>(void)"
        );
    }

    #[test]
    fn test_template_class_constructor() {
        demangle_eq!(
            "@%Node$t11GenericList%@$bctr$qqrv",
            "Node<GenericList>::Node<GenericList>(void)"
        );
    }

    #[test]
    fn test_template_class_method() {
        demangle_eq!(
            "@%List$t17INIClass@INIEntry%@Add$qqrp17INIClass@INIEntry",
            "List<INIClass::INIEntry>::Add(INIClass::INIEntry *)"
        );
    }

    // ── Template type in function parameters ────────────────────────────────

    #[test]
    fn test_template_in_param_list() {
        demangle_eq!(
            "@Generate_Prime$qqrr5Strawipx11%Int$ii$64%",
            "Generate_Prime(Straw &, int, const Int<(int)64> *)"
        );
    }
}
