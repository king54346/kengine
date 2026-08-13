use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

pub(crate) fn impl_script_message_payload(ast: DeriveInput) -> TokenStream {
    let ident = &ast.ident;
    quote! {
        impl ScriptMessagePayload for #ident {

        }
    }
}
