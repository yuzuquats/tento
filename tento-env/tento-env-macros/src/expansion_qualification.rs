use super::{EnvKvInput, expand_env_group, expand_env_kv};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use std::fmt::Write;
use std::hint::black_box;
use std::time::{Duration, Instant};
use syn::{Data, DeriveInput, Expr, Fields, Ident, ImplItem, Item, Member, Pat, Stmt, Type};

const SAMPLES: usize = 7;
const SIZES: [usize; 4] = [10, 100, 400, 1_000];
const SIMPLE_1K_BUDGET: Duration = Duration::from_millis(100);
const GROWTH_BUDGET: f64 = 6.0;

fn key_inputs(count: usize, mixed: bool) -> Vec<EnvKvInput> {
  (0..count)
    .map(|index| {
      let input = match (mixed, index % 5) {
        (true, 1) => format!(r#""SERVICE_ENABLED_{index}", bool"#),
        (true, 2) => format!(r#""SERVICE_PORT_{index}", u16, default = 8080"#),
        (true, 3) => format!(r#""SERVICE_DATA_{index}", ::std::path::PathBuf"#),
        (true, 4) => format!(r#""SERVICE_TOKEN_{index}", String, secret"#),
        _ => format!(r#""SERVICE_URL_{index}", String"#),
      };
      syn::parse_str(&input).unwrap()
    })
    .collect()
}

fn group_input(name: &str, fields: impl IntoIterator<Item = (usize, String)>) -> DeriveInput {
  let mut input = format!("#[env(version = 1)] struct {name} {{");
  for (index, ty) in fields {
    write!(input, "field_{index}: {ty},").unwrap();
  }
  input.push('}');
  syn::parse_str(&input).unwrap()
}

fn leaf_fields(range: std::ops::Range<usize>) -> impl Iterator<Item = (usize, String)> {
  range.map(|index| {
    let ty = match index % 5 {
      1 => format!("Option<Env<ServiceEnabled{index}>>"),
      2 => format!("Env<ServicePort{index}>"),
      3 => format!("Option<Env<ServiceData{index}>>"),
      4 => format!("Env<ServiceToken{index}>"),
      _ => format!("Env<ServiceUrl{index}>"),
    };
    (index, ty)
  })
}

fn nested_groups(leaves: usize) -> Vec<DeriveInput> {
  // Five settings per section, with alternating required and optional sections.
  let sections = leaves / 5;
  let mut groups: Vec<_> = (0..sections)
    .map(|index| {
      group_input(
        &format!("Section{index}"),
        leaf_fields(index * 5..(index + 1) * 5),
      )
    })
    .collect();
  groups.push(group_input(
    "Configuration",
    (0..sections).map(|index| {
      let ty = if index % 2 == 0 {
        format!("EnvGroup<Section{index}>")
      } else {
        format!("Option<EnvGroup<Section{index}>>")
      };
      (index, ty)
    }),
  ));
  groups
}

fn count_tokens(tokens: TokenStream) -> usize {
  tokens
    .into_iter()
    .map(|token| match token {
      TokenTree::Group(group) => 1 + count_tokens(group.stream()),
      _ => 1,
    })
    .sum()
}

fn check_impls(output: &syn::File, name: &Ident, expected: &[&str]) {
  for expected_trait in expected {
    assert!(
      output.items.iter().any(|item| {
        let Item::Impl(item) = item else {
          return false;
        };
        let Type::Path(self_ty) = item.self_ty.as_ref() else {
          return false;
        };
        self_ty.path.is_ident(name)
          && item
            .trait_
            .as_ref()
            .is_some_and(|(_, path, _)| path.segments.last().unwrap().ident == *expected_trait)
      }),
      "missing {expected_trait} impl for {name}"
    );
  }
}

fn check_key(input: &EnvKvInput, output: &syn::File) {
  let name = &input.struct_name;
  assert!(
    output
      .items
      .iter()
      .any(|item| matches!(item, Item::Struct(item) if item.ident == *name))
  );
  check_impls(output, name, &["FromEnvVar", "EnvTemplate", "PrintEnv"]);
  if input.default.is_some() {
    check_impls(output, name, &["Default"]);
  }
}

fn check_group(input: &DeriveInput, output: &syn::File) {
  check_impls(
    output,
    &input.ident,
    &["FromEnv", "EnvTemplate", "EnvVars", "PrintEnv"],
  );
  let Data::Struct(data) = &input.data else {
    panic!("expected a group struct");
  };
  let Fields::Named(fields) = &data.fields else {
    panic!("expected named group fields");
  };
  let load = output
    .items
    .iter()
    .find_map(|item| {
      let Item::Impl(item) = item else {
        return None;
      };
      item.items.iter().find_map(|item| match item {
        ImplItem::Fn(method) if method.sig.ident == "from_env" => Some(&method.block),
        _ => None,
      })
    })
    .expect("expected a generated from_env method");
  let mut loaded: Vec<_> = load
    .stmts
    .iter()
    .filter_map(|stmt| match stmt {
      Stmt::Local(local) => match &local.pat {
        Pat::Ident(binding) if binding.ident.to_string().starts_with("__tento_field_") => {
          Some(&binding.ident)
        }
        _ => None,
      },
      _ => None,
    })
    .collect();
  loaded.dedup(); // An inline load shadows its temporary with the parsed value.

  let Some(Stmt::Expr(Expr::MethodCall(finish), None)) = load.stmts.last() else {
    panic!("expected the generated builder call");
  };
  assert_eq!(finish.method, "finish");
  let Some(Expr::Closure(construct)) = finish.args.first() else {
    panic!("expected the generated construction closure");
  };
  let Expr::Block(block) = construct.body.as_ref() else {
    panic!("expected a construction block");
  };
  let Some(Stmt::Expr(Expr::Call(wrap), None)) = block.block.stmts.last() else {
    panic!("expected the generated optional result");
  };
  let Some(Expr::Struct(result)) = wrap.args.first() else {
    panic!("expected a generated struct literal");
  };
  assert_eq!(loaded.len(), fields.named.len());
  assert_eq!(result.fields.len(), fields.named.len());
  for (index, ((field, generated), loaded)) in
    fields.named.iter().zip(&result.fields).zip(loaded).enumerate()
  {
    assert_eq!(generated.member, Member::Named(field.ident.clone().unwrap()));
    assert_eq!(*loaded, format!("__tento_field_{index}"));
    assert!(
      uses_binding(generated.expr.to_token_stream(), loaded),
      "field {} must use its own load in {}",
      generated.member.to_token_stream(),
      input.ident
    );
  }
}

fn uses_binding(tokens: TokenStream, binding: &Ident) -> bool {
  tokens.into_iter().any(|token| match token {
    TokenTree::Ident(ident) => ident == *binding,
    TokenTree::Group(group) => uses_binding(group.stream(), binding),
    _ => false,
  })
}

#[derive(Clone, Copy)]
struct Measurement {
  median: Duration,
  tokens: usize,
}

fn measure<I>(
  workload: &str,
  leaves: usize,
  inputs: &[I],
  expand: impl Fn(&I) -> TokenStream,
  check: fn(&I, &syn::File),
) -> Measurement {
  let mut output = Vec::with_capacity(inputs.len());
  let mut samples = [Duration::ZERO; SAMPLES];
  let mut tokens = 0;
  for sample in std::iter::once(None).chain(samples.iter_mut().map(Some)) {
    let start = Instant::now();
    output.extend(
      inputs
        .iter()
        .map(|input| black_box(expand(black_box(input)))),
    );
    let elapsed = start.elapsed();
    if let Some(sample) = sample {
      *sample = elapsed;
    } else {
      for (input, generated) in inputs.iter().zip(&output) {
        let parsed = syn::parse2(generated.clone()).expect("generated Rust items must parse");
        check(input, &parsed);
        tokens += count_tokens(generated.clone());
      }
    }
    black_box(&output);
    output.clear();
  }
  assert!(tokens > leaves);
  samples.sort_unstable();
  let median = samples[SAMPLES / 2];
  println!(
    "ENV_EXPANSION workload={workload} leaves={leaves} expansions={} tokens={tokens} samples={SAMPLES} min_ns={} median_ns={} max_ns={}",
    inputs.len(),
    samples[0].as_nanos(),
    median.as_nanos(),
    samples[SAMPLES - 1].as_nanos(),
  );
  Measurement { median, tokens }
}

#[test]
#[ignore = "release expansion timing qualification"]
fn env_expansion_qualification() {
  // Supply the resolved path outside the timer. The measured work starts at
  // expansion, after input parsing and Cargo manifest lookup.
  let facade = if std::env::var_os("TENTO_ENV_QUALIFICATION_ALIAS").is_some() {
    quote::format_ident!("env_alias")
  } else {
    quote::format_ident!("tento_env")
  };
  let results = SIZES.map(|leaves| {
    [
      measure(
        "simple_keys",
        leaves,
        &key_inputs(leaves, false),
        |input| expand_env_kv(input, &facade),
        check_key,
      ),
      measure(
        "mixed_keys",
        leaves,
        &key_inputs(leaves, true),
        |input| expand_env_kv(input, &facade),
        check_key,
      ),
      measure(
        "flat_group",
        leaves,
        &[group_input("FlatConfig", leaf_fields(0..leaves))],
        |input| expand_env_group(input, &facade),
        check_group,
      ),
      measure(
        "nested_groups",
        leaves,
        &nested_groups(leaves),
        |input| expand_env_group(input, &facade),
        check_group,
      ),
    ]
  });

  let [_, at_100, at_400, at_1k] = results;
  for ((small, large), name) in
    at_100
      .into_iter()
      .zip(at_400)
      .zip(["simple_keys", "mixed_keys", "flat_group", "nested_groups"])
  {
    let time_growth = large.median.as_secs_f64() / small.median.as_secs_f64();
    let token_growth = large.tokens as f64 / small.tokens as f64;
    println!("ENV_EXPANSION_GROWTH workload={name} time={time_growth:.4} tokens={token_growth:.4}");
    assert!(
      time_growth <= GROWTH_BUDGET,
      "{name}: time growth {time_growth:.4} exceeds {GROWTH_BUDGET}"
    );
    assert!(
      token_growth <= GROWTH_BUDGET,
      "{name}: token growth {token_growth:.4} exceeds {GROWTH_BUDGET}"
    );
  }
  assert!(
    at_1k[0].median <= SIMPLE_1K_BUDGET,
    "simple_keys: 1,000-input median {:?} exceeds {SIMPLE_1K_BUDGET:?}",
    at_1k[0].median,
  );
}
