use std::{
  borrow::Cow,
  path::{Path, PathBuf},
};

use arcstr::ArcStr;
use json_escape_simd::escape;
use oxc::{
  allocator::Allocator,
  ast::AstBuilder,
  semantic::Scoping,
  span::{SPAN, SourceType as OxcSourceType},
};
use oxc_str::CompactStr;
use rolldown_common::{
  ConstExportMeta, ModuleDefFormat, ModuleType, NormalizedBundlerOptions, RUNTIME_MODULE_KEY,
  StrOrBytes, json_value_to_ecma_ast,
};
use rolldown_ecmascript::{EcmaAst, EcmaCompiler};
use rolldown_error::{BuildDiagnostic, BuildResult};
use rolldown_plugin::HookTransformAstArgs;
use rolldown_utils::mime::guess_mime;
use rustc_hash::FxHashMap;
use serde_json::Value;
use sugar_path::SugarPath;

use super::{
  dx_machine_cache::{
    DxMachineCacheConfig, DxMachineCacheHit, DxMachineCachePaths, DxMachineCacheStatus,
  },
  pre_process_ecma_ast::PreProcessEcmaAst,
};

use crate::types::{module_factory::CreateModuleContext, oxc_parse_type::OxcParseType};

const CORE_JSON_MACHINE_MAGIC: &[u8; 8] = b"RDXCJSN3";
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const CORE_JSON_DX_SERIALIZER_MACHINE_MAGIC: &[u8; 8] = b"RDXCJSRK";
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC: &[u8; 8] = b"RDXCJSR2";
const CORE_JSON_MACHINE_HEADER_LEN: usize = 8 + 32 + 32;
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const CORE_JSON_DX_SERIALIZER_ALIGNED_HEADER_LEN: usize = CORE_JSON_MACHINE_HEADER_LEN + 8;
const CORE_JSON_MACHINE_SOURCE_HASH_RANGE: std::ops::Range<usize> = 8..40;
#[cfg(test)]
const CORE_JSON_MACHINE_BODY_HASH_RANGE: std::ops::Range<usize> = 40..72;
const CORE_JSON_MACHINE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const CORE_JSON_MACHINE_MAX_STRING_LEN: usize = 256 * 1024 * 1024;
const CORE_JSON_MACHINE_MAX_NUMBER_LEN: usize = 128;
const CORE_JSON_MACHINE_MAX_DEPTH: u16 = 512;
const CORE_JSON_MACHINE_MIN_SOURCE_BYTES: usize = 128 * 1024;
const CORE_JSON_MACHINE_NAMESPACE: &str = "core_json_module_v2";
const CORE_JSON_TAG_NULL: u8 = 0;
const CORE_JSON_TAG_FALSE: u8 = 1;
const CORE_JSON_TAG_TRUE: u8 = 2;
const CORE_JSON_TAG_NUMBER: u8 = 3;
const CORE_JSON_TAG_STRING: u8 = 4;
const CORE_JSON_TAG_ARRAY: u8 = 5;
const CORE_JSON_TAG_OBJECT: u8 = 6;

#[cfg(test)]
thread_local! {
  static CORE_JSON_TEST_CACHE_TARGET_BUILDS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_SOURCE_HASH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_SOURCE_TEXT_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_JSON_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_BODY_CAPACITY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_AST_VEC_CAPACITY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_PAYLOAD_BODY_VALIDATION_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_BODY_HASH_VALIDATION_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_MACHINE_MAGIC_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_LEGACY_MAGIC_CHECK_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static CORE_JSON_TEST_ALIGNED_MAGIC_CHECK_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_AST_NUMBER_JSON_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_AST_OBJECT_KEY_DIRECT_ALLOC_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CORE_JSON_TEST_SOURCE_WORTH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static CORE_JSON_TEST_DX_SERIALIZER_BODY_VEC_DECODE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static CORE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static CORE_JSON_TEST_DX_SERIALIZER_BODY_ENCODE_CLONE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

struct CoreJsonMachineCacheTarget {
  config: DxMachineCacheConfig,
  paths: DxMachineCachePaths,
  source_path: PathBuf,
}

#[derive(Clone, Copy)]
enum CoreJsonMachineMagic {
  Legacy,
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  DxSerializer,
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  DxSerializerAligned,
}

impl CoreJsonMachineMagic {
  fn header_len(self) -> usize {
    match self {
      Self::Legacy => CORE_JSON_MACHINE_HEADER_LEN,
      #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
      Self::DxSerializer => CORE_JSON_MACHINE_HEADER_LEN,
      #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
      Self::DxSerializerAligned => CORE_JSON_DX_SERIALIZER_ALIGNED_HEADER_LEN,
    }
  }

  fn has_valid_padding(self, _machine_bytes: &[u8]) -> bool {
    match self {
      #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
      Self::DxSerializerAligned => _machine_bytes
        .get(CORE_JSON_MACHINE_HEADER_LEN..CORE_JSON_DX_SERIALIZER_ALIGNED_HEADER_LEN)
        .is_some_and(|padding| padding.iter().all(|byte| *byte == 0)),
      Self::Legacy => true,
      #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
      Self::DxSerializer => true,
    }
  }
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(compare(PartialEq))]
struct CoreJsonDxSerializerPayload {
  body: Vec<u8>,
}

enum CoreJsonMachineAstRead {
  Hit(EcmaAst),
  Repair { source_hash: blake3::Hash },
  Miss,
}

#[inline]
fn pure_esm_js_oxc_source_type(module_def_format: ModuleDefFormat) -> OxcSourceType {
  let default_source_type = OxcSourceType::default();
  debug_assert!(default_source_type.is_javascript());
  debug_assert!(!default_source_type.is_jsx());
  match module_def_format {
    ModuleDefFormat::Cjs | ModuleDefFormat::Cts => default_source_type.with_commonjs(true),
    ModuleDefFormat::EsmMjs | ModuleDefFormat::EsmMts | ModuleDefFormat::EsmPackageJson => {
      default_source_type.with_module(true)
    }
    ModuleDefFormat::CjsPackageJson | ModuleDefFormat::Unknown => {
      // treat unknown format as ESM for now: https://github.com/rolldown/rolldown/issues/7009
      default_source_type.with_module(true)
    }
  }
}

pub struct ParseToEcmaAstResult {
  pub ast: EcmaAst,
  pub scoping: Scoping,
  pub has_lazy_export: bool,
  pub warnings: Vec<BuildDiagnostic>,
  /// Whether JSX syntax should be preserved in the output, determined per-module
  /// during transformation based on the resolved tsconfig.
  pub preserve_jsx: bool,
  /// Enum member constant values, keyed by enum name → member name → value.
  /// Used by the finalizer to inline cross-module enum member accesses (e.g., `Direction.Up` → `0`).
  pub enum_member_value_map: FxHashMap<CompactStr, FxHashMap<CompactStr, ConstExportMeta>>,
}

pub async fn parse_to_ecma_ast(
  ctx: &CreateModuleContext<'_>,
  source: StrOrBytes,
) -> BuildResult<ParseToEcmaAstResult> {
  let CreateModuleContext {
    options,
    stable_id,
    resolved_id,
    module_type,
    plugin_driver,
    replace_global_define_config,
    ..
  } = ctx;

  let path = resolved_id.id.as_path();
  let is_user_defined_entry = ctx.is_user_defined_entry;

  let (has_lazy_export, source, parsed_type, parsed_json_ast) =
    if matches!(module_type, ModuleType::Json) {
      let ast = parse_json_to_ecma_ast_with_machine_cache(
        &options.cwd,
        path,
        resolved_id.id.as_str(),
        &source,
      )?;
      (true, Cow::Borrowed(""), module_type.into(), Some(ast))
    } else {
      let (has_lazy_export, source, parsed_type) =
        pre_process_source(path, source, module_type, options)?;
      (has_lazy_export, source, parsed_type, None)
    };

  let oxc_source_type = {
    let default = pure_esm_js_oxc_source_type(resolved_id.module_def_format);
    match parsed_type {
      OxcParseType::Js => default,
      OxcParseType::Jsx => default.with_jsx(!options.transform_options.is_jsx_disabled()),
      OxcParseType::Ts => default.with_typescript(true),
      OxcParseType::Tsx => {
        default.with_typescript(true).with_jsx(!options.transform_options.is_jsx_disabled())
      }
    }
  };

  let mut ecma_ast = match module_type {
    ModuleType::Json => {
      parsed_json_ast.expect("JSON modules are parsed before source preprocessing")
    }
    ModuleType::Dataurl | ModuleType::Base64 | ModuleType::Text => {
      EcmaCompiler::parse_expr_as_program(resolved_id.id.as_str(), source, oxc_source_type)?
    }
    _ => EcmaCompiler::parse(resolved_id.id.as_str(), source, oxc_source_type)?,
  };

  ecma_ast = plugin_driver
    .transform_ast(HookTransformAstArgs {
      cwd: &options.cwd,
      ast: ecma_ast,
      id: resolved_id.id.as_str(),
      stable_id,
      is_user_defined_entry,
      module_type,
    })
    .await?;

  PreProcessEcmaAst::default().build(
    ecma_ast,
    stable_id,
    resolved_id.id.as_str(),
    &parsed_type,
    replace_global_define_config.as_ref(),
    options,
    has_lazy_export,
  )
}

fn pre_process_source(
  path: &Path,
  source: StrOrBytes,
  module_type: &ModuleType,
  options: &NormalizedBundlerOptions,
) -> BuildResult<(bool, Cow<'static, str>, OxcParseType)> {
  let has_lazy_export = matches!(
    module_type,
    ModuleType::Json | ModuleType::Text | ModuleType::Base64 | ModuleType::Dataurl
  );

  let source = match module_type {
    ModuleType::Js | ModuleType::Jsx | ModuleType::Ts | ModuleType::Tsx | ModuleType::Json => {
      Cow::Owned(source.try_into_string()?)
    }
    ModuleType::Css => {
      unreachable!("CSS modules should error before reaching parse_to_ecma_ast")
    }
    ModuleType::Text => {
      let text = source.try_into_string()?;
      // Strip UTF-8 BOM if present
      let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
      Cow::Owned(escape(text))
    }
    ModuleType::Asset => {
      return Err(anyhow::format_err!(
        "Encountered a module with type `asset` during AST parsing. \
         Modules with type `asset` must be handled by the builtin AssetModulePlugin before this stage; \
         please check your plugin and loader configuration."
      ))?;
    }
    ModuleType::Base64 => {
      let encoded = rolldown_utils::base64::to_standard_base64(source.as_bytes());
      Cow::Owned(escape(&encoded))
    }
    ModuleType::Dataurl => {
      let data = source.as_bytes();
      let guessed_mime = guess_mime(path, data)?;
      let dataurl = rolldown_utils::dataurl::encode_as_shortest_dataurl(&guessed_mime, data);
      Cow::Owned(escape(&dataurl))
    }
    ModuleType::Binary => {
      let encoded = rolldown_utils::base64::to_standard_base64(source.as_bytes());
      let to_binary = match options.platform {
        rolldown_common::Platform::Node => "__toBinaryNode",
        _ => "__toBinary",
      };
      Cow::Owned(rolldown_utils::concat_string!(
        "import {",
        to_binary,
        "} from '",
        RUNTIME_MODULE_KEY,
        "'; export default ",
        to_binary,
        "('",
        encoded,
        "')"
      ))
    }
    ModuleType::Empty => Cow::Borrowed(""),
    ModuleType::Copy => {
      return Err(anyhow::format_err!(
        "Encountered a module with type `copy` during AST parsing. \
         Modules with type `copy` must be handled by the builtin CopyModulePlugin before this stage; \
         please check your plugin and loader configuration."
      ))?;
    }
    ModuleType::Custom(custom_type) => {
      // TODO: should provide friendly error message to say that this type is not supported by rolldown.
      // Users should handle this type in load/transform hooks
      return Err(anyhow::format_err!("Unknown module type: {custom_type}"))?;
    }
  };

  Ok((has_lazy_export, source, module_type.into()))
}

fn parse_json_to_ecma_ast_with_machine_cache(
  project_root: &Path,
  source_path: &Path,
  id: &str,
  source: &StrOrBytes,
) -> BuildResult<EcmaAst> {
  let source_bytes = source.as_bytes();
  let use_machine_cache = core_json_source_worth_machine_cache(source_bytes);
  let cache_target =
    use_machine_cache.then(|| core_json_machine_cache_target(project_root, source_path)).flatten();
  parse_json_to_ecma_ast_with_optional_machine_cache_and_worth(
    cache_target.as_ref(),
    id,
    source,
    use_machine_cache,
  )
}

#[cfg(test)]
fn parse_json_to_ecma_ast_with_optional_machine_cache(
  cache_target: Option<&CoreJsonMachineCacheTarget>,
  id: &str,
  source: &StrOrBytes,
) -> BuildResult<EcmaAst> {
  let source_bytes = source.as_bytes();
  let use_machine_cache = core_json_source_worth_machine_cache(source_bytes);
  parse_json_to_ecma_ast_with_optional_machine_cache_and_worth(
    cache_target,
    id,
    source,
    use_machine_cache,
  )
}

fn parse_json_to_ecma_ast_with_optional_machine_cache_and_worth(
  cache_target: Option<&CoreJsonMachineCacheTarget>,
  id: &str,
  source: &StrOrBytes,
  use_machine_cache: bool,
) -> BuildResult<EcmaAst> {
  let source_bytes = source.as_bytes();
  let mut repair_source_hash = None;
  if use_machine_cache {
    if let Some(cache_target) = cache_target {
      match read_core_json_machine_cache_ast_or_repair_hash(cache_target, source_bytes) {
        CoreJsonMachineAstRead::Hit(ast) => return Ok(ast),
        CoreJsonMachineAstRead::Repair { source_hash } => {
          repair_source_hash = Some(source_hash);
        }
        CoreJsonMachineAstRead::Miss => {}
      }
    }
  }

  let json_value = parse_json_value_for_module(id, source)?;
  let ast = json_value_to_ecma_ast(&json_value);
  if use_machine_cache {
    if let Some(cache_target) = cache_target {
      write_core_json_machine_cache_with_optional_source_hash(
        cache_target,
        source_bytes,
        &json_value,
        repair_source_hash.as_ref(),
      );
    }
  }
  Ok(ast)
}

fn core_json_source_text(source: &StrOrBytes) -> anyhow::Result<&str> {
  #[cfg(test)]
  CORE_JSON_TEST_SOURCE_TEXT_CALLS.with(|calls| calls.set(calls.get() + 1));

  source.try_as_str()
}

fn parse_json_value_for_module(id: &str, source: &StrOrBytes) -> BuildResult<serde_json::Value> {
  #[cfg(test)]
  CORE_JSON_TEST_JSON_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));

  match source {
    StrOrBytes::Str(source_text) => parse_json_value_for_module_text(id, source_text),
    StrOrBytes::Bytes(source_bytes) => match serde_json::from_slice(source_bytes) {
      Ok(value) => Ok(value),
      Err(error) => {
        let source_text = core_json_source_text(source)?;
        Err(json_parse_diagnostic(id, source_text, error).into())
      }
    },
  }
}

fn parse_json_value_for_module_text(id: &str, source: &str) -> BuildResult<serde_json::Value> {
  Ok(serde_json::from_str(source).map_err(|error| json_parse_diagnostic(id, source, error))?)
}

fn json_parse_diagnostic(id: &str, source: &str, error: serde_json::Error) -> BuildDiagnostic {
  let line = error.line() - 1;
  // Convert to 0-indexed column. serde_json returns 1-indexed columns.
  let column = error.column().saturating_sub(1);
  BuildDiagnostic::json_parse(id.into(), source.into(), line, column, error.to_string().into())
}

fn core_json_machine_cache_target(
  project_root: &Path,
  source_path: &Path,
) -> Option<CoreJsonMachineCacheTarget> {
  #[cfg(test)]
  CORE_JSON_TEST_CACHE_TARGET_BUILDS.with(|calls| calls.set(calls.get() + 1));

  let config = DxMachineCacheConfig::from_env_if_enabled(project_root)?;
  let paths = config.paths_for_source(project_root, CORE_JSON_MACHINE_NAMESPACE, source_path);
  Some(CoreJsonMachineCacheTarget { config, paths, source_path: source_path.to_path_buf() })
}

#[cfg(test)]
fn core_json_machine_cache_target_for_source(
  project_root: &Path,
  source_path: &Path,
  source_bytes: &[u8],
) -> Option<CoreJsonMachineCacheTarget> {
  if !core_json_source_worth_machine_cache(source_bytes) {
    return None;
  }
  core_json_machine_cache_target(project_root, source_path)
}

fn core_json_source_worth_machine_cache(source_bytes: &[u8]) -> bool {
  #[cfg(test)]
  CORE_JSON_TEST_SOURCE_WORTH_CALLS.with(|calls| calls.set(calls.get() + 1));

  source_bytes.len() > CORE_JSON_MACHINE_MIN_SOURCE_BYTES
    && matches!(
      source_bytes.iter().copied().find(|byte| !byte.is_ascii_whitespace()),
      Some(b'{' | b'[')
    )
}

#[cfg(test)]
fn read_core_json_machine_cache_ast(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
) -> Option<EcmaAst> {
  match read_core_json_machine_cache_ast_or_repair_hash(cache_target, source_bytes) {
    CoreJsonMachineAstRead::Hit(ast) => Some(ast),
    CoreJsonMachineAstRead::Repair { .. } | CoreJsonMachineAstRead::Miss => None,
  }
}

fn read_core_json_machine_cache_ast_or_repair_hash(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
) -> CoreJsonMachineAstRead {
  let Some(hit) = read_core_json_machine_hit(cache_target, source_bytes) else {
    return CoreJsonMachineAstRead::Miss;
  };

  match decode_core_json_machine_hit_to_ecma_ast(&hit) {
    Some(ast) => CoreJsonMachineAstRead::Hit(ast),
    None => CoreJsonMachineAstRead::Repair { source_hash: hit.source_hash },
  }
}

#[cfg(test)]
fn read_core_json_machine_cache(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
) -> Option<Value> {
  let body = read_core_json_machine_body(cache_target, source_bytes)?;
  decode_core_json_machine_body_to_value(&body)
}

#[cfg(test)]
fn read_core_json_machine_body(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
) -> Option<Vec<u8>> {
  let hit = read_core_json_machine_hit(cache_target, source_bytes)?;
  decode_core_json_machine_hit_to_body(&hit)
}

fn read_core_json_machine_hit(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
) -> Option<DxMachineCacheHit> {
  match cache_target.config.read_validated_machine_with_source_hash(
    &cache_target.paths,
    &cache_target.source_path,
    source_bytes,
  ) {
    DxMachineCacheStatus::Hit(hit) => Some(hit),
    DxMachineCacheStatus::Miss | DxMachineCacheStatus::Disabled | DxMachineCacheStatus::Invalid => {
      None
    }
  }
}

#[cfg(test)]
fn decode_core_json_machine_hit_to_body(hit: &DxMachineCacheHit) -> Option<Vec<u8>> {
  let (magic, body) = core_json_machine_hit_body_parts(hit)?;

  match magic {
    CoreJsonMachineMagic::Legacy => Some(body.to_vec()),
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    CoreJsonMachineMagic::DxSerializer | CoreJsonMachineMagic::DxSerializerAligned => {
      decode_core_json_dx_serializer_machine_body(body)
    }
  }
}

fn decode_core_json_machine_hit_to_ecma_ast(hit: &DxMachineCacheHit) -> Option<EcmaAst> {
  let (magic, body) = core_json_machine_hit_body_parts(hit)?;

  match magic {
    CoreJsonMachineMagic::Legacy => decode_core_json_machine_body_to_ecma_ast(body),
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    CoreJsonMachineMagic::DxSerializer | CoreJsonMachineMagic::DxSerializerAligned => {
      decode_core_json_dx_serializer_machine_body_to_ecma_ast(body)
    }
  }
}

fn core_json_machine_hit_body_parts(
  hit: &DxMachineCacheHit,
) -> Option<(CoreJsonMachineMagic, &[u8])> {
  let magic = core_json_machine_magic(&hit.machine_bytes)?;
  let header_len = magic.header_len();
  if hit.machine_bytes.len() < header_len {
    return None;
  }
  let header = &hit.machine_bytes[..CORE_JSON_MACHINE_HEADER_LEN];
  if !core_json_machine_header_source_hash_matches(header, &hit.source_hash) {
    return None;
  }
  if !magic.has_valid_padding(&hit.machine_bytes) {
    return None;
  }

  let body = hit.machine_bytes.get(header_len..)?;
  if body.len() as u64 > CORE_JSON_MACHINE_MAX_BYTES - header_len as u64 {
    return None;
  }

  // The cache reader already validated the full machine bytes against metadata.
  // Keep direct payload decodes hash-checked, but do not rehash hot cache hits.
  Some((magic, body))
}

#[cfg(test)]
fn write_core_json_machine_cache(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
  json_value: &Value,
) {
  write_core_json_machine_cache_with_optional_source_hash(
    cache_target,
    source_bytes,
    json_value,
    None,
  );
}

fn write_core_json_machine_cache_with_optional_source_hash(
  cache_target: &CoreJsonMachineCacheTarget,
  source_bytes: &[u8],
  json_value: &Value,
  source_hash: Option<&blake3::Hash>,
) {
  if !cache_target.config.enabled {
    return;
  }
  let computed_source_hash;
  let source_hash = match source_hash {
    Some(source_hash) => source_hash,
    None => {
      computed_source_hash = hash_core_json_source_bytes(source_bytes);
      &computed_source_hash
    }
  };
  let Some(machine_bytes) =
    encode_core_json_machine_payload_with_source_hash(source_hash, json_value)
  else {
    return;
  };
  let _ = cache_target.config.write_machine_artifact_with_source_hash(
    &cache_target.paths,
    &cache_target.source_path,
    source_bytes.len(),
    source_hash,
    &machine_bytes,
  );
}

#[cfg(test)]
fn encode_core_json_machine_payload(source_bytes: &[u8], json_value: &Value) -> Option<Vec<u8>> {
  encode_core_json_machine_payload_with_source_hash(
    &hash_core_json_source_bytes(source_bytes),
    json_value,
  )
}

fn hash_core_json_source_bytes(source_bytes: &[u8]) -> blake3::Hash {
  #[cfg(test)]
  CORE_JSON_TEST_SOURCE_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  blake3::hash(source_bytes)
}

#[cfg(test)]
fn hash_core_json_machine_body_for_validation(body: &[u8]) -> blake3::Hash {
  CORE_JSON_TEST_BODY_HASH_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));

  blake3::hash(body)
}

fn encode_core_json_machine_payload_with_source_hash(
  source_hash: &blake3::Hash,
  json_value: &Value,
) -> Option<Vec<u8>> {
  let mut body = Vec::new();
  encode_core_json_value(json_value, &mut body)?;

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  {
    return encode_core_json_dx_serializer_machine_payload(source_hash, body);
  }

  #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
  {
    return encode_core_json_legacy_machine_payload(source_hash, &body);
  }
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn encode_core_json_dx_serializer_machine_payload(
  source_hash: &blake3::Hash,
  body: Vec<u8>,
) -> Option<Vec<u8>> {
  let payload = CoreJsonDxSerializerPayload { body };
  let archived_body = serializer::machine::api::serialize(&payload).ok()?;
  let archived_body = archived_body.as_ref();
  let body_hash = blake3::hash(archived_body);
  let machine_len = CORE_JSON_DX_SERIALIZER_ALIGNED_HEADER_LEN.checked_add(archived_body.len())?;
  let mut machine_bytes = Vec::with_capacity(machine_len);
  machine_bytes.extend_from_slice(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(source_hash.as_bytes());
  machine_bytes.extend_from_slice(body_hash.as_bytes());
  machine_bytes.extend_from_slice(&[0; 8]);
  machine_bytes.extend_from_slice(archived_body);
  Some(machine_bytes)
}

#[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
fn encode_core_json_legacy_machine_payload(
  source_hash: &blake3::Hash,
  body: &[u8],
) -> Option<Vec<u8>> {
  encode_core_json_machine_payload_body_with_magic(CORE_JSON_MACHINE_MAGIC, source_hash, body)
}

#[cfg(any(test, not(all(feature = "dx-serializer-local", not(target_family = "wasm")))))]
fn encode_core_json_machine_payload_body_with_magic(
  magic: &[u8; 8],
  source_hash: &blake3::Hash,
  body: &[u8],
) -> Option<Vec<u8>> {
  let body_hash = blake3::hash(body);
  let machine_len = CORE_JSON_MACHINE_HEADER_LEN.checked_add(body.len())?;
  let mut machine_bytes = Vec::with_capacity(machine_len);
  machine_bytes.extend_from_slice(magic);
  machine_bytes.extend_from_slice(source_hash.as_bytes());
  machine_bytes.extend_from_slice(body_hash.as_bytes());
  machine_bytes.extend_from_slice(body);
  Some(machine_bytes)
}

#[cfg(all(test, feature = "dx-serializer-local", not(target_family = "wasm")))]
fn decode_core_json_dx_serializer_machine_body(machine_body: &[u8]) -> Option<Vec<u8>> {
  #[cfg(test)]
  CORE_JSON_TEST_DX_SERIALIZER_BODY_VEC_DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));

  Some(core_json_dx_serializer_archived_payload(machine_body, |archived| {
    archived.body.as_slice().to_vec()
  }))
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn decode_core_json_dx_serializer_machine_body_to_ecma_ast(machine_body: &[u8]) -> Option<EcmaAst> {
  Some(core_json_dx_serializer_archived_payload(machine_body, |archived| {
    decode_core_json_machine_body_to_ecma_ast(archived.body.as_slice())
  })?)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn core_json_dx_serializer_archived_payload<R>(
  machine_body: &[u8],
  decode: impl FnOnce(&<CoreJsonDxSerializerPayload as rkyv::Archive>::Archived) -> R,
) -> R {
  if core_json_dx_serializer_body_is_aligned(machine_body) {
    // SAFETY: The cache metadata layer validates source and machine hashes before
    // this decoder runs. Aligned bodies can be read directly through RKYV.
    let archived =
      unsafe { serializer::machine::api::deserialize::<CoreJsonDxSerializerPayload>(machine_body) };
    return decode(archived);
  }

  #[cfg(test)]
  CORE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(calls.get() + 1));

  let mut aligned: rkyv::util::AlignedVec<16> =
    rkyv::util::AlignedVec::with_capacity(machine_body.len());
  aligned.extend_from_slice(machine_body);
  // SAFETY: The cache metadata layer validates source and machine hashes before
  // this decoder runs, and the RKYV body is copied into aligned storage first.
  let archived =
    unsafe { serializer::machine::api::deserialize::<CoreJsonDxSerializerPayload>(&aligned) };
  decode(archived)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn core_json_dx_serializer_body_is_aligned(machine_body: &[u8]) -> bool {
  let alignment = std::mem::align_of::<<CoreJsonDxSerializerPayload as rkyv::Archive>::Archived>();
  machine_body.as_ptr() as usize % alignment == 0
}

#[cfg(test)]
fn core_json_machine_payload_body_capacity(value: &Value) -> Option<usize> {
  #[cfg(test)]
  CORE_JSON_TEST_BODY_CAPACITY_CALLS.with(|calls| calls.set(calls.get() + 1));

  core_json_machine_payload_body_capacity_at_depth(value, 0)
}

#[cfg(test)]
fn core_json_machine_payload_body_capacity_at_depth(value: &Value, depth: u16) -> Option<usize> {
  if depth > CORE_JSON_MACHINE_MAX_DEPTH {
    return None;
  }
  match value {
    Value::Null | Value::Bool(_) => Some(1),
    Value::Number(number) => {
      let len = number.to_string().len();
      core_json_tagged_len_prefixed_capacity(len, CORE_JSON_MACHINE_MAX_NUMBER_LEN)
    }
    Value::String(value) => {
      core_json_tagged_len_prefixed_capacity(value.len(), CORE_JSON_MACHINE_MAX_STRING_LEN)
    }
    Value::Array(items) => {
      let child_depth = depth.checked_add(1)?;
      let mut len = core_json_tagged_count_capacity(items.len())?;
      for item in items {
        len =
          len.checked_add(core_json_machine_payload_body_capacity_at_depth(item, child_depth)?)?;
      }
      Some(len)
    }
    Value::Object(entries) => {
      let child_depth = depth.checked_add(1)?;
      let mut len = core_json_tagged_count_capacity(entries.len())?;
      for (key, value) in entries {
        len = len.checked_add(core_json_len_prefixed_capacity(
          key.len(),
          CORE_JSON_MACHINE_MAX_STRING_LEN,
        )?)?;
        len =
          len.checked_add(core_json_machine_payload_body_capacity_at_depth(value, child_depth)?)?;
      }
      Some(len)
    }
  }
}

#[cfg(test)]
fn core_json_tagged_count_capacity(len: usize) -> Option<usize> {
  u32::try_from(len).ok()?;
  1usize.checked_add(4)
}

#[cfg(test)]
fn core_json_tagged_len_prefixed_capacity(len: usize, max_len: usize) -> Option<usize> {
  1usize.checked_add(core_json_len_prefixed_capacity(len, max_len)?)
}

#[cfg(test)]
fn core_json_len_prefixed_capacity(len: usize, max_len: usize) -> Option<usize> {
  if len > max_len {
    return None;
  }
  4usize.checked_add(u32::try_from(len).ok()? as usize)
}

#[cfg(test)]
fn decode_core_json_machine_payload(machine_bytes: &[u8], source_bytes: &[u8]) -> Option<Value> {
  let source_hash = blake3::hash(source_bytes);
  decode_core_json_machine_payload_with_source_hash(machine_bytes, &source_hash)
}

#[cfg(test)]
fn decode_core_json_machine_payload_with_source_hash(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
) -> Option<Value> {
  let body = core_json_machine_payload_body_with_source_hash(machine_bytes, source_hash)?;
  decode_core_json_machine_body_to_value(&body)
}

#[cfg(test)]
fn decode_core_json_machine_body_to_value(body: &[u8]) -> Option<Value> {
  let mut reader = CoreJsonMachineReader::new(body);
  let value = reader.read_value(0)?;
  (reader.is_finished()).then_some(value)
}

#[cfg(test)]
fn decode_core_json_machine_payload_to_ecma_ast(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
) -> Option<EcmaAst> {
  let body = core_json_machine_payload_body_with_source_hash(machine_bytes, source_hash)?;
  decode_core_json_machine_body_to_ecma_ast(&body)
}

fn decode_core_json_machine_body_to_ecma_ast(body: &[u8]) -> Option<EcmaAst> {
  let source = ArcStr::from("");
  let allocator = Allocator::default();
  let mut invalid = false;
  let ast = EcmaAst::from_allocator_and_source(source, allocator, |allocator| {
    let builder = AstBuilder::new(allocator);
    let mut reader = CoreJsonMachineReader::new(body);
    let Some(expr) = reader.read_expression(builder, 0) else {
      invalid = true;
      return empty_core_json_program(builder);
    };
    if !reader.is_finished() {
      invalid = true;
      return empty_core_json_program(builder);
    }
    builder.program(
      SPAN,
      OxcSourceType::default().with_module(true),
      "",
      builder.vec(),
      None,
      builder.vec(),
      builder.vec1(builder.statement_expression(SPAN, expr)),
    )
  });
  (!invalid).then_some(ast)
}

fn core_json_ast_object_key_string_literal<'a>(
  builder: AstBuilder<'a>,
  key: &str,
) -> oxc::ast::ast::PropertyKey<'a> {
  #[cfg(test)]
  CORE_JSON_TEST_AST_OBJECT_KEY_DIRECT_ALLOC_CALLS.with(|calls| calls.set(calls.get() + 1));

  oxc::ast::ast::PropertyKey::StringLiteral(builder.alloc_string_literal(
    SPAN,
    builder.str(key),
    None,
  ))
}

fn empty_core_json_program<'a>(builder: AstBuilder<'a>) -> oxc::ast::ast::Program<'a> {
  builder.program(
    SPAN,
    OxcSourceType::default().with_module(true),
    "",
    builder.vec(),
    None,
    builder.vec(),
    builder.vec(),
  )
}

#[cfg(test)]
fn core_json_machine_payload_body_with_source_hash(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
) -> Option<Vec<u8>> {
  #[cfg(test)]
  CORE_JSON_TEST_PAYLOAD_BODY_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));
  let magic = core_json_machine_magic(machine_bytes)?;
  let header_len = magic.header_len();
  if !core_json_machine_header_matches_source_hash(machine_bytes, source_hash) {
    return None;
  }
  if !magic.has_valid_padding(machine_bytes) {
    return None;
  }
  let body = machine_bytes.get(header_len..)?;
  if machine_bytes[CORE_JSON_MACHINE_BODY_HASH_RANGE]
    != hash_core_json_machine_body_for_validation(body).as_bytes()[..]
  {
    return None;
  }

  match magic {
    CoreJsonMachineMagic::Legacy => Some(body.to_vec()),
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    CoreJsonMachineMagic::DxSerializer | CoreJsonMachineMagic::DxSerializerAligned => {
      decode_core_json_dx_serializer_machine_body(body)
    }
  }
}

#[cfg(test)]
fn core_json_machine_header_matches_source_hash(header: &[u8], source_hash: &blake3::Hash) -> bool {
  header.len() >= CORE_JSON_MACHINE_HEADER_LEN
    && core_json_machine_header_has_supported_magic(header)
    && core_json_machine_header_source_hash_matches(header, source_hash)
}

fn core_json_machine_header_source_hash_matches(header: &[u8], source_hash: &blake3::Hash) -> bool {
  header.len() >= CORE_JSON_MACHINE_HEADER_LEN
    && header[CORE_JSON_MACHINE_SOURCE_HASH_RANGE] == source_hash.as_bytes()[..]
}

#[cfg(test)]
fn core_json_machine_header_has_supported_magic(header: &[u8]) -> bool {
  core_json_machine_magic(header).is_some()
}

fn core_json_machine_magic(machine_bytes: &[u8]) -> Option<CoreJsonMachineMagic> {
  #[cfg(test)]
  CORE_JSON_TEST_MACHINE_MAGIC_CALLS.with(|calls| calls.set(calls.get() + 1));

  if machine_bytes.len() < CORE_JSON_MACHINE_MAGIC.len() {
    return None;
  }
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  record_core_json_aligned_magic_check();
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  if &machine_bytes[..CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC.len()]
    == CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC
  {
    return Some(CoreJsonMachineMagic::DxSerializerAligned);
  }
  record_core_json_legacy_magic_check();
  if &machine_bytes[..CORE_JSON_MACHINE_MAGIC.len()] == CORE_JSON_MACHINE_MAGIC {
    return Some(CoreJsonMachineMagic::Legacy);
  }
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  if &machine_bytes[..CORE_JSON_DX_SERIALIZER_MACHINE_MAGIC.len()]
    == CORE_JSON_DX_SERIALIZER_MACHINE_MAGIC
  {
    return Some(CoreJsonMachineMagic::DxSerializer);
  }
  None
}

fn record_core_json_legacy_magic_check() {
  #[cfg(test)]
  CORE_JSON_TEST_LEGACY_MAGIC_CHECK_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn record_core_json_aligned_magic_check() {
  #[cfg(test)]
  CORE_JSON_TEST_ALIGNED_MAGIC_CHECK_CALLS.with(|calls| calls.set(calls.get() + 1));
}

fn encode_core_json_value(value: &Value, output: &mut Vec<u8>) -> Option<()> {
  encode_core_json_value_at_depth(value, output, 0)
}

fn encode_core_json_value_at_depth(value: &Value, output: &mut Vec<u8>, depth: u16) -> Option<()> {
  if depth > CORE_JSON_MACHINE_MAX_DEPTH {
    return None;
  }
  match value {
    Value::Null => output.push(CORE_JSON_TAG_NULL),
    Value::Bool(false) => output.push(CORE_JSON_TAG_FALSE),
    Value::Bool(true) => output.push(CORE_JSON_TAG_TRUE),
    Value::Number(number) => {
      output.push(CORE_JSON_TAG_NUMBER);
      push_core_json_len_prefixed_bytes_bounded(
        output,
        number.to_string().as_bytes(),
        CORE_JSON_MACHINE_MAX_NUMBER_LEN,
      )?;
    }
    Value::String(value) => {
      output.push(CORE_JSON_TAG_STRING);
      push_core_json_len_prefixed_bytes_bounded(
        output,
        value.as_bytes(),
        CORE_JSON_MACHINE_MAX_STRING_LEN,
      )?;
    }
    Value::Array(items) => {
      let child_depth = depth.checked_add(1)?;
      output.push(CORE_JSON_TAG_ARRAY);
      push_core_json_count(output, items.len())?;
      for item in items {
        encode_core_json_value_at_depth(item, output, child_depth)?;
      }
    }
    Value::Object(entries) => {
      let child_depth = depth.checked_add(1)?;
      output.push(CORE_JSON_TAG_OBJECT);
      push_core_json_count(output, entries.len())?;
      for (key, value) in entries {
        push_core_json_len_prefixed_bytes_bounded(
          output,
          key.as_bytes(),
          CORE_JSON_MACHINE_MAX_STRING_LEN,
        )?;
        encode_core_json_value_at_depth(value, output, child_depth)?;
      }
    }
  }
  Some(())
}

fn push_core_json_count(output: &mut Vec<u8>, len: usize) -> Option<()> {
  output.extend_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
  Some(())
}

fn push_core_json_len_prefixed_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Option<()> {
  output.extend_from_slice(&u32::try_from(bytes.len()).ok()?.to_le_bytes());
  output.extend_from_slice(bytes);
  Some(())
}

fn push_core_json_len_prefixed_bytes_bounded(
  output: &mut Vec<u8>,
  bytes: &[u8],
  max_len: usize,
) -> Option<()> {
  if bytes.len() > max_len {
    return None;
  }
  push_core_json_len_prefixed_bytes(output, bytes)
}

struct CoreJsonMachineReader<'a> {
  bytes: &'a [u8],
  offset: usize,
}

impl<'a> CoreJsonMachineReader<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self { bytes, offset: 0 }
  }

  fn is_finished(&self) -> bool {
    self.offset == self.bytes.len()
  }

  fn remaining(&self) -> usize {
    self.bytes.len().saturating_sub(self.offset)
  }

  #[cfg(test)]
  fn read_value(&mut self, depth: u16) -> Option<Value> {
    if depth > CORE_JSON_MACHINE_MAX_DEPTH {
      return None;
    }
    match self.read_u8()? {
      CORE_JSON_TAG_NULL => Some(Value::Null),
      CORE_JSON_TAG_FALSE => Some(Value::Bool(false)),
      CORE_JSON_TAG_TRUE => Some(Value::Bool(true)),
      CORE_JSON_TAG_NUMBER => {
        let bytes = self.read_len_prefixed_bytes(CORE_JSON_MACHINE_MAX_NUMBER_LEN)?;
        let number = serde_json::from_slice(bytes).ok()?;
        Some(Value::Number(number))
      }
      CORE_JSON_TAG_STRING => {
        let bytes = self.read_len_prefixed_bytes(CORE_JSON_MACHINE_MAX_STRING_LEN)?;
        Some(Value::String(std::str::from_utf8(bytes).ok()?.to_owned()))
      }
      CORE_JSON_TAG_ARRAY => {
        let len = self.read_count()?;
        if len > self.remaining() {
          return None;
        }
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
          items.push(self.read_value(depth + 1)?);
        }
        Some(Value::Array(items))
      }
      CORE_JSON_TAG_OBJECT => {
        let len = self.read_count()?;
        if len > self.remaining() / 5 {
          return None;
        }
        let mut entries = serde_json::Map::new();
        for _ in 0..len {
          let key = self.read_len_prefixed_string(CORE_JSON_MACHINE_MAX_STRING_LEN)?;
          let value = self.read_value(depth + 1)?;
          entries.insert(key, value);
        }
        Some(Value::Object(entries))
      }
      _ => None,
    }
  }

  fn read_expression<'b>(
    &mut self,
    builder: AstBuilder<'b>,
    depth: u16,
  ) -> Option<oxc::ast::ast::Expression<'b>> {
    if depth > CORE_JSON_MACHINE_MAX_DEPTH {
      return None;
    }
    match self.read_u8()? {
      CORE_JSON_TAG_NULL => Some(builder.expression_null_literal(SPAN)),
      CORE_JSON_TAG_FALSE => Some(builder.expression_boolean_literal(SPAN, false)),
      CORE_JSON_TAG_TRUE => Some(builder.expression_boolean_literal(SPAN, true)),
      CORE_JSON_TAG_NUMBER => {
        let bytes = self.read_len_prefixed_bytes(CORE_JSON_MACHINE_MAX_NUMBER_LEN)?;
        let value = parse_core_json_machine_number_for_ast(bytes)?;
        Some(builder.expression_numeric_literal(
          SPAN,
          value,
          None,
          oxc::ast::ast::NumberBase::Decimal,
        ))
      }
      CORE_JSON_TAG_STRING => {
        let bytes = self.read_len_prefixed_bytes(CORE_JSON_MACHINE_MAX_STRING_LEN)?;
        let value = std::str::from_utf8(bytes).ok()?;
        Some(builder.expression_string_literal(SPAN, builder.str(value), None))
      }
      CORE_JSON_TAG_ARRAY => {
        let len = self.read_count()?;
        if len > self.remaining() {
          return None;
        }
        if len == 0 {
          return Some(builder.expression_array(SPAN, builder.vec()));
        }
        if len == 1 {
          let expr = self.read_expression(builder, depth + 1)?;
          return Some(builder.expression_array(
            SPAN,
            builder.vec1(oxc::ast::ast::ArrayExpressionElement::from(expr)),
          ));
        }
        #[cfg(test)]
        CORE_JSON_TEST_AST_VEC_CAPACITY_CALLS.with(|calls| calls.set(calls.get() + 1));
        let mut elements = builder.vec_with_capacity(len);
        for _ in 0..len {
          let expr = self.read_expression(builder, depth + 1)?;
          elements.push(oxc::ast::ast::ArrayExpressionElement::from(expr));
        }
        Some(builder.expression_array(SPAN, elements))
      }
      CORE_JSON_TAG_OBJECT => {
        let len = self.read_count()?;
        if len > self.remaining() / 5 {
          return None;
        }
        if len == 0 {
          return Some(builder.expression_object(SPAN, builder.vec()));
        }
        if len == 1 {
          let property = self.read_object_property(builder, depth + 1)?;
          return Some(builder.expression_object(SPAN, builder.vec1(property)));
        }
        #[cfg(test)]
        CORE_JSON_TEST_AST_VEC_CAPACITY_CALLS.with(|calls| calls.set(calls.get() + 1));
        let mut properties = builder.vec_with_capacity(len);
        for _ in 0..len {
          properties.push(self.read_object_property(builder, depth + 1)?);
        }
        Some(builder.expression_object(SPAN, properties))
      }
      _ => None,
    }
  }

  fn read_object_property<'b>(
    &mut self,
    builder: AstBuilder<'b>,
    depth: u16,
  ) -> Option<oxc::ast::ast::ObjectPropertyKind<'b>> {
    let key_bytes = self.read_len_prefixed_bytes(CORE_JSON_MACHINE_MAX_STRING_LEN)?;
    let key = std::str::from_utf8(key_bytes).ok()?;
    let key = core_json_ast_object_key_string_literal(builder, key);
    let value_expr = self.read_expression(builder, depth)?;
    Some(builder.object_property_kind_object_property(
      SPAN,
      oxc::ast::ast::PropertyKind::Init,
      key,
      value_expr,
      false,
      false,
      false,
    ))
  }

  fn read_count(&mut self) -> Option<usize> {
    Some(u32::from_le_bytes(self.read_array::<4>()?) as usize)
  }

  #[cfg(test)]
  fn read_len_prefixed_string(&mut self, max_len: usize) -> Option<String> {
    let bytes = self.read_len_prefixed_bytes(max_len)?;
    Some(std::str::from_utf8(bytes).ok()?.to_owned())
  }

  fn read_len_prefixed_bytes(&mut self, max_len: usize) -> Option<&'a [u8]> {
    let len = self.read_count()?;
    if len > max_len {
      return None;
    }
    self.read_bytes(len)
  }

  fn read_u8(&mut self) -> Option<u8> {
    let value = *self.bytes.get(self.offset)?;
    self.offset += 1;
    Some(value)
  }

  fn read_array<const N: usize>(&mut self) -> Option<[u8; N]> {
    self.read_bytes(N)?.try_into().ok()
  }

  fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
    let end = self.offset.checked_add(len)?;
    let bytes = self.bytes.get(self.offset..end)?;
    self.offset = end;
    Some(bytes)
  }
}

fn parse_core_json_machine_number_for_ast(bytes: &[u8]) -> Option<f64> {
  if let Some(value) = parse_core_json_machine_plain_integer_for_ast(bytes) {
    return Some(value);
  }
  if let Some(value) = parse_core_json_machine_decimal_or_exponent_for_ast(bytes) {
    return Some(value);
  }

  #[cfg(test)]
  CORE_JSON_TEST_AST_NUMBER_JSON_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));
  serde_json::from_slice::<f64>(bytes).ok().filter(|value| value.is_finite())
}

fn parse_core_json_machine_decimal_or_exponent_for_ast(bytes: &[u8]) -> Option<f64> {
  if !core_json_machine_decimal_or_exponent_number_is_valid(bytes) {
    return None;
  }
  let value = std::str::from_utf8(bytes).ok()?.parse::<f64>().ok()?;
  value.is_finite().then_some(value)
}

fn core_json_machine_decimal_or_exponent_number_is_valid(bytes: &[u8]) -> bool {
  let mut cursor = 0;
  if bytes.get(cursor) == Some(&b'-') {
    cursor += 1;
  }

  let Some(first_digit) = bytes.get(cursor).copied() else { return false };
  match first_digit {
    b'0' => {
      cursor += 1;
    }
    b'1'..=b'9' => {
      cursor += 1;
      while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
      }
    }
    _ => return false,
  }

  let mut has_decimal_or_exponent = false;
  if bytes.get(cursor) == Some(&b'.') {
    has_decimal_or_exponent = true;
    cursor += 1;
    let fraction_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
      cursor += 1;
    }
    if cursor == fraction_start {
      return false;
    }
  }

  if matches!(bytes.get(cursor), Some(b'e' | b'E')) {
    has_decimal_or_exponent = true;
    cursor += 1;
    if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
      cursor += 1;
    }
    let exponent_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
      cursor += 1;
    }
    if cursor == exponent_start {
      return false;
    }
  }

  has_decimal_or_exponent && cursor == bytes.len()
}

fn parse_core_json_machine_plain_integer_for_ast(bytes: &[u8]) -> Option<f64> {
  let (is_negative, digits) = match bytes {
    [b'-', digits @ ..] => (true, digits),
    digits => (false, digits),
  };
  if digits.is_empty() {
    return None;
  }
  if digits.len() > 1 && digits[0] == b'0' {
    return None;
  }

  let mut value = 0_u64;
  for digit in digits {
    if !digit.is_ascii_digit() {
      return None;
    }
    value = value.checked_mul(10)?.checked_add(u64::from(digit - b'0'))?;
  }

  let value = value as f64;
  Some(if is_negative { -value } else { value })
}

#[cfg(test)]
mod tests {
  use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
  };

  use rolldown_ecmascript::PrintOptions;
  use serde_json::Value;

  use super::*;

  fn reset_core_json_cache_target_build_count() {
    CORE_JSON_TEST_CACHE_TARGET_BUILDS.with(|calls| calls.set(0));
  }

  fn core_json_cache_target_build_count() -> u64 {
    CORE_JSON_TEST_CACHE_TARGET_BUILDS.with(|calls| calls.get())
  }

  fn reset_core_json_source_hash_count() {
    CORE_JSON_TEST_SOURCE_HASH_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_source_hash_count() -> u64 {
    CORE_JSON_TEST_SOURCE_HASH_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_source_text_count() {
    CORE_JSON_TEST_SOURCE_TEXT_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_source_text_count() -> u64 {
    CORE_JSON_TEST_SOURCE_TEXT_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_json_parse_count() {
    CORE_JSON_TEST_JSON_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_json_parse_count() -> u64 {
    CORE_JSON_TEST_JSON_PARSE_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_body_capacity_count() {
    CORE_JSON_TEST_BODY_CAPACITY_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_body_capacity_count() -> u64 {
    CORE_JSON_TEST_BODY_CAPACITY_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_ast_vec_capacity_count() {
    CORE_JSON_TEST_AST_VEC_CAPACITY_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_ast_vec_capacity_count() -> u64 {
    CORE_JSON_TEST_AST_VEC_CAPACITY_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_payload_body_validation_count() {
    CORE_JSON_TEST_PAYLOAD_BODY_VALIDATION_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_payload_body_validation_count() -> u64 {
    CORE_JSON_TEST_PAYLOAD_BODY_VALIDATION_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_body_hash_validation_count() {
    CORE_JSON_TEST_BODY_HASH_VALIDATION_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_body_hash_validation_count() -> u64 {
    CORE_JSON_TEST_BODY_HASH_VALIDATION_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_machine_magic_count() {
    CORE_JSON_TEST_MACHINE_MAGIC_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_machine_magic_count() -> u64 {
    CORE_JSON_TEST_MACHINE_MAGIC_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_legacy_magic_check_count() {
    CORE_JSON_TEST_LEGACY_MAGIC_CHECK_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_legacy_magic_check_count() -> u64 {
    CORE_JSON_TEST_LEGACY_MAGIC_CHECK_CALLS.with(|calls| calls.get())
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_core_json_aligned_magic_check_count() {
    CORE_JSON_TEST_ALIGNED_MAGIC_CHECK_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_aligned_magic_check_count() -> u64 {
    CORE_JSON_TEST_ALIGNED_MAGIC_CHECK_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_ast_number_json_parse_count() {
    CORE_JSON_TEST_AST_NUMBER_JSON_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_ast_number_json_parse_count() -> u64 {
    CORE_JSON_TEST_AST_NUMBER_JSON_PARSE_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_ast_object_key_direct_alloc_count() {
    CORE_JSON_TEST_AST_OBJECT_KEY_DIRECT_ALLOC_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_ast_object_key_direct_alloc_count() -> u64 {
    CORE_JSON_TEST_AST_OBJECT_KEY_DIRECT_ALLOC_CALLS.with(|calls| calls.get())
  }

  fn reset_core_json_source_worth_count() {
    CORE_JSON_TEST_SOURCE_WORTH_CALLS.with(|calls| calls.set(0));
  }

  fn core_json_source_worth_count() -> u64 {
    CORE_JSON_TEST_SOURCE_WORTH_CALLS.with(|calls| calls.get())
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_core_json_dx_serializer_body_vec_decode_count() {
    CORE_JSON_TEST_DX_SERIALIZER_BODY_VEC_DECODE_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_body_vec_decode_count() -> u64 {
    CORE_JSON_TEST_DX_SERIALIZER_BODY_VEC_DECODE_CALLS.with(|calls| calls.get())
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_core_json_dx_serializer_aligned_copy_count() {
    CORE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_aligned_copy_count() -> u64 {
    CORE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.get())
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_core_json_dx_serializer_body_encode_clone_count() {
    CORE_JSON_TEST_DX_SERIALIZER_BODY_ENCODE_CLONE_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_body_encode_clone_count() -> u64 {
    CORE_JSON_TEST_DX_SERIALIZER_BODY_ENCODE_CLONE_CALLS.with(|calls| calls.get())
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_machine_payload_uses_dx_serializer_rkyv_when_enabled() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));
    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), Some(json_value));
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_write_moves_encoded_body_without_clone() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let source_hash = blake3::hash(source);
    let json_value = serde_json::from_slice::<Value>(source).unwrap();

    reset_core_json_dx_serializer_body_encode_clone_count();
    let machine_bytes =
      encode_core_json_machine_payload_with_source_hash(&source_hash, &json_value).unwrap();

    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));
    assert_eq!(core_json_dx_serializer_body_encode_clone_count(), 0);
    assert_eq!(
      decode_core_json_machine_payload_with_source_hash(&machine_bytes, &source_hash),
      Some(json_value)
    );
    assert!(decode_core_json_machine_payload_to_ecma_ast(&machine_bytes, &source_hash).is_some());
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_aligned_body_decodes_ast_without_aligned_copy() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let payload = CoreJsonDxSerializerPayload { body };
    let machine_body = serializer::machine::api::serialize(&payload).unwrap();

    reset_core_json_dx_serializer_aligned_copy_count();
    let cached_ast =
      decode_core_json_dx_serializer_machine_body_to_ecma_ast(machine_body.as_ref()).unwrap();

    assert_eq!(core_json_dx_serializer_aligned_copy_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_body_decodes_ast_from_unaligned_slice_with_one_copy() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let payload = CoreJsonDxSerializerPayload { body };
    let machine_body = serializer::machine::api::serialize(&payload).unwrap();
    let mut unaligned_storage = Vec::with_capacity(machine_body.len() + 1);
    unaligned_storage.push(0);
    unaligned_storage.extend_from_slice(machine_body.as_ref());
    let unaligned_machine_body = &unaligned_storage[1..];

    assert!(!core_json_dx_serializer_body_is_aligned(unaligned_machine_body));

    reset_core_json_dx_serializer_aligned_copy_count();
    let cached_ast =
      decode_core_json_dx_serializer_machine_body_to_ecma_ast(unaligned_machine_body).unwrap();

    assert_eq!(core_json_dx_serializer_aligned_copy_count(), 1);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_machine_payload_aligns_archived_body_for_direct_decode() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let source_hash = blake3::hash(source);
    let machine_bytes =
      encode_core_json_machine_payload_with_source_hash(&source_hash, &json_value).unwrap();
    let hit = DxMachineCacheHit { machine_bytes, source_hash };
    let (_, body) = core_json_machine_hit_body_parts(&hit).unwrap();

    assert!(hit.machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));
    assert!(core_json_dx_serializer_body_is_aligned(body));

    reset_core_json_dx_serializer_aligned_copy_count();
    let cached_ast = decode_core_json_machine_hit_to_ecma_ast(&hit).unwrap();

    assert_eq!(core_json_dx_serializer_aligned_copy_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_dx_serializer_legacy_machine_payload_still_decodes() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let payload = CoreJsonDxSerializerPayload { body };
    let archived_body = serializer::machine::api::serialize(&payload).unwrap();
    let machine_bytes = encode_core_json_machine_payload_body_with_magic(
      CORE_JSON_DX_SERIALIZER_MACHINE_MAGIC,
      &blake3::hash(source),
      archived_body.as_ref(),
    )
    .unwrap();

    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_MACHINE_MAGIC));
    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), Some(json_value));
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_machine_magic_checks_current_aligned_magic_before_legacy_fallback() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    reset_core_json_legacy_magic_check_count();
    reset_core_json_aligned_magic_check_count();
    let magic = core_json_machine_magic(&machine_bytes);

    assert!(matches!(magic, Some(CoreJsonMachineMagic::DxSerializerAligned)));
    assert_eq!(core_json_aligned_magic_check_count(), 1);
    assert_eq!(core_json_legacy_magic_check_count(), 0);
  }

  #[test]
  fn core_json_machine_payload_round_trips_binary_json_value() {
    let source = br#"{"name":"rolldown","items":[1,true,null]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert!(machine_bytes.starts_with(CORE_JSON_MACHINE_MAGIC));
    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), Some(json_value));
  }

  #[test]
  fn core_json_machine_binary_payload_parses_plain_integer_ast_numbers_without_json_parser() {
    let source = br#"{"items":[0,1,-42,900719925474099],"value":12345}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let hit = DxMachineCacheHit {
      machine_bytes: core_json_machine_for_body(source, &body),
      source_hash: blake3::hash(source),
    };

    reset_core_json_ast_number_json_parse_count();
    let cached_ast = decode_core_json_machine_hit_to_ecma_ast(&hit).unwrap();

    assert_eq!(core_json_ast_number_json_parse_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_large_plain_integer_ast_numbers_without_json_parser() {
    let source = br#"{"items":[9007199254740995,-9007199254740995,18446744073709551615]}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let hit = DxMachineCacheHit {
      machine_bytes: core_json_machine_for_body(source, &body),
      source_hash: blake3::hash(source),
    };

    reset_core_json_ast_number_json_parse_count();
    let cached_ast = decode_core_json_machine_hit_to_ecma_ast(&hit).unwrap();

    assert_eq!(core_json_ast_number_json_parse_count(), 0);
    assert_eq!(parse_core_json_machine_plain_integer_for_ast(b"18446744073709551616"), None);
    assert_eq!(parse_core_json_machine_plain_integer_for_ast(b"-18446744073709551616"), None);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_plain_integer_fast_path_keeps_number_fallback_boundaries() {
    reset_core_json_ast_number_json_parse_count();
    assert_eq!(parse_core_json_machine_number_for_ast(b"1.25"), Some(1.25));
    assert_eq!(core_json_ast_number_json_parse_count(), 0);

    reset_core_json_ast_number_json_parse_count();
    assert_eq!(parse_core_json_machine_number_for_ast(b"1e3"), Some(1000.0));
    assert_eq!(core_json_ast_number_json_parse_count(), 0);

    reset_core_json_ast_number_json_parse_count();
    assert_eq!(parse_core_json_machine_number_for_ast(b"1.25e-6"), Some(0.00000125));
    assert_eq!(core_json_ast_number_json_parse_count(), 0);

    reset_core_json_ast_number_json_parse_count();
    assert_eq!(parse_core_json_machine_number_for_ast(b"01"), None);
    assert_eq!(core_json_ast_number_json_parse_count(), 1);

    reset_core_json_ast_number_json_parse_count();
    let negative_zero = parse_core_json_machine_number_for_ast(b"-0").unwrap();
    assert_eq!(core_json_ast_number_json_parse_count(), 0);
    assert!(negative_zero.is_sign_negative());
  }

  #[test]
  fn core_json_machine_decimal_exponent_ast_numbers_without_json_parser() {
    let source = br#"{"numbers":[1.25,1e3,1.25e-6,0.0,-0.0],"nested":{"answer":42}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let hit = DxMachineCacheHit {
      machine_bytes: core_json_machine_for_body(source, &body),
      source_hash: blake3::hash(source),
    };

    reset_core_json_ast_number_json_parse_count();
    let cached_ast = decode_core_json_machine_hit_to_ecma_ast(&hit).unwrap();

    assert_eq!(core_json_ast_number_json_parse_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_builds_object_keys_without_temporary_key_expressions() {
    let source = br#"{"plain":1,"__proto__":{"nested-key":2},"with space":3}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut body = Vec::new();
    encode_core_json_value(&json_value, &mut body).unwrap();
    let hit = DxMachineCacheHit {
      machine_bytes: core_json_machine_for_body(source, &body),
      source_hash: blake3::hash(source),
    };

    reset_core_json_ast_object_key_direct_alloc_count();
    let cached_ast = decode_core_json_machine_hit_to_ecma_ast(&hit).unwrap();

    assert_eq!(core_json_ast_object_key_direct_alloc_count(), 4);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_payload_rejects_source_mutation_before_value_decode() {
    let source = br#"{"name":"rolldown"}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    assert_eq!(decode_core_json_machine_payload(&machine_bytes, br#"{"name":"changed"}"#), None);
  }

  #[test]
  fn core_json_machine_payload_rejects_valid_shaped_body_corruption() {
    let source = br#"{"value":1}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let mut machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();
    let digit = machine_bytes.iter().rposition(|byte| *byte == b'1').unwrap();
    machine_bytes[digit] = b'2';

    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), None);
  }

  #[test]
  fn core_json_machine_payload_rejects_malformed_binary_shapes() {
    let source = br#"{"name":"rolldown"}"#;
    assert_eq!(
      decode_core_json_machine_payload(&core_json_machine_for_body(source, &[255]), source),
      None
    );

    let mut with_trailing =
      encode_core_json_machine_payload(source, &serde_json::from_slice::<Value>(source).unwrap())
        .unwrap();
    with_trailing.push(CORE_JSON_TAG_NULL);
    assert_eq!(decode_core_json_machine_payload(&with_trailing, source), None);

    let mut oversized_string = vec![CORE_JSON_TAG_STRING];
    oversized_string.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
      decode_core_json_machine_payload(
        &core_json_machine_for_body(source, &oversized_string),
        source
      ),
      None
    );

    let mut impossible_array = vec![CORE_JSON_TAG_ARRAY];
    impossible_array.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
      decode_core_json_machine_payload(
        &core_json_machine_for_body(source, &impossible_array),
        source
      ),
      None
    );

    let mut impossible_object = vec![CORE_JSON_TAG_OBJECT];
    impossible_object.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
      decode_core_json_machine_payload(
        &core_json_machine_for_body(source, &impossible_object),
        source
      ),
      None
    );

    let mut invalid_number = vec![CORE_JSON_TAG_NUMBER];
    invalid_number.extend_from_slice(&1u32.to_le_bytes());
    invalid_number.push(b'+');
    assert_eq!(
      decode_core_json_machine_payload(
        &core_json_machine_for_body(source, &invalid_number),
        source
      ),
      None
    );

    let mut too_deep = Vec::new();
    for _ in 0..=CORE_JSON_MACHINE_MAX_DEPTH {
      too_deep.push(CORE_JSON_TAG_ARRAY);
      too_deep.extend_from_slice(&1u32.to_le_bytes());
    }
    too_deep.push(CORE_JSON_TAG_NULL);
    assert_eq!(
      decode_core_json_machine_payload(&core_json_machine_for_body(source, &too_deep), source),
      None
    );
  }

  #[test]
  fn core_json_machine_test_body_helper_builds_hash_validated_binary_payload() {
    let source = br#"{"name":"rolldown"}"#;
    let body = [CORE_JSON_TAG_NULL];
    let machine_bytes = core_json_machine_for_body(source, &body);

    assert_eq!(
      core_json_machine_payload_body_with_source_hash(&machine_bytes, &blake3::hash(source)),
      Some(body.to_vec())
    );
  }

  #[test]
  fn core_json_machine_header_rejects_stale_source_before_body_decode() {
    let source = br#"{"name":"rolldown"}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();
    let header = &machine_bytes[..CORE_JSON_MACHINE_HEADER_LEN];

    assert!(core_json_machine_header_matches_source_hash(header, &blake3::hash(source)));
    assert!(!core_json_machine_header_matches_source_hash(
      header,
      &blake3::hash(br#"{"name":"changed"}"#)
    ));
  }

  #[test]
  fn core_json_machine_encoder_uses_reader_length_limits() {
    let mut output = Vec::new();

    assert_eq!(push_core_json_len_prefixed_bytes_bounded(&mut output, b"abc", 2), None);
    assert!(output.is_empty());

    assert_eq!(push_core_json_len_prefixed_bytes_bounded(&mut output, b"ab", 2), Some(()));
    assert_eq!(&output[4..], b"ab");
  }

  #[test]
  fn core_json_machine_payload_encoder_rejects_too_deep_array_value() {
    let too_deep = nested_array_value(usize::from(CORE_JSON_MACHINE_MAX_DEPTH) + 1);
    let mut output = Vec::new();

    assert_eq!(core_json_machine_payload_body_capacity(&too_deep), None);
    assert_eq!(encode_core_json_value(&too_deep, &mut output), None);
    assert_eq!(encode_core_json_machine_payload(b"[]", &too_deep), None);
  }

  #[test]
  fn core_json_machine_payload_encoder_allows_max_depth_array_value() {
    let source = b"[]";
    let max_depth = nested_array_value(usize::from(CORE_JSON_MACHINE_MAX_DEPTH));
    let mut output = Vec::new();

    assert!(core_json_machine_payload_body_capacity(&max_depth).is_some());
    assert_eq!(encode_core_json_value(&max_depth, &mut output), Some(()));
    let machine_bytes = encode_core_json_machine_payload(source, &max_depth).unwrap();
    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), Some(max_depth));
  }

  #[test]
  fn core_json_machine_payload_encoder_rejects_too_deep_object_value() {
    let too_deep = nested_object_value(usize::from(CORE_JSON_MACHINE_MAX_DEPTH) + 1);
    let mut output = Vec::new();

    assert_eq!(core_json_machine_payload_body_capacity(&too_deep), None);
    assert_eq!(encode_core_json_value(&too_deep, &mut output), None);
    assert_eq!(encode_core_json_machine_payload(br#"{}"#, &too_deep), None);
  }

  #[test]
  #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
  fn core_json_machine_payload_capacity_matches_encoded_len_for_nested_json() {
    let source = br#"{"items":[1,true,null,"text"],"nested":{"a":1,"b":"two"}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    assert_eq!(
      machine_bytes.len(),
      CORE_JSON_MACHINE_HEADER_LEN + core_json_machine_payload_body_capacity(&json_value).unwrap()
    );
  }

  #[test]
  fn core_json_machine_payload_encode_skips_capacity_prepass() {
    let source = br#"{"items":[1,true,null,"text"],"nested":{"a":1,"b":"two"}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();

    reset_core_json_body_capacity_count();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    assert_eq!(core_json_body_capacity_count(), 0);
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert_eq!(
      machine_bytes.len(),
      CORE_JSON_MACHINE_HEADER_LEN + core_json_machine_payload_body_capacity(&json_value).unwrap()
    );
    assert_eq!(decode_core_json_machine_payload(&machine_bytes, source), Some(json_value));
  }

  #[test]
  fn core_json_machine_binary_value_matches_current_json_ast_output() {
    let source = r#"{"a":1,"a":2,"large":9007199254740995,"items":[true,null,"x"]}"#;
    let json_value = serde_json::from_str::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source.as_bytes(), &json_value).unwrap();
    let decoded_value =
      decode_core_json_machine_payload(&machine_bytes, source.as_bytes()).unwrap();

    let current_ast = json_value_to_ecma_ast(&json_value);
    let cached_ast = json_value_to_ecma_ast(&decoded_value);

    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&current_ast, PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_builds_ast_without_value_decode_api() {
    let source = r#"{"name":"rolldown","nested":{"enabled":true},"items":[1,null,"x"]}"#;
    let json_value = serde_json::from_str::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source.as_bytes(), &json_value).unwrap();

    let current_ast = json_value_to_ecma_ast(&json_value);
    let cached_ast = decode_core_json_machine_payload_to_ecma_ast(
      &machine_bytes,
      &blake3::hash(source.as_bytes()),
    )
    .unwrap();

    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&current_ast, PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_preallocates_ast_collection_vectors() {
    let source = br#"{"items":[1,2,3],"meta":{"enabled":true,"mode":"fast"}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    reset_core_json_ast_vec_capacity_count();
    let cached_ast =
      decode_core_json_machine_payload_to_ecma_ast(&machine_bytes, &blake3::hash(source)).unwrap();

    assert_eq!(core_json_ast_vec_capacity_count(), 3);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_skips_ast_vec_capacity_for_empty_collections() {
    let source = br#"{"emptyArray":[],"emptyObject":{}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    reset_core_json_ast_vec_capacity_count();
    let cached_ast =
      decode_core_json_machine_payload_to_ecma_ast(&machine_bytes, &blake3::hash(source)).unwrap();

    assert_eq!(core_json_ast_vec_capacity_count(), 1);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_uses_vec1_for_singleton_ast_collections() {
    let source = br#"{"singleArray":[1],"singleObject":{"enabled":true}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    reset_core_json_ast_vec_capacity_count();
    let cached_ast =
      decode_core_json_machine_payload_to_ecma_ast(&machine_bytes, &blake3::hash(source)).unwrap();

    assert_eq!(core_json_ast_vec_capacity_count(), 1);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_reads_numeric_ast_payloads() {
    let source = br#"{"numbers":[0,1,-2,3.5,9007199254740995,1.25e-6],"nested":{"answer":42}}"#;
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let machine_bytes = encode_core_json_machine_payload(source, &json_value).unwrap();

    let cached_ast =
      decode_core_json_machine_payload_to_ecma_ast(&machine_bytes, &blake3::hash(source)).unwrap();

    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );
  }

  #[test]
  fn core_json_machine_binary_payload_rejects_malformed_numeric_ast_payloads() {
    let source = br#"{"numbers":[1]}"#;
    for number_bytes in [
      b"+1".as_slice(),
      b"NaN".as_slice(),
      b"inf".as_slice(),
      b"Infinity".as_slice(),
      b"01".as_slice(),
      b"1.".as_slice(),
      b".5".as_slice(),
      b"1e9999".as_slice(),
    ] {
      let mut body = vec![CORE_JSON_TAG_NUMBER];
      body.extend_from_slice(&u32::try_from(number_bytes.len()).unwrap().to_le_bytes());
      body.extend_from_slice(number_bytes);

      assert!(
        decode_core_json_machine_payload_to_ecma_ast(
          &core_json_machine_for_body(source, &body),
          &blake3::hash(source),
        )
        .is_none(),
        "malformed number payload should be rejected: {}",
        std::str::from_utf8(number_bytes).unwrap()
      );
    }
  }

  #[test]
  fn core_json_machine_binary_ast_payload_rejects_impossible_collection_counts_before_vec_alloc() {
    let source = br#"{"items":[1]}"#;
    let impossible_counts = [(CORE_JSON_TAG_ARRAY, "array"), (CORE_JSON_TAG_OBJECT, "object")];

    for (tag, label) in impossible_counts {
      let mut body = vec![tag];
      body.extend_from_slice(&u32::MAX.to_le_bytes());

      reset_core_json_ast_vec_capacity_count();
      assert!(
        decode_core_json_machine_payload_to_ecma_ast(
          &core_json_machine_for_body(source, &body),
          &blake3::hash(source),
        )
        .is_none(),
        "impossible {label} count should be rejected"
      );
      assert_eq!(
        core_json_ast_vec_capacity_count(),
        0,
        "impossible {label} count should fail before AST vector allocation"
      );
    }
  }

  #[test]
  fn core_json_machine_binary_ast_payload_rejects_trailing_expression_bytes() {
    let source = br#"null"#;
    let body = [CORE_JSON_TAG_NULL, CORE_JSON_TAG_TRUE];

    assert!(
      decode_core_json_machine_payload_to_ecma_ast(
        &core_json_machine_for_body(source, &body),
        &blake3::hash(source),
      )
      .is_none()
    );
  }

  #[test]
  fn core_json_machine_cache_ast_reuses_read_validated_body_without_payload_revalidation() {
    let root = unique_temp_root("core-json-ast-validated-body");
    let project_root = root.join("project");
    let source_path = project_root.join("large.json");
    let source = pretty_core_json_source_above_threshold();
    let json_value = serde_json::from_str::<Value>(&source).unwrap();
    let config = DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    write_core_json_machine_cache(&cache_target, source.as_bytes(), &json_value);

    reset_core_json_payload_body_validation_count();
    let cached_ast = read_core_json_machine_cache_ast(&cache_target, source.as_bytes()).unwrap();

    assert_eq!(core_json_payload_body_validation_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_ast_skips_hit_body_rehash_after_machine_validation() {
    let root = unique_temp_root("core-json-ast-skip-hit-body-rehash");
    let project_root = root.join("project");
    let source_path = project_root.join("large.json");
    let source = pretty_core_json_source_above_threshold();
    let json_value = serde_json::from_str::<Value>(&source).unwrap();
    let config = DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    write_core_json_machine_cache(&cache_target, source.as_bytes(), &json_value);

    reset_core_json_body_hash_validation_count();
    let cached_ast = read_core_json_machine_cache_ast(&cache_target, source.as_bytes()).unwrap();

    assert_eq!(core_json_body_hash_validation_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_ast_checks_hit_machine_magic_once() {
    let root = unique_temp_root("core-json-ast-hit-magic-once");
    let project_root = root.join("project");
    let source_path = project_root.join("large.json");
    let source = pretty_core_json_source_above_threshold();
    let json_value = serde_json::from_str::<Value>(&source).unwrap();
    let config = DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    write_core_json_machine_cache(&cache_target, source.as_bytes(), &json_value);

    reset_core_json_machine_magic_count();
    let cached_ast = read_core_json_machine_cache_ast(&cache_target, source.as_bytes()).unwrap();

    assert_eq!(core_json_machine_magic_count(), 1);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_machine_cache_ast_uses_dx_serializer_body_without_value_materialization() {
    let root = unique_temp_root("core-json-ast-dx-direct-body");
    let project_root = root.join("project");
    let source_path = project_root.join("large.json");
    let source = pretty_core_json_source_above_threshold();
    let json_value = serde_json::from_str::<Value>(&source).unwrap();
    let config = DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    write_core_json_machine_cache(&cache_target, source.as_bytes(), &json_value);

    reset_core_json_dx_serializer_body_vec_decode_count();
    let cached_ast = read_core_json_machine_cache_ast(&cache_target, source.as_bytes()).unwrap();

    assert_eq!(core_json_dx_serializer_body_vec_decode_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&json_value_to_ecma_ast(&json_value), PrintOptions::default()).code
    );

    reset_core_json_dx_serializer_body_vec_decode_count();
    assert_eq!(read_core_json_machine_cache(&cache_target, source.as_bytes()), Some(json_value));
    assert_eq!(core_json_dx_serializer_body_vec_decode_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_ast_rejects_corrupt_read_validated_body() {
    let root = unique_temp_root("core-json-ast-corrupt-body");
    let project_root = root.join("project");
    let source_path = project_root.join("large.json");
    let source = pretty_core_json_source_above_threshold();
    let json_value = serde_json::from_str::<Value>(&source).unwrap();
    let config = DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    write_core_json_machine_cache(&cache_target, source.as_bytes(), &json_value);
    let mut machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    let body_index = core_json_machine_magic(&machine_bytes).unwrap().header_len();
    machine_bytes[body_index] ^= 0x01;
    fs::write(&cache_target.paths.machine, machine_bytes).unwrap();

    assert!(read_core_json_machine_cache_ast(&cache_target, source.as_bytes()).is_none());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_writes_and_reads_validated_artifact() {
    let root = unique_temp_root("core-json-cache");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source = pretty_core_json_source_above_threshold();
    let source = source.as_bytes();
    fs::write(&source_path, source).unwrap();
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    write_core_json_machine_cache(&cache_target, source, &json_value);

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert!(machine_bytes.starts_with(CORE_JSON_MACHINE_MAGIC));
    assert!(cache_target.paths.metadata.exists());
    assert_eq!(read_core_json_machine_cache(&cache_target, source), Some(json_value));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_writes_cache_after_current_parse() {
    let root = unique_temp_root("core-json-parse-helper");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    fs::write(&source_path, &source_text).unwrap();
    assert!(source_text.len() >= CORE_JSON_MACHINE_MIN_SOURCE_BYTES);
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    let source = StrOrBytes::Bytes(source_text.as_bytes().to_vec());
    let ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();

    assert!(cache_target.paths.machine.exists());
    let expected_value = serde_json::from_str(&source_text).unwrap();
    assert_eq!(
      read_core_json_machine_cache(&cache_target, source.as_bytes()),
      Some(expected_value)
    );
    let cached_ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&ast, PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_checks_source_worth_once_on_cache_hit() {
    let root = unique_temp_root("core-json-hit-source-worth-once");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    fs::write(&source_path, &source_text).unwrap();
    let source = StrOrBytes::Bytes(source_text.into_bytes());

    parse_json_to_ecma_ast_with_machine_cache(&project_root, &source_path, "data.json", &source)
      .unwrap();

    reset_core_json_source_worth_count();
    parse_json_to_ecma_ast_with_machine_cache(&project_root, &source_path, "data.json", &source)
      .unwrap();

    assert_eq!(core_json_source_worth_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_hit_skips_byte_source_text_conversion() {
    let root = unique_temp_root("core-json-hit-skips-source-text");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    fs::write(&source_path, &source_text).unwrap();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    let current_ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();
    assert!(cache_target.paths.machine.exists());

    reset_core_json_source_text_count();
    let cached_ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();

    assert_eq!(core_json_source_text_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&current_ast, PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_machine_hit_skips_source_json_parse() {
    let root = unique_temp_root("core-json-hit-skips-json-parse");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    fs::write(&source_path, &source_text).unwrap();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    reset_core_json_json_parse_count();
    let current_ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();
    assert_eq!(core_json_json_parse_count(), 1);
    assert!(cache_target.paths.machine.exists());

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    assert!(machine_bytes.starts_with(CORE_JSON_DX_SERIALIZER_ALIGNED_MACHINE_MAGIC));

    reset_core_json_json_parse_count();
    let cached_ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();

    assert_eq!(core_json_json_parse_count(), 0);
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&current_ast, PrintOptions::default()).code
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_miss_parses_byte_source_without_text_conversion() {
    let root = unique_temp_root("core-json-miss-skips-source-text");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    reset_core_json_source_text_count();
    let _ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
        .unwrap();

    assert_eq!(core_json_source_text_count(), 0);
    assert!(cache_target.paths.machine.exists());
    let expected_value = serde_json::from_slice::<Value>(source.as_bytes()).unwrap();
    assert_eq!(
      read_core_json_machine_cache(&cache_target, source.as_bytes()),
      Some(expected_value)
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_skips_tiny_sources() {
    let root = unique_temp_root("core-json-tiny-source");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("tiny.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source = StrOrBytes::Bytes(br#"{ "tiny": true }"#.to_vec());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    let ast =
      parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "tiny.json", &source)
        .unwrap();

    assert!(!cache_target.paths.machine.exists());
    assert_eq!(
      EcmaCompiler::print_with(&ast, PrintOptions::default()).code,
      "({ \"tiny\": true });\n"
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_skips_medium_sources_below_payoff_threshold() {
    let root = unique_temp_root("core-json-medium-source");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("medium.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = format!(r#"{{ "payload": "{}" }}"#, "x".repeat(6 * 1024));
    assert!(source_text.len() > 6 * 1024);
    assert!(source_text.len() < 7 * 1024);
    assert!(source_text.len() < CORE_JSON_MACHINE_MIN_SOURCE_BYTES);
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "medium.json", &source)
      .unwrap();

    assert!(!cache_target.paths.machine.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_skips_sources_below_hot_cache_payoff_threshold() {
    let root = unique_temp_root("core-json-hot-cache-payoff-threshold");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("medium-pretty.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let mut rows = Vec::new();
    let mut source_text = String::new();
    while source_text.len() < 64 * 1024 {
      let index = rows.len();
      rows.push(format!(
        r#"    {{ "id": {index}, "enabled": {}, "weight": {} }}"#,
        index % 2 == 0,
        index * 13
      ));
      source_text = format!("{{\n  \"rows\": [\n{}\n  ]\n}}\n", rows.join(",\n"));
    }
    assert!(source_text.len() > 64 * 1024);
    assert!(source_text.len() < 96 * 1024);
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    parse_json_to_ecma_ast_with_optional_machine_cache(
      Some(&cache_target),
      "medium-pretty.json",
      &source,
    )
    .unwrap();

    assert!(!cache_target.paths.machine.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_hashes_source_once_on_cache_miss_write() {
    let root = unique_temp_root("core-json-cache-miss-single-hash");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    reset_core_json_source_hash_count();

    parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
      .unwrap();

    assert!(cache_target.paths.machine.exists());
    assert_eq!(core_json_source_hash_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_repair_write_reuses_validated_source_hash() {
    let root = unique_temp_root("core-json-repair-reuses-source-hash");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    let stale_machine = b"validated-but-undecodable";
    cache_target
      .config
      .write_machine_artifact(
        &cache_target.paths,
        &cache_target.source_path,
        source.as_bytes(),
        stale_machine,
      )
      .unwrap();

    reset_core_json_source_hash_count();

    parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
      .unwrap();

    assert_eq!(core_json_source_hash_count(), 0);
    assert_ne!(fs::read(&cache_target.paths.machine).unwrap(), stale_machine);
    assert!(read_core_json_machine_cache(&cache_target, source.as_bytes()).is_some());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn core_json_machine_cache_repairs_invalid_aligned_padding_without_source_rehash() {
    let root = unique_temp_root("core-json-invalid-aligned-padding-repair");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = pretty_core_json_source_above_threshold();
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let json_value = serde_json::from_slice::<Value>(source.as_bytes()).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let mut invalid_machine =
      encode_core_json_machine_payload_with_source_hash(&source_hash, &json_value).unwrap();
    invalid_machine[CORE_JSON_MACHINE_HEADER_LEN] = 1;
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };
    cache_target
      .config
      .write_machine_artifact_with_source_hash(
        &cache_target.paths,
        &cache_target.source_path,
        source.as_bytes().len(),
        &source_hash,
        &invalid_machine,
      )
      .unwrap();

    reset_core_json_source_hash_count();

    parse_json_to_ecma_ast_with_optional_machine_cache(Some(&cache_target), "data.json", &source)
      .unwrap();

    let repaired_machine = fs::read(&cache_target.paths.machine).unwrap();
    assert_eq!(core_json_source_hash_count(), 0);
    assert_ne!(repaired_machine, invalid_machine);
    assert_eq!(repaired_machine[CORE_JSON_MACHINE_HEADER_LEN], 0);
    assert!(read_core_json_machine_cache(&cache_target, source.as_bytes()).is_some());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_target_skips_below_threshold_before_path_work() {
    let root = unique_temp_root("core-json-threshold-target");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("tiny.json");
    let source = br#"{"tiny":true}"#;

    reset_core_json_cache_target_build_count();

    assert!(
      core_json_machine_cache_target_for_source(&project_root, &source_path, source).is_none()
    );
    assert_eq!(core_json_cache_target_build_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_cache_target_skips_large_top_level_string_before_path_work() {
    let root = unique_temp_root("core-json-large-string-target");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("large-string.json");
    let source_text = format!(r#""{}""#, "x".repeat(CORE_JSON_MACHINE_MIN_SOURCE_BYTES + 1));

    reset_core_json_cache_target_build_count();

    assert!(
      core_json_machine_cache_target_for_source(
        &project_root,
        &source_path,
        source_text.as_bytes(),
      )
      .is_none()
    );
    assert_eq!(core_json_cache_target_build_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_parse_helper_skips_large_top_level_string_cache() {
    let root = unique_temp_root("core-json-large-string-source");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("large-string.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = format!(r#""{}""#, "x".repeat(CORE_JSON_MACHINE_MIN_SOURCE_BYTES + 1));
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    reset_core_json_source_hash_count();
    parse_json_to_ecma_ast_with_optional_machine_cache(
      Some(&cache_target),
      "large-string.json",
      &source,
    )
    .unwrap();

    assert_eq!(core_json_source_hash_count(), 0);
    assert!(!cache_target.paths.machine.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn core_json_machine_skips_sources_at_payoff_threshold() {
    let root = unique_temp_root("core-json-at-threshold-source");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("threshold.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_prefix = r#"{ "payload": ""#;
    let source_suffix = r#"" }"#;
    let source_text = format!(
      "{source_prefix}{}{source_suffix}",
      "x".repeat(CORE_JSON_MACHINE_MIN_SOURCE_BYTES - source_prefix.len() - source_suffix.len())
    );
    assert_eq!(source_text.len(), CORE_JSON_MACHINE_MIN_SOURCE_BYTES);
    let source = StrOrBytes::Bytes(source_text.into_bytes());
    fs::write(&source_path, source.as_bytes()).unwrap();

    reset_core_json_cache_target_build_count();
    assert!(
      core_json_machine_cache_target_for_source(&project_root, &source_path, source.as_bytes())
        .is_none()
    );
    assert_eq!(core_json_cache_target_build_count(), 0);

    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    reset_core_json_source_hash_count();
    parse_json_to_ecma_ast_with_optional_machine_cache(
      Some(&cache_target),
      "threshold.json",
      &source,
    )
    .unwrap();

    assert_eq!(core_json_source_hash_count(), 0);
    assert!(!cache_target.paths.machine.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  #[ignore = "writes persistent proof artifacts only when ROLLDOWN_DX_CORE_JSON_CACHE_PROOF_DIR is set"]
  fn core_json_dx_serializer_machine_cache_hot_path_writes_proof_receipt_when_requested() {
    let Some(proof_dir) = std::env::var_os("ROLLDOWN_DX_CORE_JSON_CACHE_PROOF_DIR") else { return };
    let proof_dir = PathBuf::from(proof_dir);
    let proof_root = proof_dir.join(format!(
      "core-json-machine-proof-{}-{}",
      std::process::id(),
      SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    let project_root = proof_root.join("project");
    let source_path = project_root.join("src").join("core-cache-proof.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source_text = format!(
      r#"{{ "name": "old", "name": "rolldown", "items": [ {{ "value": 1 }}, {{ "value": 2 }}, {{ "value": 3 }} ], "large": 9007199254740995, "payload": "{}" }}"#,
      "x".repeat(CORE_JSON_MACHINE_MIN_SOURCE_BYTES)
    );
    let source = source_text.as_bytes();
    fs::write(&source_path, source).unwrap();
    let json_value = serde_json::from_slice::<Value>(source).unwrap();
    let canonical_json = serde_json::to_string(&json_value).unwrap();
    let canonical_json_hash = blake3::hash(canonical_json.as_bytes()).to_hex().to_string();
    let config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let paths = config.paths_for_source(&project_root, CORE_JSON_MACHINE_NAMESPACE, &source_path);
    let cache_target =
      CoreJsonMachineCacheTarget { config, paths, source_path: source_path.clone() };

    let source_for_parse = StrOrBytes::Bytes(source.to_vec());
    let current_ast = parse_json_to_ecma_ast_with_optional_machine_cache(
      Some(&cache_target),
      "core-cache-proof.json",
      &source_for_parse,
    )
    .unwrap();

    assert_eq!(read_core_json_machine_cache(&cache_target, source), Some(json_value.clone()));
    reset_core_json_json_parse_count();
    reset_core_json_ast_number_json_parse_count();
    reset_core_json_dx_serializer_body_vec_decode_count();
    reset_core_json_dx_serializer_aligned_copy_count();
    let cached_ast = read_core_json_machine_cache_ast(&cache_target, source).unwrap();
    let hot_cache_json_parse_count = core_json_json_parse_count();
    let hot_cache_ast_number_json_parse_count = core_json_ast_number_json_parse_count();
    let hot_cache_dx_serializer_body_vec_decode_count =
      core_json_dx_serializer_body_vec_decode_count();
    let hot_cache_aligned_copy_count = core_json_dx_serializer_aligned_copy_count();
    assert_eq!(
      EcmaCompiler::print_with(&cached_ast, PrintOptions::default()).code,
      EcmaCompiler::print_with(&current_ast, PrintOptions::default()).code
    );
    assert_eq!(hot_cache_json_parse_count, 0);
    assert_eq!(hot_cache_ast_number_json_parse_count, 0);
    assert_eq!(hot_cache_dx_serializer_body_vec_decode_count, 0);
    assert_eq!(hot_cache_aligned_copy_count, 0);

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    let machine_hit =
      DxMachineCacheHit { machine_bytes: machine_bytes.clone(), source_hash: blake3::hash(source) };
    let (_, machine_body) = core_json_machine_hit_body_parts(&machine_hit).unwrap();
    let metadata_bytes = fs::read(&cache_target.paths.metadata).unwrap();
    let metadata_json = serde_json::from_slice::<serde_json::Value>(&metadata_bytes).unwrap();
    let receipt_path = proof_root.join("core-machine-cache-proof.json");
    let receipt = serde_json::json!({
      "schema": "rolldown.dx.core_json_machine_cache_proof.v1",
      "claimStatus": "local_core_serializer_path_proof_only",
      "speedupClaim": "none",
      "projectRoot": path_for_receipt(&project_root),
      "source": {
        "path": path_for_receipt(&source_path),
        "bytes": source.len(),
        "blake3": blake3::hash(source).to_hex().to_string()
      },
      "machine": {
        "path": path_for_receipt(&cache_target.paths.machine),
        "bytes": machine_bytes.len(),
        "magic": String::from_utf8_lossy(&machine_bytes[..CORE_JSON_MACHINE_MAGIC.len()]),
        "alignedBody": core_json_dx_serializer_body_is_aligned(machine_body),
        "alignedHeaderPaddingBytes": CORE_JSON_DX_SERIALIZER_ALIGNED_HEADER_LEN - CORE_JSON_MACHINE_HEADER_LEN
      },
      "metadata": {
        "sidecar": true,
        "path": path_for_receipt(&cache_target.paths.metadata),
        "bytes": metadata_bytes.len(),
        "schema": metadata_json["schema"].as_str(),
        "sourceBytes": metadata_json["source"]["bytes"].as_u64(),
        "machineBytes": metadata_json["machine"]["bytes"].as_u64(),
      },
      "cacheRead": {
        "hit": true,
        "directAstFromBinaryPayload": true,
        "decodedJsonMatchesCurrentPath": true,
        "canonicalJsonBytes": canonical_json.len(),
        "canonicalJsonBlake3": canonical_json_hash,
        "astPrintMatchesCurrentPath": true
      },
      "hotCache": {
        "jsonParseCount": hot_cache_json_parse_count,
        "astNumberJsonParseCount": hot_cache_ast_number_json_parse_count,
        "dxSerializerBodyVecDecodeCount": hot_cache_dx_serializer_body_vec_decode_count,
        "alignedCopyCount": hot_cache_aligned_copy_count
      }
    });
    assert_eq!(receipt["machine"]["magic"], "RDXCJSR2");
    assert_eq!(receipt["machine"]["alignedBody"], true);
    assert_eq!(receipt["machine"]["alignedHeaderPaddingBytes"], 8);
    assert_eq!(receipt["hotCache"]["jsonParseCount"], 0);
    assert_eq!(receipt["hotCache"]["astNumberJsonParseCount"], 0);
    assert_eq!(receipt["hotCache"]["dxSerializerBodyVecDecodeCount"], 0);
    assert_eq!(receipt["hotCache"]["alignedCopyCount"], 0);
    fs::write(&receipt_path, format!("{}\n", serde_json::to_string_pretty(&receipt).unwrap()))
      .unwrap();

    assert!(receipt_path.exists());
    assert!(cache_target.paths.machine.exists());
  }

  fn unique_temp_root(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-core-json-machine-{label}-{}-{nanos}", std::process::id()))
  }

  fn path_for_receipt(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
  }

  fn pretty_core_json_source_above_threshold() -> String {
    let mut rows = Vec::new();
    let mut source = String::new();
    while source.len() <= CORE_JSON_MACHINE_MIN_SOURCE_BYTES {
      let index = rows.len();
      rows.push(format!(
        r#"    {{ "id": {index}, "enabled": {}, "weight": {} }}"#,
        index % 2 == 0,
        index * 13
      ));
      source = format!(
        "{{\n  \"a\": 1,\n  \"a\": 2,\n  \"items\": [true, null],\n  \"rows\": [\n{}\n  ]\n}}\n",
        rows.join(",\n")
      );
    }
    source
  }

  fn nested_array_value(depth: usize) -> Value {
    let mut value = Value::Null;
    for _ in 0..depth {
      value = Value::Array(vec![value]);
    }
    value
  }

  fn nested_object_value(depth: usize) -> Value {
    let mut value = Value::Null;
    for _ in 0..depth {
      value = serde_json::json!({ "value": value });
    }
    value
  }

  fn core_json_machine_for_body(source: &[u8], body: &[u8]) -> Vec<u8> {
    let mut machine_bytes = Vec::with_capacity(CORE_JSON_MACHINE_HEADER_LEN + body.len());
    machine_bytes.extend_from_slice(CORE_JSON_MACHINE_MAGIC);
    machine_bytes.extend_from_slice(blake3::hash(source).as_bytes());
    machine_bytes.extend_from_slice(blake3::hash(body).as_bytes());
    machine_bytes.extend_from_slice(body);
    machine_bytes
  }
}
