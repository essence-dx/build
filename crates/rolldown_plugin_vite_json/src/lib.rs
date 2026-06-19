mod utils;

use std::{borrow::Cow, path::Path};

use rolldown_common::ModuleType;
use rolldown_plugin::{HookTransformOutput, HookUsage, Plugin};
use rolldown_plugin_utils::{constants, data_to_esm, is_special_query};
use rolldown_sourcemap::SourceMap;
use rolldown_utils::{
  concat_string,
  dx_machine_cache::{DxMachineCacheConfig, DxMachineCachePaths, DxMachineCacheStatus},
};
use serde_json::Value;

const JSON_MODULE_MACHINE_MAGIC: &[u8; 8] = b"RDXJSON2";
const JSON_MODULE_MACHINE_HEADER_LEN: usize = 8 + 4 + 4 + 32;
const JSON_MODULE_MACHINE_SOURCE_HASH_RANGE: std::ops::Range<usize> = 16..48;
const JSON_MODULE_MACHINE_CODEC_RAW: u8 = 0;
const JSON_MODULE_MACHINE_CODEC_LZ4: u8 = 1;
const JSON_MODULE_MACHINE_LZ4_THRESHOLD: usize = 16 * 1024;
const JSON_MODULE_MACHINE_MIN_SOURCE_BYTES: usize = 16 * 1024;
const JSON_MODULE_MACHINE_MAX_GENERATED_LEN: usize = 256 * 1024 * 1024;

#[cfg(test)]
thread_local! {
  static JSON_MODULE_TEST_PAYLOAD_VALIDATION_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_CACHE_PATH_BUILDS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_CACHE_CONFIG_LOOKUPS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_SOURCE_PATH_VIRTUAL_CHECKS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_RAW_OWNED_BUFFER_REUSES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_FROM_STR_VALUE_PARSES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_FROM_SLICE_VALUE_PARSES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_SOURCE_HASH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_LZ4_SIZE_PREPENDED_DECODE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_LZ4_BODY_DECODE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_TRANSFORM_PLAN_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_TRANSFORM_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static JSON_MODULE_TEST_SOURCE_SHAPE_SCANS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

#[derive(Debug, Default)]
pub struct ViteJsonPlugin {
  pub minify: bool,
  pub named_exports: bool,
  pub stringify: ViteJsonPluginStringify,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViteJsonPluginStringify {
  #[default]
  Auto,
  True,
  False,
}

impl ViteJsonPluginStringify {
  fn cache_tag(self) -> u8 {
    match self {
      Self::Auto => 0,
      Self::True => 1,
      Self::False => 2,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JsonModuleMachineOptions {
  named_exports: bool,
  minify: bool,
  stringify: ViteJsonPluginStringify,
}

#[derive(Clone, Copy)]
struct JsonModuleTransformPlan {
  uses_named_exports: bool,
  uses_stringify: bool,
  requires_parse: bool,
  source_is_structured: bool,
}

impl JsonModuleMachineOptions {
  fn new(named_exports: bool, minify: bool, stringify: ViteJsonPluginStringify) -> Self {
    Self { named_exports, minify, stringify }
  }

  fn cache_namespace(self) -> &'static str {
    match (self.named_exports, self.minify, self.stringify) {
      (true, true, ViteJsonPluginStringify::Auto) => {
        "json_module_v2_named_exports_1_minify_1_stringify_auto"
      }
      (true, true, ViteJsonPluginStringify::True) => {
        "json_module_v2_named_exports_1_minify_1_stringify_true"
      }
      (true, true, ViteJsonPluginStringify::False) => {
        "json_module_v2_named_exports_1_minify_1_stringify_false"
      }
      (true, false, ViteJsonPluginStringify::Auto) => {
        "json_module_v2_named_exports_1_minify_0_stringify_auto"
      }
      (true, false, ViteJsonPluginStringify::True) => {
        "json_module_v2_named_exports_1_minify_0_stringify_true"
      }
      (true, false, ViteJsonPluginStringify::False) => {
        "json_module_v2_named_exports_1_minify_0_stringify_false"
      }
      (false, true, ViteJsonPluginStringify::Auto) => {
        "json_module_v2_named_exports_0_minify_1_stringify_auto"
      }
      (false, true, ViteJsonPluginStringify::True) => {
        "json_module_v2_named_exports_0_minify_1_stringify_true"
      }
      (false, true, ViteJsonPluginStringify::False) => {
        "json_module_v2_named_exports_0_minify_1_stringify_false"
      }
      (false, false, ViteJsonPluginStringify::Auto) => {
        "json_module_v2_named_exports_0_minify_0_stringify_auto"
      }
      (false, false, ViteJsonPluginStringify::True) => {
        "json_module_v2_named_exports_0_minify_0_stringify_true"
      }
      (false, false, ViteJsonPluginStringify::False) => {
        "json_module_v2_named_exports_0_minify_0_stringify_false"
      }
    }
  }
}

struct JsonModuleMachineCacheTarget<'a> {
  config: DxMachineCacheConfig,
  source_path: &'a Path,
  paths: DxMachineCachePaths,
}

enum JsonModuleMachineCacheRead {
  Hit(String),
  Repair { source_hash: blake3::Hash },
  Miss,
}

impl Plugin for ViteJsonPlugin {
  fn name(&self) -> Cow<'static, str> {
    Cow::Borrowed("builtin:vite-json")
  }

  async fn transform(
    &self,
    ctx: rolldown_plugin::SharedTransformPluginContext,
    args: &rolldown_plugin::HookTransformArgs<'_>,
  ) -> rolldown_plugin::HookTransformReturn {
    if *args.module_type != ModuleType::Json
      || !utils::is_json_ext(args.id)
      || is_special_query(args.id)
    {
      return Ok(None);
    }

    let raw_code = args.code.as_str();
    let source_bytes = raw_code.as_bytes();
    let options = JsonModuleMachineOptions::new(self.named_exports, self.minify, self.stringify);
    let code = rolldown_plugin_utils::strip_bom(raw_code);
    let transform_plan = json_module_transform_plan(code, &options);
    let cache_target = json_module_machine_cache_target_if_worth_cache(
      ctx.cwd(),
      args.id,
      code,
      transform_plan,
      &options,
    );
    let repair_source_hash = if let Some(cache_target) = &cache_target {
      match read_json_module_machine_cache_for_repair(cache_target, source_bytes, &options) {
        JsonModuleMachineCacheRead::Hit(generated_esm) => {
          return Ok(Some(json_transform_output(generated_esm)));
        }
        JsonModuleMachineCacheRead::Repair { source_hash } => Some(source_hash),
        JsonModuleMachineCacheRead::Miss => None,
      }
    } else {
      None
    };

    let generated_esm = transform_json_module_with_plan(code, &options, transform_plan)?;
    if let Some(cache_target) = &cache_target {
      write_json_module_machine_cache_with_source_hash(
        cache_target,
        source_bytes,
        &options,
        &generated_esm,
        repair_source_hash.as_ref(),
      );
    }

    Ok(Some(json_transform_output(generated_esm)))
  }

  fn register_hook_usage(&self) -> rolldown_plugin::HookUsage {
    HookUsage::Transform
  }
}

#[cfg(test)]
fn transform_json_module(
  code: &str,
  options: &JsonModuleMachineOptions,
) -> Result<String, serde_json::Error> {
  transform_json_module_with_plan(code, options, json_module_transform_plan(code, options))
}

fn transform_json_module_with_plan(
  code: &str,
  options: &JsonModuleMachineOptions,
  plan: JsonModuleTransformPlan,
) -> Result<String, serde_json::Error> {
  #[cfg(test)]
  JSON_MODULE_TEST_TRANSFORM_CALLS.with(|calls| calls.set(calls.get() + 1));

  if !plan.uses_named_exports && plan.uses_stringify {
    let json = if options.minify {
      // TODO(perf): find better way than https://github.com/rolldown/vite/blob/3bf86e3f/packages/vite/src/node/plugins/json.ts#L55-L57
      let value = parse_json_value(code)?;
      Cow::Owned(serde_json::to_string(&value)?)
    } else {
      Cow::Borrowed(code)
    };

    return Ok(concat_string!(
      "export default /*#__PURE__*/ JSON.parse(",
      serde_json::to_string(&json)?,
      ")"
    ));
  }

  let value = parse_json_value(code)?;
  Ok(data_to_esm(&value, options.named_exports))
}

fn json_module_transform_plan(
  code: &str,
  options: &JsonModuleMachineOptions,
) -> JsonModuleTransformPlan {
  #[cfg(test)]
  JSON_MODULE_TEST_TRANSFORM_PLAN_CALLS.with(|calls| calls.set(calls.get() + 1));

  let first_non_whitespace_byte = json_module_first_non_whitespace_byte(code);
  let uses_named_exports = options.named_exports && first_non_whitespace_byte == Some(b'{');
  let uses_stringify = json_module_uses_stringify(code, options);
  JsonModuleTransformPlan {
    uses_named_exports,
    uses_stringify,
    requires_parse: uses_named_exports || !uses_stringify || options.minify,
    source_is_structured: matches!(first_non_whitespace_byte, Some(b'{' | b'[')),
  }
}

fn json_module_first_non_whitespace_byte(code: &str) -> Option<u8> {
  #[cfg(test)]
  JSON_MODULE_TEST_SOURCE_SHAPE_SCANS.with(|calls| calls.set(calls.get() + 1));

  code.as_bytes().iter().copied().find(|byte| !byte.is_ascii_whitespace())
}

fn json_module_uses_stringify(code: &str, options: &JsonModuleMachineOptions) -> bool {
  options.stringify != ViteJsonPluginStringify::False
    && (options.stringify == ViteJsonPluginStringify::True
      || code.len() > constants::THRESHOLD_SIZE)
}

fn parse_json_value(code: &str) -> Result<Value, serde_json::Error> {
  #[cfg(test)]
  JSON_MODULE_TEST_FROM_SLICE_VALUE_PARSES.with(|calls| calls.set(calls.get() + 1));

  serde_json::from_slice(code.as_bytes())
}

fn json_module_machine_cache_target<'id>(
  project_root: &Path,
  id: &'id str,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'id>> {
  let source_path = json_module_source_path(id);
  if json_module_machine_source_path_is_virtual(source_path) {
    return None;
  }
  let cache_config = json_module_machine_cache_config_from_env(project_root)?;
  json_module_machine_cache_target_from_non_virtual_source_path(
    cache_config,
    project_root,
    source_path,
    options,
  )
}

#[cfg(test)]
fn json_module_machine_cache_target_with_config<'id>(
  cache_config: DxMachineCacheConfig,
  project_root: &Path,
  id: &'id str,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'id>> {
  let source_path = json_module_source_path(id);
  if json_module_machine_source_path_is_virtual(source_path) {
    return None;
  }
  json_module_machine_cache_target_from_non_virtual_source_path(
    cache_config,
    project_root,
    source_path,
    options,
  )
}

fn json_module_machine_cache_config_from_env(project_root: &Path) -> Option<DxMachineCacheConfig> {
  #[cfg(test)]
  JSON_MODULE_TEST_CACHE_CONFIG_LOOKUPS.with(|calls| calls.set(calls.get() + 1));

  DxMachineCacheConfig::from_env_if_enabled(project_root)
}

fn json_module_machine_cache_target_if_worth_cache<'id>(
  project_root: &Path,
  id: &'id str,
  code: &str,
  transform_plan: JsonModuleTransformPlan,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'id>> {
  if !json_module_source_worth_machine_cache_with_plan(code, transform_plan) {
    return None;
  }
  json_module_machine_cache_target(project_root, id, options)
}

#[cfg(test)]
fn json_module_machine_cache_target_from_source_path<'source>(
  cache_config: DxMachineCacheConfig,
  project_root: &Path,
  source_path: &'source Path,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'source>> {
  if json_module_machine_source_path_is_virtual(source_path) {
    return None;
  }
  json_module_machine_cache_target_from_non_virtual_source_path(
    cache_config,
    project_root,
    source_path,
    options,
  )
}

fn json_module_machine_cache_target_from_non_virtual_source_path<'source>(
  cache_config: DxMachineCacheConfig,
  project_root: &Path,
  source_path: &'source Path,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'source>> {
  let paths =
    json_module_machine_cache_paths_if_enabled(&cache_config, project_root, source_path, options)?;
  Some(JsonModuleMachineCacheTarget { config: cache_config, source_path, paths })
}

fn json_module_machine_source_path_is_virtual(source_path: &Path) -> bool {
  #[cfg(test)]
  JSON_MODULE_TEST_SOURCE_PATH_VIRTUAL_CHECKS.with(|calls| calls.set(calls.get() + 1));

  source_path.as_os_str().as_encoded_bytes().contains(&0)
}

#[cfg(test)]
fn json_module_machine_cache_target_from_source_path_if_worth_cache<'source>(
  cache_config: DxMachineCacheConfig,
  project_root: &Path,
  source_path: &'source Path,
  code: &str,
  options: &JsonModuleMachineOptions,
) -> Option<JsonModuleMachineCacheTarget<'source>> {
  let transform_plan = json_module_transform_plan(code, options);
  if !json_module_source_worth_machine_cache_with_plan(code, transform_plan) {
    return None;
  }
  json_module_machine_cache_target_from_source_path(
    cache_config,
    project_root,
    source_path,
    options,
  )
}

fn json_module_source_len_worth_machine_cache(source_len: usize) -> bool {
  source_len > JSON_MODULE_MACHINE_MIN_SOURCE_BYTES
}

fn json_module_source_worth_machine_cache_with_plan(
  code: &str,
  transform_plan: JsonModuleTransformPlan,
) -> bool {
  json_module_source_len_worth_machine_cache(code.len())
    && transform_plan.requires_parse
    && transform_plan.source_is_structured
}

#[cfg(test)]
fn read_json_module_machine_cache(
  cache_target: &JsonModuleMachineCacheTarget,
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
) -> Option<String> {
  match read_json_module_machine_cache_for_repair(cache_target, source_bytes, options) {
    JsonModuleMachineCacheRead::Hit(generated_esm) => Some(generated_esm),
    JsonModuleMachineCacheRead::Repair { .. } | JsonModuleMachineCacheRead::Miss => None,
  }
}

fn read_json_module_machine_cache_for_repair(
  cache_target: &JsonModuleMachineCacheTarget,
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
) -> JsonModuleMachineCacheRead {
  let DxMachineCacheStatus::Hit(hit) = cache_target.config.read_validated_machine_with_source_hash(
    &cache_target.paths,
    cache_target.source_path,
    source_bytes,
  ) else {
    return JsonModuleMachineCacheRead::Miss;
  };

  match decode_json_module_machine_payload_with_owned_bytes(
    hit.machine_bytes,
    &hit.source_hash,
    options,
  ) {
    Some(generated_esm) => JsonModuleMachineCacheRead::Hit(generated_esm),
    None => JsonModuleMachineCacheRead::Repair { source_hash: hit.source_hash },
  }
}

#[cfg(test)]
fn write_json_module_machine_cache(
  cache_target: &JsonModuleMachineCacheTarget,
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
  generated_esm: &str,
) {
  write_json_module_machine_cache_with_source_hash(
    cache_target,
    source_bytes,
    options,
    generated_esm,
    None,
  );
}

fn write_json_module_machine_cache_with_source_hash(
  cache_target: &JsonModuleMachineCacheTarget,
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
  generated_esm: &str,
  source_hash: Option<&blake3::Hash>,
) {
  let Some((generated_len, codec, payload)) =
    encode_json_module_payload_bytes_if_beneficial(source_bytes.len(), generated_esm)
  else {
    return;
  };
  let computed_source_hash;
  let source_hash = match source_hash {
    Some(source_hash) => source_hash,
    None => {
      computed_source_hash = hash_json_module_source_bytes(source_bytes);
      &computed_source_hash
    }
  };
  let Some(machine_bytes) = encode_json_module_machine_payload_from_encoded_payload(
    source_hash,
    options,
    generated_len,
    codec,
    payload.as_ref(),
  ) else {
    return;
  };

  let _ = cache_target.config.write_machine_artifact_with_source_hash(
    &cache_target.paths,
    cache_target.source_path,
    source_bytes.len(),
    source_hash,
    &machine_bytes,
  );
}

fn hash_json_module_source_bytes(source_bytes: &[u8]) -> blake3::Hash {
  #[cfg(test)]
  JSON_MODULE_TEST_SOURCE_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  blake3::hash(source_bytes)
}

fn json_module_machine_cache_paths_if_enabled(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  source_path: &Path,
  options: &JsonModuleMachineOptions,
) -> Option<DxMachineCachePaths> {
  if !cache_config.enabled {
    return None;
  }

  #[cfg(test)]
  JSON_MODULE_TEST_CACHE_PATH_BUILDS.with(|calls| calls.set(calls.get() + 1));

  Some(cache_config.paths_for_source(project_root, options.cache_namespace(), source_path))
}

#[cfg(test)]
fn encode_json_module_machine_payload(
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
  generated_esm: &str,
) -> Option<Vec<u8>> {
  encode_json_module_machine_payload_with_source_hash(
    &blake3::hash(source_bytes),
    options,
    generated_esm,
  )
}

fn encode_json_module_machine_payload_with_source_hash(
  source_hash: &blake3::Hash,
  options: &JsonModuleMachineOptions,
  generated_esm: &str,
) -> Option<Vec<u8>> {
  let generated_bytes = generated_esm.as_bytes();
  let generated_len = generated_bytes.len();
  if !json_module_generated_len_within_machine_budget(generated_len) {
    return None;
  }
  let (codec, payload) = encode_json_module_payload_bytes(generated_bytes);
  encode_json_module_machine_payload_from_encoded_payload(
    source_hash,
    options,
    generated_len,
    codec,
    payload.as_ref(),
  )
}

fn encode_json_module_machine_payload_from_encoded_payload(
  source_hash: &blake3::Hash,
  options: &JsonModuleMachineOptions,
  generated_len: usize,
  codec: u8,
  payload: &[u8],
) -> Option<Vec<u8>> {
  if !json_module_generated_len_within_machine_budget(generated_len) {
    return None;
  }
  let generated_len = u32::try_from(generated_len).ok()?;
  let mut machine_bytes = Vec::with_capacity(JSON_MODULE_MACHINE_HEADER_LEN + payload.len());
  machine_bytes.extend_from_slice(JSON_MODULE_MACHINE_MAGIC);
  machine_bytes.push(u8::from(options.named_exports));
  machine_bytes.push(u8::from(options.minify));
  machine_bytes.push(options.stringify.cache_tag());
  machine_bytes.push(codec);
  machine_bytes.extend_from_slice(&generated_len.to_le_bytes());
  machine_bytes.extend_from_slice(source_hash.as_bytes());
  machine_bytes.extend_from_slice(&payload);
  Some(machine_bytes)
}

fn encode_json_module_payload_bytes_if_beneficial(
  source_len: usize,
  generated_esm: &str,
) -> Option<(usize, u8, Cow<'_, [u8]>)> {
  let generated_bytes = generated_esm.as_bytes();
  let generated_len = generated_bytes.len();
  if !json_module_generated_len_within_machine_budget(generated_len) {
    return None;
  }
  let (codec, payload) = encode_json_module_payload_bytes(generated_bytes);
  json_module_machine_len_benefits_source(source_len, payload.len()).then_some((
    generated_len,
    codec,
    payload,
  ))
}

#[cfg(test)]
fn decode_json_module_machine_payload(
  machine_bytes: &[u8],
  source_bytes: &[u8],
  options: &JsonModuleMachineOptions,
) -> Option<String> {
  let source_hash = blake3::hash(source_bytes);
  decode_json_module_machine_payload_with_source_hash(machine_bytes, &source_hash, options)
}

#[cfg(test)]
fn decode_json_module_machine_payload_with_source_hash(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
  options: &JsonModuleMachineOptions,
) -> Option<String> {
  let (codec, generated_len) =
    json_module_machine_payload_header(machine_bytes, source_hash, options)?;
  let payload = json_module_validated_payload(
    codec,
    machine_bytes.get(JSON_MODULE_MACHINE_HEADER_LEN..)?,
    generated_len,
  )?;

  match payload {
    ValidatedJsonModuleMachinePayload::Raw(payload) => decode_raw_json_module_payload(payload),
    ValidatedJsonModuleMachinePayload::Lz4 { compressed, generated_len } => {
      decode_lz4_json_module_payload(compressed, generated_len)
    }
  }
}

fn decode_json_module_machine_payload_with_owned_bytes(
  mut machine_bytes: Vec<u8>,
  source_hash: &blake3::Hash,
  options: &JsonModuleMachineOptions,
) -> Option<String> {
  let (codec, generated_len) =
    json_module_machine_payload_header(&machine_bytes, source_hash, options)?;
  if codec == JSON_MODULE_MACHINE_CODEC_RAW {
    let payload_start = JSON_MODULE_MACHINE_HEADER_LEN;
    if !json_module_generated_len_within_machine_budget(generated_len) {
      return None;
    }
    let expected_machine_len = payload_start.checked_add(generated_len)?;
    if machine_bytes.len() != expected_machine_len {
      return None;
    }
    std::str::from_utf8(machine_bytes.get(payload_start..expected_machine_len)?).ok()?;

    #[cfg(test)]
    JSON_MODULE_TEST_RAW_OWNED_BUFFER_REUSES.with(|calls| calls.set(calls.get() + 1));

    machine_bytes.copy_within(payload_start..expected_machine_len, 0);
    machine_bytes.truncate(generated_len);
    // SAFETY: the payload slice was UTF-8 checked before moving it to the front.
    return Some(unsafe { String::from_utf8_unchecked(machine_bytes) });
  }

  if codec != JSON_MODULE_MACHINE_CODEC_LZ4 {
    return None;
  }

  let compressed = machine_bytes.get(JSON_MODULE_MACHINE_HEADER_LEN..)?;
  if !json_module_lz4_payload_size_matches_header(compressed, generated_len) {
    return None;
  }
  decode_lz4_json_module_payload(compressed, generated_len)
}

fn json_module_machine_payload_header(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
  options: &JsonModuleMachineOptions,
) -> Option<(u8, usize)> {
  if machine_bytes.len() < JSON_MODULE_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..JSON_MODULE_MACHINE_MAGIC.len()] != JSON_MODULE_MACHINE_MAGIC {
    return None;
  }
  if machine_bytes[8] != u8::from(options.named_exports)
    || machine_bytes[9] != u8::from(options.minify)
    || machine_bytes[10] != options.stringify.cache_tag()
  {
    return None;
  }

  if machine_bytes[JSON_MODULE_MACHINE_SOURCE_HASH_RANGE] != source_hash.as_bytes()[..] {
    return None;
  }

  let generated_len = u32::from_le_bytes(machine_bytes[12..16].try_into().ok()?) as usize;
  Some((machine_bytes[11], generated_len))
}

#[cfg(test)]
fn decode_raw_json_module_payload(payload: &[u8]) -> Option<String> {
  std::str::from_utf8(payload).ok().map(str::to_owned)
}

fn decode_lz4_json_module_payload(compressed: &[u8], generated_len: usize) -> Option<String> {
  #[cfg(test)]
  JSON_MODULE_TEST_LZ4_BODY_DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));

  let compressed_body = compressed.get(4..)?;
  let decompressed = lz4_flex::decompress(compressed_body, generated_len).ok()?;
  if decompressed.len() != generated_len {
    return None;
  }
  String::from_utf8(decompressed).ok()
}

#[cfg(test)]
enum ValidatedJsonModuleMachinePayload<'a> {
  Raw(&'a [u8]),
  Lz4 { compressed: &'a [u8], generated_len: usize },
}

#[cfg(test)]
fn json_module_validated_payload(
  codec: u8,
  payload: &[u8],
  generated_len: usize,
) -> Option<ValidatedJsonModuleMachinePayload<'_>> {
  #[cfg(test)]
  JSON_MODULE_TEST_PAYLOAD_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));

  if !json_module_generated_len_within_machine_budget(generated_len) {
    return None;
  }

  match codec {
    JSON_MODULE_MACHINE_CODEC_RAW if payload.len() == generated_len => {
      Some(ValidatedJsonModuleMachinePayload::Raw(payload))
    }
    JSON_MODULE_MACHINE_CODEC_LZ4
      if json_module_lz4_payload_size_prefix_matches_header(payload, generated_len) =>
    {
      Some(ValidatedJsonModuleMachinePayload::Lz4 { compressed: payload, generated_len })
    }
    _ => None,
  }
}

#[cfg(test)]
fn json_module_payload_shape_matches_header(
  codec: u8,
  payload: &[u8],
  generated_len: usize,
) -> bool {
  json_module_validated_payload(codec, payload, generated_len).is_some()
}

fn json_module_lz4_payload_size_matches_header(payload: &[u8], generated_len: usize) -> bool {
  if !json_module_generated_len_within_machine_budget(generated_len) {
    return false;
  }
  json_module_lz4_payload_size_prefix_matches_header(payload, generated_len)
}

fn json_module_lz4_payload_size_prefix_matches_header(
  payload: &[u8],
  generated_len: usize,
) -> bool {
  let Some(prefix) = payload.get(..4) else {
    return false;
  };
  let Ok(prefix) = <[u8; 4]>::try_from(prefix) else {
    return false;
  };
  u32::from_le_bytes(prefix) as usize == generated_len
}

fn json_module_generated_len_within_machine_budget(generated_len: usize) -> bool {
  generated_len <= JSON_MODULE_MACHINE_MAX_GENERATED_LEN
}

fn json_module_machine_len_benefits_source(source_len: usize, payload_len: usize) -> bool {
  JSON_MODULE_MACHINE_HEADER_LEN.checked_add(payload_len).is_some_and(|len| len < source_len)
}

fn encode_json_module_payload_bytes(generated_bytes: &[u8]) -> (u8, Cow<'_, [u8]>) {
  if generated_bytes.len() < JSON_MODULE_MACHINE_LZ4_THRESHOLD {
    return (JSON_MODULE_MACHINE_CODEC_RAW, Cow::Borrowed(generated_bytes));
  }

  let compressed = lz4_flex::compress_prepend_size(generated_bytes);
  if compressed.len() < generated_bytes.len() {
    (JSON_MODULE_MACHINE_CODEC_LZ4, Cow::Owned(compressed))
  } else {
    (JSON_MODULE_MACHINE_CODEC_RAW, Cow::Borrowed(generated_bytes))
  }
}

fn json_transform_output(code: String) -> HookTransformOutput {
  HookTransformOutput {
    code: Some(code),
    map: SourceMap::default().into(),
    module_type: Some(ModuleType::Js),
    ..Default::default()
  }
}

fn json_module_source_path(id: &str) -> &Path {
  let path = memchr::memchr(b'?', id.as_bytes()).map_or(id, |query_start| &id[..query_start]);
  Path::new(path)
}

#[cfg(test)]
mod json_machine_cache_tests {
  use super::*;
  use rolldown_utils::dx_machine_cache::DxMachineCacheConfig;
  use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::Instant,
    time::{SystemTime, UNIX_EPOCH},
  };

  fn reset_payload_validation_call_count() {
    JSON_MODULE_TEST_PAYLOAD_VALIDATION_CALLS.with(|calls| calls.set(0));
  }

  fn payload_validation_call_count() -> u64 {
    JSON_MODULE_TEST_PAYLOAD_VALIDATION_CALLS.with(|calls| calls.get())
  }

  fn reset_cache_path_build_count() {
    JSON_MODULE_TEST_CACHE_PATH_BUILDS.with(|calls| calls.set(0));
  }

  fn cache_path_build_count() -> u64 {
    JSON_MODULE_TEST_CACHE_PATH_BUILDS.with(|calls| calls.get())
  }

  fn reset_cache_config_lookup_count() {
    JSON_MODULE_TEST_CACHE_CONFIG_LOOKUPS.with(|calls| calls.set(0));
  }

  fn cache_config_lookup_count() -> u64 {
    JSON_MODULE_TEST_CACHE_CONFIG_LOOKUPS.with(|calls| calls.get())
  }

  fn reset_source_path_virtual_check_count() {
    JSON_MODULE_TEST_SOURCE_PATH_VIRTUAL_CHECKS.with(|calls| calls.set(0));
  }

  fn source_path_virtual_check_count() -> u64 {
    JSON_MODULE_TEST_SOURCE_PATH_VIRTUAL_CHECKS.with(|calls| calls.get())
  }

  fn reset_raw_owned_buffer_reuse_count() {
    JSON_MODULE_TEST_RAW_OWNED_BUFFER_REUSES.with(|calls| calls.set(0));
  }

  fn raw_owned_buffer_reuse_count() -> u64 {
    JSON_MODULE_TEST_RAW_OWNED_BUFFER_REUSES.with(|calls| calls.get())
  }

  fn reset_from_str_value_parse_count() {
    JSON_MODULE_TEST_FROM_STR_VALUE_PARSES.with(|calls| calls.set(0));
  }

  fn from_str_value_parse_count() -> u64 {
    JSON_MODULE_TEST_FROM_STR_VALUE_PARSES.with(|calls| calls.get())
  }

  fn reset_from_slice_value_parse_count() {
    JSON_MODULE_TEST_FROM_SLICE_VALUE_PARSES.with(|calls| calls.set(0));
  }

  fn from_slice_value_parse_count() -> u64 {
    JSON_MODULE_TEST_FROM_SLICE_VALUE_PARSES.with(|calls| calls.get())
  }

  fn reset_json_module_source_hash_count() {
    JSON_MODULE_TEST_SOURCE_HASH_CALLS.with(|calls| calls.set(0));
  }

  fn json_module_source_hash_count() -> u64 {
    JSON_MODULE_TEST_SOURCE_HASH_CALLS.with(|calls| calls.get())
  }

  fn reset_lz4_size_prepended_decode_count() {
    JSON_MODULE_TEST_LZ4_SIZE_PREPENDED_DECODE_CALLS.with(|calls| calls.set(0));
  }

  fn lz4_size_prepended_decode_count() -> u64 {
    JSON_MODULE_TEST_LZ4_SIZE_PREPENDED_DECODE_CALLS.with(|calls| calls.get())
  }

  fn reset_lz4_body_decode_count() {
    JSON_MODULE_TEST_LZ4_BODY_DECODE_CALLS.with(|calls| calls.set(0));
  }

  fn lz4_body_decode_count() -> u64 {
    JSON_MODULE_TEST_LZ4_BODY_DECODE_CALLS.with(|calls| calls.get())
  }

  fn reset_transform_plan_count() {
    JSON_MODULE_TEST_TRANSFORM_PLAN_CALLS.with(|calls| calls.set(0));
  }

  fn transform_plan_count() -> u64 {
    JSON_MODULE_TEST_TRANSFORM_PLAN_CALLS.with(|calls| calls.get())
  }

  fn reset_transform_count() {
    JSON_MODULE_TEST_TRANSFORM_CALLS.with(|calls| calls.set(0));
  }

  fn transform_count() -> u64 {
    JSON_MODULE_TEST_TRANSFORM_CALLS.with(|calls| calls.get())
  }

  fn reset_source_shape_scan_count() {
    JSON_MODULE_TEST_SOURCE_SHAPE_SCANS.with(|calls| calls.set(0));
  }

  fn source_shape_scan_count() -> u64 {
    JSON_MODULE_TEST_SOURCE_SHAPE_SCANS.with(|calls| calls.get())
  }

  fn enabled_json_module_cache_target<'source>(
    project_root: &std::path::Path,
    source_path: &'source std::path::Path,
    options: &JsonModuleMachineOptions,
  ) -> JsonModuleMachineCacheTarget<'source> {
    let cache_config =
      DxMachineCacheConfig::from_env_value(project_root, Some(OsString::from("1")));
    json_module_machine_cache_target_from_source_path(
      cache_config,
      project_root,
      source_path,
      options,
    )
    .unwrap()
  }

  fn whitespace_padded_json_object() -> String {
    format!(
      "{{\n{}\n\"name\":\"rolldown\"\n}}",
      " ".repeat(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 256)
    )
  }

  #[test]
  fn json_machine_cache_reuses_precomputed_paths_for_miss_write_hit() {
    let root = unique_temp_root("precomputed-paths");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source_text = whitespace_padded_json_object();
    let source = source_text.as_bytes();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(&source_text, &options).unwrap();

    reset_cache_path_build_count();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);
    assert_eq!(read_json_module_machine_cache(&cache_target, source, &options), None);
    write_json_module_machine_cache(&cache_target, source, &options, &generated);
    assert_eq!(cache_path_build_count(), 1);

    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);
    assert_eq!(read_json_module_machine_cache(&cache_target, source, &options), Some(generated));
    assert_eq!(cache_path_build_count(), 2);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_source_path_borrows_id_after_query_strip() {
    fn borrowed_source_path<'id>(id: &'id str) -> &'id Path {
      json_module_source_path(id)
    }

    let id = r"G:\Dx\build\src\data.json?import";
    let expected = r"G:\Dx\build\src\data.json";
    let source_path = borrowed_source_path(id);
    let borrowed_bytes = source_path.as_os_str().as_encoded_bytes();

    assert_eq!(source_path, Path::new(expected));
    assert_eq!(borrowed_bytes, expected.as_bytes());
    assert!(std::ptr::eq(borrowed_bytes.as_ptr(), id.as_ptr()));

    let id_without_query = r"G:\Dx\build\src\plain.json";
    let source_path = borrowed_source_path(id_without_query);
    let borrowed_bytes = source_path.as_os_str().as_encoded_bytes();

    assert_eq!(source_path, Path::new(id_without_query));
    assert_eq!(borrowed_bytes, id_without_query.as_bytes());
    assert!(std::ptr::eq(borrowed_bytes.as_ptr(), id_without_query.as_ptr()));

    let unicode_id = "G:\\Dx\\build\\src\\データ.json?import";
    let unicode_expected = "G:\\Dx\\build\\src\\データ.json";
    let source_path = borrowed_source_path(unicode_id);
    let borrowed_bytes = source_path.as_os_str().as_encoded_bytes();

    assert_eq!(source_path, Path::new(unicode_expected));
    assert_eq!(borrowed_bytes, unicode_expected.as_bytes());
    assert!(std::ptr::eq(borrowed_bytes.as_ptr(), unicode_id.as_ptr()));
  }

  #[test]
  fn json_machine_cache_round_trips_generated_esm() {
    let root = unique_temp_root("round-trip");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source_text = whitespace_padded_json_object();
    let source = source_text.as_bytes();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(&source_text, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source, &options, &generated);

    assert_eq!(read_json_module_machine_cache(&cache_target, source, &options), Some(generated));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_warm_hit_skips_json_transform_work() {
    let root = unique_temp_root("warm-hit-skip-transform");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source = large_json_object(2_000);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, &source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(&source, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source.as_bytes(), &options, &generated);

    reset_transform_count();
    reset_from_slice_value_parse_count();
    assert_eq!(
      read_json_module_machine_cache(&cache_target, source.as_bytes(), &options),
      Some(generated)
    );
    assert_eq!(transform_count(), 0);
    assert_eq!(from_slice_value_parse_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_warm_lz4_hit_decodes_once_without_transform_work() {
    let root = unique_temp_root("warm-lz4-hit-decode-once");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("large.json");
    let source = large_json_object(4_000);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, &source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let generated = transform_json_module(&source, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source.as_bytes(), &options, &generated);
    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_LZ4);

    reset_transform_count();
    reset_from_slice_value_parse_count();
    reset_payload_validation_call_count();
    reset_lz4_size_prepended_decode_count();
    reset_lz4_body_decode_count();
    assert_eq!(
      read_json_module_machine_cache(&cache_target, source.as_bytes(), &options),
      Some(generated)
    );
    assert_eq!(transform_count(), 0);
    assert_eq!(from_slice_value_parse_count(), 0);
    assert_eq!(payload_validation_call_count(), 0);
    assert_eq!(lz4_size_prepended_decode_count(), 0);
    assert_eq!(lz4_body_decode_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_reuses_transform_plan_for_cache_target_and_transform() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = large_json_object(2_000);

    reset_transform_plan_count();
    let plan = json_module_transform_plan(&source, &options);

    assert!(json_module_source_worth_machine_cache_with_plan(&source, plan));
    assert_eq!(transform_json_module_with_plan(&source, &options, plan).is_ok(), true);
    assert_eq!(transform_plan_count(), 1);
  }

  #[test]
  fn json_module_transform_plan_reuses_single_source_shape_scan() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    reset_source_shape_scan_count();
    let plan = json_module_transform_plan(r#"{"name":"rolldown"}"#, &options);
    assert!(plan.uses_named_exports);
    assert!(plan.source_is_structured);
    assert_eq!(source_shape_scan_count(), 1);

    reset_source_shape_scan_count();
    let plan = json_module_transform_plan("  {\"name\":\"rolldown\"}", &options);
    assert!(plan.uses_named_exports);
    assert!(plan.source_is_structured);
    assert_eq!(source_shape_scan_count(), 1);
  }

  #[test]
  fn json_machine_cache_writes_compact_binary_payload() {
    let root = unique_temp_root("binary-payload");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source_text = whitespace_padded_json_object();
    let source = source_text.as_bytes();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(&source_text, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source, &options, &generated);

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    assert!(machine_bytes.starts_with(JSON_MODULE_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));
    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_RAW);
    assert_eq!(read_json_module_machine_cache(&cache_target, source, &options), Some(generated));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_write_reuses_precomputed_source_hash_for_payload_and_metadata() {
    let root = unique_temp_root("known-source-hash-write");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source_text = whitespace_padded_json_object();
    let source = source_text.as_bytes();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(&source_text, &options).unwrap();
    let source_hash = blake3::hash(source);
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    reset_json_module_source_hash_count();
    write_json_module_machine_cache(&cache_target, source, &options, &generated);

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    let metadata = fs::read_to_string(&cache_target.paths.metadata).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();

    assert_eq!(&machine_bytes[JSON_MODULE_MACHINE_SOURCE_HASH_RANGE], source_hash.as_bytes());
    assert_eq!(metadata["source"]["bytes"].as_u64(), Some(source.len() as u64));
    assert_eq!(metadata["source"]["blake3"].as_str(), Some(source_hash.to_hex().as_str()));
    assert_eq!(json_module_source_hash_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_write_skips_non_beneficial_payload_before_source_hash() {
    let root = unique_temp_root("non-beneficial-write-no-source-hash");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source = json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, &source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = "x".repeat(JSON_MODULE_MACHINE_LZ4_THRESHOLD - 1);
    assert!(!json_module_machine_len_benefits_source(source.len(), generated.len()));
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    reset_json_module_source_hash_count();
    write_json_module_machine_cache(&cache_target, source.as_bytes(), &options, &generated);

    assert_eq!(json_module_source_hash_count(), 0);
    assert!(!cache_target.paths.machine.exists());
    assert!(!cache_target.paths.metadata.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_repair_write_reuses_validated_source_hash() {
    let root = unique_temp_root("repair-known-source-hash");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source_text = whitespace_padded_json_object();
    let source = source_text.as_bytes();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let read_options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let stale_options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::False);
    let stale_generated = transform_json_module(&source_text, &stale_options).unwrap();
    let source_hash = blake3::hash(source);
    let stale_machine_bytes = encode_json_module_machine_payload_with_source_hash(
      &source_hash,
      &stale_options,
      &stale_generated,
    )
    .unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &read_options);
    cache_target
      .config
      .write_machine_artifact_with_source_hash(
        &cache_target.paths,
        cache_target.source_path,
        source.len(),
        &source_hash,
        &stale_machine_bytes,
      )
      .unwrap();

    let JsonModuleMachineCacheRead::Repair { source_hash: validated_source_hash } =
      read_json_module_machine_cache_for_repair(&cache_target, source, &read_options)
    else {
      panic!("expected validated repair source hash");
    };

    assert_eq!(validated_source_hash, source_hash);

    let repaired_generated = transform_json_module(&source_text, &read_options).unwrap();
    reset_json_module_source_hash_count();
    write_json_module_machine_cache_with_source_hash(
      &cache_target,
      source,
      &read_options,
      &repaired_generated,
      Some(&validated_source_hash),
    );

    assert_eq!(json_module_source_hash_count(), 0);
    assert_eq!(
      read_json_module_machine_cache(&cache_target, source, &read_options),
      Some(repaired_generated)
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_payload_stores_raw_source_hash_digest() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();

    let machine_bytes = encode_json_module_machine_payload(source, &options, &generated).unwrap();

    assert_eq!(JSON_MODULE_MACHINE_SOURCE_HASH_RANGE.len(), 32);
    assert_eq!(
      &machine_bytes[JSON_MODULE_MACHINE_SOURCE_HASH_RANGE],
      blake3::hash(source).as_bytes()
    );
    assert_eq!(
      decode_json_module_machine_payload(&machine_bytes, source, &options).as_deref(),
      Some(generated.as_str())
    );
  }

  #[test]
  fn json_machine_cache_decode_accepts_prevalidated_source_hash() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let machine_bytes = encode_json_module_machine_payload(source, &options, &generated).unwrap();
    let source_hash = blake3::hash(source);

    assert_eq!(
      decode_json_module_machine_payload_with_source_hash(&machine_bytes, &source_hash, &options)
        .as_deref(),
      Some(generated.as_str())
    );
    assert_eq!(
      decode_json_module_machine_payload_with_source_hash(
        &machine_bytes,
        &blake3::hash(br#"{"name":"changed"}"#),
        &options
      ),
      None
    );
  }

  #[test]
  fn json_machine_cache_rejects_source_hash_mismatch_before_payload_validation() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let machine_bytes = encode_json_module_machine_payload(source, &options, &generated).unwrap();

    reset_payload_validation_call_count();
    assert_eq!(
      decode_json_module_machine_payload_with_source_hash(
        &machine_bytes,
        &blake3::hash(br#"{"name":"changed"}"#),
        &options
      ),
      None
    );
    assert_eq!(payload_validation_call_count(), 0);
  }

  #[test]
  fn json_machine_cache_encode_accepts_precomputed_source_hash() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let source_hash = blake3::hash(source);

    let machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(&machine_bytes[JSON_MODULE_MACHINE_SOURCE_HASH_RANGE], source_hash.as_bytes());
    assert_eq!(
      decode_json_module_machine_payload_with_source_hash(&machine_bytes, &source_hash, &options)
        .as_deref(),
      Some(generated.as_str())
    );
  }

  #[test]
  fn json_machine_cache_compresses_large_generated_payloads() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = large_json_object(4_000);
    let generated = transform_json_module(&source, &options).unwrap();
    let machine_bytes =
      encode_json_module_machine_payload(source.as_bytes(), &options, &generated).unwrap();

    assert!(machine_bytes.starts_with(JSON_MODULE_MACHINE_MAGIC));
    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_LZ4);
    assert!(machine_bytes.len() < generated.len());
    assert_eq!(
      decode_json_module_machine_payload(&machine_bytes, source.as_bytes(), &options).as_deref(),
      Some(generated.as_str())
    );
  }

  #[test]
  fn json_machine_cache_rejects_raw_length_mismatch_before_payload_decode() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let mut machine_bytes =
      encode_json_module_machine_payload(source, &options, &generated).unwrap();
    machine_bytes[12..16]
      .copy_from_slice(&u32::try_from(generated.len() + 1).unwrap().to_le_bytes());
    let payload_start = JSON_MODULE_MACHINE_HEADER_LEN;

    assert!(!json_module_payload_shape_matches_header(
      machine_bytes[11],
      &machine_bytes[payload_start..],
      generated.len() + 1
    ));
    assert_eq!(decode_json_module_machine_payload(&machine_bytes, source, &options), None);
  }

  #[test]
  fn json_machine_cache_raw_payload_utf8_is_validated_before_owned_string() {
    assert_eq!(
      decode_raw_json_module_payload(b"export default 1;").as_deref(),
      Some("export default 1;")
    );
    assert_eq!(decode_raw_json_module_payload(&[0xff]), None);
  }

  #[test]
  fn json_machine_cache_raw_hit_reuses_machine_buffer_for_string() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let source_hash = blake3::hash(source);
    let machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_RAW);

    let machine_ptr = machine_bytes.as_ptr();
    let decoded =
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options)
        .unwrap();

    assert_eq!(decoded, generated);
    assert_eq!(decoded.as_ptr(), machine_ptr);
  }

  #[test]
  fn json_machine_cache_raw_owned_hit_bypasses_generic_payload_validation() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let source_hash = blake3::hash(source);
    let machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_RAW);

    let machine_ptr = machine_bytes.as_ptr();
    reset_payload_validation_call_count();
    let decoded =
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options)
        .unwrap();

    assert_eq!(decoded, generated);
    assert_eq!(decoded.as_ptr(), machine_ptr);
    assert_eq!(payload_validation_call_count(), 0);
  }

  #[test]
  fn json_machine_cache_lz4_owned_hit_bypasses_generic_payload_validation() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = large_json_object(4_000);
    let generated = transform_json_module(&source, &options).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_LZ4);

    reset_payload_validation_call_count();
    let decoded =
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options)
        .unwrap();

    assert_eq!(decoded, generated);
    assert_eq!(payload_validation_call_count(), 0);
  }

  #[test]
  fn json_machine_cache_lz4_owned_hit_uses_prechecked_lz4_body_decode() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = large_json_object(4_000);
    let generated = transform_json_module(&source, &options).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_LZ4);

    reset_payload_validation_call_count();
    reset_lz4_size_prepended_decode_count();
    let decoded =
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options)
        .unwrap();

    assert_eq!(decoded, generated);
    assert_eq!(payload_validation_call_count(), 0);
    assert_eq!(lz4_size_prepended_decode_count(), 0);
  }

  #[test]
  fn json_machine_cache_raw_owned_hit_rejects_invalid_utf8_before_buffer_reuse() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let source_hash = blake3::hash(source);
    let machine_bytes = encode_json_module_machine_payload_from_encoded_payload(
      &source_hash,
      &options,
      1,
      JSON_MODULE_MACHINE_CODEC_RAW,
      &[0xff],
    )
    .unwrap();

    reset_payload_validation_call_count();
    reset_raw_owned_buffer_reuse_count();
    assert_eq!(
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options),
      None
    );
    assert_eq!(payload_validation_call_count(), 0);
    assert_eq!(raw_owned_buffer_reuse_count(), 0);
  }

  #[test]
  fn json_machine_cache_raw_owned_hit_rejects_length_mismatch_without_generic_validation() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source = br#"{"name":"rolldown"}"#;
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let source_hash = blake3::hash(source);
    let mut machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();

    assert_eq!(machine_bytes[11], JSON_MODULE_MACHINE_CODEC_RAW);
    machine_bytes[12..16]
      .copy_from_slice(&u32::try_from(generated.len() + 1).unwrap().to_le_bytes());

    reset_payload_validation_call_count();
    assert_eq!(
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options),
      None
    );
    assert_eq!(payload_validation_call_count(), 0);

    let mut machine_bytes =
      encode_json_module_machine_payload_with_source_hash(&source_hash, &options, &generated)
        .unwrap();
    machine_bytes[12..16]
      .copy_from_slice(&u32::try_from(generated.len() - 1).unwrap().to_le_bytes());

    reset_payload_validation_call_count();
    assert_eq!(
      decode_json_module_machine_payload_with_owned_bytes(machine_bytes, &source_hash, &options),
      None
    );
    assert_eq!(payload_validation_call_count(), 0);
  }

  #[test]
  fn json_machine_cache_rejects_lz4_size_prefix_mismatch_before_decode() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = large_json_object(4_000);
    let generated = transform_json_module(&source, &options).unwrap();
    let mut machine_bytes =
      encode_json_module_machine_payload(source.as_bytes(), &options, &generated).unwrap();
    let payload_start = JSON_MODULE_MACHINE_HEADER_LEN;
    let wrong_size = u32::try_from(generated.len() + 1).unwrap().to_le_bytes();
    machine_bytes[payload_start..payload_start + 4].copy_from_slice(&wrong_size);

    assert!(!json_module_lz4_payload_size_matches_header(
      &machine_bytes[payload_start..],
      generated.len()
    ));
    assert_eq!(
      decode_json_module_machine_payload(&machine_bytes, source.as_bytes(), &options),
      None
    );
  }

  #[test]
  fn json_machine_cache_validated_payload_borrows_lz4_payload_for_decode() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = large_json_object(4_000);
    let generated = transform_json_module(&source, &options).unwrap();
    let machine_bytes =
      encode_json_module_machine_payload(source.as_bytes(), &options, &generated).unwrap();
    let payload = &machine_bytes[JSON_MODULE_MACHINE_HEADER_LEN..];
    let generated_len = u32::from_le_bytes(machine_bytes[12..16].try_into().unwrap()) as usize;

    let Some(ValidatedJsonModuleMachinePayload::Lz4 {
      compressed: validated_payload,
      generated_len: validated_len,
    }) = json_module_validated_payload(machine_bytes[11], payload, generated_len)
    else {
      panic!("expected validated lz4 payload");
    };

    assert_eq!(validated_len, generated_len);
    assert!(std::ptr::eq(validated_payload.as_ptr(), payload.as_ptr()));
    assert_eq!(validated_payload.len(), payload.len());
  }

  #[test]
  fn json_machine_cache_validated_payload_rejects_invalid_shapes() {
    assert!(matches!(
      json_module_validated_payload(JSON_MODULE_MACHINE_CODEC_RAW, b"esm", 3),
      Some(ValidatedJsonModuleMachinePayload::Raw(b"esm"))
    ));
    assert!(json_module_validated_payload(JSON_MODULE_MACHINE_CODEC_RAW, b"esm", 4).is_none());
    assert!(json_module_validated_payload(255, b"esm", 3).is_none());

    let lz4_len = 4usize;
    assert!(matches!(
      json_module_validated_payload(
        JSON_MODULE_MACHINE_CODEC_LZ4,
        &u32::try_from(lz4_len).unwrap().to_le_bytes(),
        lz4_len,
      ),
      Some(ValidatedJsonModuleMachinePayload::Lz4 { compressed: _, generated_len: 4 })
    ));
    assert!(
      json_module_validated_payload(JSON_MODULE_MACHINE_CODEC_LZ4, &5u32.to_le_bytes(), lz4_len)
        .is_none()
    );
    assert!(
      json_module_validated_payload(JSON_MODULE_MACHINE_CODEC_LZ4, &[0, 1, 2], lz4_len).is_none()
    );
    assert!(
      json_module_validated_payload(
        JSON_MODULE_MACHINE_CODEC_LZ4,
        &u32::MAX.to_le_bytes(),
        u32::MAX as usize,
      )
      .is_none()
    );
  }

  #[test]
  fn json_machine_cache_rejects_lz4_oversized_generated_len_before_decode() {
    assert!(!json_module_generated_len_within_machine_budget(u32::MAX as usize));
    assert!(json_module_generated_len_within_machine_budget(JSON_MODULE_MACHINE_MAX_GENERATED_LEN));
    assert!(!json_module_payload_shape_matches_header(
      JSON_MODULE_MACHINE_CODEC_LZ4,
      &u32::MAX.to_le_bytes(),
      u32::MAX as usize
    ));

    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source = br#"{"name":"rolldown"}"#;
    let source_hash = blake3::hash(source);
    let generated_len = u32::MAX;
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(JSON_MODULE_MACHINE_MAGIC);
    machine_bytes.push(u8::from(options.named_exports));
    machine_bytes.push(u8::from(options.minify));
    machine_bytes.push(options.stringify.cache_tag());
    machine_bytes.push(JSON_MODULE_MACHINE_CODEC_LZ4);
    machine_bytes.extend_from_slice(&generated_len.to_le_bytes());
    machine_bytes.extend_from_slice(source_hash.as_bytes());
    machine_bytes.extend_from_slice(&generated_len.to_le_bytes());

    assert_eq!(
      decode_json_module_machine_payload_with_source_hash(&machine_bytes, &source_hash, &options),
      None
    );
  }

  #[test]
  fn json_machine_cache_encode_rejects_over_budget_len_without_large_payload() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let source_hash = blake3::hash(br#"{"name":"rolldown"}"#);
    let generated_len = JSON_MODULE_MACHINE_MAX_GENERATED_LEN + 1;
    let payload_size_prefix = u32::try_from(generated_len).unwrap().to_le_bytes();

    assert!(
      encode_json_module_machine_payload_from_encoded_payload(
        &source_hash,
        &options,
        generated_len,
        JSON_MODULE_MACHINE_CODEC_LZ4,
        &payload_size_prefix,
      )
      .is_none()
    );
  }

  #[test]
  fn json_machine_cache_options_are_part_of_the_key() {
    let root = unique_temp_root("options");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source = br#"{"name":"rolldown"}"#;
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let named_options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let default_options =
      JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &named_options).unwrap();
    let named_cache_target =
      enabled_json_module_cache_target(&project_root, &source_path, &named_options);
    let default_cache_target =
      enabled_json_module_cache_target(&project_root, &source_path, &default_options);

    write_json_module_machine_cache(&named_cache_target, source, &named_options, &generated);

    assert_eq!(
      read_json_module_machine_cache(&default_cache_target, source, &default_options),
      None
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn json_machine_cache_namespace_tracks_payload_version() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    assert!(options.cache_namespace().starts_with("json_module_v2_"));
  }

  #[test]
  fn json_machine_cache_namespace_uses_static_strings() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let first = options.cache_namespace();
    let second = options.cache_namespace();

    assert_eq!(first, "json_module_v2_named_exports_1_minify_0_stringify_auto");
    assert!(std::ptr::eq(first.as_ptr(), second.as_ptr()));
  }

  #[test]
  fn json_machine_cache_disabled_config_has_no_paths() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("data.json");
    let cache_config = DxMachineCacheConfig::from_env_value(&project_root, None);
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    assert!(
      json_module_machine_cache_paths_if_enabled(
        &cache_config,
        &project_root,
        &source_path,
        &options,
      )
      .is_none()
    );
    assert!(
      json_module_machine_cache_target_from_source_path(
        cache_config,
        &project_root,
        &source_path,
        &options,
      )
      .is_none()
    );
  }

  #[test]
  fn json_machine_cache_skips_sources_at_or_below_minimum_before_path_build() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("data.json");
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config.clone(),
        &project_root,
        &source_path,
        &json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES),
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_path_build_count(), 0);

    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config,
        &project_root,
        &source_path,
        &json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1),
        &options,
      )
      .is_some()
    );
    assert_eq!(cache_path_build_count(), 1);
  }

  #[test]
  fn json_machine_cache_skips_virtual_source_paths_before_path_build() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = Path::new("\0virtual:data.json");
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config,
        &project_root,
        source_path,
        &json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1),
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_skips_virtual_id_before_env_config() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let id = "\0virtual:data.json?import";
    let source = json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1);
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let transform_plan = json_module_transform_plan(&source, &options);

    reset_cache_config_lookup_count();
    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_if_worth_cache(
        &project_root,
        id,
        &source,
        transform_plan,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_config_lookup_count(), 0);
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_target_from_id_checks_virtual_source_path_once() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let id = "G:/Dx/build/src/data.json?import";
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    reset_source_path_virtual_check_count();
    assert!(
      json_module_machine_cache_target_with_config(cache_config, &project_root, id, &options,)
        .is_some()
    );
    assert_eq!(source_path_virtual_check_count(), 1);
  }

  #[test]
  fn json_machine_cache_skips_stringify_only_fast_path_before_env_config() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let id = "G:/Dx/build/src/large-forced-stringify.json";
    let source = json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1);
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::True);
    let transform_plan = json_module_transform_plan(&source, &options);

    reset_cache_config_lookup_count();
    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_if_worth_cache(
        &project_root,
        id,
        &source,
        transform_plan,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_config_lookup_count(), 0);
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_skips_top_level_scalar_parse_path_before_env_config() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let id = "G:/Dx/build/src/large-scalar.json";
    let source =
      serde_json::to_string(&"x".repeat(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1)).unwrap();
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::False);
    let transform_plan = json_module_transform_plan(&source, &options);
    assert!(transform_plan.requires_parse);

    reset_cache_config_lookup_count();
    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_if_worth_cache(
        &project_root,
        id,
        &source,
        transform_plan,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_config_lookup_count(), 0);
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_skips_large_borrowed_stringify_fast_path() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("large-string.json");
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::Auto);
    let source =
      serde_json::to_string(&"x".repeat(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1)).unwrap();

    reset_from_slice_value_parse_count();
    assert!(
      transform_json_module(&source, &options)
        .unwrap()
        .starts_with("export default /*#__PURE__*/ JSON.parse(")
    );
    assert_eq!(from_slice_value_parse_count(), 0);

    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config,
        &project_root,
        &source_path,
        &source,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_skips_large_forced_stringify_without_named_exports() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("large-forced-stringify.json");
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::True);
    let source = json_object_source_with_len(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1);

    reset_from_slice_value_parse_count();
    assert!(
      transform_json_module(&source, &options)
        .unwrap()
        .starts_with("export default /*#__PURE__*/ JSON.parse(")
    );
    assert_eq!(from_slice_value_parse_count(), 0);

    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config,
        &project_root,
        &source_path,
        &source,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_skips_named_exports_for_large_non_object_json() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("large-non-object.json");
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let source =
      serde_json::to_string(&"x".repeat(JSON_MODULE_MACHINE_MIN_SOURCE_BYTES + 1)).unwrap();

    reset_from_slice_value_parse_count();
    assert!(
      transform_json_module(&source, &options)
        .unwrap()
        .starts_with("export default /*#__PURE__*/ JSON.parse(")
    );
    assert_eq!(from_slice_value_parse_count(), 0);

    reset_cache_path_build_count();
    assert!(
      json_module_machine_cache_target_from_source_path_if_worth_cache(
        cache_config,
        &project_root,
        &source_path,
        &source,
        &options,
      )
      .is_none()
    );
    assert_eq!(cache_path_build_count(), 0);
  }

  #[test]
  fn json_machine_cache_keeps_large_parse_paths_for_named_exports_minify_or_stringify_false() {
    let project_root = PathBuf::from(r"G:\Dx\build");
    let source_path = project_root.join("src").join("large-object.json");
    let source = large_json_object(1_600);
    let parse_options = [
      JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto),
      JsonModuleMachineOptions::new(false, true, ViteJsonPluginStringify::Auto),
      JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::False),
    ];

    reset_cache_path_build_count();
    for options in parse_options {
      let cache_config =
        DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
      assert!(
        json_module_machine_cache_target_from_source_path_if_worth_cache(
          cache_config,
          &project_root,
          &source_path,
          &source,
          &options,
        )
        .is_some()
      );
    }
    assert_eq!(cache_path_build_count(), parse_options.len() as u64);
  }

  #[test]
  fn json_machine_cache_invalidates_source_mutation() {
    let root = unique_temp_root("mutation");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("data.json");
    let source = br#"{"name":"rolldown"}"#;
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source, &options, &generated);

    assert_eq!(
      read_json_module_machine_cache(&cache_target, br#"{"name":"changed"}"#, &options),
      None
    );
    assert!(transform_json_module("{", &options).is_err());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn transform_json_module_parses_source_as_bytes() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);

    reset_from_str_value_parse_count();
    reset_from_slice_value_parse_count();
    let generated = transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap();

    assert!(generated.contains("rolldown"));
    assert_eq!(from_str_value_parse_count(), 0);
    assert_eq!(from_slice_value_parse_count(), 1);
  }

  #[test]
  fn transform_json_module_minify_parses_source_as_bytes() {
    let options = JsonModuleMachineOptions::new(false, true, ViteJsonPluginStringify::True);

    reset_from_str_value_parse_count();
    reset_from_slice_value_parse_count();
    let generated = transform_json_module(r#"{ "name" : "rolldown" }"#, &options).unwrap();

    assert!(generated.contains("JSON.parse"));
    assert_eq!(from_str_value_parse_count(), 0);
    assert_eq!(from_slice_value_parse_count(), 1);
  }

  #[test]
  fn invalid_json_error_matches_current_serde_path() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);
    let transform_error = transform_json_module("{", &options).unwrap_err().to_string();
    let serde_error = serde_json::from_str::<Value>("{").unwrap_err().to_string();

    assert_eq!(transform_error, serde_error);
  }

  #[test]
  fn json_transform_strips_bom_before_generation() {
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::Auto);

    assert_eq!(
      transform_json_module(
        rolldown_plugin_utils::strip_bom("\u{feff}{\"name\":\"rolldown\"}"),
        &options
      )
      .unwrap(),
      "export const name = \"rolldown\";\nexport default {\n  name\n};"
    );
  }

  #[test]
  fn json_transform_stringify_true_matches_current_output_shape() {
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::True);

    assert_eq!(
      transform_json_module(r#"{"name":"rolldown"}"#, &options).unwrap(),
      "export default /*#__PURE__*/ JSON.parse(\"{\\\"name\\\":\\\"rolldown\\\"}\")"
    );
  }

  #[test]
  fn large_json_auto_stringify_uses_json_parse_output() {
    let options = JsonModuleMachineOptions::new(false, false, ViteJsonPluginStringify::Auto);
    let large_json = serde_json::to_string(&"x".repeat(constants::THRESHOLD_SIZE + 1)).unwrap();

    reset_from_slice_value_parse_count();
    assert!(
      transform_json_module(&large_json, &options)
        .unwrap()
        .starts_with("export default /*#__PURE__*/ JSON.parse(")
    );
    assert_eq!(from_slice_value_parse_count(), 0);
  }

  #[test]
  #[ignore = "focused local timing proof; not stable enough for normal test runs"]
  fn json_machine_cache_large_object_microbench_reports_hit_vs_reparse() {
    let root = unique_temp_root("microbench");
    let project_root = root.join("project");
    let source_path = project_root.join("src").join("large.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let source = large_json_object(4_000);
    fs::write(&source_path, source.as_bytes()).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let generated = transform_json_module(&source, &options).unwrap();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);
    write_json_module_machine_cache(&cache_target, source.as_bytes(), &options, &generated);

    let iterations = 20;
    let transform_start = Instant::now();
    for _ in 0..iterations {
      assert_eq!(transform_json_module(&source, &options).unwrap(), generated);
    }
    let transform_elapsed = transform_start.elapsed();

    let cache_start = Instant::now();
    for _ in 0..iterations {
      assert_eq!(
        read_json_module_machine_cache(&cache_target, source.as_bytes(), &options),
        Some(generated.clone())
      );
    }
    let cache_elapsed = cache_start.elapsed();

    assert!(
      cache_elapsed < transform_elapsed,
      "cache hit path was not faster than reparse/regenerate for this local sample: transform={transform_elapsed:?}, cache={cache_elapsed:?}, iterations={iterations}"
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  #[ignore = "writes persistent proof artifacts only when ROLLDOWN_DX_JSON_CACHE_PROOF_DIR is set"]
  fn json_machine_cache_writes_persistent_proof_receipt_when_requested() {
    let Some(proof_dir) = std::env::var_os("ROLLDOWN_DX_JSON_CACHE_PROOF_DIR") else { return };
    let proof_dir = PathBuf::from(proof_dir);
    let proof_root = proof_dir.join(format!(
      "json-module-machine-proof-{}-{}",
      std::process::id(),
      SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    let project_root = proof_root.join("project");
    let source_path = project_root.join("src").join("cache-proof.json");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();

    let source = large_json_object(1_024);
    fs::write(&source_path, source.as_bytes()).unwrap();
    let options = JsonModuleMachineOptions::new(true, false, ViteJsonPluginStringify::False);
    let generated = transform_json_module(&source, &options).unwrap();
    let generated_hash = blake3::hash(generated.as_bytes()).to_hex().to_string();
    let cache_target = enabled_json_module_cache_target(&project_root, &source_path, &options);

    write_json_module_machine_cache(&cache_target, source.as_bytes(), &options, &generated);

    reset_transform_count();
    reset_from_slice_value_parse_count();
    reset_payload_validation_call_count();
    reset_lz4_size_prepended_decode_count();
    reset_lz4_body_decode_count();

    assert_eq!(
      read_json_module_machine_cache(&cache_target, source.as_bytes(), &options).as_deref(),
      Some(generated.as_str())
    );

    let machine_bytes = fs::read(&cache_target.paths.machine).unwrap();
    let machine_codec = match machine_bytes[11] {
      JSON_MODULE_MACHINE_CODEC_RAW => "raw",
      JSON_MODULE_MACHINE_CODEC_LZ4 => "lz4",
      other => panic!("unexpected json module machine codec {other}"),
    };
    let metadata_bytes = fs::read(&cache_target.paths.metadata).unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_bytes).unwrap();
    let receipt_path = proof_root.join("machine-cache-proof.json");
    let receipt = serde_json::json!({
      "schema": "rolldown.dx.json_machine_cache_proof.v1",
      "claimStatus": "local_cache_path_proof_only",
      "speedupClaim": "none",
      "projectRoot": path_for_receipt(&project_root),
      "source": {
        "path": path_for_receipt(&source_path),
        "bytes": source.len(),
        "blake3": blake3::hash(source.as_bytes()).to_hex().to_string()
      },
      "machine": {
        "path": path_for_receipt(&cache_target.paths.machine),
        "bytes": machine_bytes.len(),
        "magic": String::from_utf8_lossy(&machine_bytes[..JSON_MODULE_MACHINE_MAGIC.len()]),
        "codec": machine_codec,
      },
      "metadata": {
        "path": path_for_receipt(&cache_target.paths.metadata),
        "bytes": metadata_bytes.len(),
        "schema": metadata["schema"].as_str(),
        "sourceBytes": metadata["source"]["bytes"].as_u64(),
        "machineBytes": metadata["machine"]["bytes"].as_u64(),
      },
      "cacheRead": {
        "hit": true,
        "generatedEsmBytes": generated.len(),
        "generatedEsmBlake3": generated_hash,
      },
      "hotCache": {
        "transformCount": transform_count(),
        "fromSliceValueParseCount": from_slice_value_parse_count(),
        "payloadValidationCount": payload_validation_call_count(),
        "lz4SizePrependedDecodeCount": lz4_size_prepended_decode_count(),
        "lz4BodyDecodeCount": lz4_body_decode_count(),
      }
    });
    fs::write(&receipt_path, format!("{}\n", serde_json::to_string_pretty(&receipt).unwrap()))
      .unwrap();

    assert_eq!(receipt["machine"]["codec"].as_str(), Some("lz4"));
    assert_eq!(receipt["hotCache"]["transformCount"], 0);
    assert_eq!(receipt["hotCache"]["fromSliceValueParseCount"], 0);
    assert_eq!(receipt["hotCache"]["payloadValidationCount"], 0);
    assert_eq!(receipt["hotCache"]["lz4SizePrependedDecodeCount"], 0);
    assert_eq!(receipt["hotCache"]["lz4BodyDecodeCount"], 1);
    assert!(receipt_path.exists());
    assert!(cache_target.paths.machine.exists());
  }

  fn large_json_object(entries: usize) -> String {
    let mut source = String::from("{");
    for index in 0..entries {
      if index > 0 {
        source.push(',');
      }
      source.push('"');
      source.push_str("key_");
      source.push_str(&index.to_string());
      source.push_str(r#"":"value_"#);
      source.push_str(&index.to_string());
      source.push('"');
    }
    source.push('}');
    source
  }

  fn json_object_source_with_len(target_len: usize) -> String {
    let prefix = "{\"payload\":\"";
    let suffix = "\"}";
    assert!(target_len >= prefix.len() + suffix.len());
    let filler = "x".repeat(target_len - prefix.len() - suffix.len());
    format!("{prefix}{filler}{suffix}")
  }

  fn unique_temp_root(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-vite-json-machine-{label}-{}-{nanos}", std::process::id()))
  }

  fn path_for_receipt(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
  }
}
