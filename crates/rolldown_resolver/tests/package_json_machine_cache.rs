use std::{
  ffi::OsString,
  fs,
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
  time::{SystemTime, UNIX_EPOCH},
};

use rolldown_common::{Platform, ResolveOptions, TsConfig, decode_package_json_machine_payload};
use rolldown_fs::{OsFileSystem, OxcResolverFileSystem as _};
use rolldown_resolver::Resolver;
use rolldown_utils::dx_machine_cache::DxMachineCacheConfig;

static ENV_LOCK: Mutex<()> = Mutex::new(());
const PACKAGE_JSON_TEST_MACHINE_MIN_SOURCE_BYTES: usize = 16 * 1024;

#[test]
fn package_json_default_off_path_does_not_clone_source_bytes_for_machine_cache() {
  let resolver_source = include_str!("../src/resolver.rs");

  assert!(
    !resolver_source.contains("json_bytes.clone()"),
    "default-off package.json parsing should consume source bytes without cloning for machine cache"
  );
}

#[test]
fn package_json_default_off_path_delays_machine_source_path_until_cache_enabled() {
  let resolver_source = include_str!("../src/resolver.rs");
  let function_start = resolver_source
    .find("fn inner_try_get_package_json_or_create")
    .expect("resolver source should include package.json creation helper");
  let function_end = resolver_source[function_start..]
    .find("  /// Resolves a module specifier")
    .map(|offset| function_start + offset)
    .expect("resolver source should include resolve docs after package.json helper");
  let function_source = &resolver_source[function_start..function_end];
  let env_check = function_source
    .find("DxMachineCacheConfig::from_env_if_enabled")
    .expect("package.json cache path should check the cache opt-in env");
  let machine_source_path = function_source
    .find("package_json_machine_source_path")
    .expect("package.json cache path should compute a machine source path");

  assert!(
    env_check < machine_source_path,
    "default-off package.json parsing should not compute the DX machine source path before cache opt-in"
  );
}

#[test]
fn package_json_machine_source_path_stays_borrowed_until_use() {
  let resolver_source = include_str!("../src/resolver.rs");

  assert!(
    !resolver_source
      .contains("package_json_machine_source_path(&self.cwd, &realpath).into_owned()"),
    "resolver package.json machine cache path should carry the borrowed Cow instead of allocating a PathBuf"
  );
}

#[test]
fn package_json_cache_target_lets_paths_for_source_strip_project_root_once() {
  let resolver_source = include_str!("../src/resolver.rs");
  let function_start = resolver_source
    .find("fn inner_try_get_package_json_or_create")
    .expect("resolver source should include package.json creation helper");
  let function_end = resolver_source[function_start..]
    .find("  /// Resolves a module specifier")
    .map(|offset| function_start + offset)
    .expect("resolver source should include resolve docs after package.json helper");
  let function_source = &resolver_source[function_start..function_end];
  let cache_target_start = function_source
    .find("let cache_target =")
    .expect("package.json helper should set up a machine cache target");
  let cache_target_end = function_source[cache_target_start..]
    .find("let pkg_json_value =")
    .map(|offset| cache_target_start + offset)
    .expect("package.json helper should parse after cache target setup");
  let cache_target_source = &function_source[cache_target_start..cache_target_end];
  let compact_cache_target_source =
    cache_target_source.chars().filter(|ch| !ch.is_whitespace()).collect::<String>();

  assert!(
    !cache_target_source.contains("package_json_machine_source_path")
      && compact_cache_target_source
        .contains(".paths_for_source_if_enabled(&self.cwd,\"package_json_metadata\",&realpath)"),
    "cache target setup should pass the real path directly and let paths_for_source strip the project root once"
  );
}

#[test]
fn resolver_cached_package_json_uses_borrowed_hit_lookup() {
  let resolver_source = include_str!("../src/resolver.rs");

  assert!(
    resolver_source.contains("fn cached_package_json"),
    "resolver source should include cached package-json helper"
  );
  assert!(
    !resolver_source.contains(".entry(oxc_pkg_json.realpath.clone())"),
    "cached package-json lookup should borrow the cache key on hit and only own it on insert"
  );
}

#[test]
fn resolver_package_json_hot_hits_try_raw_key_before_dotted_normalization() {
  let resolver_source = include_str!("../src/resolver.rs");
  let inner_start = resolver_source
    .find("fn inner_try_get_package_json_or_create")
    .expect("resolver source should include package-json create helper");
  let inner_end = resolver_source[inner_start..]
    .find("  pub fn resolve(")
    .map(|offset| inner_start + offset)
    .expect("resolver source should include resolve after package-json create helper");
  let inner_source = &resolver_source[inner_start..inner_end];
  let inner_raw_lookup = inner_source
    .find("self.package_json_cache.get(path)")
    .expect("package-json create helper should try the raw path before normalization");
  let inner_normalized_key = inner_source
    .find("let cache_key = package_json_cache_key(path)")
    .expect("package-json create helper should still normalize dotted paths on miss");
  assert!(
    inner_raw_lookup < inner_normalized_key,
    "package-json create helper should avoid dotted-path normalization on exact hot hits"
  );

  let cached_start = resolver_source
    .find("fn cached_package_json")
    .expect("resolver source should include cached package-json helper");
  let cached_end = resolver_source[cached_start..]
    .find("  /// Attempts to resolve using Rollup compatibility mode.")
    .map(|offset| cached_start + offset)
    .expect("resolver source should include rollup compatibility docs after cached helper");
  let cached_source = &resolver_source[cached_start..cached_end];
  let cached_raw_lookup = cached_source
    .find("self.package_json_cache.get(oxc_pkg_json.realpath.as_path())")
    .expect("cached package-json helper should try the raw realpath before normalization");
  let cached_normalized_key = cached_source
    .find("let cache_key = package_json_cache_key(oxc_pkg_json.realpath.as_path())")
    .expect("cached package-json helper should still normalize dotted paths on miss");
  assert!(
    cached_raw_lookup < cached_normalized_key,
    "cached package-json helper should avoid dotted-path normalization on exact hot hits"
  );
}

#[test]
fn package_json_machine_repair_reuses_validated_source_hash() {
  let resolver_source = include_str!("../src/resolver.rs");

  assert!(
    resolver_source.contains("read_validated_machine_with_source_hash"),
    "package.json repair should reuse the source hash that metadata validation already computed"
  );
  assert!(
    resolver_source.contains("validated_source_hash"),
    "package.json repair should carry the validated source hash into the cache-miss rewrite"
  );
  assert!(
    resolver_source.contains("validated_source_hash = Some(hit.source_hash)"),
    "package.json repair should save hit.source_hash only after a validated machine hit fails to decode"
  );
  assert!(
    !resolver_source.contains("read_validated_machine(&cache_paths, &realpath, &json_bytes)"),
    "package.json repair should keep the validated source hash instead of discarding it"
  );
}

#[test]
fn vite_optional_peer_machine_repair_reuses_validated_source_hash() {
  let package_json_cache_source =
    include_str!("../../rolldown_plugin_vite_resolve/src/package_json_cache.rs");

  assert!(
    package_json_cache_source.contains("read_validated_machine_with_source_hash"),
    "optional peer dependency repair should keep the source hash from metadata validation"
  );
  assert!(
    package_json_cache_source.contains("write_machine_artifact_with_source_hash"),
    "optional peer dependency repair should write with the validated source hash after malformed payload repair"
  );
  assert!(
    package_json_cache_source.contains("validated_source_hash"),
    "optional peer dependency repair should thread the validated source hash to the rewrite"
  );
  assert!(
    package_json_cache_source.contains("validated_source_hash = Some(hit.source_hash)"),
    "optional peer dependency repair should save hit.source_hash only after a validated machine hit fails to decode"
  );
  assert!(
    !package_json_cache_source
      .contains("read_validated_machine(cache_paths, realpath, &source_bytes)"),
    "optional peer dependency repair should keep the validated source hash instead of discarding it"
  );
}

#[test]
fn vite_optional_peer_hot_hit_uses_borrowed_project_and_source_keys() {
  let package_json_cache_source =
    include_str!("../../rolldown_plugin_vite_resolve/src/package_json_cache.rs");
  let function_start = package_json_cache_source
    .find("  pub fn cached_package_json_optional_peer_dep")
    .expect("vite package cache should include optional-peer lookup helper");
  let function_end = package_json_cache_source[function_start..]
    .find("  pub fn clear")
    .map(|offset| function_start + offset)
    .expect("optional-peer lookup helper should appear before clear");
  let function_source = &package_json_cache_source[function_start..function_end];
  let compact_function_source =
    function_source.chars().filter(|ch| !ch.is_whitespace()).collect::<String>();

  assert!(
    package_json_cache_source.contains(
      "optional_peer_dep_cache:\n    FxDashMap<PathBuf, FxDashMap<PathBuf, Arc<PackageJsonWithOptionalPeerDependencies>>>"
    ),
    "optional peer cache should split project/source keys so hot hits can borrow both Path lookups"
  );
  assert!(
    compact_function_source
      .contains("self.optional_peer_dep_cache.get(project_cache_key.as_ref())")
      && compact_function_source.contains("project_cache.get(source_cache_key.as_ref())"),
    "optional peer hot hit should lookup borrowed project and machine-source keys before allocating owned keys"
  );
  assert!(
    !function_source.contains("optional_peer_cache_key("),
    "optional peer hot hit should not allocate the owned tuple key before cache lookup"
  );
  assert!(
    compact_function_source.contains("entry(project_cache_key.into_owned()).or_default()")
      && compact_function_source.contains("source_cache_key.into_owned()"),
    "optional peer miss should allocate owned project/source keys only for insertion"
  );
}

#[test]
fn package_json_default_off_path_parses_without_machine_artifacts() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::disabled();

  let root = unique_temp_root("package-json-default-off");
  let package_path = root.join("package.json");
  fs::create_dir_all(&root).unwrap();
  fs::write(
    &package_path,
    r#"{"name":"fixture","version":"1.2.3","type":"module","sideEffects":false}"#,
  )
  .unwrap();

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let package_json = resolver.try_get_package_json_or_create(&package_path).unwrap();

  assert_eq!(package_json.name(), Some("fixture"));
  assert_eq!(package_json.version(), Some("1.2.3"));
  assert_eq!(package_json.r#type(), Some("module"));
  assert_eq!(package_json.check_side_effects_for("src/index.js"), Some(false));
  assert!(!root.join(".dx").join("rolldown").exists());

  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_machine_cache_skips_tiny_sources_even_when_enabled() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::enabled();

  let root = unique_temp_root("package-json-tiny-cache-skip");
  let package_path = root.join("package.json");
  fs::create_dir_all(&root).unwrap();
  fs::write(
    &package_path,
    r#"{"name":"fixture","version":"1.2.3","type":"module","sideEffects":false}"#,
  )
  .unwrap();

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let package_json = resolver.try_get_package_json_or_create(&package_path).unwrap();

  assert_eq!(package_json.name(), Some("fixture"));
  assert_eq!(package_json.version(), Some("1.2.3"));
  assert!(!root.join(".dx").join("rolldown").exists());

  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_machine_cache_skips_sources_at_exact_payoff_boundary() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::enabled();

  let root = unique_temp_root("package-json-boundary-cache-skip");
  let package_path = root.join("package.json");
  fs::create_dir_all(&root).unwrap();
  fs::write(
    &package_path,
    package_json_source_with_exact_len(PACKAGE_JSON_TEST_MACHINE_MIN_SOURCE_BYTES),
  )
  .unwrap();

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let package_json = resolver.try_get_package_json_or_create(&package_path).unwrap();

  assert_eq!(package_json.name(), Some("fixture"));
  assert!(!root.join(".dx").join("rolldown").exists());

  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_machine_cache_writes_sources_above_payoff_boundary() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::enabled();

  let root = unique_temp_root("package-json-boundary-cache-write");
  let package_path = root.join("package.json");
  fs::create_dir_all(&root).unwrap();
  fs::write(
    &package_path,
    package_json_source_with_exact_len(PACKAGE_JSON_TEST_MACHINE_MIN_SOURCE_BYTES + 1),
  )
  .unwrap();

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let package_json = resolver.try_get_package_json_or_create(&package_path).unwrap();

  assert_eq!(package_json.name(), Some("fixture"));
  let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
  let cache_paths =
    cache_config.paths_for_source(&root, "package_json_metadata", Path::new("package.json"));
  assert!(cache_paths.machine.exists());
  assert!(cache_paths.metadata.exists());

  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_in_process_cache_reuses_leading_dot_relative_path() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::disabled();

  let root = unique_temp_root("package-json-dot-path-cache");
  fs::create_dir_all(&root).unwrap();
  fs::write(root.join("package.json"), r#"{"name":"fixture"}"#).unwrap();

  let previous_dir = std::env::current_dir().unwrap();
  std::env::set_current_dir(&root).unwrap();
  let _current_dir = CurrentDirGuard(previous_dir);

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );

  let package_json = resolver.try_get_package_json_or_create(Path::new("package.json")).unwrap();
  let dotted_package_json =
    resolver.try_get_package_json_or_create(&PathBuf::from(".").join("package.json")).unwrap();

  assert!(Arc::ptr_eq(&package_json, &dotted_package_json));

  drop(_current_dir);
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_in_process_cache_keeps_symlink_sensitive_path_spellings_distinct() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::disabled();

  let root = unique_temp_root("package-json-sensitive-path-cache");
  fs::create_dir_all(root.join("dir")).unwrap();
  fs::write(root.join("package.json"), r#"{"name":"fixture"}"#).unwrap();

  let previous_dir = std::env::current_dir().unwrap();
  std::env::set_current_dir(&root).unwrap();
  let current_dir = CurrentDirGuard(previous_dir);

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );

  let relative_package_json =
    resolver.try_get_package_json_or_create(Path::new("package.json")).unwrap();
  let parent_dir_package_json = resolver
    .try_get_package_json_or_create(&PathBuf::from("dir").join("..").join("package.json"))
    .unwrap();
  let absolute_package_json =
    resolver.try_get_package_json_or_create(&root.join("package.json")).unwrap();

  assert!(!Arc::ptr_eq(&relative_package_json, &parent_dir_package_json));
  assert!(!Arc::ptr_eq(&relative_package_json, &absolute_package_json));

  drop(current_dir);
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_machine_cache_falls_back_and_rewrites_malformed_payload() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::enabled();

  let root = unique_temp_root("package-json-malformed-machine");
  let package_path = root.join("package.json");
  let package_source = large_package_json_source();
  let malformed_machine = b"RDXPKG01malformed";
  fs::create_dir_all(&root).unwrap();
  fs::write(&package_path, &package_source).unwrap();

  let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
  let cache_paths = cache_config.paths_for_source(&root, "package_json_metadata", &package_path);
  cache_config
    .write_machine_artifact(
      &cache_paths,
      &package_path,
      package_source.as_bytes(),
      malformed_machine,
    )
    .unwrap();

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let package_json = resolver.try_get_package_json_or_create(&package_path).unwrap();

  assert_eq!(package_json.name(), Some("fixture"));
  let rewritten_machine = fs::read(cache_paths.machine).unwrap();
  assert_ne!(rewritten_machine, malformed_machine);
  assert!(decode_package_json_machine_payload(&rewritten_machine, package_path.clone()).is_some());

  let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_json_machine_cache_reuses_metadata_across_relative_and_absolute_paths() {
  let _guard = ENV_LOCK.lock().unwrap();
  let _env = DxCacheEnv::enabled();

  let root = unique_temp_root("package-json-relative-absolute-metadata");
  fs::create_dir_all(&root).unwrap();
  fs::write(root.join("package.json"), large_package_json_source()).unwrap();

  let previous_dir = std::env::current_dir().unwrap();
  std::env::set_current_dir(&root).unwrap();
  let current_dir = CurrentDirGuard(previous_dir);

  let resolver = Resolver::new(
    OsFileSystem::new(false),
    root.clone(),
    Platform::Node,
    &TsConfig::default(),
    ResolveOptions::default(),
  );
  let relative_package_json =
    resolver.try_get_package_json_or_create(Path::new("package.json")).unwrap();
  assert_eq!(relative_package_json.name(), Some("fixture"));

  let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
  let cache_paths =
    cache_config.paths_for_source(&root, "package_json_metadata", Path::new("package.json"));
  let absolute_cache_paths =
    cache_config.paths_for_source(&root, "package_json_metadata", &root.join("package.json"));
  assert_eq!(absolute_cache_paths.machine, cache_paths.machine);
  assert_eq!(absolute_cache_paths.metadata, cache_paths.metadata);
  let machine_before = fs::read(&cache_paths.machine).unwrap();
  let metadata_before = fs::read(&cache_paths.metadata).unwrap();

  let absolute_package_json =
    resolver.try_get_package_json_or_create(&root.join("package.json")).unwrap();
  assert_eq!(absolute_package_json.name(), Some("fixture"));
  assert!(!Arc::ptr_eq(&relative_package_json, &absolute_package_json));
  assert_eq!(fs::read(&cache_paths.machine).unwrap(), machine_before);
  assert_eq!(fs::read(&cache_paths.metadata).unwrap(), metadata_before);

  drop(current_dir);
  let _ = fs::remove_dir_all(&root);
}

struct CurrentDirGuard(PathBuf);

impl Drop for CurrentDirGuard {
  fn drop(&mut self) {
    std::env::set_current_dir(&self.0).unwrap();
  }
}

fn unique_temp_root(label: &str) -> PathBuf {
  let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
  std::env::temp_dir().join(format!("rolldown-resolver-{label}-{}-{nanos}", std::process::id()))
}

fn large_package_json_source() -> String {
  format!(
    r#"{{"name":"fixture","version":"1.2.3","type":"module","sideEffects":false,"description":"{}"}}"#,
    "x".repeat(16 * 1024)
  )
}

fn package_json_source_with_exact_len(len: usize) -> String {
  let prefix =
    r#"{"name":"fixture","version":"1.2.3","type":"module","sideEffects":false,"description":""#;
  let suffix = r#""}"#;
  assert!(len >= prefix.len() + suffix.len());
  let source = format!("{}{}{}", prefix, "x".repeat(len - prefix.len() - suffix.len()), suffix);
  assert_eq!(source.len(), len);
  source
}

struct DxCacheEnv {
  previous: Option<OsString>,
}

impl DxCacheEnv {
  fn disabled() -> Self {
    let previous = std::env::var_os("ROLLDOWN_DX_JSON_CACHE");
    // SAFETY: these tests serialize ROLLDOWN_DX_JSON_CACHE mutations through ENV_LOCK.
    unsafe {
      std::env::remove_var("ROLLDOWN_DX_JSON_CACHE");
    }
    Self { previous }
  }

  fn enabled() -> Self {
    let previous = std::env::var_os("ROLLDOWN_DX_JSON_CACHE");
    // SAFETY: these tests serialize ROLLDOWN_DX_JSON_CACHE mutations through ENV_LOCK.
    unsafe {
      std::env::set_var("ROLLDOWN_DX_JSON_CACHE", "1");
    }
    Self { previous }
  }
}

impl Drop for DxCacheEnv {
  fn drop(&mut self) {
    // SAFETY: these tests serialize ROLLDOWN_DX_JSON_CACHE mutations through ENV_LOCK.
    unsafe {
      match self.previous.take() {
        Some(previous) => std::env::set_var("ROLLDOWN_DX_JSON_CACHE", previous),
        None => std::env::remove_var("ROLLDOWN_DX_JSON_CACHE"),
      }
    }
  }
}
