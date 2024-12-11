use std::str::FromStr;
use proc_macro2::Span;
use quote::ToTokens;


#[derive(Debug)]
pub struct SlabLikeParser {
    pub_type: syn::ItemStruct,

    alloc_ty: syn::Path,
    slab_size: usize,

    backing_alloc: Option<(syn::Path, syn::Expr)>,

}

impl syn::parse::Parse for SlabLikeParser {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let this = Self {
            pub_type: input.parse()?,
            alloc_ty: input.parse()?,
            slab_size: {
                let int = input.parse::<syn::LitInt>()?;
                let num_str: usize = int.base10_parse()?;
                num_str
            },
            backing_alloc: {
                if let Ok(p) = input.parse::<syn::Path>() {

                    let _ = input.parse::<syn::Token![,]>()?;
                    let alloc_constructor = input.parse()?;
                    Some((p, alloc_constructor))
                } else {
                    None
                }
            },
        };


        if this.slab_size.is_power_of_two() {
            Ok(this)
        } else {
            // we catch this here because the actual rustc error message is nonsense
            Err(syn::Error::new(Span::call_site(),"Slab size must be power of two"))
        }
    }
}

impl From<SlabLikeParser> for proc_macro2::TokenStream {
    fn from(value: SlabLikeParser) -> Self {
        let pt = value.pub_type;
        let alloc_ty = value.alloc_ty;
        let slab_size = value.slab_size;

        // We need to resolve the backing alloc parts, because they are optional we need to generate the default too
        let (backing_alloc,construct) = value.backing_alloc
            .map(
                |(alloc,construct)|
                    (alloc.into_token_stream(),construct.into_token_stream())
            )
            .unwrap_or(
                (quote::quote!(::alloc::alloc::Global),quote::quote!(::alloc::alloc::Global))
            );
        let pt_ident = pt.ident.clone();

        #[allow(non_upper_case_globals)] // pt_ident will not be all uppercase
        let backing_static = quote::format_ident!("__KERNEL_PROC_MACRO_STATIC_SLABLIKE_ALLOCATOR_FOR_{}", pt_ident);

        quote::quote! {
            static #backing_static: ::slablike::SlabLike< #backing_alloc, #alloc_ty, #slab_size > = ::slablike::SlabLike::new( #construct );

            #pt

            unsafe impl ::core::alloc::Allocator for #pt_ident {
                fn allocate(&self, layout: ::core::alloc::Layout) -> ::core::result::Result<::core::ptr::NonNull<[u8]>, ::core::alloc::AllocError> {
                    #backing_static.allocate(layout)
                }

                unsafe fn deallocate(&self, ptr: ::core::ptr::NonNull<u8>, layout: ::core::alloc::Layout) {
                    #backing_static.deallocate(ptr, layout)
                }
            }

            impl #pt_ident {
                fn clean(&self, size: usize) -> ::core::result::Result<usize,usize> {
                    #backing_static.clean(size)
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use quote::ToTokens;
    use syn::parse::Parse;
    use crate::slablike::SlabLikeParser;

    #[test]
    fn parses_correctly() {
        let parsed = syn::parse2::<SlabLikeParser>(quote::quote! {struct MyStruct; u8 4096}).unwrap();
        assert_eq!(parsed.slab_size, 4096);
        assert_eq!(parsed.pub_type.ident, "MyStruct");
        assert_eq!(parsed.alloc_ty.clone().into_token_stream().to_string(), "u8");

        let ts: proc_macro2::TokenStream = parsed.into();

        eprintln!("{}", ts);
    }

    #[test]
    fn constructor_ok() {
        let parsed = syn::parse2::<SlabLikeParser>(quote::quote! {struct MyStruct; u8 4096 std::alloc::Global, std::alloc::Global}).unwrap();
        assert!(parsed.backing_alloc.is_some());

        assert_eq!(parsed.backing_alloc.clone().unwrap().0.to_token_stream().to_string(), "std :: alloc :: Global");
        assert_eq!(parsed.backing_alloc.clone().unwrap().1.to_token_stream().to_string(), "std :: alloc :: Global");
    }

    #[test]
    fn parse_assert_pow2() {
        let ts = syn::parse2::<SlabLikeParser>( quote::quote! {struct MyStruct; u8 4095});
        if let Err(e) = ts {
            assert_eq!(e.to_string(), "Slab size must be power of two")
        } else {
            panic!("Should've returned Err(_)")
        }
    }
}