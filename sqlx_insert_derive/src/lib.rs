extern crate proc_macro;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use std::fmt::Write;
use syn::parse_quote;

// TODO: Attribute for "returning"?
// TODO: Attribute for a custom query finish?
// TODO: Support for batch insert?

const IGNORE_ATTRIBUTE: &str = "ignore";
const ID_ATTRIBUTE: &str = "id";
const RENAME_ATTRIBUTE: &str = "rename";
const INTO_ATTRIBUTE: &str = "into";

// TODO: How should error be handled?

/// Implements SQLInsert trait for a type.
#[proc_macro_derive(SQLInsert, attributes(sqlx_insert))]
pub fn sql_insert_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = TokenStream::from(input);

    let ast = syn::parse2(input).expect("failed to parse macro input");

    // Build the trait implementation
    expand_derive_sql_insert(&ast)
        .expect("failed to expand SQLInsert macro")
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
    let db_params = container_attrs.database;

    let (ret_type, ret_field) = if let Some(returning) = container_attrs.returning {
        (returning.ty, Some(returning.field))
    } else {
        (quote! { () }, None)
    };

    #[cfg(feature = "use-macros")]
    if db_params.len() != 1 {
        return Err(syn::Error::new_spanned(
            input,
            "macro based implementation only works for a single database",
        ));
    }

    if container_attrs.batch_insert_enabled && db_params.len() != 1 || db_params[0].0 != "Postgres"
    {
        return Err(syn::Error::new_spanned(
            input,
            "batch_insert only works for postgres for now",
        ));
    }

    let generics = &input.generics;

    let (_, ty_generics, _) = generics.split_for_impl();
    let mut generics = generics.clone();

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

    for (field, attr) in &fields_with_attr {
        let ty = &field.ty;
        let is_option = is_opt(ty);

        predicates.push(parse_quote!(#ty: Clone + Send + Sync));

        for param in &db_params {
            let param = &param.0;
            if let Some(target) = attr.into.as_ref() {
                predicates.push(parse_quote!(#target: ::sqlx::types::Type<#param>));
                if is_option {
                    let ty = get_option_inner(ty)?;
                    predicates.push(parse_quote!(#ty: ::std::convert::Into<#target>));
                } else {
                    predicates.push(parse_quote!(#ty: ::std::convert::Into<#target>));
                }
            } else {
                predicates.push(parse_quote!(#ty: ::sqlx::types::Type<#param>));
            }
        }
    }

    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let mut names: Vec<String> = Vec::new();
    let mut pg_types: Vec<String> = Vec::new();
    for (field, attr) in &fields_with_attr {
        let id = if let Some(rename) = attr.clone().rename {
            rename
        } else {
            field.ident.as_ref().unwrap().to_string()
        };

        names.push(id);

        if container_attrs.batch_insert_enabled {
            let rust_type = if let Some(target) = attr.into.as_ref() {
                target.to_string()
            } else {
                get_rust_type(&field.ty)?
            };
            pg_types.push(get_pg_type(&rust_type)?);
        }
    }

    let vals: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, _)| format!("${}", i + 1))
        .collect();

    let vec_vals: Vec<String> = if container_attrs.batch_insert_enabled {
        names
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}::{}[]", i + 1, pg_types[i]))
            .collect()
    } else {
        vec![]
    };

    let table_name = if let Some(from_attr) = container_attrs.table {
        from_attr
    } else {
        ident.to_string()
    };

    let mut query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name,
        names.join(", "),
        vals.join(", ")
    );
    if let Some(ret_field) = &ret_field {
        write!(query, " RETURNING {ret_field}")
            .expect("failed to create query string: write error");
    }

    let query_vec = format!(
        "INSERT INTO {} ({}) SELECT * FROM UNNEST({})",
        table_name,
        names.join(", "),
        vec_vals.join(", ")
    );

    let values = fields_with_attr.iter().map(|(field, attrs)| {
        let field_name = field.ident.as_ref().expect("all fields should be named");
        let span = field_name.span();
        let is_option = is_opt(&field.ty);

        if let Some(target) = attrs.into.as_ref() {
            if is_option {
                quote_spanned!( span =>
                    self.#field_name.as_ref().map(|v|
                        std::convert::Into::<#target>::into(v.clone())
                    )
                )
            } else {
                quote_spanned!( span =>
                    std::convert::Into::<#target>::into(self.#field_name.clone())
                )
            }
        } else {
            quote_spanned!( span =>
                self.#field_name.clone()
            )
        }
    });

    let vecs = if container_attrs.batch_insert_enabled {
        let mut temps = Vec::with_capacity(fields_with_attr.len());
        let mut vals = Vec::with_capacity(fields_with_attr.len());

        for (field, attrs) in &fields_with_attr {
            let field_name = field.ident.as_ref().expect("all fields should be named");
            let span = field_name.span();
            let temp_name = format_ident!("{}_value", field_name);
            let is_option = is_opt(&field.ty);

            if let Some(target) = attrs.into.as_ref() {
                if is_option {
                    temps.push(quote_spanned!( span =>
                        let #temp_name = data.iter()
                            .map(|e| e.#field_name.as_ref().map(|v| std::convert::Into::<#target>::into(v.clone()))).collect::<Vec<_>>();
                    ));
                } else {
                    temps.push(quote_spanned!( span =>
                        let #temp_name = data.iter()
                            .map(|e| std::convert::Into::<#target>::into(e.#field_name.clone())).collect::<Vec<_>>();
                    ));
                }
            } else {
                temps.push(quote_spanned!( span =>
                    let #temp_name = data.iter().map(|e| e.#field_name.clone()).collect::<Vec<_>>();
                ));
            }

            let as_opt = if is_option {
                quote! { as &[Option<_>]}
            } else {
                quote! {}
            };
            vals.push(quote_spanned!( span =>
                &#temp_name #as_opt
            ));
        }
        Some((temps, vals))
    } else {
        None
    };

    #[cfg(not(feature = "use-macros"))]
    let bind_extended = values.map(|v| {
        quote! {
            .bind(#v)
        }
    });
    #[cfg(not(feature = "use-macros"))]
    let bind_vecs = vecs.map(|(temps, vals)| {
        (
            temps,
            vals.iter()
                .map(|v| {
                    quote! {
                        .bind(#v)
                    }
                })
                .collect::<Vec<_>>(),
        )
    });

    #[cfg(feature = "use-macros")]
    let macro_values = values.map(|v| {
        quote! {
            , #v
        }
    });
    #[cfg(feature = "use-macros")]
    let macro_vecs = vecs.map(|(temps, vals)| {
        (
            temps,
            vals.iter()
                .map(|v| {
                    quote! {
                        , #v
                    }
                })
                .collect::<Vec<_>>(),
        )
    });

    #[cfg(feature = "use-macros")]
    let insert = if ret_field.is_some() {
        quote! {
            sqlx::query_scalar!(
                #query
                #(#macro_values)*
            )
            .fetch_one(connection).await
        }
    } else {
        quote! {
            let query = sqlx::query!(
                #query
                #(#macro_values)*
            )
            .execute(connection).await?;
            ::std::result::Result::Ok(())
        }
    };
    #[cfg(not(feature = "use-macros"))]
    let insert = if ret_field.is_some() {
        quote! {
            sqlx::query_scalar(
                #query
            )
            #(#bind_extended)*
            .fetch_one(connection).await
        }
    } else {
        quote! {
            let query = sqlx::query(
                #query
            )
            #(#bind_extended)*
            .execute(connection).await?;
            ::std::result::Result::Ok(())
        }
    };

    #[cfg(feature = "use-macros")]
    let insert_vec = macro_vecs.map(|(temps, vals)| {
        quote! {
            #(#temps)*
            let query = sqlx::query!(
                #query_vec
                #(#vals)*
            )
        }
    });
    #[cfg(not(feature = "use-macros"))]
    let insert_vec = bind_vecs.map(|(temps, binds)| {
        quote! {
            #(#temps)*
            let query = sqlx::query(
                #query_vec
            )
            #(#binds)*
        }
    });

    let mut impls = vec![];
    for db_param in db_params {
        let db_param = db_param.0;

        let insert_vec = insert_vec.clone();
        let insert = insert.clone();

        let mut implm = quote! {
            #[automatically_derived]
            impl #impl_generics SQLInsert<#db_param, #ret_type> for #ident #ty_generics #where_clause {

                async fn sql_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = #db_param>>(&self, connection: E) -> ::sqlx::Result<#ret_type> {
                    #[allow(clippy::clone_on_copy)]
                    #insert
                }
            }

        };
        if let Some(insert_vec) = insert_vec {
            implm.extend(quote!{
                #[automatically_derived]
                impl #impl_generics sqlx_insert::BatchInsert<#db_param> for #ident #ty_generics #where_clause {


                    async fn batch_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = #db_param>>(data: &[Self], connection: E) -> ::sqlx::Result<()> {
                        #[allow(clippy::clone_on_copy)]
                        #insert_vec
                        .execute(connection).await?;

                        ::std::result::Result::Ok(())
                    }
                }
            });
        }
        impls.push(implm);
    }
    let res = impls.into_iter().fold(TokenStream::new(), |mut acc, x| {
        acc.extend(x);
        acc
    });
    Ok(res)
}

fn get_option_inner(ty: &syn::Type) -> syn::Result<&syn::Type> {
    if let syn::Type::Path(type_path) = ty {
        let segment = type_path
            .path
            .segments
            .last()
            .ok_or_else(|| syn::Error::new_spanned(ty, "invalid type path"))?;
        if segment.ident.to_string() == "Option" {
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                    return Ok(inner_ty);
                }
            }
            return Err(syn::Error::new_spanned(ty, "invalid Option type"));
        }
    }
    Err(syn::Error::new_spanned(ty, "type is not Option"))
}

fn is_opt(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(type_path) if type_path.path.segments.last().map_or(false, |seg| seg.ident == "Option"))
}

fn get_pg_type(ty: &str) -> syn::Result<String> {
    match ty {
        "i16" => Ok("SMALLINT".to_string()),
        "i32" | "u16" => Ok("INTEGER".to_string()),
        "i64" | "u32" => Ok("BIGINT".to_string()),
        "u64" => Ok("NUMERIC".to_string()),
        "f32" => Ok("REAL".to_string()),
        "f64" => Ok("DOUBLE PRECISION".to_string()),
        "String" => Ok("TEXT".to_string()),
        "bool" => Ok("BOOLEAN".to_string()),
        "Uuid" => Ok("UUID".to_string()),
        "NaiveDateTime" => Ok("TIMESTAMP".to_string()),
        "DateTime" => Ok("TIMESTAMPTZ".to_string()),
        other => Err(syn::Error::new(
            Span::call_site(),
            format!("unsupported type for Postgres: {other}"),
        )),
    }
}

fn get_rust_type(ty: &syn::Type) -> syn::Result<String> {
    match ty {
        syn::Type::Path(type_path) => {
            let segment = type_path
                .path
                .segments
                .last()
                .ok_or_else(|| syn::Error::new_spanned(ty, "invalid type path"))?;
            if segment.ident.to_string() == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return get_rust_type(inner_ty);
                    }
                }
                return Err(syn::Error::new_spanned(ty, "invalid Option type"));
            }
            return Ok(segment.ident.to_string());
        }
        _ => Err(syn::Error::new_spanned(ty, "unable to determine type")),
    }
}

#[derive(Clone)]
struct ContainerAttributes {
    table: Option<String>,
    database: Vec<DBParams>,
    batch_insert_enabled: bool,
    returning: Option<ReturningField>,
}

#[derive(Clone)]
struct ReturningField {
    field: String,
    ty: TokenStream,
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
    let mut db_param = Vec::new();
    let mut batch_insert_enabled = false;
    let mut returning = None;

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
                        syn::Meta::List(meta_list) if meta_list.path.is_ident("database") => {
                            for value in meta_list.nested.iter() {
                                let params: DBParams = syn::parse2(value.into_token_stream())?;
                                db_param.push(params);
                            }
                            Ok(())
                        }
                        syn::Meta::List(meta_list) if meta_list.path.is_ident("returning") => {
                            let (Some(field_name), Some(field_type)) =
                                (meta_list.nested.first(), meta_list.nested.iter().nth(1))
                            else {
                                return Err(syn::Error::new_spanned(
                                    meta_list,
                                    "returning attribute requires a field name and type: e.g. returning(\"id\", i32)",
                                ));
                            };
                            let field_name =
                                if let syn::NestedMeta::Lit(syn::Lit::Str(lit_str)) = field_name {
                                    lit_str.value()
                                } else {
                                    return Err(syn::Error::new_spanned(
                                        field_name,
                                        "expected string literal for field name",
                                    ));
                                };
                            let field_type =
                                if let syn::NestedMeta::Meta(syn::Meta::Path(path)) = field_type {
                                    path.into_token_stream()
                                } else {
                                    return Err(syn::Error::new_spanned(
                                        field_type,
                                        "expected type for returning field",
                                    ));
                                };
                            returning = Some(ReturningField {
                                field: field_name,
                                ty: field_type,
                            });

                            Ok(())
                        }
                        syn::Meta::Path(path) if path.is_ident("batch_insert") => {
                            batch_insert_enabled = true;
                            Ok(())
                        }
                        u => Err(syn::Error::new_spanned(u, "unexpected attribute in a list")),
                    },

                    u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                }?
            }
        }
    }

    if db_param.is_empty() {
        Err(syn::Error::new_spanned(
            "",
            "database attribute is required",
        ))
    } else {
        Ok(ContainerAttributes {
            table,
            database: db_param,
            batch_insert_enabled,
            returning,
        })
    }
}

#[derive(Clone)]
struct FieldAttributes {
    ignore: bool,
    rename: Option<String>,
    into: Option<TokenStream>,
}

fn parse_field_attributes(attrs: &[syn::Attribute]) -> syn::Result<FieldAttributes> {
    let mut sqlx_insert_attrs = FieldAttributes {
        ignore: false,
        rename: None,
        into: None,
    };

    for attr in attrs.iter().filter(|a| a.path.is_ident("sqlx_insert")) {
        let meta = attr
            .parse_meta()
            .map_err(|e| syn::Error::new_spanned(attr, e))?;

        if let syn::Meta::List(list) = meta {
            for value in &list.nested {
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
                        syn::Meta::List(syn::MetaList { path, nested, .. })
                            if path.is_ident(INTO_ATTRIBUTE) =>
                        {
                            sqlx_insert_attrs.into = Some(nested.into_token_stream());
                            Ok(())
                        }
                        syn::Meta::Path(path) if path.is_ident(IGNORE_ATTRIBUTE) => {
                            sqlx_insert_attrs.ignore = true;
                            Ok(())
                        }
                        u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                    },
                    u => Err(syn::Error::new_spanned(u, "unexpected attribute")),
                }?;
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
