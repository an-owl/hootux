use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::parse::ParseStream;

pub(super) struct MultiBootHeaderParser {
    arch: syn::Path,
    attr: Option<Vec<syn::Attribute>>,
    tags: Vec<TagBuilder>,
}

impl MultiBootHeaderParser {
    const ST_NAME: &'static str = "__MacroBuildMultiboot2Header";

    /// Builds structure definitions for the header and header-header
    fn build_str(&self) -> TokenStream {
        let st_name_header = quote::format_ident!("{}Header", Self::ST_NAME);
        let mut buff = quote!(head: #st_name_header);

        for (i, tag) in self.tags.iter().enumerate() {
            let id = tag.tag.clone();
            let tag = quote::format_ident!("tag_{}", i);

            buff = quote!(
                #buff,
                #tag : #id
            );
        }

        let st_name = quote::format_ident!("{}", Self::ST_NAME);
        // Dammit I need to define my own header here, because multiboot2_header doesn't have a
        // public constructor cor the header-header
        quote!(
            #[repr(C)]
            struct #st_name_header {
                header_magic: u32,
                arch: ::multiboot2_header::HeaderTagISA,
                length: u32,
                checksum: i32,
            }

            #[repr(C, packed(4))]
            struct #st_name {
                #buff ,
                tail: ::multiboot2_header::EndHeaderTag
            }
        )
    }

    /// Defines the header's static and constructs it.
    ///
    /// Attributes given as arguments to the macro will be used on the static
    fn build_static(&self) -> TokenStream {
        let st_name_header = quote::format_ident!("{}Header", Self::ST_NAME);
        let st_name = quote::format_ident!("{}", Self::ST_NAME);
        let arch = self.arch.clone();

        let mut construct = quote!(
            head: #st_name_header {
                header_magic: 0xE85250D6,
                arch: #arch,
                length: core::mem::size_of::<#st_name>() as u32,
                checksum: (-((0xE85250D6u32 as i32).wrapping_add(#arch as u32 as i32).wrapping_add(core::mem::size_of::<#st_name>() as i32))),
            },
        );

        for (i, t) in self.tags.iter().enumerate() {
            let tag = quote::format_ident!("tag_{}", i);
            let c = t.call();
            construct = quote!(
                #construct
                #tag : #c ,
            )
        }

        let att = self.attr.as_ref().map(|v| v.iter()).unwrap_or([].iter());

        quote!(
            #(#att)*
            static __KERNEL_MULTIBOOT2_HEADER: #st_name = #st_name { #construct tail: ::multiboot2_header::EndHeaderTag::new() };
        )
    }
}

impl syn::parse::Parse for MultiBootHeaderParser {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let arch: syn::Path = input.parse()?;

        match input.parse::<syn::Token![,]>() {
            Ok(_) => {}
            Err(_) => {
                // no more args
                return Ok(Self {
                    arch,
                    attr: None,
                    tags: Default::default(),
                });
            }
        }

        // no attributes are allowed
        let attr = syn::Attribute::parse_outer(input).ok();
        let _ = input.parse::<syn::Token![,]>(); // I don't actually care if this is found but if one is here it needs to be removed

        let mut tags = Vec::new();
        while !input.is_empty() {
            tags.push(input.parse()?);
            let _ = input.parse::<syn::Token![,]>();
        }

        Ok(Self { arch, attr, tags })
    }
}

impl Into<proc_macro::TokenStream> for MultiBootHeaderParser {
    fn into(self) -> proc_macro::TokenStream {
        let mut t = self.build_str();
        t.extend(self.build_static());
        t.into()
    }
}

impl Into<TokenStream> for MultiBootHeaderParser {
    fn into(self) -> TokenStream {
        let mut t = self.build_str();
        t.extend(self.build_static());
        t
    }
}

struct TagBuilder {
    tag: syn::Path,
    call: syn::ExprCall,
}

impl TagBuilder {
    fn call(&self) -> TokenStream {
        let call = self.call.clone();
        quote!(#call)
    }
}

impl syn::parse::Parse for TagBuilder {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let call: syn::ExprCall = input.parse()?;

        let tag = if let syn::Expr::Path(mut path) = *call.func.clone() {
            path.path.segments.pop();
            path.path.segments.pop_punct();
            path.path
        } else {
            panic!(
                "Failed to parse path for tag {}",
                call.func.to_token_stream().to_string()
            )
        };

        Ok(Self { tag, call })
    }
}

#[cfg(test)]
mod test {
    use quote::ToTokens;

    #[test]
    fn test_parser() {
        let q = quote::quote!(I386, FramebufferHeaderTag::new(Required, 1920, 1080, 8));
        let _: super::MultiBootHeaderParser = syn::parse2(q).unwrap();
    }

    #[test]
    fn tag_builder_check() {
        let b: super::TagBuilder = syn::parse2(quote::quote!(FrameBufferHeaderTag::new(
            Required, 1920, 1080, 8
        )))
        .unwrap();
        println!(
            "ty name: {}\nfn call: {:?}",
            b.tag.to_token_stream().to_string(),
            b.call.to_token_stream().to_string()
        );
    }

    // note: for some reason this fails when `s` is proc_macro::TokenStream
    #[test]
    fn build_with_no_tags() {
        let b: super::MultiBootHeaderParser = syn::parse2(quote::quote!(I386)).unwrap();
        let s: proc_macro2::TokenStream = b.into();
        println!("{}", s.to_token_stream().to_string());
    }

    #[test]
    fn tag_builder_has_correct_output() {
        let b: super::TagBuilder = syn::parse2(quote::quote!(FrameBufferHeaderTag::new(
            Required, 1920, 1080, 8
        )))
        .unwrap();

        let expect = "FrameBufferHeaderTag";
        assert_eq!(
            expect,
            b.tag.to_token_stream().to_string(),
            "Tag name incorrect: Expected {expect} got {}",
            b.tag.to_token_stream().to_string()
        );

        let expect = "FrameBufferHeaderTag :: new";
        assert_eq!(
            expect,
            b.call.func.to_token_stream().to_string(),
            "Constructor name incorrect: Expected {expect} got {}",
            b.call.to_token_stream().to_string()
        );

        let args: Vec<String> = b
            .call
            .args
            .iter()
            .map(|e| e.to_token_stream().to_string())
            .collect();
        let expect = ["Required", "1920", "1080", "8"];
        assert_eq!(
            expect, *args,
            "Constructor args incorrect: Expected {expect:?} got {:?}",
            &*args
        )
    }

    #[test]
    fn handles_paths_correctly() {
        let b: super::MultiBootHeaderParser = syn::parse2(quote::quote!(
            multiboot2_header::HeaderTagISA::I386,
            multiboot2_header::FrameBufferHeaderTag::new(
                multiboot2_header::HeaderTagFlag::Required,
                1920,
                1080,
                8
            )
        ))
        .unwrap();
        let expect = "multiboot2_header :: HeaderTagISA :: I386";
        assert_eq!(
            b.arch.to_token_stream().to_string(),
            expect,
            "Incorrect arch identifier: Expected {expect} got {}",
            b.arch.to_token_stream().to_string()
        );
        let expect = "multiboot2_header :: FrameBufferHeaderTag";
        assert_eq!(
            b.tags[0].tag.to_token_stream().to_string(),
            expect,
            "Incorrect arch identifier: Expected {expect} got {}",
            b.tags[0].tag.to_token_stream().to_string()
        )
    }

    #[test]
    fn handle_attributes() {
        let b: super::MultiBootHeaderParser = syn::parse2(quote::quote!(I386, #[unsafe(link_section = ".bootloader")] #[deprecated], FrameBufferHeaderTag::new(Required,1920,1080,8))).unwrap();
        let u: proc_macro2::TokenStream = b.into();
        eprintln!("{}", u.to_string());
    }

    #[test]
    fn check_complex_arg() {
        let p: super::MultiBootHeaderParser = syn::parse2(quote::quote!(
            multiboot2_header::HeaderTagISA::I386,
            #[link_section = ".multiboot2"]
            #[no_mangle],
            multiboot2_header::EfiBootServiceHeaderTag::new(multiboot2_header::HeaderTagFlag::Required),
            multiboot2_header::ConsoleHeaderTag::new(multiboot2_header::HeaderTagFlag::Optional,multiboot2_header::ConsoleHeaderTagFlags::EgaTextSupported)
        )).unwrap();
        let s: proc_macro2::TokenStream = p.into();
        println!("{}", s.to_string());
    }
}
