use std::{
  borrow::Cow,
  fmt,
  ops::Deref,
  path::{Path, PathBuf},
  sync::{Arc, LazyLock, atomic::Ordering},
};

use anyhow::Context;
use arcstr::ArcStr;
use regex::Regex;
use rolldown_common::{
  AssetFilenamesOutputOption, EmittedAsset, Output, OutputChunk, OutputFormat,
  RollupPreRenderedAsset,
};
use rolldown_plugin::{HookRenderChunkArgs, PluginContext};
use rolldown_plugin_utils::{
  AssetUrlItem, AssetUrlIter, AssetUrlResult, PublicAssetUrlCache, RenderAssetUrlInJsEnv,
  ToOutputFilePathEnv,
  constants::{
    CSSBundleName, CSSChunkCache, CSSEntriesCache, CSSStyles, CSSUrlCache, PureCSSChunks,
    RemovedPureCSSFilesCache, ViteMetadata,
  },
  create_to_import_meta_url_based_relative_runtime,
  css::is_css_request,
  get_chunk_original_name,
  uri::encode_uri_path,
};
use rolldown_utils::{
  dx_machine_cache::{
    DxMachineCacheConfig, DxMachineCacheHit, DxMachineCachePaths, DxMachineCacheStatus,
  },
  indexmap::FxIndexSet,
  url::clean_url,
};
use rustc_hash::{FxHashMap, FxHashSet};
use string_wizard::MagicString;
use sugar_path::SugarPath;

use crate::ViteCSSPostPlugin;

pub const VITE_HASH_UPDATE_MARKER: &str = "/*$vite$:1*/";
pub const DEFAULT_CSS_BUNDLE_NAME: &str = "style.css";
const CSS_PACKAGE_NAME_MACHINE_MAGIC: &[u8; 8] = b"RDXCSSN1";
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC: &[u8; 8] = b"RDXCSSRK";
const CSS_PACKAGE_NAME_MACHINE_HEADER_LEN: usize = 16;
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN: usize = 16;
const CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES: usize = 16 * 1024;

// TODO: improve below logic
pub static RE_UMD: LazyLock<Regex> = std::sync::LazyLock::new(|| {
  Regex::new(r#"\}\)\((?:this,\s*)?function\([^()]*\)\s*\{(?:\s*"use strict";)?"#).unwrap()
});

pub static RE_IIFE: LazyLock<Regex> = std::sync::LazyLock::new(|| {
  Regex::new(
    r#"(?:(?:const|var)\s+\S+\s*=\s*|^|\n)\(?function\([^()]*\)\s*\{(?:\s*"use strict";)?"#,
  )
  .unwrap()
});

static AT_IMPORT_RE: LazyLock<Regex> = std::sync::LazyLock::new(|| {
  Regex::new(r#"@import(?:\s*(?:url\([^)]*\)|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')[^;]*|[^;]*);"#)
    .unwrap()
});

static AT_CHARSET_RE: LazyLock<Regex> = std::sync::LazyLock::new(|| {
  Regex::new(r#"@charset(?:\s*(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*').*?|[^;]*);"#).unwrap()
});

static MULTI_LINE_COMMENTS_RE: LazyLock<Regex> =
  std::sync::LazyLock::new(|| Regex::new(r"\/\*[^*]*\*+(?:[^/*][^*]*\*+)*\/").unwrap());

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(compare(PartialEq))]
struct CssPackageNameDxSerializerPayload {
  name: String,
}

#[cfg(test)]
thread_local! {
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static CSS_PACKAGE_NAME_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CSS_PACKAGE_NAME_TEST_VALIDATED_HASH_REPAIR_WRITES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CSS_PACKAGE_NAME_TEST_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CSS_PACKAGE_NAME_TEST_JSON_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CSS_PACKAGE_NAME_TEST_MACHINE_SOURCE_PATH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static CSS_PACKAGE_NAME_TEST_MACHINE_CACHE_READ_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

fn encode_css_package_name_machine_payload(name: &str) -> Option<Vec<u8>> {
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  {
    return encode_css_package_name_dx_serializer_machine_payload(name);
  }

  #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
  {
    return encode_css_package_name_legacy_machine_payload(name);
  }
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn encode_css_package_name_dx_serializer_machine_payload(name: &str) -> Option<Vec<u8>> {
  let payload = CssPackageNameDxSerializerPayload { name: name.to_string() };
  let body = serializer::machine::api::serialize(&payload).ok()?;
  let body_len = u32::try_from(body.len()).ok()?;
  let mut machine_bytes =
    Vec::with_capacity(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN + body.len());
  machine_bytes.extend_from_slice(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
  machine_bytes.extend_from_slice(&body_len.to_le_bytes());
  machine_bytes.extend_from_slice(body.as_ref());
  Some(machine_bytes)
}

#[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
fn encode_css_package_name_legacy_machine_payload(name: &str) -> Option<Vec<u8>> {
  let len = u32::try_from(name.len()).ok()?;
  let mut machine_bytes = Vec::with_capacity(CSS_PACKAGE_NAME_MACHINE_HEADER_LEN + name.len());
  machine_bytes.extend_from_slice(CSS_PACKAGE_NAME_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  machine_bytes.extend_from_slice(name.as_bytes());
  Some(machine_bytes)
}

fn decode_css_package_name_machine_payload(machine_bytes: &[u8]) -> Option<String> {
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  if machine_bytes.starts_with(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC) {
    return decode_css_package_name_dx_serializer_machine_payload(machine_bytes);
  }

  decode_css_package_name_legacy_machine_payload(machine_bytes)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn decode_css_package_name_dx_serializer_machine_payload(machine_bytes: &[u8]) -> Option<String> {
  if machine_bytes.len() < CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC.len()]
    != CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC
  {
    return None;
  }
  if machine_bytes[8..12] != [0, 0, 0, 0] {
    return None;
  }

  let len = u32::from_le_bytes(machine_bytes[12..16].try_into().ok()?);
  let len = usize::try_from(len).ok()?;
  let end = CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN.checked_add(len)?;
  let body = machine_bytes.get(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN..end)?;
  if end != machine_bytes.len() {
    return None;
  }

  Some(css_package_name_dx_serializer_archived_payload(body, |archived| {
    archived.name.as_str().to_string()
  }))
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn css_package_name_dx_serializer_archived_payload<R>(
  body: &[u8],
  decode: impl FnOnce(&<CssPackageNameDxSerializerPayload as rkyv::Archive>::Archived) -> R,
) -> R {
  if css_package_name_dx_serializer_body_is_aligned(body) {
    // SAFETY: The cache metadata layer validates source and machine hashes before
    // this decoder runs. Aligned bodies can be read directly through RKYV.
    let archived =
      unsafe { serializer::machine::api::deserialize::<CssPackageNameDxSerializerPayload>(body) };
    return decode(archived);
  }

  #[cfg(test)]
  CSS_PACKAGE_NAME_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(calls.get() + 1));

  let mut aligned: rkyv::util::AlignedVec<16> = rkyv::util::AlignedVec::with_capacity(body.len());
  aligned.extend_from_slice(body);
  // SAFETY: The cache metadata layer validates source and machine hashes before
  // this decoder runs, and the RKYV body is copied into aligned storage first.
  let archived =
    unsafe { serializer::machine::api::deserialize::<CssPackageNameDxSerializerPayload>(&aligned) };
  decode(archived)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn css_package_name_dx_serializer_body_is_aligned(body: &[u8]) -> bool {
  let alignment =
    std::mem::align_of::<<CssPackageNameDxSerializerPayload as rkyv::Archive>::Archived>();
  body.as_ptr() as usize % alignment == 0
}

fn decode_css_package_name_legacy_machine_payload(machine_bytes: &[u8]) -> Option<String> {
  if machine_bytes.len() < CSS_PACKAGE_NAME_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..CSS_PACKAGE_NAME_MACHINE_MAGIC.len()] != CSS_PACKAGE_NAME_MACHINE_MAGIC {
    return None;
  }
  if machine_bytes[8..12] != [0, 0, 0, 0] {
    return None;
  }

  let len = u32::from_le_bytes(machine_bytes[12..16].try_into().ok()?);
  let len = usize::try_from(len).ok()?;
  let end = CSS_PACKAGE_NAME_MACHINE_HEADER_LEN.checked_add(len)?;
  let bytes = machine_bytes.get(CSS_PACKAGE_NAME_MACHINE_HEADER_LEN..end)?;
  if end != machine_bytes.len() {
    return None;
  }
  std::str::from_utf8(bytes).ok().map(ToString::to_string)
}

fn parse_css_package_name_for_bundle_bytes(source: &[u8]) -> anyhow::Result<Option<String>> {
  #[cfg(test)]
  CSS_PACKAGE_NAME_TEST_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));

  let Some(document) = parse_css_package_name_document(source)? else {
    return Ok(None);
  };
  let name = document.name.ok_or_else(|| {
    anyhow::anyhow!(
      "Name in package.json is required if option 'build.lib.cssFileName' is not provided."
    )
  })?;

  Ok(Some(normalize_css_package_name_for_bundle(name).to_string()))
}

fn parse_css_package_name_document(
  source: &[u8],
) -> anyhow::Result<Option<CssPackageNameDocument<'_>>> {
  #[cfg(test)]
  CSS_PACKAGE_NAME_TEST_JSON_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));

  match serde_json::from_slice::<CssPackageNameRoot<'_>>(strip_utf8_bom_bytes(source))? {
    CssPackageNameRoot::Object(document) => Ok(Some(document)),
    CssPackageNameRoot::Other(_) => Ok(None),
  }
}

fn strip_utf8_bom_bytes(source: &[u8]) -> &[u8] {
  source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source)
}

#[derive(serde::Deserialize)]
#[serde(untagged, bound(deserialize = "'de: 'a"))]
enum CssPackageNameRoot<'a> {
  Object(CssPackageNameDocument<'a>),
  Other(serde::de::IgnoredAny),
}

struct CssPackageNameDocument<'a> {
  name: Option<&'a str>,
}

impl<'de> serde::Deserialize<'de> for CssPackageNameDocument<'de> {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    struct CssPackageNameDocumentVisitor;

    impl<'de> serde::de::Visitor<'de> for CssPackageNameDocumentVisitor {
      type Value = CssPackageNameDocument<'de>;

      fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a package.json object")
      }

      fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
      where
        A: serde::de::MapAccess<'de>,
      {
        let mut name = None;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
          if key == "name" {
            name = match map.next_value::<CssPackageNameField<'de>>()? {
              CssPackageNameField::String(value) => Some(value),
              CssPackageNameField::Other(_) => None,
            };
          } else {
            map.next_value::<serde::de::IgnoredAny>()?;
          }
        }

        Ok(CssPackageNameDocument { name })
      }
    }

    deserializer.deserialize_map(CssPackageNameDocumentVisitor)
  }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum CssPackageNameField<'a> {
  String(#[serde(borrow)] &'a str),
  Other(serde::de::IgnoredAny),
}

fn normalize_css_package_name_for_bundle(name: &str) -> &str {
  if name.starts_with('@') {
    name.split_once('/').map(|(_, second)| second).unwrap_or(name)
  } else {
    name
  }
}

fn read_css_package_name_for_bundle(
  project_root: &Path,
  pkg_path: &Path,
) -> anyhow::Result<Option<String>> {
  let cache_config = DxMachineCacheConfig::from_env_if_enabled(project_root);
  read_css_package_name_for_bundle_with_cache(project_root, pkg_path, cache_config.as_ref())
}

fn read_css_package_name_for_bundle_with_cache(
  project_root: &Path,
  pkg_path: &Path,
  cache_config: Option<&DxMachineCacheConfig>,
) -> anyhow::Result<Option<String>> {
  let source_bytes = std::fs::read(pkg_path)?;
  let cache_target = (source_bytes.len() > CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES)
    .then(|| {
      cache_config.and_then(|cache_config| {
        let machine_source_path = css_package_name_machine_source_path(project_root, pkg_path);
        let cache_paths = css_package_name_machine_cache_paths_if_enabled(
          project_root,
          machine_source_path.as_ref(),
          cache_config,
        )?;
        Some((cache_config, machine_source_path, cache_paths))
      })
    })
    .flatten();

  let mut validated_source_hash = None;
  if let Some((cache_config, machine_source_path, cache_paths)) = cache_target.as_ref()
    && let Some(hit) = read_css_package_name_validated_machine_hit(
      cache_config,
      cache_paths,
      machine_source_path.as_ref(),
      pkg_path,
      &source_bytes,
    )
  {
    if let Some(name) = decode_css_package_name_machine_payload(&hit.machine_bytes) {
      return Ok(Some(name));
    }
    validated_source_hash = Some(hit.source_hash);
  }

  let Some(name) = parse_css_package_name_for_bundle_bytes(&source_bytes)? else {
    return Ok(None);
  };

  if let Some((cache_config, machine_source_path, cache_paths)) = cache_target.as_ref()
    && let Some(machine_bytes) = encode_css_package_name_machine_payload(&name)
  {
    if let Some(source_hash) = validated_source_hash {
      #[cfg(test)]
      CSS_PACKAGE_NAME_TEST_VALIDATED_HASH_REPAIR_WRITES
        .with(|writes| writes.set(writes.get() + 1));
      let _ = cache_config.write_machine_artifact_with_source_hash(
        cache_paths,
        machine_source_path.as_ref(),
        source_bytes.len(),
        &source_hash,
        &machine_bytes,
      );
    } else {
      let _ = cache_config.write_machine_artifact(
        cache_paths,
        machine_source_path.as_ref(),
        &source_bytes,
        &machine_bytes,
      );
    }
  }

  Ok(Some(name))
}

fn css_package_name_machine_source_path<'pkg>(
  project_root: &Path,
  pkg_path: &'pkg Path,
) -> Cow<'pkg, Path> {
  #[cfg(test)]
  CSS_PACKAGE_NAME_TEST_MACHINE_SOURCE_PATH_CALLS.with(|calls| calls.set(calls.get() + 1));

  pkg_path.strip_prefix(project_root).map_or_else(|_| Cow::Borrowed(pkg_path), Cow::Borrowed)
}

fn read_css_package_name_validated_machine_hit(
  cache_config: &DxMachineCacheConfig,
  cache_paths: &DxMachineCachePaths,
  machine_source_path: &Path,
  pkg_path: &Path,
  source_bytes: &[u8],
) -> Option<DxMachineCacheHit> {
  match read_css_package_name_machine_cache(
    cache_config,
    cache_paths,
    machine_source_path,
    source_bytes,
  ) {
    DxMachineCacheStatus::Hit(hit) => Some(hit),
    DxMachineCacheStatus::Invalid if machine_source_path != pkg_path => {
      match read_css_package_name_machine_cache(cache_config, cache_paths, pkg_path, source_bytes) {
        DxMachineCacheStatus::Hit(hit) => Some(hit),
        _ => None,
      }
    }
    _ => None,
  }
}

fn read_css_package_name_machine_cache(
  cache_config: &DxMachineCacheConfig,
  cache_paths: &DxMachineCachePaths,
  source_path: &Path,
  source_bytes: &[u8],
) -> DxMachineCacheStatus<DxMachineCacheHit> {
  #[cfg(test)]
  CSS_PACKAGE_NAME_TEST_MACHINE_CACHE_READ_CALLS.with(|calls| calls.set(calls.get() + 1));

  cache_config.read_validated_machine_with_source_hash(cache_paths, source_path, source_bytes)
}

fn css_package_name_machine_cache_paths_if_enabled(
  project_root: &Path,
  pkg_path: &Path,
  cache_config: &DxMachineCacheConfig,
) -> Option<DxMachineCachePaths> {
  cache_config
    .enabled
    .then(|| cache_config.paths_for_source(project_root, "package_json_css_name", pkg_path))
}

pub fn extract_index(id: &str) -> Option<&str> {
  let s = id.split_once("&index=")?.1;
  let end = s.as_bytes().iter().take_while(|b| b.is_ascii_digit()).count();
  (end > 0).then_some(&s[..end])
}

pub struct FinalizedContext<'a, 'b, 'c> {
  pub plugin_ctx: &'a PluginContext,
  pub env: &'a ToOutputFilePathEnv<'b>,
  pub args: &'a HookRenderChunkArgs<'c>,
}

impl Deref for FinalizedContext<'_, '_, '_> {
  type Target = PluginContext;

  fn deref(&self) -> &Self::Target {
    self.plugin_ctx
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_css_package_name_dx_serializer_aligned_copy_count() {
    CSS_PACKAGE_NAME_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn css_package_name_dx_serializer_aligned_copy_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.get())
  }

  fn reset_css_package_name_validated_hash_repair_write_count() {
    CSS_PACKAGE_NAME_TEST_VALIDATED_HASH_REPAIR_WRITES.with(|writes| writes.set(0));
  }

  fn css_package_name_validated_hash_repair_write_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_VALIDATED_HASH_REPAIR_WRITES.with(|writes| writes.get())
  }

  fn reset_css_package_name_parse_count() {
    CSS_PACKAGE_NAME_TEST_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn css_package_name_parse_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_PARSE_CALLS.with(|calls| calls.get())
  }

  fn reset_css_package_name_json_parse_count() {
    CSS_PACKAGE_NAME_TEST_JSON_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn css_package_name_json_parse_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_JSON_PARSE_CALLS.with(|calls| calls.get())
  }

  fn reset_css_package_name_machine_source_path_count() {
    CSS_PACKAGE_NAME_TEST_MACHINE_SOURCE_PATH_CALLS.with(|calls| calls.set(0));
  }

  fn css_package_name_machine_source_path_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_MACHINE_SOURCE_PATH_CALLS.with(|calls| calls.get())
  }

  fn reset_css_package_name_machine_cache_read_count() {
    CSS_PACKAGE_NAME_TEST_MACHINE_CACHE_READ_CALLS.with(|calls| calls.set(0));
  }

  fn css_package_name_machine_cache_read_count() -> u64 {
    CSS_PACKAGE_NAME_TEST_MACHINE_CACHE_READ_CALLS.with(|calls| calls.get())
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn css_package_name_dx_serializer_body_decodes_without_aligned_copy() {
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();
    let body = &machine_bytes[CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN..];

    assert!(css_package_name_dx_serializer_body_is_aligned(body));

    reset_css_package_name_dx_serializer_aligned_copy_count();
    assert_eq!(decode_css_package_name_machine_payload(&machine_bytes).as_deref(), Some("ui-kit"));
    assert_eq!(css_package_name_dx_serializer_aligned_copy_count(), 0);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn css_package_name_dx_serializer_body_decodes_from_unaligned_slice_with_one_copy() {
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();
    let mut unaligned_storage = Vec::with_capacity(machine_bytes.len() + 1);
    unaligned_storage.push(0);
    unaligned_storage.extend_from_slice(&machine_bytes);
    let unaligned_machine_bytes = &unaligned_storage[1..];
    let body = &unaligned_machine_bytes[CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_HEADER_LEN..];

    assert!(!css_package_name_dx_serializer_body_is_aligned(body));

    reset_css_package_name_dx_serializer_aligned_copy_count();
    assert_eq!(
      decode_css_package_name_machine_payload(unaligned_machine_bytes).as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_dx_serializer_aligned_copy_count(), 1);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn css_package_name_machine_payload_uses_dx_serializer_rkyv_when_enabled() {
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();

    assert!(machine_bytes.starts_with(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));
    assert_eq!(decode_css_package_name_machine_payload(&machine_bytes).as_deref(), Some("ui-kit"));
  }

  #[test]
  fn css_package_name_machine_payload_round_trips_compact_binary() {
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();

    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    assert!(machine_bytes.starts_with(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert!(machine_bytes.starts_with(CSS_PACKAGE_NAME_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));
    assert_eq!(decode_css_package_name_machine_payload(&machine_bytes).as_deref(), Some("ui-kit"));
  }

  #[test]
  fn css_package_name_parser_preserves_current_bundle_name_rules() {
    assert_eq!(
      parse_css_package_name_for_bundle_bytes(b"\xef\xbb\xbf{\"name\":\"@scope/ui-kit\"}")
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(parse_css_package_name_for_bundle_bytes(br#"["not", "object"]"#).unwrap(), None);

    let error = parse_css_package_name_for_bundle_bytes(br#"{"version":"1.0.0"}"#).unwrap_err();
    assert!(error.to_string().contains("Name in package.json is required"));
  }

  #[test]
  fn css_package_name_parser_accepts_bom_bytes_without_utf8_prepass() {
    assert_eq!(
      parse_css_package_name_for_bundle_bytes(b"\xef\xbb\xbf{\"name\":\"@scope/ui-kit\"}")
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
  }

  #[test]
  fn css_package_name_document_ignores_unused_values() {
    let document = parse_css_package_name_document(
      br#"{"unused":{"deep":["value",{"nested":true}]},"name":"@scope/ui-kit"}"#,
    )
    .unwrap();

    assert_eq!(document.and_then(|document| document.name), Some("@scope/ui-kit"));
  }

  #[test]
  fn css_package_name_cache_writes_and_invalidates_machine_payload() {
    let project_root = unique_temp_root("css-name-cache").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/ui-kit")).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", &pkg_path);

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    let machine_bytes = std::fs::read(&cache_paths.machine).unwrap();
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    assert!(machine_bytes.starts_with(CSS_PACKAGE_NAME_DX_SERIALIZER_MACHINE_MAGIC));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert!(machine_bytes.starts_with(CSS_PACKAGE_NAME_MACHINE_MAGIC));

    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/new-kit")).unwrap();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("new-kit")
    );

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_repairs_validated_decode_miss_with_source_hash() {
    let project_root = unique_temp_root("css-name-validated-hash-repair").join("project");
    let pkg_path = project_root.join("package.json");
    let source = large_css_package_json_for_name("@scope/ui-kit");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, &source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", &pkg_path);
    let stale_machine = b"validated-but-undecodable";
    cache_config
      .write_machine_artifact(&cache_paths, &pkg_path, source.as_bytes(), stale_machine)
      .unwrap();

    reset_css_package_name_validated_hash_repair_write_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );

    assert_eq!(css_package_name_validated_hash_repair_write_count(), 1);
    let repaired_machine = std::fs::read(&cache_paths.machine).unwrap();
    assert_ne!(repaired_machine, stale_machine);
    assert_eq!(
      decode_css_package_name_machine_payload(&repaired_machine).as_deref(),
      Some("ui-kit")
    );

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_machine_cache_hit_skips_package_json_parse() {
    let project_root = unique_temp_root("css-name-hit-skips-parse").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/ui-kit")).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );

    reset_css_package_name_parse_count();
    reset_css_package_name_json_parse_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_parse_count(), 0);
    assert_eq!(css_package_name_json_parse_count(), 0);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_reuses_relative_metadata_for_absolute_package_path() {
    let project_root = unique_temp_root("css-name-relative-metadata").join("project");
    let pkg_path = project_root.join("package.json");
    let source = large_css_package_json_for_name("@scope/ui-kit");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, &source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let relative_pkg_path = Path::new("package.json");
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", relative_pkg_path);
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();
    cache_config
      .write_machine_artifact(&cache_paths, relative_pkg_path, source.as_bytes(), &machine_bytes)
      .unwrap();

    reset_css_package_name_parse_count();
    reset_css_package_name_json_parse_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_parse_count(), 0);
    assert_eq!(css_package_name_json_parse_count(), 0);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_cold_miss_skips_absolute_package_path_retry() {
    let project_root = unique_temp_root("css-name-cold-miss-single-read").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/ui-kit")).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));

    reset_css_package_name_machine_cache_read_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );

    assert_eq!(css_package_name_machine_cache_read_count(), 1);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_preserves_absolute_metadata_fallback_after_relative_invalid() {
    let project_root = unique_temp_root("css-name-absolute-metadata-fallback").join("project");
    let pkg_path = project_root.join("package.json");
    let source = large_css_package_json_for_name("@scope/ui-kit");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, &source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let relative_pkg_path = Path::new("package.json");
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", relative_pkg_path);
    let machine_bytes = encode_css_package_name_machine_payload("ui-kit").unwrap();
    cache_config
      .write_machine_artifact(&cache_paths, &pkg_path, source.as_bytes(), &machine_bytes)
      .unwrap();

    reset_css_package_name_parse_count();
    reset_css_package_name_json_parse_count();
    reset_css_package_name_machine_cache_read_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );

    assert_eq!(css_package_name_parse_count(), 0);
    assert_eq!(css_package_name_json_parse_count(), 0);
    assert_eq!(css_package_name_machine_cache_read_count(), 2);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_skips_tiny_package_json() {
    let project_root = unique_temp_root("css-name-tiny-cache").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, br#"{"name":"@scope/ui-kit"}"#).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert!(!project_root.join(".dx").exists());

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_skips_machine_source_path_for_tiny_package_json() {
    let project_root = unique_temp_root("css-name-tiny-no-machine-source-path").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, br#"{"name":"@scope/ui-kit"}"#).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));

    reset_css_package_name_machine_source_path_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_machine_source_path_count(), 0);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_skips_machine_source_path_when_cache_disabled() {
    let project_root = unique_temp_root("css-name-disabled-no-machine-source-path").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/ui-kit")).unwrap();

    reset_css_package_name_machine_source_path_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, None)
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_machine_source_path_count(), 0);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_keeps_machine_source_path_for_cacheable_package_json() {
    let project_root = unique_temp_root("css-name-cacheable-machine-source-path").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, large_css_package_json_for_name("@scope/ui-kit")).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));

    reset_css_package_name_machine_source_path_count();
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert_eq!(css_package_name_machine_source_path_count(), 1);

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_skips_sources_at_exact_payoff_boundary() {
    let project_root = unique_temp_root("css-name-boundary-cache-skip").join("project");
    let pkg_path = project_root.join("package.json");
    let source = css_package_json_for_name_with_exact_len(
      "@scope/ui-kit",
      CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES,
    );
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", &pkg_path);

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_writes_sources_above_payoff_boundary() {
    let project_root = unique_temp_root("css-name-boundary-cache-write").join("project");
    let pkg_path = project_root.join("package.json");
    let source = css_package_json_for_name_with_exact_len(
      "@scope/ui-kit",
      CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES + 1,
    );
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", &pkg_path);

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert!(cache_paths.machine.exists());
    assert!(cache_paths.metadata.exists());

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_cache_skips_large_non_object_json_on_cold_miss() {
    let project_root = unique_temp_root("css-name-large-non-object-cache").join("project");
    let pkg_path = project_root.join("package.json");
    let source = format!(r#"["{}"]"#, "x".repeat(CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES));
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, source).unwrap();
    let cache_config =
      DxMachineCacheConfig::from_env_value(&project_root, Some(std::ffi::OsString::from("1")));
    let cache_paths =
      cache_config.paths_for_source(&project_root, "package_json_css_name", &pkg_path);

    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, Some(&cache_config))
        .unwrap(),
      None
    );
    assert!(!cache_paths.machine.exists());
    assert!(!cache_paths.metadata.exists());

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  #[test]
  fn css_package_name_disabled_cache_has_no_paths() {
    let project_root = unique_temp_root("css-name-disabled-cache").join("project");
    let pkg_path = project_root.join("package.json");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(&pkg_path, br#"{"name":"@scope/ui-kit"}"#).unwrap();
    let cache_config = DxMachineCacheConfig::from_env_value(&project_root, None);

    assert!(
      css_package_name_machine_cache_paths_if_enabled(&project_root, &pkg_path, &cache_config)
        .is_none()
    );
    assert_eq!(
      read_css_package_name_for_bundle_with_cache(&project_root, &pkg_path, None)
        .unwrap()
        .as_deref(),
      Some("ui-kit")
    );
    assert!(!project_root.join(".dx").exists());

    let _ = std::fs::remove_dir_all(project_root.parent().unwrap());
  }

  fn unique_temp_root(label: &str) -> PathBuf {
    let nanos =
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-vite-css-post-{label}-{}-{nanos}", std::process::id()))
  }

  fn large_css_package_json_for_name(name: &str) -> String {
    format!(
      r#"{{"name":"{name}","padding":"{}"}}"#,
      "x".repeat(CSS_PACKAGE_NAME_MACHINE_MIN_SOURCE_BYTES)
    )
  }

  fn css_package_json_for_name_with_exact_len(name: &str, len: usize) -> String {
    let prefix = format!(r#"{{"name":"{name}","padding":""#);
    let suffix = r#""}"#;
    let padding_len = len.checked_sub(prefix.len() + suffix.len()).unwrap();
    format!("{prefix}{}{suffix}", "x".repeat(padding_len))
  }
}

impl ViteCSSPostPlugin {
  pub async fn finalize_vite_css_urls<'a>(
    &self,
    ctx: &FinalizedContext<'a, '_, '_>,
    css_styles: &CSSStyles,
    magic_string: &mut Option<MagicString<'a>>,
  ) -> anyhow::Result<()> {
    let mut css_url_iter = ctx.args.code.match_indices("__VITE_CSS_URL__").peekable();
    if css_url_iter.peek().is_some() {
      let indices = css_url_iter.map(|(index, _)| index).collect::<Vec<_>>();
      let magic_string =
        magic_string.get_or_insert_with(|| string_wizard::MagicString::new(ctx.args.code.as_str()));

      let url_cache = ctx.meta().get_or_insert_default::<CSSUrlCache>();
      for index in indices {
        let start = index + "__VITE_CSS_URL__".len();
        let Some(pos) = ctx.args.code[start..].find("__") else {
          return Err(anyhow::anyhow!(
            "Invalid __VITE_CSS_URL__ in '{}', expected '__VITE_CSS_URL__<base64>__'",
            ctx.args.chunk.name
          ));
        };

        let id = unsafe {
          String::from_utf8_unchecked(
            base64_simd::STANDARD
              .decode_to_vec(&ctx.args.code[start..start + pos])
              .context("Invalid base64 in '__VITE_CSS_URL__'")?,
          )
        };

        if let Some(url) = url_cache.inner.get(&id) {
          #[expect(clippy::cast_possible_truncation)]
          magic_string
            .update(index as u32, (start + pos + 2) as u32, url.clone())
            .expect("update should not fail in css post plugin");
          continue;
        }

        let Some(style) = css_styles.inner.get(&id).map(|s| s.to_owned()) else {
          return Err(anyhow::anyhow!("CSS content for  '{id}' was not found"));
        };

        let original_file_name = clean_url(&id).to_string();
        let css_asset_path = PathBuf::from(&original_file_name).with_extension("css");
        let css_asset_name =
          css_asset_path.file_name().map(|v| v.to_string_lossy().into_owned()).unwrap();

        let content = self
          .resolve_asset_urls_in_css(
            ctx,
            &style,
            &css_asset_name,
            &ctx.args.options.asset_filenames,
          )
          .await?;
        let content = self.finalize_css(content).await?;

        let reference_id = ctx
          .emit_file_async(EmittedAsset {
            name: Some(css_asset_name),
            source: content.into(),
            original_file_name: Some(original_file_name),
            ..Default::default()
          })
          .await?;

        let filename = ctx.get_file_name(&reference_id)?;
        let vite_metadata = ctx.meta().get_or_insert_default::<ViteMetadata>();
        let chunk_metadata = vite_metadata.get(ctx.env.host_id.into());

        chunk_metadata.imported_assets.insert(clean_url(&filename).into());

        let url = ctx
          .env
          .to_output_file_path(
            &filename,
            "js",
            false,
            create_to_import_meta_url_based_relative_runtime(ctx.options().format, self.is_worker),
          )
          .await?
          .to_asset_url_in_js()?;

        url_cache.inner.insert(id, url.clone());
        #[expect(clippy::cast_possible_truncation)]
        magic_string
          .update(index as u32, (start + pos + 2) as u32, url)
          .expect("update should not fail in css post plugin");
      }
    }
    Ok(())
  }

  pub async fn finalize_css_chunk<'a>(
    &self,
    ctx: &FinalizedContext<'a, '_, '_>,
    css_chunk: Option<String>,
    is_pure_css_chunk: bool,
    magic_string: &mut Option<MagicString<'a>>,
  ) -> anyhow::Result<()> {
    let Some(css_chunk) = css_chunk else {
      return Ok(());
    };

    if is_pure_css_chunk && ctx.args.options.format.is_esm_or_cjs() {
      ctx
        .meta()
        .get::<PureCSSChunks>()
        .expect("PureCSSChunks missing")
        .inner
        .insert(ctx.args.chunk.filename.clone());
    }

    if self.css_code_split {
      if ctx.args.options.format.is_esm_or_cjs() && !ctx.args.chunk.filename.contains("-legacy") {
        let css_asset_path = PathBuf::from(ctx.args.chunk.name.as_str()).with_extension("css");
        // if facadeModuleId doesn't exist or doesn't have a CSS extension,
        // that means a JS entry file imports a CSS file.
        // in this case, only use the filename for the CSS chunk name like JS chunks.
        let css_asset_name = if ctx.args.chunk.is_entry
          && ctx.args.chunk.facade_module_id.as_ref().is_none_or(|id| !is_css_request(id.as_str()))
        {
          css_asset_path.file_name().map(|v| v.to_string_lossy().into_owned()).unwrap()
        } else {
          css_asset_path.to_string_lossy().into_owned()
        };

        // TODO: Cache legacy check result per chunk
        let is_legacy = match &self.is_legacy {
          Some(is_legacy_fn) => is_legacy_fn(ctx.args.options).await?,
          None => false,
        };

        let original_file_name = get_chunk_original_name(
          &self.root,
          is_legacy,
          &ctx.args.chunk.name,
          ctx.args.chunk.facade_module_id.as_ref(),
        );

        let content = self
          .resolve_asset_urls_in_css(
            ctx,
            &css_chunk,
            &css_asset_name,
            &ctx.args.options.asset_filenames,
          )
          .await?;
        let content = self.finalize_css(content).await?;

        let reference_id = ctx
          .emit_file_async(EmittedAsset {
            name: Some(css_asset_name),
            source: content.into(),
            original_file_name,
            ..Default::default()
          })
          .await?;

        let vite_metadata = ctx.meta().get_or_insert_default::<ViteMetadata>();
        let chunk_metadata = vite_metadata.get(ctx.env.host_id.into());
        chunk_metadata.imported_css.insert(ctx.get_file_name(&reference_id)?);

        if ctx.args.chunk.is_entry && is_pure_css_chunk {
          ctx
            .meta()
            .get::<CSSEntriesCache>()
            .expect("CSSEntriesCache missing")
            .inner
            .insert(ctx.args.chunk.name.clone(), reference_id);
        }
      } else if self.is_client {
        let injection_point = match ctx.args.options.format {
          OutputFormat::Esm => {
            if ctx.args.code.starts_with("#!") {
              ctx.args.code.find('\n').unwrap_or(0)
            } else {
              0
            }
          }
          OutputFormat::Iife | OutputFormat::Umd => {
            let regex = if matches!(ctx.args.options.format, OutputFormat::Iife) {
              &RE_IIFE
            } else {
              &RE_UMD
            };
            let Some(m) = regex.find(&ctx.args.code) else {
              return Err(anyhow::anyhow!("Injection point for inlined CSS not found"));
            };
            m.end()
          }
          OutputFormat::Cjs => {
            return Err(anyhow::anyhow!("CJS format is not supported for CSS injection"));
          }
        };

        let content = serde_json::to_string(&self.finalize_css(css_chunk).await?)?;
        let env =
          RenderAssetUrlInJsEnv { ctx, env: ctx.env, code: &content, is_worker: self.is_worker };

        let css_string = env.render_asset_url_in_js().await?.unwrap_or(content);
        let inject_code = rolldown_utils::concat_string!(
          "var __vite_style__ = document.createElement('style');__vite_style__.textContent = ",
          css_string,
          ";document.head.appendChild(__vite_style__);"
        );

        #[expect(clippy::cast_possible_truncation)]
        magic_string
          .get_or_insert_with(|| string_wizard::MagicString::new(ctx.args.code.as_str()))
          .append_right(injection_point as u32, inject_code);
      }
    } else {
      ctx.meta().get::<CSSChunkCache>().expect("CSSChunkCache missing").inner.insert(
        ctx.args.chunk.filename.clone(),
        self
          .resolve_asset_urls_in_css(
            ctx,
            &css_chunk,
            &self.get_css_bundle_name(ctx)?,
            &ctx.args.options.asset_filenames,
          )
          .await?,
      );
    }

    Ok(())
  }

  pub async fn resolve_asset_urls_in_css(
    &self,
    ctx: &FinalizedContext<'_, '_, '_>,
    css_chunk: &str,
    css_asset_name: &str,
    css_file_names: &AssetFilenamesOutputOption,
  ) -> anyhow::Result<String> {
    let css_asset_dirname = self.get_css_asset_dir_name(css_asset_name, css_file_names).await?;

    let to_relative = |filename: &Path, _host_id: &Path| {
      let relative_path = filename.relative(css_asset_dirname.as_str());
      let relative_path = relative_path.to_slash_lossy();
      if relative_path.starts_with('.') {
        AssetUrlResult::WithoutRuntime(relative_path.into_owned())
      } else {
        AssetUrlResult::WithoutRuntime(rolldown_utils::concat_string!("./", relative_path))
      }
    };

    let mut magic_string = None;
    for item in AssetUrlIter::from(css_chunk).into_asset_url_iter() {
      let s = magic_string.get_or_insert_with(|| string_wizard::MagicString::new(css_chunk));
      match item {
        AssetUrlItem::Asset((range, reference_id, postfix)) => {
          let filename = ctx.get_file_name(reference_id)?;
          let filename = if let Some(postfix) = postfix {
            Cow::Owned(rolldown_utils::concat_string!(filename, postfix))
          } else {
            Cow::Borrowed(filename.as_str())
          };

          let vite_metadata = ctx.meta().get_or_insert_default::<ViteMetadata>();
          let chunk_metadata = vite_metadata.get(ctx.env.host_id.into());

          chunk_metadata.imported_assets.insert(clean_url(&filename).into());

          let env = ToOutputFilePathEnv {
            is_ssr: self.is_ssr,
            host_id: &ctx.args.chunk.filename,
            url_base: &self.url_base,
            decoded_base: &self.decoded_base,
            render_built_url: self.render_built_url.as_deref(),
          };

          s.update(
            range.start,
            range.end,
            encode_uri_path(
              env
                .to_output_file_path(&filename, "css", false, to_relative)
                .await?
                .to_asset_url_in_css_or_html(),
            ),
          )
          .expect("update should not fail in css post plugin");
        }
        AssetUrlItem::PublicAsset((range, hash)) => {
          let cache = ctx
            .meta()
            .get::<PublicAssetUrlCache>()
            .ok_or_else(|| anyhow::anyhow!("PublicAssetUrlCache missing"))?;

          let public_url = cache
            .0
            .get(hash)
            .ok_or_else(|| {
              anyhow::anyhow!(
                "Can't find the cache of {}",
                &css_chunk[(range.start as usize)..(range.end as usize)]
              )
            })?
            .to_string();

          let env = ToOutputFilePathEnv {
            is_ssr: self.is_ssr,
            host_id: &ctx.args.chunk.filename,
            url_base: &self.url_base,
            decoded_base: &self.decoded_base,
            render_built_url: self.render_built_url.as_deref(),
          };

          let relative_path = self.root.relative(css_asset_dirname.as_str());
          let relative_path = relative_path.to_slash_lossy();

          s.update(
            range.start,
            range.end,
            encode_uri_path(
              env
                .to_output_file_path(&public_url, "css", true, |_: &Path, _: &Path| {
                  AssetUrlResult::WithoutRuntime(rolldown_utils::concat_string!(
                    relative_path,
                    public_url
                  ))
                })
                .await?
                .to_asset_url_in_css_or_html(),
            ),
          )
          .expect("update should not fail in css post plugin");
        }
      }
    }

    Ok(if let Some(magic_string) = magic_string {
      magic_string.to_string()
    } else {
      css_chunk.to_owned()
    })
  }

  pub async fn finalize_css(&self, mut content: String) -> anyhow::Result<String> {
    // hoist external @imports and @charset to the top of the CSS chunk per spec (#1845 and #6333)
    if content.contains("@import") || content.contains("@charset") {
      content = Self::hoist_at_rules(&content);
    }
    // TODO: Maybe we should use internal lightningcss minify
    if let Some(css_minify) = &self.css_minify {
      content = (css_minify)(content, false).await?;
    }
    // inject an additional string to generate a different hash for https://github.com/vitejs/vite/issues/18038
    //
    // pre-5.4.3, we generated CSS link tags without crossorigin attribute and generated an hash without
    // this string
    // in 5.4.3, we added crossorigin attribute to the generated CSS link tags but that made chromium browsers
    // to block the CSSs from loading due to chromium's weird behavior
    // (https://www.hacksoft.io/blog/handle-images-cors-error-in-chrome, https://issues.chromium.org/issues/40381978)
    // to avoid that happening, we inject an additional string so that a different hash is generated
    // for the same CSS content
    Ok(rolldown_utils::concat_string!(content, VITE_HASH_UPDATE_MARKER))
  }

  pub fn hoist_at_rules(css: &str) -> String {
    let mut css_without_comments = css.to_owned();
    let bytes = unsafe { css_without_comments.as_bytes_mut() };
    for matched in MULTI_LINE_COMMENTS_RE.find_iter(css) {
      bytes[matched.range()].fill(b' ');
    }

    let mut s = string_wizard::MagicString::new(css);
    for matched in AT_IMPORT_RE.find_iter(&css_without_comments) {
      #[expect(clippy::cast_possible_truncation)]
      s.remove(matched.start() as u32, matched.end() as u32)
        .expect("remove should not fail in css post plugin");
      s.append_left(0, matched.as_str());
    }

    let mut found_charset = false;
    for matched in AT_CHARSET_RE.find_iter(&css_without_comments) {
      #[expect(clippy::cast_possible_truncation)]
      s.remove(matched.start() as u32, matched.end() as u32)
        .expect("remove should not fail in css post plugin");
      if !found_charset {
        s.prepend(matched.as_str());
        found_charset = true;
      }
    }

    s.to_string()
  }

  pub async fn get_css_asset_dir_name(
    &self,
    css_asset_name: &str,
    css_file_names: &AssetFilenamesOutputOption,
  ) -> anyhow::Result<String> {
    match css_file_names {
      AssetFilenamesOutputOption::String(css_file_names) => {
        let assets_dir = if css_file_names.is_empty() {
          Path::new(&self.assets_dir)
        } else {
          Path::new(&css_file_names).parent().unwrap()
        };
        Ok(
          assets_dir
            .join(Path::new(css_asset_name).parent().unwrap())
            .to_slash_lossy()
            .into_owned(),
        )
      }
      AssetFilenamesOutputOption::Fn(css_file_names_fn) => {
        (css_file_names_fn)(&RollupPreRenderedAsset {
          names: vec![css_asset_name.into()],
          original_file_names: Vec::new(),
          source: "/* vite internal call, ignore */".to_owned().into(),
        })
        .await
      }
    }
  }

  pub fn get_css_bundle_name(&self, ctx: &PluginContext) -> anyhow::Result<String> {
    Ok(if let Some(css_asset_name) = ctx.meta().get::<CSSBundleName>() {
      css_asset_name.0.clone()
    } else {
      let css_bundle_name = if self.is_lib {
        if let Some(lib_css_filename) = &self.lib_css_filename {
          lib_css_filename.to_owned()
        } else {
          // TODO: Maybe we should use try_get_package_json_or_create for cache
          let mut base_dir = self.root.clone();
          loop {
            let pkg_path = base_dir.join("package.json");
            if pkg_path.is_file() {
              if let Some(name) = read_css_package_name_for_bundle(&self.root, &pkg_path)? {
                break format!("{name}.css");
              }
            }
            base_dir = match base_dir.parent() {
              Some(next) => next.to_path_buf(),
              None => {
                return Err(anyhow::anyhow!(
                  "Didn't find the nearest package.json when determining the library CSS bundle name.",
                ));
              }
            };
          }
        }
      } else {
        DEFAULT_CSS_BUNDLE_NAME.to_owned()
      };
      ctx.meta().insert(Arc::new(CSSBundleName(css_bundle_name.clone())));
      css_bundle_name
    })
  }

  pub async fn emit_non_codesplit_css_bundle(
    &self,
    ctx: &PluginContext,
    bundle: &[Output],
  ) -> anyhow::Result<()> {
    if !self.css_code_split && !self.has_emitted.load(Ordering::Relaxed) {
      fn collect(
        ctx: &PluginContext,
        chunk: &OutputChunk,
        bundle: &FxHashMap<ArcStr, Arc<OutputChunk>>,
        collected: &mut FxHashSet<ArcStr>,
        dynamic_imports: &mut FxIndexSet<ArcStr>,
        extracted_css: &mut String,
      ) {
        if collected.contains(&chunk.filename) {
          return;
        }
        collected.insert(chunk.filename.clone());
        // First collect all styles from the synchronous imports (lowest priority)
        chunk.imports.iter().for_each(|name| {
          if let Some(chunk) = bundle.get(name) {
            collect(ctx, chunk, bundle, collected, dynamic_imports, extracted_css);
          }
        });
        // Save dynamic imports in deterministic order to add the styles later (to have the highest priority)
        chunk.dynamic_imports.iter().for_each(|name| {
          dynamic_imports.insert(name.clone());
        });
        // Then collect the styles of the current chunk (might overwrite some styles from previous imports)
        if let Some(css_chunk) = ctx
          .meta()
          .get::<CSSChunkCache>()
          .expect("CSSChunkCache is missing")
          .inner
          .get(&Into::<ArcStr>::into(&chunk.preliminary_filename))
        {
          extracted_css.push_str(&css_chunk);
        }
      }

      let chunks = bundle
        .iter()
        .filter_map(|output| match output {
          Output::Chunk(chunk) => Some((chunk.filename.clone(), Arc::clone(chunk))),
          Output::Asset(_) => None,
        })
        .collect::<FxHashMap<_, _>>();

      let mut extracted_css = String::new();
      let mut collected = FxHashSet::default();
      let mut dynamic_imports = FxIndexSet::default();
      // The bundle is guaranteed to be deterministic, if not then we have a bug in rollup.
      // So we use it to ensure a deterministic order of styles
      for output in bundle {
        if let Output::Chunk(chunk) = output
          && chunk.is_entry
        {
          collect(ctx, chunk, &chunks, &mut collected, &mut dynamic_imports, &mut extracted_css);
        }
      }
      // Now collect the dynamic chunks, this is done last to have the styles overwrite the previous ones
      while let imports = std::mem::take(&mut dynamic_imports)
        && !imports.is_empty()
      {
        for name in imports {
          if let Some(chunk) = chunks.get(&name) {
            collect(ctx, chunk, &chunks, &mut collected, &mut dynamic_imports, &mut extracted_css);
          }
        }
      }

      if !extracted_css.is_empty() {
        self.has_emitted.store(true, Ordering::Relaxed);
        ctx
          .emit_file_async(rolldown_common::EmittedAsset {
            name: Some(self.get_css_bundle_name(ctx)?),
            // this file is an implicit entry point, use `style.css` as the original file name
            // this name is also used as a key in the manifest
            original_file_name: Some("style.css".to_owned()),
            source: self.finalize_css(extracted_css).await?.into(),
            ..Default::default()
          })
          .await?;
      }
    }
    Ok(())
  }

  pub fn prune_pure_css_chunks(
    &self,
    ctx: &PluginContext,
    args: &mut rolldown_plugin::HookGenerateBundleArgs<'_>,
  ) {
    if let Some(pure_css_chunks) = ctx.meta().get::<PureCSSChunks>()
      && !pure_css_chunks.inner.is_empty()
    {
      let mut pure_css_chunk_names = Vec::with_capacity(pure_css_chunks.inner.len());
      for output in args.bundle.iter() {
        if let Output::Chunk(chunk) = output
          && pure_css_chunks.inner.contains(chunk.preliminary_filename.as_str())
        {
          pure_css_chunk_names.push(chunk.filename.clone());
        }
      }

      // TODO: improve below regex logic
      let empty_chunk_re = LazyLock::new(|| {
        let empty_chunk_files = pure_css_chunk_names
          .iter()
          .filter_map(|file| {
            Path::new(file.as_str()).file_name().and_then(|v| v.to_str().map(regex::escape))
          })
          .collect::<Vec<_>>()
          .join("|");

        Regex::new(&if args.options.format.is_esm() {
          rolldown_utils::concat_string!(
            r#"\bimport\s*["'][^"']*(?:"#,
            empty_chunk_files,
            r#")["'];"#
          )
        } else {
          rolldown_utils::concat_string!(
            r#"(\b|,\s*)require\(\s*["'\`][^"'\`]*(?:"#,
            empty_chunk_files,
            r#")["'\`]\)(;|,)"#
          )
        })
        .unwrap()
      });
      let chunks = args
        .bundle
        .iter()
        .filter_map(|output| match output {
          Output::Chunk(chunk) => Some((chunk.filename.clone(), Arc::clone(chunk))),
          Output::Asset(_) => None,
        })
        .collect::<FxHashMap<_, _>>();
      for output in args.bundle.iter_mut() {
        if let Output::Chunk(chunk) = output {
          let mut chunk_imports_pure_css_chunk = false;
          let mut new_chunk = (**chunk).clone();
          // remove pure css chunk from other chunk's imports, and also
          // register the emitted CSS files under the importer chunks instead.
          let vite_metadata = ctx.meta().get_or_insert_default::<ViteMetadata>();
          let chunk_metadata = vite_metadata.get(chunk.preliminary_filename.as_str().into());
          new_chunk.imports = new_chunk
            .imports
            .into_iter()
            .filter(|file| {
              if pure_css_chunk_names.contains(file) {
                let chunk = &chunks[file];
                let file_metadata = vite_metadata.get(chunk.preliminary_filename.as_str().into());
                file_metadata.imported_css.iter().for_each(|file| {
                  chunk_metadata.imported_css.insert(file.clone());
                });
                file_metadata.imported_assets.iter().for_each(|file| {
                  chunk_metadata.imported_assets.insert(file.clone());
                });
                chunk_imports_pure_css_chunk = true;
                return false;
              }
              true
            })
            .collect::<Vec<_>>();
          if chunk_imports_pure_css_chunk {
            new_chunk.code = empty_chunk_re
              .replace_all(&chunk.code, |captures: &regex::Captures<'_>| {
                let len = captures.get(0).unwrap().len();
                if args.options.format.is_esm() {
                  return format!("/* empty css {:<width$}*/", "", width = len.saturating_sub(15));
                }
                if let Some(p2) = captures.get(2)
                  && p2.as_str() == ";"
                {
                  return format!(";/* empty css {:<width$}*/", "", width = len.saturating_sub(16));
                }
                let p1 = captures.get(1).map_or("", |m| m.as_str());
                format!(
                  "{p1}/* empty css {:<width$}*/",
                  "",
                  width = len.saturating_sub(15 + p1.len())
                )
              })
              .into_owned();
          }
          *chunk = Arc::new(new_chunk);
        }
      }

      args.bundle.retain(|output| !match output {
        Output::Chunk(chunk) => {
          let is_pure_css_chunk = pure_css_chunk_names.contains(&chunk.filename);
          if is_pure_css_chunk {
            ctx
              .meta()
              .get::<RemovedPureCSSFilesCache>()
              .expect("RemovedPureCSSFilesCache missing")
              .inner
              .insert(chunk.filename.clone(), Arc::<OutputChunk>::clone(chunk));
          }
          is_pure_css_chunk
        }
        Output::Asset(asset) => pure_css_chunk_names
          .contains(&rolldown_utils::concat_string!(asset.filename, ".map").into()),
      });
    }
  }
}
