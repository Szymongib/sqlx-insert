extern crate proc_macro;

use proc_macro2::{Span, TokenStream};
use quote::__private::ext::RepToTokensExt;
use quote::{quote, quote_spanned, ToTokens};
use syn::{parse_quote, Lifetime};

// TODO: document those attributes
// sqlx_insert(ignore)
// sqlx_insert(rename = "???")

const IGNORE_ATTRIBUTE: &str = "ignore";
const RENAME_ATTRIBUTE: &str = "rename";

// TODO: How should error be handled?

/// Implements SQLInsert trait for a type.
#[proc_macro_derive(SQLInsert, attributes(sqlx_insert))]
pub fn sql_insert_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = TokenStream::from(input);

    let ast = syn::parse2(input).expect("failed to parse macro input");

    // Build the trait implementation
    expand_derive_sql_insert(&ast)
        .expect("failed to expand SQLInsert macro") // TODO: what is convention here?
        .into()
}

fn expand_derive_sql_insert(input: &syn::DeriveInput) -> syn::Result<TokenStream> {
    match &input.data {
        syn::Data::Struct(syn::DataStruct {
            fields: syn::Fields::Named(syn::FieldsNamed { named, .. }),
            ..
        }) => expand_derive_sql_insert_struct(input, named),

        // TODO: support unnamed and unit structs
        syn::Data::Struct(_) => Err(syn::Error::new_spanned(
            input,
            "only named struct fields are supported",
        )),
        syn::Data::Enum(_) => Err(syn::Error::new_spanned(input, "enums are not supported")),
        syn::Data::Union(_) => Err(syn::Error::new_spanned(input, "unions are not supported")),
    }
}

fn expand_derive_sql_insert_struct(
    input: &syn::DeriveInput,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> syn::Result<TokenStream> {
    let ident = &input.ident;

    let container_attrs =
        parse_container_attributes(&input.attrs).expect("failed to parse container attrs");
    let db_param = container_attrs.database.0;

    let generics = &input.generics;

    let (lifetime, provided) = generics
        .lifetimes()
        .next()
        .map(|def| (def.lifetime.clone(), false))
        .unwrap_or_else(|| (Lifetime::new("'a", Span::call_site()), true));

    let (_, ty_generics, _) = generics.split_for_impl();
    let mut generics = generics.clone();

    if provided {
        generics.params.insert(0, parse_quote!(#lifetime));
    }
    let predicates = &mut generics.make_where_clause().predicates;

    // Check field attributes and filter ignored fields.
    let fields_with_attr: Vec<(&syn::Field, FieldAttributes)> = fields
        .into_iter()
        .map(|f| {
            let attributes =
                parse_field_attributes(&f.attrs).expect("Failed to parse fields attribute.");
            (f, attributes)
        })
        .filter(|(_, attr)| !attr.ignore)
        .collect();

    // Set additional sqlx constraints for types.
    for (field, _) in &fields_with_attr {
        let ty = &field.ty;

        predicates.push(parse_quote!(#ty: #lifetime));
        predicates.push(parse_quote!(#ty: Clone + Send + Sync));
        predicates.push(parse_quote!(#ty: ::sqlx::encode::Encode<#lifetime, #db_param>));
        predicates.push(parse_quote!(#ty: ::sqlx::types::Type<#db_param>));
    }

    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let mut names: Vec<String> = Vec::new();
    for (field, attr) in fields_with_attr.iter() {
        let id = if let Some(rename) = attr.clone().rename {
            rename
        } else {
            field.ident.as_ref().unwrap().to_string()
        };

        names.push(id);
    }

    let vals: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, _)| format!("${}", i + 1))
        .collect();

    let table_name = if let Some(from_attr) = container_attrs.table {
        from_attr
    } else {
        ident.to_string()
    };

    let query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name,
        names.join(", "),
        vals.join(", ")
    );

    let bind_extended = fields_with_attr.into_iter().map(|(field, _)| {
        let field_name = field.clone().ident.expect("all fields should be named");
        let span = field_name.span();

        quote_spanned!( span =>
            .bind(self.#field_name.clone())
        )
    });

    Ok(quote! {
        #[automatically_derived]
        #[async_trait]
        impl #impl_generics SQLInsert<#db_param> for #ident #ty_generics #where_clause {

            async fn sql_insert<'c, E: sqlx::Executor<'c, Database = #db_param>>(&self, connection: E) -> ::sqlx::Result<()> {
                let query = sqlx::query(
                    #query
                )
                #(#bind_extended)*
                .execute(connection).await?;

                ::std::result::Result::Ok(())
            }
        }
    })
}

#[derive(Clone)]
struct ContainerAttributes {
    table: Option<String>,
    database: DBParams,
}

#[derive(Clone, Debug)]
struct DBParams(syn::Ident);

impl syn::parse::Parse for DBParams {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let type1 = input.parse()?;
        Ok(DBParams(type1))
    }
}

fn parse_container_attributes(attrs: &[syn::Attribute]) -> syn::Result<ContainerAttributes> {
    let mut table = None;
    let mut db_param = None;

    for attr in attrs.iter().filter(|a| a.path.is_ident("sqlx_insert")) {
        let meta = attr
            .parse_meta()
            .map_err(|e| syn::Error::new_spanned(attr, e))
            .expect("failed to parse ATTR");

        if let syn::Meta::List(list) = meta {
            for value in list.nested.iter() {
                match value {
                    syn::NestedMeta::Meta(meta) => match meta {
                        syn::Meta::NameValue(syn::MetaNameValue {
                            path,
                            lit: syn::Lit::Str(val),
                            ..
                        }) if path.is_ident("table") => {
                            table = Some(val.value());
                            Ok(())
                        }
                        syn::Meta::List(path) if path.path.is_ident("database") => {
                            let db_params: DBParams =
                                syn::parse2(path.nested.next().unwrap().into_token_stream())?;
                            db_param = Some(db_params);
                            Ok(())
                        }
                        u => Err(syn::Error::new_spanned(u, "unexpected attribute in a list")),
                    },

                    u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                }?
            }
        }
    }

    // TODO: can I do it better?
    if db_param.is_none() {
        Err(syn::Error::new_spanned(
            "TODO",
            "database parameter not found",
        ))
    } else {
        Ok(ContainerAttributes {
            table,
            database: db_param.unwrap(),
        })
    }
}

#[derive(Clone)]
struct FieldAttributes {
    ignore: bool,
    rename: Option<String>,
}

fn parse_field_attributes(attrs: &[syn::Attribute]) -> syn::Result<FieldAttributes> {
    let mut sqlx_insert_attrs = FieldAttributes {
        ignore: false,
        rename: None,
    };

    for attr in attrs.iter().filter(|a| a.path.is_ident("sqlx_insert")) {
        let meta = attr
            .parse_meta()
            .map_err(|e| syn::Error::new_spanned(attr, e))?;

        if let syn::Meta::List(list) = meta {
            for value in list.nested.iter() {
                match value {
                    syn::NestedMeta::Meta(meta) => match meta {
                        syn::Meta::NameValue(syn::MetaNameValue {
                            path,
                            lit: syn::Lit::Str(val),
                            ..
                        }) if path.is_ident(RENAME_ATTRIBUTE) => {
                            sqlx_insert_attrs.rename = Some(val.value());
                            Ok(())
                        }
                        syn::Meta::Path(path) if path.is_ident(IGNORE_ATTRIBUTE) => {
                            sqlx_insert_attrs.ignore = true;
                            Ok(())
                        }
                        u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                    },
                    u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                }?
            }
        }
    }

    Ok(sqlx_insert_attrs)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test() {
        // TODO: any tests here?
    }
}
