//! Proc macro for compile-time validated StructFS paths.
//!
//! `oxpath!` validates string literal components against UAX#31 (Namecode
//! identifiers) at compile time. Expression arguments must be
//! `PathComponent` values — pre-validated at construction time.
//!
//! ```ignore
//! // All literals — validated at compile time
//! let p = oxpath!("gate", "defaults", "model");
//!
//! // Mixed — literals validated at compile, expressions must be PathComponent
//! let name = PathComponent::try_new("personal")?;
//! let p = oxpath!("gate", "accounts", name, "provider");
//!
//! // Compile error:
//! // let p = oxpath!("gate", "bad-name");
//! //                        ^^^^^^^^^ invalid character '-'
//! ```

use proc_macro::TokenStream;

use quote::quote;
use syn::punctuated::Punctuated;
use syn::{Expr, Lit, Token, parse_macro_input};

/// Build a `Path` from a mix of literal and runtime components.
///
/// - **String literals** are validated at compile time against UAX#31.
/// - **Expressions** must be of type `PathComponent` (runtime-validated).
///
/// Returns a `structfs_core_store::Path`.
#[proc_macro]
pub fn oxpath(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input with Punctuated::<Expr, Token![,]>::parse_terminated);

    if args.is_empty() {
        return quote! {
            ::structfs_core_store::Path::from_components(::std::vec::Vec::new())
        }
        .into();
    }

    let mut component_exprs = Vec::new();

    for expr in &args {
        match expr {
            Expr::Lit(expr_lit) => match &expr_lit.lit {
                Lit::Str(s) => {
                    let value = s.value();
                    if let Err(msg) = validate_component(&value) {
                        return syn::Error::new(s.span(), msg).to_compile_error().into();
                    }
                    // Validated — emit as a direct string, no runtime check needed
                    component_exprs.push(quote! { ::std::string::String::from(#s) });
                }
                Lit::Int(n) => {
                    // Numeric literals are valid components (array indexing)
                    let s = n.to_string();
                    component_exprs.push(quote! { ::std::string::String::from(#s) });
                }
                other => {
                    return syn::Error::new(
                        other.span(),
                        "expected string literal, integer literal, or PathComponent expression",
                    )
                    .to_compile_error()
                    .into();
                }
            },
            other => {
                // Runtime expression — must be PathComponent.
                // Call .as_str() to get the validated inner string.
                component_exprs.push(quote! { ::std::string::String::from(#other.as_str()) });
            }
        }
    }

    // Construct Path directly — all components are validated (literals at
    // compile time, PathComponent values at their construction site).
    // Use `from_components` which re-validates at runtime as a safety net.
    quote! {
        ::structfs_core_store::Path::from_components(
            ::std::vec![#(#component_exprs),*]
        )
    }
    .into()
}

/// Validate a single path component against UAX#31 (matching structfs rules).
fn validate_component(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty path component".into());
    }

    // Pure numeric — valid (array indexing)
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(());
    }

    let mut chars = s.chars();
    let first = chars.next().unwrap();

    // First char: XID_Start, or underscore followed by XID_Continue
    let valid_start = unicode_ident::is_xid_start(first)
        || (first == '_'
            && chars
                .clone()
                .next()
                .is_some_and(unicode_ident::is_xid_continue));

    if !valid_start {
        return Err(format!(
            "must start with a letter or underscore followed by letter/digit, got '{first}'"
        ));
    }

    // Remaining: XID_Continue
    for c in chars {
        if !unicode_ident::is_xid_continue(c) {
            return Err(format!("invalid character '{c}' in component \"{s}\""));
        }
    }

    Ok(())
}
