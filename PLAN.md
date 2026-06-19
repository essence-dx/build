# Rolldown DX Machine Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` for independent implementation slices, or `superpowers:executing-plans` for tightly coupled edits. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the local Rolldown fork measurably faster than official Rolldown on JSON/config-heavy workloads by reading pre-generated DX serializer `.machine` artifacts, while keeping upstream-compatible behavior and refusing unproven speed claims.

**Architecture:** Source files stay canonical. The DX ecosystem prepares `.dx/rolldown/*.machine` artifacts before user timing starts. The runtime may use a `.machine` hit only after validating source identity, source length, source hash, machine length, machine hash, cache policy, payload magic, and payload shape.

**Tech Stack:** Rust, Rolldown, OXC, TypeScript receipt tooling, `dx-serializer`, `rkyv`, `bytecheck`, `blake3`, guarded Cargo features, and G-drive benchmark evidence.

---

## Current Score

Overall goal status: **80 / 100**.

- Code path maturity: **84 / 100**. Core JSON and several package/config metadata paths have guarded `.machine` cache support, but the benchmark workload still exercises only part of the surface.
- Practical speed evidence: **76 / 100**. The latest corrected 30-sample run shows local cache-enabled faster than official on median and p95, with matching output hashes.
- Governed proof: **48 / 100**. The gate still fails because the run only exercised `RDXCJSR2`, did not exercise `RDXJSON2/RDXCSSRK/RDXOPDRK`, missed the enabled-vs-disabled 5% median threshold, and the confidence interval still includes zero.
- Production-readiness discipline: **88 / 100**. Safety rules are good, the benchmark plan, artifact exercise helper, source receipt contract, and governed gate derive from the same current machine-family requirements, and cache-scan rows without positive byte evidence are now rejected.

## Current Evidence

- Repo: `G:\Dx\rolldown`
- Branch: `dev`
- Current HEAD at plan refresh: `285e258e5c72f9d2635bb34346bf9c11e4467f03`
- Git status at plan refresh: `## dev...origin/dev [ahead 124]`
- Latest measured local build commit: `285e258e5c72f9d2635bb34346bf9c11e4467f03`
- Official baseline installed for latest run: `rolldown@1.0.3`
- Latest current run root: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed`
- Latest result file: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\current-official-vs-local-results.json`
- Latest corrected benchmark log: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-official-vs-local-18x1280-now-30samples.log`
- Latest corrected governed validation log: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-official-vs-local-18x1280-now-30samples-validate.log`
- Latest source receipt validation log: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-before-current-governed-benchmark-validate.log`
- Local DX feature build log: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\local-release-build-dx-serializer-after-threshold.log`

Latest corrected measured medians:

- Official `rolldown@1.0.3`: `219.71 ms`
- Local cache disabled: `218.37 ms`
- Local cache enabled: `208.92 ms`
- Local cache enabled vs official median: `5.17% faster`
- Local cache enabled vs local cache-disabled median: `4.53% faster`

Latest corrected measured p95:

- Official `rolldown@1.0.3`: `467.14 ms`
- Local cache disabled: `422.23 ms`
- Local cache enabled: `358.88 ms`

Latest governed gate status:

- Output hashes matched across official, local cache-disabled, and local cache-enabled arms.
- `.machine` writes during timing were `0`.
- `RDXCJSR2` artifacts were present and hit.
- `RDXJSON2`, `RDXCSSRK`, and `RDXOPDRK` were not exercised in the latest run.
- Local cache-enabled was faster than official by `5.17%` median and about `30.17%` p95 in the latest corrected run.
- Local cache-enabled was faster than local cache-disabled by `4.53%` median, below the governed `5%` threshold.
- Confidence interval still included zero.
- The gate failed, so the honest status is: **the practical current benchmark is faster than official, but the stricter governed faster-than-official claim is not proven yet.**

## Hard Rules For This Goal Mode

- Do not run heavy build commands in this lane.
- Do not run release builds, broad workspace tests, broad benchmarks, dev servers, package publishing, or network installs unless the user explicitly reopens that window.
- Allowed commands: `git status`, `git diff --check`, `rg`, `node --check`, targeted `node --test`, targeted `cargo test -p <crate> <filter> --lib`, targeted `cargo check -p <crate> --features <feature>`, and `cargo fmt --all --check`.
- New support scripts must be `.ts`, not `.js`, `.cjs`, or `.mjs`.
- Benchmark and test outputs must stay on G drive.
- Do not count `.machine` generation time in benchmark timing.
- Do not claim governed faster-than-official until the governed gate passes. Practical run-specific speed comparisons are allowed only with the exact log/result file.
- Keep generated `.dx/rolldown` artifacts out of git.

## Active Six-Agent Review Lanes

The current app already has six GPT-5.5/xhigh subagents allocated, so this plan reuses them instead of spawning duplicates.

- [x] Lane 1: Refresh-plan audit against current git state and latest benchmark artifacts. Agent: `019e7d62-0fc4-7532-8258-5749fa054f7a`.
- [x] Lane 2: Governed execution gate failure audit. Agent: `019e7d62-147e-7882-b2eb-3c6ab259183e`.
- [x] Lane 3: `RDXJSON2` Vite JSON cache and proof-gap audit. Agent: `019e7d62-1525-73b0-a072-c0e6ce9e44a5`.
- [x] Lane 4: `RDXCSSRK` and `RDXOPDRK` fixture/proof audit. Agent: `019e7d62-15ff-7f83-86b9-89c4a8dbb633`.
- [x] Lane 5: Shared machine-cache hot-path overhead audit. Agent: `019e7d62-16ea-78d3-a4dd-65d6b36a9bbe`.
- [x] Lane 6: TypeScript governed runner audit for all required magic families. Agent: `019e7d62-17b7-7ab3-83ea-63aeb7a8c316`.

Six-agent audit result:

- The latest plan and benchmark truth are now current in this file.
- The governed gate is failing for real reasons, not because of `.machine` generation timing: missing `RDXJSON2/RDXCSSRK/RDXOPDRK`, worse p95, and confidence including zero.
- The smallest TypeScript fix was to align `governed-benchmark.ts --plan` with the same all-magic contract as the gate; Task 1 implements that.
- The next benchmark work must prove the artifacts came from the current run and exercise all required magic families in every local-cache-enabled measured sample.
- The next Rust hot-path candidates are CSS invalid-metadata retry narrowing and core JSON cache target path borrowing, each requiring a focused red-green test before implementation.
- New current-run helper progress: `packages/bench/receipt/current-governed-benchmark.ts` now defines the required all-magic exercise plan, machine magic counts, artifact matrix validation, and read-hit count gating that refuses hits when artifacts are missing, unstable, not used in every enabled sample, or written/repaired during timing.
- New gate progress: `packages/bench/receipt/governed-execution-gate.ts` now rejects proven speedup receipts that omit `cacheArtifactEvidence.artifactExerciseMatrix` or provide a matrix missing `RDXJSON2`, `RDXCSSRK`, or `RDXOPDRK`.
- New evidence-composer progress: `packages/bench/receipt/current-governed-benchmark.ts` now builds `cacheArtifactEvidence` from `artifactExerciseMatrix`, with explicit zero counts for missing required magic and no fabricated `RDXJSON2` benefit evidence.
- New cache-scan conversion progress: `packages/bench/receipt/current-governed-benchmark.ts` now converts collected `machineFilesByMagic` cache scans into `artifactExerciseMatrix` rows, skips JSON-family rows without source metadata, and preserves honest all-magic failures when only `RDXCJSR2` exists.
- New evidence hardening progress: `packages/bench/receipt/current-governed-benchmark.ts` now rejects cache-scan machine rows that do not include positive byte evidence, so zero-byte or malformed rows cannot become benchmark proof.
- External runner progress: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\tools\current-governed-benchmark.ts` now imports the repo helper and emits cache evidence from the cache-scan-derived `artifactExerciseMatrix`; the latest execution receipt reports one real `RDXCJSR2` matrix row and zero fabricated rows for missing families.

## Implementation Tasks

### Task 1: Align the Governed Benchmark Plan With the Gate

**Files:**

- Create: `G:\Dx\rolldown\packages\bench\receipt\governed-benchmark.test.ts`
- Modify: `G:\Dx\rolldown\packages\bench\receipt\governed-benchmark.ts`

- [x] **Step 1: Write the failing test**

Create a test that imports the governed benchmark plan and verifies:

- `requiredCurrentMagic` includes `RDXCJSR2`, `RDXJSON2`, `RDXCSSRK`, and `RDXOPDRK`.
- `forbiddenLegacyMagic` includes `RDXCJSRK` and `RDXCJSN3`.
- `successCriteria.minimumIterations` is `30`.
- `successCriteria.minimumWarmupIterations` is `5`.
- `successCriteria.speedupClaimValidation.requireConfidenceIntervalExcludesZero` is `true`.
- `successCriteria.p95MustBeat` includes `upstream-stock` and `local-cache-disabled`.

Run:

```powershell
node --test packages\bench\receipt\governed-benchmark.test.ts *> G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-red.log
```

Expected: FAIL because the CLI plan currently does not export a testable plan and does not list all required magic families.

- [x] **Step 2: Implement the smallest plan-contract fix**

Export the plan object from `governed-benchmark.ts` and derive its magic and success criteria from `source-receipt.ts` constants so the CLI plan cannot drift away from the governed gate.

- [x] **Step 3: Verify the test passes**

Run:

```powershell
node --test packages\bench\receipt\governed-benchmark.test.ts *> G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-green.log
```

Expected: PASS.

- [x] **Step 4: Run a syntax check**

Run:

```powershell
node --check packages\bench\receipt\governed-benchmark.ts *> G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-node-check.log
```

Expected: PASS.

Task 1 proof logs:

- Red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-red.log`
- Green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-green.log`
- Syntax check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-benchmark-plan-node-check.log`

### Task 2: Make the Current Governed Runner Exercise All Required Magic Families

**Files:**

- Modify or promote: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\tools\current-governed-benchmark.ts`
- Prefer create: `G:\Dx\rolldown\packages\bench\receipt\current-governed-benchmark.ts`
- Test: `G:\Dx\rolldown\packages\bench\receipt\current-governed-benchmark.test.ts`

- [ ] Add fixture generation for a structured core JSON workload that produces `RDXCJSR2`.
- [ ] Add fixture generation for Vite JSON named-export workload that produces `RDXJSON2`.
- [ ] Add fixture generation for CSS package-name metadata that produces `RDXCSSRK`.
- [ ] Add fixture generation for optional peer dependency metadata that produces `RDXOPDRK`.
- [ ] Keep `.machine` generation and validation outside the timed section.
- [x] Add a `.ts` artifact exercise helper that requires all four current magic families before read-hit evidence can be granted.
- [x] Add magic-count evidence helpers that include required current magic and forbidden legacy magic counts.
- [x] Add read-hit gating that returns zero when timing writes/repairs occur, cache fingerprints are unstable, machine paths are missing, or a family is not used in every enabled sample.
- [x] Include the new helper and tests in the source receipt default file set.
- [x] Require `artifactExerciseMatrix` in executed receipt cache evidence.
- [x] Make the governed execution gate validate `artifactExerciseMatrix` before accepting a proven speedup claim.
- [x] Add a pure `buildCurrentGovernedCacheArtifactEvidence` helper so future runners can compose receipt cache evidence without inferring read hits from magic-count presence.
- [x] Prove an `RDXCJSR2`-only artifact matrix keeps `RDXJSON2`, `RDXCSSRK`, and `RDXOPDRK` at zero and still fails all-magic validation.
- [x] Add a pure cache-scan-to-artifact-matrix converter for external runner integration.
- [x] Reject cache-scan machine rows without positive byte evidence before they can become artifact exercise rows.
- [x] Keep all benchmark support code in `.ts` files.
- [x] Verify the helper with targeted `node --test`, syntax checks, and diff checks, not a heavy benchmark run.

**Task 2 partial proof logs:**

- Red helper test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-artifact-contract-red.log`
- Green helper test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-helper-final-test.log`
- Magic-count red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-magic-counts-red.log`
- Magic-count green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-magic-counts-green.log`
- Source receipt red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-current-governed-files-red.log`
- Source receipt green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-current-governed-files-final.log`
- Gate artifact-matrix red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-gate-artifact-matrix-red.log`
- Gate artifact-matrix green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-gate-artifact-matrix-green.log`
- Source receipt artifact-matrix field red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-artifact-matrix-field-red.log`
- Source receipt artifact-matrix field green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-artifact-matrix-field-green.log`
- Final gate test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-gate-artifact-matrix-full.log`
- Final source receipt test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-artifact-matrix-full.log`
- Cache evidence composer red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-artifact-evidence-composer-red.log`
- Cache evidence composer green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-artifact-evidence-composer-green.log`
- Cache evidence composer final test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-artifact-evidence-composer-final.log`
- Cache evidence composer gate regression: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-gate-after-cache-evidence-composer.log`
- Syntax check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-helper-node-check-final.log`
- Composer syntax check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-artifact-evidence-composer-node-check.log`
- Diff check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-helper-diff-check-final.log`
- Composer diff check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-artifact-evidence-composer-diff-check.log`
- Cache-scan converter red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\artifact-matrix-from-cache-scan-red.log`
- Cache-scan converter final test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\artifact-matrix-from-cache-scan-final-rerun.log`
- Cache-scan converter gate regression: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\governed-gate-after-cache-scan-matrix-rerun.log`
- Cache-scan converter syntax checks: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\artifact-matrix-from-cache-scan-node-check-rerun.log`, `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\artifact-matrix-from-cache-scan-test-node-check-rerun.log`
- Cache-scan converter diff check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\artifact-matrix-from-cache-scan-diff-check-rerun.log`
- Source receipt refresh and validation: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-after-cache-scan-matrix.log`, `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\source-receipt-after-cache-scan-matrix-validate.log`
- External runner helper syntax check: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\external-runner-repo-helper-node-check.log`
- External runner helper benchmark rerun: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-official-vs-local-18x1280-repo-helper.log`
- External runner helper receipt validation: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\current-governed-benchmark-repo-helper-validate.log`
- Positive-byte evidence red test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-scan-positive-bytes-red.log`
- Positive-byte evidence green test: `G:\Dx\rolldown-benchmarks\run-20260531-current-governed\logs\cache-scan-positive-bytes-green.log`

### Task 3: Add Runtime Cache-Hit Counters That Match the Gate

**Files:**

- Inspect first: `G:\Dx\rolldown\crates\rolldown_plugin_vite_json\src\lib.rs`
- Inspect first: `G:\Dx\rolldown\crates\rolldown_plugin_vite_css_post\src\utils.rs`
- Inspect first: `G:\Dx\rolldown\crates\rolldown_plugin_vite_resolve\src\package_json_cache.rs`
- Modify only after a failing targeted test proves the missing counter.

- [ ] Prove `RDXJSON2` hot hits record `lz4BodyDecodeCount >= 1` and skip transform work.
- [ ] Prove `RDXCSSRK` and `RDXOPDRK` read-hit counts are attached to executed benchmark evidence.
- [ ] Keep counters test-only or receipt-only unless production diagnostics already have a local pattern.

### Task 4: Reduce p95 Variance Before Claiming Victory

**Files:**

- Inspect first: `G:\Dx\rolldown\crates\rolldown_utils\src\dx_machine_cache.rs`
- Inspect first: `G:\Dx\rolldown\crates\rolldown\src\utils\parse_to_ecma_ast.rs`
- Inspect first: Vite JSON, CSS package-name, and optional-peer cache consumers.

- [ ] Identify one cache-hit overhead source that applies to every measured hit.
- [ ] Write a focused failing test for the overhead source.
- [ ] Implement the smallest safe fix.
- [ ] Run only the targeted Rust test and `cargo fmt --all --check`.

### Task 5: Run the Governed Benchmark Window Only After the Proof Surface Is Ready

**Files:**

- Use: `G:\Dx\rolldown\packages\bench\receipt\governed-benchmark.ts`
- Use: `G:\Dx\rolldown\packages\bench\receipt\governed-execution-gate.ts`
- Use: `G:\Dx\rolldown\.dx\rolldown\benchmark-source-receipt.json`

- [ ] Refresh source receipt on the final commit.
- [ ] Build local release with `dx-serializer-local` only when the user allows heavy commands.
- [ ] Install official `rolldown@latest` into an isolated G-drive benchmark folder only when the user allows network/heavy setup.
- [ ] Generate and validate all `.machine` artifacts before timing.
- [ ] Run 30 measured iterations and 5 warmups across all three arms.
- [ ] Require the governed gate to pass before any faster-than-official claim.

## Immediate Next Move

Continue Task 2 by adding fixture coverage for `RDXJSON2`, `RDXCSSRK`, and `RDXOPDRK` in the current governed workload. The current practical benchmark is faster than official, but the governed receipt still has only one real `RDXCJSR2` artifact row and cannot pass until the missing families are exercised and validated before timing.
