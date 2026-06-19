use std::{
  borrow::Cow,
  ffi::OsString,
  fmt, fs,
  io::{self, Read, Write},
  path::{Component, Path, PathBuf},
  sync::atomic::{AtomicU64, Ordering},
};

use serde::Deserialize;

const DX_MACHINE_CACHE_ENV: &str = "ROLLDOWN_DX_JSON_CACHE";
const DX_MACHINE_CACHE_UNSYNCED_WRITES_ENV: &str = "ROLLDOWN_DX_JSON_CACHE_UNSYNCED_WRITES";
const DX_MACHINE_METADATA_SCHEMA: &str = "dx.machine.source_metadata.v1";
const DX_MACHINE_CACHE_STEM_MAX_BYTES: usize = 160;
const DX_MACHINE_METADATA_MAX_BYTES: u64 = 64 * 1024;
const DX_MACHINE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const DX_MACHINE_READ_PREALLOC_MAX_BYTES: u64 = 256 * 1024;
const BLAKE3_HEX_LEN: usize = 64;
const LOWER_HEX_BYTES: &[u8; 16] = b"0123456789abcdef";
const MACHINE_METADATA_SOURCE_PATH_PREFIX: &str = concat!(
  "{\n",
  "  \"schema\": \"dx.machine.source_metadata.v1\",\n",
  "  \"source\": {\n",
  "    \"path\": \""
);
const MACHINE_METADATA_SOURCE_BYTES_PREFIX: &str = "\",\n    \"bytes\": ";
const MACHINE_METADATA_MODIFIED_PREFIX: &str = ",\n    \"modified_unix_ms\": ";
const MACHINE_METADATA_SOURCE_HASH_PREFIX: &str = ",\n    \"blake3\": \"";
const MACHINE_METADATA_MACHINE_PATH_PREFIX: &str = "\"\n  },\n  \"machine\": {\n    \"path\": \"";
const MACHINE_METADATA_MACHINE_BYTES_PREFIX: &str = "\",\n    \"bytes\": ";
const MACHINE_METADATA_MACHINE_HASH_PREFIX: &str = ",\n    \"blake3\": \"";
const MACHINE_METADATA_SUFFIX: &str = concat!(
  "\"\n",
  "  },\n",
  "  \"cache\": {\n",
  "    \"rebuildable\": true,\n",
  "    \"fallback_on_mismatch\": true\n",
  "  }\n",
  "}\n"
);
static DX_MACHINE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
  static DX_MACHINE_TEST_TEMP_PATH_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_PATH_IDENTITY_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_SOURCE_ENTRY_SHAPE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_ENTRY_HASH_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_MACHINE_FILE_METADATA_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_SOURCE_METADATA_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_WRITE_SOURCE_HASH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_SYNC_ALL_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_TARGET_EXISTS_PROBE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_TEMP_EXISTS_PROBE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_PARENT_DIR_CREATE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_METADATA_FILE_METADATA_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_EXISTING_FILE_PATH_METADATA_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_EXISTING_FILE_HANDLE_METADATA_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_MACHINE_ENTRY_BYTES_HASH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_MACHINE_ENTRY_READ_SHAPE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_MACHINE_FILE_READ_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_HASHING_READER_READ_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_METADATA_SERDE_PARSE_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_SMALL_EXACT_INLINE_HASH_UPDATES: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
  static DX_MACHINE_TEST_NORMALIZED_METADATA_PATH_MATCH_CALLS: std::cell::Cell<u64> =
    const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DxMachineCacheStatus<T> {
  Hit(T),
  Miss,
  Disabled,
  Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxMachineCacheConfig {
  pub enabled: bool,
  pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxMachineCachePaths {
  pub machine: PathBuf,
  pub metadata: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxMachineCacheHit {
  pub machine_bytes: Vec<u8>,
  pub source_hash: blake3::Hash,
}

impl DxMachineCacheConfig {
  pub fn from_env(cwd: &Path) -> Self {
    Self::from_env_value(cwd, std::env::var_os(DX_MACHINE_CACHE_ENV))
  }

  pub fn from_env_if_enabled(cwd: &Path) -> Option<Self> {
    Self::from_env_value_if_enabled(cwd, std::env::var_os(DX_MACHINE_CACHE_ENV))
  }

  pub fn from_env_value(cwd: &Path, env_value: Option<OsString>) -> Self {
    Self {
      enabled: env_enables_dx_machine_cache(env_value),
      root: cwd.join(".dx").join("rolldown"),
    }
  }

  pub fn from_env_value_if_enabled(cwd: &Path, env_value: Option<OsString>) -> Option<Self> {
    env_enables_dx_machine_cache(env_value)
      .then(|| Self { enabled: true, root: cwd.join(".dx").join("rolldown") })
  }

  pub fn paths_for_source(
    &self,
    project_root: &Path,
    namespace: &str,
    source_path: &Path,
  ) -> DxMachineCachePaths {
    let relative_path = source_path.strip_prefix(project_root).unwrap_or(source_path);
    let stem = cache_stem(namespace, relative_path);

    DxMachineCachePaths {
      machine: self.root.join(cache_artifact_file_name(&stem, ".machine")),
      metadata: self.root.join(cache_artifact_file_name(&stem, ".machine.meta.json")),
    }
  }

  pub fn paths_for_source_if_enabled(
    &self,
    project_root: &Path,
    namespace: &str,
    source_path: &Path,
  ) -> Option<DxMachineCachePaths> {
    self.enabled.then(|| self.paths_for_source(project_root, namespace, source_path))
  }

  pub fn read_validated_machine(
    &self,
    paths: &DxMachineCachePaths,
    source_path: &Path,
    source_bytes: &[u8],
  ) -> DxMachineCacheStatus<Vec<u8>> {
    match self.read_validated_machine_with_source_hash(paths, source_path, source_bytes) {
      DxMachineCacheStatus::Hit(hit) => DxMachineCacheStatus::Hit(hit.machine_bytes),
      DxMachineCacheStatus::Miss => DxMachineCacheStatus::Miss,
      DxMachineCacheStatus::Disabled => DxMachineCacheStatus::Disabled,
      DxMachineCacheStatus::Invalid => DxMachineCacheStatus::Invalid,
    }
  }

  pub fn read_validated_machine_with_source_hash(
    &self,
    paths: &DxMachineCachePaths,
    source_path: &Path,
    source_bytes: &[u8],
  ) -> DxMachineCacheStatus<DxMachineCacheHit> {
    if !self.enabled {
      return DxMachineCacheStatus::Disabled;
    }

    let metadata_bytes =
      match read_file_with_max_len(&paths.metadata, DX_MACHINE_METADATA_MAX_BYTES) {
        DxMachineCacheStatus::Hit(metadata_bytes) => metadata_bytes,
        DxMachineCacheStatus::Miss => return DxMachineCacheStatus::Miss,
        DxMachineCacheStatus::Invalid => return DxMachineCacheStatus::Invalid,
        DxMachineCacheStatus::Disabled => return DxMachineCacheStatus::Disabled,
      };
    let Some(metadata) = parse_machine_metadata_document(&metadata_bytes) else {
      return DxMachineCacheStatus::Invalid;
    };

    if !validate_machine_metadata_source_and_policy_before_hash(
      &metadata,
      source_path,
      source_bytes,
    ) {
      return DxMachineCacheStatus::Invalid;
    }

    let machine = &metadata.machine;
    match validate_metadata_path_and_hash_shape(machine, &paths.machine) {
      DxMachineCacheStatus::Hit(()) => {}
      DxMachineCacheStatus::Miss => return DxMachineCacheStatus::Miss,
      DxMachineCacheStatus::Invalid => return DxMachineCacheStatus::Invalid,
      DxMachineCacheStatus::Disabled => return DxMachineCacheStatus::Disabled,
    }

    let Some(source_hash) =
      validate_metadata_entry_hash_after_precheck(&metadata.source, source_bytes)
    else {
      return DxMachineCacheStatus::Invalid;
    };

    let machine_file = match open_machine_file(&paths.machine) {
      DxMachineCacheStatus::Hit(machine_file) => Some(machine_file),
      DxMachineCacheStatus::Miss => None,
      DxMachineCacheStatus::Invalid => return DxMachineCacheStatus::Invalid,
      DxMachineCacheStatus::Disabled => return DxMachineCacheStatus::Disabled,
    };

    let machine_read = match machine_file {
      Some(machine_file) => {
        match read_open_file_with_exact_len_and_hash(machine_file, machine.bytes) {
          DxMachineCacheStatus::Hit(machine_read) => Some(machine_read),
          DxMachineCacheStatus::Miss => return DxMachineCacheStatus::Miss,
          DxMachineCacheStatus::Invalid => return DxMachineCacheStatus::Invalid,
          DxMachineCacheStatus::Disabled => return DxMachineCacheStatus::Disabled,
        }
      }
      None => None,
    };

    let Some(machine_read) = machine_read else {
      return DxMachineCacheStatus::Miss;
    };

    if !validate_metadata_entry_read_hash(machine, machine_read.bytes.len(), &machine_read.hash) {
      return DxMachineCacheStatus::Invalid;
    }

    DxMachineCacheStatus::Hit(DxMachineCacheHit { machine_bytes: machine_read.bytes, source_hash })
  }

  pub fn write_machine_artifact(
    &self,
    paths: &DxMachineCachePaths,
    source_path: &Path,
    source_bytes: &[u8],
    machine_bytes: &[u8],
  ) -> io::Result<()> {
    if !self.enabled {
      return Ok(());
    }
    if !machine_artifact_benefits_source_size(source_bytes.len() as u64, machine_bytes.len() as u64)
    {
      return Ok(());
    }
    let source_hash = hash_source_bytes_for_machine_write(source_bytes);
    self.write_machine_artifact_with_source_hash(
      paths,
      source_path,
      source_bytes.len(),
      &source_hash,
      machine_bytes,
    )
  }

  pub fn write_machine_artifact_with_source_hash(
    &self,
    paths: &DxMachineCachePaths,
    source_path: &Path,
    source_len: usize,
    source_hash: &blake3::Hash,
    machine_bytes: &[u8],
  ) -> io::Result<()> {
    self.write_machine_artifact_with_source_hash_and_options(
      paths,
      source_path,
      source_len,
      source_hash,
      machine_bytes,
      DxMachineCacheWriteOptions::from_env(),
    )
  }

  fn write_machine_artifact_with_source_hash_and_options(
    &self,
    paths: &DxMachineCachePaths,
    source_path: &Path,
    source_len: usize,
    source_hash: &blake3::Hash,
    machine_bytes: &[u8],
    write_options: DxMachineCacheWriteOptions,
  ) -> io::Result<()> {
    if !self.enabled {
      return Ok(());
    }
    if !machine_artifact_benefits_source_size(source_len as u64, machine_bytes.len() as u64) {
      return Ok(());
    }

    write_atomic_with_options(&paths.machine, machine_bytes, write_options)?;
    let metadata = machine_metadata_json_with_source_hash(
      source_path,
      source_len,
      source_hash,
      &paths.machine,
      machine_bytes,
    );
    write_atomic_with_options(&paths.metadata, metadata.as_bytes(), write_options)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DxMachineCacheWriteOptions {
  sync_writes: bool,
}

impl Default for DxMachineCacheWriteOptions {
  fn default() -> Self {
    Self { sync_writes: true }
  }
}

impl DxMachineCacheWriteOptions {
  fn from_env() -> Self {
    Self::from_env_value(std::env::var_os(DX_MACHINE_CACHE_UNSYNCED_WRITES_ENV))
  }

  fn from_env_value(env_value: Option<OsString>) -> Self {
    Self { sync_writes: !env_enables_dx_machine_cache(env_value) }
  }
}

fn hash_source_bytes_for_machine_write(source_bytes: &[u8]) -> blake3::Hash {
  #[cfg(test)]
  DX_MACHINE_TEST_WRITE_SOURCE_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  blake3::hash(source_bytes)
}

pub fn validate_machine_metadata(
  metadata_json: &str,
  source_path: &Path,
  source_bytes: &[u8],
  machine_path: &Path,
  machine_bytes: &[u8],
) -> DxMachineCacheStatus<()> {
  let Some(metadata) = parse_machine_metadata_document(metadata_json.as_bytes()) else {
    return DxMachineCacheStatus::Invalid;
  };

  if !validate_machine_metadata_source_and_policy(&metadata, source_path, source_bytes) {
    return DxMachineCacheStatus::Invalid;
  }

  if !validate_metadata_entry(&metadata.machine, machine_path, machine_bytes) {
    return DxMachineCacheStatus::Invalid;
  }

  DxMachineCacheStatus::Hit(())
}

#[derive(Debug, Deserialize)]
struct MachineMetadataDocument<'a> {
  schema: &'a str,
  #[serde(borrow)]
  source: MachineMetadataEntry<'a>,
  #[serde(borrow)]
  machine: MachineMetadataEntry<'a>,
  cache: MachineCachePolicy,
}

#[derive(Debug, Deserialize)]
struct MachineMetadataEntry<'a> {
  #[serde(borrow)]
  path: Cow<'a, str>,
  bytes: u64,
  blake3: &'a str,
}

#[derive(Debug, Deserialize)]
struct MachineCachePolicy {
  rebuildable: bool,
  fallback_on_mismatch: bool,
}

fn parse_machine_metadata_document(metadata_json: &[u8]) -> Option<MachineMetadataDocument<'_>> {
  parse_canonical_machine_metadata_document(metadata_json).or_else(|| {
    #[cfg(test)]
    DX_MACHINE_TEST_METADATA_SERDE_PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));

    serde_json::from_slice(metadata_json).ok()
  })
}

fn parse_canonical_machine_metadata_document(
  metadata_json: &[u8],
) -> Option<MachineMetadataDocument<'_>> {
  let mut parser = CanonicalMachineMetadataParser::new(metadata_json);

  parser.consume(MACHINE_METADATA_SOURCE_PATH_PREFIX.as_bytes())?;
  let source_path = parser.read_json_string_content()?;
  parser.consume(MACHINE_METADATA_SOURCE_BYTES_PREFIX.as_bytes())?;
  let source_bytes = parser.read_decimal_u64()?;
  parser.consume(MACHINE_METADATA_MODIFIED_PREFIX.as_bytes())?;
  parser.skip_modified_unix_ms()?;
  parser.consume(MACHINE_METADATA_SOURCE_HASH_PREFIX.as_bytes())?;
  let source_hash = parser.read_blake3_hex()?;
  parser.consume(MACHINE_METADATA_MACHINE_PATH_PREFIX.as_bytes())?;
  let machine_path = parser.read_json_string_content()?;
  parser.consume(MACHINE_METADATA_MACHINE_BYTES_PREFIX.as_bytes())?;
  let machine_bytes = parser.read_decimal_u64()?;
  parser.consume(MACHINE_METADATA_MACHINE_HASH_PREFIX.as_bytes())?;
  let machine_hash = parser.read_blake3_hex()?;
  parser.consume(MACHINE_METADATA_SUFFIX.as_bytes())?;
  if !parser.is_done() {
    return None;
  }

  Some(MachineMetadataDocument {
    schema: DX_MACHINE_METADATA_SCHEMA,
    source: MachineMetadataEntry { path: source_path, bytes: source_bytes, blake3: source_hash },
    machine: MachineMetadataEntry {
      path: machine_path,
      bytes: machine_bytes,
      blake3: machine_hash,
    },
    cache: MachineCachePolicy { rebuildable: true, fallback_on_mismatch: true },
  })
}

struct CanonicalMachineMetadataParser<'a> {
  bytes: &'a [u8],
  cursor: usize,
}

impl<'a> CanonicalMachineMetadataParser<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self { bytes, cursor: 0 }
  }

  fn consume(&mut self, expected: &[u8]) -> Option<()> {
    let end = self.cursor.checked_add(expected.len())?;
    if self.bytes.get(self.cursor..end)? != expected {
      return None;
    }
    self.cursor = end;
    Some(())
  }

  fn read_json_string_content(&mut self) -> Option<Cow<'a, str>> {
    let start = self.cursor;
    let mut chunk_start = start;
    let mut output: Option<String> = None;

    while self.cursor < self.bytes.len() {
      match self.bytes[self.cursor] {
        b'"' => {
          let end = self.cursor;
          return match output {
            Some(mut output) => {
              if chunk_start < end {
                output.push_str(std::str::from_utf8(&self.bytes[chunk_start..end]).ok()?);
              }
              Some(Cow::Owned(output))
            }
            None => Some(Cow::Borrowed(std::str::from_utf8(&self.bytes[start..end]).ok()?)),
          };
        }
        b'\\' => {
          let escape_start = self.cursor;
          let output =
            output.get_or_insert_with(|| String::with_capacity(self.bytes.len() - start));
          if chunk_start < escape_start {
            output.push_str(std::str::from_utf8(&self.bytes[chunk_start..escape_start]).ok()?);
          }
          self.cursor += 1;
          self.push_escaped_json_byte(output)?;
          chunk_start = self.cursor;
        }
        0..=0x1F => return None,
        _ => {
          self.cursor += 1;
        }
      }
    }

    None
  }

  fn push_escaped_json_byte(&mut self, output: &mut String) -> Option<()> {
    match *self.bytes.get(self.cursor)? {
      b'"' => {
        output.push('"');
        self.cursor += 1;
      }
      b'\\' => {
        output.push('\\');
        self.cursor += 1;
      }
      b'n' => {
        output.push('\n');
        self.cursor += 1;
      }
      b'r' => {
        output.push('\r');
        self.cursor += 1;
      }
      b't' => {
        output.push('\t');
        self.cursor += 1;
      }
      b'u' => {
        let escape = self.bytes.get(self.cursor..self.cursor.checked_add(5)?)?;
        if escape[1] != b'0' || escape[2] != b'0' {
          return None;
        }
        let high = hex_nibble(escape[3])?;
        let low = hex_nibble(escape[4])?;
        output.push(char::from((high << 4) | low));
        self.cursor += 5;
      }
      _ => return None,
    }
    Some(())
  }

  fn read_decimal_u64(&mut self) -> Option<u64> {
    let start = self.cursor;
    let mut value = 0_u64;
    while let Some(byte) = self.bytes.get(self.cursor).copied() {
      if !byte.is_ascii_digit() {
        break;
      }
      value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
      self.cursor += 1;
    }
    (self.cursor > start).then_some(value)
  }

  fn skip_modified_unix_ms(&mut self) -> Option<()> {
    if self.bytes.get(self.cursor..self.cursor.checked_add(4)?) == Some(b"null") {
      self.cursor += 4;
      return Some(());
    }
    self.read_decimal_u64().map(|_| ())
  }

  fn read_blake3_hex(&mut self) -> Option<&'a str> {
    let end = self.cursor.checked_add(BLAKE3_HEX_LEN)?;
    let bytes = self.bytes.get(self.cursor..end)?;
    if !bytes.iter().copied().all(is_lower_hex_byte) {
      return None;
    }
    self.cursor = end;
    Some(lower_hex_bytes_to_str(bytes))
  }

  fn is_done(&self) -> bool {
    self.cursor == self.bytes.len()
  }
}

fn hex_nibble(byte: u8) -> Option<u8> {
  match byte {
    b'0'..=b'9' => Some(byte - b'0'),
    b'a'..=b'f' => Some(byte - b'a' + 10),
    _ => None,
  }
}

fn is_lower_hex_byte(byte: u8) -> bool {
  matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

fn lower_hex_bytes_to_str(bytes: &[u8]) -> &str {
  debug_assert!(bytes.iter().copied().all(is_lower_hex_byte));
  // SAFETY: The canonical metadata parser accepts only lowercase ASCII hex
  // bytes before calling this helper, so the slice is valid UTF-8.
  unsafe { std::str::from_utf8_unchecked(bytes) }
}

fn read_file_with_max_len(path: &Path, max_len: u64) -> DxMachineCacheStatus<Vec<u8>> {
  let file = match fs::File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return DxMachineCacheStatus::Miss,
    Err(_) => return DxMachineCacheStatus::Invalid,
  };

  match read_file_bytes_with_limit(file, 0, max_len) {
    Ok(Some(bytes)) => DxMachineCacheStatus::Hit(bytes),
    Ok(None) => DxMachineCacheStatus::Invalid,
    Err(()) => DxMachineCacheStatus::Invalid,
  }
}

struct ReadFileBytesWithHash {
  bytes: Vec<u8>,
  hash: blake3::Hash,
}

struct HashingReader<R> {
  inner: R,
  hasher: blake3::Hasher,
}

impl<R> HashingReader<R> {
  fn new(inner: R) -> Self {
    Self { inner, hasher: blake3::Hasher::new() }
  }

  fn finalize(self) -> blake3::Hash {
    self.hasher.finalize()
  }
}

impl<R: Read> Read for HashingReader<R> {
  fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
    let read_len = self.inner.read(output)?;
    if read_len > 0 {
      #[cfg(test)]
      DX_MACHINE_TEST_HASHING_READER_READ_CALLS.with(|calls| calls.set(calls.get() + 1));

      self.hasher.update(&output[..read_len]);
    }
    Ok(read_len)
  }
}

fn read_open_file_with_exact_len_and_hash(
  file: fs::File,
  expected_len: u64,
) -> DxMachineCacheStatus<ReadFileBytesWithHash> {
  match read_file_bytes_with_limit_and_hash(file, expected_len, expected_len) {
    Ok(Some(read)) if read.bytes.len() as u64 == expected_len => DxMachineCacheStatus::Hit(read),
    Ok(Some(_)) | Ok(None) => DxMachineCacheStatus::Invalid,
    Err(()) => DxMachineCacheStatus::Miss,
  }
}

fn read_file_bytes_with_limit(
  file: fs::File,
  initial_capacity: u64,
  max_len: u64,
) -> Result<Option<Vec<u8>>, ()> {
  let initial_capacity = read_file_initial_capacity(initial_capacity, max_len);
  let mut bytes = Vec::new();
  bytes.try_reserve_exact(initial_capacity).map_err(|_| ())?;
  let mut reader = file.take(max_len.saturating_add(1));
  reader.read_to_end(&mut bytes).map_err(|_| ())?;
  if bytes.len() as u64 > max_len { Ok(None) } else { Ok(Some(bytes)) }
}

fn read_file_bytes_with_limit_and_hash(
  file: fs::File,
  initial_capacity: u64,
  max_len: u64,
) -> Result<Option<ReadFileBytesWithHash>, ()> {
  #[cfg(test)]
  DX_MACHINE_TEST_MACHINE_FILE_READ_CALLS.with(|calls| calls.set(calls.get() + 1));

  if initial_capacity == max_len && max_len <= DX_MACHINE_READ_PREALLOC_MAX_BYTES {
    return read_small_exact_file_bytes_with_hash(file, max_len);
  }

  let initial_capacity = read_file_initial_capacity(initial_capacity, max_len);
  let mut bytes = Vec::new();
  bytes.try_reserve_exact(initial_capacity).map_err(|_| ())?;
  let mut reader = HashingReader::new(file.take(max_len.saturating_add(1)));
  reader.read_to_end(&mut bytes).map_err(|_| ())?;
  if bytes.len() as u64 > max_len {
    return Ok(None);
  }
  Ok(Some(ReadFileBytesWithHash { bytes, hash: reader.finalize() }))
}

fn read_small_exact_file_bytes_with_hash(
  mut file: fs::File,
  expected_len: u64,
) -> Result<Option<ReadFileBytesWithHash>, ()> {
  let expected_len = usize::try_from(expected_len).map_err(|_| ())?;
  let mut bytes = Vec::new();
  bytes.try_reserve_exact(expected_len).map_err(|_| ())?;
  let mut hasher = blake3::Hasher::new();
  let mut remaining = expected_len;
  let mut buffer = [0; 16 * 1024];

  while remaining > 0 {
    let chunk_len = remaining.min(buffer.len());
    let read_len = file.read(&mut buffer[..chunk_len]).map_err(|_| ())?;
    if read_len == 0 {
      return Ok(Some(ReadFileBytesWithHash { bytes, hash: hasher.finalize() }));
    }
    bytes.extend_from_slice(&buffer[..read_len]);
    #[cfg(test)]
    DX_MACHINE_TEST_SMALL_EXACT_INLINE_HASH_UPDATES.with(|calls| calls.set(calls.get() + 1));
    hasher.update(&buffer[..read_len]);
    remaining -= read_len;
  }

  let mut extra = [0; 1];
  if file.read(&mut extra).map_err(|_| ())? != 0 {
    return Ok(None);
  }

  Ok(Some(ReadFileBytesWithHash { bytes, hash: hasher.finalize() }))
}

fn read_file_initial_capacity(initial_capacity: u64, max_len: u64) -> usize {
  initial_capacity.min(max_len).min(DX_MACHINE_READ_PREALLOC_MAX_BYTES) as usize
}

#[cfg(test)]
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  write_atomic_with_options(path, bytes, DxMachineCacheWriteOptions::default())
}

fn write_atomic_with_options(
  path: &Path,
  bytes: &[u8],
  write_options: DxMachineCacheWriteOptions,
) -> io::Result<()> {
  if existing_file_matches(path, bytes) {
    return Ok(());
  }
  if let Some(parent) = path.parent() {
    create_parent_dir_all(parent)?;
  }

  let temp_path = atomic_temp_path(path);
  remove_file_if_exists(&temp_path)?;

  let mut file = fs::File::create(&temp_path)?;
  file.write_all(bytes)?;
  if write_options.sync_writes {
    sync_file(&file)?;
  }
  drop(file);

  remove_file_if_exists(path)?;

  fs::rename(&temp_path, path).inspect_err(|_| {
    let _ = fs::remove_file(&temp_path);
  })
}

fn sync_file(file: &fs::File) -> io::Result<()> {
  #[cfg(test)]
  DX_MACHINE_TEST_SYNC_ALL_CALLS.with(|calls| calls.set(calls.get() + 1));

  file.sync_all()
}

fn create_parent_dir_all(parent: &Path) -> io::Result<()> {
  #[cfg(test)]
  DX_MACHINE_TEST_PARENT_DIR_CREATE_CALLS.with(|calls| calls.set(calls.get() + 1));

  fs::create_dir_all(parent)
}

fn existing_file_matches(path: &Path, bytes: &[u8]) -> bool {
  let Ok(mut file) = fs::File::open(path) else {
    return false;
  };
  reader_matches_bytes(&mut file, bytes).unwrap_or(false)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

fn reader_matches_bytes<R: Read>(reader: &mut R, bytes: &[u8]) -> io::Result<bool> {
  let mut offset = 0;
  let mut buffer = [0; 16 * 1024];

  loop {
    let read_len = reader.read(&mut buffer)?;
    if read_len == 0 {
      return Ok(offset == bytes.len());
    }

    let end = offset + read_len;
    if end > bytes.len() || buffer[..read_len] != bytes[offset..end] {
      return Ok(false);
    }
    offset = end;
  }
}

fn atomic_temp_path(path: &Path) -> PathBuf {
  #[cfg(test)]
  DX_MACHINE_TEST_TEMP_PATH_CALLS.with(|calls| calls.set(calls.get() + 1));

  let file_name = path.file_name().and_then(|file_name| file_name.to_str()).unwrap_or("cache");
  let counter = DX_MACHINE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
  path.with_file_name(atomic_temp_file_name(file_name, std::process::id(), counter))
}

fn atomic_temp_file_name(file_name: &str, process_id: u32, counter: u64) -> String {
  let mut output = String::with_capacity(
    file_name.len()
      + ".".len()
      + decimal_u128_len(process_id as u128)
      + ".".len()
      + decimal_u128_len(counter as u128)
      + ".tmp".len(),
  );
  output.push_str(file_name);
  output.push('.');
  push_decimal_u32(&mut output, process_id);
  output.push('.');
  push_decimal_u64(&mut output, counter);
  output.push_str(".tmp");
  output
}

#[cfg(test)]
fn machine_metadata_json(
  source_path: &Path,
  source_bytes: &[u8],
  machine_path: &Path,
  machine_bytes: &[u8],
) -> String {
  let source_hash = blake3::hash(source_bytes);
  machine_metadata_json_with_source_hash(
    source_path,
    source_bytes.len(),
    &source_hash,
    machine_path,
    machine_bytes,
  )
}

fn machine_metadata_json_with_source_hash(
  source_path: &Path,
  source_len: usize,
  source_hash: &blake3::Hash,
  machine_path: &Path,
  machine_bytes: &[u8],
) -> String {
  let modified_unix_ms = ModifiedUnixMsJson(None);
  let machine_hash = blake3::hash(machine_bytes);
  let mut metadata = String::with_capacity(machine_metadata_json_len(
    source_path,
    source_len,
    &modified_unix_ms,
    machine_path,
    machine_bytes.len(),
  ));

  metadata.push_str(MACHINE_METADATA_SOURCE_PATH_PREFIX);
  push_json_escape_path(&mut metadata, source_path);
  metadata.push_str(MACHINE_METADATA_SOURCE_BYTES_PREFIX);
  push_decimal_usize_json(&mut metadata, source_len);
  metadata.push_str(MACHINE_METADATA_MODIFIED_PREFIX);
  push_modified_unix_ms_json(&mut metadata, &modified_unix_ms);
  metadata.push_str(MACHINE_METADATA_SOURCE_HASH_PREFIX);
  push_blake3_hex(&mut metadata, source_hash);
  metadata.push_str(MACHINE_METADATA_MACHINE_PATH_PREFIX);
  push_json_escape_path(&mut metadata, machine_path);
  metadata.push_str(MACHINE_METADATA_MACHINE_BYTES_PREFIX);
  push_decimal_usize_json(&mut metadata, machine_bytes.len());
  metadata.push_str(MACHINE_METADATA_MACHINE_HASH_PREFIX);
  push_blake3_hex(&mut metadata, &machine_hash);
  metadata.push_str(MACHINE_METADATA_SUFFIX);

  metadata
}

fn machine_metadata_json_len(
  source_path: &Path,
  source_len: usize,
  modified_unix_ms: &ModifiedUnixMsJson,
  machine_path: &Path,
  machine_len: usize,
) -> usize {
  MACHINE_METADATA_SOURCE_PATH_PREFIX.len()
    + json_escaped_path_len(source_path)
    + MACHINE_METADATA_SOURCE_BYTES_PREFIX.len()
    + decimal_usize_len(source_len)
    + MACHINE_METADATA_MODIFIED_PREFIX.len()
    + modified_unix_ms.json_len()
    + MACHINE_METADATA_SOURCE_HASH_PREFIX.len()
    + BLAKE3_HEX_LEN
    + MACHINE_METADATA_MACHINE_PATH_PREFIX.len()
    + json_escaped_path_len(machine_path)
    + MACHINE_METADATA_MACHINE_BYTES_PREFIX.len()
    + decimal_usize_len(machine_len)
    + MACHINE_METADATA_MACHINE_HASH_PREFIX.len()
    + BLAKE3_HEX_LEN
    + MACHINE_METADATA_SUFFIX.len()
}

struct ModifiedUnixMsJson(Option<u128>);

impl ModifiedUnixMsJson {
  fn json_len(&self) -> usize {
    self.0.map(decimal_u128_len).unwrap_or("null".len())
  }
}

impl fmt::Display for ModifiedUnixMsJson {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.0 {
      Some(modified_unix_ms) => write!(formatter, "{modified_unix_ms}"),
      None => formatter.write_str("null"),
    }
  }
}

fn push_decimal_usize_json(output: &mut String, value: usize) {
  let mut buffer = itoa::Buffer::new();
  output.push_str(buffer.format(value));
}

fn push_decimal_u32(output: &mut String, value: u32) {
  let mut buffer = itoa::Buffer::new();
  output.push_str(buffer.format(value));
}

fn push_decimal_u64(output: &mut String, value: u64) {
  let mut buffer = itoa::Buffer::new();
  output.push_str(buffer.format(value));
}

fn push_modified_unix_ms_json(output: &mut String, modified_unix_ms: &ModifiedUnixMsJson) {
  match modified_unix_ms.0 {
    Some(modified_unix_ms) => {
      let mut buffer = itoa::Buffer::new();
      output.push_str(buffer.format(modified_unix_ms));
    }
    None => output.push_str("null"),
  }
}

fn validate_metadata_entry(metadata: &MachineMetadataEntry<'_>, path: &Path, bytes: &[u8]) -> bool {
  validate_metadata_entry_with_hash(metadata, path, bytes).is_some()
}

#[cfg(test)]
fn validate_metadata_entry_bytes_with_known_hash_shape(
  metadata: &MachineMetadataEntry<'_>,
  bytes: &[u8],
) -> bool {
  if metadata.bytes != bytes.len() as u64 {
    return false;
  }

  debug_assert!(is_blake3_hex(metadata.blake3));
  #[cfg(test)]
  DX_MACHINE_TEST_MACHINE_ENTRY_BYTES_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  let expected = blake3::hash(bytes);
  blake3_hex_matches_known_valid_hash(metadata.blake3, &expected)
}

fn validate_metadata_entry_read_hash(
  metadata: &MachineMetadataEntry<'_>,
  bytes_len: usize,
  hash: &blake3::Hash,
) -> bool {
  debug_assert!(metadata.bytes <= DX_MACHINE_MAX_BYTES);
  debug_assert_eq!(metadata.bytes, bytes_len as u64);
  debug_assert!(is_blake3_hex(metadata.blake3));
  blake3_hex_matches_known_valid_hash(metadata.blake3, hash)
}

fn validate_metadata_entry_with_hash(
  metadata: &MachineMetadataEntry<'_>,
  path: &Path,
  bytes: &[u8],
) -> Option<blake3::Hash> {
  if metadata.bytes > DX_MACHINE_MAX_BYTES
    || metadata.bytes != bytes.len() as u64
    || !is_blake3_hex(metadata.blake3)
  {
    return None;
  }
  if !metadata_path_matches(metadata.path.as_ref(), path) {
    return None;
  }

  #[cfg(test)]
  DX_MACHINE_TEST_SOURCE_ENTRY_SHAPE_CALLS.with(|calls| calls.set(calls.get() + 1));

  #[cfg(test)]
  DX_MACHINE_TEST_ENTRY_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  let expected = blake3::hash(bytes);
  blake3_hex_matches_known_valid_hash(metadata.blake3, &expected).then_some(expected)
}

fn validate_metadata_entry_hash_after_precheck(
  metadata: &MachineMetadataEntry<'_>,
  bytes: &[u8],
) -> Option<blake3::Hash> {
  debug_assert_eq!(metadata.bytes, bytes.len() as u64);
  debug_assert!(is_blake3_hex(metadata.blake3));

  #[cfg(test)]
  DX_MACHINE_TEST_ENTRY_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));

  let expected = blake3::hash(bytes);
  blake3_hex_matches_known_valid_hash(metadata.blake3, &expected).then_some(expected)
}

fn validate_metadata_entry_before_hash(
  metadata: &MachineMetadataEntry<'_>,
  path: &Path,
  bytes: &[u8],
) -> bool {
  if metadata.bytes != bytes.len() as u64 || !is_blake3_hex(metadata.blake3) {
    return false;
  }
  if !metadata_path_matches(metadata.path.as_ref(), path) {
    return false;
  }

  #[cfg(test)]
  DX_MACHINE_TEST_SOURCE_ENTRY_SHAPE_CALLS.with(|calls| calls.set(calls.get() + 1));

  true
}

fn open_machine_file(path: &Path) -> DxMachineCacheStatus<fs::File> {
  let file = match fs::File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return DxMachineCacheStatus::Miss,
    Err(_) => return DxMachineCacheStatus::Invalid,
  };

  DxMachineCacheStatus::Hit(file)
}

fn validate_metadata_path_and_hash_shape(
  metadata: &MachineMetadataEntry<'_>,
  path: &Path,
) -> DxMachineCacheStatus<()> {
  if metadata.bytes == 0 || metadata.bytes > DX_MACHINE_MAX_BYTES {
    return DxMachineCacheStatus::Invalid;
  }

  if !is_blake3_hex(metadata.blake3) {
    return DxMachineCacheStatus::Invalid;
  }

  if !metadata_path_matches(metadata.path.as_ref(), path) {
    return DxMachineCacheStatus::Invalid;
  }

  DxMachineCacheStatus::Hit(())
}

fn is_blake3_hex(hash: &str) -> bool {
  hash.len() == 64 && hash.bytes().all(|byte| lowercase_hex_nibble(byte).is_some())
}

#[cfg(test)]
fn blake3_hex_matches_bytes(hash: &str, bytes: &[u8]) -> bool {
  blake3_hash_matches_bytes(hash, bytes).is_some()
}

#[cfg(test)]
fn blake3_hash_matches_bytes(hash: &str, bytes: &[u8]) -> Option<blake3::Hash> {
  if hash.len() != 64 {
    return None;
  }

  let expected = blake3::hash(bytes);
  if blake3_hex_matches_hash(hash, &expected) { Some(expected) } else { None }
}

pub fn blake3_hex_matches_hash(hash: &str, expected: &blake3::Hash) -> bool {
  if hash.len() != 64 {
    return false;
  }

  for (hex_byte, expected_byte) in hash.as_bytes().chunks_exact(2).zip(expected.as_bytes()) {
    let Some(high) = lowercase_hex_nibble(hex_byte[0]) else {
      return false;
    };
    let Some(low) = lowercase_hex_nibble(hex_byte[1]) else {
      return false;
    };
    if (high << 4) | low != *expected_byte {
      return false;
    }
  }

  true
}

fn blake3_hex_matches_known_valid_hash(hash: &str, expected: &blake3::Hash) -> bool {
  debug_assert_eq!(hash.len(), 64);
  debug_assert!(hash.bytes().all(|byte| lowercase_hex_nibble(byte).is_some()));

  for (hex_byte, expected_byte) in hash.as_bytes().chunks_exact(2).zip(expected.as_bytes()) {
    let high = lowercase_hex_nibble_unchecked(hex_byte[0]);
    let low = lowercase_hex_nibble_unchecked(hex_byte[1]);
    if (high << 4) | low != *expected_byte {
      return false;
    }
  }

  true
}

fn push_blake3_hex(output: &mut String, hash: &blake3::Hash) {
  let start_len = output.len();
  output.reserve(BLAKE3_HEX_LEN);
  for byte in hash.as_bytes() {
    output.push(LOWER_HEX_BYTES[(byte >> 4) as usize] as char);
    output.push(LOWER_HEX_BYTES[(byte & 0x0f) as usize] as char);
  }
  debug_assert_eq!(output.len(), start_len + BLAKE3_HEX_LEN);
}

fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
  match byte {
    b'0'..=b'9' => Some(byte - b'0'),
    b'a'..=b'f' => Some(byte - b'a' + 10),
    _ => None,
  }
}

fn lowercase_hex_nibble_unchecked(byte: u8) -> u8 {
  match byte {
    b'0'..=b'9' => byte - b'0',
    b'a'..=b'f' => byte - b'a' + 10,
    _ => unreachable!("validated BLAKE3 hex contains only lowercase hex bytes"),
  }
}

fn metadata_path_matches(metadata_path: &str, runtime_path: &Path) -> bool {
  let runtime_path = runtime_path_str(runtime_path);
  let runtime_path = runtime_path.as_ref();
  if metadata_path == runtime_path {
    return true;
  }
  if common_windows_metadata_path_bytes_match(metadata_path, runtime_path) {
    return true;
  }
  normalized_metadata_paths_match(metadata_path, runtime_path)
}

fn runtime_path_str(path: &Path) -> Cow<'_, str> {
  path
    .as_os_str()
    .to_str()
    .map(Cow::Borrowed)
    .unwrap_or_else(|| Cow::Owned(path.display().to_string()))
}

fn normalized_metadata_paths_match(left: &str, right: &str) -> bool {
  #[cfg(test)]
  DX_MACHINE_TEST_NORMALIZED_METADATA_PATH_MATCH_CALLS.with(|calls| calls.set(calls.get() + 1));

  let mut left_components = metadata_path_components(left);
  let mut right_components = metadata_path_components(right);

  loop {
    match (left_components.next(), right_components.next()) {
      (None, None) => return true,
      (Some(left), Some(right)) if metadata_path_components_match(left, right) => {}
      _ => return false,
    }
  }
}

fn common_windows_metadata_path_bytes_match(left: &str, right: &str) -> bool {
  if !cfg!(windows) || left.len() != right.len() {
    return false;
  }

  left.as_bytes().iter().zip(right.as_bytes()).all(|(left, right)| {
    left == right
      || (metadata_path_byte_is_separator(*left) && metadata_path_byte_is_separator(*right))
      || left.eq_ignore_ascii_case(right)
  })
}

fn metadata_path_byte_is_separator(byte: u8) -> bool {
  byte == b'/' || byte == b'\\'
}

fn metadata_path_components(path: &str) -> impl Iterator<Item = &str> {
  path.split(['\\', '/']).filter(|component| !component.is_empty() && *component != ".")
}

fn metadata_path_components_match(left: &str, right: &str) -> bool {
  if cfg!(windows) { left.eq_ignore_ascii_case(right) } else { left == right }
}

#[cfg(test)]
fn normalize_metadata_path_components(path: &str) -> String {
  let mut normalized = String::with_capacity(normalized_metadata_path_len(path));
  let mut wrote_component = false;
  for component in
    path.split(['\\', '/']).filter(|component| !component.is_empty() && *component != ".")
  {
    if wrote_component {
      normalized.push('/');
    }
    normalized.push_str(component);
    wrote_component = true;
  }
  normalized
}

#[cfg(test)]
fn normalized_metadata_path_len(path: &str) -> usize {
  let mut len = 0;
  let mut component_count: usize = 0;
  for component in
    path.split(['\\', '/']).filter(|component| !component.is_empty() && *component != ".")
  {
    len += component.len();
    component_count += 1;
  }
  len + component_count.saturating_sub(1)
}

fn validate_machine_metadata_source_and_policy(
  metadata: &MachineMetadataDocument<'_>,
  source_path: &Path,
  source_bytes: &[u8],
) -> bool {
  validate_machine_metadata_source_and_policy_with_hash(metadata, source_path, source_bytes)
    .is_some()
}

fn validate_machine_metadata_source_and_policy_before_hash(
  metadata: &MachineMetadataDocument<'_>,
  source_path: &Path,
  source_bytes: &[u8],
) -> bool {
  if metadata.schema != DX_MACHINE_METADATA_SCHEMA || !validate_cache_policy(&metadata.cache) {
    return false;
  }
  if !machine_artifact_benefits_source_size(metadata.source.bytes, metadata.machine.bytes) {
    return false;
  }

  validate_metadata_entry_before_hash(&metadata.source, source_path, source_bytes)
}

fn validate_machine_metadata_source_and_policy_with_hash(
  metadata: &MachineMetadataDocument<'_>,
  source_path: &Path,
  source_bytes: &[u8],
) -> Option<blake3::Hash> {
  if metadata.schema != DX_MACHINE_METADATA_SCHEMA || !validate_cache_policy(&metadata.cache) {
    return None;
  }
  if !machine_artifact_benefits_source_size(metadata.source.bytes, metadata.machine.bytes) {
    return None;
  }

  let source_hash = validate_metadata_entry_with_hash(&metadata.source, source_path, source_bytes)?;
  Some(source_hash)
}

fn machine_artifact_benefits_source_size(source_len: u64, machine_len: u64) -> bool {
  machine_len > 0 && machine_len < source_len
}

fn validate_cache_policy(cache: &MachineCachePolicy) -> bool {
  cache.rebuildable && cache.fallback_on_mismatch
}

fn env_enables_dx_machine_cache(env_value: Option<OsString>) -> bool {
  let Some(env_value) = env_value else {
    return false;
  };
  let value = env_value.to_string_lossy();
  env_value_enables_dx_machine_cache(value.as_ref())
}

fn env_value_enables_dx_machine_cache(value: &str) -> bool {
  let value = value.trim();
  !value.is_empty() && !is_falsey_dx_machine_cache_env(value)
}

fn is_falsey_dx_machine_cache_env(value: &str) -> bool {
  value == "0"
    || value.eq_ignore_ascii_case("false")
    || value.eq_ignore_ascii_case("off")
    || value.eq_ignore_ascii_case("no")
}

#[cfg(test)]
fn flatten_path(path: &Path) -> String {
  let mut flattened = String::with_capacity(flatten_path_len(path));
  push_flattened_path(path, &mut flattened);
  flattened
}

fn push_flattened_path(path: &Path, flattened: &mut String) {
  let mut wrote_component = false;
  for value in path.components().filter_map(path_identity_component_value) {
    if wrote_component {
      flattened.push('-');
    }
    push_sanitized_path_component(flattened, &value);
    wrote_component = true;
  }

  if !wrote_component {
    flattened.push_str("source");
  }
}

#[cfg(test)]
fn flatten_path_len(path: &Path) -> usize {
  let mut len = 0;
  let mut component_count: usize = 0;
  for value in path.components().filter_map(path_identity_component_value) {
    len += sanitized_path_component_len(&value);
    component_count += 1;
  }

  if component_count == 0 { "source".len() } else { len + component_count - 1 }
}

#[cfg(test)]
fn path_identity_hash(path: &Path) -> String {
  let hash = path_identity_digest(path);
  short_blake3_hex_from_hash(&hash)
}

struct CachePathIdentity {
  flattened_path: String,
  path_hash: String,
}

fn cache_path_identity(path: &Path) -> CachePathIdentity {
  let (flattened_path_len, identity_digest) = flattened_path_len_and_identity_digest(path);
  let mut flattened_path = String::with_capacity(flattened_path_len);
  push_flattened_path(path, &mut flattened_path);

  CachePathIdentity { flattened_path, path_hash: short_blake3_hex_from_hash(&identity_digest) }
}

fn flattened_path_len_and_identity_digest(path: &Path) -> (usize, blake3::Hash) {
  let mut flattened_len = 0;
  let mut component_count: usize = 0;
  let mut hasher = blake3::Hasher::new();
  for value in path.components().filter_map(path_identity_component_value) {
    if component_count > 0 {
      hasher.update(b"/");
    }
    update_path_identity_component(&mut hasher, &value);
    flattened_len += sanitized_path_component_len(&value);
    component_count += 1;
  }

  if component_count == 0 {
    hasher.update(b"source");
    ("source".len(), hasher.finalize())
  } else {
    (flattened_len + component_count - 1, hasher.finalize())
  }
}

#[cfg(test)]
fn short_blake3_hex(bytes: &[u8]) -> String {
  let hash = blake3::hash(bytes);
  short_blake3_hex_from_hash(&hash)
}

fn short_blake3_hex_from_hash(hash: &blake3::Hash) -> String {
  let mut output = String::with_capacity(12);
  for byte in hash.as_bytes().iter().take(6) {
    output.push(LOWER_HEX_BYTES[(byte >> 4) as usize] as char);
    output.push(LOWER_HEX_BYTES[(byte & 0x0f) as usize] as char);
  }
  output
}

fn cache_stem(namespace: &str, relative_path: &Path) -> String {
  let namespace = sanitize_path_component(namespace);
  let cache_path = cache_path_identity(relative_path);
  let (namespace, flattened_path) =
    readable_cache_stem_parts(&namespace, &cache_path.flattened_path, &cache_path.path_hash);
  let mut stem = String::with_capacity(cache_stem_len_from_parts(
    namespace,
    flattened_path,
    &cache_path.path_hash,
  ));
  stem.push_str(namespace);
  stem.push_str("__");
  stem.push_str(flattened_path);
  stem.push_str("--");
  stem.push_str(&cache_path.path_hash);
  stem
}

fn cache_artifact_file_name(stem: &str, suffix: &str) -> String {
  let mut file_name = String::with_capacity(stem.len() + suffix.len());
  file_name.push_str(stem);
  file_name.push_str(suffix);
  file_name
}

fn readable_cache_stem_parts<'a>(
  namespace: &'a str,
  flattened_path: &'a str,
  path_hash: &str,
) -> (&'a str, &'a str) {
  let readable_budget = DX_MACHINE_CACHE_STEM_MAX_BYTES
    .saturating_sub(namespace.len() + "__".len() + "--".len() + path_hash.len());
  if readable_budget > 0 {
    (namespace, truncate_ascii_prefix(flattened_path, readable_budget))
  } else {
    let namespace_budget = DX_MACHINE_CACHE_STEM_MAX_BYTES
      .saturating_sub("source".len() + "__".len() + "--".len() + path_hash.len());
    (truncate_ascii_prefix(namespace, namespace_budget), "source")
  }
}

#[cfg(test)]
fn cache_stem_len(namespace: &str, relative_path: &Path) -> usize {
  let namespace = sanitize_path_component(namespace);
  let cache_path = cache_path_identity(relative_path);
  let (namespace, flattened_path) =
    readable_cache_stem_parts(&namespace, &cache_path.flattened_path, &cache_path.path_hash);
  cache_stem_len_from_parts(namespace, flattened_path, &cache_path.path_hash)
}

fn cache_stem_len_from_parts(namespace: &str, flattened_path: &str, path_hash: &str) -> usize {
  namespace.len() + "__".len() + flattened_path.len() + "--".len() + path_hash.len()
}

#[cfg(test)]
fn path_identity(path: &Path) -> String {
  let mut identity = String::with_capacity(path_identity_len(path));
  let mut wrote_component = false;
  for value in path.components().filter_map(path_identity_component_value) {
    if wrote_component {
      identity.push('/');
    }
    push_path_identity_component(&mut identity, &value);
    wrote_component = true;
  }

  if wrote_component { identity } else { "source".to_string() }
}

#[cfg(test)]
fn path_identity_digest(path: &Path) -> blake3::Hash {
  let mut hasher = blake3::Hasher::new();
  let mut wrote_component = false;
  for value in path.components().filter_map(path_identity_component_value) {
    if wrote_component {
      hasher.update(b"/");
    }
    update_path_identity_component(&mut hasher, &value);
    wrote_component = true;
  }

  if !wrote_component {
    hasher.update(b"source");
  }

  hasher.finalize()
}

#[cfg(test)]
fn path_identity_len(path: &Path) -> usize {
  let mut len = 0;
  let mut component_count: usize = 0;
  for value in path.components().filter_map(path_identity_component_value) {
    len += value.len();
    component_count += 1;
  }

  if component_count == 0 { "source".len() } else { len + component_count - 1 }
}

fn path_identity_component_value(component: Component<'_>) -> Option<Cow<'_, str>> {
  match component {
    Component::Normal(value) => Some(value.to_string_lossy()),
    Component::Prefix(value) => Some(value.as_os_str().to_string_lossy()),
    Component::ParentDir => Some(Cow::Borrowed("..")),
    Component::RootDir | Component::CurDir => None,
  }
}

#[cfg(test)]
fn push_path_identity_component(identity: &mut String, value: &str) {
  for ch in value.chars() {
    if ch == '\\' {
      identity.push('/');
    } else {
      identity.push(ch.to_ascii_lowercase());
    }
  }
}

fn update_path_identity_component(hasher: &mut blake3::Hasher, value: &str) {
  let mut buffer = [0; 4];
  for ch in value.chars() {
    let ch = if ch == '\\' { '/' } else { ch.to_ascii_lowercase() };
    hasher.update(ch.encode_utf8(&mut buffer).as_bytes());
  }
}

fn sanitize_path_component(value: &str) -> Cow<'_, str> {
  if is_borrowable_sanitized_path_component(value) {
    return Cow::Borrowed(value);
  }

  let mut output = String::with_capacity(sanitized_path_component_len(value));
  push_sanitized_path_component(&mut output, value);
  Cow::Owned(output)
}

fn is_borrowable_sanitized_path_component(value: &str) -> bool {
  !value.is_empty()
    && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn push_sanitized_path_component(output: &mut String, value: &str) {
  let mut wrote_alnum = false;
  let mut pending_dash = false;

  for ch in value.chars() {
    if ch.is_ascii_alphanumeric() || ch == '_' {
      if wrote_alnum && pending_dash {
        output.push('-');
      }
      output.push(ch.to_ascii_lowercase());
      wrote_alnum = true;
      pending_dash = false;
    } else if wrote_alnum {
      pending_dash = true;
    }
  }

  if !wrote_alnum {
    output.push_str("path");
  }
}

fn sanitized_path_component_len(value: &str) -> usize {
  let mut len = 0;
  let mut wrote_alnum = false;
  let mut pending_dash = false;

  for ch in value.chars() {
    if ch.is_ascii_alphanumeric() || ch == '_' {
      if wrote_alnum && pending_dash {
        len += 1;
      }
      len += 1;
      wrote_alnum = true;
      pending_dash = false;
    } else if wrote_alnum {
      pending_dash = true;
    }
  }

  if wrote_alnum { len } else { "path".len() }
}

fn truncate_ascii_prefix(value: &str, max_bytes: usize) -> &str {
  if value.len() <= max_bytes {
    return value;
  }

  value.get(..max_bytes).unwrap_or(value)
}

#[cfg(test)]
fn json_escape(value: &str) -> String {
  let mut output = String::with_capacity(json_escaped_len(value));
  push_json_escape(&mut output, value);
  output
}

#[cfg(test)]
fn push_json_escape(output: &mut String, value: &str) {
  let mut chunk_start = 0;
  for (index, byte) in value.as_bytes().iter().copied().enumerate() {
    if json_escape_extra_len_for_byte(byte) == 0 {
      continue;
    }

    if chunk_start < index {
      output.push_str(&value[chunk_start..index]);
    }
    push_json_escaped_byte(output, byte);
    chunk_start = index + 1;
  }

  if chunk_start < value.len() {
    output.push_str(&value[chunk_start..]);
  }
}

#[cfg(test)]
fn json_escape_path(path: &Path) -> String {
  let mut output = String::with_capacity(json_escaped_path_len(path));
  push_json_escape_path(&mut output, path);
  output
}

fn push_json_escape_path(output: &mut String, path: &Path) {
  let path = runtime_path_str(path);
  push_json_escape_metadata_path(output, path.as_ref());
}

fn json_escaped_path_len(path: &Path) -> usize {
  let path = runtime_path_str(path);
  json_escaped_metadata_path_len(path.as_ref())
}

#[cfg(test)]
fn json_escaped_len(value: &str) -> usize {
  value.len() + json_escape_extra_len(value)
}

#[cfg(test)]
fn json_escape_extra_len(value: &str) -> usize {
  value.as_bytes().iter().copied().map(json_escape_extra_len_for_byte).sum()
}

fn json_escaped_metadata_path_len(value: &str) -> usize {
  value.len() + json_escape_metadata_path_extra_len(value)
}

fn json_escape_metadata_path_extra_len(value: &str) -> usize {
  value.as_bytes().iter().copied().map(json_escape_metadata_path_extra_len_for_byte).sum()
}

fn json_escape_metadata_path_extra_len_for_byte(byte: u8) -> usize {
  if cfg!(windows) && byte == b'\\' { 0 } else { json_escape_extra_len_for_byte(byte) }
}

fn json_escape_extra_len_for_byte(byte: u8) -> usize {
  match byte {
    b'"' | b'\\' | b'\n' | b'\r' | b'\t' => 1,
    0..=0x1F => 5,
    _ => 0,
  }
}

fn push_json_escape_metadata_path(output: &mut String, value: &str) {
  let mut chunk_start = 0;
  for (index, byte) in value.as_bytes().iter().copied().enumerate() {
    if cfg!(windows) && byte == b'\\' {
      if chunk_start < index {
        output.push_str(&value[chunk_start..index]);
      }
      output.push('/');
      chunk_start = index + 1;
      continue;
    }

    if json_escape_extra_len_for_byte(byte) == 0 {
      continue;
    }

    if chunk_start < index {
      output.push_str(&value[chunk_start..index]);
    }
    push_json_escaped_byte(output, byte);
    chunk_start = index + 1;
  }

  if chunk_start < value.len() {
    output.push_str(&value[chunk_start..]);
  }
}

fn push_json_escaped_byte(output: &mut String, byte: u8) {
  match byte {
    b'"' => output.push_str("\\\""),
    b'\\' => output.push_str("\\\\"),
    b'\n' => output.push_str("\\n"),
    b'\r' => output.push_str("\\r"),
    b'\t' => output.push_str("\\t"),
    0..=0x1F => {
      const HEX: &[u8; 16] = b"0123456789abcdef";
      output.push_str("\\u00");
      output.push(HEX[(byte >> 4) as usize] as char);
      output.push(HEX[(byte & 0x0F) as usize] as char);
    }
    _ => unreachable!("only JSON-escaped bytes should be pushed"),
  }
}

fn decimal_usize_len(value: usize) -> usize {
  decimal_u128_len(value as u128)
}

fn decimal_u128_len(mut value: u128) -> usize {
  let mut len = 1;
  while value >= 10 {
    value /= 10;
    len += 1;
  }
  len
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    ffi::OsString,
    fs,
    time::{SystemTime, UNIX_EPOCH},
  };

  #[test]
  fn config_is_disabled_without_env_value() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, None);

    assert!(!config.enabled);
    assert_eq!(config.root, cwd.join(".dx").join("rolldown"));
  }

  #[test]
  fn config_is_disabled_for_falsey_env_values() {
    let cwd = Path::new(r"G:\Dx\build");

    for value in ["", "0", "false", "FALSE", "off", "no"] {
      let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from(value)));
      assert!(!config.enabled, "{value:?} should not enable the DX machine cache");
      assert_eq!(config.root, cwd.join(".dx").join("rolldown"));
    }
  }

  #[test]
  fn env_value_parser_handles_trimmed_mixed_case_falsey_values_without_owned_lowercase() {
    assert!(!env_value_enables_dx_machine_cache(" \tOFF\r\n"));
    assert!(!env_value_enables_dx_machine_cache(" \tFaLsE\r\n"));
    assert!(env_value_enables_dx_machine_cache(" \t1\r\n"));
  }

  #[test]
  fn env_value_parser_disables_all_trimmed_falsey_spellings() {
    for value in ["   ", " 0 ", "\tfalse\n", "\tFALSE\n", " off ", " OFF ", " no ", " NO "] {
      assert!(!env_value_enables_dx_machine_cache(value), "{value:?} should disable cache");
    }

    for value in ["1", "true", "yes", "enabled", "random"] {
      assert!(env_value_enables_dx_machine_cache(value), "{value:?} should enable cache");
    }
  }

  #[test]
  fn unsynced_cache_write_env_defaults_to_sync_writes() {
    assert!(DxMachineCacheWriteOptions::from_env_value(None).sync_writes);
  }

  #[test]
  fn unsynced_cache_write_env_accepts_truthy_values() {
    for value in ["1", "true", "yes", "enabled", "random"] {
      assert!(
        !DxMachineCacheWriteOptions::from_env_value(Some(OsString::from(value))).sync_writes,
        "{value:?} should skip cache write sync_all"
      );
    }
  }

  #[test]
  fn unsynced_cache_write_env_rejects_falsey_values() {
    for value in ["", " ", "0", "false", "FALSE", "off", "OFF", "no", "NO"] {
      assert!(
        DxMachineCacheWriteOptions::from_env_value(Some(OsString::from(value))).sync_writes,
        "{value:?} should keep cache write sync_all"
      );
    }
  }

  #[test]
  fn config_if_enabled_is_none_for_missing_and_falsey_env_values() {
    let cwd = Path::new(r"G:\Dx\build");

    assert!(DxMachineCacheConfig::from_env_value_if_enabled(cwd, None).is_none());
    for value in ["", "0", "false", "FALSE", "off", "no"] {
      assert!(
        DxMachineCacheConfig::from_env_value_if_enabled(cwd, Some(OsString::from(value))).is_none(),
        "{value:?} should skip DX machine cache config creation"
      );
    }
  }

  #[test]
  fn config_if_enabled_builds_project_cache_root_for_truthy_env_value() {
    let cwd = Path::new(r"G:\Dx\build");
    let config =
      DxMachineCacheConfig::from_env_value_if_enabled(cwd, Some(OsString::from("1"))).unwrap();

    assert!(config.enabled);
    assert_eq!(config.root, cwd.join(".dx").join("rolldown"));
  }

  #[test]
  fn config_uses_project_cache_root_when_enabled() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));

    assert!(config.enabled);
    assert_eq!(config.root, cwd.join(".dx").join("rolldown"));
  }

  #[test]
  fn paths_keep_namespace_and_relative_source_identity() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(
      cwd,
      "package_json",
      &cwd.join("node_modules").join("@scope").join("pkg").join("package.json"),
    );
    let machine_name = paths.machine.file_name().unwrap().to_string_lossy();
    let metadata_name = paths.metadata.file_name().unwrap().to_string_lossy();

    assert_eq!(paths.machine.parent().unwrap(), cwd.join(".dx").join("rolldown"));
    assert!(machine_name.starts_with("package_json__node_modules-scope-pkg-package-json--"));
    assert!(machine_name.ends_with(".machine"));
    assert!(metadata_name.starts_with("package_json__node_modules-scope-pkg-package-json--"));
    assert!(metadata_name.ends_with(".machine.meta.json"));
  }

  #[test]
  fn paths_for_source_if_enabled_skips_disabled_cache() {
    let cwd = Path::new(r"G:\Dx\build");
    let source = cwd.join("package.json");
    let disabled_config = DxMachineCacheConfig::from_env_value(cwd, None);
    let enabled_config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));

    assert!(disabled_config.paths_for_source_if_enabled(cwd, "package_json", &source).is_none());
    assert_eq!(
      enabled_config.paths_for_source_if_enabled(cwd, "package_json", &source),
      Some(enabled_config.paths_for_source(cwd, "package_json", &source))
    );
  }

  #[test]
  fn paths_do_not_collide_for_different_sources_with_same_sanitized_name() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));
    let first = config.paths_for_source(cwd, "json_module", &cwd.join("src").join("foo@bar.json"));
    let second = config.paths_for_source(cwd, "json_module", &cwd.join("src").join("foo-bar.json"));

    assert_ne!(first.machine, second.machine);
    assert_ne!(first.metadata, second.metadata);
  }

  #[test]
  fn paths_cap_deep_source_names_for_windows_cache_compatibility() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));
    let mut source_path = cwd.join("node_modules");
    for index in 0..40 {
      source_path = source_path.join(format!("very-long-package-name-segment-{index}"));
    }
    source_path = source_path.join("package.json");

    let paths = config.paths_for_source(cwd, "package_json_metadata", &source_path);
    let machine_name = paths.machine.file_name().unwrap().to_string_lossy();
    let metadata_name = paths.metadata.file_name().unwrap().to_string_lossy();

    assert!(
      machine_name.len() <= 180,
      "machine cache file name should stay Windows-friendly, got {} bytes",
      machine_name.len()
    );
    assert!(
      metadata_name.len() <= 190,
      "metadata cache file name should stay Windows-friendly, got {} bytes",
      metadata_name.len()
    );
    assert!(machine_name.ends_with(".machine"));
    assert!(metadata_name.ends_with(".machine.meta.json"));
  }

  #[test]
  fn flatten_path_preallocates_exact_len_without_component_vec() {
    let path = Path::new(r"node_modules\@scope/pkg/.\package.json");
    let flattened = flatten_path(path);

    assert_eq!(flattened, "node_modules-scope-pkg-package-json");
    assert_eq!(flatten_path_len(path), flattened.len());
    assert_eq!(flattened.capacity(), flattened.len());
  }

  #[test]
  fn sanitize_path_component_borrows_already_sanitized_namespace() {
    assert!(matches!(
      sanitize_path_component("package_json_metadata"),
      Cow::Borrowed("package_json_metadata")
    ));
    assert!(matches!(sanitize_path_component("json_module"), Cow::Borrowed("json_module")));
    assert!(matches!(
      sanitize_path_component("Package JSON"),
      Cow::Owned(value) if value == "package-json"
    ));
    assert!(matches!(sanitize_path_component(""), Cow::Owned(value) if value == "path"));
  }

  #[test]
  fn cache_stem_preallocates_exact_len_without_prefix_suffix_format_growth() {
    let path = Path::new(r"node_modules\@scope/pkg/package.json");
    let stem = cache_stem("package_json_metadata", path);

    assert!(stem.starts_with("package_json_metadata__node_modules-scope-pkg-package-json--"));
    assert_eq!(cache_stem_len("package_json_metadata", path), stem.len());
    assert_eq!(stem.capacity(), stem.len());
  }

  #[test]
  fn cache_artifact_file_name_preallocates_exact_len_without_format_growth() {
    let file_name = cache_artifact_file_name("package-json--abcdef123456", ".machine.meta.json");

    assert_eq!(file_name, "package-json--abcdef123456.machine.meta.json");
    assert_eq!(file_name.capacity(), file_name.len());
  }

  #[test]
  fn paths_for_source_preserves_machine_and_metadata_file_names() {
    let cwd = Path::new(r"G:\Dx\build");
    let config = DxMachineCacheConfig::from_env_value(cwd, Some(OsString::from("1")));
    let source_path = cwd.join("src").join("data.json");
    let relative_path = source_path.strip_prefix(cwd).unwrap();
    let stem = cache_stem("json_module", relative_path);

    let paths = config.paths_for_source(cwd, "json_module", &source_path);

    assert_eq!(paths.machine.parent(), Some(config.root.as_path()));
    assert_eq!(paths.metadata.parent(), Some(config.root.as_path()));
    assert_eq!(
      paths.machine.file_name().and_then(|value| value.to_str()),
      Some(cache_artifact_file_name(&stem, ".machine").as_str())
    );
    assert_eq!(
      paths.metadata.file_name().and_then(|value| value.to_str()),
      Some(cache_artifact_file_name(&stem, ".machine.meta.json").as_str())
    );
  }

  #[test]
  fn cache_stem_caps_overlong_namespace_with_source_fallback() {
    let namespace = "very_long_namespace_segment_".repeat(20);
    let stem = cache_stem(&namespace, Path::new("package.json"));

    assert!(stem.len() <= DX_MACHINE_CACHE_STEM_MAX_BYTES);
    assert!(stem.contains("__source--"));
    assert_eq!(cache_stem_len(&namespace, Path::new("package.json")), stem.len());
    assert_eq!(stem.capacity(), stem.len());
  }

  #[test]
  fn cache_path_identity_matches_flattened_path_and_hash_without_extra_identity_string() {
    for path in [
      Path::new(""),
      Path::new(r"node_modules\@scope/pkg/.\package.json"),
      Path::new(r"src\Data\countries.JSON"),
    ] {
      let identity = cache_path_identity(path);

      assert_eq!(identity.flattened_path, flatten_path(path));
      assert_eq!(identity.flattened_path.capacity(), identity.flattened_path.len());
      assert_eq!(identity.path_hash, path_identity_hash(path));
    }
  }

  #[test]
  fn path_identity_hash_is_stable_across_separator_spellings() {
    assert_eq!(
      path_identity_hash(Path::new(r"src\data.json")),
      path_identity_hash(Path::new("src/data.json"))
    );
  }

  #[test]
  fn path_identity_digest_matches_materialized_identity_without_identity_string_allocation() {
    for path in [
      Path::new(""),
      Path::new(r"node_modules\@scope/pkg/.\package.json"),
      Path::new(r"src\Data\countries.JSON"),
    ] {
      let materialized = path_identity(path);
      let expected = blake3::hash(materialized.as_bytes());

      assert_eq!(path_identity_digest(path), expected);
      assert_eq!(path_identity_hash(path), short_blake3_hex(materialized.as_bytes()));
    }
  }

  #[test]
  fn path_identity_digest_normalizes_backslashes_inside_component_values() {
    let mut hasher = blake3::Hasher::new();
    update_path_identity_component(&mut hasher, r"Pkg\Name.JSON");

    assert_eq!(hasher.finalize(), blake3::hash(b"pkg/name.json"));
  }

  #[test]
  fn cache_stem_suffix_matches_streamed_path_identity_hash() {
    let path = Path::new(r"node_modules\@scope/pkg/.\package.json");
    let stem = cache_stem("package_json_metadata", path);
    let expected_suffix = format!("--{}", path_identity_hash(path));

    assert!(stem.ends_with(&expected_suffix));
  }

  #[test]
  fn path_identity_preallocates_exact_len_without_component_vec() {
    let path = Path::new(r"node_modules\@scope/pkg/.\package.json");
    let identity = path_identity(path);

    assert_eq!(identity, "node_modules/@scope/pkg/package.json");
    assert_eq!(path_identity_len(path), identity.len());
    assert_eq!(identity.capacity(), identity.len());
  }

  #[test]
  fn short_blake3_hex_matches_digest_prefix_without_full_hex_allocation() {
    let input = b"src/data.json";
    let expected = blake3::hash(input).to_hex().to_string();
    let short = short_blake3_hex(input);

    assert_eq!(short, &expected[..12]);
    assert_eq!(short.len(), 12);
    assert_eq!(short.capacity(), 12);
  }

  #[test]
  fn push_blake3_hex_appends_lowercase_digest_without_to_hex_output() {
    let hash = blake3::hash(b"metadata bytes");
    let expected = hash.to_hex().to_string();
    let mut output = String::with_capacity("source:".len() + BLAKE3_HEX_LEN + ";".len());

    output.push_str("source:");
    push_blake3_hex(&mut output, &hash);
    output.push(';');

    assert_eq!(output, format!("source:{expected};"));
    assert_eq!(output.capacity(), output.len());
  }

  #[test]
  fn push_decimal_json_appends_metadata_numbers_without_format_write() {
    let mut bytes_output = String::with_capacity("bytes:".len() + decimal_usize_len(123_456));
    bytes_output.push_str("bytes:");
    push_decimal_usize_json(&mut bytes_output, 123_456);

    assert_eq!(bytes_output, "bytes:123456");
    assert_eq!(bytes_output.capacity(), bytes_output.len());

    let modified = ModifiedUnixMsJson(Some(1_780_050_000_000));
    let mut modified_output = String::with_capacity("modified:".len() + modified.json_len());
    modified_output.push_str("modified:");
    push_modified_unix_ms_json(&mut modified_output, &modified);

    assert_eq!(modified_output, "modified:1780050000000");
    assert_eq!(modified_output.capacity(), modified_output.len());

    let missing = ModifiedUnixMsJson(None);
    let mut missing_output = String::with_capacity("modified:".len() + missing.json_len());
    missing_output.push_str("modified:");
    push_modified_unix_ms_json(&mut missing_output, &missing);

    assert_eq!(missing_output, "modified:null");
    assert_eq!(missing_output.capacity(), missing_output.len());
  }

  #[test]
  fn blake3_hex_matches_bytes_without_full_hex_allocation() {
    let input = b"machine bytes";
    let expected = blake3::hash(input).to_hex().to_string();

    assert!(blake3_hex_matches_bytes(&expected, input));
    assert!(!blake3_hex_matches_bytes(&expected, b"changed machine bytes"));
    assert!(!blake3_hex_matches_bytes(&expected.to_ascii_uppercase(), input));
    assert!(!blake3_hex_matches_bytes("not-a-blake3-hash", input));
  }

  #[test]
  fn validate_metadata_entry_bytes_with_known_hash_shape_checks_len_and_hash() {
    let bytes = b"machine bytes";
    let hash = blake3::hash(bytes).to_hex().to_string();
    let metadata = MachineMetadataEntry {
      path: Cow::Borrowed(r"G:\Dx\build\.dx\rolldown\machine.machine"),
      bytes: bytes.len() as u64,
      blake3: &hash,
    };

    assert!(validate_metadata_entry_bytes_with_known_hash_shape(&metadata, bytes));
    assert!(!validate_metadata_entry_bytes_with_known_hash_shape(&metadata, b"machine bytez"));

    let wrong_len_metadata = MachineMetadataEntry {
      path: metadata.path.clone(),
      bytes: bytes.len() as u64 + 1,
      blake3: &hash,
    };
    assert!(!validate_metadata_entry_bytes_with_known_hash_shape(&wrong_len_metadata, bytes));
  }

  #[test]
  fn read_file_with_max_len_rejects_oversized_file() {
    let root = unique_temp_root("read-max-len");
    let path = root.join("metadata.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"1234").unwrap();

    assert_eq!(read_file_with_max_len(&path, 3), DxMachineCacheStatus::Invalid);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_file_bytes_with_limit_does_not_reserve_untrusted_initial_capacity() {
    let root = unique_temp_root("read-prealloc-cap");
    let path = root.join("machine.bin");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"1234").unwrap();

    let file = fs::File::open(&path).unwrap();

    assert_eq!(read_file_bytes_with_limit(file, u64::MAX, 3), Ok(None));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_file_bytes_with_limit_and_hash_hashes_large_reads_with_hashing_reader() {
    let root = unique_temp_root("read-hashing-reader");
    let path = root.join("machine.bin");
    let bytes = vec![b'x'; DX_MACHINE_READ_PREALLOC_MAX_BYTES as usize + 4096];
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();

    let file = fs::File::open(&path).unwrap();

    reset_hashing_reader_read_call_count();
    let read = read_file_bytes_with_limit_and_hash(file, bytes.len() as u64, bytes.len() as u64)
      .unwrap()
      .unwrap();

    assert_eq!(read.bytes, bytes);
    assert_eq!(read.hash, blake3::hash(&read.bytes));
    assert!(hashing_reader_read_call_count() > 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_file_bytes_with_limit_and_hash_hashes_small_exact_reads_without_hashing_reader() {
    let root = unique_temp_root("read-small-exact-hash");
    let path = root.join("machine.bin");
    let bytes = b"machine bytes";
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();

    let file = fs::File::open(&path).unwrap();

    reset_hashing_reader_read_call_count();
    let read = read_file_bytes_with_limit_and_hash(file, bytes.len() as u64, bytes.len() as u64)
      .unwrap()
      .unwrap();

    assert_eq!(read.bytes, bytes);
    assert_eq!(read.hash, blake3::hash(bytes));
    assert_eq!(hashing_reader_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_file_bytes_with_limit_and_hash_hashes_small_exact_reads_while_reading() {
    let root = unique_temp_root("read-small-exact-hash-inline");
    let path = root.join("machine.bin");
    let bytes = b"machine bytes";
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();

    let file = fs::File::open(&path).unwrap();

    reset_hashing_reader_read_call_count();
    reset_small_exact_inline_hash_update_count();
    let read = read_file_bytes_with_limit_and_hash(file, bytes.len() as u64, bytes.len() as u64)
      .unwrap()
      .unwrap();

    assert_eq!(read.bytes, bytes);
    assert_eq!(read.hash, blake3::hash(bytes));
    assert_eq!(hashing_reader_read_call_count(), 0);
    assert!(small_exact_inline_hash_update_count() > 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn atomic_temp_file_name_preallocates_exact_len_without_format_growth() {
    let file_name = atomic_temp_file_name("package-json.machine", 12_345, 67_890);

    assert_eq!(file_name, "package-json.machine.12345.67890.tmp");
    assert_eq!(file_name.capacity(), file_name.len());
  }

  #[test]
  fn atomic_temp_paths_do_not_collide_for_parallel_cache_writers() {
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");

    let first = atomic_temp_path(machine_path);
    let second = atomic_temp_path(machine_path);

    assert_ne!(first, second);
    assert_eq!(first.parent(), machine_path.parent());
    assert_eq!(second.parent(), machine_path.parent());
    let process_marker = format!(".{}.", std::process::id());
    assert!(
      first.file_name().unwrap().to_string_lossy().starts_with("package-json.machine."),
      "temp file should keep the cache artifact name visible"
    );
    assert!(first.file_name().unwrap().to_string_lossy().contains(&process_marker));
    assert!(second.file_name().unwrap().to_string_lossy().contains(&process_marker));
    assert!(first.file_name().unwrap().to_string_lossy().ends_with(".tmp"));
    assert!(second.file_name().unwrap().to_string_lossy().ends_with(".tmp"));
  }

  #[test]
  fn write_atomic_skips_identical_readonly_cache_file() {
    let root = unique_temp_root("readonly-identical");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();
    set_readonly(&path, true);

    let result = write_atomic(&path, b"same machine bytes");

    set_readonly(&path, false);
    assert!(result.is_ok(), "identical cache write should not rewrite read-only file: {result:?}");
    assert_eq!(fs::read(&path).unwrap(), b"same machine bytes");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_skips_identical_cache_file_without_temp_write() {
    let root = unique_temp_root("identical-no-temp");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();
    let temp_calls_before = temp_path_call_count();

    write_atomic(&path, b"same machine bytes").unwrap();

    assert_eq!(temp_path_call_count(), temp_calls_before);
    assert_eq!(fs::read(&path).unwrap(), b"same machine bytes");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_skips_parent_dir_create_for_identical_cache_file() {
    let root = unique_temp_root("identical-no-parent-create");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();

    reset_parent_dir_create_call_count();
    write_atomic(&path, b"same machine bytes").unwrap();

    assert_eq!(parent_dir_create_call_count(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"same machine bytes");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_skips_identical_cache_file_without_path_metadata_probe() {
    let root = unique_temp_root("identical-no-path-metadata");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();

    reset_existing_file_path_metadata_call_count();
    write_atomic(&path, b"same machine bytes").unwrap();

    assert_eq!(existing_file_path_metadata_call_count(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"same machine bytes");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_skips_identical_cache_file_without_file_metadata_probe() {
    let root = unique_temp_root("identical-no-file-metadata");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();

    reset_existing_file_handle_metadata_call_count();
    write_atomic(&path, b"same machine bytes").unwrap();

    assert_eq!(existing_file_handle_metadata_call_count(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"same machine bytes");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_rewrites_same_len_changed_cache_file() {
    let root = unique_temp_root("same-len-changed-write");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();
    assert_eq!(b"same machine bytes".len(), b"changed bytes data".len());
    let temp_calls_before = temp_path_call_count();

    write_atomic(&path, b"changed bytes data").unwrap();

    assert!(temp_path_call_count() > temp_calls_before);
    assert_eq!(fs::read(&path).unwrap(), b"changed bytes data");

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_rewrites_changed_cache_file_without_existing_target_probe() {
    let root = unique_temp_root("changed-write-no-target-probe");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();

    reset_target_exists_probe_call_count();
    write_atomic(&path, b"changed bytes data").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"changed bytes data");
    assert_eq!(target_exists_probe_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_atomic_rewrites_changed_cache_file_without_temp_exists_probe() {
    let root = unique_temp_root("changed-write-no-temp-probe");
    let path = root.join("artifact.machine");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"same machine bytes").unwrap();

    reset_temp_exists_probe_call_count();
    write_atomic(&path, b"changed bytes data").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"changed bytes data");
    assert_eq!(temp_exists_probe_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn reader_matches_bytes_short_circuits_on_first_mismatch() {
    struct OneByteReader<'a> {
      bytes: &'a [u8],
      offset: usize,
      reads: usize,
    }

    impl io::Read for OneByteReader<'_> {
      fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset == self.bytes.len() {
          return Ok(0);
        }

        buf[0] = self.bytes[self.offset];
        self.offset += 1;
        self.reads += 1;
        Ok(1)
      }
    }

    let mut reader = OneByteReader { bytes: b"xame machine bytes", offset: 0, reads: 0 };

    assert!(!reader_matches_bytes(&mut reader, b"same machine bytes").unwrap());
    assert_eq!(reader.reads, 1);
  }

  #[test]
  fn reader_matches_bytes_rejects_short_and_trailing_reads() {
    let mut short_reader = io::Cursor::new(b"same machine".as_slice());
    assert!(!reader_matches_bytes(&mut short_reader, b"same machine bytes").unwrap());

    let mut trailing_reader = io::Cursor::new(b"same machine bytes plus trailing".as_slice());
    assert!(!reader_matches_bytes(&mut trailing_reader, b"same machine bytes").unwrap());
  }

  #[test]
  fn metadata_validation_hits_when_source_and_machine_hashes_match() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    assert_eq!(
      validate_machine_metadata(&metadata, source_path, source_bytes, machine_path, machine_bytes),
      DxMachineCacheStatus::Hit(())
    );
  }

  #[test]
  fn metadata_document_deserializes_borrowed_fields() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    let document = parse_machine_metadata_document(metadata.as_bytes()).unwrap();

    assert_eq!(document.schema, DX_MACHINE_METADATA_SCHEMA);
    assert_eq!(
      document.source.path.as_ref(),
      metadata_path_text_for_assertion(source_path.display().to_string().as_ref())
    );
    assert_eq!(document.source.bytes, source_bytes.len() as u64);
    assert_eq!(document.machine.bytes, machine_bytes.len() as u64);
    assert!(document.cache.rebuildable);
    assert!(document.cache.fallback_on_mismatch);
  }

  #[test]
  fn metadata_document_parses_canonical_writer_output_without_serde_fallback() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    reset_metadata_serde_parse_count();
    let document = parse_machine_metadata_document(metadata.as_bytes()).unwrap();

    assert_eq!(
      document.source.path.as_ref(),
      metadata_path_text_for_assertion(source_path.display().to_string().as_ref())
    );
    assert_eq!(document.source.blake3, blake3::hash(source_bytes).to_hex().as_str());
    assert_eq!(
      document.machine.path.as_ref(),
      metadata_path_text_for_assertion(machine_path.display().to_string().as_ref())
    );
    assert_eq!(document.machine.blake3, blake3::hash(machine_bytes).to_hex().as_str());
    assert_eq!(metadata_serde_parse_count(), 0);
  }

  #[cfg(windows)]
  #[test]
  fn metadata_document_parses_common_windows_writer_paths_as_borrowed_slashes() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    let document = parse_machine_metadata_document(metadata.as_bytes()).unwrap();

    assert!(matches!(document.source.path, Cow::Borrowed(_)));
    assert!(matches!(document.machine.path, Cow::Borrowed(_)));
    assert_eq!(document.source.path.as_ref(), "G:/Dx/build/package.json");
    assert_eq!(document.machine.path.as_ref(), "G:/Dx/build/.dx/rolldown/package-json.machine");
  }

  #[test]
  fn metadata_document_parses_canonical_hashes_without_revalidating_utf8() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    let document = parse_machine_metadata_document(metadata.as_bytes()).unwrap();
    let metadata_base = metadata.as_ptr() as usize;
    let metadata_end = metadata_base + metadata.len();
    let source_hash = blake3::hash(source_bytes).to_hex().to_string();
    let machine_hash = blake3::hash(machine_bytes).to_hex().to_string();
    let source_hash_start = metadata.find(&source_hash).unwrap();
    let machine_hash_start = metadata.find(&machine_hash).unwrap();
    let source_hash_ptr = document.source.blake3.as_ptr() as usize;
    let machine_hash_ptr = document.machine.blake3.as_ptr() as usize;

    assert_eq!(document.source.blake3, source_hash);
    assert_eq!(document.machine.blake3, machine_hash);
    assert!(source_hash_ptr >= metadata_base);
    assert!(source_hash_ptr + BLAKE3_HEX_LEN <= metadata_end);
    assert!(machine_hash_ptr >= metadata_base);
    assert!(machine_hash_ptr + BLAKE3_HEX_LEN <= metadata_end);
    assert_eq!(source_hash_ptr - metadata_base, source_hash_start);
    assert_eq!(machine_hash_ptr - metadata_base, machine_hash_start);
  }

  #[test]
  fn modified_unix_ms_json_formats_millis_or_null_without_temp_string() {
    assert_eq!(ModifiedUnixMsJson(Some(1234)).to_string(), "1234");
    assert_eq!(ModifiedUnixMsJson(None).to_string(), "null");
  }

  #[test]
  fn machine_metadata_json_writes_null_modified_unix_ms_for_missing_source() {
    let source_path = Path::new(r"G:\Dx\build\missing-package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let metadata = machine_metadata_json(source_path, b"{}", machine_path, b"machine bytes");

    assert!(metadata.contains("    \"modified_unix_ms\": null,\n"));
  }

  #[test]
  fn machine_metadata_json_direct_writer_matches_previous_format_output_for_missing_source() {
    let source_path = Path::new("G:\\Dx\\build\\quote\"tab\tline\nmissing-package.json");
    let machine_path = Path::new("G:\\Dx\\build\\.dx\\rolldown\\quote\"machine.machine");
    let source_bytes = b"{}";
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    let expected = format!(
      concat!(
        "{{\n",
        "  \"schema\": \"dx.machine.source_metadata.v1\",\n",
        "  \"source\": {{\n",
        "    \"path\": \"{}\",\n",
        "    \"bytes\": 2,\n",
        "    \"modified_unix_ms\": null,\n",
        "    \"blake3\": \"{}\"\n",
        "  }},\n",
        "  \"machine\": {{\n",
        "    \"path\": \"{}\",\n",
        "    \"bytes\": 7,\n",
        "    \"blake3\": \"{}\"\n",
        "  }},\n",
        "  \"cache\": {{\n",
        "    \"rebuildable\": true,\n",
        "    \"fallback_on_mismatch\": true\n",
        "  }}\n",
        "}}\n"
      ),
      json_escape_path(source_path),
      blake3::hash(source_bytes).to_hex(),
      json_escape_path(machine_path),
      blake3::hash(machine_bytes).to_hex()
    );

    assert_eq!(metadata, expected);
  }

  #[test]
  fn machine_metadata_json_preallocates_exact_len_for_missing_source() {
    let source_path = Path::new("G:\\Dx\\build\\quote\"tab\tline\nmissing-package.json");
    let machine_path = Path::new("G:\\Dx\\build\\.dx\\rolldown\\quote\"machine.machine");
    let metadata = machine_metadata_json(source_path, b"{}", machine_path, b"machine bytes");

    assert_eq!(metadata.capacity(), metadata.len());
  }

  #[test]
  fn json_escape_extra_len_counts_only_ascii_escape_bytes() {
    let raw = "plain🚀\\quote\"tab\tline\ncarriage\r";

    assert_eq!(json_escape_extra_len(raw), 5);
    assert_eq!(json_escaped_len(raw), raw.len() + 5);
  }

  #[test]
  fn json_escape_preallocates_exact_escaped_len() {
    let raw = "G:\\Dx\\build\\quote\"tab\tline\npackage.json";
    let escaped = json_escape(raw);

    assert_eq!(escaped, "G:\\\\Dx\\\\build\\\\quote\\\"tab\\tline\\npackage.json");
    assert_eq!(json_escaped_len(raw), escaped.len());
    assert_eq!(escaped.capacity(), escaped.len());
  }

  #[test]
  fn machine_metadata_json_escapes_non_whitespace_control_paths_as_valid_json() {
    let source_path = Path::new("G:\\Dx\\build\\control\u{0008}\u{001f}.json");
    let machine_path = Path::new("G:\\Dx\\build\\.dx\\rolldown\\control\u{000c}.machine");
    let escaped = json_escape("control\u{0008}\u{001f}.json");

    assert_eq!(escaped, "control\\u0008\\u001f.json");
    assert_eq!(json_escaped_len("control\u{0008}\u{001f}.json"), escaped.len());
    assert_eq!(escaped.capacity(), escaped.len());

    let metadata = machine_metadata_json(source_path, b"{}", machine_path, b"machine bytes");
    assert!(
      parse_machine_metadata_document(metadata.as_bytes()).is_some(),
      "metadata with non-whitespace ASCII controls in paths must remain valid JSON"
    );
  }

  #[test]
  fn json_escape_preserves_unicode_chunks_while_escaping_ascii_bytes() {
    let raw = "G:\\Dx\\🚀\\quote\"tab\tline\npackage.json";
    let escaped = json_escape(raw);

    assert_eq!(escaped, "G:\\\\Dx\\\\🚀\\\\quote\\\"tab\\tline\\npackage.json");
    assert_eq!(json_escaped_len(raw), escaped.len());
    assert_eq!(escaped.capacity(), escaped.len());
  }

  #[test]
  fn json_escape_path_borrows_valid_unicode_path_text_before_escaping() {
    let path = Path::new("G:\\Dx\\build\\quote\"tab\tline\npackage.json");
    let escaped = json_escape_path(path);

    assert_eq!(
      escaped,
      json_escape_metadata_path_text_for_assertion("G:\\Dx\\build\\quote\"tab\tline\npackage.json")
    );
    assert_eq!(escaped.capacity(), escaped.len());
  }

  #[test]
  fn push_json_escape_path_matches_json_escape_path_without_intermediate_string() {
    let path = Path::new("G:\\Dx\\build\\quote\"tab\tline\npackage.json");
    let mut escaped = String::with_capacity(json_escaped_path_len(path));

    push_json_escape_path(&mut escaped, path);

    assert_eq!(escaped, json_escape_path(path));
    assert_eq!(escaped.capacity(), escaped.len());
  }

  #[cfg(windows)]
  #[test]
  fn json_escape_path_falls_back_to_display_for_non_utf_paths() {
    use std::os::windows::ffi::OsStringExt;

    let path = PathBuf::from(OsString::from_wide(&[
      b'G' as u16,
      b':' as u16,
      b'\\' as u16,
      0xD800,
      b'\\' as u16,
      b'q' as u16,
      b'"' as u16,
      b'.' as u16,
      b'j' as u16,
      b's' as u16,
      b'o' as u16,
      b'n' as u16,
    ]));

    assert_eq!(
      json_escape_path(&path),
      json_escape_metadata_path_text_for_assertion(path.display().to_string().as_ref())
    );
  }

  #[test]
  fn metadata_validation_accepts_equivalent_separator_spellings() {
    let metadata_source_path = Path::new("G:/Dx/build/src/data.json");
    let metadata_machine_path = Path::new("G:/Dx/build/.dx/rolldown/src-data-json.machine");
    let runtime_source_path = Path::new(r"G:\Dx\build\src\data.json");
    let runtime_machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\src-data-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(
      metadata_source_path,
      source_bytes,
      metadata_machine_path,
      machine_bytes,
    );

    assert_eq!(
      validate_machine_metadata(
        &metadata,
        runtime_source_path,
        source_bytes,
        runtime_machine_path,
        machine_bytes
      ),
      DxMachineCacheStatus::Hit(())
    );
  }

  #[test]
  fn metadata_validation_accepts_equivalent_dot_components() {
    let metadata_source_path = Path::new(r"G:\Dx\build\src\.\data.json");
    let metadata_machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\.\src-data-json.machine");
    let runtime_source_path = Path::new(r"G:\Dx\build\src\data.json");
    let runtime_machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\src-data-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(
      metadata_source_path,
      source_bytes,
      metadata_machine_path,
      machine_bytes,
    );

    assert_eq!(
      validate_machine_metadata(
        &metadata,
        runtime_source_path,
        source_bytes,
        runtime_machine_path,
        machine_bytes
      ),
      DxMachineCacheStatus::Hit(())
    );
  }

  #[cfg(windows)]
  #[test]
  fn metadata_validation_accepts_windows_case_differences() {
    let metadata_source_path = Path::new(r"G:\Dx\build\SRC\Data.JSON");
    let metadata_machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\SRC-Data-JSON.machine");
    let runtime_source_path = Path::new(r"g:\dx\build\src\data.json");
    let runtime_machine_path = Path::new(r"g:\dx\build\.dx\rolldown\src-data-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(
      metadata_source_path,
      source_bytes,
      metadata_machine_path,
      machine_bytes,
    );

    assert_eq!(
      validate_machine_metadata(
        &metadata,
        runtime_source_path,
        source_bytes,
        runtime_machine_path,
        machine_bytes
      ),
      DxMachineCacheStatus::Hit(())
    );
  }

  #[test]
  fn metadata_path_matches_exact_path_without_identity_normalization() {
    let runtime_path = Path::new(r"G:\Dx\build\package.json");
    let metadata_path = runtime_path.display().to_string();

    reset_path_identity_call_count();

    assert!(metadata_path_matches(&metadata_path, runtime_path));
    assert_eq!(path_identity_call_count(), 0);
  }

  #[cfg(windows)]
  #[test]
  fn metadata_path_matches_windows_slash_form_writer_path_without_component_fallback() {
    reset_normalized_metadata_path_match_count();

    assert!(metadata_path_matches(
      "G:/Dx/build/src/data.json",
      Path::new(r"G:\Dx\build\src\data.json"),
    ));
    assert_eq!(normalized_metadata_path_match_count(), 0);

    reset_normalized_metadata_path_match_count();
    assert!(metadata_path_matches(
      "G:/Dx/build/./src/data.json",
      Path::new(r"G:\Dx\build\src\data.json"),
    ));
    assert_eq!(normalized_metadata_path_match_count(), 1);
  }

  #[test]
  fn runtime_path_str_borrows_valid_unicode_paths() {
    let runtime_path = Path::new(r"G:\Dx\build\package.json");
    let runtime_path_str = runtime_path_str(runtime_path);

    assert!(matches!(runtime_path_str, Cow::Borrowed(_)));
    assert_eq!(runtime_path_str.as_ref(), r"G:\Dx\build\package.json");
  }

  #[cfg(windows)]
  #[test]
  fn runtime_path_str_falls_back_to_display_for_non_utf_paths() {
    use std::os::windows::ffi::OsStringExt;

    let path = PathBuf::from(OsString::from_wide(&[
      b'G' as u16,
      b':' as u16,
      b'\\' as u16,
      0xD800,
      b'.' as u16,
      b'j' as u16,
      b's' as u16,
      b'o' as u16,
      b'n' as u16,
    ]));
    let runtime_path_str = runtime_path_str(&path);

    assert!(matches!(runtime_path_str, Cow::Owned(_)));
    assert_eq!(runtime_path_str.as_ref(), path.display().to_string());
  }

  #[test]
  fn normalize_metadata_path_components_preallocates_exact_len() {
    let raw = r"G:\Dx//build\.\src/./data.json";
    let normalized = normalize_metadata_path_components(raw);

    assert_eq!(normalized, "G:/Dx/build/src/data.json");
    assert_eq!(normalized_metadata_path_len(raw), normalized.len());
    assert_eq!(normalized.capacity(), normalized.len());
  }

  #[cfg(windows)]
  #[test]
  fn normalized_metadata_paths_match_windows_case_without_lowercase_identity_copy() {
    let left = r"G:\Dx\build\SRC\.\Data.JSON";
    let right = "g:/dx/build/src/data.json";
    let normalized = normalize_metadata_path_components(left);

    assert!(normalized_metadata_paths_match(left, right));
    assert_eq!(normalized, "G:/Dx/build/SRC/Data.JSON");
    assert_eq!(normalized.capacity(), normalized.len());
  }

  #[test]
  fn metadata_validation_invalidates_stale_source_hash() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    assert_eq!(
      validate_machine_metadata(
        &metadata,
        source_path,
        br#"{"name":"changedx"}"#,
        machine_path,
        machine_bytes
      ),
      DxMachineCacheStatus::Invalid
    );
  }

  #[test]
  fn metadata_validation_rejects_source_len_mismatch_before_hashing_source_bytes() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    reset_entry_hash_call_count();
    assert_eq!(
      validate_machine_metadata(
        &metadata,
        source_path,
        br#"{"name":"changed package with a different byte length"}"#,
        machine_path,
        machine_bytes
      ),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
  }

  #[test]
  fn metadata_validation_rejects_non_beneficial_machine_size_before_hashing_source_bytes() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes);

    reset_entry_hash_call_count();
    assert_eq!(
      validate_machine_metadata(&metadata, source_path, source_bytes, machine_path, machine_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
  }

  #[test]
  fn metadata_validation_rejects_stale_machine_hash() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let metadata =
      machine_metadata_json(source_path, source_bytes, machine_path, b"old machine bytes");

    assert_eq!(
      validate_machine_metadata(
        &metadata,
        source_path,
        source_bytes,
        machine_path,
        b"new machine bytes"
      ),
      DxMachineCacheStatus::Invalid
    );
  }

  #[test]
  fn metadata_validation_rejects_non_rebuildable_cache_policy() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes)
      .replace("\"rebuildable\": true", "\"rebuildable\": false");

    assert_eq!(
      validate_machine_metadata(&metadata, source_path, source_bytes, machine_path, machine_bytes),
      DxMachineCacheStatus::Invalid
    );
  }

  #[test]
  fn metadata_validation_rejects_cache_without_fallback_policy() {
    let source_path = Path::new(r"G:\Dx\build\package.json");
    let machine_path = Path::new(r"G:\Dx\build\.dx\rolldown\package-json.machine");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = b"machine";
    let metadata = machine_metadata_json(source_path, source_bytes, machine_path, machine_bytes)
      .replace("\"fallback_on_mismatch\": true", "\"fallback_on_mismatch\": false");

    assert_eq!(
      validate_machine_metadata(&metadata, source_path, source_bytes, machine_path, machine_bytes),
      DxMachineCacheStatus::Invalid
    );
  }

  #[test]
  fn read_validated_machine_falls_back_on_missing_files() {
    let root = unique_temp_root("missing");
    let cwd = root.join("project");
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &cwd.join("data.json"));

    assert_eq!(
      config.read_validated_machine(&paths, &cwd.join("data.json"), b"{}"),
      DxMachineCacheStatus::Miss
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_corrupt_metadata_bytes() {
    let root = unique_temp_root("corrupt-metadata-bytes");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(&paths.metadata, [0xff, b'{', b'}']).unwrap();

    assert_eq!(
      config.read_validated_machine(&paths, &source_path, br#"{"value":1}"#),
      DxMachineCacheStatus::Invalid
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_oversized_metadata_before_machine_lookup() {
    let root = unique_temp_root("oversized-metadata");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    let mut metadata =
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes);
    metadata.push_str(&" ".repeat(70_000));
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(&paths.metadata, metadata).unwrap();

    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_stale_source_before_machine_read() {
    let root = unique_temp_root("stale-source-before-machine-read");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let old_source_bytes = br#"{"value":1}"#;
    let new_source_bytes = br#"{"value":2}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, old_source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, new_source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_stale_source_before_machine_path_normalization() {
    let root = unique_temp_root("stale-source-before-machine-path-normalization");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let old_source_bytes = br#"{"value":1}"#;
    let new_source_bytes = br#"{"value":20}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    let metadata_machine_path =
      paths.machine.parent().unwrap().join(".").join(paths.machine.file_name().unwrap());
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, old_source_bytes, &metadata_machine_path, machine_bytes),
    )
    .unwrap();

    reset_normalized_metadata_path_match_count();
    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, new_source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(normalized_metadata_path_match_count(), 0);
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_declared_machine_bytes_above_cap_before_source_hash_and_read() {
    let root = unique_temp_root("oversized-machine-declared-before-read");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    let mut metadata =
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes);
    let declared_machine_bytes =
      format!("{MACHINE_METADATA_MACHINE_BYTES_PREFIX}{}", machine_bytes.len());
    let declared_machine_bytes_start = metadata.rfind(&declared_machine_bytes).unwrap();
    metadata.replace_range(
      declared_machine_bytes_start..declared_machine_bytes_start + declared_machine_bytes.len(),
      &format!("{MACHINE_METADATA_MACHINE_BYTES_PREFIX}{}", DX_MACHINE_MAX_BYTES + 1),
    );
    fs::write(&paths.metadata, metadata).unwrap();

    reset_entry_hash_call_count();
    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_machine_path_mismatch_before_source_hash_and_machine_read() {
    let root = unique_temp_root("machine-path-mismatch-before-read");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    let metadata_machine_path = paths.machine.with_file_name("other.machine");
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &metadata_machine_path, machine_bytes),
    )
    .unwrap();

    reset_entry_hash_call_count();
    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_zero_len_machine_before_source_hash_and_file_read() {
    let root = unique_temp_root("zero-len-machine-declared-before-read");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_entry_hash_call_count();
    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_stale_len_mismatch_skips_machine_file_metadata() {
    let root = unique_temp_root("stale-source-len-skip-machine-metadata");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let old_source_bytes = br#"{"value":1}"#;
    let new_source_bytes = br#"{"value":20}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, old_source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_file_metadata_call_count();
    reset_entry_hash_call_count();
    assert_ne!(old_source_bytes.len(), new_source_bytes.len(), "test requires length mismatch");
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, new_source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(machine_file_metadata_call_count(), 0);
    assert_eq!(entry_hash_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_stale_source_even_when_machine_file_is_missing() {
    let root = unique_temp_root("stale-source-no-machine");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let old_source_bytes = br#"{"value":1}"#;
    let new_source_bytes = br#"{"value":2}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, old_source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    assert_eq!(
      config.read_validated_machine(&paths, &source_path, new_source_bytes),
      DxMachineCacheStatus::Invalid
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_misses_matching_source_when_machine_file_is_missing() {
    let root = unique_temp_root("matching-source-no-machine");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_entry_hash_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Miss
    );
    assert_eq!(entry_hash_call_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_directory_machine_path_without_metadata_probe() {
    let root = unique_temp_root("directory-machine-no-metadata-probe");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(&paths.machine).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_file_metadata_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(machine_file_metadata_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_wrong_size_machine_after_source_hash_precheck() {
    let root = unique_temp_root("wrong-machine-size");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(&paths.machine, b"short").unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_entry_hash_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_same_len_changed_machine_after_read() {
    let root = unique_temp_root("same-len-changed-machine");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let metadata_machine_bytes = b"old";
    let changed_machine_bytes = b"new";
    assert_eq!(metadata_machine_bytes.len(), changed_machine_bytes.len());
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, changed_machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, metadata_machine_bytes),
    )
    .unwrap();

    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_non_beneficial_machine_before_source_hash_and_read() {
    let root = unique_temp_root("non-beneficial-machine-before-read");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = br#"{"value":1}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_entry_hash_call_count();
    reset_machine_file_read_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);
    assert_eq!(machine_file_read_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_bad_machine_hash_shape_before_file_metadata() {
    let root = unique_temp_root("bad-machine-hash-shape");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    let machine_hash = blake3::hash(machine_bytes).to_hex().to_string();
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes)
        .replace(&machine_hash, "not-a-blake3-hash"),
    )
    .unwrap();

    reset_entry_hash_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(entry_hash_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_rejects_bad_machine_hash_shape_before_path_normalization() {
    let root = unique_temp_root("bad-machine-hash-before-path-normalization");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    let machine_hash = blake3::hash(machine_bytes).to_hex().to_string();
    let metadata_machine_path = PathBuf::from(paths.machine.to_string_lossy().replace('\\', "/"));
    fs::create_dir_all(paths.metadata.parent().unwrap()).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &metadata_machine_path, machine_bytes)
        .replace(&machine_hash, "not-a-blake3-hash"),
    )
    .unwrap();

    reset_path_identity_call_count();
    reset_entry_hash_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Invalid
    );
    assert_eq!(path_identity_call_count(), 0);
    assert_eq!(entry_hash_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_returns_machine_bytes_on_hit() {
    let root = unique_temp_root("hit");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_hit_skips_metadata_file_stat() {
    let root = unique_temp_root("hit-skip-metadata-stat");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_metadata_file_metadata_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );
    assert_eq!(metadata_file_metadata_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_with_source_hash_returns_validated_digest_on_hit() {
    let root = unique_temp_root("hit-source-hash");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    let DxMachineCacheStatus::Hit(hit) =
      config.read_validated_machine_with_source_hash(&paths, &source_path, source_bytes)
    else {
      panic!("expected validated machine cache hit with source hash");
    };
    assert_eq!(hit.machine_bytes, machine_bytes);
    assert_eq!(hit.source_hash, blake3::hash(source_bytes));

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_hit_uses_machine_hash_from_file_read() {
    let root = unique_temp_root("hit-machine-streamed-hash");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_entry_bytes_hash_call_count();
    let DxMachineCacheStatus::Hit(hit) =
      config.read_validated_machine_with_source_hash(&paths, &source_path, source_bytes)
    else {
      panic!("expected validated machine cache hit");
    };

    assert_eq!(hit.machine_bytes, machine_bytes);
    assert_eq!(machine_entry_bytes_hash_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_hit_reuses_machine_shape_precheck_for_hash_validation() {
    let root = unique_temp_root("hit-reuse-machine-shape-precheck");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_entry_read_shape_call_count();
    let DxMachineCacheStatus::Hit(hit) =
      config.read_validated_machine_with_source_hash(&paths, &source_path, source_bytes)
    else {
      panic!("expected validated machine cache hit");
    };

    assert_eq!(hit.machine_bytes, machine_bytes);
    assert_eq!(machine_entry_read_shape_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_hit_reuses_source_precheck_for_hash_validation() {
    let root = unique_temp_root("hit-reuse-source-precheck");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_source_entry_shape_call_count();
    reset_entry_hash_call_count();
    let DxMachineCacheStatus::Hit(hit) =
      config.read_validated_machine_with_source_hash(&paths, &source_path, source_bytes)
    else {
      panic!("expected validated machine cache hit");
    };

    assert_eq!(hit.machine_bytes, machine_bytes);
    assert_eq!(hit.source_hash, blake3::hash(source_bytes));
    assert_eq!(entry_hash_call_count(), 1);
    assert_eq!(source_entry_shape_call_count(), 1);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_hit_skips_machine_file_metadata_probe() {
    let root = unique_temp_root("hit-machine-file-metadata-once");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &paths.machine, machine_bytes),
    )
    .unwrap();

    reset_machine_file_metadata_call_count();
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );
    assert_eq!(machine_file_metadata_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn read_validated_machine_read_path_has_no_fs_metadata_probe() {
    for function_name in [
      "read_validated_machine",
      "read_validated_machine_with_source_hash",
      "read_file_with_max_len",
      "open_machine_file",
      "read_open_file_with_exact_len_and_hash",
      "read_file_bytes_with_limit",
      "read_file_bytes_with_limit_and_hash",
      "read_small_exact_file_bytes_with_hash",
      "validate_metadata_path_and_hash_shape",
      "validate_machine_metadata_source_and_policy_before_hash",
      "validate_metadata_entry_before_hash",
      "validate_metadata_entry_hash_after_precheck",
      "validate_metadata_entry_read_hash",
      "metadata_path_matches",
      "runtime_path_str",
      "normalized_metadata_paths_match",
      "metadata_path_components",
      "metadata_path_components_match",
      "validate_cache_policy",
    ] {
      let function_source = dx_machine_cache_function_source(function_name);
      assert_no_fs_metadata_probe(function_name, function_source);
    }
  }

  #[test]
  fn read_validated_machine_compares_separator_equivalent_machine_path_without_normalized_path_allocations()
   {
    let root = unique_temp_root("machine-path-normalized-once");
    let cwd = root.join("project");
    let source_path = cwd.join("data.json");
    let source_bytes = br#"{"value":1}"#;
    let machine_bytes = b"machine";
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "json_module", &source_path);
    fs::create_dir_all(paths.machine.parent().unwrap()).unwrap();
    fs::write(&paths.machine, machine_bytes).unwrap();
    let metadata_machine_path = PathBuf::from(paths.machine.to_string_lossy().replace('\\', "/"));
    fs::write(
      &paths.metadata,
      machine_metadata_json(&source_path, source_bytes, &metadata_machine_path, machine_bytes),
    )
    .unwrap();

    reset_path_identity_call_count();
    let DxMachineCacheStatus::Hit(hit) =
      config.read_validated_machine_with_source_hash(&paths, &source_path, source_bytes)
    else {
      panic!("expected validated machine cache hit");
    };

    assert_eq!(hit.machine_bytes, machine_bytes);
    assert_eq!(path_identity_call_count(), 0);

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_writes_validated_machine_and_metadata() {
    let root = unique_temp_root("write");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes =
      br#"{"name":"rolldown","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_optional_peer_deps", &source_path);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_bytes).unwrap();

    config.write_machine_artifact(&paths, &source_path, source_bytes, machine_bytes).unwrap();

    assert!(!atomic_temp_path(&paths.machine).exists());
    assert!(!atomic_temp_path(&paths.metadata).exists());
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_syncs_machine_and_metadata_by_default() {
    let root = unique_temp_root("write-sync-default");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes =
      br#"{"name":"rolldown","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_optional_peer_deps", &source_path);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_bytes).unwrap();

    reset_sync_all_call_count();
    config.write_machine_artifact(&paths, &source_path, source_bytes, machine_bytes).unwrap();

    assert_eq!(sync_all_call_count(), 2);
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_skips_sync_all_when_unsynced_writes_enabled() {
    let root = unique_temp_root("write-unsynced-enabled");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes =
      br#"{"name":"rolldown","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let source_hash = blake3::hash(source_bytes);
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_optional_peer_deps", &source_path);
    let write_options = DxMachineCacheWriteOptions::from_env_value(Some(OsString::from("1")));
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_bytes).unwrap();

    reset_sync_all_call_count();
    config
      .write_machine_artifact_with_source_hash_and_options(
        &paths,
        &source_path,
        source_bytes.len(),
        &source_hash,
        machine_bytes,
        write_options,
      )
      .unwrap();

    assert_eq!(sync_all_call_count(), 0);
    assert_eq!(
      config.read_validated_machine(&paths, &source_path, source_bytes),
      DxMachineCacheStatus::Hit(machine_bytes.to_vec())
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_disabled_skips_source_hash_work() {
    let root = unique_temp_root("write-disabled-no-hash");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("0")));
    let paths = config.paths_for_source(&cwd, "package_json_metadata", &source_path);

    reset_write_source_hash_call_count();
    config.write_machine_artifact(&paths, &source_path, source_bytes, machine_bytes).unwrap();

    assert_eq!(write_source_hash_call_count(), 0);
    assert!(!paths.machine.exists());
    assert!(!paths.metadata.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_skips_non_beneficial_machine_bytes_before_source_hash() {
    let root = unique_temp_root("write-non-beneficial-no-hash");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes = br#"{"name":"rolldown"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_metadata", &source_path);

    reset_write_source_hash_call_count();
    config.write_machine_artifact(&paths, &source_path, source_bytes, machine_bytes).unwrap();

    assert_eq!(write_source_hash_call_count(), 0);
    assert!(!paths.machine.exists());
    assert!(!paths.metadata.exists());

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_with_source_hash_writes_validated_metadata_without_source_bytes() {
    let root = unique_temp_root("write-known-source-hash");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes =
      br#"{"name":"rolldown","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_metadata", &source_path);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_bytes).unwrap();
    let source_hash = blake3::hash(source_bytes);

    config
      .write_machine_artifact_with_source_hash(
        &paths,
        &source_path,
        source_bytes.len(),
        &source_hash,
        machine_bytes,
      )
      .unwrap();

    let metadata = fs::read_to_string(&paths.metadata).unwrap();
    assert_eq!(
      validate_machine_metadata(
        &metadata,
        &source_path,
        source_bytes,
        &paths.machine,
        machine_bytes
      ),
      DxMachineCacheStatus::Hit(())
    );

    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn write_machine_artifact_with_source_hash_skips_source_metadata_stat() {
    let root = unique_temp_root("write-known-source-hash-no-source-stat");
    let cwd = root.join("project");
    let source_path = cwd.join("package.json");
    let source_bytes =
      br#"{"name":"rolldown","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let machine_bytes = br#"{"name":"rolldown","optional_peer_dependencies":[]}"#;
    let config = DxMachineCacheConfig::from_env_value(&cwd, Some(OsString::from("1")));
    let paths = config.paths_for_source(&cwd, "package_json_metadata", &source_path);
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_bytes).unwrap();
    let source_hash = blake3::hash(source_bytes);

    reset_source_metadata_call_count();
    config
      .write_machine_artifact_with_source_hash(
        &paths,
        &source_path,
        source_bytes.len(),
        &source_hash,
        machine_bytes,
      )
      .unwrap();

    assert_eq!(source_metadata_call_count(), 0);
    let metadata = fs::read_to_string(&paths.metadata).unwrap();
    assert!(metadata.contains("\"modified_unix_ms\": null"));
    assert_eq!(
      validate_machine_metadata(
        &metadata,
        &source_path,
        source_bytes,
        &paths.machine,
        machine_bytes
      ),
      DxMachineCacheStatus::Hit(())
    );

    let _ = fs::remove_dir_all(root);
  }

  fn unique_temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir()
      .join(format!("rolldown-dx-machine-cache-{label}-{}-{nanos}", std::process::id()))
  }

  fn set_readonly(path: &Path, readonly: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(readonly);
    fs::set_permissions(path, permissions).unwrap();
  }

  fn dx_machine_cache_function_source(function_name: &str) -> &'static str {
    let source = include_str!("dx_machine_cache.rs");
    let function_token = format!("fn {function_name}(");
    let function_start = source.find(&function_token).unwrap_or_else(|| {
      panic!("expected to find function source for {function_name}");
    });
    let body_start = function_start
      + source[function_start..].find('{').unwrap_or_else(|| {
        panic!("expected to find function body for {function_name}");
      });

    let mut depth = 0_u32;
    for (offset, byte) in source[body_start..].bytes().enumerate() {
      match byte {
        b'{' => depth += 1,
        b'}' => {
          depth -= 1;
          if depth == 0 {
            let function_end = body_start + offset + 1;
            return &source[function_start..function_end];
          }
        }
        _ => {}
      }
    }

    panic!("expected to find function end for {function_name}");
  }

  fn assert_no_fs_metadata_probe(function_name: &str, function_source: &str) {
    for forbidden_probe in [
      "fs::metadata(",
      "std::fs::metadata(",
      ".metadata()",
      "fs::symlink_metadata(",
      "std::fs::symlink_metadata(",
      ".symlink_metadata()",
      ".exists(",
      ".try_exists(",
      "fs::canonicalize(",
      "std::fs::canonicalize(",
      ".canonicalize()",
      ".is_file()",
      ".is_dir()",
      ".is_symlink()",
    ] {
      assert!(
        !function_source.contains(forbidden_probe),
        "{function_name} must not use {forbidden_probe} in the validated machine read path"
      );
    }
  }

  fn temp_path_call_count() -> u64 {
    DX_MACHINE_TEST_TEMP_PATH_CALLS.with(std::cell::Cell::get)
  }

  fn reset_path_identity_call_count() {
    DX_MACHINE_TEST_PATH_IDENTITY_CALLS.with(|calls| calls.set(0));
  }

  fn path_identity_call_count() -> u64 {
    DX_MACHINE_TEST_PATH_IDENTITY_CALLS.with(std::cell::Cell::get)
  }

  fn reset_source_entry_shape_call_count() {
    DX_MACHINE_TEST_SOURCE_ENTRY_SHAPE_CALLS.with(|calls| calls.set(0));
  }

  fn source_entry_shape_call_count() -> u64 {
    DX_MACHINE_TEST_SOURCE_ENTRY_SHAPE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_entry_hash_call_count() {
    DX_MACHINE_TEST_ENTRY_HASH_CALLS.with(|calls| calls.set(0));
  }

  fn entry_hash_call_count() -> u64 {
    DX_MACHINE_TEST_ENTRY_HASH_CALLS.with(std::cell::Cell::get)
  }

  fn reset_machine_entry_bytes_hash_call_count() {
    DX_MACHINE_TEST_MACHINE_ENTRY_BYTES_HASH_CALLS.with(|calls| calls.set(0));
  }

  fn machine_entry_bytes_hash_call_count() -> u64 {
    DX_MACHINE_TEST_MACHINE_ENTRY_BYTES_HASH_CALLS.with(std::cell::Cell::get)
  }

  fn reset_machine_entry_read_shape_call_count() {
    DX_MACHINE_TEST_MACHINE_ENTRY_READ_SHAPE_CALLS.with(|calls| calls.set(0));
  }

  fn machine_entry_read_shape_call_count() -> u64 {
    DX_MACHINE_TEST_MACHINE_ENTRY_READ_SHAPE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_machine_file_read_call_count() {
    DX_MACHINE_TEST_MACHINE_FILE_READ_CALLS.with(|calls| calls.set(0));
  }

  fn machine_file_read_call_count() -> u64 {
    DX_MACHINE_TEST_MACHINE_FILE_READ_CALLS.with(std::cell::Cell::get)
  }

  fn reset_hashing_reader_read_call_count() {
    DX_MACHINE_TEST_HASHING_READER_READ_CALLS.with(|calls| calls.set(0));
  }

  fn hashing_reader_read_call_count() -> u64 {
    DX_MACHINE_TEST_HASHING_READER_READ_CALLS.with(std::cell::Cell::get)
  }

  fn reset_metadata_serde_parse_count() {
    DX_MACHINE_TEST_METADATA_SERDE_PARSE_CALLS.with(|calls| calls.set(0));
  }

  fn metadata_serde_parse_count() -> u64 {
    DX_MACHINE_TEST_METADATA_SERDE_PARSE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_normalized_metadata_path_match_count() {
    DX_MACHINE_TEST_NORMALIZED_METADATA_PATH_MATCH_CALLS.with(|calls| calls.set(0));
  }

  fn normalized_metadata_path_match_count() -> u64 {
    DX_MACHINE_TEST_NORMALIZED_METADATA_PATH_MATCH_CALLS.with(std::cell::Cell::get)
  }

  fn metadata_path_text_for_assertion(path: &str) -> String {
    if cfg!(windows) { path.replace('\\', "/") } else { path.to_string() }
  }

  fn json_escape_metadata_path_text_for_assertion(path: &str) -> String {
    json_escape(&metadata_path_text_for_assertion(path))
  }

  fn reset_small_exact_inline_hash_update_count() {
    DX_MACHINE_TEST_SMALL_EXACT_INLINE_HASH_UPDATES.with(|calls| calls.set(0));
  }

  fn small_exact_inline_hash_update_count() -> u64 {
    DX_MACHINE_TEST_SMALL_EXACT_INLINE_HASH_UPDATES.with(std::cell::Cell::get)
  }

  fn reset_machine_file_metadata_call_count() {
    DX_MACHINE_TEST_MACHINE_FILE_METADATA_CALLS.with(|calls| calls.set(0));
  }

  fn machine_file_metadata_call_count() -> u64 {
    DX_MACHINE_TEST_MACHINE_FILE_METADATA_CALLS.with(std::cell::Cell::get)
  }

  fn reset_source_metadata_call_count() {
    DX_MACHINE_TEST_SOURCE_METADATA_CALLS.with(|calls| calls.set(0));
  }

  fn source_metadata_call_count() -> u64 {
    DX_MACHINE_TEST_SOURCE_METADATA_CALLS.with(std::cell::Cell::get)
  }

  fn reset_write_source_hash_call_count() {
    DX_MACHINE_TEST_WRITE_SOURCE_HASH_CALLS.with(|calls| calls.set(0));
  }

  fn write_source_hash_call_count() -> u64 {
    DX_MACHINE_TEST_WRITE_SOURCE_HASH_CALLS.with(std::cell::Cell::get)
  }

  fn reset_sync_all_call_count() {
    DX_MACHINE_TEST_SYNC_ALL_CALLS.with(|calls| calls.set(0));
  }

  fn sync_all_call_count() -> u64 {
    DX_MACHINE_TEST_SYNC_ALL_CALLS.with(std::cell::Cell::get)
  }

  fn reset_target_exists_probe_call_count() {
    DX_MACHINE_TEST_TARGET_EXISTS_PROBE_CALLS.with(|calls| calls.set(0));
  }

  fn target_exists_probe_call_count() -> u64 {
    DX_MACHINE_TEST_TARGET_EXISTS_PROBE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_temp_exists_probe_call_count() {
    DX_MACHINE_TEST_TEMP_EXISTS_PROBE_CALLS.with(|calls| calls.set(0));
  }

  fn temp_exists_probe_call_count() -> u64 {
    DX_MACHINE_TEST_TEMP_EXISTS_PROBE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_parent_dir_create_call_count() {
    DX_MACHINE_TEST_PARENT_DIR_CREATE_CALLS.with(|calls| calls.set(0));
  }

  fn parent_dir_create_call_count() -> u64 {
    DX_MACHINE_TEST_PARENT_DIR_CREATE_CALLS.with(std::cell::Cell::get)
  }

  fn reset_metadata_file_metadata_call_count() {
    DX_MACHINE_TEST_METADATA_FILE_METADATA_CALLS.with(|calls| calls.set(0));
  }

  fn metadata_file_metadata_call_count() -> u64 {
    DX_MACHINE_TEST_METADATA_FILE_METADATA_CALLS.with(std::cell::Cell::get)
  }

  fn reset_existing_file_path_metadata_call_count() {
    DX_MACHINE_TEST_EXISTING_FILE_PATH_METADATA_CALLS.with(|calls| calls.set(0));
  }

  fn existing_file_path_metadata_call_count() -> u64 {
    DX_MACHINE_TEST_EXISTING_FILE_PATH_METADATA_CALLS.with(std::cell::Cell::get)
  }

  fn reset_existing_file_handle_metadata_call_count() {
    DX_MACHINE_TEST_EXISTING_FILE_HANDLE_METADATA_CALLS.with(|calls| calls.set(0));
  }

  fn existing_file_handle_metadata_call_count() -> u64 {
    DX_MACHINE_TEST_EXISTING_FILE_HANDLE_METADATA_CALLS.with(std::cell::Cell::get)
  }
}
