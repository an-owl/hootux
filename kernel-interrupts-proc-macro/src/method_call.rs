use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{Token, parenthesized};

pub struct MethodCallParser {
    input_ident: syn::Ident,
    args_ident: syn::Ident,
    vec: Vec<MethodDefinition>,
}

impl syn::parse::Parse for MethodCallParser {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let input_ident = input.parse::<syn::Ident>()?;
        let _ = input.parse::<Token![,]>()?;
        let args_ident = input.parse::<syn::Ident>()?;
        let _ = input.parse::<Token![=>]>()?;
        let mut v = Vec::new();
        while !input.is_empty() {
            v.push(input.parse()?);
        }

        Ok(Self {
            input_ident,
            args_ident,
            vec: v,
        })
    }
}

impl ToTokens for MethodCallParser {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let in_ident = &self.input_ident;
        let args_ident = &self.args_ident;
        let methods = &self.vec;

        let ts = quote::quote! {
            match (#in_ident, #args_ident) {
                #(#methods)*
                _ => ::alloc::boxed::Box::pin(async { ::core::result::Result::Err( ::hootux::fs::IoError::NotPresent )}),
            }
        };

        tokens.extend(ts);
    }
}

struct MethodDefinition {
    ident: syn::Ident,
    generics: syn::Generics,
    args: Vec<syn::Type>,
    async_container: bool,
}

impl syn::parse::Parse for MethodDefinition {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let async_container = input.parse::<Token![async]>().is_ok();
        let ident: syn::Ident = input.parse()?;
        let generics = input
            .parse::<syn::Generics>()
            .unwrap_or(syn::Generics::default()); // Just get something

        let mut args_parsed = Vec::new();
        let args;
        parenthesized!(args in input);

        while !args.is_empty() {
            args_parsed.push(args.parse()?);
            let _ = args.parse::<Token![,]>();
        }

        Ok(Self {
            ident,
            generics,
            args: args_parsed,
            async_container,
        })
    }
}

impl ToTokens for MethodDefinition {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let ident = &self.ident;
        let generics = &self.generics;
        let mut arg_names = Vec::new();
        for i in 0..self.args.len() {
            arg_names.push(quote::format_ident!("arg{}", i));
        }
        let args_ty = &self.args;

        let mut exec_line = quote::quote! {Self::#ident #generics(self,#(#arg_names),*)};
        println!("{exec_line:?}");
        if self.async_container {
            exec_line = quote::quote! { ::alloc::boxed::Box::pin( async { #exec_line } ) };
        }

        // we need this as a string to match against
        let ident_str = format!("{}{}", ident, generics.into_token_stream().to_string());

        let ts = quote::quote! {
            (#ident_str, fn_args) =>  {
                let ::core::option::Option::Some((#(#arg_names),*)) = <dyn ::core::any::Any>::downcast_ref(fn_args) else { return ::alloc::boxed::Box::pin(async {::core::result::Result::Err(::hootux::fs::IoError::InvalidData)})};
                ::alloc::boxed::Box::pin(async {::core::result::Result::Ok(::hootux::fs::file::MethodRc::wrap(#exec_line.await))})
            }
        };

        tokens.extend(ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_method() {
        let t: MethodDefinition = syn::parse2(quote::quote!(foo<usize>(&str))).unwrap();
    }

    #[test]
    fn single_method_to_tokens() {
        let t: MethodDefinition = syn::parse2(quote::quote!(foo<usize>(&str))).unwrap();
        let ts = t.to_token_stream();
    }

    #[test]
    fn async_method_to_tokens() {
        let t: MethodDefinition = syn::parse2(quote::quote!( async foo<usize>(&str))).unwrap();
        let ts = t.to_token_stream();
    }

    #[test]
    fn method_with_no_args() {
        let t: MethodDefinition = syn::parse2(quote::quote!(foo())).unwrap();
    }

    #[test]
    fn parse_single_method_complete() {
        let parsed: MethodCallParser = syn::parse2(quote::quote! {
            method_name, args =>
            foo(&str)
        })
        .unwrap();
    }

    #[test]
    fn build_single_method_complete() {
        let parsed: MethodCallParser = syn::parse2(quote::quote! {
            method_name, args =>
            foo(&str)
        })
        .unwrap();
        let ts = parsed.to_token_stream();
        println!("{}", ts.to_string());
    }

    #[test]
    fn parse_multiple() {
        let parsed: MethodCallParser = syn::parse2(quote::quote! {
            method_name, args =>
            foo(&str)
            bar(Box<u8>,usize)
            async baz(u8, u8)
        })
        .unwrap();
    }

    #[test]
    fn build_multiple() {
        let parsed: MethodCallParser = syn::parse2(quote::quote! {
            method_name, args =>
            foo(&str)
            bar(Box<u8>,usize)
            async baz(u8, u8)
        })
        .unwrap();

        let ts = parsed.to_token_stream();
        println!("{}", ts.to_string());
    }

    #[test]
    fn try_build() {
        let tb = trybuild::TestCases::new();
        tb.pass("tests/trybuild/pass/*.rs*")
    }
}
