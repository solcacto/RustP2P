//! Proc macros for the guest SDK.
//!
//! `#[export(GameType)]` turns a plain function into the guest's C entry point:
//! it emits a `#[no_mangle] pub extern "C"` wrapper of the same name that calls
//! `guest_sdk::run::<GameType>()`, so the SDK dispatches every host tick to
//! the game instance.

use proc_macro::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Expr, ItemFn, Lit, Meta, Token};

/// Attribute macro: `#[export(MyGame)] fn game_tick() {}`.
///
/// The annotated function's body is replaced by the SDK dispatch call; the
/// function name is preserved as the exported symbol.
#[proc_macro_attribute]
pub fn export(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args with Punctuated::<Meta, Token![,]>::parse_terminated);
    let game_type = parse_game_type(&args);

    let item = parse_macro_input!(input as ItemFn);
    let fn_name = item.sig.ident.clone();
    let vis = item.vis;
    let Some(game_type) = game_type else {
        return quote! {
            compile_error!("#[export] requires a game type: #[export(MyGame)]");
        }
        .into();
    };

    let expanded = quote! {
        #[no_mangle]
        #vis extern "C" fn #fn_name() {
            ::guest_sdk::run::<#game_type>()
        }
    };
    TokenStream::from(expanded)
}

fn parse_game_type(args: &Punctuated<Meta, Token![,]>) -> Option<syn::Type> {
    match args.first()? {
        // #[export(MyGame)] — the bare type name.
        Meta::Path(path) => syn::parse2(quote!(#path)).ok(),
        // #[export(game = "MyGame")] — string alias.
        Meta::NameValue(nv) if nv.path.is_ident("game") => {
            if let Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                    return syn::parse_str(&s.value()).ok();
                }
            }
            None
        }
        _ => None,
    }
}
