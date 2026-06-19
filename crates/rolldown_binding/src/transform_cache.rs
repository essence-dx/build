use std::{
  borrow::Cow,
  fs, io,
  path::{Path, PathBuf},
  sync::Arc,
};

use dashmap::Entry;
use napi_derive::napi;
use oxc_resolver::{ResolveError, ResolveOptions, Resolver, TsConfig, TsconfigDiscovery};
use rolldown_utils::{
  dashmap::FxDashMap,
  dx_machine_cache::{DxMachineCacheConfig, DxMachineCachePaths, DxMachineCacheStatus},
};
use serde::{
  Deserialize,
  de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
#[cfg(test)]
use serde_json::Value;

const DX_TSCONFIG_MACHINE_SCHEMA: &str = "rolldown.dx.tsconfig.v3";
const DX_TSCONFIG_MACHINE_MAGIC: &[u8; 8] = b"RDXTSC03";
const DX_TSCONFIG_MACHINE_HEADER_LEN: usize = 12;
const DX_TSCONFIG_SOURCE_HASH_LEN: usize = 32;
const DX_TSCONFIG_CACHE_NAMESPACE: &str = "tsconfig_metadata";
const DX_TSCONFIG_MACHINE_MIN_SOURCE_BYTES: usize = 16 * 1024;
const DX_TSCONFIG_NONE_LEN: u32 = u32::MAX;

#[cfg(test)]
thread_local! {
  static DX_TSCONFIG_TEST_STRING_MATERIALIZATIONS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_DYNAMIC_JSON_OBJECT_BUILDS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_DEPENDENCY_VALUE_PARSES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_DEPENDENCY_PREFLIGHT_SCANS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_NEAREST_PATH_PROBES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_PAYLOAD_LAYOUT_VALIDATIONS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_WRITE_SOURCE_HASHES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_TSCONFIG_TEST_CACHE_PATH_BUILDS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

#[napi]
pub struct TsconfigCache {
  resolver: Arc<Resolver>,
  cache: FxDashMap<PathBuf, Arc<TsConfig>>,
  nearest_tsconfig_path_cache: FxDashMap<PathBuf, PathBuf>,
}

#[napi]
impl TsconfigCache {
  /// Create a new transform cache with auto tsconfig discovery enabled.
  #[napi(constructor)]
  pub fn new(yarn_pnp: bool) -> Self {
    Self {
      resolver: Arc::new(Resolver::new(ResolveOptions {
        tsconfig: Some(TsconfigDiscovery::Auto),
        yarn_pnp,
        ..Default::default()
      })),
      cache: FxDashMap::default(),
      nearest_tsconfig_path_cache: FxDashMap::default(),
    }
  }

  /// Clear the cache.
  ///
  /// Call this when tsconfig files have changed to ensure fresh resolution.
  #[napi]
  pub fn clear(&self) {
    self.cache.clear();
    self.nearest_tsconfig_path_cache.clear();
  }

  /// Get the number of cached entries.
  #[napi]
  pub fn size(&self) -> u32 {
    u32::try_from(self.cache.len()).unwrap_or(u32::MAX)
  }
}

impl TsconfigCache {
  /// Get the resolver instance.
  pub fn resolver(&self) -> &Resolver {
    &self.resolver
  }

  /// Find and cache tsconfig for a given file path.
  ///
  /// Returns None if no tsconfig is found for the file.
  pub fn find_tsconfig(&self, file_path: &Path) -> Result<Option<Arc<TsConfig>>, ResolveError> {
    let dx_tsconfig = self.find_dx_tsconfig(file_path);
    if let Some(tsconfig) = dx_tsconfig.tsconfig {
      return Ok(Some(tsconfig));
    }

    let tsconfig_result = self.resolver.find_tsconfig(file_path);
    match tsconfig_result {
      Ok(Some(arc_tsconfig)) => {
        let cache_key = arc_tsconfig.path.clone();

        match self.cache.entry(cache_key) {
          Entry::Occupied(entry) => Ok(Some(Arc::clone(entry.get()))),
          Entry::Vacant(vacant_entry) => {
            self.write_dx_tsconfig_with_repair(&arc_tsconfig, dx_tsconfig.repair.as_ref());
            vacant_entry.insert(Arc::clone(&arc_tsconfig));
            Ok(Some(arc_tsconfig))
          }
        }
      }
      Ok(None) | Err(_) => tsconfig_result,
    }
  }

  fn find_dx_tsconfig(&self, file_path: &Path) -> DxTsconfigLookup {
    if !tsconfig_file_path_is_dx_cache_eligible(file_path) {
      return DxTsconfigLookup::default();
    }

    let project_root = dx_machine_cache_project_root(file_path);
    let Some(cache_config) = DxMachineCacheConfig::from_env_if_enabled(&project_root) else {
      return DxTsconfigLookup::default();
    };

    let Some(tsconfig_path) = self.nearest_dx_tsconfig_path(file_path) else {
      return DxTsconfigLookup::default();
    };
    if let Some(tsconfig) = self.cache.get(&tsconfig_path) {
      return DxTsconfigLookup { tsconfig: Some(Arc::clone(tsconfig.value())), repair: None };
    }

    let Ok(source_bytes) = fs::read(&tsconfig_path) else {
      return DxTsconfigLookup::default();
    };
    let Some(read) = read_dx_tsconfig_machine_cache_or_repair_hash(
      &cache_config,
      &project_root,
      &tsconfig_path,
      &source_bytes,
    ) else {
      return DxTsconfigLookup::default();
    };

    let Some(tsconfig) = read.tsconfig else {
      return DxTsconfigLookup {
        tsconfig: None,
        repair: read
          .repair_source_hash
          .map(|source_hash| DxTsconfigRepair { tsconfig_path, source_hash }),
      };
    };

    match self.cache.entry(tsconfig.path.clone()) {
      Entry::Occupied(entry) => {
        DxTsconfigLookup { tsconfig: Some(Arc::clone(entry.get())), repair: None }
      }
      Entry::Vacant(vacant_entry) => {
        vacant_entry.insert(Arc::clone(&tsconfig));
        DxTsconfigLookup { tsconfig: Some(tsconfig), repair: None }
      }
    }
  }

  fn write_dx_tsconfig_with_repair(&self, tsconfig: &TsConfig, repair: Option<&DxTsconfigRepair>) {
    let project_root = dx_machine_cache_project_root(&tsconfig.path);
    let Some(cache_config) = DxMachineCacheConfig::from_env_if_enabled(&project_root) else {
      return;
    };

    let Ok(source_bytes) = fs::read(&tsconfig.path) else {
      return;
    };
    let repair_source_hash = repair
      .and_then(|repair| (repair.tsconfig_path == tsconfig.path).then_some(&repair.source_hash));
    write_dx_tsconfig_machine_cache_with_optional_source_hash(
      &cache_config,
      &project_root,
      &tsconfig.path,
      &source_bytes,
      tsconfig,
      repair_source_hash,
    );
  }

  fn nearest_dx_tsconfig_path(&self, file_path: &Path) -> Option<PathBuf> {
    let source_directory = tsconfig_search_directory(file_path)?;

    let mut directory = source_directory;
    let mut searched_directories = Vec::new();
    loop {
      let candidate = directory.join("tsconfig.json");
      if let Some(cached_path) = self.nearest_tsconfig_path_cache.get(&directory) {
        let tsconfig_path = cached_path.value().clone();
        drop(cached_path);
        if tsconfig_path.parent() != Some(directory.as_path()) {
          #[cfg(test)]
          DX_TSCONFIG_TEST_NEAREST_PATH_PROBES.with(|count| count.set(count.get() + 1));
          if candidate.is_file() {
            searched_directories.push(directory.clone());
            for searched_directory in searched_directories {
              self.nearest_tsconfig_path_cache.insert(searched_directory, candidate.clone());
            }
            return Some(candidate);
          }
        }
        if tsconfig_path.is_file() {
          for searched_directory in searched_directories {
            self.nearest_tsconfig_path_cache.insert(searched_directory, tsconfig_path.clone());
          }
          return Some(tsconfig_path);
        }
        self.nearest_tsconfig_path_cache.remove(&directory);
      }

      searched_directories.push(directory.clone());
      #[cfg(test)]
      DX_TSCONFIG_TEST_NEAREST_PATH_PROBES.with(|count| count.set(count.get() + 1));
      if candidate.is_file() {
        for searched_directory in searched_directories {
          self.nearest_tsconfig_path_cache.insert(searched_directory, candidate.clone());
        }
        return Some(candidate);
      }
      if !directory.pop() {
        return None;
      }
    }
  }
}

#[derive(Default)]
struct DxTsconfigLookup {
  tsconfig: Option<Arc<TsConfig>>,
  repair: Option<DxTsconfigRepair>,
}

struct DxTsconfigRepair {
  tsconfig_path: PathBuf,
  source_hash: blake3::Hash,
}

#[derive(Debug)]
struct DxTsconfigMachinePayload {
  schema: &'static str,
  source_blake3: [u8; DX_TSCONFIG_SOURCE_HASH_LEN],
  files: Option<Vec<String>>,
  include: Option<Vec<String>>,
  exclude: Option<Vec<String>>,
  compiler_options: DxTsconfigCompilerOptionsPayload,
}

#[derive(Debug, Default)]
struct DxTsconfigCompilerOptionsPayload {
  base_url: Option<String>,
  paths: Option<Vec<DxTsconfigPathMappingPayload>>,
  experimental_decorators: Option<bool>,
  emit_decorator_metadata: Option<bool>,
  use_define_for_class_fields: Option<bool>,
  rewrite_relative_import_extensions: Option<bool>,
  jsx: Option<String>,
  jsx_factory: Option<String>,
  jsx_fragment_factory: Option<String>,
  jsx_import_source: Option<String>,
  verbatim_module_syntax: Option<bool>,
  preserve_value_imports: Option<bool>,
  imports_not_used_as_values: Option<String>,
  target: Option<String>,
  module: Option<String>,
  allow_js: Option<bool>,
  root_dirs: Option<Vec<String>>,
}

#[derive(Debug)]
struct DxTsconfigPathMappingPayload {
  key: String,
  values: Vec<String>,
}

impl DxTsconfigMachinePayload {
  #[cfg(test)]
  fn from_tsconfig(tsconfig: &TsConfig, source_bytes: &[u8]) -> Self {
    Self::from_tsconfig_with_source_hash(tsconfig, &blake3::hash(source_bytes))
  }

  fn from_tsconfig_with_source_hash(tsconfig: &TsConfig, source_hash: &blake3::Hash) -> Self {
    let compiler_options = &tsconfig.compiler_options;
    Self {
      schema: DX_TSCONFIG_MACHINE_SCHEMA,
      source_blake3: *source_hash.as_bytes(),
      files: pathbufs_to_strings(&tsconfig.files),
      include: pathbufs_to_strings(&tsconfig.include),
      exclude: pathbufs_to_strings(&tsconfig.exclude),
      compiler_options: DxTsconfigCompilerOptionsPayload {
        base_url: compiler_options.base_url.as_ref().map(pathbuf_to_string),
        paths: compiler_options.paths.as_ref().map(|paths| {
          paths
            .iter()
            .map(|(key, values)| DxTsconfigPathMappingPayload {
              key: key.clone(),
              values: values.iter().map(pathbuf_to_string).collect(),
            })
            .collect()
        }),
        experimental_decorators: compiler_options.experimental_decorators,
        emit_decorator_metadata: compiler_options.emit_decorator_metadata,
        use_define_for_class_fields: compiler_options.use_define_for_class_fields,
        rewrite_relative_import_extensions: compiler_options.rewrite_relative_import_extensions,
        jsx: compiler_options.jsx.clone(),
        jsx_factory: compiler_options.jsx_factory.clone(),
        jsx_fragment_factory: compiler_options.jsx_fragment_factory.clone(),
        jsx_import_source: compiler_options.jsx_import_source.clone(),
        verbatim_module_syntax: compiler_options.verbatim_module_syntax,
        preserve_value_imports: compiler_options.preserve_value_imports,
        imports_not_used_as_values: compiler_options.imports_not_used_as_values.clone(),
        target: compiler_options.target.clone(),
        module: compiler_options.module.clone(),
        allow_js: compiler_options.allow_js,
        root_dirs: pathbufs_to_strings(&compiler_options.root_dirs),
      },
    }
  }

  #[cfg(test)]
  fn to_tsconfig(&self, tsconfig_path: &Path, source_bytes: &[u8]) -> Option<TsConfig> {
    if self.schema != DX_TSCONFIG_MACHINE_SCHEMA
      || !source_blake3_matches(&self.source_blake3, source_bytes)
    {
      return None;
    }

    self.to_tsconfig_without_source_validation(tsconfig_path)
  }

  fn to_tsconfig_with_source_hash(
    &self,
    tsconfig_path: &Path,
    source_hash: &blake3::Hash,
  ) -> Option<TsConfig> {
    if self.schema != DX_TSCONFIG_MACHINE_SCHEMA || self.source_blake3 != *source_hash.as_bytes() {
      return None;
    }

    self.to_tsconfig_without_source_validation(tsconfig_path)
  }

  fn to_tsconfig_without_source_validation(&self, tsconfig_path: &Path) -> Option<TsConfig> {
    let tsconfig_json = self.to_tsconfig_json_string()?;
    TsConfig::parse(true, tsconfig_path, tsconfig_json).ok()
  }

  fn to_tsconfig_json_string(&self) -> Option<String> {
    let mut json = String::with_capacity(self.tsconfig_json_string_capacity_hint());
    json.push('{');
    let mut needs_comma = false;
    push_json_string_array_property(&mut json, &mut needs_comma, "files", self.files.as_ref())?;
    push_json_string_array_property(&mut json, &mut needs_comma, "include", self.include.as_ref())?;
    push_json_string_array_property(&mut json, &mut needs_comma, "exclude", self.exclude.as_ref())?;
    push_json_property_name(&mut json, &mut needs_comma, "compilerOptions");
    self.compiler_options.push_compiler_options_json(&mut json)?;
    json.push('}');
    Some(json)
  }

  fn tsconfig_json_string_capacity_hint(&self) -> usize {
    "{\"compilerOptions\":{}}".len()
      + optional_string_array_json_capacity_hint(self.files.as_ref())
      + optional_string_array_json_capacity_hint(self.include.as_ref())
      + optional_string_array_json_capacity_hint(self.exclude.as_ref())
      + self.compiler_options.compiler_options_json_capacity_hint()
  }
}

impl DxTsconfigCompilerOptionsPayload {
  fn push_compiler_options_json(&self, json: &mut String) -> Option<()> {
    json.push('{');
    let mut needs_comma = false;
    push_json_string_property(json, &mut needs_comma, "baseUrl", self.base_url.as_ref())?;
    push_json_paths_property(json, &mut needs_comma, self.paths.as_ref())?;
    push_json_bool_property(
      json,
      &mut needs_comma,
      "experimentalDecorators",
      self.experimental_decorators,
    );
    push_json_bool_property(
      json,
      &mut needs_comma,
      "emitDecoratorMetadata",
      self.emit_decorator_metadata,
    );
    push_json_bool_property(
      json,
      &mut needs_comma,
      "useDefineForClassFields",
      self.use_define_for_class_fields,
    );
    push_json_bool_property(
      json,
      &mut needs_comma,
      "rewriteRelativeImportExtensions",
      self.rewrite_relative_import_extensions,
    );
    push_json_string_property(json, &mut needs_comma, "jsx", self.jsx.as_ref())?;
    push_json_string_property(json, &mut needs_comma, "jsxFactory", self.jsx_factory.as_ref())?;
    push_json_string_property(
      json,
      &mut needs_comma,
      "jsxFragmentFactory",
      self.jsx_fragment_factory.as_ref(),
    )?;
    push_json_string_property(
      json,
      &mut needs_comma,
      "jsxImportSource",
      self.jsx_import_source.as_ref(),
    )?;
    push_json_bool_property(
      json,
      &mut needs_comma,
      "verbatimModuleSyntax",
      self.verbatim_module_syntax,
    );
    push_json_bool_property(
      json,
      &mut needs_comma,
      "preserveValueImports",
      self.preserve_value_imports,
    );
    push_json_string_property(
      json,
      &mut needs_comma,
      "importsNotUsedAsValues",
      self.imports_not_used_as_values.as_ref(),
    )?;
    push_json_string_property(json, &mut needs_comma, "target", self.target.as_ref())?;
    push_json_string_property(json, &mut needs_comma, "module", self.module.as_ref())?;
    push_json_bool_property(json, &mut needs_comma, "allowJs", self.allow_js);
    push_json_string_array_property(json, &mut needs_comma, "rootDirs", self.root_dirs.as_ref())?;
    json.push('}');
    Some(())
  }

  fn compiler_options_json_capacity_hint(&self) -> usize {
    "{}".len()
      + optional_string_json_capacity_hint(self.base_url.as_ref())
      + optional_path_mappings_json_capacity_hint(self.paths.as_ref())
      + optional_bool_json_capacity_hint(self.experimental_decorators)
      + optional_bool_json_capacity_hint(self.emit_decorator_metadata)
      + optional_bool_json_capacity_hint(self.use_define_for_class_fields)
      + optional_bool_json_capacity_hint(self.rewrite_relative_import_extensions)
      + optional_string_json_capacity_hint(self.jsx.as_ref())
      + optional_string_json_capacity_hint(self.jsx_factory.as_ref())
      + optional_string_json_capacity_hint(self.jsx_fragment_factory.as_ref())
      + optional_string_json_capacity_hint(self.jsx_import_source.as_ref())
      + optional_bool_json_capacity_hint(self.verbatim_module_syntax)
      + optional_bool_json_capacity_hint(self.preserve_value_imports)
      + optional_string_json_capacity_hint(self.imports_not_used_as_values.as_ref())
      + optional_string_json_capacity_hint(self.target.as_ref())
      + optional_string_json_capacity_hint(self.module.as_ref())
      + optional_bool_json_capacity_hint(self.allow_js)
      + optional_string_array_json_capacity_hint(self.root_dirs.as_ref())
  }
}

struct JsonStringWriter<'a>(&'a mut String);

impl io::Write for JsonStringWriter<'_> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let value = std::str::from_utf8(buf)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 json bytes"))?;
    self.0.push_str(value);
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

fn push_json_property_name(json: &mut String, needs_comma: &mut bool, key: &str) {
  if *needs_comma {
    json.push(',');
  } else {
    *needs_comma = true;
  }
  json.push('"');
  json.push_str(key);
  json.push_str("\":");
}

fn push_json_string_property(
  json: &mut String,
  needs_comma: &mut bool,
  key: &str,
  value: Option<&String>,
) -> Option<()> {
  let Some(value) = value else {
    return Some(());
  };
  push_json_property_name(json, needs_comma, key);
  push_json_string(json, value)
}

fn push_json_bool_property(
  json: &mut String,
  needs_comma: &mut bool,
  key: &str,
  value: Option<bool>,
) {
  if let Some(value) = value {
    push_json_property_name(json, needs_comma, key);
    json.push_str(if value { "true" } else { "false" });
  }
}

fn push_json_string_array_property(
  json: &mut String,
  needs_comma: &mut bool,
  key: &str,
  value: Option<&Vec<String>>,
) -> Option<()> {
  let Some(value) = value else {
    return Some(());
  };
  push_json_property_name(json, needs_comma, key);
  push_json_string_array(json, value)
}

fn push_json_paths_property(
  json: &mut String,
  needs_comma: &mut bool,
  paths: Option<&Vec<DxTsconfigPathMappingPayload>>,
) -> Option<()> {
  let Some(paths) = paths else {
    return Some(());
  };
  push_json_property_name(json, needs_comma, "paths");
  json.push('{');
  let mut paths_needs_comma = false;
  for mapping in paths {
    if paths_needs_comma {
      json.push(',');
    } else {
      paths_needs_comma = true;
    }
    push_json_string(json, &mapping.key)?;
    json.push(':');
    push_json_string_array(json, &mapping.values)?;
  }
  json.push('}');
  Some(())
}

fn push_json_string_array(json: &mut String, values: &[String]) -> Option<()> {
  json.push('[');
  for (index, value) in values.iter().enumerate() {
    if index > 0 {
      json.push(',');
    }
    push_json_string(json, value)?;
  }
  json.push(']');
  Some(())
}

fn push_json_string(json: &mut String, value: &str) -> Option<()> {
  serde_json::to_writer(JsonStringWriter(json), value).ok()
}

fn optional_string_json_capacity_hint(value: Option<&String>) -> usize {
  value.map_or(0, |value| value.len() + 16)
}

fn optional_bool_json_capacity_hint(value: Option<bool>) -> usize {
  value.map_or(0, |_| 8)
}

fn optional_string_array_json_capacity_hint(values: Option<&Vec<String>>) -> usize {
  values.map_or(0, |values| 2 + values.iter().map(|value| value.len() + 3).sum::<usize>())
}

fn optional_path_mappings_json_capacity_hint(
  mappings: Option<&Vec<DxTsconfigPathMappingPayload>>,
) -> usize {
  mappings.map_or(0, |mappings| {
    2 + mappings
      .iter()
      .map(|mapping| {
        mapping.key.len() + 3 + optional_string_array_json_capacity_hint(Some(&mapping.values))
      })
      .sum::<usize>()
  })
}

fn encode_dx_tsconfig_machine_payload(payload: &DxTsconfigMachinePayload) -> Option<Vec<u8>> {
  let encoded_len = dx_tsconfig_machine_payload_encoded_len(payload)?;
  let mut machine_bytes = Vec::with_capacity(encoded_len);
  machine_bytes.extend_from_slice(DX_TSCONFIG_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
  machine_bytes.extend_from_slice(&payload.source_blake3);
  write_optional_string_vec(&mut machine_bytes, payload.files.as_ref())?;
  write_optional_string_vec(&mut machine_bytes, payload.include.as_ref())?;
  write_optional_string_vec(&mut machine_bytes, payload.exclude.as_ref())?;
  encode_dx_tsconfig_compiler_options(&mut machine_bytes, &payload.compiler_options)?;
  debug_assert_eq!(machine_bytes.len(), encoded_len);
  Some(machine_bytes)
}

fn dx_tsconfig_machine_payload_encoded_len(payload: &DxTsconfigMachinePayload) -> Option<usize> {
  let mut len = DX_TSCONFIG_MACHINE_HEADER_LEN;
  checked_add_len(&mut len, DX_TSCONFIG_SOURCE_HASH_LEN)?;
  checked_add_len(&mut len, optional_string_vec_encoded_len(payload.files.as_ref())?)?;
  checked_add_len(&mut len, optional_string_vec_encoded_len(payload.include.as_ref())?)?;
  checked_add_len(&mut len, optional_string_vec_encoded_len(payload.exclude.as_ref())?)?;
  checked_add_len(&mut len, dx_tsconfig_compiler_options_encoded_len(&payload.compiler_options)?)?;
  Some(len)
}

fn dx_tsconfig_compiler_options_encoded_len(
  compiler_options: &DxTsconfigCompilerOptionsPayload,
) -> Option<usize> {
  let mut len = 0;
  checked_add_len(&mut len, optional_string_encoded_len(compiler_options.base_url.as_ref())?)?;
  checked_add_len(&mut len, optional_path_mappings_encoded_len(compiler_options.paths.as_ref())?)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, optional_string_encoded_len(compiler_options.jsx.as_ref())?)?;
  checked_add_len(&mut len, optional_string_encoded_len(compiler_options.jsx_factory.as_ref())?)?;
  checked_add_len(
    &mut len,
    optional_string_encoded_len(compiler_options.jsx_fragment_factory.as_ref())?,
  )?;
  checked_add_len(
    &mut len,
    optional_string_encoded_len(compiler_options.jsx_import_source.as_ref())?,
  )?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(
    &mut len,
    optional_string_encoded_len(compiler_options.imports_not_used_as_values.as_ref())?,
  )?;
  checked_add_len(&mut len, optional_string_encoded_len(compiler_options.target.as_ref())?)?;
  checked_add_len(&mut len, optional_string_encoded_len(compiler_options.module.as_ref())?)?;
  checked_add_len(&mut len, 1)?;
  checked_add_len(&mut len, optional_string_vec_encoded_len(compiler_options.root_dirs.as_ref())?)?;
  Some(len)
}

fn optional_path_mappings_encoded_len(
  mappings: Option<&Vec<DxTsconfigPathMappingPayload>>,
) -> Option<usize> {
  let Some(mappings) = mappings else {
    return Some(4);
  };
  let len = u32::try_from(mappings.len()).ok()?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }

  let mut encoded_len = 4;
  for mapping in mappings {
    checked_add_len(&mut encoded_len, string_encoded_len(&mapping.key)?)?;
    checked_add_len(&mut encoded_len, string_vec_encoded_len(&mapping.values)?)?;
  }
  Some(encoded_len)
}

fn optional_string_vec_encoded_len(values: Option<&Vec<String>>) -> Option<usize> {
  values.map_or(Some(4), |values| string_vec_encoded_len(values))
}

fn string_vec_encoded_len(values: &[String]) -> Option<usize> {
  let len = u32::try_from(values.len()).ok()?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }

  let mut encoded_len = 4;
  for value in values {
    checked_add_len(&mut encoded_len, string_encoded_len(value)?)?;
  }
  Some(encoded_len)
}

fn optional_string_encoded_len(value: Option<&String>) -> Option<usize> {
  value.map_or(Some(4), |value| string_encoded_len(value))
}

fn string_encoded_len(value: &str) -> Option<usize> {
  let len = u32::try_from(value.len()).ok()?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }

  4_usize.checked_add(value.len())
}

fn checked_add_len(total: &mut usize, value: usize) -> Option<()> {
  *total = total.checked_add(value)?;
  Some(())
}

fn decode_dx_tsconfig_machine_payload(machine_bytes: &[u8]) -> Option<DxTsconfigMachinePayload> {
  decode_dx_tsconfig_machine_payload_with_expected_source_hash(machine_bytes, None)
}

fn decode_dx_tsconfig_machine_payload_with_source_hash(
  machine_bytes: &[u8],
  source_hash: &blake3::Hash,
) -> Option<DxTsconfigMachinePayload> {
  decode_dx_tsconfig_machine_payload_with_expected_source_hash(machine_bytes, Some(source_hash))
}

fn decode_dx_tsconfig_machine_payload_with_expected_source_hash(
  machine_bytes: &[u8],
  expected_source_hash: Option<&blake3::Hash>,
) -> Option<DxTsconfigMachinePayload> {
  if machine_bytes.len() < DX_TSCONFIG_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..DX_TSCONFIG_MACHINE_MAGIC.len()] != DX_TSCONFIG_MACHINE_MAGIC {
    return None;
  }
  if machine_bytes[8..12] != [0, 0, 0, 0] {
    return None;
  }

  let mut cursor = DX_TSCONFIG_MACHINE_HEADER_LEN;
  let source_blake3 = read_source_hash(machine_bytes, &mut cursor)?;
  if expected_source_hash.is_some_and(|hash| source_blake3 != *hash.as_bytes()) {
    return None;
  }
  if !dx_tsconfig_machine_payload_layout_is_valid(machine_bytes) {
    return None;
  }

  let files = read_optional_string_vec(machine_bytes, &mut cursor)?;
  let include = read_optional_string_vec(machine_bytes, &mut cursor)?;
  let exclude = read_optional_string_vec(machine_bytes, &mut cursor)?;
  let compiler_options = decode_dx_tsconfig_compiler_options(machine_bytes, &mut cursor)?;
  if cursor != machine_bytes.len() {
    return None;
  }

  Some(DxTsconfigMachinePayload {
    schema: DX_TSCONFIG_MACHINE_SCHEMA,
    source_blake3,
    files,
    include,
    exclude,
    compiler_options,
  })
}

fn dx_tsconfig_machine_payload_layout_is_valid(machine_bytes: &[u8]) -> bool {
  #[cfg(test)]
  DX_TSCONFIG_TEST_PAYLOAD_LAYOUT_VALIDATIONS.with(|count| count.set(count.get() + 1));

  let mut cursor = DX_TSCONFIG_MACHINE_HEADER_LEN;
  skip_source_hash(machine_bytes, &mut cursor)
    .and_then(|()| skip_optional_string_vec(machine_bytes, &mut cursor))
    .and_then(|()| skip_optional_string_vec(machine_bytes, &mut cursor))
    .and_then(|()| skip_optional_string_vec(machine_bytes, &mut cursor))
    .and_then(|()| skip_dx_tsconfig_compiler_options(machine_bytes, &mut cursor))
    .is_some_and(|()| cursor == machine_bytes.len())
}

fn encode_dx_tsconfig_compiler_options(
  machine_bytes: &mut Vec<u8>,
  compiler_options: &DxTsconfigCompilerOptionsPayload,
) -> Option<()> {
  write_optional_string(machine_bytes, compiler_options.base_url.as_ref())?;
  write_optional_path_mappings(machine_bytes, compiler_options.paths.as_ref())?;
  write_optional_bool(machine_bytes, compiler_options.experimental_decorators);
  write_optional_bool(machine_bytes, compiler_options.emit_decorator_metadata);
  write_optional_bool(machine_bytes, compiler_options.use_define_for_class_fields);
  write_optional_bool(machine_bytes, compiler_options.rewrite_relative_import_extensions);
  write_optional_string(machine_bytes, compiler_options.jsx.as_ref())?;
  write_optional_string(machine_bytes, compiler_options.jsx_factory.as_ref())?;
  write_optional_string(machine_bytes, compiler_options.jsx_fragment_factory.as_ref())?;
  write_optional_string(machine_bytes, compiler_options.jsx_import_source.as_ref())?;
  write_optional_bool(machine_bytes, compiler_options.verbatim_module_syntax);
  write_optional_bool(machine_bytes, compiler_options.preserve_value_imports);
  write_optional_string(machine_bytes, compiler_options.imports_not_used_as_values.as_ref())?;
  write_optional_string(machine_bytes, compiler_options.target.as_ref())?;
  write_optional_string(machine_bytes, compiler_options.module.as_ref())?;
  write_optional_bool(machine_bytes, compiler_options.allow_js);
  write_optional_string_vec(machine_bytes, compiler_options.root_dirs.as_ref())
}

fn decode_dx_tsconfig_compiler_options(
  machine_bytes: &[u8],
  cursor: &mut usize,
) -> Option<DxTsconfigCompilerOptionsPayload> {
  Some(DxTsconfigCompilerOptionsPayload {
    base_url: read_optional_string(machine_bytes, cursor)?,
    paths: read_optional_path_mappings(machine_bytes, cursor)?,
    experimental_decorators: read_optional_bool(machine_bytes, cursor)?,
    emit_decorator_metadata: read_optional_bool(machine_bytes, cursor)?,
    use_define_for_class_fields: read_optional_bool(machine_bytes, cursor)?,
    rewrite_relative_import_extensions: read_optional_bool(machine_bytes, cursor)?,
    jsx: read_optional_string(machine_bytes, cursor)?,
    jsx_factory: read_optional_string(machine_bytes, cursor)?,
    jsx_fragment_factory: read_optional_string(machine_bytes, cursor)?,
    jsx_import_source: read_optional_string(machine_bytes, cursor)?,
    verbatim_module_syntax: read_optional_bool(machine_bytes, cursor)?,
    preserve_value_imports: read_optional_bool(machine_bytes, cursor)?,
    imports_not_used_as_values: read_optional_string(machine_bytes, cursor)?,
    target: read_optional_string(machine_bytes, cursor)?,
    module: read_optional_string(machine_bytes, cursor)?,
    allow_js: read_optional_bool(machine_bytes, cursor)?,
    root_dirs: read_optional_string_vec(machine_bytes, cursor)?,
  })
}

fn skip_dx_tsconfig_compiler_options(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_path_mappings(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_string(machine_bytes, cursor)?;
  skip_optional_bool(machine_bytes, cursor)?;
  skip_optional_string_vec(machine_bytes, cursor)
}

fn write_optional_path_mappings(
  machine_bytes: &mut Vec<u8>,
  mappings: Option<&Vec<DxTsconfigPathMappingPayload>>,
) -> Option<()> {
  let Some(mappings) = mappings else {
    machine_bytes.extend_from_slice(&DX_TSCONFIG_NONE_LEN.to_le_bytes());
    return Some(());
  };

  let len = u32::try_from(mappings.len()).ok()?;
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  for mapping in mappings {
    write_string(machine_bytes, &mapping.key)?;
    write_string_vec(machine_bytes, &mapping.values)?;
  }
  Some(())
}

fn read_optional_path_mappings(
  machine_bytes: &[u8],
  cursor: &mut usize,
) -> Option<Option<Vec<DxTsconfigPathMappingPayload>>> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(None);
  }

  let len = usize::try_from(len).ok()?;
  if !declared_path_mapping_count_fits_remaining(machine_bytes, *cursor, len) {
    return None;
  }

  let mut mappings = Vec::with_capacity(len);
  for _ in 0..len {
    mappings.push(DxTsconfigPathMappingPayload {
      key: read_string(machine_bytes, cursor)?,
      values: read_string_vec(machine_bytes, cursor)?,
    });
  }
  Some(Some(mappings))
}

fn skip_optional_path_mappings(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(());
  }

  let len = usize::try_from(len).ok()?;
  if !declared_path_mapping_count_fits_remaining(machine_bytes, *cursor, len) {
    return None;
  }

  for _ in 0..len {
    skip_string(machine_bytes, cursor)?;
    skip_string_vec(machine_bytes, cursor)?;
  }
  Some(())
}

fn write_optional_string_vec(
  machine_bytes: &mut Vec<u8>,
  values: Option<&Vec<String>>,
) -> Option<()> {
  match values {
    Some(values) => write_string_vec(machine_bytes, values),
    None => {
      machine_bytes.extend_from_slice(&DX_TSCONFIG_NONE_LEN.to_le_bytes());
      Some(())
    }
  }
}

fn write_string_vec(machine_bytes: &mut Vec<u8>, values: &[String]) -> Option<()> {
  let len = u32::try_from(values.len()).ok()?;
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  for value in values {
    write_string(machine_bytes, value)?;
  }
  Some(())
}

fn read_optional_string_vec(
  machine_bytes: &[u8],
  cursor: &mut usize,
) -> Option<Option<Vec<String>>> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(None);
  }

  read_string_vec_with_len(machine_bytes, cursor, len).map(Some)
}

fn skip_optional_string_vec(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(());
  }

  skip_string_vec_with_len(machine_bytes, cursor, len)
}

fn read_string_vec(machine_bytes: &[u8], cursor: &mut usize) -> Option<Vec<String>> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }
  read_string_vec_with_len(machine_bytes, cursor, len)
}

fn skip_string_vec(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }
  skip_string_vec_with_len(machine_bytes, cursor, len)
}

fn read_string_vec_with_len(
  machine_bytes: &[u8],
  cursor: &mut usize,
  len: u32,
) -> Option<Vec<String>> {
  let len = usize::try_from(len).ok()?;
  if !declared_string_count_fits_remaining(machine_bytes, *cursor, len) {
    return None;
  }

  let mut values = Vec::with_capacity(len);
  for _ in 0..len {
    values.push(read_string(machine_bytes, cursor)?);
  }
  Some(values)
}

fn skip_string_vec_with_len(machine_bytes: &[u8], cursor: &mut usize, len: u32) -> Option<()> {
  let len = usize::try_from(len).ok()?;
  if !declared_string_count_fits_remaining(machine_bytes, *cursor, len) {
    return None;
  }

  for _ in 0..len {
    skip_string(machine_bytes, cursor)?;
  }
  Some(())
}

fn declared_string_count_fits_remaining(machine_bytes: &[u8], cursor: usize, count: usize) -> bool {
  count <= machine_bytes.len().saturating_sub(cursor) / 4
}

fn declared_path_mapping_count_fits_remaining(
  machine_bytes: &[u8],
  cursor: usize,
  count: usize,
) -> bool {
  count <= machine_bytes.len().saturating_sub(cursor) / 8
}

fn write_optional_string(machine_bytes: &mut Vec<u8>, value: Option<&String>) -> Option<()> {
  match value {
    Some(value) => write_string(machine_bytes, value),
    None => {
      machine_bytes.extend_from_slice(&DX_TSCONFIG_NONE_LEN.to_le_bytes());
      Some(())
    }
  }
}

fn write_string(machine_bytes: &mut Vec<u8>, value: &str) -> Option<()> {
  let len = u32::try_from(value.len()).ok()?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  machine_bytes.extend_from_slice(value.as_bytes());
  Some(())
}

fn read_optional_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<Option<String>> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(None);
  }
  read_string_with_len(machine_bytes, cursor, len).map(Some)
}

fn skip_optional_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return Some(());
  }
  skip_string_with_len(machine_bytes, cursor, len)
}

fn read_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<String> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }
  read_string_with_len(machine_bytes, cursor, len)
}

fn skip_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == DX_TSCONFIG_NONE_LEN {
    return None;
  }
  skip_string_with_len(machine_bytes, cursor, len)
}

fn read_string_with_len(machine_bytes: &[u8], cursor: &mut usize, len: u32) -> Option<String> {
  let len = usize::try_from(len).ok()?;
  let end = cursor.checked_add(len)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  *cursor = end;
  let value = std::str::from_utf8(bytes).ok()?;
  #[cfg(test)]
  DX_TSCONFIG_TEST_STRING_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
  Some(value.to_string())
}

fn skip_string_with_len(machine_bytes: &[u8], cursor: &mut usize, len: u32) -> Option<()> {
  let len = usize::try_from(len).ok()?;
  let end = cursor.checked_add(len)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  std::str::from_utf8(bytes).ok()?;
  *cursor = end;
  Some(())
}

fn write_optional_bool(machine_bytes: &mut Vec<u8>, value: Option<bool>) {
  machine_bytes.push(match value {
    None => 0,
    Some(false) => 1,
    Some(true) => 2,
  });
}

fn read_optional_bool(machine_bytes: &[u8], cursor: &mut usize) -> Option<Option<bool>> {
  let value = *machine_bytes.get(*cursor)?;
  *cursor = cursor.checked_add(1)?;
  match value {
    0 => Some(None),
    1 => Some(Some(false)),
    2 => Some(Some(true)),
    _ => None,
  }
}

fn skip_optional_bool(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  match *machine_bytes.get(*cursor)? {
    0..=2 => {
      *cursor = cursor.checked_add(1)?;
      Some(())
    }
    _ => None,
  }
}

fn read_u32(machine_bytes: &[u8], cursor: &mut usize) -> Option<u32> {
  let end = cursor.checked_add(4)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  *cursor = end;
  Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_source_hash(
  machine_bytes: &[u8],
  cursor: &mut usize,
) -> Option<[u8; DX_TSCONFIG_SOURCE_HASH_LEN]> {
  let end = cursor.checked_add(DX_TSCONFIG_SOURCE_HASH_LEN)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  *cursor = end;
  bytes.try_into().ok()
}

fn skip_source_hash(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let end = cursor.checked_add(DX_TSCONFIG_SOURCE_HASH_LEN)?;
  machine_bytes.get(*cursor..end)?;
  *cursor = end;
  Some(())
}

fn read_dx_tsconfig_machine_cache(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  tsconfig_path: &Path,
  source_bytes: &[u8],
) -> Option<Arc<TsConfig>> {
  read_dx_tsconfig_machine_cache_or_repair_hash(
    cache_config,
    project_root,
    tsconfig_path,
    source_bytes,
  )
  .and_then(|read| read.tsconfig)
}

struct DxTsconfigMachineCacheRead {
  tsconfig: Option<Arc<TsConfig>>,
  repair_source_hash: Option<blake3::Hash>,
}

fn read_dx_tsconfig_machine_cache_or_repair_hash(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  tsconfig_path: &Path,
  source_bytes: &[u8],
) -> Option<DxTsconfigMachineCacheRead> {
  if source_bytes.len() <= DX_TSCONFIG_MACHINE_MIN_SOURCE_BYTES {
    return None;
  }
  let cache_paths =
    dx_tsconfig_machine_cache_paths_if_enabled(cache_config, project_root, tsconfig_path)?;

  let DxMachineCacheStatus::Hit(hit) =
    cache_config.read_validated_machine_with_source_hash(&cache_paths, tsconfig_path, source_bytes)
  else {
    return None;
  };

  let payload =
    decode_dx_tsconfig_machine_payload_with_source_hash(&hit.machine_bytes, &hit.source_hash);
  Some(
    match payload.and_then(|payload| payload.to_tsconfig_without_source_validation(tsconfig_path)) {
      Some(tsconfig) => {
        DxTsconfigMachineCacheRead { tsconfig: Some(Arc::new(tsconfig)), repair_source_hash: None }
      }
      None => {
        DxTsconfigMachineCacheRead { tsconfig: None, repair_source_hash: Some(hit.source_hash) }
      }
    },
  )
}

fn write_dx_tsconfig_machine_cache(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  tsconfig_path: &Path,
  source_bytes: &[u8],
  tsconfig: &TsConfig,
) {
  write_dx_tsconfig_machine_cache_with_optional_source_hash(
    cache_config,
    project_root,
    tsconfig_path,
    source_bytes,
    tsconfig,
    None,
  );
}

fn write_dx_tsconfig_machine_cache_with_optional_source_hash(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  tsconfig_path: &Path,
  source_bytes: &[u8],
  tsconfig: &TsConfig,
  source_hash: Option<&blake3::Hash>,
) {
  if source_bytes.len() <= DX_TSCONFIG_MACHINE_MIN_SOURCE_BYTES {
    return;
  }
  if !cache_config.enabled {
    return;
  }
  if tsconfig_source_has_external_dependencies(source_bytes) {
    return;
  }
  let Some(cache_paths) =
    dx_tsconfig_machine_cache_paths_if_enabled(cache_config, project_root, tsconfig_path)
  else {
    return;
  };

  let computed_source_hash;
  let source_hash = match source_hash {
    Some(source_hash) => source_hash,
    None => {
      computed_source_hash = hash_tsconfig_source_for_write(source_bytes);
      &computed_source_hash
    }
  };
  let payload = DxTsconfigMachinePayload::from_tsconfig_with_source_hash(tsconfig, source_hash);
  let Some(machine_bytes) = encode_dx_tsconfig_machine_payload(&payload) else {
    return;
  };
  let _ = cache_config.write_machine_artifact_with_source_hash(
    &cache_paths,
    tsconfig_path,
    source_bytes.len(),
    source_hash,
    &machine_bytes,
  );
}

fn hash_tsconfig_source_for_write(source_bytes: &[u8]) -> blake3::Hash {
  #[cfg(test)]
  DX_TSCONFIG_TEST_WRITE_SOURCE_HASHES.with(|count| count.set(count.get() + 1));

  blake3::hash(source_bytes)
}

fn dx_tsconfig_machine_cache_paths_if_enabled(
  cache_config: &DxMachineCacheConfig,
  project_root: &Path,
  tsconfig_path: &Path,
) -> Option<DxMachineCachePaths> {
  cache_config.enabled.then(|| {
    #[cfg(test)]
    DX_TSCONFIG_TEST_CACHE_PATH_BUILDS.with(|count| count.set(count.get() + 1));

    cache_config.paths_for_source(project_root, DX_TSCONFIG_CACHE_NAMESPACE, tsconfig_path)
  })
}

fn tsconfig_source_has_external_dependencies(source_bytes: &[u8]) -> bool {
  #[cfg(test)]
  DX_TSCONFIG_TEST_DEPENDENCY_PREFLIGHT_SCANS.with(|count| count.set(count.get() + 1));

  let mut json = source_bytes.to_vec();
  replace_utf8_bom_with_whitespace(&mut json);
  _ = json_strip_comments::strip_slice(&mut json);

  let Ok(has_dependencies) = serde_json::from_slice::<TsconfigExternalDependencies>(&json) else {
    return true;
  };

  has_dependencies.0
}

struct TsconfigExternalDependencies(bool);

impl<'de> Deserialize<'de> for TsconfigExternalDependencies {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    deserializer.deserialize_any(TsconfigExternalDependenciesVisitor)
  }
}

struct TsconfigExternalDependenciesVisitor;

impl<'de> Visitor<'de> for TsconfigExternalDependenciesVisitor {
  type Value = TsconfigExternalDependencies;

  fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str("a tsconfig JSON value")
  }

  fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
  where
    A: MapAccess<'de>,
  {
    let mut has_external_dependencies = false;
    while let Some(key) = map.next_key::<Cow<'de, str>>()? {
      if key.as_ref() == "extends" || key.as_ref() == "references" {
        has_external_dependencies = true;
      }
      map.next_value::<IgnoredAny>()?;
    }
    Ok(TsconfigExternalDependencies(has_external_dependencies))
  }

  fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
  where
    A: SeqAccess<'de>,
  {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_unit<E>(self) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_none<E>(self) -> Result<Self::Value, E> {
    Ok(TsconfigExternalDependencies(false))
  }

  fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    deserializer.deserialize_any(self)
  }
}

fn replace_utf8_bom_with_whitespace(json: &mut [u8]) {
  if json.starts_with(&[0xEF, 0xBB, 0xBF]) {
    json[0] = b' ';
    json[1] = b' ';
    json[2] = b' ';
  }
}

fn tsconfig_search_directory(file_path: &Path) -> Option<PathBuf> {
  let absolute_file_path = if file_path.is_absolute() {
    file_path.to_path_buf()
  } else {
    std::env::current_dir().ok()?.join(file_path)
  };
  absolute_file_path.parent().map(Path::to_path_buf)
}

fn tsconfig_file_path_is_dx_cache_eligible(file_path: &Path) -> bool {
  file_path.is_absolute()
    && !file_path.components().any(
      |component| matches!(component, std::path::Component::Normal(name) if name == "node_modules"),
    )
}

fn dx_machine_cache_project_root(path: &Path) -> PathBuf {
  std::env::current_dir()
    .ok()
    .or_else(|| path.parent().map(Path::to_path_buf))
    .unwrap_or_else(|| PathBuf::from("."))
}

fn pathbufs_to_strings(paths: &Option<Vec<PathBuf>>) -> Option<Vec<String>> {
  paths.as_ref().map(|paths| paths.iter().map(pathbuf_to_string).collect())
}

fn pathbuf_to_string(path: &PathBuf) -> String {
  path.to_string_lossy().to_string()
}

#[cfg(test)]
fn source_blake3(source_bytes: &[u8]) -> [u8; DX_TSCONFIG_SOURCE_HASH_LEN] {
  *blake3::hash(source_bytes).as_bytes()
}

#[cfg(test)]
fn source_blake3_matches(
  expected: &[u8; DX_TSCONFIG_SOURCE_HASH_LEN],
  source_bytes: &[u8],
) -> bool {
  expected == blake3::hash(source_bytes).as_bytes()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::{Mutex, MutexGuard};

  static TSCONFIG_PROCESS_STATE_LOCK: Mutex<()> = Mutex::new(());

  #[test]
  fn test_cache_creation() {
    let cache = TsconfigCache::new(false);
    assert_eq!(cache.size(), 0);
  }

  #[test]
  fn test_cache_clear() {
    let cache = TsconfigCache::new(false);
    cache.nearest_tsconfig_path_cache.insert(PathBuf::from("src"), PathBuf::from("tsconfig.json"));
    cache.clear();
    assert_eq!(cache.size(), 0);
    assert!(cache.nearest_tsconfig_path_cache.is_empty());
  }

  #[test]
  fn tsconfig_machine_payload_round_trips_derived_transform_options() {
    let root = unique_temp_root("payload");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{
      "compilerOptions": {
        "jsx": "react-jsx",
        "jsxImportSource": "react",
        "experimentalDecorators": true,
        "useDefineForClassFields": false,
        "target": "es2019"
      }
    }"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();

    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let restored = payload.to_tsconfig(&tsconfig_path, source.as_bytes()).unwrap();

    assert_eq!(restored.path, tsconfig_path);
    assert_eq!(restored.compiler_options.jsx.as_deref(), Some("react-jsx"));
    assert_eq!(restored.compiler_options.jsx_import_source.as_deref(), Some("react"));
    assert_eq!(restored.compiler_options.experimental_decorators, Some(true));
    assert_eq!(restored.compiler_options.use_define_for_class_fields, Some(false));
    assert_eq!(restored.compiler_options.target.as_deref(), Some("es2019"));

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_round_trips_compact_binary() {
    let root = unique_temp_root("binary-payload");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{
      "files": ["src/main.ts"],
      "include": ["src"],
      "exclude": ["dist"],
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx",
        "jsxImportSource": "react",
        "experimentalDecorators": true,
        "useDefineForClassFields": false,
        "rootDirs": ["src", "generated"]
      }
    }"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();

    assert!(machine_bytes.starts_with(DX_TSCONFIG_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));

    let restored_payload = decode_dx_tsconfig_machine_payload(&machine_bytes).unwrap();
    let restored = restored_payload.to_tsconfig(&tsconfig_path, source.as_bytes()).unwrap();
    assert_eq!(restored.compiler_options.base_url, tsconfig.compiler_options.base_url);
    assert_eq!(restored.compiler_options.paths, tsconfig.compiler_options.paths);
    assert_eq!(restored.compiler_options.root_dirs, tsconfig.compiler_options.root_dirs);
    assert_eq!(restored.compiler_options.jsx.as_deref(), Some("react-jsx"));
    assert_eq!(restored.compiler_options.jsx_import_source.as_deref(), Some("react"));
    assert_eq!(restored.compiler_options.experimental_decorators, Some(true));
    assert_eq!(restored.compiler_options.use_define_for_class_fields, Some(false));

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_stores_raw_source_hash_bytes() {
    let root = unique_temp_root("raw-source-hash");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let source_hash_hex = source_hash.to_hex().to_string();

    assert!(machine_bytes.starts_with(b"RDXTSC03"));
    assert_eq!(
      &machine_bytes[DX_TSCONFIG_MACHINE_HEADER_LEN
        ..DX_TSCONFIG_MACHINE_HEADER_LEN + source_hash.as_bytes().len()],
      source_hash.as_bytes()
    );
    assert!(
      !machine_bytes
        .windows(source_hash_hex.len())
        .any(|window| window == source_hash_hex.as_bytes())
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_rejects_old_magic() {
    let root = unique_temp_root("old-magic");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let mut machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();

    machine_bytes[..DX_TSCONFIG_MACHINE_MAGIC.len()].copy_from_slice(b"RDXTSC01");

    assert!(decode_dx_tsconfig_machine_payload(&machine_bytes).is_none());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_rejects_previous_dependency_preflight_magic() {
    let root = unique_temp_root("previous-preflight-magic");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let mut machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();

    machine_bytes[..DX_TSCONFIG_MACHINE_MAGIC.len()].copy_from_slice(b"RDXTSC02");

    assert!(decode_dx_tsconfig_machine_payload(&machine_bytes).is_none());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_rejects_trailing_bytes_before_string_materialization() {
    let root = unique_temp_root("trailing-before-materialization");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{
      "files": ["src/main.ts"],
      "include": ["src"],
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx",
        "rootDirs": ["src"]
      }
    }"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let mut machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();
    machine_bytes.push(0);

    reset_tsconfig_string_materialization_count();

    assert!(decode_dx_tsconfig_machine_payload(&machine_bytes).is_none());
    assert_eq!(tsconfig_string_materialization_count(), 0);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_encoder_preallocates_exact_payload_len() {
    let root = unique_temp_root("binary-capacity");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{
      "files": ["src/main.ts"],
      "include": ["src"],
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx"
      }
    }"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());
    let machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();
    let encoded_len = dx_tsconfig_machine_payload_encoded_len(&payload).unwrap();

    assert_eq!(machine_bytes.len(), encoded_len);
    assert_eq!(machine_bytes.capacity(), encoded_len);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_rejects_impossible_string_vec_count_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(&16_u32.to_le_bytes());
    let mut cursor = 4;

    assert!(!declared_string_count_fits_remaining(&machine_bytes, cursor, 16));
    assert!(read_string_vec_with_len(&machine_bytes, &mut cursor, 16).is_none());
  }

  #[test]
  fn tsconfig_machine_payload_rejects_impossible_path_mapping_count_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(&16_u32.to_le_bytes());
    let mut cursor = 0;

    assert!(!declared_path_mapping_count_fits_remaining(&machine_bytes, cursor + 4, 16));
    assert!(read_optional_path_mappings(&machine_bytes, &mut cursor).is_none());
  }

  #[test]
  fn tsconfig_machine_source_hash_matches_without_allocating_expected_string() {
    let source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let expected = source_blake3(source);
    let source_hash = blake3::hash(source);
    let source_hash_hex = source_hash.to_hex().to_string();

    assert_eq!(expected, *source_hash.as_bytes());
    assert!(source_blake3_matches(&expected, source));
    assert!(rolldown_utils::dx_machine_cache::blake3_hex_matches_hash(
      &source_hash_hex,
      &source_hash
    ));
    assert!(!rolldown_utils::dx_machine_cache::blake3_hex_matches_hash(
      &source_hash_hex.to_ascii_uppercase(),
      &source_hash
    ));
    assert!(!source_blake3_matches(&expected, br#"{"compilerOptions":{"jsx":"preserve"}}"#));
  }

  #[test]
  fn tsconfig_machine_payload_accepts_prevalidated_source_hash() {
    let root = unique_temp_root("prevalidated-source-hash");
    let tsconfig_path = root.join("tsconfig.json");
    let source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(source.to_vec()).unwrap()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source);
    let source_hash = blake3::hash(source);

    assert!(payload.to_tsconfig_with_source_hash(&tsconfig_path, &source_hash).is_some());
    assert!(
      payload
        .to_tsconfig_with_source_hash(
          &tsconfig_path,
          &blake3::hash(br#"{"compilerOptions":{"jsx":"preserve"}}"#),
        )
        .is_none()
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_accepts_known_source_hash_on_write() {
    let root = unique_temp_root("known-source-hash");
    let tsconfig_path = root.join("tsconfig.json");
    let source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(source.to_vec()).unwrap()).unwrap();
    let source_hash = blake3::hash(source);

    let payload = DxTsconfigMachinePayload::from_tsconfig_with_source_hash(&tsconfig, &source_hash);

    assert_eq!(payload.source_blake3, *source_hash.as_bytes());
    assert!(payload.to_tsconfig_with_source_hash(&tsconfig_path, &source_hash).is_some());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_borrows_static_schema_without_string_allocation() {
    let root = unique_temp_root("static-schema");
    let tsconfig_path = root.join("tsconfig.json");
    let source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(source.to_vec()).unwrap()).unwrap();
    let source_hash = blake3::hash(source);

    let payload = DxTsconfigMachinePayload::from_tsconfig_with_source_hash(&tsconfig, &source_hash);

    assert_eq!(payload.schema, DX_TSCONFIG_MACHINE_SCHEMA);
    assert_eq!(std::mem::size_of_val(&payload.schema), std::mem::size_of::<&'static str>());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_reconstructs_without_dynamic_json_object_tree() {
    let root = unique_temp_root("direct-json-writer");
    let tsconfig_path = root.join("tsconfig.json");
    let source = r#"{
      "files": ["src/main.ts"],
      "include": ["src"],
      "exclude": ["dist"],
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx",
        "experimentalDecorators": true,
        "useDefineForClassFields": false,
        "rootDirs": ["src", "generated"]
      }
    }"#;
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.to_string()).unwrap();
    let payload = DxTsconfigMachinePayload::from_tsconfig(&tsconfig, source.as_bytes());

    reset_tsconfig_dynamic_json_object_build_count();

    let restored = payload.to_tsconfig(&tsconfig_path, source.as_bytes()).unwrap();

    assert_eq!(restored.compiler_options.base_url, tsconfig.compiler_options.base_url);
    assert_eq!(restored.compiler_options.paths, tsconfig.compiler_options.paths);
    assert_eq!(restored.compiler_options.jsx, tsconfig.compiler_options.jsx);
    assert_eq!(
      restored.compiler_options.experimental_decorators,
      tsconfig.compiler_options.experimental_decorators
    );
    assert_eq!(
      restored.compiler_options.use_define_for_class_fields,
      tsconfig.compiler_options.use_define_for_class_fields
    );
    assert_eq!(restored.compiler_options.root_dirs, tsconfig.compiler_options.root_dirs);
    assert_eq!(tsconfig_dynamic_json_object_build_count(), 0);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_payload_direct_json_writer_escapes_strings_and_preserves_empty_values() {
    let payload = DxTsconfigMachinePayload {
      schema: DX_TSCONFIG_MACHINE_SCHEMA,
      source_blake3: [0; DX_TSCONFIG_SOURCE_HASH_LEN],
      files: Some(vec![]),
      include: None,
      exclude: Some(vec![
        "quote\"dir".to_string(),
        "line\nbreak".to_string(),
        "unicode-নদী".to_string(),
      ]),
      compiler_options: DxTsconfigCompilerOptionsPayload {
        base_url: Some(r#"G:\Dx\quote\"#.to_string()),
        paths: Some(vec![DxTsconfigPathMappingPayload {
          key: "@scope/*\n".to_string(),
          values: vec!["src\\quote\"/*".to_string(), "নদী".to_string()],
        }]),
        experimental_decorators: Some(false),
        use_define_for_class_fields: Some(false),
        root_dirs: Some(vec![]),
        ..Default::default()
      },
    };

    reset_tsconfig_dynamic_json_object_build_count();

    let json = payload.to_tsconfig_json_string().unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    let compiler_options = parsed.get("compilerOptions").unwrap();
    let paths = compiler_options.get("paths").unwrap();
    let scoped_values = paths.get("@scope/*\n").unwrap().as_array().unwrap();

    assert_eq!(parsed.get("files").unwrap().as_array().unwrap().len(), 0);
    assert!(parsed.get("include").is_none());
    assert_eq!(parsed["exclude"][0], "quote\"dir");
    assert_eq!(parsed["exclude"][1], "line\nbreak");
    assert_eq!(compiler_options["baseUrl"], r#"G:\Dx\quote\"#);
    assert_eq!(compiler_options["experimentalDecorators"], false);
    assert_eq!(compiler_options["useDefineForClassFields"], false);
    assert_eq!(compiler_options["rootDirs"].as_array().unwrap().len(), 0);
    assert_eq!(scoped_values[0], "src\\quote\"/*");
    assert_eq!(scoped_values[1], "নদী");
    assert_eq!(tsconfig_dynamic_json_object_build_count(), 0);
  }

  #[test]
  fn tsconfig_machine_cache_invalidates_source_mutation() {
    let root = unique_temp_root("mutation");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(r#"{"compilerOptions":{"jsx":"react-jsx"}}"#);
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );

    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    assert!(
      read_dx_tsconfig_machine_cache(
        &cache_config,
        &project_root,
        &tsconfig_path,
        br#"{"compilerOptions":{"jsx":"preserve"}}"#,
      )
      .is_none()
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_rejects_payload_source_hash_before_string_materialization() {
    let root = unique_temp_root("payload-hash-before-strings");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
        "files": ["src/entry.ts"],
        "compilerOptions": {
          "baseUrl": ".",
          "paths": { "@app/*": ["src/*"] },
          "jsx": "react-jsx"
        }
      }"#,
    );
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let payload = DxTsconfigMachinePayload::from_tsconfig_with_source_hash(
      &tsconfig,
      &blake3::hash(b"stale payload source"),
    );
    let machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();
    cache_config
      .write_machine_artifact_with_source_hash(
        &cache_paths,
        &tsconfig_path,
        source.len(),
        &source_hash,
        &machine_bytes,
      )
      .unwrap();

    reset_tsconfig_string_materialization_count();
    assert!(
      read_dx_tsconfig_machine_cache(
        &cache_config,
        &project_root,
        &tsconfig_path,
        source.as_bytes(),
      )
      .is_none()
    );
    assert_eq!(tsconfig_string_materialization_count(), 0);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_rejects_payload_source_hash_before_layout_validation() {
    let root = unique_temp_root("payload-hash-before-layout");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
        "files": ["src/entry.ts"],
        "compilerOptions": {
          "baseUrl": ".",
          "paths": { "@app/*": ["src/*"] },
          "jsx": "react-jsx"
        }
      }"#,
    );
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let payload = DxTsconfigMachinePayload::from_tsconfig_with_source_hash(
      &tsconfig,
      &blake3::hash(b"stale payload source"),
    );
    let machine_bytes = encode_dx_tsconfig_machine_payload(&payload).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();
    cache_config
      .write_machine_artifact_with_source_hash(
        &cache_paths,
        &tsconfig_path,
        source.len(),
        &source_hash,
        &machine_bytes,
      )
      .unwrap();

    reset_tsconfig_payload_layout_validation_count();
    assert!(
      read_dx_tsconfig_machine_cache(
        &cache_config,
        &project_root,
        &tsconfig_path,
        source.as_bytes(),
      )
      .is_none()
    );
    assert_eq!(tsconfig_payload_layout_validation_count(), 0);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_reuses_validated_source_hash_for_decode_miss_repair() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("decode-miss-repair-hash-reuse");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source_path = project_root.join("src").join("entry.ts");
    let source = large_tsconfig_source(
      r#"{
        "files": ["src/entry.ts"],
        "compilerOptions": {
          "baseUrl": ".",
          "paths": { "@app/*": ["src/*"] },
          "jsx": "react-jsx"
        }
      }"#,
    );
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, "export {};\n").unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let source_hash = blake3::hash(source.as_bytes());
    let stale_machine = b"validated-but-undecodable";
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();
    cache_config
      .write_machine_artifact_with_source_hash(
        &cache_paths,
        &tsconfig_path,
        source.len(),
        &source_hash,
        stale_machine,
      )
      .unwrap();

    let _current_dir = CurrentDirGuard::set(&project_root);
    let _cache_env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache = TsconfigCache::new(false);

    reset_tsconfig_write_source_hash_count();
    let found = cache.find_tsconfig(&source_path).unwrap().unwrap();

    assert_eq!(found.path, tsconfig_path);
    assert_eq!(tsconfig_write_source_hash_count(), 0);
    assert_ne!(std::fs::read(&cache_paths.machine).unwrap(), stale_machine);
    assert!(
      read_dx_tsconfig_machine_cache(
        &cache_config,
        &project_root,
        &tsconfig_path,
        source.as_bytes(),
      )
      .is_some()
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_find_tsconfig_refreshes_stale_source_artifact() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("stale-source-repair");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source_path = project_root.join("src").join("entry.ts");
    let old_source = large_tsconfig_source(
      r#"{
        "files": ["src/entry.ts"],
        "compilerOptions": { "jsx": "react-jsx" }
      }"#,
    );
    let new_source = large_tsconfig_source(
      r#"{
        "files": ["src/entry.ts"],
        "compilerOptions": { "jsx": "preserve" }
      }"#,
    );
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, "export {};\n").unwrap();
    std::fs::write(&tsconfig_path, old_source.as_bytes()).unwrap();
    let old_tsconfig = TsConfig::parse(true, &tsconfig_path, old_source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();

    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      old_source.as_bytes(),
      &old_tsconfig,
    );
    let old_machine = std::fs::read(&cache_paths.machine).unwrap();
    std::fs::write(&tsconfig_path, new_source.as_bytes()).unwrap();

    let _current_dir = CurrentDirGuard::set(&project_root);
    let _cache_env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache = TsconfigCache::new(false);

    reset_tsconfig_write_source_hash_count();
    let found = cache.find_tsconfig(&source_path).unwrap().unwrap();

    assert_eq!(found.path, tsconfig_path);
    assert_eq!(found.compiler_options.jsx.as_deref(), Some("preserve"));
    assert_eq!(tsconfig_write_source_hash_count(), 1);
    assert_ne!(std::fs::read(&cache_paths.machine).unwrap(), old_machine);
    let repaired = read_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      new_source.as_bytes(),
    )
    .unwrap();
    assert_eq!(repaired.compiler_options.jsx.as_deref(), Some("preserve"));

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_cold_write_hashes_source_once() {
    let root = unique_temp_root("cold-write-source-hash-count");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(r#"{"compilerOptions":{"jsx":"react-jsx"}}"#);
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();

    reset_tsconfig_write_source_hash_count();
    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    assert!(cache_paths.machine.exists());
    assert_eq!(tsconfig_write_source_hash_count(), 1);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_read_hit_skips_dependency_preflight_scan() {
    let root = unique_temp_root("read-hit-skip-preflight");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(r#"{"compilerOptions":{"jsx":"react-jsx"}}"#);
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );

    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    reset_tsconfig_dependency_preflight_scan_count();
    let hit = read_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
    );

    assert!(hit.is_some());
    assert_eq!(tsconfig_dependency_preflight_scan_count(), 0);

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_write_preflight_blocks_external_dependency_source_bytes() {
    let root = unique_temp_root("write-preflight-blocks");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source =
      large_tsconfig_source(r#"{"extends":"./base.json","compilerOptions":{"jsx":"react-jsx"}}"#);
    let parse_source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(parse_source.to_vec()).unwrap())
        .unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();

    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_write_preflight_skips_path_work_for_external_dependencies() {
    let root = unique_temp_root("write-preflight-skip-paths");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source =
      large_tsconfig_source(r#"{"extends":"./base.json","compilerOptions":{"jsx":"react-jsx"}}"#);
    let parse_source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(parse_source.to_vec()).unwrap())
        .unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );

    reset_tsconfig_cache_path_build_count();
    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    assert_eq!(tsconfig_cache_path_build_count(), 0);

    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();
    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_skips_tiny_sources_even_when_enabled() {
    let root = unique_temp_root("tiny-source-cache");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = br#"{"compilerOptions":{"jsx":"react-jsx"}}"#;
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&tsconfig_path, source).unwrap();
    let tsconfig =
      TsConfig::parse(true, &tsconfig_path, String::from_utf8(source.to_vec()).unwrap()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    let cache_paths =
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .unwrap();

    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source,
      &tsconfig,
    );

    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());
    assert!(
      read_dx_tsconfig_machine_cache(&cache_config, &project_root, &tsconfig_path, source)
        .is_none()
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_uses_in_process_hit_before_machine_decode() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("in-process-before-machine");
    let project_root = root.join("project");
    let src_path = project_root.join("src").join("entry.ts");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
      "files": ["src/entry.ts"],
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx"
      }
    }"#,
    );
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "export {};\n").unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();

    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    let _current_dir = CurrentDirGuard::set(&project_root);
    let _cache_env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache = TsconfigCache::new(false);
    cache.cache.insert(tsconfig_path.clone(), Arc::new(tsconfig));

    reset_tsconfig_string_materialization_count();
    let found = cache.find_tsconfig(&src_path).unwrap().unwrap();

    assert_eq!(found.path, tsconfig_path);
    assert_eq!(tsconfig_string_materialization_count(), 0);

    drop(_cache_env);
    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_reuses_nearest_path_for_sibling_sources() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("nearest-path-sibling");
    let project_root = root.join("project");
    let src_dir = project_root.join("src");
    let first_path = src_dir.join("a.ts");
    let second_path = src_dir.join("b.ts");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx"
      }
    }"#,
    );
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(&first_path, "export const a = 1;\n").unwrap();
    std::fs::write(&second_path, "export const b = 1;\n").unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();

    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    let _current_dir = CurrentDirGuard::set(&project_root);
    let _cache_env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache = TsconfigCache::new(false);

    reset_tsconfig_nearest_path_probe_count();
    reset_tsconfig_string_materialization_count();
    let first = cache.find_tsconfig(&first_path).unwrap().unwrap();
    let first_probe_count = tsconfig_nearest_path_probe_count();
    let first_string_materialization_count = tsconfig_string_materialization_count();

    let second = cache.find_tsconfig(&second_path).unwrap().unwrap();

    assert_eq!(first.path, tsconfig_path);
    assert_eq!(second.path, tsconfig_path);
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(cache.size(), 1);
    assert_eq!(tsconfig_nearest_path_probe_count(), first_probe_count + 1);
    assert_eq!(tsconfig_string_materialization_count(), first_string_materialization_count);

    drop(_cache_env);
    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_nearest_path_cache_reuses_cached_ancestor_hit() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("nearest-path-cousin-ancestor");
    let project_root = root.join("project");
    let first_dir = project_root.join("src").join("a");
    let second_dir = project_root.join("src").join("b");
    let first_path = first_dir.join("entry.ts");
    let second_path = second_dir.join("entry.ts");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx"
      }
    }"#,
    );
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    std::fs::write(&first_path, "export const a = 1;\n").unwrap();
    std::fs::write(&second_path, "export const b = 1;\n").unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();

    let _current_dir = CurrentDirGuard::set(&project_root);
    let cache = TsconfigCache::new(false);

    reset_tsconfig_nearest_path_probe_count();
    assert_eq!(cache.nearest_dx_tsconfig_path(&first_path), Some(tsconfig_path.clone()));
    let first_probe_count = tsconfig_nearest_path_probe_count();

    assert_eq!(cache.nearest_dx_tsconfig_path(&second_path), Some(tsconfig_path));
    assert_eq!(tsconfig_nearest_path_probe_count(), first_probe_count + 2);

    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_nearest_path_cache_refreshes_stale_positive_when_closer_tsconfig_appears() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("nearest-path-stale-positive-closer");
    let project_root = root.join("project");
    let src_dir = project_root.join("src");
    let first_dir = src_dir.join("a");
    let second_dir = src_dir.join("b");
    let first_path = first_dir.join("entry.ts");
    let second_path = second_dir.join("entry.ts");
    let root_tsconfig_path = project_root.join("tsconfig.json");
    let closer_tsconfig_path = src_dir.join("tsconfig.json");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    std::fs::write(&first_path, "export const a = 1;\n").unwrap();
    std::fs::write(&second_path, "export const b = 1;\n").unwrap();
    std::fs::write(&root_tsconfig_path, "{}").unwrap();

    let _current_dir = CurrentDirGuard::set(&project_root);
    let cache = TsconfigCache::new(false);

    assert_eq!(cache.nearest_dx_tsconfig_path(&first_path), Some(root_tsconfig_path));
    std::fs::write(&closer_tsconfig_path, "{}").unwrap();

    assert_eq!(cache.nearest_dx_tsconfig_path(&second_path), Some(closer_tsconfig_path));

    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_skips_upstream_ineligible_paths() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("ineligible-paths");
    let project_root = root.join("project");
    let src_path = project_root.join("src").join("entry.ts");
    let node_modules_path =
      project_root.join("node_modules").join("pkg").join("src").join("entry.ts");
    let relative_path = PathBuf::from("src").join("entry.ts");
    let virtual_path = PathBuf::from("\0virtual-module").join("entry.ts");
    let tsconfig_path = project_root.join("tsconfig.json");
    let source = large_tsconfig_source(
      r#"{
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/*"] },
        "jsx": "react-jsx"
      }
    }"#,
    );
    std::fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(node_modules_path.parent().unwrap()).unwrap();
    std::fs::write(&src_path, "export const app = 1;\n").unwrap();
    std::fs::write(&node_modules_path, "export const dep = 1;\n").unwrap();
    std::fs::write(&tsconfig_path, source.as_bytes()).unwrap();

    let tsconfig = TsConfig::parse(true, &tsconfig_path, source.clone()).unwrap();
    let cache_config = rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(
      &project_root,
      Some(std::ffi::OsString::from("1")),
    );
    write_dx_tsconfig_machine_cache(
      &cache_config,
      &project_root,
      &tsconfig_path,
      source.as_bytes(),
      &tsconfig,
    );

    let _current_dir = CurrentDirGuard::set(&project_root);
    let _cache_env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache = TsconfigCache::new(false);

    assert!(cache.resolver().find_tsconfig(&node_modules_path).unwrap().is_none());
    assert!(cache.resolver().find_tsconfig(&relative_path).unwrap().is_none());
    assert!(cache.resolver().find_tsconfig(&virtual_path).unwrap().is_none());

    reset_tsconfig_nearest_path_probe_count();
    reset_tsconfig_string_materialization_count();

    assert!(cache.find_tsconfig(&node_modules_path).unwrap().is_none());
    assert!(cache.find_tsconfig(&relative_path).unwrap().is_none());
    assert!(cache.find_tsconfig(&virtual_path).unwrap().is_none());
    assert_eq!(tsconfig_nearest_path_probe_count(), 0);
    assert_eq!(tsconfig_string_materialization_count(), 0);

    drop(_cache_env);
    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_nearest_path_cache_does_not_store_misses() {
    let _process_state = lock_tsconfig_process_state();
    let root = unique_temp_root("nearest-path-positive-only");
    let project_root = root.join("project");
    let src_dir = project_root.join("src");
    let source_path = src_dir.join("entry.ts");
    let tsconfig_path = project_root.join("tsconfig.json");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(&source_path, "export {};\n").unwrap();

    let _current_dir = CurrentDirGuard::set(&project_root);
    let cache = TsconfigCache::new(false);

    assert!(cache.nearest_dx_tsconfig_path(&source_path).is_none());
    assert!(cache.nearest_tsconfig_path_cache.is_empty());

    std::fs::write(&tsconfig_path, "{}").unwrap();
    assert_eq!(cache.nearest_dx_tsconfig_path(&source_path), Some(tsconfig_path));
    assert_eq!(cache.nearest_tsconfig_path_cache.len(), 2);

    drop(_current_dir);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_disabled_cache_has_no_paths() {
    let root = unique_temp_root("disabled-cache");
    let project_root = root.join("project");
    let tsconfig_path = project_root.join("tsconfig.json");
    let cache_config =
      rolldown_utils::dx_machine_cache::DxMachineCacheConfig::from_env_value(&project_root, None);

    assert!(
      dx_tsconfig_machine_cache_paths_if_enabled(&cache_config, &project_root, &tsconfig_path)
        .is_none()
    );

    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn tsconfig_machine_cache_skips_extends_and_references_sources() {
    reset_tsconfig_dependency_value_parse_count();

    assert!(tsconfig_source_has_external_dependencies(br#"{"extends":"./base.json"}"#));
    assert!(tsconfig_source_has_external_dependencies(
      br#"{"references":[{"path":"./subproject"}]}"#
    ));
    assert!(tsconfig_source_has_external_dependencies(br#"{"extends":null}"#));
    assert!(tsconfig_source_has_external_dependencies(br#"{"references":false}"#));
    assert!(tsconfig_source_has_external_dependencies(br#"{"exte\u006eds":"./base.json"}"#));
    assert!(!tsconfig_source_has_external_dependencies(
      br#"{"compilerOptions":{"jsx":"react-jsx"}}"#
    ));
    assert!(!tsconfig_source_has_external_dependencies(
      br#"{"compilerOptions":{"extends":"not-top-level","references":[]}}"#
    ));
    assert!(!tsconfig_source_has_external_dependencies(br#"[{"extends":"not-top-level"}]"#));
    assert!(!tsconfig_source_has_external_dependencies(br#"null"#));
    assert!(!tsconfig_source_has_external_dependencies(
      "\u{feff}{\"compilerOptions\":{\"jsx\":\"react-jsx\"}}".as_bytes()
    ));
    assert!(tsconfig_source_has_external_dependencies(
      b"{\n// keep cache off for external graph\n\"extends\":\"./base.json\"\n}"
    ));
    assert!(tsconfig_source_has_external_dependencies(b"{"));
    assert_eq!(tsconfig_dependency_value_parse_count(), 0);
  }

  #[test]
  fn tsconfig_dependency_preflight_avoids_full_json_value_tree_for_local_configs() {
    reset_tsconfig_dependency_value_parse_count();

    assert!(!tsconfig_source_has_external_dependencies(
      br#"{
        "compilerOptions": {
          "paths": {
            "@local/extends": ["src/extends.ts"]
          }
        },
        "include": ["src"]
      }"#
    ));
    assert_eq!(tsconfig_dependency_value_parse_count(), 0);
  }

  fn unique_temp_root(label: &str) -> PathBuf {
    let nanos =
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-tsconfig-machine-{label}-{}-{nanos}", std::process::id()))
  }

  fn large_tsconfig_source(source: &str) -> String {
    let trimmed = source.trim_end();
    let Some(prefix) = trimmed.strip_suffix('}') else {
      panic!("large_tsconfig_source requires a top-level object");
    };
    let separator = if prefix.trim_end().ends_with('{') { "" } else { "," };
    format!(
      r#"{prefix}{separator}"dxPadding":"{}"}}"#,
      "x".repeat(DX_TSCONFIG_MACHINE_MIN_SOURCE_BYTES)
    )
  }

  fn lock_tsconfig_process_state() -> MutexGuard<'static, ()> {
    TSCONFIG_PROCESS_STATE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  struct CurrentDirGuard {
    previous: PathBuf,
  }

  impl CurrentDirGuard {
    fn set(path: &Path) -> Self {
      let previous = std::env::current_dir().unwrap();
      std::env::set_current_dir(path).unwrap();
      Self { previous }
    }
  }

  impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
      let _ = std::env::set_current_dir(&self.previous);
    }
  }

  struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
  }

  impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
      let previous = std::env::var_os(key);
      // SAFETY: this test mutates a process environment variable for a short scoped guard and
      // restores it before returning. The focused test commands for this helper run a single
      // test, and the broader tsconfig machine suite does not otherwise depend on this variable.
      unsafe {
        std::env::set_var(key, value);
      }
      Self { key, previous }
    }
  }

  impl Drop for EnvVarGuard {
    fn drop(&mut self) {
      // SAFETY: see EnvVarGuard::set; this restores the previous scoped process environment.
      unsafe {
        match &self.previous {
          Some(previous) => std::env::set_var(self.key, previous),
          None => std::env::remove_var(self.key),
        }
      }
    }
  }

  fn reset_tsconfig_string_materialization_count() {
    DX_TSCONFIG_TEST_STRING_MATERIALIZATIONS.with(|count| count.set(0));
  }

  fn tsconfig_string_materialization_count() -> u64 {
    DX_TSCONFIG_TEST_STRING_MATERIALIZATIONS.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_dynamic_json_object_build_count() {
    DX_TSCONFIG_TEST_DYNAMIC_JSON_OBJECT_BUILDS.with(|count| count.set(0));
  }

  fn tsconfig_dynamic_json_object_build_count() -> u64 {
    DX_TSCONFIG_TEST_DYNAMIC_JSON_OBJECT_BUILDS.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_dependency_value_parse_count() {
    DX_TSCONFIG_TEST_DEPENDENCY_VALUE_PARSES.with(|count| count.set(0));
  }

  fn tsconfig_dependency_value_parse_count() -> u64 {
    DX_TSCONFIG_TEST_DEPENDENCY_VALUE_PARSES.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_dependency_preflight_scan_count() {
    DX_TSCONFIG_TEST_DEPENDENCY_PREFLIGHT_SCANS.with(|count| count.set(0));
  }

  fn tsconfig_dependency_preflight_scan_count() -> u64 {
    DX_TSCONFIG_TEST_DEPENDENCY_PREFLIGHT_SCANS.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_payload_layout_validation_count() {
    DX_TSCONFIG_TEST_PAYLOAD_LAYOUT_VALIDATIONS.with(|count| count.set(0));
  }

  fn tsconfig_payload_layout_validation_count() -> u64 {
    DX_TSCONFIG_TEST_PAYLOAD_LAYOUT_VALIDATIONS.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_nearest_path_probe_count() {
    DX_TSCONFIG_TEST_NEAREST_PATH_PROBES.with(|count| count.set(0));
  }

  fn tsconfig_nearest_path_probe_count() -> u64 {
    DX_TSCONFIG_TEST_NEAREST_PATH_PROBES.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_write_source_hash_count() {
    DX_TSCONFIG_TEST_WRITE_SOURCE_HASHES.with(|count| count.set(0));
  }

  fn tsconfig_write_source_hash_count() -> u64 {
    DX_TSCONFIG_TEST_WRITE_SOURCE_HASHES.with(std::cell::Cell::get)
  }

  fn reset_tsconfig_cache_path_build_count() {
    DX_TSCONFIG_TEST_CACHE_PATH_BUILDS.with(|count| count.set(0));
  }

  fn tsconfig_cache_path_build_count() -> u64 {
    DX_TSCONFIG_TEST_CACHE_PATH_BUILDS.with(std::cell::Cell::get)
  }
}
