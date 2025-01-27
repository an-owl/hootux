use proc_macro2::TokenStream;
use quote::TokenStreamExt;
use syn::{spanned::Spanned,Attribute};

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

                fn device(&self) -> crate::fs::vfs::DevID {
                    crate::fs::vfs::DevID::NULL
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
                            #(#file_arms),*,
                            _ => async { Err(IoError::NotPresent) }
                        }
                    }
                }

                fn file_list(&self) -> IoResult<Vec<String>> {
                    async {::alloc::vec![#(String::from(#file_names)),*]}.boxed()
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
        let field_name = self.field.ident.as_ref().unwrap();

        tokens.append_all(quote::quote! { #field_ident => async { Ok(self. #field_name .clone_file()) } });

    }
}

pub struct SysfsDirDerive {
    def: syn::DeriveInput,
    files: Vec<DirDeriveFieldHelper>,
    index: Option<DirDeriveFieldHelper>
}

impl syn::parse::Parse for SysfsDirDerive {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let def: syn::DeriveInput = input.parse()?;
        let syn::Data::Struct(ref ty_def) = def.data else {
            return Err(syn::Error::new(input.span(), "SysfsDirDerive only supports structs"));
        };

        let mut files = Vec::new();
        let mut index = None;

        for i in &ty_def.fields {
            match DirDeriveFieldHelper::new(i) {
                Ok(field) => {
                    if let HelperType::File(_) = field.helper {
                        files.push(field)
                    } else {
                        index.replace(field).ok_or(syn::Error::new(input.span(), "Multiple indexes are not allowed"))?;
                    }
                },
                Err(e) if format!("{e}") == "no-attr" => continue,
                Err(e) => return Err(e),
            }
        }

        Ok(
            Self {
                def,
                files,
                index,
            }
        )
    }
}

impl quote::ToTokens for SysfsDirDerive {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let derive_ident = &self.def.ident;
        let const_files = self.files.len();
        let add_index = { // Add len of index into entries
            let mut ts = TokenStream::new();
            if let Some(ref helper) = self.index {
                ts.append_all(quote::quote! {+});
                helper.field.ident.to_tokens(&mut ts);
            }
            ts
        };

        let file_iter = self.files.iter().map(|f| f.pub_name()).flatten();
        let index_list_extend = if let Some(ref index) = self.index {
            let HelperType::Index(ref args) = index.helper else {unreachable!()};
            let keys = args.keys.as_ref().unwrap(); // expected arg.
            Some(quote::quote! {::core::iter::Extend::extend::(&mut v, #keys )})
        } else {
            None
        };

        // iter over all relevant fields. Index is appended to the end, because it must come last.
        let match_getters = self.files.iter().map(|f| f.match_getter()).chain(self.index.as_ref().map(|f| f.match_getter()));
        let store = if let Some(ref index) = self.index {
            let ident = index.field.ident.as_ref().unwrap();
            quote::quote! { #ident .store(name)}
        } else {
            quote::quote! {::core::result::Err(hootux::fs::IoError::NotSupported)}
        };

        let remove = if let Some(ref index) = self.index {
            let ident = index.field.ident.as_ref().unwrap();
            quote::quote! { #ident .remove(name)}
        } else {
            quote::quote! {::core::result::Err(hootux::fs::IoError::DeviceError)}
        };

        let file_ident = self.files.iter().map(|f| f.pub_name()).flatten();


        let ts = quote::quote! {
            impl hootux::fs::sysfs::SysfsDirectory for #derive_ident {
                fn entries(&self) -> usize {
                    #const_files #add_index
                }

                fn file_list(&self) -> Vec<String> {
                    let v = vec![#(#file_iter),*];
                    #index_list_extend
                }

                fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
                    match {
                        #(#match_getters),*
                    }
                }

                fn store(&self, name: &str, file: Box<dyn SysfsFile>) -> Result<(), IoError> {
                    #store
                }

                fn remove(&self, name: &str) -> Result<(), IoError> {
                    match {
                        #(#file_ident) | * => Err(hootux::fs::IoError::NotSupported)
                        r => #remove
                    }
                }
            }
        };

        tokens.append_all(ts)
    }
}

enum HelperType {
    File(HelperArgs),
    Index(HelperArgs),
}

struct HelperArgs {
    alias: Option<String>,
    getter: Option<syn::ExprBlock>,
    keys: Option<syn::ExprBlock>,
}

impl syn::parse::Parse for HelperArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut alias = None;
        let mut getter = None;
        let mut keys = None;

        while !input.is_empty() {
            if input.peek(kw::alias) {
                let _: kw::alias = input.parse()?;
                let _: syn::Token![=] = input.parse()?;
                let ident: syn::Ident = input.parse()?;
                alias.replace(ident.to_string()).ok_or(syn::Error::new(ident.span(), "Found multiple alias"))?;

            } else if input.peek(kw::getter) {
                let _: kw::getter = input.parse()?;
                let _: syn::Token![=] = input.parse()?;
                getter.replace(input.parse()?).ok_or(syn::Error::new(input.span(), "Found multiple getters"))?;

            } else if input.peek(kw::keys) {
                let _: kw::keys = input.parse()?;
                let _: syn::Token![=] = input.parse()?;
                keys.replace(input.parse()?).ok_or(syn::Error::new(input.span(), "Found multiple keys"))?;

            } else {
                return Err(syn::Error::new(input.span(), "Unexpected token"));
            }
        };

        Ok(Self { alias, getter, keys })
    }
}
impl TryFrom<&syn::Attribute> for HelperType {
    type Error = syn::Error;

    fn try_from(value: &Attribute) -> Result<Self, Self::Error> {
        let args: HelperArgs = value.parse_args()?;
        if value.path().is_ident("file") {
            if args.keys.is_some() {
                return Err(syn::Error::new(value.span(), ""));
            }

            Ok(Self::File(args))
        } else if value.path().is_ident("index") {
            if args.alias.is_some() {
                return Err(syn::Error::new(value.span(), "`alias` not valid for index"));
            }
            Ok(Self::Index(args))
        } else {
            Err(syn::Error::new(value.span(), "Unknown helper type"))
        }
    }
}

struct DirDeriveFieldHelper {
    field: syn::Field,
    helper: HelperType,
}

impl DirDeriveFieldHelper {

    /// Resolves helper type and returns parsed config.
    /// If no helper is found returns error message "no-attr".
    fn new(field: &syn::Field) -> syn::Result<DirDeriveFieldHelper> {
        let mut h = None;
        for i in &field.attrs {
            match HelperType::try_from(i) {
                Ok(helper) => h = Some(helper),
                Err(e) if format!("{e}") == "Unknown helper type" => continue,
                Err(e) => return Err(e),
            }
        }

        let helper = h.ok_or(syn::Error::new(field.span(), "no-attr"))?;

        Ok(Self {
            field: field.clone(),
            helper,
        })
    }

    /// Returns the string to be used as the file name.
    ///
    /// Returns `None` if self is for an index
    fn pub_name(&self) -> Option<String> {
        let HelperType::File(helper) = &self.helper else {
            return None;
        };
        if let Some(alias) = &helper.alias {
            Some(alias.clone())
        } else {
            Some(self.field.ident.as_ref().unwrap().to_string())
        }
    }

    fn match_getter(&self) -> TokenStream {
        let pattern = self.pub_name().map(|s| quote::quote! {#s} ).unwrap_or_else(|| quote::quote! {_});
        let (HelperType::Index(HelperArgs{ ref getter ,..}) | HelperType::File(HelperArgs{ ref getter ,..})) = self.helper;
        quote::quote! {#pattern => #getter}
    }
}

mod kw {
    syn::custom_keyword!(alias);
    syn::custom_keyword!(getter);
    syn::custom_keyword!(keys);
}

#[cfg(test)]
mod tests {
    use quote::ToTokens;

    #[test]
    fn test_sysfs_root_impl() {
        let t = syn::parse2(quote::quote! {
            struct TestRoot {
                root: &'static SysFsRoot,
            }
        });

        let t: super::ImplSysFsRoot = t.unwrap();

        let ts: proc_macro2::TokenStream = t.into();
        eprintln!("{:?}", ts.to_string());
    }
}