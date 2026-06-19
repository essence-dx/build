use std::path::{Path, PathBuf};

use arcstr::ArcStr;
use oxc_resolver::PackageType;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, de::IgnoredAny};

use crate::side_effects::{SideEffects, glob_match_with_normalized_pattern};

pub const PACKAGE_JSON_MACHINE_MAGIC: &[u8; 8] = b"RDXPKG01";
pub const PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC: &[u8; 8] = b"RDXOPD01";
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
pub const PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC: &[u8; 8] = b"RDXOPDRK";
const PACKAGE_JSON_MACHINE_HEADER_LEN: usize = 12;
const PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_HEADER_LEN: usize = 12;
#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
const PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN: usize = 16;
const NONE_STRING_LEN: u32 = u32::MAX;

#[cfg(test)]
thread_local! {
  static PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_TEST_ARCSTR_MATERIALIZATIONS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_TEST_COUNTED_STRING_VALIDATIONS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static PACKAGE_JSON_TEST_U32_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  static PACKAGE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone)]
pub struct PackageJson {
  name: Option<ArcStr>,
  version: Option<ArcStr>,
  pub r#type: Option<&'static str>,
  pub side_effects: Option<SideEffects>,
  realpath: PathBuf,
}

impl PackageJson {
  pub fn from_oxc_pkg_json(oxc_pkg_json: &oxc_resolver::PackageJson) -> Self {
    Self {
      name: oxc_pkg_json.name().map(ArcStr::from),
      version: oxc_pkg_json.version().map(ArcStr::from),
      r#type: oxc_pkg_json.r#type().map(|t| match t {
        PackageType::CommonJs => "commonjs",
        PackageType::Module => "module",
      }),
      side_effects: oxc_pkg_json.side_effects().as_ref().map(SideEffects::from_resolver),
      realpath: oxc_pkg_json.realpath.clone(),
    }
  }

  pub fn from_cache_parts(
    name: Option<String>,
    version: Option<String>,
    package_type: Option<String>,
    side_effects: Option<SideEffects>,
    realpath: PathBuf,
  ) -> Option<Self> {
    let r#type = match package_type.as_deref() {
      Some("commonjs") => Some("commonjs"),
      Some("module") => Some("module"),
      Some(_) => return None,
      None => None,
    };

    Some(Self {
      name: name.map(ArcStr::from),
      version: version.map(ArcStr::from),
      r#type,
      side_effects,
      realpath,
    })
  }

  fn from_machine_parts(
    name: Option<ArcStr>,
    version: Option<ArcStr>,
    package_type: Option<&'static str>,
    side_effects: Option<SideEffects>,
    realpath: PathBuf,
  ) -> Self {
    Self { name, version, r#type: package_type, side_effects, realpath }
  }

  /// Realpath to `package.json`. Contains the `package.json` filename.
  pub fn realpath(&self) -> &Path {
    &self.realpath
  }

  pub fn name(&self) -> Option<&str> {
    self.name.as_deref()
  }

  pub fn version(&self) -> Option<&str> {
    self.version.as_deref()
  }

  pub fn r#type(&self) -> Option<&str> {
    self.r#type
  }

  /// * `module_path`: relative path to the module from `package.json` path
  pub fn check_side_effects_for(&self, module_path: &str) -> Option<bool> {
    let side_effects = self.side_effects.as_ref()?;
    // Is it necessary to convert module_path to relative path?
    match side_effects {
      SideEffects::Bool(s) => Some(*s),
      SideEffects::String(p) => Some(glob_match_with_normalized_pattern(p.as_str(), module_path)),
      SideEffects::Array(pats) => {
        Some(pats.iter().any(|p| glob_match_with_normalized_pattern(p.as_str(), module_path)))
      }
    }
  }
}

pub fn encode_package_json_machine_payload(package_json: &PackageJson) -> Option<Vec<u8>> {
  let mut machine_bytes = Vec::with_capacity(package_json_machine_payload_capacity(package_json)?);
  machine_bytes.extend_from_slice(PACKAGE_JSON_MACHINE_MAGIC);
  machine_bytes.push(package_type_tag(package_json.r#type)?);
  machine_bytes.push(side_effects_tag(package_json.side_effects.as_ref()));
  machine_bytes.extend_from_slice(&[0, 0]);
  write_optional_string(&mut machine_bytes, package_json.name())?;
  write_optional_string(&mut machine_bytes, package_json.version())?;

  match package_json.side_effects.as_ref() {
    Some(SideEffects::String(value)) => write_string(&mut machine_bytes, value)?,
    Some(SideEffects::Array(values)) => {
      let len = u32::try_from(values.len()).ok()?;
      machine_bytes.extend_from_slice(&len.to_le_bytes());
      for value in values {
        write_string(&mut machine_bytes, value)?;
      }
    }
    Some(SideEffects::Bool(_)) | None => {}
  }

  Some(machine_bytes)
}

fn package_json_machine_payload_capacity(package_json: &PackageJson) -> Option<usize> {
  let mut capacity = PACKAGE_JSON_MACHINE_HEADER_LEN;
  capacity = capacity.checked_add(encoded_optional_string_len(package_json.name())?)?;
  capacity = capacity.checked_add(encoded_optional_string_len(package_json.version())?)?;

  match package_json.side_effects.as_ref() {
    Some(SideEffects::String(value)) => {
      capacity = capacity.checked_add(encoded_string_len(value)?)?;
    }
    Some(SideEffects::Array(values)) => {
      u32::try_from(values.len()).ok()?;
      capacity = capacity.checked_add(4)?;
      for value in values {
        capacity = capacity.checked_add(encoded_string_len(value)?)?;
      }
    }
    Some(SideEffects::Bool(_)) | None => {}
  }

  Some(capacity)
}

pub fn decode_package_json_machine_payload(
  machine_bytes: &[u8],
  realpath: PathBuf,
) -> Option<PackageJson> {
  if machine_bytes.len() < PACKAGE_JSON_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..PACKAGE_JSON_MACHINE_MAGIC.len()] != PACKAGE_JSON_MACHINE_MAGIC {
    return None;
  }
  if machine_bytes[10] != 0 || machine_bytes[11] != 0 {
    return None;
  }

  let package_type = package_type_from_machine_tag(machine_bytes[8])?;
  let side_effects_tag = machine_bytes[9];
  if !is_package_json_machine_side_effects_tag(side_effects_tag) {
    return None;
  }

  if !package_json_machine_payload_layout_is_valid(machine_bytes, side_effects_tag) {
    return None;
  }

  let mut cursor = PACKAGE_JSON_MACHINE_HEADER_LEN;
  let name = read_optional_arcstr(machine_bytes, &mut cursor)?;
  let version = read_optional_arcstr(machine_bytes, &mut cursor)?;
  let side_effects = match side_effects_tag {
    0 => None,
    1 => Some(SideEffects::Bool(false)),
    2 => Some(SideEffects::Bool(true)),
    3 => Some(SideEffects::String(read_string(machine_bytes, &mut cursor)?)),
    4 => {
      let count = read_u32(machine_bytes, &mut cursor)? as usize;
      let mut values = Vec::with_capacity(count);
      for _ in 0..count {
        values.push(read_string(machine_bytes, &mut cursor)?);
      }
      Some(SideEffects::Array(values))
    }
    _ => return None,
  };
  if cursor != machine_bytes.len() {
    return None;
  }

  Some(PackageJson::from_machine_parts(name, version, package_type, side_effects, realpath))
}

fn package_json_machine_payload_layout_is_valid(
  machine_bytes: &[u8],
  side_effects_tag: u8,
) -> bool {
  let mut cursor = PACKAGE_JSON_MACHINE_HEADER_LEN;
  if skip_optional_string(machine_bytes, &mut cursor).is_none()
    || skip_optional_string(machine_bytes, &mut cursor).is_none()
  {
    return false;
  }

  match side_effects_tag {
    0..=2 => cursor == machine_bytes.len(),
    3 => skip_string(machine_bytes, &mut cursor).is_some() && cursor == machine_bytes.len(),
    4 => {
      let Some(count) = read_u32(machine_bytes, &mut cursor) else {
        return false;
      };
      validated_counted_strings_end(machine_bytes, cursor, count as usize)
        .is_some_and(|end| end == machine_bytes.len())
    }
    _ => false,
  }
}

fn package_type_tag(package_type: Option<&str>) -> Option<u8> {
  match package_type {
    None => Some(0),
    Some("commonjs") => Some(1),
    Some("module") => Some(2),
    Some(_) => None,
  }
}

fn package_type_from_machine_tag(tag: u8) -> Option<Option<&'static str>> {
  match tag {
    0 => Some(None),
    1 => Some(Some("commonjs")),
    2 => Some(Some("module")),
    _ => None,
  }
}

fn is_package_json_machine_side_effects_tag(tag: u8) -> bool {
  matches!(tag, 0..=4)
}

fn side_effects_tag(side_effects: Option<&SideEffects>) -> u8 {
  match side_effects {
    None => 0,
    Some(SideEffects::Bool(false)) => 1,
    Some(SideEffects::Bool(true)) => 2,
    Some(SideEffects::String(_)) => 3,
    Some(SideEffects::Array(_)) => 4,
  }
}

fn write_optional_string(machine_bytes: &mut Vec<u8>, value: Option<&str>) -> Option<()> {
  match value {
    Some(value) => write_string(machine_bytes, value),
    None => {
      machine_bytes.extend_from_slice(&NONE_STRING_LEN.to_le_bytes());
      Some(())
    }
  }
}

fn write_string(machine_bytes: &mut Vec<u8>, value: &str) -> Option<()> {
  let len = u32::try_from(value.len()).ok()?;
  if len == NONE_STRING_LEN {
    return None;
  }
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  machine_bytes.extend_from_slice(value.as_bytes());
  Some(())
}

fn encoded_optional_string_len(value: Option<&str>) -> Option<usize> {
  match value {
    Some(value) => encoded_string_len(value),
    None => Some(4),
  }
}

fn encoded_string_len(value: &str) -> Option<usize> {
  let len = u32::try_from(value.len()).ok()?;
  if len == NONE_STRING_LEN {
    return None;
  }
  4_usize.checked_add(value.len())
}

fn read_optional_arcstr(machine_bytes: &[u8], cursor: &mut usize) -> Option<Option<ArcStr>> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == NONE_STRING_LEN {
    return Some(None);
  }
  read_arcstr_with_len(machine_bytes, cursor, len).map(Some)
}

fn skip_optional_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == NONE_STRING_LEN {
    return Some(());
  }
  skip_string_with_len(machine_bytes, cursor, len)
}

fn read_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<String> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == NONE_STRING_LEN {
    return None;
  }
  read_string_with_len(machine_bytes, cursor, len)
}

fn skip_string(machine_bytes: &[u8], cursor: &mut usize) -> Option<()> {
  let len = read_u32(machine_bytes, cursor)?;
  if len == NONE_STRING_LEN {
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
  PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS.with(|calls| calls.set(calls.get() + 1));
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

fn read_arcstr_with_len(machine_bytes: &[u8], cursor: &mut usize, len: u32) -> Option<ArcStr> {
  let len = usize::try_from(len).ok()?;
  let end = cursor.checked_add(len)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  *cursor = end;
  let value = std::str::from_utf8(bytes).ok()?;
  #[cfg(test)]
  PACKAGE_JSON_TEST_ARCSTR_MATERIALIZATIONS.with(|calls| calls.set(calls.get() + 1));
  Some(ArcStr::from(value))
}

fn read_u32(machine_bytes: &[u8], cursor: &mut usize) -> Option<u32> {
  #[cfg(test)]
  PACKAGE_JSON_TEST_U32_READS.with(|calls| calls.set(calls.get() + 1));

  let end = cursor.checked_add(4)?;
  let bytes = machine_bytes.get(*cursor..end)?;
  *cursor = end;
  Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn declared_string_count_fits_remaining(machine_bytes: &[u8], cursor: usize, count: usize) -> bool {
  count <= machine_bytes.len().saturating_sub(cursor) / 4
}

#[cfg(test)]
fn declared_counted_strings_fit_remaining(
  machine_bytes: &[u8],
  cursor: usize,
  count: usize,
) -> bool {
  validated_counted_strings_end(machine_bytes, cursor, count).is_some()
}

fn validated_counted_strings_end(
  machine_bytes: &[u8],
  cursor: usize,
  count: usize,
) -> Option<usize> {
  #[cfg(test)]
  PACKAGE_JSON_TEST_COUNTED_STRING_VALIDATIONS.with(|calls| calls.set(calls.get() + 1));

  if !declared_string_count_fits_remaining(machine_bytes, cursor, count) {
    return None;
  }

  let mut cursor = cursor;
  for _ in 0..count {
    let len = read_u32(machine_bytes, &mut cursor)?;
    if len == NONE_STRING_LEN {
      return None;
    }
    skip_string_with_len(machine_bytes, &mut cursor, len)?;
  }

  Some(cursor)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PackageJsonOptionalPeerDependencies {
  pub name: String,
  pub optional_peer_dependencies: FxHashSet<String>,
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(compare(PartialEq))]
struct PackageJsonOptionalPeerDependenciesDxSerializerPayload {
  name: String,
  optional_peer_dependencies: Vec<String>,
}

pub fn encode_package_json_optional_peer_dependencies_machine_payload(
  package_json: &PackageJsonOptionalPeerDependencies,
) -> Option<Vec<u8>> {
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  {
    return encode_package_json_optional_peer_dependencies_dx_serializer_machine_payload(
      package_json,
    );
  }

  #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
  {
    return encode_package_json_optional_peer_dependencies_legacy_machine_payload(package_json);
  }
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn encode_package_json_optional_peer_dependencies_dx_serializer_machine_payload(
  package_json: &PackageJsonOptionalPeerDependencies,
) -> Option<Vec<u8>> {
  let (optional_peer_dependencies, _) =
    sorted_optional_peer_dependencies_with_capacity(package_json)?;
  let payload = PackageJsonOptionalPeerDependenciesDxSerializerPayload {
    name: package_json.name.clone(),
    optional_peer_dependencies: optional_peer_dependencies
      .into_iter()
      .map(ToString::to_string)
      .collect(),
  };
  let body = serializer::machine::api::serialize(&payload).ok()?;
  let body_len = u32::try_from(body.len()).ok()?;
  let mut machine_bytes = Vec::with_capacity(
    PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN + body.len(),
  );
  machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
  machine_bytes.extend_from_slice(&body_len.to_le_bytes());
  machine_bytes.extend_from_slice(body.as_ref());
  Some(machine_bytes)
}

#[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
fn encode_package_json_optional_peer_dependencies_legacy_machine_payload(
  package_json: &PackageJsonOptionalPeerDependencies,
) -> Option<Vec<u8>> {
  let (optional_peer_dependencies, capacity) =
    sorted_optional_peer_dependencies_with_capacity(package_json)?;
  let mut machine_bytes = Vec::with_capacity(capacity);
  machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC);
  machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
  write_string(&mut machine_bytes, &package_json.name)?;
  let len = u32::try_from(optional_peer_dependencies.len()).ok()?;
  machine_bytes.extend_from_slice(&len.to_le_bytes());
  for dependency in optional_peer_dependencies {
    write_string(&mut machine_bytes, dependency)?;
  }
  Some(machine_bytes)
}

#[cfg(all(test, not(all(feature = "dx-serializer-local", not(target_family = "wasm")))))]
fn package_json_optional_peer_dependencies_machine_payload_capacity(
  package_json: &PackageJsonOptionalPeerDependencies,
) -> Option<usize> {
  sorted_optional_peer_dependencies_with_capacity(package_json).map(|(_, capacity)| capacity)
}

fn sorted_optional_peer_dependencies_with_capacity(
  package_json: &PackageJsonOptionalPeerDependencies,
) -> Option<(Vec<&str>, usize)> {
  let mut capacity = PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_HEADER_LEN;
  capacity = capacity.checked_add(encoded_string_len(&package_json.name)?)?;
  capacity = capacity.checked_add(4)?;
  u32::try_from(package_json.optional_peer_dependencies.len()).ok()?;
  let mut optional_peer_dependencies =
    Vec::with_capacity(package_json.optional_peer_dependencies.len());
  for dependency in &package_json.optional_peer_dependencies {
    capacity = capacity.checked_add(encoded_string_len(dependency)?)?;
    optional_peer_dependencies.push(dependency.as_str());
  }
  optional_peer_dependencies.sort_unstable();
  Some((optional_peer_dependencies, capacity))
}

pub fn decode_package_json_optional_peer_dependencies_machine_payload(
  machine_bytes: &[u8],
) -> Option<PackageJsonOptionalPeerDependencies> {
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  if machine_bytes.starts_with(PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC) {
    return decode_package_json_optional_peer_dependencies_dx_serializer_machine_payload(
      machine_bytes,
    );
  }

  decode_package_json_optional_peer_dependencies_legacy_machine_payload(machine_bytes)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn decode_package_json_optional_peer_dependencies_dx_serializer_machine_payload(
  machine_bytes: &[u8],
) -> Option<PackageJsonOptionalPeerDependencies> {
  if machine_bytes.len() < PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC.len()]
    != PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC
  {
    return None;
  }
  if machine_bytes[8..12] != [0, 0, 0, 0] {
    return None;
  }

  let len = u32::from_le_bytes(machine_bytes[12..16].try_into().ok()?);
  let len = usize::try_from(len).ok()?;
  let end = PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN.checked_add(len)?;
  let body =
    machine_bytes.get(PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN..end)?;
  if end != machine_bytes.len() {
    return None;
  }

  Some(package_json_optional_peer_dependencies_dx_serializer_archived_payload(body, |archived| {
    let mut optional_peer_dependencies = FxHashSet::default();
    optional_peer_dependencies.reserve(archived.optional_peer_dependencies.len());
    for dependency in archived.optional_peer_dependencies.iter() {
      optional_peer_dependencies.insert(dependency.as_str().to_string());
    }

    PackageJsonOptionalPeerDependencies {
      name: archived.name.as_str().to_string(),
      optional_peer_dependencies,
    }
  }))
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn package_json_optional_peer_dependencies_dx_serializer_archived_payload<R>(
  body: &[u8],
  decode: impl FnOnce(
    &<PackageJsonOptionalPeerDependenciesDxSerializerPayload as rkyv::Archive>::Archived,
  ) -> R,
) -> R {
  if package_json_optional_peer_dependencies_dx_serializer_body_is_aligned(body) {
    // SAFETY: The cache metadata layer validates source and machine hashes before
    // this decoder runs. Aligned bodies can be read directly through RKYV.
    let archived = unsafe {
      serializer::machine::api::deserialize::<PackageJsonOptionalPeerDependenciesDxSerializerPayload>(
        body,
      )
    };
    return decode(archived);
  }

  #[cfg(test)]
  PACKAGE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(calls.get() + 1));

  let mut aligned: rkyv::util::AlignedVec<16> = rkyv::util::AlignedVec::with_capacity(body.len());
  aligned.extend_from_slice(body);
  // SAFETY: The cache metadata layer validates source and machine hashes before
  // this decoder runs, and the RKYV body is copied into aligned storage first.
  let archived = unsafe {
    serializer::machine::api::deserialize::<PackageJsonOptionalPeerDependenciesDxSerializerPayload>(
      &aligned,
    )
  };
  decode(archived)
}

#[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
fn package_json_optional_peer_dependencies_dx_serializer_body_is_aligned(body: &[u8]) -> bool {
  let alignment = std::mem::align_of::<
    <PackageJsonOptionalPeerDependenciesDxSerializerPayload as rkyv::Archive>::Archived,
  >();
  body.as_ptr() as usize % alignment == 0
}

fn decode_package_json_optional_peer_dependencies_legacy_machine_payload(
  machine_bytes: &[u8],
) -> Option<PackageJsonOptionalPeerDependencies> {
  if machine_bytes.len() < PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_HEADER_LEN {
    return None;
  }
  if &machine_bytes[..PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC.len()]
    != PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC
  {
    return None;
  }
  if machine_bytes[8..12] != [0, 0, 0, 0] {
    return None;
  }

  let mut cursor = PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_HEADER_LEN;
  let mut validation_cursor = cursor;
  skip_string(machine_bytes, &mut validation_cursor)?;
  let count = read_u32(machine_bytes, &mut validation_cursor)? as usize;
  let optional_peer_dependencies_cursor = validation_cursor;
  if validated_counted_strings_end(machine_bytes, validation_cursor, count)? != machine_bytes.len()
  {
    return None;
  }

  let name = read_string(machine_bytes, &mut cursor)?;
  cursor = optional_peer_dependencies_cursor;
  let mut optional_peer_dependencies = FxHashSet::default();
  optional_peer_dependencies.reserve(count);
  for _ in 0..count {
    optional_peer_dependencies.insert(read_string(machine_bytes, &mut cursor)?);
  }
  if cursor != machine_bytes.len() {
    return None;
  }

  Some(PackageJsonOptionalPeerDependencies { name, optional_peer_dependencies })
}

pub fn parse_package_json_optional_peer_dependencies(
  source_bytes: &[u8],
) -> PackageJsonOptionalPeerDependencies {
  let Ok(package_json) = serde_json::from_slice::<PackageJsonWithPeerDependenciesRaw>(
    package_json_source_without_utf8_bom(source_bytes),
  ) else {
    return Default::default();
  };

  package_json.into_optional_peer_dependencies()
}

fn package_json_source_without_utf8_bom(source_bytes: &[u8]) -> &[u8] {
  source_bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source_bytes)
}

#[derive(Deserialize)]
struct PackageJsonWithPeerDependenciesRaw {
  pub name: String,
  #[serde(rename = "peerDependencies")]
  pub peer_dependencies: Option<FxHashMap<String, IgnoredAny>>,
  #[serde(rename = "peerDependenciesMeta")]
  pub peer_dependencies_meta: Option<FxHashMap<String, PackageJsonPeerDependenciesMetaRaw>>,
}

impl PackageJsonWithPeerDependenciesRaw {
  fn into_optional_peer_dependencies(self) -> PackageJsonOptionalPeerDependencies {
    let (Some(peer_dependencies), Some(peer_dependencies_meta)) =
      (self.peer_dependencies, self.peer_dependencies_meta)
    else {
      return PackageJsonOptionalPeerDependencies {
        name: self.name,
        optional_peer_dependencies: FxHashSet::default(),
      };
    };

    PackageJsonOptionalPeerDependencies {
      name: self.name,
      optional_peer_dependencies: optional_peer_dependencies_from_maps(
        peer_dependencies,
        &peer_dependencies_meta,
      ),
    }
  }
}

#[derive(Deserialize)]
struct PackageJsonPeerDependenciesMetaRaw {
  pub optional: bool,
}

fn optional_peer_dependencies_from_maps(
  peer_dependencies: FxHashMap<String, IgnoredAny>,
  peer_dependencies_meta: &FxHashMap<String, PackageJsonPeerDependenciesMetaRaw>,
) -> FxHashSet<String> {
  let mut optional_peer_dependencies = FxHashSet::default();
  optional_peer_dependencies.reserve(peer_dependencies.len().min(peer_dependencies_meta.len()));
  for dependency in peer_dependencies.into_keys() {
    if peer_dependencies_meta.get(&dependency).is_some_and(|meta| meta.optional) {
      optional_peer_dependencies.insert(dependency);
    }
  }
  optional_peer_dependencies
}

#[cfg(test)]
mod tests {
  use super::*;

  fn reset_string_materialization_count() {
    PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS.with(|calls| calls.set(0));
  }

  fn string_materialization_count() -> u64 {
    PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS.with(|calls| calls.get())
  }

  fn reset_package_json_materialization_counts() {
    PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS.with(|calls| calls.set(0));
    PACKAGE_JSON_TEST_ARCSTR_MATERIALIZATIONS.with(|calls| calls.set(0));
  }

  fn package_json_materialization_count() -> u64 {
    PACKAGE_JSON_TEST_STRING_MATERIALIZATIONS.with(|calls| calls.get())
      + PACKAGE_JSON_TEST_ARCSTR_MATERIALIZATIONS.with(|calls| calls.get())
  }

  fn reset_counted_string_validation_count() {
    PACKAGE_JSON_TEST_COUNTED_STRING_VALIDATIONS.with(|calls| calls.set(0));
  }

  fn counted_string_validation_count() -> u64 {
    PACKAGE_JSON_TEST_COUNTED_STRING_VALIDATIONS.with(|calls| calls.get())
  }

  fn reset_u32_read_count() {
    PACKAGE_JSON_TEST_U32_READS.with(|calls| calls.set(0));
  }

  fn u32_read_count() -> u64 {
    PACKAGE_JSON_TEST_U32_READS.with(|calls| calls.get())
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn reset_dx_serializer_aligned_copy_count() {
    PACKAGE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.set(0));
  }

  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn dx_serializer_aligned_copy_count() -> u64 {
    PACKAGE_JSON_TEST_DX_SERIALIZER_ALIGNED_COPY_CALLS.with(|calls| calls.get())
  }

  fn package_json_optional_peer_dependencies_for_test(
    name: &str,
    dependencies: impl IntoIterator<Item = &'static str>,
  ) -> PackageJsonOptionalPeerDependencies {
    let mut optional_peer_dependencies = FxHashSet::default();
    for dependency in dependencies {
      optional_peer_dependencies.insert(dependency.to_string());
    }
    PackageJsonOptionalPeerDependencies { name: name.to_string(), optional_peer_dependencies }
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn optional_peer_dependencies_dx_serializer_body_decodes_without_aligned_copy() {
    let package_json =
      package_json_optional_peer_dependencies_for_test("pkg", ["react", "solid", "vue"]);
    let machine_bytes =
      encode_package_json_optional_peer_dependencies_machine_payload(&package_json).unwrap();
    let body = &machine_bytes[PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN..];

    assert!(package_json_optional_peer_dependencies_dx_serializer_body_is_aligned(body));

    reset_dx_serializer_aligned_copy_count();
    assert_eq!(
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes),
      Some(package_json)
    );
    assert_eq!(dx_serializer_aligned_copy_count(), 0);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn optional_peer_dependencies_dx_serializer_body_decodes_from_unaligned_slice_with_one_copy() {
    let package_json =
      package_json_optional_peer_dependencies_for_test("pkg", ["react", "solid", "vue"]);
    let machine_bytes =
      encode_package_json_optional_peer_dependencies_machine_payload(&package_json).unwrap();
    let mut unaligned_storage = Vec::with_capacity(machine_bytes.len() + 1);
    unaligned_storage.push(0);
    unaligned_storage.extend_from_slice(&machine_bytes);
    let unaligned_machine_bytes = &unaligned_storage[1..];
    let body =
      &unaligned_machine_bytes[PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_HEADER_LEN..];

    assert!(!package_json_optional_peer_dependencies_dx_serializer_body_is_aligned(body));

    reset_dx_serializer_aligned_copy_count();
    assert_eq!(
      decode_package_json_optional_peer_dependencies_machine_payload(unaligned_machine_bytes),
      Some(package_json)
    );
    assert_eq!(dx_serializer_aligned_copy_count(), 1);
  }

  #[test]
  #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
  fn optional_peer_dependencies_machine_payload_uses_dx_serializer_rkyv_when_enabled() {
    let package_json =
      package_json_optional_peer_dependencies_for_test("pkg", ["react", "solid", "vue"]);
    let machine_bytes =
      encode_package_json_optional_peer_dependencies_machine_payload(&package_json).unwrap();

    assert!(machine_bytes.starts_with(PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC));
    assert_eq!(
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes),
      Some(package_json)
    );
  }

  #[test]
  fn package_json_from_cache_parts_rebuilds_public_behavior() {
    let package_json = PackageJson::from_cache_parts(
      Some("pkg".to_string()),
      Some("1.0.0".to_string()),
      Some("module".to_string()),
      Some(SideEffects::Array(vec!["./style.css".to_string()])),
      PathBuf::from(r"G:\Dx\build\node_modules\pkg\package.json"),
    )
    .unwrap();

    assert_eq!(package_json.name(), Some("pkg"));
    assert_eq!(package_json.version(), Some("1.0.0"));
    assert_eq!(package_json.r#type(), Some("module"));
    assert_eq!(package_json.check_side_effects_for("style.css"), Some(true));
    assert_eq!(package_json.check_side_effects_for("index.js"), Some(false));
  }

  #[test]
  fn package_json_from_cache_parts_rejects_unknown_package_type() {
    assert!(
      PackageJson::from_cache_parts(
        None,
        None,
        Some("future-type".to_string()),
        None,
        PathBuf::from("package.json"),
      )
      .is_none()
    );
  }

  #[test]
  fn package_json_machine_payload_round_trips_compact_binary() {
    let realpath = PathBuf::from(r"G:\Dx\build\node_modules\pkg\package.json");
    let package_json = PackageJson::from_cache_parts(
      Some("pkg".to_string()),
      Some("1.0.0".to_string()),
      Some("module".to_string()),
      Some(SideEffects::Array(vec!["./style.css".to_string(), "./theme.css".to_string()])),
      realpath.clone(),
    )
    .unwrap();

    let machine_bytes = encode_package_json_machine_payload(&package_json).unwrap();

    assert!(machine_bytes.starts_with(PACKAGE_JSON_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));
    assert_eq!(machine_bytes.len(), package_json_machine_payload_capacity(&package_json).unwrap());

    let restored = decode_package_json_machine_payload(&machine_bytes, realpath).unwrap();
    assert_eq!(restored.name(), Some("pkg"));
    assert_eq!(restored.version(), Some("1.0.0"));
    assert_eq!(restored.r#type(), Some("module"));
    assert_eq!(restored.check_side_effects_for("style.css"), Some(true));
    assert_eq!(restored.check_side_effects_for("theme.css"), Some(true));
    assert_eq!(restored.check_side_effects_for("index.js"), Some(false));
  }

  #[test]
  fn package_json_machine_payload_validates_counted_strings_once_before_materialization() {
    let realpath = PathBuf::from(r"G:\Dx\build\node_modules\pkg\package.json");
    let package_json = PackageJson::from_cache_parts(
      Some("pkg".to_string()),
      Some("1.0.0".to_string()),
      Some("module".to_string()),
      Some(SideEffects::Array(vec!["./style.css".to_string(), "./theme.css".to_string()])),
      realpath.clone(),
    )
    .unwrap();
    let machine_bytes = encode_package_json_machine_payload(&package_json).unwrap();

    reset_counted_string_validation_count();
    let restored = decode_package_json_machine_payload(&machine_bytes, realpath).unwrap();

    assert_eq!(restored.check_side_effects_for("style.css"), Some(true));
    assert_eq!(counted_string_validation_count(), 1);
  }

  #[test]
  fn package_json_machine_package_type_tag_decodes_static_values() {
    assert_eq!(package_type_from_machine_tag(0), Some(None));
    assert_eq!(package_type_from_machine_tag(1), Some(Some("commonjs")));
    assert_eq!(package_type_from_machine_tag(2), Some(Some("module")));
    assert_eq!(package_type_from_machine_tag(3), None);
  }

  #[test]
  fn package_json_machine_rejects_unknown_side_effects_tag_before_reading_fields() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_MACHINE_MAGIC);
    machine_bytes.push(0);
    machine_bytes.push(99);
    machine_bytes.extend_from_slice(&[0, 0]);

    assert!(!is_package_json_machine_side_effects_tag(99));
    assert!(
      decode_package_json_machine_payload(&machine_bytes, PathBuf::from("package.json")).is_none()
    );
  }

  #[test]
  fn package_json_machine_name_version_reader_decodes_arcstr() {
    let mut machine_bytes = Vec::new();
    write_optional_string(&mut machine_bytes, Some("pkg")).unwrap();
    write_optional_string(&mut machine_bytes, Some("1.0.0")).unwrap();
    let mut cursor = 0;

    let name = read_optional_arcstr(&machine_bytes, &mut cursor).unwrap();
    let version = read_optional_arcstr(&machine_bytes, &mut cursor).unwrap();

    assert_eq!(name.as_deref(), Some("pkg"));
    assert_eq!(version.as_deref(), Some("1.0.0"));
    assert_eq!(cursor, machine_bytes.len());
  }

  #[test]
  fn package_json_machine_payload_rejects_impossible_string_count_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_MACHINE_MAGIC);
    machine_bytes.push(0);
    machine_bytes.push(4);
    machine_bytes.extend_from_slice(&[0, 0]);
    machine_bytes.extend_from_slice(&NONE_STRING_LEN.to_le_bytes());
    machine_bytes.extend_from_slice(&NONE_STRING_LEN.to_le_bytes());
    let count_cursor = machine_bytes.len();
    machine_bytes.extend_from_slice(&16_u32.to_le_bytes());

    assert!(!declared_string_count_fits_remaining(&machine_bytes, count_cursor + 4, 16));
    assert!(
      decode_package_json_machine_payload(&machine_bytes, PathBuf::from("package.json")).is_none()
    );
  }

  #[test]
  fn package_json_machine_payload_rejects_truncated_counted_string_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_MACHINE_MAGIC);
    machine_bytes.push(0);
    machine_bytes.push(4);
    machine_bytes.extend_from_slice(&[0, 0]);
    machine_bytes.extend_from_slice(&NONE_STRING_LEN.to_le_bytes());
    machine_bytes.extend_from_slice(&NONE_STRING_LEN.to_le_bytes());
    machine_bytes.extend_from_slice(&1_u32.to_le_bytes());
    let string_cursor = machine_bytes.len();
    machine_bytes.extend_from_slice(&8_u32.to_le_bytes());

    assert!(declared_string_count_fits_remaining(&machine_bytes, string_cursor, 1));
    assert!(!declared_counted_strings_fit_remaining(&machine_bytes, string_cursor, 1));
    assert!(
      decode_package_json_machine_payload(&machine_bytes, PathBuf::from("package.json")).is_none()
    );
  }

  #[test]
  fn package_json_machine_payload_rejects_trailing_bytes_before_materialization() {
    let realpath = PathBuf::from(r"G:\Dx\build\node_modules\pkg\package.json");
    let package_json = PackageJson::from_cache_parts(
      Some("pkg".to_string()),
      Some("1.0.0".to_string()),
      Some("module".to_string()),
      Some(SideEffects::Array(vec!["./style.css".to_string()])),
      realpath.clone(),
    )
    .unwrap();
    let mut machine_bytes = encode_package_json_machine_payload(&package_json).unwrap();
    machine_bytes.push(0);

    reset_package_json_materialization_counts();
    assert!(decode_package_json_machine_payload(&machine_bytes, realpath).is_none());
    assert_eq!(package_json_materialization_count(), 0);
  }

  #[test]
  fn package_json_optional_peer_deps_payload_rejects_impossible_count_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC);
    machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
    write_string(&mut machine_bytes, "pkg").unwrap();
    let count_cursor = machine_bytes.len();
    machine_bytes.extend_from_slice(&16_u32.to_le_bytes());

    assert!(!declared_string_count_fits_remaining(&machine_bytes, count_cursor + 4, 16));
    assert!(
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes).is_none()
    );
  }

  #[test]
  fn package_json_optional_peer_deps_payload_rejects_truncated_counted_string_before_reserve() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC);
    machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
    write_string(&mut machine_bytes, "pkg").unwrap();
    machine_bytes.extend_from_slice(&1_u32.to_le_bytes());
    let string_cursor = machine_bytes.len();
    machine_bytes.extend_from_slice(&8_u32.to_le_bytes());

    assert!(declared_string_count_fits_remaining(&machine_bytes, string_cursor, 1));
    assert!(!declared_counted_strings_fit_remaining(&machine_bytes, string_cursor, 1));
    assert!(
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes).is_none()
    );
  }

  #[test]
  fn package_json_optional_peer_deps_rejects_trailing_bytes_before_string_materialization() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC);
    machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
    write_string(&mut machine_bytes, "pkg").unwrap();
    machine_bytes.extend_from_slice(&1_u32.to_le_bytes());
    write_string(&mut machine_bytes, "react").unwrap();
    machine_bytes.push(0);

    reset_string_materialization_count();
    assert!(
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes).is_none()
    );
    assert_eq!(string_materialization_count(), 0);
  }

  #[test]
  fn package_json_optional_peer_deps_parse_optional_dependencies() {
    let parsed = parse_package_json_optional_peer_dependencies(
      br#"{
        "name": "pkg",
        "peerDependencies": {
          "react": "*",
          "vue": "*",
          "solid-js": "*"
        },
        "peerDependenciesMeta": {
          "react": { "optional": true },
          "vue": { "optional": false }
        }
      }"#,
    );

    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.contains("react"));
    assert!(!parsed.optional_peer_dependencies.contains("vue"));
    assert!(!parsed.optional_peer_dependencies.contains("solid-js"));
  }

  #[test]
  fn package_json_optional_peer_deps_machine_payload_round_trips_compact_binary() {
    let parsed = parse_package_json_optional_peer_dependencies(
      br#"{
        "name": "pkg",
        "peerDependencies": {
          "react": "*",
          "vue": "*"
        },
        "peerDependenciesMeta": {
          "react": { "optional": true },
          "vue": { "optional": true }
        }
      }"#,
    );

    let machine_bytes = encode_package_json_optional_peer_dependencies_machine_payload(&parsed)
      .expect("optional peer dependency payload should encode");

    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    assert!(machine_bytes.starts_with(PACKAGE_JSON_OPTIONAL_PEER_DEPS_DX_SERIALIZER_MACHINE_MAGIC));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert!(machine_bytes.starts_with(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC));
    assert_ne!(machine_bytes.first(), Some(&b'{'));
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert_eq!(
      machine_bytes.len(),
      package_json_optional_peer_dependencies_machine_payload_capacity(&parsed).unwrap()
    );

    let restored =
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes).unwrap();
    assert_eq!(restored.name, "pkg");
    assert!(restored.optional_peer_dependencies.contains("react"));
    assert!(restored.optional_peer_dependencies.contains("vue"));
  }

  #[test]
  fn package_json_optional_peer_deps_legacy_decode_reuses_validated_count() {
    let mut machine_bytes = Vec::new();
    machine_bytes.extend_from_slice(PACKAGE_JSON_OPTIONAL_PEER_DEPS_MACHINE_MAGIC);
    machine_bytes.extend_from_slice(&[0, 0, 0, 0]);
    write_string(&mut machine_bytes, "pkg").unwrap();
    machine_bytes.extend_from_slice(&2_u32.to_le_bytes());
    write_string(&mut machine_bytes, "react").unwrap();
    write_string(&mut machine_bytes, "vue").unwrap();

    reset_counted_string_validation_count();
    reset_u32_read_count();
    let restored =
      decode_package_json_optional_peer_dependencies_machine_payload(&machine_bytes).unwrap();

    assert_eq!(restored.name, "pkg");
    assert!(restored.optional_peer_dependencies.contains("react"));
    assert!(restored.optional_peer_dependencies.contains("vue"));
    assert_eq!(counted_string_validation_count(), 1);
    assert_eq!(u32_read_count(), 7);
  }

  #[test]
  fn package_json_optional_peer_deps_sorted_refs_and_capacity_are_built_together() {
    let mut optional_peer_dependencies = FxHashSet::default();
    optional_peer_dependencies.insert("vue".to_string());
    optional_peer_dependencies.insert("react".to_string());
    let parsed =
      PackageJsonOptionalPeerDependencies { name: "pkg".to_string(), optional_peer_dependencies };

    let (dependencies, capacity) = sorted_optional_peer_dependencies_with_capacity(&parsed)
      .expect("optional peer dependency payload plan should encode");
    #[cfg(all(feature = "dx-serializer-local", not(target_family = "wasm")))]
    let _ = capacity;
    let machine_bytes = encode_package_json_optional_peer_dependencies_machine_payload(&parsed)
      .expect("optional peer dependency payload should encode");

    assert_eq!(dependencies, vec!["react", "vue"]);
    #[cfg(not(all(feature = "dx-serializer-local", not(target_family = "wasm"))))]
    assert_eq!(capacity, machine_bytes.len());
    assert_eq!(machine_bytes.capacity(), machine_bytes.len());
  }

  #[test]
  fn package_json_optional_peer_deps_machine_payload_is_stable_across_set_order() {
    fn parsed_with_deps(dependencies: &[&str]) -> PackageJsonOptionalPeerDependencies {
      let mut optional_peer_dependencies = FxHashSet::default();
      for dependency in dependencies {
        optional_peer_dependencies.insert((*dependency).to_string());
      }
      PackageJsonOptionalPeerDependencies { name: "pkg".to_string(), optional_peer_dependencies }
    }

    let first =
      encode_package_json_optional_peer_dependencies_machine_payload(&parsed_with_deps(&[
        "vue",
        "@scope/pkg",
        "react",
      ]))
      .expect("optional peer dependency payload should encode");
    let second =
      encode_package_json_optional_peer_dependencies_machine_payload(&parsed_with_deps(&[
        "react",
        "vue",
        "@scope/pkg",
      ]))
      .expect("optional peer dependency payload should encode");

    assert_eq!(first, second);
    let restored = decode_package_json_optional_peer_dependencies_machine_payload(&first)
      .expect("encoded payload should decode");
    assert!(restored.optional_peer_dependencies.contains("@scope/pkg"));
    assert!(restored.optional_peer_dependencies.contains("react"));
    assert!(restored.optional_peer_dependencies.contains("vue"));
  }

  #[test]
  fn package_json_optional_peer_deps_reserves_candidate_set_from_raw_maps() {
    let mut peer_dependencies = FxHashMap::default();
    peer_dependencies.insert("react".to_string(), IgnoredAny);
    peer_dependencies.insert("vue".to_string(), IgnoredAny);
    peer_dependencies.insert("solid-js".to_string(), IgnoredAny);

    let mut peer_dependencies_meta = FxHashMap::default();
    peer_dependencies_meta
      .insert("react".to_string(), PackageJsonPeerDependenciesMetaRaw { optional: true });
    peer_dependencies_meta
      .insert("vue".to_string(), PackageJsonPeerDependenciesMetaRaw { optional: false });

    let optional_peer_dependencies =
      optional_peer_dependencies_from_maps(peer_dependencies, &peer_dependencies_meta);

    assert!(optional_peer_dependencies.contains("react"));
    assert!(!optional_peer_dependencies.contains("vue"));
    assert!(!optional_peer_dependencies.contains("solid-js"));
    assert!(optional_peer_dependencies.capacity() >= peer_dependencies_meta.len());
  }

  #[test]
  fn package_json_optional_peer_deps_strip_bom_and_reject_syntax_errors() {
    let parsed =
      parse_package_json_optional_peer_dependencies("\u{feff}{\"name\":\"pkg\"}".as_bytes());
    assert_eq!(parsed.name, "pkg");
    assert!(parsed.optional_peer_dependencies.is_empty());

    let invalid = parse_package_json_optional_peer_dependencies(br#"{"name":"pkg""#);
    assert_eq!(invalid, PackageJsonOptionalPeerDependencies::default());
  }

  #[test]
  fn package_json_optional_peer_deps_bom_strip_works_on_bytes() {
    let source = "\u{feff}{\"name\":\"pkg\"}".as_bytes();

    assert_eq!(package_json_source_without_utf8_bom(source), br#"{"name":"pkg"}"#);
    assert_eq!(package_json_source_without_utf8_bom(br#"{"name":"pkg"}"#), br#"{"name":"pkg"}"#);
  }

  #[test]
  fn package_json_optional_peer_deps_duplicate_keys_follow_serde_json_semantics() {
    let parsed = parse_package_json_optional_peer_dependencies(
      br#"{
        "name": "pkg",
        "peerDependencies": { "react": "*" },
        "peerDependenciesMeta": {
          "react": { "optional": true },
          "react": { "optional": false }
        }
      }"#,
    );

    assert!(!parsed.optional_peer_dependencies.contains("react"));
  }
}
