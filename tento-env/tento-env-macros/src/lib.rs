use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, Ident, LitStr, Token, Type, parse_macro_input};

fn env_facade() -> Ident {
  use proc_macro_crate::{FoundCrate, crate_name};

  let alias = match crate_name("tento-env") {
    Ok(FoundCrate::Name(name)) => dependency_ident(&name),
    _ => None,
  };
  if let Some(ident) = alias {
    return ident;
  }
  // The facade aliases itself as `tento_env`; its examples, integration tests
  // and doctests also link the library under that name.
  Ident::new("tento_env", proc_macro2::Span::call_site())
}

fn dependency_ident(name: &str) -> Option<Ident> {
  // Cargo normalizes hyphens to underscores. Use the Rust 2024 strict and
  // reserved keywords here because syn may accept some of them as plain idents.
  match name {
    // Rust forbids these spellings even as raw identifiers.
    "_" | "crate" | "self" | "Self" | "super" => None,
    "as" | "async" | "await" | "break" | "const" | "continue" | "dyn" | "else" | "enum"
    | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match"
    | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "static" | "struct" | "trait"
    | "true" | "type" | "unsafe" | "use" | "where" | "while" | "abstract" | "become" | "box"
    | "do" | "final" | "gen" | "macro" | "override" | "priv" | "try" | "typeof" | "unsized"
    | "virtual" | "yield" => syn::parse_str(&format!("r#{name}")).ok(),
    _ => syn::parse_str(name).ok(),
  }
}

/// Converts a SCREAMING_SNAKE_CASE string to PascalCase.
///
/// Examples:
/// - "DATABASE_URL" -> "DatabaseUrl"
/// - "DEMO_JS" -> "DemoJs"
/// - "PORT" -> "Port"
fn screaming_snake_to_pascal(s: &str) -> String {
  s.split('_')
    .filter(|part| !part.is_empty())
    .map(|part| {
      let mut chars = part.chars();
      match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
      }
    })
    .collect()
}

// This is syntactic recognition, not type resolution: aliases and custom paths
// deliberately keep the generic formatting behavior.
fn is_std_type(ty: &Type, module: &str, name: &str) -> bool {
  let Type::Path(ty) = ty else {
    return false;
  };
  if ty.qself.is_some()
    || ty
      .path
      .segments
      .iter()
      .any(|segment| !segment.arguments.is_none())
  {
    return false;
  }
  let segments = &ty.path.segments;
  (ty.path.leading_colon.is_none() && segments.len() == 1 && segments[0].ident == name)
    || (segments.len() == 3
      && segments[0].ident == "std"
      && segments[1].ident == module
      && segments[2].ident == name)
}

fn value_type_hint(ty: &Type) -> Ident {
  let hint = if is_std_type(ty, "path", "PathBuf") {
    "Path"
  } else {
    let spelling = quote!(#ty).to_string();
    match spelling.as_str() {
      "bool" => "Bool",
      "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128"
      | "isize" | "f32" | "f64" => "Number",
      _ if spelling.contains("Redacted") => "Redacted",
      _ => "String",
    }
  };
  Ident::new(hint, proc_macro2::Span::call_site())
}

struct EnvKvInput {
  struct_name: Ident,
  key: LitStr,
  ty: Type,
  default: Option<syn::Expr>,
  secret: bool,
}

/// Optional `env_kv!` modifiers, comma-separated after the type.
struct EnvKvModifiers {
  default: Option<syn::Expr>,
  secret: bool,
}

/// Parse optional modifiers from input stream: `default = expr`, `secret`.
fn parse_modifiers(input: syn::parse::ParseStream) -> syn::Result<EnvKvModifiers> {
  let mut default = None;
  let mut secret = false;

  while input.peek(Token![,]) {
    input.parse::<Token![,]>()?;
    let ident: Ident = input.parse()?;

    if ident == "default" {
      input.parse::<Token![=]>()?;
      if default.is_some() {
        return Err(syn::Error::new(
          ident.span(),
          "duplicate `default` modifier",
        ));
      }
      default = Some(input.parse()?);
    } else if ident == "secret" {
      if secret {
        return Err(syn::Error::new(ident.span(), "duplicate `secret` modifier"));
      }
      secret = true;
    } else {
      return Err(syn::Error::new(
        ident.span(),
        "expected `default` or `secret`",
      ));
    }
  }

  Ok(EnvKvModifiers { default, secret })
}

// Environment names cannot contain NUL or `=`; empty names cannot be loaded.
fn validate_key(key: &LitStr) -> syn::Result<()> {
  let value = key.value();
  if value.is_empty() || value.contains(['\0', '=']) {
    return Err(syn::Error::new(
      key.span(),
      "environment key must be nonempty and contain neither NUL nor `=`",
    ));
  }
  Ok(())
}

impl syn::parse::Parse for EnvKvInput {
  fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
    // Try to parse as: StructName, "KEY", Type
    // or as: "KEY", Type
    let first: syn::Expr = input.parse()?;

    match first {
      // If it's a string literal, derive the struct name from it
      syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(key),
        ..
      }) => {
        input.parse::<Token![,]>()?;
        let ty: Type = input.parse()?;
        let EnvKvModifiers { default, secret } = parse_modifiers(input)?;

        let struct_name_str = screaming_snake_to_pascal(&key.value());
        validate_key(&key)?;
        let mut struct_name = syn::parse_str::<Ident>(&struct_name_str).map_err(|_| {
          syn::Error::new(
            key.span(),
            "key must produce a valid Rust identifier; use the named form",
          )
        })?;
        struct_name.set_span(key.span());

        Ok(EnvKvInput {
          struct_name,
          key,
          ty,
          default,
          secret,
        })
      }
      // If it's an identifier, it's the struct name
      syn::Expr::Path(syn::ExprPath { path, .. }) => {
        let struct_name = path
          .get_ident()
          .cloned()
          .ok_or_else(|| syn::Error::new_spanned(&path, "expected identifier"))?;

        input.parse::<Token![,]>()?;
        let key: LitStr = input.parse()?;
        validate_key(&key)?;
        input.parse::<Token![,]>()?;
        let ty: Type = input.parse()?;
        let EnvKvModifiers { default, secret } = parse_modifiers(input)?;

        Ok(EnvKvInput {
          struct_name,
          key,
          ty,
          default,
          secret,
        })
      }
      _ => Err(syn::Error::new_spanned(
        first,
        "expected string literal or identifier",
      )),
    }
  }
}

/// Creates a newtype struct for a single environment variable.
///
/// # Forms
///
/// ## Standard Form (derives struct name from key)
///
/// ```ignore
/// env_kv!("DATABASE_URL", String);
/// env_kv!("DEMO_JS", PathBuf);
/// env_kv!("PORT", u16, default = 8080);
/// ```
///
/// Produces:
/// - `DatabaseUrl(String)` from "DATABASE_URL"
/// - `DemoJs(PathBuf)` from "DEMO_JS"
/// - `Port(u16)` with default value 8080
///
/// ## Named Form (explicit struct name)
///
/// ```ignore
/// env_kv!(StripeApiKey, "API_KEY", String);
/// env_kv!(SendgridApiKey, "API_KEY", String);
/// ```
///
/// Use this when the derived name would conflict or you want a custom name.
///
/// # Generated Code
///
/// The macro generates:
/// - A newtype struct with `Debug`, `Clone` derives
/// - `Deref` implementation for ergonomic access
/// - `Display` implementation
/// - `FromEnvVar` implementation for loading from environment
///
/// # Example
///
/// ```ignore
/// use tento_env::{Env, FromEnvVar, env_kv};
///
/// env_kv!("DATABASE_URL", String);
/// env_kv!("PORT", u16, default = 8080);
///
/// // Load from environment
/// let db: Env<DatabaseUrl> = DatabaseUrl::from_env()?;
/// let port: Env<Port> = Port::from_env()?; // Returns default if not set
/// ```
#[proc_macro]
pub fn env_kv(input: TokenStream) -> TokenStream {
  expand_env_kv(&parse_macro_input!(input as EnvKvInput), &env_facade()).into()
}

fn expand_env_kv(input: &EnvKvInput, facade: &Ident) -> proc_macro2::TokenStream {
  let EnvKvInput {
    struct_name,
    key,
    ty,
    default,
    secret,
  } = input;

  let from_env_impl = default.as_ref().map(|default_expr| {
    quote! {
      const HAS_ABSENT_FALLBACK: ::core::primitive::bool = true;

      fn from_env() -> ::core::result::Result<::#facade::Env<Self>, ::#facade::EnvError> {
        let value = match ::#facade::resolve_key(Self::KEY)
          .map_err(<Self as ::#facade::FromEnvVar>::__sanitize_lookup_error)?
        {
          ::core::option::Option::Some(s) => Self::parse_value(&s)?,
          ::core::option::Option::None => #struct_name(#default_expr),
        };
        ::core::result::Result::Ok(::#facade::Env::new(Self::KEY, value))
      }
    }
  });

  // Generate Default impl if default value is provided
  let default_impl = default.as_ref().map(|default_expr| {
    quote! {
      impl ::std::default::Default for #struct_name {
        fn default() -> Self {
          #struct_name(#default_expr)
        }
      }
    }
  });

  let is_pathbuf = is_std_type(ty, "path", "PathBuf");
  let is_string = is_std_type(ty, "string", "String");
  let value_type = value_type_hint(ty);

  // Generate Display impl - PathBuf and String must not pick up Debug quotes:
  // a quoted value used as a path resolves as RELATIVE (it starts with `"`),
  // silently creating `"/home/user"/...` dirs under the process cwd.
  let display_impl = if is_pathbuf {
    quote! {
      impl ::std::fmt::Display for #struct_name {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          ::std::write!(f, "{}", self.0.display())
        }
      }
    }
  } else if is_string {
    quote! {
      impl ::std::fmt::Display for #struct_name {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          ::std::write!(f, "{}", self.0)
        }
      }
    }
  } else {
    quote! {
      impl ::std::fmt::Display for #struct_name {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          ::std::fmt::Debug::fmt(&self.0, f)
        }
      }
    }
  };

  quote! {
    #[derive(::core::fmt::Debug, ::core::clone::Clone)]
    pub struct #struct_name(pub #ty);

    impl ::#facade::serde::Serialize for #struct_name {
      fn serialize<S>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error>
      where
        S: ::#facade::serde::Serializer,
      {
        ::#facade::serde::Serialize::serialize(&self.0, serializer)
      }
    }

    impl<'de> ::#facade::serde::Deserialize<'de> for #struct_name {
      fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
      where
        D: ::#facade::serde::Deserializer<'de>,
      {
        <<Self as ::core::ops::Deref>::Target as ::#facade::serde::Deserialize<'de>>::deserialize(deserializer)
          .map(#struct_name)
      }
    }

    impl #struct_name {
      /// Create a new Env wrapper from a value, using the correct KEY.
      ///
      /// This is useful when programmatically constructing values
      /// (e.g., deriving paths from a bundle root) rather than loading from env.
      pub fn new(value: #ty) -> ::#facade::Env<Self> {
        ::#facade::Env::new(<Self as ::#facade::FromEnvVar>::KEY, #struct_name(value))
      }
    }

    impl ::std::ops::Deref for #struct_name {
      type Target = #ty;

      fn deref(&self) -> &#ty {
        &self.0
      }
    }

    #display_impl

    #default_impl

    impl ::#facade::FromEnvVar for #struct_name {
      const KEY: &'static ::core::primitive::str = #key;
      const VALUE_TYPE: ::#facade::ValueTypeHint = ::#facade::ValueTypeHint::#value_type;
      const SECRET: ::core::primitive::bool = #secret;

      fn parse_value(s: &::core::primitive::str) -> ::core::result::Result<Self, ::#facade::EnvError> {
        let value = <#ty as ::#facade::ParseEnvValue>::parse_env_value(Self::KEY, s)?;
        ::core::result::Result::Ok(#struct_name(value))
      }

      fn __sanitize_lookup_error(error: ::#facade::EnvError) -> ::#facade::EnvError {
        <#ty as ::#facade::ParseEnvValue>::__sanitize_lookup_error(Self::KEY, error)
      }

      #from_env_impl
    }

    impl ::#facade::EnvTemplate for #struct_name {
      fn env_template() -> ::#facade::EnvGroupMeta {
        ::#facade::__key_template::<Self>(::std::stringify!(#struct_name))
      }

      fn env_fallbacks() -> ::#facade::EnvFallbacks {
        ::#facade::__key_fallbacks::<Self>()
      }
    }

    impl ::#facade::PrintEnv for #struct_name {
      fn print_env(&self) {
        ::std::eprintln!("{}", self.fmt_env());
      }

      fn fmt_env(&self) -> ::std::string::String {
        <Self as ::#facade::PrintEnv>::fmt_env_indented(self, 0)
      }

      fn fmt_env_indented(&self, indent: ::core::primitive::usize) -> ::std::string::String {
        ::#facade::__format_key::<Self>(indent, &self.0)
      }

      fn fmt_env_with_seen(&self, indent: ::core::primitive::usize, _seen: &mut ::std::collections::HashSet<&'static ::core::primitive::str>) -> ::std::string::String {
        self.fmt_env_indented(indent)
      }

      fn type_name() -> &'static ::core::primitive::str {
        ::std::any::type_name::<Self>()
      }
    }
  }
}

/// Represents a parsed field from the struct.
enum FieldKind {
  /// `field: Env<SomeType>` - required field with env var wrapper
  EnvRequired { inner_type: Type },
  /// `field: Option<Env<SomeType>>` - optional field with env var wrapper
  EnvOptional { inner_type: Type },
  /// `field: EnvResult<SomeType>` - three-state result field for explicit handling
  EnvResultField { inner_type: Type },
  /// `field: EnvGroup<SomeType>` where SomeType implements FromEnv - nested config
  Nested { inner_type: Type },
  /// `field: Option<EnvGroup<SomeType>>` where SomeType implements FromEnv - optional nested config
  NestedOptional { inner_type: Type },
  /// `field: EnvGroupResult<SomeType>` - deferred result for nested config, caller handles errors
  NestedResult { inner_type: Type },
}

struct InlineKey {
  key: LitStr,
  default: Option<syn::Expr>,
}

fn parse_inline_key(field: &syn::Field) -> syn::Result<Option<InlineKey>> {
  let mut attributes = field
    .attrs
    .iter()
    .filter(|attr| attr.path().is_ident("env"));
  let Some(attr) = attributes.next() else {
    return Ok(None);
  };
  if let Some(duplicate) = attributes.next() {
    return Err(syn::Error::new_spanned(
      duplicate,
      "duplicate field `env` attribute",
    ));
  }

  let mut key = None;
  let mut default = None;
  attr.parse_nested_meta(|meta| {
    if meta.path.is_ident("from") {
      if key.is_some() {
        return Err(meta.error("duplicate `from` option"));
      }
      let value = meta.value()?.parse::<LitStr>()?;
      validate_key(&value)?;
      key = Some(value);
    } else if meta.path.is_ident("default") {
      if default.is_some() {
        return Err(meta.error("duplicate `default` option"));
      }
      default = Some(meta.value()?.parse()?);
    } else {
      return Err(meta.error("expected `from` or `default`"));
    }
    Ok(())
  })?;
  let key = key.ok_or_else(|| syn::Error::new_spanned(attr, "expected `from = \"KEY\"`"))?;
  if parse_field_kind(&field.ty).is_some() {
    return Err(syn::Error::new_spanned(
      attr,
      "use a plain field type with `env(from = ...)`",
    ));
  }
  Ok(Some(InlineKey { key, default }))
}

/// Parse the field type to determine how to load it.
fn parse_field_kind(ty: &Type) -> Option<FieldKind> {
  // Get the path from the type
  let path = match ty {
    Type::Path(type_path) => &type_path.path,
    _ => return None,
  };

  // Get the last segment (the actual type name)
  let segment = path.segments.last()?;
  let ident = &segment.ident;

  if ident == "Env" {
    // It's `Env<T>`, extract T
    let inner = extract_generic_arg(segment)?;
    Some(FieldKind::EnvRequired { inner_type: inner })
  } else if ident == "EnvResult" {
    // It's `EnvResult<T>`, extract T
    let inner = extract_generic_arg(segment)?;
    Some(FieldKind::EnvResultField { inner_type: inner })
  } else if ident == "EnvGroup" {
    // It's `EnvGroup<T>` where T implements FromEnv
    let inner = extract_generic_arg(segment)?;
    Some(FieldKind::Nested { inner_type: inner })
  } else if ident == "EnvGroupResult" {
    // It's `EnvGroupResult<T>` where T implements FromEnv - deferred result
    let inner = extract_generic_arg(segment)?;
    Some(FieldKind::NestedResult { inner_type: inner })
  } else if ident == "Option" {
    // It's `Option<...>`, check if inner is `Env<T>` or `EnvGroup<T>`
    let inner = extract_generic_arg(segment)?;
    if let Type::Path(inner_path) = &inner {
      let inner_segment = inner_path.path.segments.last()?;
      if inner_segment.ident == "Env" {
        let env_inner = extract_generic_arg(inner_segment)?;
        return Some(FieldKind::EnvOptional {
          inner_type: env_inner,
        });
      } else if inner_segment.ident == "EnvGroup" {
        let group_inner = extract_generic_arg(inner_segment)?;
        return Some(FieldKind::NestedOptional {
          inner_type: group_inner,
        });
      }
    }
    None
  } else {
    None
  }
}

/// Extract the first generic argument from a path segment.
fn extract_generic_arg(segment: &syn::PathSegment) -> Option<Type> {
  match &segment.arguments {
    syn::PathArguments::AngleBracketed(args) => args.args.first().and_then(|arg| {
      if let syn::GenericArgument::Type(ty) = arg {
        Some(ty.clone())
      } else {
        None
      }
    }),
    _ => None,
  }
}

/// Derive macro for generating `FromEnv` and `PrintEnv` implementations on configuration structs.
///
/// # Usage
///
/// ```ignore
/// use tento_env::{env_kv, Env, EnvGroupResult, FromEnv, PrintEnv};
///
/// env_kv!("DATABASE_URL", String);
/// env_kv!("PORT", u16, default = 8080);
/// env_kv!("API_KEY", String);
///
/// #[derive(FromEnv)]
/// struct AppConfig {
///   database_url: Env<DatabaseUrl>,
///   port: Env<Port>,
///   api_key: Option<Env<ApiKey>>,
/// }
///
/// // The derive generates:
/// // impl tento_env::FromEnv for AppConfig {
/// //   fn from_env() -> EnvGroupResult<Self> { ... }
/// // }
/// // impl tento_env::PrintEnv for AppConfig {
/// //   fn print_env(&self) { ... }
/// //   fn fmt_env(&self) -> String { ... }
/// // }
///
/// let config = AppConfig::from_env().assert_envars();
/// config.print_env();
/// // Output:
/// // AppConfig:
/// //   DATABASE_URL: "postgres://localhost/mydb"
/// //   PORT: "8080"
/// //   API_KEY: <not set>
/// ```
///
/// # Field Types
///
/// - `Env<T>` - Required field. Uses `T::KEY` and `T::from_env()` to load.
/// - `Option<Env<T>>` - Optional field. Missing values result in `None`.
/// - `EnvGroup<T>` - Nested configuration struct implementing `FromEnv`.
///
/// For `Env<T>` fields, the inner type `T` must implement `FromEnvVar`.
/// For nested fields, the type must implement `FromEnv`.
#[proc_macro_derive(FromEnv, attributes(env))]
pub fn derive_env_group(input: TokenStream) -> TokenStream {
  expand_env_group(&parse_macro_input!(input as DeriveInput), &env_facade()).into()
}

fn expand_env_group(input: &DeriveInput, facade: &Ident) -> proc_macro2::TokenStream {
  if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
    return syn::Error::new_spanned(
      &input.generics,
      "FromEnv derive does not support generic structs",
    )
    .to_compile_error();
  }
  let name = &input.ident;

  // Optional struct-level `#[env(version = N)]` (per-group display version) and
  // `#[env(secrets_version = "x.y.z")]` (the slow-moving secret-surface semver
  // a manifest root carries). Both flow into `EnvGroupMeta`.
  let mut group_version: Option<u32> = None;
  let mut group_secrets_version: Option<String> = None;
  for attr in &input.attrs {
    if !attr.path().is_ident("env") {
      continue;
    }
    let res = attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("version") {
        group_version = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?);
        Ok(())
      } else if meta.path.is_ident("secrets_version") {
        group_secrets_version = Some(meta.value()?.parse::<LitStr>()?.value());
        Ok(())
      } else {
        Err(meta.error("expected `version` or `secrets_version`"))
      }
    });
    if let Err(e) = res {
      return e.to_compile_error();
    }
  }
  let group_version_tok = match group_version {
    Some(v) => quote!(::core::option::Option::Some(#v)),
    None => quote!(::core::option::Option::None),
  };
  let group_secrets_version_tok = match group_secrets_version {
    Some(s) => quote!(::core::option::Option::Some(#s.to_string())),
    None => quote!(::core::option::Option::None),
  };

  // Get the struct fields
  let fields = match &input.data {
    syn::Data::Struct(data) => match &data.fields {
      syn::Fields::Named(fields) => &fields.named,
      _ => {
        return syn::Error::new_spanned(input, "FromEnv only supports structs with named fields")
          .to_compile_error();
      }
    },
    _ => {
      return syn::Error::new_spanned(input, "FromEnv only supports structs").to_compile_error();
    }
  };

  // Keep the accumulator separate from locals named after caller fields.
  let builder = Ident::new("builder", proc_macro2::Span::mixed_site());

  // Parse each field
  let mut field_loads = Vec::new();
  let mut field_constructs = Vec::new();
  let mut field_prints = Vec::new();
  let mut field_prints_with_seen = Vec::new();
  let mut field_templates = Vec::new();
  let mut var_fallbacks = Vec::new();
  let mut nested_fallbacks = Vec::new();
  let mut field_env_vars = Vec::new();

  for (index, field) in fields.iter().enumerate() {
    let field_name = field.ident.as_ref().unwrap();
    let field_ty = &field.ty;
    // Defaults resolve names at the call site, outside generated field locals.
    let loaded = Ident::new(
      &format!("__tento_field_{index}"),
      proc_macro2::Span::mixed_site(),
    );

    let inline = match parse_inline_key(field) {
      Ok(inline) => inline,
      Err(error) => return error.to_compile_error(),
    };
    if let Some(InlineKey { key, default }) = inline {
      let has_fallback = default.is_some();
      let absent = match default {
        Some(expr) => quote!(::core::result::Result::Ok(#expr)),
        None => quote!(::core::result::Result::Err(::#facade::EnvError::missing(#key))),
      };
      let hint = value_type_hint(field_ty);
      let format_value = if is_std_type(field_ty, "path", "PathBuf") {
        quote!(|value: &#field_ty| ::std::format!("{}", value.display()))
      } else if is_std_type(field_ty, "string", "String") {
        quote!(|value: &#field_ty| ::std::format!("{}", value))
      } else {
        quote!(|value: &#field_ty| ::std::format!("{:?}", value))
      };
      field_loads.push(quote! {
        let #loaded = match ::#facade::resolve_key(#key) {
          ::core::result::Result::Ok(::core::option::Option::Some(raw)) => <#field_ty as ::#facade::ParseEnvValue>::parse_env_value(#key, &raw),
          ::core::result::Result::Ok(::core::option::Option::None) => #absent,
          ::core::result::Result::Err(error) => ::core::result::Result::Err(
            <#field_ty as ::#facade::ParseEnvValue>::__sanitize_lookup_error(#key, error),
          ),
        };
        let #loaded = match #loaded {
          ::core::result::Result::Ok(value) => {
            #builder.loaded_req(#key, (#format_value)(&value));
            ::core::option::Option::Some(value)
          }
          ::core::result::Result::Err(error) => {
            #builder.error(error);
            ::core::option::Option::None
          }
        };
      });
      field_constructs.push(quote!(#field_name: #loaded?,));
      var_fallbacks.push(quote!(#has_fallback));
      field_templates.push(quote! {
        meta.vars.push(::#facade::EnvVarMeta::required(
          #key, ::#facade::ValueTypeHint::#hint.as_str()
        ));
      });
      let print_value = quote! {
        let value = (#format_value)(&self.#field_name);
        let value = ::#facade::ValueTypeHint::#hint.colorize(&value);
        output.push_str(&::std::format!("{}{}: {}\n", indent_str, #key.green(), value));
      };
      field_prints.push(quote! {{ use ::#facade::Colorize; #print_value }});
      field_prints_with_seen.push(quote! {{
        use ::#facade::Colorize;
        if seen.insert(#key) {
          #print_value
        } else {
          output.push_str(&::std::format!("{}{}: {}\n", indent_str, #key.green(), "{ dupe }".dimmed()));
        }
      }});
      field_env_vars.push(quote! {
        vars.push((#key, __tento_export_env_value(#key, &self.#field_name)));
      });
      continue;
    }

    let Some(kind) = parse_field_kind(field_ty) else {
      return syn::Error::new_spanned(
        field_ty,
        "Use #[env(from = \"KEY\")] on a plain field, or use Env<T>, Option<Env<T>>, EnvResult<T>, EnvGroup<T>, Option<EnvGroup<T>>, or EnvGroupResult<T>",
      )
      .to_compile_error();
    };

    match &kind {
      FieldKind::EnvRequired { inner_type }
      | FieldKind::EnvOptional { inner_type }
      | FieldKind::EnvResultField { inner_type } => {
        var_fallbacks.push(quote! {
          <#inner_type as ::#facade::FromEnvVar>::HAS_ABSENT_FALLBACK
        });
      }
      FieldKind::Nested { inner_type }
      | FieldKind::NestedOptional { inner_type }
      | FieldKind::NestedResult { inner_type } => {
        nested_fallbacks.push(quote! {
          <#inner_type as ::#facade::EnvTemplate>::env_fallbacks()
        });
      }
    }

    match kind {
      FieldKind::EnvRequired { ref inner_type } => {
        // Generate: let field_name = builder.require(InnerType::KEY, InnerType::from_env().map(|e| e.into_value()));
        field_loads.push(quote! {
          let #loaded = #builder.require(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            <#inner_type as ::#facade::FromEnvVar>::from_env().map(|e| e.into_value()),
          );
        });

        // Generate: field_name: Env::new(InnerType::KEY, field_name.clone()?),
        field_constructs.push(quote! {
          #field_name: ::#facade::Env::new(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            #loaded.clone()?,
          ),
        });

        // Generate print for required field with type-based coloring
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            let value_str = ::std::format!("{}", self.#field_name.value());
            let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
            output.push_str(&::std::format!("{}{}: {}\n",
              indent_str,
              self.#field_name.key().green(),
              colored_value
            ));
          }
        });

        // Seen-aware print with deduplication by env var key
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let key = self.#field_name.key();
            if seen.insert(key) {
              let value_str = ::std::format!("{}", self.#field_name.value());
              let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
              output.push_str(&::std::format!("{}{}: {}\n",
                indent_str,
                key.green(),
                colored_value
              ));
            } else {
              output.push_str(&::std::format!("{}{}: {}\n",
                indent_str,
                key.green(),
                "{ dupe }".dimmed()
              ));
            }
          }
        });

        // Generate template metadata for required env var
        field_templates.push(quote! {
          meta.vars.push(::#facade::EnvVarMeta::required(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.as_str(),
          ).with_secret(<#inner_type as ::#facade::FromEnvVar>::SECRET));
        });

        // Generate env_vars extraction for required field
        // Use serde to get raw value (handles Redacted types which are #[serde(transparent)])
        field_env_vars.push(quote! {
          vars.push((
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            __tento_export_env_value(
              <#inner_type as ::#facade::FromEnvVar>::KEY, self.#field_name.value()
            ),
          ));
        });
      }
      FieldKind::EnvOptional { ref inner_type } => {
        // Generate: let field_name = builder.optional(InnerType::KEY, InnerType::from_env().map(|e| e.into_value()));
        field_loads.push(quote! {
          let #loaded = #builder.optional(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            <#inner_type as ::#facade::FromEnvVar>::from_env().map(|e| e.into_value()),
          );
        });

        // Generate: field_name: field_name.map(|v| Env::new(InnerType::KEY, v)),
        field_constructs.push(quote! {
          #field_name: #loaded.map(|v| ::#facade::Env::new(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            v,
          )),
        });

        // Generate print for optional field with type-based coloring
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            match &self.#field_name {
              ::core::option::Option::Some(env) => {
                let value_str = ::std::format!("{}", env.value());
                let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
                output.push_str(&::std::format!("{}{}: {}\n",
                  indent_str,
                  env.key().green(),
                  colored_value
                ));
              }
              ::core::option::Option::None => output.push_str(&::std::format!("{}{}: {}\n",
                indent_str,
                <#inner_type as ::#facade::FromEnvVar>::KEY.green(),
                "<not set>".dimmed()
              )),
            }
          }
        });

        // Seen-aware print with deduplication by env var key
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let key = <#inner_type as ::#facade::FromEnvVar>::KEY;
            if seen.insert(key) {
              match &self.#field_name {
                ::core::option::Option::Some(env) => {
                  let value_str = ::std::format!("{}", env.value());
                  let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
                  output.push_str(&::std::format!("{}{}: {}\n",
                    indent_str,
                    key.green(),
                    colored_value
                  ));
                }
                ::core::option::Option::None => output.push_str(&::std::format!("{}{}: {}\n",
                  indent_str,
                  key.green(),
                  "<not set>".dimmed()
                )),
              }
            } else {
              output.push_str(&::std::format!("{}{}: {}\n",
                indent_str,
                key.green(),
                "{ dupe }".dimmed()
              ));
            }
          }
        });

        // Generate template metadata for optional env var
        field_templates.push(quote! {
          meta.vars.push(::#facade::EnvVarMeta::optional(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.as_str(),
          ).with_secret(<#inner_type as ::#facade::FromEnvVar>::SECRET));
        });

        // Generate env_vars extraction for optional field
        // Use serde to get raw value (handles Redacted types which are #[serde(transparent)])
        field_env_vars.push(quote! {
          if let ::core::option::Option::Some(env) = &self.#field_name {
            vars.push((
              <#inner_type as ::#facade::FromEnvVar>::KEY,
              __tento_export_env_value(
                <#inner_type as ::#facade::FromEnvVar>::KEY, env.value()
              ),
            ));
          }
        });
      }
      FieldKind::EnvResultField { ref inner_type } => {
        // Generate: let field_name: EnvResult<InnerType> = InnerType::from_env().into();
        // Then record it in the builder
        field_loads.push(quote! {
          let #loaded: ::#facade::EnvResult<#inner_type> =
            <#inner_type as ::#facade::FromEnvVar>::from_env().into();
          #builder.optional_result(&#loaded);
        });

        // Generate: field_name: field_name, (it's already an EnvResult)
        field_constructs.push(quote! {
          #field_name: #loaded,
        });

        // Generate print for EnvResult field with type-based coloring
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            match &self.#field_name {
              ::#facade::EnvResult::Present(env) => {
                let value_str = ::std::format!("{}", env.value());
                let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
                output.push_str(&::std::format!("{}{}: {}\n",
                  indent_str,
                  env.key().green(),
                  colored_value
                ));
              }
              ::#facade::EnvResult::Missing { key } => {
                output.push_str(&::std::format!("{}{}: {}\n",
                  indent_str,
                  key.green(),
                  "<not set>".dimmed()
                ));
              }
              ::#facade::EnvResult::Malformed(err) => {
                output.push_str(&::std::format!("{}{}\n",
                  indent_str,
                  err.fmt_colored()
                ));
              }
            }
          }
        });

        // Seen-aware print with deduplication by env var key
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let key = <#inner_type as ::#facade::FromEnvVar>::KEY;
            if seen.insert(key) {
              match &self.#field_name {
                ::#facade::EnvResult::Present(env) => {
                  let value_str = ::std::format!("{}", env.value());
                  let colored_value = <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.colorize(&value_str);
                  output.push_str(&::std::format!("{}{}: {}\n",
                    indent_str,
                    key.green(),
                    colored_value
                  ));
                }
                ::#facade::EnvResult::Missing { .. } => {
                  output.push_str(&::std::format!("{}{}: {}\n",
                    indent_str,
                    key.green(),
                    "<not set>".dimmed()
                  ));
                }
                ::#facade::EnvResult::Malformed(err) => {
                  output.push_str(&::std::format!("{}{}\n",
                    indent_str,
                    err.fmt_colored()
                  ));
                }
              }
            } else {
              output.push_str(&::std::format!("{}{}: {}\n",
                indent_str,
                key.green(),
                "{ dupe }".dimmed()
              ));
            }
          }
        });

        // Generate template metadata for EnvResult field (treated as optional)
        field_templates.push(quote! {
          meta.vars.push(::#facade::EnvVarMeta::optional(
            <#inner_type as ::#facade::FromEnvVar>::KEY,
            <#inner_type as ::#facade::FromEnvVar>::VALUE_TYPE.as_str(),
          ).with_secret(<#inner_type as ::#facade::FromEnvVar>::SECRET));
        });

        // Generate env_vars extraction for EnvResult field
        // Use serde to get raw value (handles Redacted types which are #[serde(transparent)])
        field_env_vars.push(quote! {
          if let ::#facade::EnvResult::Present(env) = &self.#field_name {
            vars.push((
              <#inner_type as ::#facade::FromEnvVar>::KEY,
              __tento_export_env_value(
                <#inner_type as ::#facade::FromEnvVar>::KEY, env.value()
              ),
            ));
          }
        });
      }
      FieldKind::Nested { ref inner_type } => {
        // Generate: let field_name = <Type as FromEnv>::from_env();
        // Then collect errors from the nested result
        field_loads.push(quote! {
          let #loaded = {
            let nested_result = <#inner_type as ::#facade::FromEnv>::from_env();
            match nested_result.into_result() {
              ::core::result::Result::Ok(value) => ::core::option::Option::Some(::#facade::EnvGroup::new(value)),
              ::core::result::Result::Err(nested_error) => {
                // Merge nested errors into our builder
                for loaded in nested_error.loaded() {
                  if loaded.required {
                    #builder.loaded_req(loaded.key, &loaded.display_value);
                  } else {
                    #builder.loaded_opt(loaded.key, &loaded.display_value);
                  }
                }
                for error in nested_error.errors() {
                  #builder.error(error.clone());
                }
                ::core::option::Option::None
              }
            }
          };
        });

        // Generate: field_name: field_name?,
        field_constructs.push(quote! {
          #field_name: #loaded?,
        });

        // Generate print for nested config with field name and type name
        let field_name_str = field_name.to_string();
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
            output.push_str(&::#facade::PrintEnv::fmt_env_indented(&*self.#field_name, indent + 2));
          }
        });

        // Generate seen-aware print for nested config with deduplication
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            if seen.insert(type_name) {
              output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
              output.push_str(&::#facade::PrintEnv::fmt_env_with_seen(&*self.#field_name, indent + 2, seen));
            } else {
              output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "{ dupe }".dimmed()));
            }
          }
        });

        // Generate template metadata for nested config (required)
        field_templates.push(quote! {
          let mut nested = <#inner_type as ::#facade::EnvTemplate>::env_template();
          nested.name = #field_name_str;
          nested.required = true;
          meta.nested.push(nested);
        });

        // Generate env_vars extraction for nested config
        field_env_vars.push(quote! {
          vars.extend(::#facade::EnvVars::env_vars(&*self.#field_name));
        });
      }
      FieldKind::NestedOptional { ref inner_type } => {
        // Optional nested config - if any env var in the group is set, try to load all of them
        // If no env vars are set, return None
        field_loads.push(quote! {
          let #loaded = {
            let (nested_result, presence) = ::#facade::__capture_env_presence(
              || <#inner_type as ::#facade::FromEnv>::from_env()
            );
            match nested_result.into_result() {
              ::core::result::Result::Ok(value) => {
                if presence == ::core::option::Option::Some(false) {
                  ::core::option::Option::None
                } else {
                  ::core::option::Option::Some(::#facade::EnvGroup::new(value))
                }
              }
              ::core::result::Result::Err(nested_error) => {
                let all_missing = nested_error.errors().iter().all(|e| e.is_missing());
                if presence == ::core::option::Option::Some(false) && all_missing {
                  ::core::option::Option::None
                } else {
                  // Active or unknown: retain ALL context and ALL errors.
                  // A Parse error overrides even observed absence.
                  for loaded in nested_error.loaded() {
                    if loaded.required {
                      #builder.loaded_req(loaded.key, &loaded.display_value);
                    } else {
                      #builder.loaded_opt(loaded.key, &loaded.display_value);
                    }
                  }
                  for error in nested_error.errors() {
                    #builder.error(error.clone());
                  }
                  ::core::option::Option::None
                }
              }
            }
          };
        });

        // Generate: field_name: field_name, (no `?` - it's already Option)
        field_constructs.push(quote! {
          #field_name: #loaded,
        });

        // Generate print for optional nested config with field name and type name
        let field_name_str = field_name.to_string();
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            match &self.#field_name {
              ::core::option::Option::Some(group) => {
                output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
                output.push_str(&::#facade::PrintEnv::fmt_env_indented(&**group, indent + 2));
              }
              ::core::option::Option::None => output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "<not set>".dimmed())),
            }
          }
        });

        // Generate seen-aware print for optional nested config with deduplication
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            match &self.#field_name {
              ::core::option::Option::Some(group) => {
                if seen.insert(type_name) {
                  output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
                  output.push_str(&::#facade::PrintEnv::fmt_env_with_seen(&**group, indent + 2, seen));
                } else {
                  output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "{ dupe }".dimmed()));
                }
              }
              ::core::option::Option::None => output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "<not set>".dimmed())),
            }
          }
        });

        // Generate template metadata for optional nested config
        field_templates.push(quote! {
          let mut nested = <#inner_type as ::#facade::EnvTemplate>::env_template();
          nested.name = #field_name_str;
          nested.required = false;
          meta.nested.push(nested);
        });

        // Generate env_vars extraction for optional nested config
        field_env_vars.push(quote! {
          if let ::core::option::Option::Some(group) = &self.#field_name {
            vars.extend(::#facade::EnvVars::env_vars(&**group));
          }
        });
      }
      FieldKind::NestedResult { ref inner_type } => {
        // Generate: let field_name = <Type as FromEnv>::from_env();
        // Store the result directly - caller will handle errors via assert_envars() or into_result()
        field_loads.push(quote! {
          let #loaded = <#inner_type as ::#facade::FromEnv>::from_env();
        });

        // Generate: field_name: field_name, (it's already EnvGroupResult)
        field_constructs.push(quote! {
          #field_name: #loaded,
        });

        // Generate print for EnvGroupResult field with type name
        let field_name_str = field_name.to_string();
        field_prints.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            match &self.#field_name.0 {
              ::core::result::Result::Ok(value) => {
                output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
                output.push_str(&::#facade::PrintEnv::fmt_env_indented(value, indent + 2));
              }
              ::core::result::Result::Err(err) => {
                output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "<error>".red()));
                output.push_str(&err.fmt_indented(indent + 2));
              }
            }
          }
        });

        // Generate seen-aware print for EnvGroupResult field with deduplication
        field_prints_with_seen.push(quote! {
          {
            use ::#facade::Colorize;
            let type_name = <#inner_type as ::#facade::PrintEnv>::type_name();
            match &self.#field_name.0 {
              ::core::result::Result::Ok(value) => {
                if seen.insert(type_name) {
                  output.push_str(&::std::format!("{}{}: {}\n", indent_str, #field_name_str, type_name.bright_blue()));
                  output.push_str(&::#facade::PrintEnv::fmt_env_with_seen(value, indent + 2, seen));
                } else {
                  output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "{ dupe }".dimmed()));
                }
              }
              ::core::result::Result::Err(err) => {
                output.push_str(&::std::format!("{}{}: {} {}\n", indent_str, #field_name_str, type_name.bright_blue(), "<error>".red()));
                output.push_str(&err.fmt_indented(indent + 2));
              }
            }
          }
        });

        // Generate template metadata for NestedResult (treated as optional since it's deferred)
        field_templates.push(quote! {
          let mut nested = <#inner_type as ::#facade::EnvTemplate>::env_template();
          nested.name = #field_name_str;
          nested.required = false;
          meta.nested.push(nested);
        });

        // Generate env_vars extraction for NestedResult
        field_env_vars.push(quote! {
          if let ::core::result::Result::Ok(value) = &self.#field_name.0 {
            vars.extend(::#facade::EnvVars::env_vars(value));
          }
        });
      }
    }
  }

  let name_str = name.to_string();

  quote! {
    impl ::#facade::FromEnv for #name {
      /// Load configuration from environment variables.
      ///
      /// Returns an `EnvGroupResult` that can be converted to a `Result` for use with `?`.
      /// On error, contains both successfully loaded values and all errors encountered.
      fn from_env() -> ::#facade::EnvGroupResult<Self> {
        let mut #builder = ::#facade::EnvGroupBuilder::new();

        #(#field_loads)*

        #builder.finish(|| {
          ::core::option::Option::Some(#name {
            #(#field_constructs)*
          })
        })
      }
    }

    impl ::#facade::PrintEnv for #name {
      fn print_env(&self) {
        ::std::eprintln!("{}", self.fmt_env());
      }

      fn fmt_env(&self) -> ::std::string::String {
        let mut output = ::std::string::String::new();
        let mut seen = ::std::collections::HashSet::new();
        seen.insert(#name_str);
        output.push_str(&::std::format!("{}:\n", #name_str));
        output.push_str(&self.fmt_env_with_seen(2, &mut seen));
        output
      }

      fn fmt_env_indented(&self, indent: ::core::primitive::usize) -> ::std::string::String {
        let indent_str = " ".repeat(indent);
        let mut output = ::std::string::String::new();
        #(#field_prints)*
        output
      }

      fn fmt_env_with_seen(&self, indent: ::core::primitive::usize, seen: &mut ::std::collections::HashSet<&'static ::core::primitive::str>) -> ::std::string::String {
        let indent_str = " ".repeat(indent);
        let mut output = ::std::string::String::new();
        #(#field_prints_with_seen)*
        output
      }

      fn type_name() -> &'static ::core::primitive::str {
        ::std::any::type_name::<Self>()
      }
    }

    impl ::#facade::EnvTemplate for #name {
      fn env_template() -> ::#facade::EnvGroupMeta {
        let mut meta = ::#facade::EnvGroupMeta::required(#name_str);
        meta.version = #group_version_tok;
        meta.secrets_version = #group_secrets_version_tok;
        #(#field_templates)*
        meta
      }

      fn env_fallbacks() -> ::#facade::EnvFallbacks {
        ::#facade::EnvFallbacks {
          vars: ::std::vec![#(#var_fallbacks),*],
          nested: ::std::vec![#(#nested_fallbacks),*],
        }
      }
    }

    impl ::#facade::EnvVars for #name {
      fn env_vars(&self) -> ::std::vec::Vec<(&'static ::core::primitive::str, ::std::string::String)> {
        // Preserve the original text serializer's nonstring domain and order.
        // Only a top-level JSON string is decoded; its payload is never trimmed.
        fn __tento_export_env_value<T: ::#facade::serde::Serialize + ?::core::marker::Sized>(
          key: &'static ::core::primitive::str, value: &T
        ) -> ::std::string::String {
          let text = match ::#facade::serde_json::to_string(value) {
            ::core::result::Result::Ok(text) => text,
            ::core::result::Result::Err(_) => ::std::panic!("failed to serialize environment value for {}", key),
          };
          let token = text.bytes().find(|byte| !::std::matches!(*byte, b' ' | b'\n' | b'\r' | b'\t'));
          if token == ::core::option::Option::Some(b'"') {
            match ::#facade::serde_json::from_str::<::std::string::String>(&text) {
              ::core::result::Result::Ok(raw) => raw,
              ::core::result::Result::Err(_) => ::std::panic!("failed to serialize environment value for {}", key),
            }
          } else {
            text
          }
        }
        let mut vars = ::std::vec::Vec::new();
        #(#field_env_vars)*
        vars
      }
    }
  }
}

#[cfg(test)]
mod expansion_qualification;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn inline_keys_reject_invalid_declarations() {
    for (field, expected) in [
      (r#"#[env(default = 8080)] port: u16"#, "expected `from"),
      (
        r#"#[env(from = "BAD=KEY")] port: u16"#,
        "environment key must be nonempty",
      ),
      (
        r#"#[env(from = "PORT", from = "PORT2")] port: u16"#,
        "duplicate `from` option",
      ),
      (
        r#"#[env(from = "PORT", default = 1, default = 2)] port: u16"#,
        "duplicate `default` option",
      ),
      (
        r#"#[env(from = "PORT")] port: Env<Port>"#,
        "use a plain field type",
      ),
    ] {
      let field = syn::parse::Parser::parse_str(syn::Field::parse_named, field).unwrap();
      let Err(error) = parse_inline_key(&field) else {
        panic!("expected a field declaration error");
      };
      assert!(error.to_string().contains(expected), "{error}");
    }
  }

  #[test]
  fn dependency_names_are_path_identifiers() {
    for (name, spelling) in [
      ("tento_env", "tento_env"),
      ("env_alias", "env_alias"),
      ("union", "union"),
      ("gen", "r#gen"),
      ("type", "r#type"),
      ("async", "r#async"),
    ] {
      assert_eq!(dependency_ident(name).unwrap().to_string(), spelling);
    }
    for name in ["_", "crate", "self", "Self", "super", "1env", "env-alias"] {
      assert!(dependency_ident(name).is_none(), "{name}");
    }
  }

  #[test]
  fn standard_presentation_type_spellings() {
    for (module, name) in [("path", "PathBuf"), ("string", "String")] {
      for spelling in [
        name.to_owned(),
        format!("std::{module}::{name}"),
        format!("::std::{module}::{name}"),
      ] {
        let ty: Type = syn::parse_str(&spelling).unwrap();
        assert!(is_std_type(&ty, module, name), "{spelling}");
      }
      for spelling in [
        "Alias".to_owned(),
        format!("custom::{name}"),
        format!("::custom::{name}"),
        format!("::{name}"),
        format!("other::std::{module}::{name}"),
        format!("std::{module}::{name}<u8>"),
        format!("std::<u8>::{module}::{name}"),
        format!("<Custom as Trait>::{name}"),
        format!("&std::{module}::{name}"),
      ] {
        let ty: Type = syn::parse_str(&spelling).unwrap();
        assert!(!is_std_type(&ty, module, name), "{spelling}");
      }
    }
  }

  #[test]
  fn test_screaming_snake_to_pascal() {
    assert_eq!(screaming_snake_to_pascal("DATABASE_URL"), "DatabaseUrl");
    assert_eq!(screaming_snake_to_pascal("DEMO_JS"), "DemoJs");
    assert_eq!(screaming_snake_to_pascal("PORT"), "Port");
    assert_eq!(screaming_snake_to_pascal("API_KEY"), "ApiKey");
    assert_eq!(screaming_snake_to_pascal("MY_APP_CONFIG"), "MyAppConfig");
  }
}
