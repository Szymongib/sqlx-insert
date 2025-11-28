extern crate proc_macro;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use std::fmt::Write;
use syn::{parse_macro_input, parse_quote, spanned::Spanned};

// TODO: Attribute for a custom query finish?
// TODO: Support for batch insert? (done for Postgres)

const IGNORE_ATTRIBUTE: &str = "ignore";
const RENAME_ATTRIBUTE: &str = "rename";
const INTO_ATTRIBUTE: &str = "into";

/// Implements `SQLInsert` trait for a type.
#[proc_macro_derive(SQLInsert, attributes(sqlx_insert))]
pub fn sql_insert_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let ast = parse_macro_input!(input as syn::DeriveInput);

    // Build the trait implementation
    expand_derive_sql_insert(&ast)
        .unwrap_or_else(|e| e.to_compile_error())
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

#[allow(clippy::too_many_lines)]
fn expand_derive_sql_insert_struct(
    input: &syn::DeriveInput,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> syn::Result<TokenStream> {
    let ident = &input.ident;

    let (container_attrs, db_params) = parse_container_attributes(input.span(), &input.attrs)?;

    let ret_type = if let Some(returning) = &container_attrs.returning {
        returning.ty.clone()
    } else {
        quote! { () }
    };

    if cfg!(feature = "use-macros") && db_params.len() != 1 {
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
    let mut fields_with_attr = Vec::with_capacity(fields.len());
    for f in fields {
        let attributes = parse_field_attributes(&f.attrs)?;
        if !attributes.ignore {
            fields_with_attr.push((f, attributes));
        }
    }

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

    let (insert, insert_vec) = create_sqlx_calls(ident, container_attrs, &fields_with_attr)?;

    let mut impls = vec![];
    for db_param in db_params {
        let db_param = db_param.0;

        let insert_vec = insert_vec.clone();
        let insert = insert.clone();

        let mut impl_block = quote! {
            #[automatically_derived]
            impl #impl_generics SQLInsert<#db_param, #ret_type> for #ident #ty_generics #where_clause {

                #[allow(clippy::clone_on_copy)]
                async fn sql_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = #db_param>>(&self, connection: E) -> ::sqlx::Result<#ret_type> {
                    #insert
                }
            }

        };
        if let Some(insert_vec) = insert_vec {
            impl_block.extend(quote!{
                #[automatically_derived]
                impl #impl_generics sqlx_insert::BatchInsert<#db_param> for #ident #ty_generics #where_clause {


                    #[allow(clippy::clone_on_copy)]
                    async fn batch_insert<'e, 'c, E: 'e + sqlx::Executor<'c, Database = #db_param>>(data: &[Self], connection: E) -> ::sqlx::Result<()> {
                        #insert_vec
                        .execute(connection).await?;

                        ::std::result::Result::Ok(())
                    }
                }
            });
        }
        impls.push(impl_block);
    }
    let res = impls.into_iter().fold(TokenStream::new(), |mut acc, x| {
        acc.extend(x);
        acc
    });
    Ok(res)
}

fn create_sqlx_calls(
    ident: &syn::Ident,
    container_attrs: ContainerAttributes,
    fields_with_attr: &[(&syn::Field, FieldAttributes)],
) -> Result<(TokenStream, Option<TokenStream>), syn::Error> {
    let is_returning = container_attrs.returning.is_some();
    let (query, query_vec) = create_queries(
        ident,
        container_attrs.batch_insert_enabled,
        container_attrs.update,
        container_attrs.returning.map(|r| r.field),
        fields_with_attr,
        container_attrs.table,
    )?;

    let values = create_values(container_attrs.batch_insert_enabled, fields_with_attr);

    if cfg!(feature = "use-macros") {
        Ok(create_macro_calls(&query, &query_vec, values, is_returning))
    } else {
        Ok(create_non_macro_calls(
            &query,
            &query_vec,
            values,
            is_returning,
        ))
    }
}

fn create_macro_calls(
    query: &str,
    query_vec: &str,
    values: QueryValues,
    is_returning: bool,
) -> (TokenStream, Option<TokenStream>) {
    let macro_values = values.single.iter().map(|v| {
        quote! {
            , #v
        }
    });

    let macro_vecs = values.vecs.map(|(temps, vals)| {
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

    let insert = if is_returning {
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

    let insert_vec = macro_vecs.map(|(temps, vals)| {
        quote! {
            #(#temps)*
            let query = sqlx::query!(
                #query_vec
                #(#vals)*
            )
        }
    });
    (insert, insert_vec)
}

fn create_non_macro_calls(
    query: &str,
    query_vec: &str,
    values: QueryValues,
    is_returning: bool,
) -> (TokenStream, Option<TokenStream>) {
    let bind_extended = values.single.iter().map(|v| {
        quote! {
            .bind(#v)
        }
    });

    let bind_vecs = values.vecs.map(|(temps, vals)| {
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

    let insert = if is_returning {
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

    let insert_vec = bind_vecs.map(|(temps, binds)| {
        quote! {
            #(#temps)*
            let query = sqlx::query(
                #query_vec
            )
            #(#binds)*
        }
    });
    (insert, insert_vec)
}

struct QueryValues {
    single: Vec<TokenStream>,
    vecs: Option<(Vec<TokenStream>, Vec<TokenStream>)>,
}
fn create_values(
    batch_insert_enabled: bool,
    fields_with_attr: &[(&syn::Field, FieldAttributes)],
) -> QueryValues {
    let values = fields_with_attr
        .iter()
        .map(|(field, attrs)| {
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
        })
        .collect::<Vec<_>>();

    let vecs = if batch_insert_enabled {
        let mut temps = Vec::with_capacity(fields_with_attr.len());
        let mut vals = Vec::with_capacity(fields_with_attr.len());

        for (field, attrs) in fields_with_attr {
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
    QueryValues {
        single: values,
        vecs,
    }
}

const WRITE_ERROR: &str = "failed to create query string: write error";
fn create_queries(
    ident: &syn::Ident,
    batch_insert_enabled: bool,
    update: Option<String>,
    ret_field: Option<String>,
    fields_with_attr: &[(&syn::Field, FieldAttributes)],
    table: Option<String>,
) -> Result<(String, String), syn::Error> {
    let mut names: Vec<String> = Vec::new();
    let mut pg_types: Vec<String> = Vec::new();
    for (field, attr) in fields_with_attr {
        let name = if let Some(rename) = attr.clone().rename {
            rename
        } else {
            field.ident.as_ref().unwrap().to_string()
        };

        names.push(name);

        if batch_insert_enabled {
            let rust_type = if let Some(target) = attr.into.as_ref() {
                target.to_string()
            } else {
                get_rust_type(&field.ty)?
            };
            pg_types.push(get_pg_type(&rust_type, field.ty.span())?);
        }
    }
    let vals: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, _)| format!("${}", i + 1))
        .collect();
    let vec_vals: Vec<String> = if batch_insert_enabled {
        names
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}::{}[]", i + 1, pg_types[i]))
            .collect()
    } else {
        vec![]
    };
    let table_name = if let Some(from_attr) = table {
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
    if let Some(ret_field) = ret_field {
        write!(query, " RETURNING {ret_field}").expect(WRITE_ERROR);
    }
    let mut query_vec = format!(
        "INSERT INTO {} ({}) SELECT * FROM UNNEST({})",
        table_name,
        names.join(", "),
        vec_vals.join(", ")
    );
    if let Some(update) = update {
        write!(query, " ON CONFLICT ({update}) DO UPDATE SET ").expect(WRITE_ERROR);
        write!(query_vec, " ON CONFLICT ({update}) DO UPDATE SET ").expect(WRITE_ERROR);
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                write!(query, ", ").expect(WRITE_ERROR);
                write!(query_vec, ", ").expect(WRITE_ERROR);
            }
            write!(query, "{name} = EXCLUDED.{name}").expect(WRITE_ERROR);
            write!(query_vec, "{name} = EXCLUDED.{name}").expect(WRITE_ERROR);
        }
    }
    Ok((query, query_vec))
}

fn get_option_inner(ty: &syn::Type) -> syn::Result<&syn::Type> {
    if let syn::Type::Path(type_path) = ty {
        let segment = type_path
            .path
            .segments
            .last()
            .ok_or_else(|| syn::Error::new_spanned(ty, "invalid type path"))?;
        if segment.ident == "Option" {
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
    matches!(ty, syn::Type::Path(type_path) if type_path.path.segments.last().is_some_and(|seg| seg.ident == "Option"))
}

fn get_pg_type(ty: &str, span: Span) -> syn::Result<String> {
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
            span,
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
            if segment.ident == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return get_rust_type(inner_ty);
                    }
                }
                return Err(syn::Error::new_spanned(ty, "invalid Option type"));
            }
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(ty, "unable to determine type")),
    }
}

#[derive(Clone)]
struct ContainerAttributes {
    table: Option<String>,
    batch_insert_enabled: bool,
    returning: Option<ReturningField>,
    update: Option<String>,
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

fn parse_container_attributes(
    span: Span,
    attrs: &[syn::Attribute],
) -> syn::Result<(ContainerAttributes, Vec<DBParams>)> {
    let mut table = None;
    let mut db_param = Vec::new();
    let mut batch_insert_enabled = false;
    let mut returning = None;
    let mut update = None;

    for attr in attrs.iter().filter(|a| a.path.is_ident("sqlx_insert")) {
        let meta = attr.parse_meta()?;

        if let syn::Meta::List(list) = meta {
            for value in &list.nested {
                match value {
                    syn::NestedMeta::Meta(meta) => match meta {
                        syn::Meta::NameValue(syn::MetaNameValue {
                            path,
                            lit: syn::Lit::Str(val),
                            ..
                        }) if path.is_ident("table") => {
                            table = Some(val.value());
                        }
                        syn::Meta::List(meta_list) if meta_list.path.is_ident("database") => {
                            for value in &meta_list.nested {
                                let params: DBParams = syn::parse2(value.into_token_stream())?;
                                db_param.push(params);
                            }
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
                        }
                        syn::Meta::List(meta_list) if meta_list.path.is_ident("update") => {
                            if let Some(syn::NestedMeta::Lit(syn::Lit::Str(lit_str))) =
                                meta_list.nested.first()
                            {
                                update = Some(lit_str.value());
                            } else {
                                return Err(syn::Error::new_spanned(
                                    meta_list,
                                    "expected string literal for update clause",
                                ));
                            }
                        }
                        syn::Meta::Path(path) if path.is_ident("batch_insert") => {
                            batch_insert_enabled = true;
                        }
                        u => {
                            return Err(syn::Error::new_spanned(
                                u,
                                "unexpected attribute in a list",
                            ))
                        }
                    },

                    u @ syn::NestedMeta::Lit(_) => {
                        return Err(syn::Error::new_spanned(u, "unexpected attribute"))
                    }
                }
            }
        }
    }

    if db_param.is_empty() {
        Err(syn::Error::new(span, "database attribute is required"))
    } else {
        Ok((
            ContainerAttributes {
                table,
                batch_insert_enabled,
                returning,
                update,
            },
            db_param,
        ))
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
                    u @ syn::NestedMeta::Lit(_) => {
                        Err(syn::Error::new_spanned(u, "unexpected attribute"))
                    }
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
