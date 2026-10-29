/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The KVM crates' thread rule, as a check: outside tests, reverie-kvm-core,
//! reverie-kvm and reverie-linuxemu create every Rust thread with
//! `reverie_kvm_core::supervisor_thread::builder`, so every such thread gets
//! the supervisor stack (see that module for why).
//!
//! The check parses the three crates' sources with syn, from each `src/lib.rs`
//! through every `mod` it declares, resolving module files as rustc does. It
//! leaves out what is compiled only for tests: any item, statement,
//! expression, match arm, field or variant whose `cfg` cannot hold unless
//! `test`, or a feature whose name contains "test", is set. Everything else
//! is production code, and in it the check refuses:
//! - a path through `thread::spawn` or `thread::scope`, or an import of either
//!   or of all of `std::thread`;
//! - `thread::Builder::new`, or `Builder::new` where `Builder` is imported
//!   from `std::thread`, except in `builder` itself;
//! - a `spawn_scoped` call;
//! - those same paths among the tokens of a macro invocation or definition;
//! - `pthread_create`, `libc::clone`, `libc::fork`, `libc::vfork`, or a
//!   `syscall` of `SYS_clone`, `SYS_clone3`, `SYS_fork` or `SYS_vfork`,
//!   except the named exceptions.
//!
//! Comments never reach the parser and doc comments are string literals, so
//! prose that names these functions is not a finding, and test code is never
//! searched at all. In the other direction, every `.rs` file under the three
//! crates' `src` directories must be reached as production or as test code,
//! so a file this resolver misses cannot hide a spawn. C sources are checked
//! by a count of their thread and process creation calls.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use syn::ext::IdentExt;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::visit::Visit;

/// The crates the rule covers, as directories of the workspace.
const CRATES: [&str; 3] = ["reverie-kvm-core", "reverie-kvm", "reverie-linuxemu"];

/// The one place a `std::thread::Builder` may be made: the file and function.
const HELPER: (&str, &str) = ("reverie-kvm-core/src/supervisor_thread.rs", "builder");

/// Raw process or thread creation in Rust that the rule allows: the file, the
/// enclosing function and the reason.
const RAW_EXCEPTIONS: [(&str, &str, &str); 2] = [
    (
        "reverie-linuxemu/src/native_exit_broker/bootstrap.rs",
        "start_masked",
        "clone with flags 0 makes the native exit broker, a separate process \
         with its own copy of the address space that runs a syscall-only loop",
    ),
    (
        "reverie-linuxemu/src/native_exit_broker/daemon.rs",
        "spawn_job",
        "fork, in the broker process, makes one of the broker's job workers, \
         another separate process",
    ),
];

/// Thread and process creation in the crates' C sources: the file, the call,
/// how many calls there are and why each is allowed. Every other C file under
/// `src` must have none.
const C_EXCEPTIONS: [(&str, &str, usize, &str); 1] = [(
    "reverie-linuxemu/src/terminal_read.c",
    "pthread_create",
    1,
    "the terminal reader thread runs only C: pthread calls, read, atomics and \
     its own C state, which the Rust side frees after joining it",
)];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("reverie-kvm-core is a workspace member")
        .canonicalize()
        .expect("workspace directory")
}

/// A `cfg` predicate.
enum Cfg {
    Name(String),
    Value(String, String),
    Any(Vec<Cfg>),
    All(Vec<Cfg>),
    Not(Box<Cfg>),
}

impl Parse for Cfg {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let name = input.call(syn::Ident::parse_any)?.to_string();
        if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            let mut items: Vec<Cfg> = content
                .parse_terminated(Cfg::parse, syn::Token![,])?
                .into_iter()
                .collect();
            return match name.as_str() {
                "any" => Ok(Cfg::Any(items)),
                "all" => Ok(Cfg::All(items)),
                "not" if items.len() == 1 => Ok(Cfg::Not(Box::new(items.remove(0)))),
                _ => Err(input.error(format!("unknown cfg predicate {name}"))),
            };
        }
        if input.peek(syn::Token![=]) {
            input.parse::<syn::Token![=]>()?;
            let value: syn::LitStr = input.parse()?;
            return Ok(Cfg::Value(name, value.value()));
        }
        Ok(Cfg::Name(name))
    }
}

impl Cfg {
    /// The predicate's value in a build that is not a test: `test` and every
    /// feature named for tests are false, and anything else is unknown.
    fn outside_tests(&self) -> Option<bool> {
        match self {
            Cfg::Name(name) => (name == "test").then_some(false),
            Cfg::Value(key, value) => (key == "feature" && value.contains("test")).then_some(false),
            Cfg::Not(inner) => inner.outside_tests().map(|value| !value),
            Cfg::Any(items) => {
                let values: Vec<_> = items.iter().map(Cfg::outside_tests).collect();
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(|value| *value == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            }
            Cfg::All(items) => {
                let values: Vec<_> = items.iter().map(Cfg::outside_tests).collect();
                if values.contains(&Some(false)) {
                    Some(false)
                } else if values.iter().all(|value| *value == Some(true)) {
                    Some(true)
                } else {
                    None
                }
            }
        }
    }
}

/// Whether these attributes make their node test-only: a `cfg` that cannot
/// hold outside tests. An unparsable `cfg` fails the check rather than hide
/// code from it.
fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .any(|attr| {
            let cfg: Cfg = attr
                .parse_args()
                .unwrap_or_else(|error| panic!("unparsable cfg attribute: {error}"));
            cfg.outside_tests() == Some(false)
        })
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        syn::Meta::NameValue(meta) if meta.path.is_ident("path") => match &meta.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) => Some(value.value()),
            _ => None,
        },
        _ => None,
    })
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Array(expr) => &expr.attrs,
        syn::Expr::Assign(expr) => &expr.attrs,
        syn::Expr::Async(expr) => &expr.attrs,
        syn::Expr::Await(expr) => &expr.attrs,
        syn::Expr::Binary(expr) => &expr.attrs,
        syn::Expr::Block(expr) => &expr.attrs,
        syn::Expr::Break(expr) => &expr.attrs,
        syn::Expr::Call(expr) => &expr.attrs,
        syn::Expr::Cast(expr) => &expr.attrs,
        syn::Expr::Closure(expr) => &expr.attrs,
        syn::Expr::Const(expr) => &expr.attrs,
        syn::Expr::Continue(expr) => &expr.attrs,
        syn::Expr::Field(expr) => &expr.attrs,
        syn::Expr::ForLoop(expr) => &expr.attrs,
        syn::Expr::Group(expr) => &expr.attrs,
        syn::Expr::If(expr) => &expr.attrs,
        syn::Expr::Index(expr) => &expr.attrs,
        syn::Expr::Infer(expr) => &expr.attrs,
        syn::Expr::Let(expr) => &expr.attrs,
        syn::Expr::Lit(expr) => &expr.attrs,
        syn::Expr::Loop(expr) => &expr.attrs,
        syn::Expr::Macro(expr) => &expr.attrs,
        syn::Expr::Match(expr) => &expr.attrs,
        syn::Expr::MethodCall(expr) => &expr.attrs,
        syn::Expr::Paren(expr) => &expr.attrs,
        syn::Expr::Path(expr) => &expr.attrs,
        syn::Expr::Range(expr) => &expr.attrs,
        syn::Expr::RawAddr(expr) => &expr.attrs,
        syn::Expr::Reference(expr) => &expr.attrs,
        syn::Expr::Repeat(expr) => &expr.attrs,
        syn::Expr::Return(expr) => &expr.attrs,
        syn::Expr::Struct(expr) => &expr.attrs,
        syn::Expr::Try(expr) => &expr.attrs,
        syn::Expr::TryBlock(expr) => &expr.attrs,
        syn::Expr::Tuple(expr) => &expr.attrs,
        syn::Expr::Unary(expr) => &expr.attrs,
        syn::Expr::Unsafe(expr) => &expr.attrs,
        syn::Expr::While(expr) => &expr.attrs,
        syn::Expr::Yield(expr) => &expr.attrs,
        _ => &[],
    }
}

fn segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

/// Whether `names` has `first` immediately followed by `second`.
fn adjacent(names: &[String], first: &str, second: &str) -> bool {
    names
        .windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

/// The paths a `use` tree imports, each with the name it binds.
fn use_paths(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, String)>) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            use_paths(&path.tree, prefix, out);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let mut full = prefix.clone();
            let ident = name.ident.to_string();
            if ident != "self" {
                full.push(ident.clone());
            }
            let bound = if ident == "self" {
                prefix.last().cloned().unwrap_or_default()
            } else {
                ident
            };
            out.push((full, bound));
        }
        syn::UseTree::Rename(rename) => {
            let mut full = prefix.clone();
            if rename.ident != "self" {
                full.push(rename.ident.to_string());
            }
            out.push((full, rename.rename.to_string()));
        }
        syn::UseTree::Glob(_) => {
            let mut full = prefix.clone();
            full.push("*".into());
            out.push((full, "*".into()));
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                use_paths(tree, prefix, out);
            }
        }
    }
}

/// A macro's tokens as identifiers and punctuation, groups flattened.
fn macro_tokens(tokens: proc_macro2::TokenStream, out: &mut Vec<String>) {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Group(group) => macro_tokens(group.stream(), out),
            proc_macro2::TokenTree::Ident(ident) => out.push(ident.to_string()),
            proc_macro2::TokenTree::Punct(punct) => out.push(punct.as_char().to_string()),
            proc_macro2::TokenTree::Literal(_) => out.push("<literal>".into()),
        }
    }
}

/// Finds rule violations in one file's production code.
struct Finder<'a> {
    file: &'a str,
    /// Names bound to `std::thread::Builder` by this file's production imports.
    builder_names: &'a BTreeSet<String>,
    /// Enclosing modules, impls and functions, innermost last.
    context: Vec<String>,
    /// Enclosing function names, innermost last.
    functions: Vec<String>,
    findings: Vec<String>,
}

impl Finder<'_> {
    fn report(&mut self, what: String) {
        let place = if self.context.is_empty() {
            "the file's top level".to_owned()
        } else {
            self.context.join("::")
        };
        self.findings
            .push(format!("{}: in {place}: {what}", self.file));
    }

    fn in_helper(&self) -> bool {
        self.file == HELPER.0 && self.functions.last().map(String::as_str) == Some(HELPER.1)
    }

    fn raw_exception(&self) -> bool {
        RAW_EXCEPTIONS.iter().any(|(file, function, _)| {
            self.file == *file && self.functions.last().map(String::as_str) == Some(*function)
        })
    }

    fn check_names(&mut self, names: &[String], text: &str) {
        for function in ["spawn", "scope"] {
            if adjacent(names, "thread", function) {
                self.report(format!("`{text}` uses thread::{function}"));
            }
        }
        let builder_new = adjacent(names, "Builder", "new")
            && (adjacent(names, "thread", "Builder")
                || (names.len() == 2 && self.builder_names.contains(&names[0])));
        let alias_new = names.len() == 2
            && names[1] == "new"
            && names[0] != "Builder"
            && self.builder_names.contains(&names[0]);
        if (builder_new || alias_new) && !self.in_helper() {
            self.report(format!(
                "`{text}` makes a std::thread::Builder; use supervisor_thread::builder"
            ));
        }
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if test_only(item_attrs(item)) {
            return;
        }
        let name = match item {
            syn::Item::Fn(item) => Some(item.sig.ident.to_string()),
            syn::Item::Mod(item) => Some(item.ident.to_string()),
            syn::Item::Impl(item) => {
                let ty = &item.self_ty;
                Some(format!("impl {}", quote_type(ty)))
            }
            syn::Item::Trait(item) => Some(format!("trait {}", item.ident)),
            _ => None,
        };
        let is_fn = matches!(item, syn::Item::Fn(_));
        if let Some(name) = &name {
            self.context.push(name.clone());
            if is_fn {
                self.functions.push(name.clone());
            }
        }
        syn::visit::visit_item(self, item);
        if name.is_some() {
            self.context.pop();
            if is_fn {
                self.functions.pop();
            }
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let (attrs, name) = match item {
            syn::ImplItem::Fn(item) => (&item.attrs, Some(item.sig.ident.to_string())),
            syn::ImplItem::Const(item) => (&item.attrs, None),
            syn::ImplItem::Type(item) => (&item.attrs, None),
            syn::ImplItem::Macro(item) => (&item.attrs, None),
            _ => return syn::visit::visit_impl_item(self, item),
        };
        if test_only(attrs) {
            return;
        }
        if let Some(name) = &name {
            self.context.push(name.clone());
            self.functions.push(name.clone());
        }
        syn::visit::visit_impl_item(self, item);
        if name.is_some() {
            self.context.pop();
            self.functions.pop();
        }
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs = match item {
            syn::TraitItem::Fn(item) => &item.attrs,
            syn::TraitItem::Const(item) => &item.attrs,
            syn::TraitItem::Type(item) => &item.attrs,
            syn::TraitItem::Macro(item) => &item.attrs,
            _ => return syn::visit::visit_trait_item(self, item),
        };
        if !test_only(attrs) {
            syn::visit::visit_trait_item(self, item);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let skip = match stmt {
            syn::Stmt::Local(local) => test_only(&local.attrs),
            syn::Stmt::Macro(mac) => test_only(&mac.attrs),
            syn::Stmt::Expr(..) | syn::Stmt::Item(..) => false,
        };
        if !skip {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if !test_only(expr_attrs(expr)) {
            syn::visit::visit_expr(self, expr);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if !test_only(&arm.attrs) {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        if !test_only(&field.attrs) {
            syn::visit::visit_field_value(self, field);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if !test_only(&field.attrs) {
            syn::visit::visit_field(self, field);
        }
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        if !test_only(&variant.attrs) {
            syn::visit::visit_variant(self, variant);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let names = segments(path);
        let text = names.join("::");
        self.check_names(&names, &text);
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut paths = Vec::new();
        use_paths(&item.tree, &mut Vec::new(), &mut paths);
        for (path, bound) in paths {
            let text = path.join("::");
            match path.as_slice() {
                [first, thread, rest] if first == "std" && thread == "thread" => {
                    if rest == "spawn" || rest == "scope" || rest == "*" {
                        self.report(format!("imports `{text}`"));
                    }
                }
                // Under another name, `std::thread::spawn` would escape the
                // path checks.
                [first, thread] if first == "std" && thread == "thread" && bound != "thread" => {
                    self.report(format!("imports `{text}` as `{bound}`"));
                }
                _ => {}
            }
        }
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        // `<std::thread::Builder>::new`: the type is in the qualified self.
        if let Some(qself) = &expr.qself
            && let syn::Type::Path(ty) = &*qself.ty
        {
            let names = segments(&ty.path);
            let builder = names.ends_with(&["thread".to_owned(), "Builder".to_owned()])
                || (names.len() == 1 && self.builder_names.contains(&names[0]));
            let new = expr
                .path
                .segments
                .last()
                .is_some_and(|last| last.ident == "new");
            if builder && new && !self.in_helper() {
                self.report(format!(
                    "`<{}>::new` makes a std::thread::Builder; use supervisor_thread::builder",
                    names.join("::")
                ));
            }
        }
        syn::visit::visit_expr_path(self, expr);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "spawn_scoped" {
            self.report("calls spawn_scoped".into());
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(func) = &*call.func {
            let names = segments(&func.path);
            let last = names.last().map(String::as_str).unwrap_or_default();
            let first = names.first().map(String::as_str).unwrap_or_default();
            let raw = match last {
                "pthread_create" => true,
                "clone" | "fork" | "vfork" => first == "libc",
                "syscall" => call.args.first().is_some_and(|arg| match arg {
                    syn::Expr::Path(number) => segments(&number.path).last().is_some_and(|name| {
                        matches!(
                            name.as_str(),
                            "SYS_clone" | "SYS_clone3" | "SYS_fork" | "SYS_vfork"
                        )
                    }),
                    _ => false,
                }),
                _ => false,
            };
            if raw && !self.raw_exception() {
                self.report(format!(
                    "`{}` creates a thread or process outside the named exceptions",
                    names.join("::")
                ));
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let mut tokens = Vec::new();
        macro_tokens(mac.tokens.clone(), &mut tokens);
        // Paths in tokens: identifiers joined by `::`, which is two `:` tokens.
        let mut names: Vec<String> = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            let is_ident = token
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
                && token != "<literal>";
            if is_ident {
                names.push(token.clone());
                if tokens.get(index + 1).map(String::as_str) == Some(":")
                    && tokens.get(index + 2).map(String::as_str) == Some(":")
                {
                    index += 3;
                    continue;
                }
                if names.len() > 1 || names[0] == "spawn_scoped" {
                    let text = names.join("::");
                    if names.len() == 1 {
                        self.report("calls spawn_scoped in a macro".into());
                    } else {
                        self.check_names(&names, &format!("{text} (in a macro)"));
                    }
                }
                names.clear();
            } else {
                names.clear();
            }
            index += 1;
        }
        syn::visit::visit_macro(self, mac);
    }
}

fn quote_type(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => segments(&path.path).join("::"),
        _ => "<type>".into(),
    }
}

/// Where a module's items look for the files of the modules they declare.
#[derive(Clone)]
struct ModDirs {
    /// The directory of the source file that holds the items.
    file_dir: PathBuf,
    /// Where `mod name;` without `#[path]` finds `name.rs` or `name/mod.rs`.
    child_dir: PathBuf,
    /// Whether the items are inside an inline `mod name { ... }`.
    inline: bool,
}

impl ModDirs {
    fn for_file(file: &Path, mod_rs: bool) -> Self {
        let file_dir = file.parent().expect("source file directory").to_owned();
        let child_dir = if mod_rs {
            file_dir.clone()
        } else {
            file_dir.join(file.file_stem().expect("source file stem"))
        };
        Self {
            file_dir,
            child_dir,
            inline: false,
        }
    }

    fn inline_module(&self, name: &str) -> Self {
        Self {
            file_dir: self.file_dir.clone(),
            child_dir: self.child_dir.join(name),
            inline: true,
        }
    }

    /// The file of `mod name;`, and whether it resolves its own children
    /// like a mod.rs file (as a file named by `#[path]` does).
    fn resolve(&self, item: &syn::ItemMod) -> (PathBuf, bool) {
        if let Some(path) = path_attr(&item.attrs) {
            let base = if self.inline {
                &self.child_dir
            } else {
                &self.file_dir
            };
            return (base.join(path), true);
        }
        let name = item.ident.to_string();
        let flat = self.child_dir.join(format!("{name}.rs"));
        if flat.exists() {
            (flat, false)
        } else {
            (self.child_dir.join(name).join("mod.rs"), true)
        }
    }
}

#[derive(Default)]
struct Survey {
    /// Every file reached, and whether it held any production code.
    reached: BTreeMap<PathBuf, bool>,
    findings: Vec<String>,
}

impl Survey {
    fn relative(path: &Path) -> String {
        path.strip_prefix(workspace())
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn load(&mut self, path: &Path, mod_rs: bool, test: bool) {
        let path = path
            .canonicalize()
            .unwrap_or_else(|error| panic!("module file {}: {error}", path.display()));
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let file = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} does not parse: {error}", path.display()));
        let test = test || test_only(&file.attrs);
        let entry = self.reached.entry(path.clone()).or_insert(false);
        *entry |= !test;
        let dirs = ModDirs::for_file(&path, mod_rs);
        let relative = Self::relative(&path);
        let builder_names = if test {
            BTreeSet::new()
        } else {
            builder_imports(&file.items)
        };
        self.walk(&file.items, &dirs, &path, &relative, &builder_names, test);
    }

    fn walk(
        &mut self,
        items: &[syn::Item],
        dirs: &ModDirs,
        file: &Path,
        relative: &str,
        builder_names: &BTreeSet<String>,
        test: bool,
    ) {
        for item in items {
            let item_test = test || test_only(item_attrs(item));
            match item {
                syn::Item::Mod(module) => match &module.content {
                    Some((_, content)) => {
                        let inner = dirs.inline_module(&module.ident.to_string());
                        self.walk(content, &inner, file, relative, builder_names, item_test);
                    }
                    None => {
                        let (path, mod_rs) = dirs.resolve(module);
                        self.load(&path, mod_rs, item_test);
                    }
                },
                syn::Item::Macro(mac) if mac.mac.path.is_ident("include") => {
                    let literal: syn::LitStr = mac
                        .mac
                        .parse_body()
                        .unwrap_or_else(|error| panic!("{relative}: include!: {error}"));
                    let path = file.parent().unwrap().join(literal.value());
                    let source = std::fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                    let included = syn::parse_file(&source).unwrap_or_else(|error| {
                        panic!("{} does not parse: {error}", path.display())
                    });
                    let canonical = path.canonicalize().unwrap();
                    *self.reached.entry(canonical).or_insert(false) |= !item_test;
                    self.walk(
                        &included.items,
                        dirs,
                        file,
                        relative,
                        builder_names,
                        item_test,
                    );
                }
                other if !item_test => {
                    let mut finder = Finder {
                        file: relative,
                        builder_names,
                        context: Vec::new(),
                        functions: Vec::new(),
                        findings: Vec::new(),
                    };
                    finder.visit_item(other);
                    self.findings.append(&mut finder.findings);
                }
                _ => {}
            }
        }
    }
}

/// Names that a file's production `use` items bind to `std::thread::Builder`.
fn builder_imports(items: &[syn::Item]) -> BTreeSet<String> {
    struct Imports(BTreeSet<String>);
    impl<'ast> Visit<'ast> for Imports {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            if !test_only(item_attrs(item)) {
                syn::visit::visit_item(self, item);
            }
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            use_paths(&item.tree, &mut Vec::new(), &mut paths);
            for (path, bound) in paths {
                if path == ["std", "thread", "Builder"] {
                    self.0.insert(bound);
                }
            }
        }
    }
    let mut imports = Imports(BTreeSet::new());
    for item in items {
        imports.visit_item(item);
    }
    imports.0
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.canonicalize().unwrap());
        }
    }
}

/// C source with comments and string and character literals blanked out.
fn c_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest = &bytes[index..];
        if rest.starts_with(b"/*") {
            let end = source[index + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| index + 2 + end + 2);
            out.push(' ');
            index = end;
        } else if rest.starts_with(b"//") {
            let end = source[index..]
                .find('\n')
                .map_or(bytes.len(), |end| index + end);
            index = end;
        } else if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() && bytes[index] != quote {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
            }
            index += 1;
            out.push(' ');
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

/// How many times `name(` is called in C code (identifier boundaries kept).
fn c_calls(code: &str, name: &str) -> usize {
    let bytes = code.as_bytes();
    code.match_indices(name)
        .filter(|(at, _)| {
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let boundary = before.is_none_or(|b| !(b.is_ascii_alphanumeric() || b == b'_'));
            let after = code[at + name.len()..].trim_start();
            boundary && after.starts_with('(')
        })
        .count()
}

fn c_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            c_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "c") {
            out.push(path);
        }
    }
}

#[test]
fn every_production_thread_in_the_kvm_crates_gets_the_supervisor_stack() {
    let root = workspace();
    let mut survey = Survey::default();
    for krate in CRATES {
        survey.load(&root.join(krate).join("src/lib.rs"), true, false);
    }
    let mut unreached = Vec::new();
    for krate in CRATES {
        let mut files = Vec::new();
        rust_files(&root.join(krate).join("src"), &mut files);
        unreached.extend(
            files
                .into_iter()
                .filter(|file| !survey.reached.contains_key(file))
                .map(|file| Survey::relative(&file)),
        );
    }
    assert!(
        unreached.is_empty(),
        "source files the module resolver did not reach, which could hide a spawn: {unreached:#?}"
    );
    let production = survey
        .reached
        .values()
        .filter(|production| **production)
        .count();
    assert!(
        production > 50,
        "only {production} production files found; the resolver is broken"
    );
    eprintln!(
        "supervisor thread rule: {production} source files with production code, {} reached \
         only as test code",
        survey.reached.len() - production
    );
    assert!(
        survey.findings.is_empty(),
        "production threads must be built by supervisor_thread::builder:\n{}",
        survey.findings.join("\n")
    );

    let mut found = BTreeMap::new();
    for krate in CRATES {
        let mut files = Vec::new();
        c_files(&root.join(krate).join("src"), &mut files);
        for file in files {
            let code = c_code(&std::fs::read_to_string(&file).unwrap());
            for call in [
                "pthread_create",
                "clone",
                "fork",
                "vfork",
                "posix_spawn",
                "thrd_create",
            ] {
                let count = c_calls(&code, call);
                if count > 0 {
                    found.insert((Survey::relative(&file), call.to_owned()), count);
                }
            }
        }
    }
    let allowed: BTreeMap<_, _> = C_EXCEPTIONS
        .iter()
        .map(|(file, call, count, _)| (((*file).to_owned(), (*call).to_owned()), *count))
        .collect();
    assert_eq!(
        found, allowed,
        "thread or process creation in C sources must match the named exceptions"
    );
}

/// Runs the finder over one source text as if it were a production file.
fn findings_in(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let builder_names = builder_imports(&file.items);
    let mut survey = Survey::default();
    let dirs = ModDirs::for_file(Path::new("/nonexistent/lib.rs"), true);
    survey.walk(
        &file.items,
        &dirs,
        Path::new("/nonexistent/lib.rs"),
        "sample.rs",
        &builder_names,
        false,
    );
    survey.findings
}

#[test]
fn the_rule_check_finds_spawns_outside_tests_and_none_inside_them() {
    let source = r#"
        use std::thread::Builder;
        use std::thread::spawn as start;

        /// Prose may say std::thread::spawn freely.
        fn direct() {
            // So may comments: std::thread::Builder::new().
            std::thread::spawn(|| {});
            let _ = std::thread::Builder::new().name("x".into()).spawn(|| {});
            let _ = Builder::new();
            std::thread::scope(|_| {});
            #[cfg(test)]
            std::thread::spawn(|| {});
            #[cfg(not(test))]
            let _ = thread::spawn(|| {});
        }

        fn in_a_macro() {
            assert!(std::thread::spawn(|| 1).join().is_ok());
            println!("std::thread::spawn in a string is fine");
        }

        fn raw() {
            unsafe { libc::syscall(libc::SYS_clone3, 0, 0) };
            unsafe { raw::syscall(libc::SYS_clone, 0, 0, 0, 0, 0, 0) };
            let _ = std::sync::Arc::clone(&std::sync::Arc::new(1));
        }

        fn takes_a_builder(builder: std::thread::Builder) -> std::thread::Builder {
            builder
        }

        fn qualified() {
            use std::thread as threads;
            let _ = <std::thread::Builder>::new();
        }

        #[cfg(test)]
        mod tests {
            fn spawns() {
                std::thread::spawn(|| {});
                let _ = std::thread::Builder::new();
            }
        }

        #[cfg(any(test, feature = "test-support"))]
        fn support() {
            std::thread::spawn(|| {});
        }

        #[cfg(all(unix, not(test)))]
        fn production_only() {
            let _ = std::thread::Builder::new();
        }
    "#;
    let findings = findings_in(source);
    let expected = [
        "sample.rs: in the file's top level: imports `std::thread::spawn`",
        "sample.rs: in direct: `std::thread::spawn` uses thread::spawn",
        "sample.rs: in direct: `std::thread::Builder::new` makes a std::thread::Builder; use supervisor_thread::builder",
        "sample.rs: in direct: `Builder::new` makes a std::thread::Builder; use supervisor_thread::builder",
        "sample.rs: in direct: `std::thread::scope` uses thread::scope",
        "sample.rs: in direct: `thread::spawn` uses thread::spawn",
        "sample.rs: in in_a_macro: `std::thread::spawn (in a macro)` uses thread::spawn",
        "sample.rs: in raw: `libc::syscall` creates a thread or process outside the named exceptions",
        "sample.rs: in raw: `raw::syscall` creates a thread or process outside the named exceptions",
        "sample.rs: in qualified: imports `std::thread` as `threads`",
        "sample.rs: in qualified: `<std::thread::Builder>::new` makes a std::thread::Builder; use supervisor_thread::builder",
        "sample.rs: in production_only: `std::thread::Builder::new` makes a std::thread::Builder; use supervisor_thread::builder",
    ];
    assert_eq!(findings, expected, "{findings:#?}");
}
