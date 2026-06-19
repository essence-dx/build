import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { validateGovernedBenchmarkExecutionSpeedupClaimGate } from './governed-execution-gate.ts';
import {
  createSourceBenchmarkReceipt,
  DEFAULT_SOURCE_RECEIPT_FILES,
  SOURCE_RECEIPT_CLAIM_STATUS,
  validateSourceBenchmarkReceipt,
} from './source-receipt.ts';

const selectedSourceOwnedFixturePath =
  'packages/rolldown/tests/fixtures/builtin-plugin/json/side-effects-named/large.json';

function writeDefaultSourceReceiptFiles(repoRoot) {
  for (const sourcePath of DEFAULT_SOURCE_RECEIPT_FILES) {
    const targetPath = path.join(repoRoot, sourcePath);
    fs.mkdirSync(path.dirname(targetPath), { recursive: true });
    fs.writeFileSync(targetPath, sourceReceiptFileContents(sourcePath), 'utf8');
  }
}

function sourceReceiptFileContents(sourcePath) {
  if (sourcePath === selectedSourceOwnedFixturePath) {
    return `${JSON.stringify({ payload: 'x'.repeat(16 * 1024) })}\n`;
  }
  return `${sourcePath}\n`;
}

function createValidGovernedExecutionSpeedupReceipt() {
  const outputHash = 'a'.repeat(64);
  return {
    receiptKind: 'governed_benchmark_execution',
    claimStatus: 'proven',
    benchmarkStatus: 'measured',
    upstreamComparison: 'measured',
    speedupClaim: 'local-cache-enabled beats upstream-stock and local-cache-disabled',
    environment: {
      nodeVersion: 'v22.19.0',
      rustcVersion: 'rustc 1.90.0',
      platform: 'win32',
      cpu: 'test cpu',
      powerThermalNotes: 'plugged in, stable power mode, no thermal throttling observed',
    },
    benchmarkExecution: {
      executed: true,
      sourceReceiptEvidence: {
        path: '.dx/rolldown/benchmark-source-receipt.json',
        sha256: '3'.repeat(64),
        sourceFilesSha256: '4'.repeat(64),
        gitCommit: 'abc123',
        gitDirty: false,
      },
      officialBaselineEvidence: {
        arm: 'upstream-stock',
        packageName: 'rolldown',
        packageVersion: '1.0.0',
        packageManager: 'pnpm',
        packageRegistry: 'npm',
        packageRegistryOrigin: 'https://registry.npmjs.org',
        packageResolvedUrl: 'https://registry.npmjs.org/rolldown/-/rolldown-1.0.0.tgz',
        packageResolvedUrlOrigin: 'https://registry.npmjs.org',
        packageInstallRoot: 'C:/bench/official/node_modules/rolldown',
        packageInstallRootRealpath: 'C:/bench/official/node_modules/rolldown',
        lockfilePath: 'C:/bench/official/pnpm-lock.yaml',
        lockfileEntryKey: 'rolldown@1.0.0',
        lockfileSha256: 'b'.repeat(64),
        packageIntegrity: 'sha512-test',
        installedPackageJsonSha256: 'c'.repeat(64),
        mainEntrypointPath: 'C:/bench/official/node_modules/rolldown/dist/index.js',
        mainEntrypointSha256: 'd'.repeat(64),
        binEntrypointPath: 'C:/bench/official/node_modules/.bin/rolldown',
        binEntrypointSha256: 'e'.repeat(64),
        nativeBindingPath: 'C:/bench/official/node_modules/rolldown/binding.node',
        nativeBindingSha256: 'f'.repeat(64),
        globalInstall: false,
        workspaceLink: false,
      },
      localBuildEvidence: {
        arm: 'local-cache-enabled',
        gitCommit: 'abc123',
        gitDirty: false,
        gitStatusShort: [],
        packageVersion: '1.0.0',
        packageInstallRoot: 'G:/Dx/build/packages/rolldown',
        packageInstallRootRealpath: 'G:/Dx/build/packages/rolldown',
        binaryPath: 'G:/Dx/build/packages/rolldown/bin/rolldown',
        binaryRealpath: 'G:/Dx/build/packages/rolldown/bin/rolldown',
        binarySha256: '1'.repeat(64),
        nativeBindingPath: 'G:/Dx/build/packages/rolldown/binding.node',
        nativeBindingRealpath: 'G:/Dx/build/packages/rolldown/binding.node',
        nativeBindingSha256: '2'.repeat(64),
        buildCommand: 'just build-rolldown-release',
        buildProfile: 'release',
        buildFeatures: ['dx_json_cache'],
        builtOutsideTimedSection: true,
      },
      perArmEvidence: [
        createGovernedArmEvidence('upstream-stock', outputHash, {
          meanMs: 105,
          medianMs: 100,
          p95Ms: 130,
          coldCacheMs: 112,
          hotCacheMs: 100,
        }),
        createGovernedArmEvidence('local-cache-disabled', outputHash, {
          meanMs: 102,
          medianMs: 98,
          p95Ms: 125,
          coldCacheMs: 108,
          hotCacheMs: 98,
        }),
        createGovernedArmEvidence('local-cache-enabled', outputHash, {
          meanMs: 82,
          medianMs: 80,
          p95Ms: 95,
          coldCacheMs: 86,
          hotCacheMs: 80,
        }),
      ],
      cacheArtifactEvidence: {
        arm: 'local-cache-enabled',
        dxRolldownFileCount: 3,
        dxRolldownBytes: 4096,
        artifactExerciseMatrix: createValidArtifactExerciseMatrix(),
        machineMagicCounts: {
          RDXCJSR2: 1,
          RDXJSON2: 1,
          RDXCSSRK: 1,
          RDXOPDRK: 1,
        },
        machineReadHitCounts: {
          RDXCJSR2: 30,
          RDXJSON2: 30,
          RDXCSSRK: 30,
          RDXOPDRK: 30,
        },
        machineHotCacheCounters: {
          RDXCJSR2: {
            jsonParseCount: 0,
            astNumberJsonParseCount: 0,
            dxSerializerBodyVecDecodeCount: 0,
            alignedCopyCount: 0,
          },
          RDXJSON2: {
            transformCount: 0,
            fromSliceValueParseCount: 0,
            payloadValidationCount: 0,
            lz4SizePrependedDecodeCount: 0,
            lz4BodyDecodeCount: 1,
          },
        },
        machineArtifactBenefitEvidence: {
          RDXCJSR2: {
            artifactKind: 'core-json',
            sourceBytes: 30 * 1024,
            machineBytes: 20 * 1024,
            sourceShape: 'object',
          },
          RDXJSON2: {
            artifactKind: 'vite-json',
            sourceBytes: 30 * 1024,
            machineBytes: 18 * 1024,
            sourceShape: 'object',
          },
        },
        machineWriteCountDuringTiming: 0,
        machineRepairCountDuringTiming: 0,
      },
      outputEquality: {
        hashAlgorithm: 'sha256',
        matchedArms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
        matchingOutputHash: outputHash,
      },
      statisticalEvidence: {
        confidenceLevel: 0.95,
        confidenceIntervalExcludesZero: true,
      },
      timingWindowEvidence: {
        machineArtifactsGeneratedBeforeTiming: true,
        machineArtifactsValidatedBeforeTiming: true,
        machineGenerationIncludedInTimedSection: false,
        timedSectionStartsAfterArtifactValidation: true,
      },
    },
  };
}

function createValidArtifactExerciseMatrix() {
  return [
    createArtifactExercise('core-json', 'RDXCJSR2'),
    createArtifactExercise('vite-json', 'RDXJSON2'),
    createArtifactExercise('css-package-name', 'RDXCSSRK'),
    createArtifactExercise('optional-peer-deps', 'RDXOPDRK'),
  ];
}

function createArtifactExercise(family, expectedMagic, overrides = {}) {
  return {
    family,
    expectedMagic,
    producedMachinePaths: [`G:/bench/.dx/rolldown/${expectedMagic}.machine`],
    sourceBytes: 30 * 1024,
    machineBytes: 18 * 1024,
    sourceShape: 'object',
    exercisedInEveryLocalCacheEnabledSample: true,
    cacheFingerprintStable: true,
    ...overrides,
  };
}

function createValidSourceReceiptContext(overrides = {}) {
  const { sourceReceipt: sourceReceiptOverrides, ...contextOverrides } = overrides;
  return {
    sourceReceiptSha256: '3'.repeat(64),
    ...contextOverrides,
    sourceReceipt: {
      claimStatus: SOURCE_RECEIPT_CLAIM_STATUS,
      benchmarkStatus: 'not_run',
      upstreamComparison: 'not_measured',
      speedupClaim: 'none',
      repos: {
        local: {
          commit: 'abc123',
          dirty: false,
        },
      },
      sourceFilesSha256: '4'.repeat(64),
      benchmarkExecution: {
        executed: false,
      },
      ...sourceReceiptOverrides,
    },
  };
}

function createGovernedArmEvidence(arm, outputHash, overrides) {
  return {
    arm,
    env:
      arm === 'local-cache-disabled'
        ? { ROLLDOWN_DX_JSON_CACHE: '0' }
        : arm === 'local-cache-enabled'
          ? { ROLLDOWN_DX_JSON_CACHE: '1' }
          : {},
    meanMs: overrides.meanMs,
    medianMs: overrides.medianMs,
    p95Ms: overrides.p95Ms,
    stdDevMs: 3,
    minMs: overrides.medianMs - 4,
    maxMs: overrides.p95Ms + 6,
    coldCacheMs: overrides.coldCacheMs,
    hotCacheMs: overrides.hotCacheMs,
    iterations: 30,
    warmupIterations: 5,
    selectedFixtureBytes: 20 * 1024,
    outputHash,
  };
}

test('governed execution speedup gate is idle for source-only receipts', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-execution-gate-idle-'));
  try {
    writeDefaultSourceReceiptFiles(repoRoot);

    const receipt = createSourceBenchmarkReceipt({
      generatedAt: '2026-05-30T00:00:00.000Z',
      gitInfo: {
        branch: 'dev',
        commit: 'abc123',
        dirty: false,
        statusShort: [],
      },
      repoRoot,
    });

    assert.deepEqual(validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt), {
      ok: true,
      errors: [],
    });
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test('governed execution speedup gate rejects proven claims without three-arm evidence', () => {
  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate({
    claimStatus: 'proven',
    speedupClaim: 'local-cache-enabled is faster',
    benchmarkExecution: {
      executed: true,
      perArmEvidence: [
        createGovernedArmEvidence('local-cache-enabled', 'a'.repeat(64), {
          meanMs: 82,
          medianMs: 80,
          p95Ms: 95,
          coldCacheMs: 86,
          hotCacheMs: 80,
        }),
      ],
    },
  });
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /receiptKind must be governed_benchmark_execution/);
  assert.match(errors, /officialBaselineEvidence\.packageName is required/);
  assert.match(errors, /localBuildEvidence\.gitDirty is required/);
  assert.match(errors, /missing governed benchmark arm: upstream-stock/);
  assert.match(errors, /missing governed benchmark arm: local-cache-disabled/);
  assert.match(errors, /cacheArtifactEvidence\.dxRolldownFileCount is required/);
  assert.match(errors, /timingWindowEvidence\.machineArtifactsGeneratedBeforeTiming is required/);
});

test('governed execution speedup gate rejects missing machine read-hit evidence', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.cacheArtifactEvidence.machineReadHitCounts.RDXJSON2 = 0;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineWriteCountDuringTiming = 1;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineRepairCountDuringTiming = 1;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /machineReadHitCounts\.RDXJSON2 must be >= 1/);
  assert.match(errors, /cacheArtifactEvidence\.machineWriteCountDuringTiming must be 0/);
  assert.match(errors, /cacheArtifactEvidence\.machineRepairCountDuringTiming must be 0/);
});

test('governed execution speedup gate rejects missing artifact exercise matrix', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  delete receipt.benchmarkExecution.cacheArtifactEvidence.artifactExerciseMatrix;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /cacheArtifactEvidence\.artifactExerciseMatrix is required/);
});

test('governed execution speedup gate rejects artifact exercise matrix without all magic families', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.cacheArtifactEvidence.artifactExerciseMatrix = [
    createArtifactExercise('core-json', 'RDXCJSR2'),
    createArtifactExercise('vite-json', 'RDXJSON2', { sourceShape: 'scalar' }),
    createArtifactExercise('legacy-core-json', 'RDXCJSRK'),
  ];

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /artifactExerciseMatrix\.RDXCSSRK is required/);
  assert.match(errors, /artifactExerciseMatrix\.RDXOPDRK is required/);
  assert.match(errors, /artifactExerciseMatrix\.RDXCJSRK must not use legacy magic/);
  assert.match(errors, /artifactExerciseMatrix\.RDXJSON2\.sourceShape must be object or array/);
});

test('governed execution speedup gate rejects machine read-hit counts below measured iterations', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.cacheArtifactEvidence.machineReadHitCounts.RDXJSON2 = 29;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /machineReadHitCounts\.RDXJSON2 must be >= 30/);
});

test('governed execution speedup gate rejects legacy or non-beneficial JSON machine artifacts', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.cacheArtifactEvidence.machineMagicCounts.RDXCJSRK = 1;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineArtifactBenefitEvidence.RDXCJSR2 = {
    artifactKind: 'core-json',
    sourceBytes: 30 * 1024,
    machineBytes: 30 * 1024,
    sourceShape: 'object',
  };
  receipt.benchmarkExecution.cacheArtifactEvidence.machineArtifactBenefitEvidence.RDXJSON2 = {
    artifactKind: 'vite-json',
    sourceBytes: 30 * 1024,
    machineBytes: 18 * 1024,
    sourceShape: 'scalar',
  };

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /machineMagicCounts\.RDXCJSRK must be 0/);
  assert.match(
    errors,
    /machineArtifactBenefitEvidence\.RDXCJSR2\.machineBytes must be < sourceBytes/,
  );
  assert.match(
    errors,
    /machineArtifactBenefitEvidence\.RDXJSON2\.sourceShape must be one of object, array/,
  );
});

test('governed execution speedup gate rejects missing hot-cache proof counters', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  delete receipt.benchmarkExecution.cacheArtifactEvidence.machineHotCacheCounters;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /machineHotCacheCounters\.RDXCJSR2 is required/);
  assert.match(errors, /machineHotCacheCounters\.RDXJSON2 is required/);
});

test('governed execution speedup gate rejects weak hot-cache proof counters', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.cacheArtifactEvidence.machineHotCacheCounters.RDXCJSR2.jsonParseCount = 1;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineHotCacheCounters.RDXJSON2.transformCount = 1;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineHotCacheCounters.RDXJSON2.lz4SizePrependedDecodeCount = 1;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineHotCacheCounters.RDXJSON2.lz4BodyDecodeCount = 0;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /machineHotCacheCounters\.RDXCJSR2\.jsonParseCount must be 0/);
  assert.match(errors, /machineHotCacheCounters\.RDXJSON2\.transformCount must be 0/);
  assert.match(errors, /machineHotCacheCounters\.RDXJSON2\.lz4SizePrependedDecodeCount must be 0/);
  assert.match(errors, /machineHotCacheCounters\.RDXJSON2\.lz4BodyDecodeCount must be >= 1/);
});

test('governed execution speedup gate rejects local cache claims without dx_json_cache build feature', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.localBuildEvidence.buildFeatures = [];

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /localBuildEvidence\.buildFeatures must include dx_json_cache/);
});

test('governed execution speedup gate rejects placeholder provenance evidence', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();

  Object.assign(receipt.benchmarkExecution.officialBaselineEvidence, {
    packageVersion: '',
    packageManager: '',
    lockfileSha256: 'not-a-sha',
    installedPackageJsonSha256: 'not-a-sha',
    mainEntrypointSha256: 'not-a-sha',
    binEntrypointSha256: 'not-a-sha',
    nativeBindingSha256: 'not-a-sha',
  });

  Object.assign(receipt.benchmarkExecution.localBuildEvidence, {
    gitStatusShort: [' M fake-dirty'],
    packageVersion: '',
    binaryPath: '',
    binarySha256: 'not-a-sha',
    nativeBindingSha256: 'not-a-sha',
    buildCommand: '',
  });

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /officialBaselineEvidence\.lockfileSha256 must be a sha256 hex digest/);
  assert.match(errors, /localBuildEvidence\.binarySha256 must be a sha256 hex digest/);
  assert.match(errors, /localBuildEvidence\.gitStatusShort must be empty/);
});

test('governed execution speedup gate rejects exaggerated speedup claim text', () => {
  for (const speedupClaim of [
    'local-cache-enabled is 2x faster than upstream-stock',
    'local-cache-enabled is 1.5 times faster than upstream-stock',
    'local-cache-enabled is 50% faster than upstream-stock',
    'local-cache-enabled is 50 percent faster than upstream-stock',
    'local-cache-enabled saves 10ms against upstream-stock',
  ]) {
    const receipt = createValidGovernedExecutionSpeedupReceipt();
    receipt.speedupClaim = speedupClaim;

    const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
      receipt,
      createValidSourceReceiptContext(),
    );
    const errors = validation.errors.join('\n');

    assert.equal(validation.ok, false, speedupClaim);
    assert.match(
      errors,
      /speedupClaim must not contain free-text numeric speed claims/,
      speedupClaim,
    );
  }
});

test('governed execution speedup gate accepts proven claims with required evidence', () => {
  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    createValidGovernedExecutionSpeedupReceipt(),
    createValidSourceReceiptContext(),
  );

  assert.deepEqual(validation, { ok: true, errors: [] });
});

test('governed execution speedup gate rejects proven claims without validated source receipt context', () => {
  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    createValidGovernedExecutionSpeedupReceipt(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /validated source receipt sha256 is required/);
  assert.match(errors, /validated source receipt is required/);
});

test('governed execution speedup gate rejects source receipt evidence digest drift', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.sourceReceiptEvidence.sourceFilesSha256 = '9'.repeat(64);

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext(),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /sourceReceiptEvidence\.sourceFilesSha256 must match validated source receipt sourceFilesSha256/,
  );
});

test('governed execution speedup gate rejects proven claims without source receipt binding', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  delete receipt.benchmarkExecution.sourceReceiptEvidence;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /sourceReceiptEvidence\.path is required/);
  assert.match(errors, /sourceReceiptEvidence\.sha256 is required/);
  assert.match(errors, /sourceReceiptEvidence\.sourceFilesSha256 is required/);
  assert.match(errors, /sourceReceiptEvidence\.gitCommit is required/);
});

test('governed execution speedup gate rejects stale source receipt binding', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.sourceReceiptEvidence.path = 'tmp/benchmark-source-receipt.json';
  receipt.benchmarkExecution.sourceReceiptEvidence.sha256 = 'not-a-sha';
  receipt.benchmarkExecution.sourceReceiptEvidence.sourceFilesSha256 = 'also-not-a-sha';
  receipt.benchmarkExecution.sourceReceiptEvidence.gitCommit = 'stale';
  receipt.benchmarkExecution.sourceReceiptEvidence.gitDirty = true;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /sourceReceiptEvidence\.path must be \.dx\/rolldown\/benchmark-source-receipt\.json/,
  );
  assert.match(errors, /sourceReceiptEvidence\.sha256 must be a sha256 hex digest/);
  assert.match(
    errors,
    /sourceReceiptEvidence\.sourceFilesSha256 must be a sha256 hex digest/,
  );
  assert.match(errors, /sourceReceiptEvidence\.gitDirty must be false/);
  assert.match(
    errors,
    /sourceReceiptEvidence\.gitCommit must match localBuildEvidence\.gitCommit/,
  );
});

test('governed execution speedup gate rejects official baseline inside local build roots', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.officialBaselineEvidence.packageInstallRootRealpath =
    'G:/Dx/build/packages/rolldown/node_modules/rolldown';
  receipt.benchmarkExecution.officialBaselineEvidence.nativeBindingPath =
    'G:/Dx/build/packages/rolldown/binding.node';
  receipt.benchmarkExecution.officialBaselineEvidence.mainEntrypointPath =
    'G:/Dx/build/packages/rolldown/dist/index.js';

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /officialBaselineEvidence\.packageInstallRootRealpath must stay outside localBuildEvidence\.packageInstallRootRealpath/,
  );
  assert.match(
    errors,
    /officialBaselineEvidence\.nativeBindingPath must stay outside localBuildEvidence\.packageInstallRootRealpath/,
  );
  assert.match(
    errors,
    /officialBaselineEvidence\.mainEntrypointPath must stay outside localBuildEvidence\.packageInstallRootRealpath/,
  );
});

test('governed execution speedup gate rejects official baseline inside validated repo root', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.officialBaselineEvidence.packageInstallRootRealpath =
    'G:/Dx/build/node_modules/rolldown';
  receipt.benchmarkExecution.officialBaselineEvidence.mainEntrypointPath =
    'G:/Dx/build/node_modules/rolldown/dist/index.js';

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(
    receipt,
    createValidSourceReceiptContext({
      sourceReceipt: {
        repos: {
          local: {
            root: 'G:/Dx/build',
            commit: 'abc123',
            dirty: false,
          },
        },
      },
    }),
  );
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /officialBaselineEvidence\.packageInstallRootRealpath must stay outside validatedSourceReceipt\.repos\.local\.root/,
  );
  assert.match(
    errors,
    /officialBaselineEvidence\.mainEntrypointPath must stay outside validatedSourceReceipt\.repos\.local\.root/,
  );
});

test('governed execution speedup gate rejects mismatched selected fixture bytes', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.perArmEvidence[0].selectedFixtureBytes = 17 * 1024;
  receipt.benchmarkExecution.perArmEvidence[1].selectedFixtureBytes = 32 * 1024;
  receipt.benchmarkExecution.perArmEvidence[2].selectedFixtureBytes = 64 * 1024;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /selectedFixtureBytes must match across governed benchmark arms/,
  );
});

test('governed execution speedup gate rejects duplicate output equality arms', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.outputEquality.matchedArms = [
    'upstream-stock',
    'local-cache-disabled',
    'local-cache-enabled',
    'local-cache-enabled',
  ];

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /outputEquality\.matchedArms contains duplicate arm local-cache-enabled/);
});

test('governed execution speedup gate rejects non-positive timing metrics', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  const localEnabled = receipt.benchmarkExecution.perArmEvidence[2];
  localEnabled.meanMs = 0;
  localEnabled.medianMs = 0;
  localEnabled.p95Ms = 0;
  localEnabled.minMs = 0;
  localEnabled.maxMs = 0;
  localEnabled.coldCacheMs = 0;
  localEnabled.hotCacheMs = 0;
  localEnabled.stdDevMs = -1;

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /local-cache-enabled meanMs must be > 0/);
  assert.match(errors, /local-cache-enabled medianMs must be > 0/);
  assert.match(errors, /local-cache-enabled p95Ms must be > 0/);
  assert.match(errors, /local-cache-enabled minMs must be > 0/);
  assert.match(errors, /local-cache-enabled maxMs must be > 0/);
  assert.match(errors, /local-cache-enabled coldCacheMs must be > 0/);
  assert.match(errors, /local-cache-enabled hotCacheMs must be > 0/);
  assert.match(errors, /local-cache-enabled stdDevMs must be >= 0/);
});

test('source receipt validation keeps execution receipts outside the source-only contract', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-execution-gate-source-only-'));
  try {
    writeDefaultSourceReceiptFiles(repoRoot);

    const validation = validateSourceBenchmarkReceipt(
      createValidGovernedExecutionSpeedupReceipt(),
      { repoRoot },
    );
    const errors = validation.errors.join('\n');

    assert.equal(validation.ok, false);
    assert.match(errors, /schema mismatch/);
    assert.match(errors, /claimStatus must be not_proven_source_receipt_only/);
    assert.match(errors, /speedupClaim must be none/);
    assert.match(errors, /benchmarkExecution\.executed must be false/);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test('governed execution speedup gate rejects weak stats output drift and timed generation', () => {
  const receipt = createValidGovernedExecutionSpeedupReceipt();
  receipt.benchmarkExecution.perArmEvidence[2].medianMs = 101;
  receipt.benchmarkExecution.perArmEvidence[2].p95Ms = 126;
  receipt.benchmarkExecution.perArmEvidence[2].outputHash = '9'.repeat(64);
  receipt.benchmarkExecution.perArmEvidence[2].iterations = 10;
  receipt.benchmarkExecution.perArmEvidence[2].env.ROLLDOWN_DX_JSON_CACHE = '0';
  receipt.benchmarkExecution.cacheArtifactEvidence.machineMagicCounts.RDXCJSR2 = 0;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineMagicCounts.RDXJSON2 = 0;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineMagicCounts.RDXCSSRK = 0;
  receipt.benchmarkExecution.cacheArtifactEvidence.machineMagicCounts.RDXOPDRK = 0;
  receipt.benchmarkExecution.statisticalEvidence.confidenceIntervalExcludesZero = false;
  receipt.benchmarkExecution.timingWindowEvidence.machineGenerationIncludedInTimedSection = true;
  receipt.environment.powerThermalNotes = '';

  const validation = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /local-cache-enabled medianMs must beat upstream-stock/);
  assert.match(errors, /local-cache-enabled medianMs must improve by at least 5% over local-cache-disabled/);
  assert.match(errors, /local-cache-enabled p95Ms must beat local-cache-disabled/);
  assert.match(errors, /perArmEvidence outputHash values must match across governed arms/);
  assert.match(errors, /local-cache-enabled iterations must be >= 30/);
  assert.match(errors, /local-cache-enabled env\.ROLLDOWN_DX_JSON_CACHE must be 1/);
  assert.match(errors, /machineMagicCounts\.RDXCJSR2 must be >= 1/);
  assert.match(errors, /machineMagicCounts\.RDXJSON2 must be >= 1/);
  assert.match(errors, /machineMagicCounts\.RDXCSSRK must be >= 1/);
  assert.match(errors, /machineMagicCounts\.RDXOPDRK must be >= 1/);
  assert.match(errors, /statisticalEvidence\.confidenceIntervalExcludesZero must be true/);
  assert.match(errors, /environment\.powerThermalNotes is required/);
  assert.match(
    errors,
    /timingWindowEvidence\.machineGenerationIncludedInTimedSection must be false/,
  );
});
