//! The `copilot!` macro: declarative sugar over the builder.
//!
//! Everything here desugars to [`copilot_lang::Builder`] calls. The macro adds
//! no semantics of its own — it is a second way to write the same builder
//! program, which is why `crates/copilot-lang/tests/macro_spec.rs` can assert
//! that the two produce literally equal `Spec`s rather than merely equivalent
//! ones.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, BinOp, Expr, Ident, Path, Token, Type, braced, bracketed, parenthesized};

mod kw {
    syn::custom_keyword!(stream);
    syn::custom_keyword!(observe);
    syn::custom_keyword!(trigger);
    syn::custom_keyword!(property);
    syn::custom_keyword!(when);
    syn::custom_keyword!(exists);
}

/// A whole `copilot! { .. }` block.
pub struct SpecBlock {
    krate: Path,
    items: Vec<Item>,
}

enum Item {
    /// `extern name: Ty;`
    Extern { name: Ident, ty: Type },
    /// `stream name: Ty = [a, b] ++ body;`
    ///
    /// Boxed: it carries far more than the other forms, and would otherwise
    /// set the size of every element in the block's `Vec<Item>`.
    Stream(Box<StreamItem>),
    /// `let name = expr;`
    Let { name: Ident, value: Expr },
    /// `observe name;` or `observe name = expr;`
    Observe { name: Ident, value: Option<Expr> },
    /// `trigger name(args..) when guard;`
    Trigger {
        name: Ident,
        args: Punctuated<Expr, Token![,]>,
        guard: Expr,
    },
    /// `property name = expr;` or `property exists name = expr;`
    Property {
        name: Ident,
        exists: bool,
        value: Expr,
    },
}

/// The parts of a `stream` declaration.
struct StreamItem {
    name: Ident,
    ty: Type,
    initial: Punctuated<Expr, Token![,]>,
    body: Expr,
}

impl Parse for SpecBlock {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // An optional `#![crate = ::path]` first, for crates that reach the
        // language through a facade rather than depending on `copilot-lang`
        // by name. A proc macro cannot see its own call path, so it has to be
        // told.
        let mut krate: Path = syn::parse_quote!(::copilot_lang);
        for attribute in input.call(Attribute::parse_inner)? {
            if attribute.path().is_ident("crate") {
                krate = attribute.parse_args()?;
            } else {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "the only inner attribute a copilot! block accepts is `#![crate(..)]`",
                ));
            }
        }

        let mut items = Vec::new();
        while !input.is_empty() {
            items.push(input.parse()?);
        }
        Ok(SpecBlock { krate, items })
    }
}

impl Parse for Item {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();

        if lookahead.peek(Token![extern]) {
            input.parse::<Token![extern]>()?;
            let name = input.parse()?;
            input.parse::<Token![:]>()?;
            let ty = input.parse()?;
            input.parse::<Token![;]>()?;
            return Ok(Item::Extern { name, ty });
        }

        if lookahead.peek(Token![let]) {
            input.parse::<Token![let]>()?;
            let name = input.parse()?;
            input.parse::<Token![=]>()?;
            let value = input.parse()?;
            input.parse::<Token![;]>()?;
            return Ok(Item::Let { name, value });
        }

        if lookahead.peek(kw::stream) {
            input.parse::<kw::stream>()?;
            let name = input.parse()?;
            input.parse::<Token![:]>()?;
            let ty = input.parse()?;
            input.parse::<Token![=]>()?;

            let contents;
            bracketed!(contents in input);
            let initial = contents.parse_terminated(Expr::parse, Token![,])?;

            // `++`, upstream's stream-append. Two tokens to Rust's lexer.
            input.parse::<Token![+]>()?;
            input.parse::<Token![+]>()?;

            let body = input.parse()?;
            input.parse::<Token![;]>()?;
            return Ok(Item::Stream(Box::new(StreamItem {
                name,
                ty,
                initial,
                body,
            })));
        }

        if lookahead.peek(kw::observe) {
            input.parse::<kw::observe>()?;
            let name: Ident = input.parse()?;
            let value = if input.peek(Token![=]) {
                input.parse::<Token![=]>()?;
                Some(input.parse()?)
            } else {
                None
            };
            input.parse::<Token![;]>()?;
            return Ok(Item::Observe { name, value });
        }

        if lookahead.peek(kw::trigger) {
            input.parse::<kw::trigger>()?;
            let name = input.parse()?;
            let contents;
            parenthesized!(contents in input);
            let args = contents.parse_terminated(Expr::parse, Token![,])?;
            input.parse::<kw::when>()?;
            let guard = input.parse()?;
            input.parse::<Token![;]>()?;
            return Ok(Item::Trigger { name, args, guard });
        }

        if lookahead.peek(kw::property) {
            input.parse::<kw::property>()?;
            let exists = input.peek(kw::exists);
            if exists {
                input.parse::<kw::exists>()?;
            }
            let name = input.parse()?;
            input.parse::<Token![=]>()?;
            let value = input.parse()?;
            input.parse::<Token![;]>()?;
            return Ok(Item::Property {
                name,
                exists,
                value,
            });
        }

        Err(lookahead.error())
    }
}

/// Expands a block into a `Result<Spec, Error>` expression.
pub fn expand(block: SpecBlock) -> TokenStream {
    let krate = &block.krate;
    let builder = format_ident!("__copilot_builder");
    let rewrite = Rewriter {
        builder: builder.clone(),
    };

    let mut externs = Vec::new();
    let mut declarations = Vec::new();
    let mut bindings = Vec::new();
    let mut definitions = Vec::new();
    let mut outputs = Vec::new();

    for item in &block.items {
        match item {
            Item::Extern { name, ty } => {
                let literal = name.to_string();
                externs.push(quote! {
                    let #name = #builder.extern_::<#ty>(#literal);
                });
            }

            // Declaring every stream before defining any is what makes mutual
            // recursion work: each body is built when all the handles already
            // exist.
            Item::Stream(declaration) => {
                let StreamItem {
                    name,
                    ty,
                    initial,
                    body,
                } = &**declaration;
                let pending = format_ident!("__copilot_pending_{}", name);
                let values = initial.iter();
                declarations.push(quote! {
                    let #pending = #builder.declare::<#ty>(&[#(#values),*]);
                    let #name = #pending.stream();
                });
                let body = rewrite.expr(body);
                definitions.push(quote! { #pending.define(#body); });
            }

            Item::Let { name, value } => {
                let value = rewrite.expr(value);
                bindings.push(quote! { let #name = #value; });
            }

            Item::Observe { name, value } => {
                let literal = name.to_string();
                let value = match value {
                    Some(value) => rewrite.expr(value),
                    None => quote! { #name },
                };
                outputs.push(quote! { #builder.observe(#literal, #value); });
            }

            Item::Trigger { name, args, guard } => {
                let literal = name.to_string();
                let guard = rewrite.expr(guard);
                let args = args.iter().map(|arg| rewrite.expr(arg));
                outputs.push(quote! {
                    #builder.trigger(#literal, #guard, #krate::args![#(#args),*]);
                });
            }

            Item::Property {
                name,
                exists,
                value,
            } => {
                let literal = name.to_string();
                let value = rewrite.expr(value);
                let method = if *exists {
                    format_ident!("property_exists")
                } else {
                    format_ident!("property_forall")
                };
                outputs.push(quote! { #builder.#method(#literal, #value); });
            }
        }
    }

    quote! {{
        let #builder = #krate::Builder::new();
        #(#externs)*
        #(#declarations)*
        #(#bindings)*
        #(#definitions)*
        #(#outputs)*
        #builder.finish()
    }}
}

/// Rewrites the Rust-shaped expressions in a block into stream operations.
///
/// Two things need translating, and nothing else does — ordinary arithmetic is
/// already the operator implementations on `Stream`:
///
/// - **Comparisons and boolean connectives.** `a < b` cannot be `PartialOrd`,
///   because comparing two streams yields a *stream* of booleans rather than a
///   `bool`. They become the method calls that do.
/// - **Literals in operand position.** `celsius < 18.0` needs the `18.0` to be
///   a stream too. Lifting only in operand position leaves `s.drop(1)` and
///   `history.index(i)` alone, where a bare number is what is wanted.
struct Rewriter {
    builder: Ident,
}

impl Rewriter {
    fn expr(&self, expr: &Expr) -> TokenStream {
        match expr {
            Expr::Binary(binary) => {
                let left = self.operand(&binary.left);
                let right = self.operand(&binary.right);
                match binary.op {
                    BinOp::Lt(_) => quote! { (#left).lt(#right) },
                    BinOp::Le(_) => quote! { (#left).le(#right) },
                    BinOp::Gt(_) => quote! { (#left).gt(#right) },
                    BinOp::Ge(_) => quote! { (#left).ge(#right) },
                    BinOp::Eq(_) => quote! { (#left).eq_(#right) },
                    BinOp::Ne(_) => quote! { (#left).ne_(#right) },
                    BinOp::And(_) => quote! { (#left).and(#right) },
                    BinOp::Or(_) => quote! { (#left).or(#right) },
                    // Arithmetic and bitwise operators already mean the right
                    // thing on streams.
                    op => quote! { (#left) #op (#right) },
                }
            }

            Expr::Unary(unary) => {
                let operand = self.operand(&unary.expr);
                let op = unary.op;
                quote! { #op (#operand) }
            }

            Expr::Paren(paren) => {
                let inner = self.expr(&paren.expr);
                quote! { (#inner) }
            }

            // Method calls have to be descended into: `(a < b).mux(x, y)` hides
            // a comparison in the receiver, and `.mux(true, ..)` a literal that
            // needs lifting.
            Expr::MethodCall(call) => {
                let receiver = self.expr(&call.receiver);
                let method = &call.method;
                let turbofish = &call.turbofish;
                let arguments = call.args.iter().map(|argument| {
                    if takes_plain_arguments(&call.method) {
                        self.expr(argument)
                    } else {
                        self.operand(argument)
                    }
                });
                quote! { (#receiver).#method #turbofish (#(#arguments),*) }
            }

            // A free call's signature is unknown here, so descend without
            // lifting: whatever it takes, it is not this macro's to guess.
            Expr::Call(call) => {
                let function = &call.func;
                let arguments = call.args.iter().map(|argument| self.expr(argument));
                quote! { #function(#(#arguments),*) }
            }

            Expr::Field(field) => {
                let base = self.expr(&field.base);
                let member = &field.member;
                quote! { (#base).#member }
            }

            Expr::Cast(cast) => {
                let inner = self.expr(&cast.expr);
                let ty = &cast.ty;
                quote! { (#inner) as #ty }
            }

            // Paths, literals in their own right, and anything else pass
            // through: they are already written against the stream API.
            other => quote! { #other },
        }
    }

    /// An operand of an operator, with bare literals lifted into streams.
    fn operand(&self, expr: &Expr) -> TokenStream {
        match expr {
            Expr::Lit(literal) if liftable(literal) => {
                let builder = &self.builder;
                quote! { #builder.lit(#literal) }
            }
            // A negated literal is still a literal for this purpose, so that
            // `x * -1` lifts the way `x * 1` does.
            Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => match &*unary.expr {
                Expr::Lit(literal) if liftable(literal) => {
                    let builder = &self.builder;
                    quote! { #builder.lit(#expr) }
                }
                _ => self.expr(expr),
            },
            other => self.expr(other),
        }
    }
}

/// Whether a literal denotes a value a stream can carry.
///
/// Numbers and booleans do. Strings do not — they appear as method arguments
/// naming a field or a label, where lifting would be nonsense.
fn liftable(literal: &syn::ExprLit) -> bool {
    matches!(
        literal.lit,
        syn::Lit::Int(_) | syn::Lit::Float(_) | syn::Lit::Bool(_)
    )
}

/// Whether a method's arguments are plain values rather than streams.
///
/// `drop` is the exception in the whole API: its argument is how far to shift,
/// a quantity fixed when the specification is built, not a value that varies
/// over time. Everything else — `mux`, the comparisons, `index`, the shifts —
/// takes streams, so a literal there is lifted.
fn takes_plain_arguments(method: &Ident) -> bool {
    method == "drop"
}

/// Parses a `copilot! { .. }` block, including the outer braces when the macro
/// is invoked with them.
pub fn parse(input: proc_macro::TokenStream) -> syn::Result<SpecBlock> {
    syn::parse::Parser::parse(
        |stream: ParseStream| {
            if stream.peek(syn::token::Brace) {
                let inner;
                braced!(inner in stream);
                inner.parse()
            } else {
                stream.parse()
            }
        },
        input,
    )
}
