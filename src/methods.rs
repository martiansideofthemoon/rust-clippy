use rustc::lint::*;
use rustc::middle::const_eval::EvalHint::ExprTypeChecked;
use rustc::middle::const_eval::{ConstVal, eval_const_expr_partial};
use rustc::middle::cstore::CrateStore;
use rustc::middle::subst::{Subst, TypeSpace};
use rustc::middle::ty;
use rustc_front::hir::*;
use std::borrow::Cow;
use std::{fmt, iter};
use syntax::codemap::Span;
use syntax::ptr::P;
use utils::{get_trait_def_id, implements_trait, in_external_macro, in_macro, match_path, match_trait_method,
            match_type, method_chain_args, snippet, snippet_opt, span_lint, span_lint_and_then, span_note_and_lint,
            walk_ptrs_ty, walk_ptrs_ty_depth};
use utils::{BTREEMAP_ENTRY_PATH, DEFAULT_TRAIT_PATH, HASHMAP_ENTRY_PATH, OPTION_PATH, RESULT_PATH, STRING_PATH,
            VEC_PATH};
use utils::MethodArgs;

#[derive(Clone)]
pub struct MethodsPass;

/// **What it does:** This lint checks for `.unwrap()` calls on `Option`s.
///
/// **Why is this bad?** Usually it is better to handle the `None` case, or to at least call `.expect(_)` with a more helpful message. Still, for a lot of quick-and-dirty code, `unwrap` is a good choice, which is why this lint is `Allow` by default.
///
/// **Known problems:** None
///
/// **Example:** `x.unwrap()`
declare_lint! {
    pub OPTION_UNWRAP_USED, Allow,
    "using `Option.unwrap()`, which should at least get a better message using `expect()`"
}

/// **What it does:** This lint checks for `.unwrap()` calls on `Result`s.
///
/// **Why is this bad?** `result.unwrap()` will let the thread panic on `Err` values. Normally, you want to implement more sophisticated error handling, and propagate errors upwards with `try!`.
///
/// Even if you want to panic on errors, not all `Error`s implement good messages on display. Therefore it may be beneficial to look at the places where they may get displayed. Activate this lint to do just that.
///
/// **Known problems:** None
///
/// **Example:** `x.unwrap()`
declare_lint! {
    pub RESULT_UNWRAP_USED, Allow,
    "using `Result.unwrap()`, which might be better handled"
}

/// **What it does:** This lint checks for `.to_string()` method calls on values of type `&str`.
///
/// **Why is this bad?** This uses the whole formatting machinery just to clone a string. Using `.to_owned()` is lighter on resources. You can also consider using a [`Cow<'a, str>`](http://doc.rust-lang.org/std/borrow/enum.Cow.html) instead in some cases.
///
/// **Known problems:** None
///
/// **Example:** `s.to_string()` where `s: &str`
declare_lint! {
    pub STR_TO_STRING, Warn,
    "using `to_string()` on a str, which should be `to_owned()`"
}

/// **What it does:** This lint checks for `.to_string()` method calls on values of type `String`.
///
/// **Why is this bad?** This is an non-efficient way to clone a `String`, `.clone()` should be used
/// instead. `String` implements `ToString` mostly for generics.
///
/// **Known problems:** None
///
/// **Example:** `s.to_string()` where `s: String`
declare_lint! {
    pub STRING_TO_STRING, Warn,
    "calling `String::to_string` which is inefficient"
}

/// **What it does:** This lint checks for methods that should live in a trait implementation of a `std` trait (see [llogiq's blog post](http://llogiq.github.io/2015/07/30/traits.html) for further information) instead of an inherent implementation.
///
/// **Why is this bad?** Implementing the traits improve ergonomics for users of the code, often with very little cost. Also people seeing a `mul(..)` method may expect `*` to work equally, so you should have good reason to disappoint them.
///
/// **Known problems:** None
///
/// **Example:**
/// ```
/// struct X;
/// impl X {
///    fn add(&self, other: &X) -> X { .. }
/// }
/// ```
declare_lint! {
    pub SHOULD_IMPLEMENT_TRAIT, Warn,
    "defining a method that should be implementing a std trait"
}

/// **What it does:** This lint checks for methods with certain name prefixes and which doesn't match how self is taken. The actual rules are:
///
/// |Prefix |`self` taken        |
/// |-------|--------------------|
/// |`as_`  |`&self` or &mut self|
/// |`from_`| none               |
/// |`into_`|`self`              |
/// |`is_`  |`&self` or none     |
/// |`to_`  |`&self`             |
///
/// **Why is this bad?** Consistency breeds readability. If you follow the conventions, your users won't be surprised that they e.g. need to supply a mutable reference to a `as_..` function.
///
/// **Known problems:** None
///
/// **Example**
///
/// ```
/// impl X {
///     fn as_str(self) -> &str { .. }
/// }
/// ```
declare_lint! {
    pub WRONG_SELF_CONVENTION, Warn,
    "defining a method named with an established prefix (like \"into_\") that takes \
     `self` with the wrong convention"
}

/// **What it does:** This is the same as [`wrong_self_convention`](#wrong_self_convention), but for public items.
///
/// **Why is this bad?** See [`wrong_self_convention`](#wrong_self_convention).
///
/// **Known problems:** Actually *renaming* the function may break clients if the function is part of the public interface. In that case, be mindful of the stability guarantees you've given your users.
///
/// **Example:**
/// ```
/// impl X {
///     pub fn as_str(self) -> &str { .. }
/// }
/// ```
declare_lint! {
    pub WRONG_PUB_SELF_CONVENTION, Allow,
    "defining a public method named with an established prefix (like \"into_\") that takes \
     `self` with the wrong convention"
}

/// **What it does:** This lint checks for usage of `ok().expect(..)`.
///
/// **Why is this bad?** Because you usually call `expect()` on the `Result` directly to get a good error message.
///
/// **Known problems:** None.
///
/// **Example:** `x.ok().expect("why did I do this again?")`
declare_lint! {
    pub OK_EXPECT, Warn,
    "using `ok().expect()`, which gives worse error messages than \
     calling `expect` directly on the Result"
}

/// **What it does:** This lint checks for usage of `_.map(_).unwrap_or(_)`.
///
/// **Why is this bad?** Readability, this can be written more concisely as `_.map_or(_, _)`.
///
/// **Known problems:** None.
///
/// **Example:** `x.map(|a| a + 1).unwrap_or(0)`
declare_lint! {
    pub OPTION_MAP_UNWRAP_OR, Warn,
    "using `Option.map(f).unwrap_or(a)`, which is more succinctly expressed as \
     `map_or(a, f)`"
}

/// **What it does:** This lint `Warn`s on `_.map(_).unwrap_or_else(_)`.
///
/// **Why is this bad?** Readability, this can be written more concisely as `_.map_or_else(_, _)`.
///
/// **Known problems:** None.
///
/// **Example:** `x.map(|a| a + 1).unwrap_or_else(some_function)`
declare_lint! {
    pub OPTION_MAP_UNWRAP_OR_ELSE, Warn,
    "using `Option.map(f).unwrap_or_else(g)`, which is more succinctly expressed as \
     `map_or_else(g, f)`"
}

/// **What it does:** This lint `Warn`s on `_.filter(_).next()`.
///
/// **Why is this bad?** Readability, this can be written more concisely as `_.find(_)`.
///
/// **Known problems:** None.
///
/// **Example:** `iter.filter(|x| x == 0).next()`
declare_lint! {
    pub FILTER_NEXT, Warn,
    "using `filter(p).next()`, which is more succinctly expressed as `.find(p)`"
}

/// **What it does:** This lint `Warn`s on an iterator search (such as `find()`, `position()`, or
/// `rposition()`) followed by a call to `is_some()`.
///
/// **Why is this bad?** Readability, this can be written more concisely as `_.any(_)`.
///
/// **Known problems:** None.
///
/// **Example:** `iter.find(|x| x == 0).is_some()`
declare_lint! {
    pub SEARCH_IS_SOME, Warn,
    "using an iterator search followed by `is_some()`, which is more succinctly \
     expressed as a call to `any()`"
}

/// **What it does:** This lint `Warn`s on using `.chars().next()` on a `str` to check if it
/// starts with a given char.
///
/// **Why is this bad?** Readability, this can be written more concisely as `_.starts_with(_)`.
///
/// **Known problems:** None.
///
/// **Example:** `name.chars().next() == Some('_')`
declare_lint! {
    pub CHARS_NEXT_CMP, Warn,
    "using `.chars().next()` to check if a string starts with a char"
}

/// **What it does:** This lint checks for calls to `.or(foo(..))`, `.unwrap_or(foo(..))`, etc., and
/// suggests to use `or_else`, `unwrap_or_else`, etc., or `unwrap_or_default` instead.
///
/// **Why is this bad?** The function will always be called and potentially allocate an object
/// in expressions such as:
/// ```rust
/// foo.unwrap_or(String::new())
/// ```
/// this can instead be written:
/// ```rust
/// foo.unwrap_or_else(String::new)
/// ```
/// or
/// ```rust
/// foo.unwrap_or_default()
/// ```
///
/// **Known problems:** If the function as side-effects, not calling it will change the semantic of
/// the program, but you shouldn't rely on that anyway.
declare_lint! {
    pub OR_FUN_CALL, Warn,
    "using any `*or` method when the `*or_else` would do"
}

/// **What it does:** This lint checks for usage of `.extend(s)` on a `Vec` to extend the vector by a slice.
///
/// **Why is this bad?** Since Rust 1.6, the `extend_from_slice(_)` method is stable and at least for now faster.
///
/// **Known problems:** None.
///
/// **Example:** `my_vec.extend(&xs)`
declare_lint! {
    pub EXTEND_FROM_SLICE, Warn,
    "`.extend_from_slice(_)` is a faster way to extend a Vec by a slice"
}

/// **What it does:** This lint warns on using `.clone()` on a `Copy` type.
///
/// **Why is this bad?** The only reason `Copy` types implement `Clone` is for generics, not for
/// using the `clone` method on a concrete type.
///
/// **Known problems:** None.
///
/// **Example:** `42u64.clone()`
declare_lint! {
    pub CLONE_ON_COPY, Warn, "using `clone` on a `Copy` type"
}

/// **What it does:** This lint warns on using `.clone()` on an `&&T`
///
/// **Why is this bad?** Cloning an `&&T` copies the inner `&T`, instead of cloning the underlying
/// `T`
///
/// **Known problems:** None.
///
/// **Example:**
/// ```rust
/// fn main() {
///    let x = vec![1];
///    let y = &&x;
///    let z = y.clone();
///    println!("{:p} {:p}",*y, z); // prints out the same pointer
/// }
/// ```
declare_lint! {
    pub CLONE_DOUBLE_REF, Warn, "using `clone` on `&&T`"
}

/// **What it does:** This lint warns about `new` not returning `Self`.
///
/// **Why is this bad?** As a convention, `new` methods are used to make a new instance of a type.
///
/// **Known problems:** None.
///
/// **Example:**
/// ```rust
/// impl Foo {
///     fn new(..) -> NotAFoo {
///     }
/// }
/// ```
declare_lint! {
    pub NEW_RET_NO_SELF, Warn, "not returning `Self` in a `new` method"
}

/// **What it does:** This lint checks for string methods that receive a single-character `str` as an argument, e.g. `_.split("x")`.
///
/// **Why is this bad?** Performing these methods using a `char` is faster than using a `str`.
///
/// **Known problems:** Does not catch multi-byte unicode characters.
///
/// **Example:** `_.split("x")` could be `_.split('x')`
declare_lint! {
    pub SINGLE_CHAR_PATTERN,
    Warn,
    "using a single-character str where a char could be used, e.g. \
     `_.split(\"x\")`"
}

impl LintPass for MethodsPass {
    fn get_lints(&self) -> LintArray {
        lint_array!(EXTEND_FROM_SLICE,
                    OPTION_UNWRAP_USED,
                    RESULT_UNWRAP_USED,
                    STR_TO_STRING,
                    STRING_TO_STRING,
                    SHOULD_IMPLEMENT_TRAIT,
                    WRONG_SELF_CONVENTION,
                    WRONG_PUB_SELF_CONVENTION,
                    OK_EXPECT,
                    OPTION_MAP_UNWRAP_OR,
                    OPTION_MAP_UNWRAP_OR_ELSE,
                    OR_FUN_CALL,
                    CHARS_NEXT_CMP,
                    CLONE_ON_COPY,
                    CLONE_DOUBLE_REF,
                    NEW_RET_NO_SELF,
                    SINGLE_CHAR_PATTERN)
    }
}

impl LateLintPass for MethodsPass {
    fn check_expr(&mut self, cx: &LateContext, expr: &Expr) {
        if in_macro(cx, expr.span) {
            return;
        }

        match expr.node {
            ExprMethodCall(name, _, ref args) => {
                // Chain calls
                if let Some(arglists) = method_chain_args(expr, &["unwrap"]) {
                    lint_unwrap(cx, expr, arglists[0]);
                } else if let Some(arglists) = method_chain_args(expr, &["to_string"]) {
                    lint_to_string(cx, expr, arglists[0]);
                } else if let Some(arglists) = method_chain_args(expr, &["ok", "expect"]) {
                    lint_ok_expect(cx, expr, arglists[0]);
                } else if let Some(arglists) = method_chain_args(expr, &["map", "unwrap_or"]) {
                    lint_map_unwrap_or(cx, expr, arglists[0], arglists[1]);
                } else if let Some(arglists) = method_chain_args(expr, &["map", "unwrap_or_else"]) {
                    lint_map_unwrap_or_else(cx, expr, arglists[0], arglists[1]);
                } else if let Some(arglists) = method_chain_args(expr, &["filter", "next"]) {
                    lint_filter_next(cx, expr, arglists[0]);
                } else if let Some(arglists) = method_chain_args(expr, &["find", "is_some"]) {
                    lint_search_is_some(cx, expr, "find", arglists[0], arglists[1]);
                } else if let Some(arglists) = method_chain_args(expr, &["position", "is_some"]) {
                    lint_search_is_some(cx, expr, "position", arglists[0], arglists[1]);
                } else if let Some(arglists) = method_chain_args(expr, &["rposition", "is_some"]) {
                    lint_search_is_some(cx, expr, "rposition", arglists[0], arglists[1]);
                } else if let Some(arglists) = method_chain_args(expr, &["extend"]) {
                    lint_extend(cx, expr, arglists[0]);
                }
                lint_or_fun_call(cx, expr, &name.node.as_str(), &args);
                if args.len() == 1 && name.node.as_str() == "clone" {
                    lint_clone_on_copy(cx, expr);
                    lint_clone_double_ref(cx, expr, &args[0]);
                }
                for &(method, pos) in &PATTERN_METHODS {
                    if name.node.as_str() == method && args.len() > pos {
                        lint_single_char_pattern(cx, expr, &args[pos]);
                    }
                }
            }
            ExprBinary(op, ref lhs, ref rhs) if op.node == BiEq || op.node == BiNe => {
                if !lint_chars_next(cx, expr, lhs, rhs, op.node == BiEq) {
                    lint_chars_next(cx, expr, rhs, lhs, op.node == BiEq);
                }
            }
            _ => (),
        }
    }

    fn check_item(&mut self, cx: &LateContext, item: &Item) {
        if in_external_macro(cx, item.span) {
            return;
        }

        if let ItemImpl(_, _, _, None, _, ref items) = item.node {
            for implitem in items {
                let name = implitem.name;
                if let ImplItemKind::Method(ref sig, _) = implitem.node {
                    // check missing trait implementations
                    for &(method_name, n_args, self_kind, out_type, trait_name) in &TRAIT_METHODS {
                        if_let_chain! {
                            [
                                name.as_str() == method_name,
                                sig.decl.inputs.len() == n_args,
                                out_type.matches(&sig.decl.output),
                                self_kind.matches(&sig.explicit_self.node, false)
                            ], {
                                span_lint(cx, SHOULD_IMPLEMENT_TRAIT, implitem.span, &format!(
                                    "defining a method called `{}` on this type; consider implementing \
                                     the `{}` trait or choosing a less ambiguous name", name, trait_name));
                            }
                        }
                    }

                    // check conventions w.r.t. conversion method names and predicates
                    let ty = cx.tcx.lookup_item_type(cx.tcx.map.local_def_id(item.id)).ty;
                    let is_copy = is_copy(cx, &ty, &item);
                    for &(ref conv, self_kinds) in &CONVENTIONS {
                        if conv.check(&name.as_str()) &&
                           !self_kinds.iter().any(|k| k.matches(&sig.explicit_self.node, is_copy)) {
                            let lint = if item.vis == Visibility::Public {
                                WRONG_PUB_SELF_CONVENTION
                            } else {
                                WRONG_SELF_CONVENTION
                            };
                            span_lint(cx,
                                      lint,
                                      sig.explicit_self.span,
                                      &format!("methods called `{}` usually take {}; consider choosing a less \
                                                ambiguous name",
                                               conv,
                                               &self_kinds.iter()
                                                          .map(|k| k.description())
                                                          .collect::<Vec<_>>()
                                                          .join(" or ")));
                        }
                    }

                    if &name.as_str() == &"new" {
                        let returns_self = if let FunctionRetTy::Return(ref ret_ty) = sig.decl.output {
                            let ast_ty_to_ty_cache = cx.tcx.ast_ty_to_ty_cache.borrow();
                            let ret_ty = ast_ty_to_ty_cache.get(&ret_ty.id);

                            if let Some(&ret_ty) = ret_ty {
                                ret_ty.walk().any(|t| t == ty)
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if !returns_self {
                            span_lint(cx,
                                      NEW_RET_NO_SELF,
                                      sig.explicit_self.span,
                                      "methods called `new` usually return `Self`");
                        }
                    }
                }
            }
        }
    }
}

/// Checks for the `OR_FUN_CALL` lint.
fn lint_or_fun_call(cx: &LateContext, expr: &Expr, name: &str, args: &[P<Expr>]) {
    /// Check for `unwrap_or(T::new())` or `unwrap_or(T::default())`.
    fn check_unwrap_or_default(cx: &LateContext, name: &str, fun: &Expr, self_expr: &Expr, arg: &Expr,
                               or_has_args: bool, span: Span)
                               -> bool {
        if or_has_args {
            return false;
        }

        if name == "unwrap_or" {
            if let ExprPath(_, ref path) = fun.node {
                let path: &str = &path.segments
                                      .last()
                                      .expect("A path must have at least one segment")
                                      .identifier
                                      .name
                                      .as_str();

                if ["default", "new"].contains(&path) {
                    let arg_ty = cx.tcx.expr_ty(arg);
                    let default_trait_id = if let Some(default_trait_id) = get_trait_def_id(cx, &DEFAULT_TRAIT_PATH) {
                        default_trait_id
                    } else {
                        return false;
                    };

                    if implements_trait(cx, arg_ty, default_trait_id, None) {
                        span_lint(cx,
                                  OR_FUN_CALL,
                                  span,
                                  &format!("use of `{}` followed by a call to `{}`", name, path))
                            .span_suggestion(span,
                                             "try this",
                                             format!("{}.unwrap_or_default()", snippet(cx, self_expr.span, "_")));
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check for `*or(foo())`.
    fn check_general_case(cx: &LateContext, name: &str, fun: &Expr, self_expr: &Expr, arg: &Expr, or_has_args: bool,
                          span: Span) {
        // (path, fn_has_argument, methods)
        let know_types: &[(&[_], _, &[_], _)] = &[(&BTREEMAP_ENTRY_PATH, false, &["or_insert"], "with"),
                                                  (&HASHMAP_ENTRY_PATH, false, &["or_insert"], "with"),
                                                  (&OPTION_PATH,
                                                   false,
                                                   &["map_or", "ok_or", "or", "unwrap_or"],
                                                   "else"),
                                                  (&RESULT_PATH, true, &["or", "unwrap_or"], "else")];

        let self_ty = cx.tcx.expr_ty(self_expr);

        let (fn_has_arguments, poss, suffix) = if let Some(&(_, fn_has_arguments, poss, suffix)) =
                                                      know_types.iter().find(|&&i| match_type(cx, self_ty, i.0)) {
            (fn_has_arguments, poss, suffix)
        } else {
            return;
        };

        if !poss.contains(&name) {
            return;
        }

        let sugg: Cow<_> = match (fn_has_arguments, !or_has_args) {
            (true, _) => format!("|_| {}", snippet(cx, arg.span, "..")).into(),
            (false, false) => format!("|| {}", snippet(cx, arg.span, "..")).into(),
            (false, true) => snippet(cx, fun.span, ".."),
        };

        span_lint(cx, OR_FUN_CALL, span, &format!("use of `{}` followed by a function call", name))
            .span_suggestion(span,
                             "try this",
                             format!("{}.{}_{}({})", snippet(cx, self_expr.span, "_"), name, suffix, sugg));
    }

    if args.len() == 2 {
        if let ExprCall(ref fun, ref or_args) = args[1].node {
            let or_has_args = !or_args.is_empty();
            if !check_unwrap_or_default(cx, name, fun, &args[0], &args[1], or_has_args, expr.span) {
                check_general_case(cx, name, fun, &args[0], &args[1], or_has_args, expr.span);
            }
        }
    }
}

/// Checks for the `CLONE_ON_COPY` lint.
fn lint_clone_on_copy(cx: &LateContext, expr: &Expr) {
    let ty = cx.tcx.expr_ty(expr);
    let parent = cx.tcx.map.get_parent(expr.id);
    let parameter_environment = ty::ParameterEnvironment::for_item(cx.tcx, parent);

    if !ty.moves_by_default(&parameter_environment, expr.span) {
        span_lint(cx, CLONE_ON_COPY, expr.span, "using `clone` on a `Copy` type");
    }
}

/// Checks for the `CLONE_DOUBLE_REF` lint.
fn lint_clone_double_ref(cx: &LateContext, expr: &Expr, arg: &Expr) {
    let ty = cx.tcx.expr_ty(arg);
    if let ty::TyRef(_, ty::TypeAndMut { ty: ref inner, .. }) = ty.sty {
        if let ty::TyRef(..) = inner.sty {
            let mut db = span_lint(cx,
                                   CLONE_DOUBLE_REF,
                                   expr.span,
                                   "using `clone` on a double-reference; \
                                    this will copy the reference instead of cloning \
                                    the inner type");
            if let Some(snip) = snippet_opt(cx, arg.span) {
                db.span_suggestion(expr.span, "try dereferencing it", format!("(*{}).clone()", snip));
            }
        }
    }
}

fn lint_extend(cx: &LateContext, expr: &Expr, args: &MethodArgs) {
    let (obj_ty, _) = walk_ptrs_ty_depth(cx.tcx.expr_ty(&args[0]));
    if !match_type(cx, obj_ty, &VEC_PATH) {
        return;
    }
    let arg_ty = cx.tcx.expr_ty(&args[1]);
    if let Some((span, r)) = derefs_to_slice(cx, &args[1], &arg_ty) {
        span_lint(cx, EXTEND_FROM_SLICE, expr.span, "use of `extend` to extend a Vec by a slice")
            .span_suggestion(expr.span,
                             "try this",
                             format!("{}.extend_from_slice({}{})",
                                     snippet(cx, args[0].span, "_"),
                                     r,
                                     snippet(cx, span, "_")));
    }
}

fn derefs_to_slice(cx: &LateContext, expr: &Expr, ty: &ty::Ty) -> Option<(Span, &'static str)> {
    fn may_slice(cx: &LateContext, ty: &ty::Ty) -> bool {
        match ty.sty {
            ty::TySlice(_) => true,
            ty::TyStruct(..) => match_type(cx, ty, &VEC_PATH),
            ty::TyArray(_, size) => size < 32,
            ty::TyRef(_, ty::TypeAndMut { ty: ref inner, .. }) |
            ty::TyBox(ref inner) => may_slice(cx, inner),
            _ => false,
        }
    }
    if let ExprMethodCall(name, _, ref args) = expr.node {
        if &name.node.as_str() == &"iter" && may_slice(cx, &cx.tcx.expr_ty(&args[0])) {
            Some((args[0].span, "&"))
        } else {
            None
        }
    } else {
        match ty.sty {
            ty::TySlice(_) => Some((expr.span, "")),
            ty::TyRef(_, ty::TypeAndMut { ty: ref inner, .. }) |
            ty::TyBox(ref inner) => {
                if may_slice(cx, inner) {
                    Some((expr.span, ""))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `unwrap()` for `Option`s and `Result`s
fn lint_unwrap(cx: &LateContext, expr: &Expr, unwrap_args: &MethodArgs) {
    let (obj_ty, _) = walk_ptrs_ty_depth(cx.tcx.expr_ty(&unwrap_args[0]));

    let mess = if match_type(cx, obj_ty, &OPTION_PATH) {
        Some((OPTION_UNWRAP_USED, "an Option", "None"))
    } else if match_type(cx, obj_ty, &RESULT_PATH) {
        Some((RESULT_UNWRAP_USED, "a Result", "Err"))
    } else {
        None
    };

    if let Some((lint, kind, none_value)) = mess {
        span_lint(cx,
                  lint,
                  expr.span,
                  &format!("used unwrap() on {} value. If you don't want to handle the {} case gracefully, consider \
                            using expect() to provide a better panic
                            message",
                           kind,
                           none_value));
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `to_string()` for `&str`s and `String`s
fn lint_to_string(cx: &LateContext, expr: &Expr, to_string_args: &MethodArgs) {
    let (obj_ty, ptr_depth) = walk_ptrs_ty_depth(cx.tcx.expr_ty(&to_string_args[0]));

    if obj_ty.sty == ty::TyStr {
        let mut arg_str = snippet(cx, to_string_args[0].span, "_");
        if ptr_depth > 1 {
            arg_str = Cow::Owned(format!("({}{})", iter::repeat('*').take(ptr_depth - 1).collect::<String>(), arg_str));
        }
        span_lint(cx, STR_TO_STRING, expr.span, &format!("`{}.to_owned()` is faster", arg_str));
    } else if match_type(cx, obj_ty, &STRING_PATH) {
        span_lint(cx,
                  STRING_TO_STRING,
                  expr.span,
                  "`String::to_string` is an inefficient way to clone a `String`; use `clone()` instead");
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `ok().expect()` for `Result`s
fn lint_ok_expect(cx: &LateContext, expr: &Expr, ok_args: &MethodArgs) {
    // lint if the caller of `ok()` is a `Result`
    if match_type(cx, cx.tcx.expr_ty(&ok_args[0]), &RESULT_PATH) {
        let result_type = cx.tcx.expr_ty(&ok_args[0]);
        if let Some(error_type) = get_error_type(cx, result_type) {
            if has_debug_impl(error_type, cx) {
                span_lint(cx,
                          OK_EXPECT,
                          expr.span,
                          "called `ok().expect()` on a Result value. You can call `expect` directly on the `Result`");
            }
        }
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `map().unwrap_or()` for `Option`s
fn lint_map_unwrap_or(cx: &LateContext, expr: &Expr, map_args: &MethodArgs, unwrap_args: &MethodArgs) {
    // lint if the caller of `map()` is an `Option`
    if match_type(cx, cx.tcx.expr_ty(&map_args[0]), &OPTION_PATH) {
        // lint message
        let msg = "called `map(f).unwrap_or(a)` on an Option value. This can be done more directly by calling \
                   `map_or(a, f)` instead";
        // get snippets for args to map() and unwrap_or()
        let map_snippet = snippet(cx, map_args[1].span, "..");
        let unwrap_snippet = snippet(cx, unwrap_args[1].span, "..");
        // lint, with note if neither arg is > 1 line and both map() and
        // unwrap_or() have the same span
        let multiline = map_snippet.lines().count() > 1 || unwrap_snippet.lines().count() > 1;
        let same_span = map_args[1].span.expn_id == unwrap_args[1].span.expn_id;
        if same_span && !multiline {
            span_note_and_lint(cx,
                               OPTION_MAP_UNWRAP_OR,
                               expr.span,
                               msg,
                               expr.span,
                               &format!("replace `map({0}).unwrap_or({1})` with `map_or({1}, {0})`",
                                        map_snippet,
                                        unwrap_snippet));
        } else if same_span && multiline {
            span_lint(cx, OPTION_MAP_UNWRAP_OR, expr.span, msg);
        };
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `map().unwrap_or_else()` for `Option`s
fn lint_map_unwrap_or_else(cx: &LateContext, expr: &Expr, map_args: &MethodArgs, unwrap_args: &MethodArgs) {
    // lint if the caller of `map()` is an `Option`
    if match_type(cx, cx.tcx.expr_ty(&map_args[0]), &OPTION_PATH) {
        // lint message
        let msg = "called `map(f).unwrap_or_else(g)` on an Option value. This can be done more directly by calling \
                   `map_or_else(g, f)` instead";
        // get snippets for args to map() and unwrap_or_else()
        let map_snippet = snippet(cx, map_args[1].span, "..");
        let unwrap_snippet = snippet(cx, unwrap_args[1].span, "..");
        // lint, with note if neither arg is > 1 line and both map() and
        // unwrap_or_else() have the same span
        let multiline = map_snippet.lines().count() > 1 || unwrap_snippet.lines().count() > 1;
        let same_span = map_args[1].span.expn_id == unwrap_args[1].span.expn_id;
        if same_span && !multiline {
            span_note_and_lint(cx,
                               OPTION_MAP_UNWRAP_OR_ELSE,
                               expr.span,
                               msg,
                               expr.span,
                               &format!("replace `map({0}).unwrap_or_else({1})` with `with map_or_else({1}, {0})`",
                                        map_snippet,
                                        unwrap_snippet));
        } else if same_span && multiline {
            span_lint(cx, OPTION_MAP_UNWRAP_OR_ELSE, expr.span, msg);
        };
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint use of `filter().next() for Iterators`
fn lint_filter_next(cx: &LateContext, expr: &Expr, filter_args: &MethodArgs) {
    // lint if caller of `.filter().next()` is an Iterator
    if match_trait_method(cx, expr, &["core", "iter", "Iterator"]) {
        let msg = "called `filter(p).next()` on an Iterator. This is more succinctly expressed by calling `.find(p)` \
                   instead.";
        let filter_snippet = snippet(cx, filter_args[1].span, "..");
        if filter_snippet.lines().count() <= 1 {
            // add note if not multi-line
            span_note_and_lint(cx,
                               FILTER_NEXT,
                               expr.span,
                               msg,
                               expr.span,
                               &format!("replace `filter({0}).next()` with `find({0})`", filter_snippet));
        } else {
            span_lint(cx, FILTER_NEXT, expr.span, msg);
        }
    }
}

#[allow(ptr_arg)]
// Type of MethodArgs is potentially a Vec
/// lint searching an Iterator followed by `is_some()`
fn lint_search_is_some(cx: &LateContext, expr: &Expr, search_method: &str, search_args: &MethodArgs,
                       is_some_args: &MethodArgs) {
    // lint if caller of search is an Iterator
    if match_trait_method(cx, &*is_some_args[0], &["core", "iter", "Iterator"]) {
        let msg = format!("called `is_some()` after searching an iterator with {}. This is more succinctly expressed \
                           by calling `any()`.",
                          search_method);
        let search_snippet = snippet(cx, search_args[1].span, "..");
        if search_snippet.lines().count() <= 1 {
            // add note if not multi-line
            span_note_and_lint(cx,
                               SEARCH_IS_SOME,
                               expr.span,
                               &msg,
                               expr.span,
                               &format!("replace `{0}({1}).is_some()` with `any({1})`", search_method, search_snippet));
        } else {
            span_lint(cx, SEARCH_IS_SOME, expr.span, &msg);
        }
    }
}

/// Checks for the `CHARS_NEXT_CMP` lint.
fn lint_chars_next(cx: &LateContext, expr: &Expr, chain: &Expr, other: &Expr, eq: bool) -> bool {
    if_let_chain! {[
        let Some(args) = method_chain_args(chain, &["chars", "next"]),
        let ExprCall(ref fun, ref arg_char) = other.node,
        arg_char.len() == 1,
        let ExprPath(None, ref path) = fun.node,
        path.segments.len() == 1 && path.segments[0].identifier.name.as_str() == "Some"
    ], {
        let self_ty = walk_ptrs_ty(cx.tcx.expr_ty_adjusted(&args[0][0]));

        if self_ty.sty != ty::TyStr {
            return false;
        }

        span_lint_and_then(cx,
                           CHARS_NEXT_CMP,
                           expr.span,
                           "you should use the `starts_with` method",
                           |db| {
                               let sugg = format!("{}{}.starts_with({})",
                                                  if eq { "" } else { "!" },
                                                  snippet(cx, args[0][0].span, "_"),
                                                  snippet(cx, arg_char[0].span, "_")
                                                  );

                               db.span_suggestion(expr.span, "like this", sugg);
                           });

        return true;
    }}

    false
}

/// lint for length-1 `str`s for methods in `PATTERN_METHODS`
fn lint_single_char_pattern(cx: &LateContext, expr: &Expr, arg: &Expr) {
    if let Ok(ConstVal::Str(r)) = eval_const_expr_partial(cx.tcx, arg, ExprTypeChecked, None) {
        if r.len() == 1 {
            let hint = snippet(cx, expr.span, "..").replace(&format!("\"{}\"", r), &format!("'{}'", r));
            span_lint_and_then(cx,
                               SINGLE_CHAR_PATTERN,
                               arg.span,
                               "single-character string constant used as pattern",
                               |db| {
                                   db.span_suggestion(expr.span, "try using a char instead:", hint);
                               });
        }
    }
}

/// Given a `Result<T, E>` type, return its error type (`E`).
fn get_error_type<'a>(cx: &LateContext, ty: ty::Ty<'a>) -> Option<ty::Ty<'a>> {
    if !match_type(cx, ty, &RESULT_PATH) {
        return None;
    }
    if let ty::TyEnum(_, substs) = ty.sty {
        if let Some(err_ty) = substs.types.opt_get(TypeSpace, 1) {
            return Some(err_ty);
        }
    }
    None
}

/// This checks whether a given type is known to implement Debug.
fn has_debug_impl<'a, 'b>(ty: ty::Ty<'a>, cx: &LateContext<'b, 'a>) -> bool {
    match cx.tcx.lang_items.debug_trait() {
        Some(debug) => implements_trait(cx, ty, debug, Some(vec![])),
        None => false,
    }
}

enum Convention {
    Eq(&'static str),
    StartsWith(&'static str),
}

#[cfg_attr(rustfmt, rustfmt_skip)]
const CONVENTIONS: [(Convention, &'static [SelfKind]); 6] = [
    (Convention::Eq("new"), &[SelfKind::No]),
    (Convention::StartsWith("as_"), &[SelfKind::Ref, SelfKind::RefMut]),
    (Convention::StartsWith("from_"), &[SelfKind::No]),
    (Convention::StartsWith("into_"), &[SelfKind::Value]),
    (Convention::StartsWith("is_"), &[SelfKind::Ref, SelfKind::No]),
    (Convention::StartsWith("to_"), &[SelfKind::Ref]),
];

#[cfg_attr(rustfmt, rustfmt_skip)]
const TRAIT_METHODS: [(&'static str, usize, SelfKind, OutType, &'static str); 30] = [
    ("add", 2, SelfKind::Value, OutType::Any, "std::ops::Add"),
    ("as_mut", 1, SelfKind::RefMut, OutType::Ref, "std::convert::AsMut"),
    ("as_ref", 1, SelfKind::Ref, OutType::Ref, "std::convert::AsRef"),
    ("bitand", 2, SelfKind::Value, OutType::Any, "std::ops::BitAnd"),
    ("bitor", 2, SelfKind::Value, OutType::Any, "std::ops::BitOr"),
    ("bitxor", 2, SelfKind::Value, OutType::Any, "std::ops::BitXor"),
    ("borrow", 1, SelfKind::Ref, OutType::Ref, "std::borrow::Borrow"),
    ("borrow_mut", 1, SelfKind::RefMut, OutType::Ref, "std::borrow::BorrowMut"),
    ("clone", 1, SelfKind::Ref, OutType::Any, "std::clone::Clone"),
    ("cmp", 2, SelfKind::Ref, OutType::Any, "std::cmp::Ord"),
    ("default", 0, SelfKind::No, OutType::Any, "std::default::Default"),
    ("deref", 1, SelfKind::Ref, OutType::Ref, "std::ops::Deref"),
    ("deref_mut", 1, SelfKind::RefMut, OutType::Ref, "std::ops::DerefMut"),
    ("div", 2, SelfKind::Value, OutType::Any, "std::ops::Div"),
    ("drop", 1, SelfKind::RefMut, OutType::Unit, "std::ops::Drop"),
    ("eq", 2, SelfKind::Ref, OutType::Bool, "std::cmp::PartialEq"),
    ("from_iter", 1, SelfKind::No, OutType::Any, "std::iter::FromIterator"),
    ("from_str", 1, SelfKind::No, OutType::Any, "std::str::FromStr"),
    ("hash", 2, SelfKind::Ref, OutType::Unit, "std::hash::Hash"),
    ("index", 2, SelfKind::Ref, OutType::Ref, "std::ops::Index"),
    ("index_mut", 2, SelfKind::RefMut, OutType::Ref, "std::ops::IndexMut"),
    ("into_iter", 1, SelfKind::Value, OutType::Any, "std::iter::IntoIterator"),
    ("mul", 2, SelfKind::Value, OutType::Any, "std::ops::Mul"),
    ("neg", 1, SelfKind::Value, OutType::Any, "std::ops::Neg"),
    ("next", 1, SelfKind::RefMut, OutType::Any, "std::iter::Iterator"),
    ("not", 1, SelfKind::Value, OutType::Any, "std::ops::Not"),
    ("rem", 2, SelfKind::Value, OutType::Any, "std::ops::Rem"),
    ("shl", 2, SelfKind::Value, OutType::Any, "std::ops::Shl"),
    ("shr", 2, SelfKind::Value, OutType::Any, "std::ops::Shr"),
    ("sub", 2, SelfKind::Value, OutType::Any, "std::ops::Sub"),
];

#[cfg_attr(rustfmt, rustfmt_skip)]
const PATTERN_METHODS: [(&'static str, usize); 17] = [
    ("contains", 1),
    ("starts_with", 1),
    ("ends_with", 1),
    ("find", 1),
    ("rfind", 1),
    ("split", 1),
    ("rsplit", 1),
    ("split_terminator", 1),
    ("rsplit_terminator", 1),
    ("splitn", 2),
    ("rsplitn", 2),
    ("matches", 1),
    ("rmatches", 1),
    ("match_indices", 1),
    ("rmatch_indices", 1),
    ("trim_left_matches", 1),
    ("trim_right_matches", 1),
];


#[derive(Clone, Copy)]
enum SelfKind {
    Value,
    Ref,
    RefMut,
    No,
}

impl SelfKind {
    fn matches(&self, slf: &ExplicitSelf_, allow_value_for_ref: bool) -> bool {
        match (self, slf) {
            (&SelfKind::Value, &SelfValue(_)) |
            (&SelfKind::Ref, &SelfRegion(_, Mutability::MutImmutable, _)) |
            (&SelfKind::RefMut, &SelfRegion(_, Mutability::MutMutable, _)) |
            (&SelfKind::No, &SelfStatic) => true,
            (&SelfKind::Ref, &SelfValue(_)) | (&SelfKind::RefMut, &SelfValue(_)) => allow_value_for_ref,
            (_, &SelfExplicit(ref ty, _)) => self.matches_explicit_type(ty, allow_value_for_ref),
            _ => false,
        }
    }

    fn matches_explicit_type(&self, ty: &Ty, allow_value_for_ref: bool) -> bool {
        match (self, &ty.node) {
            (&SelfKind::Value, &TyPath(..)) |
            (&SelfKind::Ref, &TyRptr(_, MutTy { mutbl: Mutability::MutImmutable, .. })) |
            (&SelfKind::RefMut, &TyRptr(_, MutTy { mutbl: Mutability::MutMutable, .. })) => true,
            (&SelfKind::Ref, &TyPath(..)) |
            (&SelfKind::RefMut, &TyPath(..)) => allow_value_for_ref,
            _ => false,
        }
    }

    fn description(&self) -> &'static str {
        match *self {
            SelfKind::Value => "self by value",
            SelfKind::Ref => "self by reference",
            SelfKind::RefMut => "self by mutable reference",
            SelfKind::No => "no self",
        }
    }
}

impl Convention {
    fn check(&self, other: &str) -> bool {
        match *self {
            Convention::Eq(this) => this == other,
            Convention::StartsWith(this) => other.starts_with(this),
        }
    }
}

impl fmt::Display for Convention {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            Convention::Eq(this) => this.fmt(f),
            Convention::StartsWith(this) => this.fmt(f).and_then(|_| '*'.fmt(f)),
        }
    }
}

#[derive(Clone, Copy)]
enum OutType {
    Unit,
    Bool,
    Any,
    Ref,
}

impl OutType {
    fn matches(&self, ty: &FunctionRetTy) -> bool {
        match (self, ty) {
            (&OutType::Unit, &DefaultReturn(_)) => true,
            (&OutType::Unit, &Return(ref ty)) if ty.node == TyTup(vec![].into()) => true,
            (&OutType::Bool, &Return(ref ty)) if is_bool(ty) => true,
            (&OutType::Any, &Return(ref ty)) if ty.node != TyTup(vec![].into()) => true,
            (&OutType::Ref, &Return(ref ty)) => {
                if let TyRptr(_, _) = ty.node {
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

fn is_bool(ty: &Ty) -> bool {
    if let TyPath(None, ref p) = ty.node {
        if match_path(p, &["bool"]) {
            return true;
        }
    }
    false
}

fn is_copy<'a, 'ctx>(cx: &LateContext<'a, 'ctx>, ty: ty::Ty<'ctx>, item: &Item) -> bool {
    let env = ty::ParameterEnvironment::for_item(cx.tcx, item.id);
    !ty.subst(cx.tcx, &env.free_substs).moves_by_default(&env, item.span)
}
