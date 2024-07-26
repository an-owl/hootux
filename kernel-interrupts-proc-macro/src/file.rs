use proc_macro2::TokenStream;
use syn::parse::ParseStream;

pub(crate) struct UpcastImpl {
    native: bool,
    ty: syn::Path,
    yes_cast: Vec<YesCast>,
}

impl UpcastImpl {
    fn all_trait_paths() ->  Vec<syn::Path> {
        use syn::parse_quote;
        vec![
            parse_quote!(file::NormalFile<u8>),
            parse_quote!(file::Directory),
            parse_quote!(file::FileSystem),
            parse_quote!(device::Fifo<u8>),
            parse_quote!(device::DeviceFile),
        ]
    }
}

impl Into<proc_macro::TokenStream> for UpcastImpl {
    fn into(self) -> proc_macro::TokenStream {

        let crate_name = if self.native {
            quote::quote! {crate}
        } else {
            quote::quote! {hootux}
        };

        let bind = Self::all_trait_paths();
        let no_cast = bind.iter().filter(
            |i| {
                for y in &self.yes_cast {
                    if y.name == **i {
                        return false
                    }
                }
                true
            }
        );

        let yes_cast = self.yes_cast.iter().map(|i| i.tokens(&self.ty,&crate_name));

        let ty = &self.ty;

        let q = quote::quote! {
            #(#yes_cast)*

            #(
                impl TryFrom<alloc::boxed::Box< #ty >> for alloc::boxed::Box< dyn #crate_name ::fs:: #no_cast > {
                    type Error = alloc::boxed::Box<dyn #crate_name ::fs::file::File>;
                    fn try_from(value: alloc::boxed::Box< #ty > ) -> Result<alloc::boxed::Box<dyn #crate_name ::fs:: #no_cast>, Self::Error> {
                        Err(value)
                    }
                }
            )*
        };
        q.into()
    }
}

impl syn::parse::Parse for UpcastImpl {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let native = input.parse::<syn::Token![crate]>().is_ok();
        let ty = input.parse()?;
        let mut vec = Vec::new();
        while !input.is_empty() {
            let yes = YesCast::parse(input)?;
            vec.push(yes);
        };

        if vec.is_empty() {
            return Err(syn::Error::new(proc_macro2::Span::call_site(),"File must be cast into at least one major type"));
        }

        Ok(Self {
            native,
            ty,
            yes_cast: vec
        })
    }
}

struct YesCast {
    name: syn::Path,
    code: Option<syn::Block>,
}

impl YesCast {
    fn tokens(&self, ident: &syn::Path, crate_name: &TokenStream ) -> TokenStream {
        let name = &self.name;
        let code = self.code.clone().unwrap_or(syn::parse2(quote::quote!{{Ok(value)}}).unwrap());
        quote::quote! {
            impl TryFrom<alloc::boxed::Box< #ident >> for alloc::boxed::Box< dyn #crate_name ::fs:: #name > {
                type Error = alloc::boxed::Box<dyn #crate_name ::fs::file::File>;
                fn try_from(value: alloc::boxed::Box< #ident >) -> Result<alloc::boxed::Box< dyn #crate_name :: fs:: #name>, Self::Error> #code
            }
        }
    }
}

impl syn::parse::Parse for YesCast {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let s = Self {
            name: input.parse()?,
            code: input.fork().parse().ok(),
        };

        if s.code.is_some() {
            let _: syn::Block = input.parse().unwrap(); // step cursor
        };

        Ok(s)
    }
}


#[cfg(test)]
mod tests {
    use quote::quote;
    use crate::file::{UpcastImpl};

    #[test]
    fn test_parse() {
        let q= quote!{ hootux Foo NormalFile<u8> {self} };
        let p: UpcastImpl = syn::parse2(q).unwrap();

        assert_eq!(&p.ty.segments[0].ident.to_string(),"Foo");
        if let syn::Stmt::Expr(syn::Expr::Path(e),_) = &p.yes_cast[0].code.as_ref().unwrap().stmts[0] {
            assert_eq!(e.path.segments[0].ident.to_string(),"self");
        } else {
            panic!("Failed to capture type name");
        }

        assert_eq!(p.yes_cast[0].name.segments[0].ident.to_string(),"NormalFile");
        if let syn::PathArguments::AngleBracketed(a) = &p.yes_cast[0].name.segments[0].arguments {
            if let syn::GenericArgument::Type(syn::Type::Path(ref ty)) = a.args[0] {
                assert_eq!(ty.path.segments[0].ident.to_string(),"u8")
            } else {
                panic!("Failed to capture type argument");
            }
        } else {
            panic!("Failed to capture type argument");
        }
    }

    #[test]
    fn trait_fragments() {
        for partial in UpcastImpl::all_trait_paths() {
            println!("{:?}",partial);
        }

    }
}