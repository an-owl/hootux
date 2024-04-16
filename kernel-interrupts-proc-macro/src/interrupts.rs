use quote::ToTokens;
use syn::Token;
use quote::quote;

/// Stores the highest possible vector.
///
/// This should not be used to create inclusive iterators. This is because the spurious interrupt
/// handler always uses the highest numbered vector
const MAX_VECTOR: u32 = 255;

pub(crate) struct InterruptConfig {
    cosnt: syn::ItemConst,
    func: syn::Ident,
}

impl InterruptConfig {
    fn con_count(&self) -> u32 {
        if let syn::Expr::Lit(syn::ExprLit{lit: syn::Lit::Int(i), .. }) = &*self.cosnt.expr {
            i.base10_parse().expect(&format!("Failed to parse {i} into u32"))
        } else {
            panic!("Expected {} to be integer",self.cosnt.ident)
        }
    }

    pub fn gen_vector_bindings(&self) -> proc_macro2::TokenStream {
        let c = self.con_count();
        let mut r = quote!();

        for i in c..MAX_VECTOR {
            let byte = syn::LitInt::new(&*i.to_string(),proc_macro2::Span::call_site());
            let f_name = quote::format_ident!("interrupt_stub_fn_{i:}");
            r = quote!(
                #r

                extern "x86-interrupt" fn #f_name(_sf: ::x86_64::structures::idt::InterruptStackFrame) {
                    unsafe {
                        crate::interrupts::vector_tables::INT_LOG.log(#byte);
                    }
                    if let Some(f) = vector_tables::IHR.get(#byte).read().callable() {
                        f.call();
                    } else {
                        ::log::warn!("Unhandled interrupt at vector {}", #byte)
                    }
                }

                idt[ #byte ].set_handler_fn( #f_name );
            )
        }
        r
    }
}

impl Into<proc_macro::TokenStream> for InterruptConfig {
    fn into(self) -> proc_macro::TokenStream {
        let mut ts = self.cosnt.to_token_stream();
        let f_name = quote::format_ident!("{}",self.func.to_string());

        let bindings = self.gen_vector_bindings();

        ts = quote!(
            #ts

            fn #f_name (idt: &mut ::x86_64::structures::idt::InterruptDescriptorTable) {
                #bindings
            }
        );

        ts.into()
    }
}

impl syn::parse::Parse for InterruptConfig {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let cosnt = input.parse()?;
        let _f: Token![fn] = input.parse()?;

        Ok(Self {
            cosnt,
            func: input.parse()?,
        })
    }
}