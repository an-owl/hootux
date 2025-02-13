use syn::parse::Parse;

pub struct OkOrLazy {
    expr: syn::Expr,
    err_branch: proc_macro2::TokenStream,
}

impl Parse for OkOrLazy {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let expr: syn::Expr = input.parse()?;
        let _ = input.parse::<syn::Token![=>]>()?;
        let err_branch = input.parse()?;

        Ok(Self { expr, err_branch })
    }
}

impl From<OkOrLazy> for proc_macro2::TokenStream {
    fn from(ok: OkOrLazy) -> Self {
        let expr = ok.expr;
        let err = ok.err_branch;
        quote::quote! {
            match #expr {
                Some(val) => val,
                None => {
                    return #err
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::lazy_prop::OkOrLazy;
    use quote::quote;

    #[test]
    fn parse_ok() {
        let ok: OkOrLazy = syn::parse2(quote! {foo.bar()?.baz() => () }).unwrap();
    }

    #[test]
    fn codegen_succeeds() {
        let ok: OkOrLazy = syn::parse2(quote! {foo.bar() => () }).unwrap();
        let ret: proc_macro2::TokenStream = ok.into();
        eprintln!("{}", ret.to_string());
    }
}
