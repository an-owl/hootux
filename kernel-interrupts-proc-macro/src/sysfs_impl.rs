use quote::TokenStreamExt;

pub struct ImplSysFsRoot {
    struct_def: syn::ItemStruct,
}

impl syn::parse::Parse for ImplSysFsRoot {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        Ok(Self {
            struct_def: input.parse()?,
        })
    }
}


impl From<ImplSysFsRoot> for proc_macro2::TokenStream {
    fn from(value: ImplSysFsRoot) -> Self {
        let verbatim = &value.struct_def;
        let num_fields = value.struct_def.fields.len();
        let struct_name = &value.struct_def.ident;

        let file_arms = value.struct_def.fields.iter().map(|field| FileMatchArbBuilder {field});
        let file_names = value.struct_def.fields.iter().map(|field| field.ident.as_ref().unwrap().to_string());


        quote::quote! {
            #verbatim

            impl crate::fs::file::File for #struct_name {
                fn file_type(&self) -> FileType {
                    FileType::Directory
                }

                fn block_size(&self) -> u64 {
                    crate::mem::PAGE_SIZE as u64
                }

                fn device(&self) -> DevID {
                    crate::fs::file::DevID::NULL
                }

                fn clone_file(&self) -> Box<dyn File> {
                    ::alloc::boxed::Box::new(self.clone())
                }

                fn id(&self) -> u64 {
                    0
                }

                fn len(&self) -> IoResult<u64> {
                    async {Some(#num_fields)}
                }
            }

            impl crate::fs::file::Directory for SysFsRootObject {
                fn entries(&self) -> IoResult<usize> {
                    async {Some(#num_fields)}
                }

                fn new_file<'f, 'b: 'f, 'a: 'f>(&'a self, name: &'b str, file: Option<&'b mut dyn NormalFile<u8>>) -> BoxFuture<'f, Result<(), (Option<IoError>, Option<IoError>)>> {
                    async {
                        Err((Some(IoError::NotSupported), None))
                    }.boxed()
                }

                fn new_dir<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, Box<dyn Directory>> {
                    async {
                        Err(Some(IoError::NotSupported))
                    }.boxed()
                }

                fn get_file<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, Box<dyn File>> {
                    async {
                        match name {
                            #(#file_arms),*
                        }
                    }
                }

                fn file_list(&self) -> IoResult<Vec<String>> {
                    async {vec![#(String::from(#file_names)),*]}.boxed()
                }

                fn remove<'f, 'a: 'f, 'b: 'f>(&'a self, name: &'b str) -> IoResult<'f, ()> {
                    async {
                        Err(Some(IoError::NotSupported))
                    }.boxed()
                }
            }

            impl FileSystem for SysFsRootObject {
                fn root(&self) -> Box<dyn Directory> {
                    Box(self.clone())
                }

                fn get_opt(&self, option: &str) -> Option<FsOptionVariant> {
                    match option {
                        FsOpts::DIR_CACHE => Some(FsOptionVariant::Bool(false)),
                        FsOpts::DEV_ALLOWED => Some(FsOptionVariant::Bool(true)),
                        _ => None,
                    }
                }

                fn set_opts(&mut self, options: &str) {
                    log::error!("Called set_opts() on {}: Not allowed", core::any::type_name::<Self>());
                }

                fn driver_name(&self) -> &'static str {
                    "sysfs"
                }

                fn raw_file(&self) -> Option<&str> {
                    None
                }
            }
        }
    }
}

struct FileMatchArbBuilder<'a> {
    field: &'a syn::Field,
}

impl quote::ToTokens for FileMatchArbBuilder<'_> {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        let field_ident = self.field.ident.as_ref().unwrap().to_string();
        let field_name = self.field;

        tokens.append_all(quote::quote! { #field_ident => async { Ok(self. #field_name .clone_file()) } });

    }
}