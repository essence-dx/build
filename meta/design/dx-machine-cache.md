# DX Machine Cache

## Summary

Rolldown can optionally use project-local `.dx/rolldown/*.machine` files as rebuildable read models for JSON-derived work. Source files remain canonical. A machine artifact is trusted only after the reader validates source identity, source length, source hash, machine length, machine hash, cache policy, and payload format. Any miss, stale source, corrupt artifact, unsupported payload, or validation failure falls back to the normal Rolldown path.

The first production surfaces are intentionally narrow:

- core JSON imports that can read a validated binary body directly into an OXC AST
- Vite JSON transforms whose options require parsing and whose source is above the payoff threshold
- resolver and Vite package metadata derived from `package.json`
- tsconfig-derived transform data when the config has no dependency graph through `extends` or `references`

Small files, no-parse stringify paths, virtual inputs, and dependency graphs that cannot yet be validated cheaply skip the machine cache. The cache is opt-in through `ROLLDOWN_DX_JSON_CACHE` so existing behavior stays the default until benchmarks justify broader rollout.

## Performance Claim Policy

The cache implementation is not itself a speedup claim. A speedup claim requires a governed benchmark that compares the same current checkout against an official package baseline and a local cache-disabled arm. The benchmark must use source-owned JSON or config fixtures above the payoff threshold, produce byte-identical outputs across arms, record cold and hot cache behavior separately, and prove `.dx/rolldown` artifacts were created and reused during the measured workload.

Until that benchmark passes the configured gate, benchmark receipts must keep `speedupClaim` set to `none`.

## Related

- [cache](./cache.md)
- [benchmarking](../../docs/development-guide/benchmarking.md)
