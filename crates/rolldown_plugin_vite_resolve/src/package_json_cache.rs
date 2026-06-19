use std::{
  borrow::Cow,
  fs,
  path::{Path, PathBuf},
  sync::Arc,
};

use rolldown_common::{
  PackageJson, PackageJsonOptionalPeerDependencies,
  decode_package_json_optional_peer_dependencies_machine_payload,
  encode_package_json_optional_peer_dependencies_machine_payload,
  parse_package_json_optional_peer_dependencies,
};
use rolldown_utils::{
  dashmap::FxDashMap,
  dx_machine_cache::{DxMachineCacheConfig, DxMachineCacheStatus},
};

const PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES: usize = 16 * 1024;

#[cfg(test)]
thread_local! {
  static PACKAGE_JSON_MACHINE_SOURCE_PATH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_OPTIONAL_PEER_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_CACHE_KEY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_SIDE_EFFECTS_CACHE_LOOKUPS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

#[derive(Debug, Default)]
pub struct PackageJsonCache {
  side_effects_cache: FxDashMap<PathBuf, Arc<PackageJson>>,
  optional_peer_dep_cache:
    FxDashMap<PathBuf, FxDashMap<PathBuf, Arc<PackageJsonWithOptionalPeerDependencies>>>,
}

impl PackageJsonCache {
  pub fn cached_package_json_side_effects(
    &self,
    oxc_pkg_json: &oxc_resolver::PackageJson,
  ) -> Arc<PackageJson> {
    record_side_effects_cache_lookup();
    if let Some(v) = self.side_effects_cache.get(oxc_pkg_json.realpath.as_path()) {
      return Arc::clone(v.value());
    }

    let cache_key = package_json_cache_key(&oxc_pkg_json.realpath);
    if let Cow::Owned(cache_key) = cache_key {
      record_side_effects_cache_lookup();
      if let Some(v) = self.side_effects_cache.get(cache_key.as_path()) {
        return Arc::clone(v.value());
      }

      let pkg_json = Arc::new(PackageJson::from_oxc_pkg_json(oxc_pkg_json));
      self.side_effects_cache.insert(cache_key, Arc::clone(&pkg_json));
      return pkg_json;
    }

    let pkg_json = Arc::new(PackageJson::from_oxc_pkg_json(oxc_pkg_json));
    self.side_effects_cache.insert(oxc_pkg_json.realpath.clone(), Arc::clone(&pkg_json));
    pkg_json
  }

  pub fn cached_package_json_optional_peer_dep(
    &self,
    project_root: &Path,
    oxc_pkg_json: &oxc_resolver::PackageJson,
  ) -> Arc<PackageJsonWithOptionalPeerDependencies> {
    if let Some(project_cache) = self.optional_peer_dep_cache.get(project_root)
      && let Some(v) = project_cache.get(oxc_pkg_json.realpath.as_path())
    {
      return Arc::clone(v.value());
    }

    let project_cache_key = package_json_cache_key(project_root);
    let direct_source_cache_key = package_json_cache_key(&oxc_pkg_json.realpath);
    if let Some(project_cache) = self.optional_peer_dep_cache.get(project_cache_key.as_ref())
      && let Some(v) = project_cache.get(direct_source_cache_key.as_ref())
    {
      return Arc::clone(v.value());
    }

    let machine_source_path =
      package_json_machine_source_path(project_root, &oxc_pkg_json.realpath);
    let source_cache_key = package_json_cache_key(machine_source_path.as_ref());
    if let Some(project_cache) = self.optional_peer_dep_cache.get(project_cache_key.as_ref())
      && let Some(v) = project_cache.get(source_cache_key.as_ref())
    {
      let pkg_json = Arc::clone(v.value());
      drop(v);
      insert_optional_peer_dep_cache_aliases(
        &project_cache,
        project_root,
        &oxc_pkg_json.realpath,
        direct_source_cache_key.as_ref(),
        source_cache_key.as_ref(),
        &pkg_json,
      );
      return pkg_json;
    }

    let package_json_with_optional_peer_deps =
      package_json_optional_peer_deps_with_dx_cache_for_machine_source(
        &oxc_pkg_json.realpath,
        project_root,
        machine_source_path.as_ref(),
      );
    let pkg_json = Arc::new(package_json_with_optional_peer_deps);
    let project_cache =
      self.optional_peer_dep_cache.entry(project_cache_key.into_owned()).or_default();
    if let Some(v) = project_cache.get(source_cache_key.as_ref()) {
      let pkg_json = Arc::clone(v.value());
      drop(v);
      insert_optional_peer_dep_cache_aliases(
        &project_cache,
        project_root,
        &oxc_pkg_json.realpath,
        direct_source_cache_key.as_ref(),
        source_cache_key.as_ref(),
        &pkg_json,
      );
      return pkg_json;
    }
    insert_optional_peer_dep_cache_aliases(
      &project_cache,
      project_root,
      &oxc_pkg_json.realpath,
      direct_source_cache_key.as_ref(),
      source_cache_key.as_ref(),
      &pkg_json,
    );
    project_cache.insert(source_cache_key.into_owned(), Arc::clone(&pkg_json));
    pkg_json
  }

  pub fn clear(&self) {
    self.side_effects_cache.clear();
    self.optional_peer_dep_cache.clear();
  }
}

fn record_side_effects_cache_lookup() {
  #[cfg(test)]
  PACKAGE_JSON_SIDE_EFFECTS_CACHE_LOOKUPS.with(|calls| calls.set(calls.get() + 1));
}

pub type PackageJsonWithOptionalPeerDependencies = PackageJsonOptionalPeerDependencies;

#[cfg(test)]
fn package_json_optional_peer_deps_with_dx_cache(
  realpath: &Path,
  project_root: &Path,
) -> PackageJsonWithOptionalPeerDependencies {
  let machine_source_path = package_json_machine_source_path(project_root, realpath);
  package_json_optional_peer_deps_with_dx_cache_for_machine_source(
    realpath,
    project_root,
    machine_source_path.as_ref(),
  )
}

fn package_json_optional_peer_deps_with_dx_cache_for_machine_source(
  realpath: &Path,
  project_root: &Path,
  machine_source_path: &Path,
) -> PackageJsonWithOptionalPeerDependencies {
  let Ok(source_bytes) = fs::read(realpath) else {
    return Default::default();
  };

  let cache_target = package_json_source_len_worth_machine_cache(source_bytes.len())
    .then(|| DxMachineCacheConfig::from_env_if_enabled(project_root))
    .flatten()
    .and_then(|cache_config| {
      cache_config
        .paths_for_source_if_enabled(
          project_root,
          "package_json_optional_peer_deps",
          machine_source_path,
        )
        .map(|cache_paths| (cache_config, cache_paths))
    });

  let mut validated_source_hash = None;
  if let Some((cache_config, cache_paths)) = &cache_target
    && let DxMachineCacheStatus::Hit(hit) = cache_config.read_validated_machine_with_source_hash(
      cache_paths,
      machine_source_path,
      &source_bytes,
    )
  {
    if let Some(package_json) =
      decode_package_json_optional_peer_dependencies_machine_payload(&hit.machine_bytes)
    {
      return package_json;
    }
    validated_source_hash = Some(hit.source_hash);
  }

  let parsed = parse_package_json_optional_peer_deps(&source_bytes);
  let should_write_machine_artifact =
    validated_source_hash.is_some() || !parsed.optional_peer_dependencies.is_empty();
  if should_write_machine_artifact
    && let Some((cache_config, cache_paths)) = &cache_target
    && let Some(machine_bytes) =
      encode_package_json_optional_peer_dependencies_machine_payload(&parsed)
  {
    if let Some(source_hash) = validated_source_hash {
      let _ = cache_config.write_machine_artifact_with_source_hash(
        cache_paths,
        machine_source_path,
        source_bytes.len(),
        &source_hash,
        &machine_bytes,
      );
    } else {
      let _ = cache_config.write_machine_artifact(
        cache_paths,
        machine_source_path,
        &source_bytes,
        &machine_bytes,
      );
    }
  }

  parsed
}

fn parse_package_json_optional_peer_deps(
  source_bytes: &[u8],
) -> PackageJsonWithOptionalPeerDependencies {
  #[cfg(test)]
  PACKAGE_JSON_OPTIONAL_PEER_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));

  parse_package_json_optional_peer_dependencies(source_bytes)
}

fn package_json_source_len_worth_machine_cache(source_len: usize) -> bool {
  source_len > PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES
}

fn package_json_cache_key(path: &Path) -> Cow<'_, Path> {
  #[cfg(test)]
  PACKAGE_JSON_CACHE_KEY_CALLS.with(|calls| calls.set(calls.get() + 1));

  let components = path.components();
  if !components.clone().any(|component| matches!(component, std::path::Component::CurDir)) {
    return Cow::Borrowed(path);
  }

  let mut cache_key = PathBuf::new();
  for component in components {
    if !matches!(component, std::path::Component::CurDir) {
      cache_key.push(component.as_os_str());
    }
  }
  Cow::Owned(cache_key)
}

fn package_json_machine_source_path<'a>(project_root: &Path, realpath: &'a Path) -> Cow<'a, Path> {
  #[cfg(test)]
  PACKAGE_JSON_MACHINE_SOURCE_PATH_CALLS.with(|calls| calls.set(calls.get() + 1));

  realpath.strip_prefix(project_root).map_or_else(|_| Cow::Borrowed(realpath), Cow::Borrowed)
}

fn insert_optional_peer_dep_cache_aliases(
  project_cache: &FxDashMap<PathBuf, Arc<PackageJsonWithOptionalPeerDependencies>>,
  project_root: &Path,
  realpath: &Path,
  direct_source_cache_key: &Path,
  source_cache_key: &Path,
  pkg_json: &Arc<PackageJsonWithOptionalPeerDependencies>,
) {
  if direct_source_cache_key != source_cache_key {
    project_cache.insert(direct_source_cache_key.to_path_buf(), Arc::clone(pkg_json));
  }
  if let Some(project_absolute_cache_key) =
    package_json_project_absolute_alias_key(project_root, realpath)
    && project_absolute_cache_key.as_path() != source_cache_key
    && project_absolute_cache_key.as_path() != direct_source_cache_key
  {
    project_cache.insert(project_absolute_cache_key, Arc::clone(pkg_json));
  }
}

fn package_json_project_absolute_alias_key(
  project_root: &Path,
  realpath: &Path,
) -> Option<PathBuf> {
  if realpath.is_absolute()
    || realpath.components().any(|component| matches!(component, std::path::Component::ParentDir))
  {
    return None;
  }

  Some(package_json_cache_key(&project_root.join(realpath)).into_owned())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
  };

  static PACKAGE_JSON_CACHE_PROCESS_STATE_LOCK: Mutex<()> = Mutex::new(());

  #[test]
  fn optional_peer_machine_cache_uses_project_root_when_cwd_differs() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-project-root");
    let launcher_root = root.join("launcher");
    let project_root = root.join("project");
    let package_json_path = project_root.join("node_modules").join("pkg").join("package.json");
    fs::create_dir_all(&launcher_root).unwrap();
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, large_optional_peer_package_json_source("pkg")).unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let _cwd = CurrentDirGuard::set(&launcher_root);

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &project_root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));
    assert!(!parsed.optional_peer_dependencies.contains("vue"));

    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(OsString::from("1")));
    let cache_paths = cache_config.paths_for_source(
      &project_root,
      "package_json_optional_peer_deps",
      &package_json_path,
    );
    assert!(cache_paths.machine.exists());
    assert!(cache_paths.metadata.exists());
    assert!(!launcher_root.join(".dx").join("rolldown").exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_skips_tiny_sources_even_when_enabled() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-tiny-cache-skip");
    let package_json_path = root.join("package.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(
      &package_json_path,
      br#"{
        "name": "pkg",
        "peerDependencies": {
          "react": "^18.0.0",
          "vue": "^3.0.0"
        },
        "peerDependenciesMeta": {
          "react": { "optional": true }
        }
      }"#,
    )
    .unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));
    assert!(!parsed.optional_peer_dependencies.contains("vue"));
    assert!(!root.join(".dx").join("rolldown").exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_skips_sources_at_exact_payoff_boundary() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-boundary-cache-skip");
    let package_json_path = root.join("package.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(
      &package_json_path,
      optional_peer_package_json_source_with_exact_len(
        "pkg",
        PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES,
      ),
    )
    .unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));
    assert!(!root.join(".dx").join("rolldown").exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_writes_sources_above_payoff_boundary() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-boundary-cache-write");
    let package_json_path = root.join("package.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(
      &package_json_path,
      optional_peer_package_json_source_with_exact_len(
        "pkg",
        PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES + 1,
      ),
    )
    .unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));
    let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
    let cache_paths = cache_config.paths_for_source(
      &root,
      "package_json_optional_peer_deps",
      Path::new("package.json"),
    );
    assert!(cache_paths.machine.exists());
    assert!(cache_paths.metadata.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_hit_skips_package_json_parse() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-hit-skips-parse");
    let package_json_path = root.join("package.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(&package_json_path, large_optional_peer_package_json_source("pkg")).unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");

    let cold = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);
    assert_eq!(cold.name, "pkg");
    assert!(cold.optional_peer_dependencies.contains("react"));

    reset_optional_peer_parse_count();
    let warm = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(warm.name, "pkg");
    assert!(warm.optional_peer_dependencies.contains("react"));
    assert_eq!(optional_peer_parse_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_skips_empty_dependency_sets_on_cold_miss() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-empty-cache-skip");
    let package_json_path = root.join("package.json");
    fs::create_dir_all(&root).unwrap();
    fs::write(
      &package_json_path,
      package_json_source_without_optional_peers_with_exact_len(
        "pkg",
        PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES + 1,
      ),
    )
    .unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.is_empty());
    let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
    let cache_paths = cache_config.paths_for_source(
      &root,
      "package_json_optional_peer_deps",
      Path::new("package.json"),
    );
    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_repairs_empty_dependency_artifact_after_validated_decode_miss() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-empty-cache-repair");
    let package_json_path = root.join("package.json");
    let source = package_json_source_without_optional_peers_with_exact_len(
      "pkg",
      PACKAGE_JSON_MACHINE_MIN_SOURCE_BYTES + 1,
    );
    fs::create_dir_all(&root).unwrap();
    fs::write(&package_json_path, &source).unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
    let cache_paths = cache_config.paths_for_source(
      &root,
      "package_json_optional_peer_deps",
      Path::new("package.json"),
    );
    let stale_machine = b"validated-but-undecodable";
    cache_config
      .write_machine_artifact(&cache_paths, Path::new("package.json"), &source, stale_machine)
      .unwrap();

    let parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.is_empty());
    let repaired_machine = fs::read(&cache_paths.machine).unwrap();
    assert_ne!(repaired_machine, stale_machine);
    let repaired =
      decode_package_json_optional_peer_dependencies_machine_payload(&repaired_machine).unwrap();
    assert_eq!(repaired.name, "pkg");
    assert!(repaired.optional_peer_dependencies.is_empty());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_machine_cache_reuses_metadata_across_relative_and_absolute_paths() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-relative-absolute-metadata");
    let package_json_path = root.join("package.json");
    let source = large_optional_peer_package_json_source("pkg");
    fs::create_dir_all(&root).unwrap();
    fs::write(&package_json_path, &source).unwrap();

    let _env = EnvVarGuard::set("ROLLDOWN_DX_JSON_CACHE", "1");
    let _cwd = CurrentDirGuard::set(&root);

    let relative_path = Path::new("package.json");
    let parsed = package_json_optional_peer_deps_with_dx_cache(relative_path, &root);
    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));

    let cache_config = DxMachineCacheConfig::from_env_value(&root, Some(OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&root, "package_json_optional_peer_deps", relative_path);
    let absolute_cache_paths =
      cache_config.paths_for_source(&root, "package_json_optional_peer_deps", &package_json_path);
    assert_eq!(absolute_cache_paths.machine, cache_paths.machine);
    assert_eq!(absolute_cache_paths.metadata, cache_paths.metadata);
    let machine_before = fs::read(&cache_paths.machine).unwrap();
    let metadata_before = fs::read(&cache_paths.metadata).unwrap();

    let absolute_parsed = package_json_optional_peer_deps_with_dx_cache(&package_json_path, &root);
    assert_eq!(absolute_parsed.name, "pkg");
    assert!(absolute_parsed.optional_peer_dependencies.contains("react"));
    assert_eq!(fs::read(&cache_paths.machine).unwrap(), machine_before);
    assert_eq!(fs::read(&cache_paths.metadata).unwrap(), metadata_before);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn package_json_in_process_caches_reuse_leading_dot_realpath_keys() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("leading-dot-cache");
    fs::create_dir_all(root.join("dir")).unwrap();
    let package_json_path = PathBuf::from("package.json");
    let dotted_package_json_path = PathBuf::from(".").join("package.json");
    let parent_dir_package_json_path = PathBuf::from("dir").join("..").join("package.json");
    let source = br#"{
      "name": "pkg",
      "sideEffects": false,
      "peerDependencies": {
        "react": "^18.0.0",
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::write(root.join("package.json"), source).unwrap();
    let _cwd = CurrentDirGuard::set(&root);
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);
    let dotted_package_json = oxc_package_json(&dotted_package_json_path, source);
    let parent_dir_package_json = oxc_package_json(&parent_dir_package_json_path, source);

    let side_effects = package_cache.cached_package_json_side_effects(&package_json);
    let dotted_side_effects = package_cache.cached_package_json_side_effects(&dotted_package_json);
    let parent_dir_side_effects =
      package_cache.cached_package_json_side_effects(&parent_dir_package_json);

    assert!(Arc::ptr_eq(&side_effects, &dotted_side_effects));
    assert!(!Arc::ptr_eq(&side_effects, &parent_dir_side_effects));

    let optional_peers = package_cache.cached_package_json_optional_peer_dep(&root, &package_json);
    let dotted_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &dotted_package_json);
    let parent_dir_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &parent_dir_package_json);

    assert!(Arc::ptr_eq(&optional_peers, &dotted_optional_peers));
    assert!(!Arc::ptr_eq(&optional_peers, &parent_dir_optional_peers));
    assert!(optional_peers.optional_peer_dependencies.contains("react"));
    assert!(!optional_peers.optional_peer_dependencies.contains("vue"));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_cache_reuses_project_relative_identity() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-relative-absolute-cache-key");
    fs::create_dir_all(&root).unwrap();
    let relative_package_json_path = PathBuf::from("package.json");
    let absolute_package_json_path = root.join("package.json");
    let source = br#"{
      "name": "pkg",
      "sideEffects": false,
      "peerDependencies": {
        "react": "^18.0.0",
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::write(&absolute_package_json_path, source).unwrap();
    let _cwd = CurrentDirGuard::set(&root);
    let package_cache = PackageJsonCache::default();
    let relative_package_json = oxc_package_json(&relative_package_json_path, source);
    let absolute_package_json = oxc_package_json(&absolute_package_json_path, source);

    let relative_side_effects =
      package_cache.cached_package_json_side_effects(&relative_package_json);
    let absolute_side_effects =
      package_cache.cached_package_json_side_effects(&absolute_package_json);
    assert!(!Arc::ptr_eq(&relative_side_effects, &absolute_side_effects));

    let relative_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &relative_package_json);
    let absolute_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &absolute_package_json);

    assert!(Arc::ptr_eq(&relative_optional_peers, &absolute_optional_peers));
    assert!(relative_optional_peers.optional_peer_dependencies.contains("react"));
    assert!(!relative_optional_peers.optional_peer_dependencies.contains("vue"));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_relative_alias_hit_skips_package_json_parse() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-relative-absolute-parse-skip");
    fs::create_dir_all(&root).unwrap();
    let relative_package_json_path = PathBuf::from("package.json");
    let absolute_package_json_path = root.join("package.json");
    let source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0",
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::write(&absolute_package_json_path, source).unwrap();
    let _cwd = CurrentDirGuard::set(&root);
    let package_cache = PackageJsonCache::default();
    let relative_package_json = oxc_package_json(&relative_package_json_path, source);
    let absolute_package_json = oxc_package_json(&absolute_package_json_path, source);

    let relative_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &relative_package_json);
    assert!(relative_optional_peers.optional_peer_dependencies.contains("react"));

    reset_optional_peer_parse_count();
    let absolute_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &absolute_package_json);

    assert!(Arc::ptr_eq(&relative_optional_peers, &absolute_optional_peers));
    assert_eq!(optional_peer_parse_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_alias_hit_promotes_direct_hot_hit() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-relative-absolute-direct-hit");
    fs::create_dir_all(&root).unwrap();
    let relative_package_json_path = PathBuf::from("package.json");
    let absolute_package_json_path = root.join("package.json");
    let source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0",
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::write(&absolute_package_json_path, source).unwrap();
    let _cwd = CurrentDirGuard::set(&root);
    let package_cache = PackageJsonCache::default();
    let relative_package_json = oxc_package_json(&relative_package_json_path, source);
    let absolute_package_json = oxc_package_json(&absolute_package_json_path, source);

    let relative_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &relative_package_json);
    assert!(relative_optional_peers.optional_peer_dependencies.contains("react"));

    reset_optional_peer_parse_count();
    reset_machine_source_path_count();
    reset_cache_key_count();
    let absolute_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &absolute_package_json);

    assert!(Arc::ptr_eq(&relative_optional_peers, &absolute_optional_peers));
    assert_eq!(optional_peer_parse_count(), 0);
    assert_eq!(machine_source_path_count(), 0);
    assert_eq!(cache_key_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_cache_keeps_project_roots_distinct() {
    let _process_state = lock_package_json_cache_process_state();
    let temp_root = unique_temp_root("optional-peer-project-root-key-isolation");
    let project_a = temp_root.join("project-a");
    let project_b = temp_root.join("project-b");
    let package_json_a_path = project_a.join("pkg").join("package.json");
    let package_json_b_path = project_b.join("pkg").join("package.json");
    let source_a = br#"{
      "name": "pkg-a",
      "peerDependencies": {
        "react": "^18.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    let source_b = br#"{
      "name": "pkg-b",
      "peerDependencies": {
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "vue": { "optional": true }
      }
    }"#;
    fs::create_dir_all(package_json_a_path.parent().unwrap()).unwrap();
    fs::create_dir_all(package_json_b_path.parent().unwrap()).unwrap();
    fs::write(&package_json_a_path, source_a).unwrap();
    fs::write(&package_json_b_path, source_b).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json_a = oxc_package_json(&package_json_a_path, source_a);
    let package_json_b = oxc_package_json(&package_json_b_path, source_b);

    let optional_peers_a =
      package_cache.cached_package_json_optional_peer_dep(&project_a, &package_json_a);
    let optional_peers_b =
      package_cache.cached_package_json_optional_peer_dep(&project_b, &package_json_b);

    assert!(!Arc::ptr_eq(&optional_peers_a, &optional_peers_b));
    assert_eq!(optional_peers_a.name, "pkg-a");
    assert!(optional_peers_a.optional_peer_dependencies.contains("react"));
    assert!(!optional_peers_a.optional_peer_dependencies.contains("vue"));
    assert_eq!(optional_peers_b.name, "pkg-b");
    assert!(optional_peers_b.optional_peer_dependencies.contains("vue"));
    assert!(!optional_peers_b.optional_peer_dependencies.contains("react"));

    let _ = fs::remove_dir_all(temp_root);
  }

  #[test]
  fn package_json_cache_clear_drops_cached_optional_peers() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-clear-cache");
    let package_json_path = root.join("package.json");
    let initial_source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    let updated_source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "vue": "^3.0.0"
      },
      "peerDependenciesMeta": {
        "vue": { "optional": true }
      }
    }"#;
    fs::create_dir_all(&root).unwrap();
    fs::write(&package_json_path, initial_source).unwrap();
    let package_cache = PackageJsonCache::default();
    let initial_package_json = oxc_package_json(&package_json_path, initial_source);

    let initial_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &initial_package_json);
    assert!(initial_optional_peers.optional_peer_dependencies.contains("react"));
    assert!(!initial_optional_peers.optional_peer_dependencies.contains("vue"));

    fs::write(&package_json_path, updated_source).unwrap();
    let updated_package_json = oxc_package_json(&package_json_path, updated_source);
    let stale_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &updated_package_json);
    assert!(Arc::ptr_eq(&initial_optional_peers, &stale_optional_peers));
    assert!(stale_optional_peers.optional_peer_dependencies.contains("react"));

    package_cache.clear();

    let refreshed_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &updated_package_json);
    assert!(!Arc::ptr_eq(&initial_optional_peers, &refreshed_optional_peers));
    assert!(refreshed_optional_peers.optional_peer_dependencies.contains("vue"));
    assert!(!refreshed_optional_peers.optional_peer_dependencies.contains("react"));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_cold_miss_computes_machine_source_path_once() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-source-path-once");
    let package_json_path = root.join("node_modules").join("pkg").join("package.json");
    let source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, source).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);

    reset_machine_source_path_count();
    let optional_peers = package_cache.cached_package_json_optional_peer_dep(&root, &package_json);

    assert!(optional_peers.optional_peer_dependencies.contains("react"));
    assert_eq!(machine_source_path_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_hot_hit_skips_machine_source_path_work() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-hot-hit-source-path-skip");
    let package_json_path = root.join("node_modules").join("pkg").join("package.json");
    let source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, source).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);

    let initial_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &package_json);
    assert!(initial_optional_peers.optional_peer_dependencies.contains("react"));

    reset_machine_source_path_count();
    let cached_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &package_json);

    assert!(Arc::ptr_eq(&initial_optional_peers, &cached_optional_peers));
    assert_eq!(machine_source_path_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn optional_peer_in_process_hot_hit_skips_cache_key_normalization_work() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("optional-peer-hot-hit-cache-key-skip");
    let package_json_path = root.join("node_modules").join("pkg").join("package.json");
    let source = br#"{
      "name": "pkg",
      "peerDependencies": {
        "react": "^18.0.0"
      },
      "peerDependenciesMeta": {
        "react": { "optional": true }
      }
    }"#;
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, source).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);

    let initial_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &package_json);
    assert!(initial_optional_peers.optional_peer_dependencies.contains("react"));

    reset_cache_key_count();
    let cached_optional_peers =
      package_cache.cached_package_json_optional_peer_dep(&root, &package_json);

    assert!(Arc::ptr_eq(&initial_optional_peers, &cached_optional_peers));
    assert_eq!(cache_key_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn package_json_side_effects_hot_hit_skips_cache_key_normalization_work() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("side-effects-hot-hit-cache-key-skip");
    let package_json_path = root.join("node_modules").join("pkg").join("package.json");
    let source = br#"{
      "name": "pkg",
      "sideEffects": false
    }"#;
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, source).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);

    let initial_side_effects = package_cache.cached_package_json_side_effects(&package_json);

    reset_cache_key_count();
    let cached_side_effects = package_cache.cached_package_json_side_effects(&package_json);

    assert!(Arc::ptr_eq(&initial_side_effects, &cached_side_effects));
    assert_eq!(cache_key_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn package_json_side_effects_cold_miss_without_dot_path_skips_duplicate_cache_lookup() {
    let _process_state = lock_package_json_cache_process_state();
    let root = unique_temp_root("side-effects-cold-miss-cache-lookup-once");
    let package_json_path = root.join("node_modules").join("pkg").join("package.json");
    let source = br#"{
      "name": "pkg",
      "sideEffects": false
    }"#;
    fs::create_dir_all(package_json_path.parent().unwrap()).unwrap();
    fs::write(&package_json_path, source).unwrap();
    let package_cache = PackageJsonCache::default();
    let package_json = oxc_package_json(&package_json_path, source);

    reset_side_effects_cache_lookup_count();
    let side_effects = package_cache.cached_package_json_side_effects(&package_json);

    assert_eq!(side_effects.name().as_deref(), Some("pkg"));
    assert_eq!(side_effects_cache_lookup_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  fn oxc_package_json(realpath: &Path, source: &[u8]) -> oxc_resolver::PackageJson {
    let fs = <oxc_resolver::FileSystemOs as oxc_resolver::FileSystem>::new(false);
    oxc_resolver::PackageJson::parse(
      &fs,
      realpath.to_path_buf(),
      realpath.to_path_buf(),
      source.to_vec(),
    )
    .unwrap()
  }

  fn unique_temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-vite-resolve-package-json-{label}-{}-{nanos}", std::process::id()))
  }

  fn large_optional_peer_package_json_source(name: &str) -> Vec<u8> {
    format!(
      r#"{{
        "name": "{name}",
        "description": "{}",
        "peerDependencies": {{
          "react": "^18.0.0",
          "vue": "^3.0.0"
        }},
        "peerDependenciesMeta": {{
          "react": {{ "optional": true }}
        }}
      }}"#,
      "x".repeat(16 * 1024)
    )
    .into_bytes()
  }

  fn optional_peer_package_json_source_with_exact_len(name: &str, len: usize) -> Vec<u8> {
    let prefix = format!(r#"{{"name":"{name}","description":""#);
    let suffix = r#"","peerDependencies":{"react":"^18.0.0","vue":"^3.0.0"},"peerDependenciesMeta":{"react":{"optional":true}}}"#;
    assert!(len >= prefix.len() + suffix.len());
    let source = format!("{}{}{}", prefix, "x".repeat(len - prefix.len() - suffix.len()), suffix);
    assert_eq!(source.len(), len);
    source.into_bytes()
  }

  fn package_json_source_without_optional_peers_with_exact_len(name: &str, len: usize) -> Vec<u8> {
    let prefix = format!(r#"{{"name":"{name}","description":""#);
    let suffix = r#""}"#;
    assert!(len >= prefix.len() + suffix.len());
    let source = format!("{}{}{}", prefix, "x".repeat(len - prefix.len() - suffix.len()), suffix);
    assert_eq!(source.len(), len);
    source.into_bytes()
  }

  fn lock_package_json_cache_process_state() -> MutexGuard<'static, ()> {
    PACKAGE_JSON_CACHE_PROCESS_STATE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  fn reset_machine_source_path_count() {
    PACKAGE_JSON_MACHINE_SOURCE_PATH_CALLS.with(|calls| calls.set(0));
  }

  fn machine_source_path_count() -> u64 {
    PACKAGE_JSON_MACHINE_SOURCE_PATH_CALLS.with(std::cell::Cell::get)
  }

  fn reset_optional_peer_parse_count() {
    PACKAGE_JSON_OPTIONAL_PEER_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn optional_peer_parse_count() -> u64 {
    PACKAGE_JSON_OPTIONAL_PEER_PARSE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_cache_key_count() {
    PACKAGE_JSON_CACHE_KEY_CALLS.with(|calls| calls.set(0));
  }

  fn cache_key_count() -> u64 {
    PACKAGE_JSON_CACHE_KEY_CALLS.with(std::cell::Cell::get)
  }

  fn reset_side_effects_cache_lookup_count() {
    PACKAGE_JSON_SIDE_EFFECTS_CACHE_LOOKUPS.with(|calls| calls.set(0));
  }

  fn side_effects_cache_lookup_count() -> u64 {
    PACKAGE_JSON_SIDE_EFFECTS_CACHE_LOOKUPS.with(std::cell::Cell::get)
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
    previous: Option<OsString>,
  }

  impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
      let previous = std::env::var_os(key);
      // SAFETY: this focused test scopes one process environment variable and restores it on drop.
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
}
