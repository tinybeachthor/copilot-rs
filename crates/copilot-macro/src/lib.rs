//! Macros for copilot-rs.
//!
//! [`CopilotStruct`] makes a Rust struct usable as a stream type;
//! [`copilot`] is declarative sugar over the builder.

mod spec;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, Path, Type, parse_macro_input, parse_quote};

/// Makes a Rust struct usable as a type in a specification.
///
/// Generates the `Typed` implementation reifying the
/// struct into the IR, plus a trait giving type-checked access to its fields on
/// a stream:
///
/// ```ignore
/// #[derive(Clone, Copy, CopilotStruct)]
/// #[repr(C)]
/// struct Reading {
///     altitude: f32,
///     valid: bool,
/// }
///
/// let sensor = b.extern_::<Reading>("sensor");
/// let climbing = sensor.altitude().gt_val(1000.0);   // Stream<f32>
/// let corrected = sensor.set_altitude(b.lit(0.0));   // Stream<Reading>
/// ```
///
/// The accessors arrive through a generated `ReadingFields` trait rather than
/// as inherent methods, because `Stream` belongs to another crate and only
/// that crate can add inherent methods to it. The trait is emitted next to the
/// struct, so it is in scope wherever the struct is.
///
/// # Requirements
///
/// - The struct must be `Copy`, since `Typed` requires
///   it, and should be `#[repr(C)]` so that its layout matches what
///   `copilot_core::resources` computes for it.
/// - Every field's type must itself implement `Typed`.
/// - Tuple and unit structs are rejected: the IR names fields, and generated
///   code needs those names.
///
/// # Crate path
///
/// Generated code refers to `::copilot_lang` by default. A crate that depends
/// on the `copilot` facade instead should say so:
///
/// ```ignore
/// #[derive(Clone, Copy, CopilotStruct)]
/// #[copilot(crate = ::copilot)]
/// struct Reading { /* .. */ }
/// ```
#[proc_macro_derive(CopilotStruct, attributes(copilot))]
pub fn copilot_struct(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let krate = crate_path(&input)?;
    let name = &input.ident;
    let name_str = name.to_string();

    let fields = named_fields(&input)?;
    if fields.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "a Copilot struct must have at least one field: the IR has no representation for an \
             empty one, matching upstream Copilot",
        ));
    }

    let (idents, types): (Vec<&Ident>, Vec<&Type>) = fields.iter().copied().unzip();
    let field_names: Vec<String> = idents.iter().map(|i| i.to_string()).collect();

    let typed_impl = quote! {
        impl #krate::Typed for #name {
            fn ty() -> #krate::Type {
                #krate::Type::structure(
                    #name_str,
                    [
                        #((
                            ::std::string::String::from(#field_names),
                            <#types as #krate::Typed>::ty(),
                        ),)*
                    ],
                )
            }

            fn lift(self) -> #krate::Value {
                #krate::Value::Struct {
                    name: ::std::string::String::from(#name_str),
                    fields: ::std::vec![
                        #((
                            ::std::string::String::from(#field_names),
                            #krate::Typed::lift(self.#idents),
                        ),)*
                    ],
                }
            }
        }
    };

    let trait_name = format_ident!("{}Fields", name);
    let setters: Vec<Ident> = idents.iter().map(|i| format_ident!("set_{}", i)).collect();
    let getter_docs: Vec<String> = field_names
        .iter()
        .map(|f| format!("The `{f}` field of this stream."))
        .collect();
    let setter_docs: Vec<String> = field_names
        .iter()
        .map(|f| format!("This stream with its `{f}` field replaced."))
        .collect();

    let trait_doc = format!("Field access for streams of [`{name}`].");
    let fields_trait = quote! {
        #[doc = #trait_doc]
        pub trait #trait_name<'a> {
            #(
                #[doc = #getter_docs]
                fn #idents(self) -> #krate::Stream<'a, #types>;

                #[doc = #setter_docs]
                fn #setters(self, value: #krate::Stream<'a, #types>)
                    -> #krate::Stream<'a, #name>;
            )*
        }

        impl<'a> #trait_name<'a> for #krate::Stream<'a, #name> {
            #(
                fn #idents(self) -> #krate::Stream<'a, #types> {
                    self.field(#field_names)
                }

                fn #setters(self, value: #krate::Stream<'a, #types>)
                    -> #krate::Stream<'a, #name>
                {
                    self.with_field(#field_names, value)
                }
            )*
        }
    };

    Ok(quote! {
        #typed_impl
        #fields_trait
    })
}

fn named_fields(input: &DeriveInput) -> syn::Result<Vec<(&Ident, &Type)>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "CopilotStruct can only be derived for structs",
        ));
    };
    match &data.fields {
        Fields::Named(named) => Ok(named
            .named
            .iter()
            .map(|f| {
                (
                    f.ident.as_ref().expect("named fields always have an ident"),
                    &f.ty,
                )
            })
            .collect()),
        _ => Err(syn::Error::new_spanned(
            &input.ident,
            "CopilotStruct requires named fields: the IR identifies fields by name, and generated \
             code needs those names",
        )),
    }
}

/// The path to the crate exporting `Typed`, `Type`, `Value`, and `Stream`.
///
/// Defaults to `::copilot_lang`; `#[copilot(crate = ::copilot)]` overrides it
/// for crates that depend on the facade instead.
fn crate_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut path: Path = parse_quote!(::copilot_lang);
    for attr in &input.attrs {
        if !attr.path().is_ident("copilot") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                path = meta.value()?.parse()?;
                Ok(())
            } else {
                Err(meta.error("unrecognised copilot attribute; expected `crate = <path>`"))
            }
        })?;
    }
    Ok(path)
}

/// Declarative sugar over `copilot_lang::Builder`.
///
/// A specification reads as declarations rather than as builder calls, and
/// expands to exactly those calls — the macro adds no semantics of its own.
///
/// ```ignore
/// let spec = copilot! {
///     extern temperature: f32;
///
///     stream counter: u64 = [0] ++ counter + 1;
///     let celsius = temperature * 0.5 - 30.0;
///
///     observe celsius;
///     trigger heat_on(celsius) when celsius < 18.0;
///     property bounded = counter < 1000;
/// }?;
/// ```
///
/// # Items
///
/// | Form | Means |
/// |---|---|
/// | `extern name: Ty;` | a value the environment supplies each step |
/// | `stream name: Ty = [a, b] ++ body;` | `body` preceded by the initial values, upstream's `++` |
/// | `let name = expr;` | a named expression, not buffered |
/// | `observe name;` / `observe name = expr;` | sample a value every step |
/// | `trigger name(args..) when guard;` | call a handler while `guard` holds |
/// | `property name = expr;` | a claim for the prover; `property exists name = ..` for the existential form |
///
/// Streams are declared before any body is built, so they may refer to each
/// other as well as to themselves.
///
/// # Expressions
///
/// Bodies are ordinary Rust expressions over the names in scope, so arithmetic,
/// bitwise operators and method calls work as written. Two things are
/// translated:
///
/// - `< <= > >= == != && ||` become the stream methods, because comparing two
///   streams yields a stream of booleans rather than a `bool` and so cannot be
///   `PartialOrd`.
/// - A bare literal used as an operand is lifted into a stream, so
///   `celsius < 18.0` works. Literals elsewhere are left alone, which is what
///   `counter.drop(1)` and `history.index(i)` need.
///
/// # Crate path
///
/// Generated code refers to `::copilot_lang`. A crate that reaches the language
/// through a facade should say so, since a macro cannot see how it was
/// reached:
///
/// ```ignore
/// copilot! {
///     #![crate(::copilot)]
///     // ..
/// }
/// ```
#[proc_macro]
pub fn copilot(input: TokenStream) -> TokenStream {
    match spec::parse(input) {
        Ok(block) => spec::expand(block).into(),
        Err(e) => e.to_compile_error().into(),
    }
}
