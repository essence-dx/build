use std::fs;

#[test]
fn raw_asset_loader_uses_async_read_to_avoid_blocking_plugin_runtime() {
  let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();

  assert!(
    source.contains("tokio::fs::read_to_string(path.as_ref()).await?"),
    "raw asset loading should read files through tokio::fs inside the async load hook"
  );
  assert!(
    !source.contains("std::fs::read_to_string(path.as_ref())?"),
    "raw asset loading must not use a blocking std::fs read inside the async load hook"
  );
}

#[test]
fn asset_url_loader_serializes_url_without_json_value_allocation() {
  let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();

  assert!(
    source.contains("serde_json::to_string(&url)?"),
    "asset URL wrapper should serialize the URL string directly"
  );
  assert!(
    !source.contains("Value::String(url)"),
    "asset URL wrapper should not allocate an intermediate serde_json::Value"
  );
}
