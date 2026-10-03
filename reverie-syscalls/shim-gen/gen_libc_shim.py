#!/usr/bin/env python3
"""Generate reverie-syscalls' std-free `libc` module from libc's own source.

Input: `libc-expanded.rs`, produced in the reverie workspace on the x86_64
Linux host by

    cargo rustc -p libc@<ver> --lib -- -Zunpretty=expanded

That output is libc with every `cfg` already resolved for
x86_64-unknown-linux-gnu and every `s!`/`f!` macro expanded, so the item text is
exactly what `libc::<name>` means on the host. This script copies the named
items (and everything they reference) verbatim, drops the macro-generated trait
impls, and re-derives `Clone`/`Copy`/`Debug`/`PartialEq`/`Eq`/`Hash`, which is what
libc's `s!` macro provides with the `extra_traits` feature.

Usage: gen_libc_shim.py libc-expanded.rs ROOTS_FILE LIBC_VERSION RESOLVED_OUT > libc_shim.rs
"""

import re
import sys

src_path, roots_path, libc_version, resolved_path = sys.argv[1:5]
src = open(src_path).read()
roots = [r.strip() for r in open(roots_path) if r.strip() and not r.startswith("#")]

PRIMS = {
    "c_char", "c_double", "c_float", "c_int", "c_long", "c_longlong", "c_schar",
    "c_short", "c_uchar", "c_uint", "c_ulong", "c_ulonglong", "c_ushort", "c_void",
}

# Items whose text is taken from here rather than from libc (support types).
SUPPORT = {"Padding"}

item_re = re.compile(
    r"^(?P<indent>[ \t]*)(?P<vis>pub(?:\(crate\))? )?(?P<kind>struct|union|type|const|enum) (?P<name>(?!(?:extern|fn|unsafe)\b)[A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)


def item_end(text, start, kind):
    """Return the end offset of the item beginning at `start`."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0 and c == "}" and kind in ("struct", "union", "enum"):
                # A tuple/unit struct ends at ';' instead; braces close it here.
                return i + 1
        elif c == ";" and depth == 0:
            return i + 1
        i += 1
    raise ValueError("unterminated item")


def attrs_before(text, start):
    """Collect `#[repr]`-style attributes immediately preceding an item."""
    lines = text[:start].split("\n")
    lines.pop()  # the partial line holding the item's indentation
    out = []
    while lines:
        line = lines[-1].strip()
        if line.startswith("#[") and line.endswith("]"):
            if line.startswith("#[repr") or line.startswith("#[allow"):
                out.insert(0, line)
            lines.pop()
        elif line.startswith("///") or line.startswith("//"):
            lines.pop()
        else:
            break
    return out


defs = {}
for m in item_re.finditer(src):
    name = m.group("name")
    kind = m.group("kind")
    end = item_end(src, m.start(), kind)
    body = src[m.start():end]
    attrs = attrs_before(src, m.start())
    defs.setdefault(name, []).append((kind, attrs, body, bool(m.group("vis"))))

# Which of the standard traits libc implements for each type, and whether
# the impl is derived (`#[automatically_derived]`) or written by hand.
TRAITS = ["Clone", "Copy", "Debug", "PartialEq", "Eq", "Hash"]
impl_re = re.compile(
    r"^(?P<pre>(?:[ \t]*#\[[^\n]*\]\n)*)[ \t]*(?:unsafe )?impl(?:<[^>]*>)? (?:::core::(?:clone|marker|fmt|cmp|hash)::|hash::|fmt::)?(?P<trait>Clone|Copy|Debug|PartialEq|Eq|Hash) for (?P<name>[A-Za-z_][A-Za-z0-9_]*)\b",
    re.M,
)
impls = {}
for m in impl_re.finditer(src):
    derived = "automatically_derived" in m.group("pre")
    impls.setdefault(m.group("name"), {})[m.group("trait")] = derived

ident_re = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")

wanted = []
seen = set()
missing = []
stack = list(roots)
while stack:
    name = stack.pop(0)
    if name in seen or name in PRIMS or name in SUPPORT:
        continue
    seen.add(name)
    if name not in defs:
        missing.append(name)
        continue
    if len(defs[name]) != 1:
        kinds = [d[0] for d in defs[name]]
        sys.exit(f"ambiguous definition for {name}: {kinds}")
    kind, attrs, body, public = defs[name][0]
    wanted.append((name, kind, attrs, body, public))
    for ident in ident_re.findall(body.split("\n", 1)[1] if kind in ("struct", "union", "enum") else body.split("=", 1)[-1] if kind in ("type", "const") else body):
        if ident in defs and ident not in seen:
            stack.append(ident)
    if kind == "const":
        ty = body.split(":", 1)[1].split("=", 1)[0]
        for ident in ident_re.findall(ty):
            if ident in defs and ident not in seen:
                stack.append(ident)

if missing:
    sys.exit(f"not defined in libc-expanded.rs: {missing}")


# libc helpers whose expanded bodies use compiler-internal paths
# (`::core::panicking::panic` from `assert!`), rewritten to the expression
# they compute. The constants tests check the resulting values.
REWRITES = [
    # `u32_cast_int(x)`: asserts `size_of::<u32>() <= size_of::<c_int>()`
    # (true on x86_64), then returns `x as i32`.
    (re.compile(r"\bu32_cast_int\((0x[0-9a-fA-F]+)\)"), r"\1_u32 as c_int"),
]


def clean(body):
    body = body.replace("crate::", "")
    for pat, rep in REWRITES:
        body = pat.sub(rep, body)
    # Dedent to the item's own indentation.
    lines = body.split("\n")
    first = lines[0]
    rest = lines[1:]
    indents = [len(l) - len(l.lstrip()) for l in rest if l.strip()]
    cut = min(indents) - 4 if indents else 0
    cut = max(cut, 0)
    rest = [l[cut:] if len(l) >= cut else l.lstrip() for l in rest]
    return "\n".join([first.lstrip()] + rest)


order = {"type": 0, "const": 1, "struct": 2, "union": 2, "enum": 2}
wanted.sort(key=lambda w: (order[w[1]], w[0]))
field_re = re.compile(r"^\s*pub (?P<f>[A-Za-z_][A-Za-z0-9_]*):", re.M)

print(f"""// @generated by reverie-syscalls/shim-gen/regen.sh from libc {libc_version}
// (x86_64-unknown-linux-gnu, `-Zunpretty=expanded`). Do not edit by hand:
// regenerate, then run the reverie-syscalls host tests, which check every item
// here against `libc`.

//! The subset of `libc` that reverie-syscalls names, for builds without
//! `std`.
//!
//! On `target_os = "none"` the `libc` crate is empty. Guest memory is still
//! Linux memory, so the syscall argument types must keep glibc's x86_64
//! layouts exactly. These definitions are libc's own, copied verbatim. The
//! tests at the end of this file check, against `libc` on the host, that every
//! typedef is the same type, every constant has the same type and value, and
//! every struct has the same size, alignment, and public field offsets and
//! sizes.
//!
//! Each struct derives exactly the standard traits libc implements for it.
//! libc writes `PartialEq`/`Eq`/`Hash` by hand for `epoll_event` and
//! `sigevent`; its versions compare and hash every non-padding field, which is
//! what the derives here do, since `Padding` always compares equal and hashes
//! to nothing.

#![allow(non_camel_case_types)]
#![allow(missing_docs)]
// libc's text is copied verbatim; `FD_SETSIZE as usize` is a no-op cast on
// x86_64 that libc keeps for other targets.
#![allow(clippy::unnecessary_cast)]

use core::mem::MaybeUninit;

pub use core::ffi::{{
    c_char, c_double, c_float, c_int, c_long, c_longlong, c_schar, c_short, c_uchar, c_uint,
    c_ulong, c_ulonglong, c_ushort, c_void,
}};

/// libc's padding wrapper: uninitialized bytes that compare equal, hash to
/// nothing, and default to zero.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub(crate) struct Padding<T: Copy>(MaybeUninit<T>);

impl<T: Copy> Default for Padding<T> {{
    fn default() -> Self {{
        Self(MaybeUninit::zeroed())
    }}
}}

impl<T: Copy> core::fmt::Debug for Padding<T> {{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{
        let full_name = core::any::type_name::<Self>();
        let prefix_len = full_name.find("Padding").unwrap();
        f.pad(&full_name[prefix_len..])
    }}
}}

impl<T: Copy> core::hash::Hash for Padding<T> {{
    fn hash<H: core::hash::Hasher>(&self, _state: &mut H) {{}}
}}

impl<T: Copy> PartialEq for Padding<T> {{
    fn eq(&self, _other: &Self) -> bool {{
        true
    }}
}}

impl<T: Copy> Eq for Padding<T> {{}}
""")

for name, kind, attrs, body, public in wanted:
    print()
    text = clean(body)
    for a in attrs:
        if a.startswith("#[allow(deprecated)]"):
            continue
        print(a)
    if kind in ("struct", "union"):
        have = impls.get(name, {})
        traits = [t for t in TRAITS if t in have]
        manual = [t for t in traits if not have[t]]
        if manual:
            print(f"manual impls in libc for {name}: {manual}", file=sys.stderr)
        if traits:
            print("#[derive(" + ", ".join(traits) + ")]")
    print(text)

# The host test: every public item here against the real libc.
def names(k):
    return [w[0] for w in wanted if w[1] == k and w[4]]


def wrap(items, indent):
    """Comma-separated `items`, wrapped at 100 columns."""
    lines, line = [], indent
    for it in items:
        piece = it + ","
        if len(line) + 1 + len(piece) > 100 and line.strip():
            lines.append(line.rstrip())
            line = indent
        line += ("" if line == indent else " ") + piece
    if line.strip():
        lines.append(line.rstrip())
    return "\n".join(lines)


print("""
#[cfg(all(test, feature = "std"))]
mod tests {
    use core::mem::align_of;
    use core::mem::offset_of;
    use core::mem::size_of;

    fn field_size<T, F>(_: fn(&T) -> &F) -> usize {
        size_of::<F>()
    }

    /// Size and alignment, then each public field's offset and (unless the
    /// struct is packed, where fields cannot be borrowed) size.
    macro_rules! same_struct {
        ($name:ident $(, $field:ident)* $(,)?) => {
            same_struct!(@layout $name $(, $field)*);
            $(
                assert_eq!(
                    field_size(|s: &super::$name| &s.$field),
                    field_size(|s: &::libc::$name| &s.$field),
                    concat!("size of ", stringify!($name), ".", stringify!($field)),
                );
            )*
        };
        (packed $name:ident $(, $field:ident)* $(,)?) => {
            same_struct!(@layout $name $(, $field)*);
        };
        (@layout $name:ident $(, $field:ident)*) => {
            assert_eq!(
                size_of::<super::$name>(),
                size_of::<::libc::$name>(),
                concat!("size of ", stringify!($name)),
            );
            assert_eq!(
                align_of::<super::$name>(),
                align_of::<::libc::$name>(),
                concat!("alignment of ", stringify!($name)),
            );
            $(
                assert_eq!(
                    offset_of!(super::$name, $field),
                    offset_of!(::libc::$name, $field),
                    concat!("offset of ", stringify!($name), ".", stringify!($field)),
                );
            )*
        };
    }

    /// Each typedef must be the same type as libc's: the identity function
    /// only type-checks if it is.
    #[test]
    fn typedefs_match_libc() {
        macro_rules! same_type {
            ($($name:ident),* $(,)?) => {
                $(let _: fn(super::$name) -> ::libc::$name = |x| x;)*
            };
        }
        same_type!(""")
print(wrap(names("type"), " " * 12))
print("""        );
    }

    /// Each constant must have libc's type and value.
    #[test]
    fn constants_match_libc() {
        macro_rules! same_const {
            ($($name:ident),* $(,)?) => {
                $(assert_eq!(super::$name, ::libc::$name, stringify!($name));)*
            };
        }
        same_const!(""")
print(wrap(names("const"), " " * 12))
print("""        );
    }

    /// Each struct must have libc's layout.
    #[test]
    #[allow(deprecated)] // siginfo_t::_pad is deprecated in libc too.
    fn structs_match_libc() {""")
for name, kind, attrs, body, public in wanted:
    if kind not in ("struct", "union") or not public:
        continue
    packed = any("packed" in a for a in attrs)
    fields = field_re.findall(body.split("\n", 1)[1])
    if not fields:
        print(f"        // `{name}` has no public fields.")
    head = ("packed " if packed else "") + name
    one = f"        same_struct!({head}" + "".join(f", {f}" for f in fields) + ");"
    if len(one) <= 100:
        print(one)
    else:
        print(f"        same_struct!(\n            {head},")
        print(wrap(fields, " " * 12))
        print("        );")
print("""    }
}""")

# Record every item copied, roots and their dependencies.
with open(resolved_path, "w") as f:
    for name, kind, attrs, body, public in wanted:
        f.write(f"{kind} {name}\n")
