import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import url from 'node:url';

import * as sourceReceiptModule from './source-receipt.ts';
import {
  createSourceBenchmarkReceipt,
  DEFAULT_SOURCE_RECEIPT_FILES,
  GOVERNED_BENCHMARK_ARMS,
  REQUIRED_BENCHMARK_METRICS,
  SOURCE_RECEIPT_CLAIM_STATUS,
  validateSourceBenchmarkReceipt,
  writeSourceBenchmarkReceipt,
} from './source-receipt.ts';

const dirname = path.dirname(url.fileURLToPath(import.meta.url));
const modulePath = path.join(dirname, 'source-receipt.ts');
const selectedSourceOwnedFixturePath =
  'packages/rolldown/tests/fixtures/builtin-plugin/json/side-effects-named/large.json';
const selectedPackageJsonFixturePath =
  'packages/bench/fixtures/dx-machine-cache/package-json-artifacts/package.json';
const selectedSourceOwnedFixturePaths = [
  selectedSourceOwnedFixturePath,
  selectedPackageJsonFixturePath,
];

const officialBaselineRequiredFields = [
  'packageName',
  'packageVersion',
  'packageManager',
  'packageRegistry',
  'packageRegistryOrigin',
  'packageResolvedUrl',
  'packageResolvedUrlOrigin',
  'packageInstallRoot',
  'packageInstallRootRealpath',
  'lockfilePath',
  'lockfileEntryKey',
  'lockfileSha256',
  'packageIntegrity',
  'installedPackageJsonSha256',
  'mainEntrypointPath',
  'mainEntrypointSha256',
  'binEntrypointPath',
  'binEntrypointSha256',
  'nativeBindingPath',
  'nativeBindingSha256',
  'globalInstall',
  'workspaceLink',
];

const officialBaselineRequiredValues = {
  packageName: 'rolldown',
  packageRegistryOrigin: 'https://registry.npmjs.org',
  packageResolvedUrlOrigin: 'https://registry.npmjs.org',
  globalInstall: false,
  workspaceLink: false,
};

const localBuildRequiredFields = [
  'gitCommit',
  'gitDirty',
  'gitStatusShort',
  'packageVersion',
  'packageInstallRoot',
  'packageInstallRootRealpath',
  'binaryPath',
  'binaryRealpath',
  'binarySha256',
  'nativeBindingPath',
  'nativeBindingRealpath',
  'nativeBindingSha256',
  'buildCommand',
  'buildProfile',
  'buildFeatures',
  'builtOutsideTimedSection',
];

const localBuildRequiredValues = {
  gitDirty: false,
  buildProfile: 'release',
  buildFeatures: ['dx_json_cache'],
  builtOutsideTimedSection: true,
};

const sourceReceiptRequiredFields = [
  'path',
  'sha256',
  'sourceFilesSha256',
  'gitCommit',
  'gitDirty',
];
const sourceReceiptRequiredValues = {
  path: '.dx/rolldown/benchmark-source-receipt.json',
  gitDirty: false,
};

const governedDryRunReceipt = {
  id: 'source-only-governed-benchmark-dry-run',
  script: 'bench:governed:dry',
  command: 'node ./receipt/source-receipt.ts',
  executesBenchmarks: false,
  importsBenchmarkRunner: false,
  writesBenchmarkResults: false,
  allowedOutputPath: '.dx/rolldown/benchmark-source-receipt.json',
  blockedCommands: [
    'oxnode ./benches/compare.js',
    'oxnode ./benches/ci.js',
    'oxnode ./benches/par.js',
    'pnpm --filter bench bench-ci',
    'cargo bench -p bench',
    'just build-rolldown-release',
  ],
  allowedClaims: [
    'source_receipt_generated',
    'governed_plan_recorded',
    'benchmarks_not_run',
  ],
  forbiddenClaims: [
    'faster_than_upstream',
    'local_cache_wins',
    'upstream_comparison_measured',
    'output_equivalence_proven',
    'official_baseline_collected',
  ],
};
const governedExecutedReceiptRequirements = {
  receiptKind: 'governed_benchmark_execution',
  sourceReceiptEvidence: {
    requiredFields: sourceReceiptRequiredFields,
    requiredValues: sourceReceiptRequiredValues,
    sourceFilesHashAlgorithm: 'sha256',
  },
  officialBaselineEvidence: {
    arm: 'upstream-stock',
    installMode: 'clean_pinned_package_install',
    requiredFields: officialBaselineRequiredFields,
    requiredValues: officialBaselineRequiredValues,
  },
  localBuildEvidence: {
    arm: 'local-cache-enabled',
    buildTimingPolicy: 'built_before_timed_section',
    requiredFields: localBuildRequiredFields,
    requiredValues: localBuildRequiredValues,
  },
  perArmEvidence: {
    arms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
    requiredFields: [
      'arm',
      'meanMs',
      'medianMs',
      'p95Ms',
      'stdDevMs',
      'minMs',
      'maxMs',
      'coldCacheMs',
      'hotCacheMs',
      'iterations',
      'warmupIterations',
      'selectedFixtureBytes',
      'outputHash',
    ],
  },
  cacheArtifactEvidence: {
    arms: ['local-cache-enabled'],
    root: '.dx/rolldown',
    requiredFields: [
      'dxRolldownFileCount',
      'dxRolldownBytes',
      'artifactExerciseMatrix',
      'machineMagicCounts',
      'machineReadHitCounts',
      'machineHotCacheCounters',
      'machineArtifactBenefitEvidence',
      'machineWriteCountDuringTiming',
      'machineRepairCountDuringTiming',
    ],
    requiredMachineMagic: [
      {
        artifactKind: 'core-json',
        magic: 'RDXCJSR2',
        minimumCount: 1,
      },
      {
        artifactKind: 'vite-json',
        magic: 'RDXJSON2',
        minimumCount: 1,
      },
      {
        artifactKind: 'css-package-name',
        magic: 'RDXCSSRK',
        minimumCount: 1,
      },
      {
        artifactKind: 'optional-peer-deps',
        magic: 'RDXOPDRK',
        minimumCount: 1,
      },
    ],
    requiredHotCacheCounters: [
      {
        artifactKind: 'core-json',
        magic: 'RDXCJSR2',
        exact: {
          jsonParseCount: 0,
          astNumberJsonParseCount: 0,
          dxSerializerBodyVecDecodeCount: 0,
          alignedCopyCount: 0,
        },
      },
      {
        artifactKind: 'vite-json',
        magic: 'RDXJSON2',
        exact: {
          transformCount: 0,
          fromSliceValueParseCount: 0,
          payloadValidationCount: 0,
          lz4SizePrependedDecodeCount: 0,
        },
        minimum: {
          lz4BodyDecodeCount: 1,
        },
      },
    ],
    forbiddenMachineMagic: [
      {
        artifactKind: 'legacy-core-json',
        magic: 'RDXCJSRK',
      },
      {
        artifactKind: 'legacy-core-json',
        magic: 'RDXCJSN3',
      },
    ],
    requiredMachineArtifactBenefits: [
      {
        artifactKind: 'core-json',
        magic: 'RDXCJSR2',
        allowedSourceShapes: ['object', 'array'],
        machineBytesMustBeLessThanSourceBytes: true,
      },
      {
        artifactKind: 'vite-json',
        magic: 'RDXJSON2',
        allowedSourceShapes: ['object', 'array'],
        machineBytesMustBeLessThanSourceBytes: true,
      },
    ],
  },
  outputEquality: {
    hashAlgorithm: 'sha256',
    mustMatchAcrossArms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
  },
  timingWindowEvidence: {
    requiredFields: [
      'machineArtifactsGeneratedBeforeTiming',
      'machineArtifactsValidatedBeforeTiming',
      'machineGenerationIncludedInTimedSection',
      'timedSectionStartsAfterArtifactValidation',
    ],
    requiredValues: {
      machineArtifactsGeneratedBeforeTiming: true,
      machineArtifactsValidatedBeforeTiming: true,
      machineGenerationIncludedInTimedSection: false,
      timedSectionStartsAfterArtifactValidation: true,
    },
  },
  claimPolicy: {
    speedupClaimAllowedOnlyAfter: 'governed_speedup_gate_passes',
  },
};
const governedMachineArtifactPreparation = {
  model: 'dx_ecosystem_pre_generated_artifacts',
  countedInTimedSection: false,
  requiredBeforeTiming: [
    'machineArtifactsGenerated',
    'machineArtifactsValidated',
    'cacheArtifactFileCountRecorded',
    'cacheArtifactBytesRecorded',
  ],
  excludedFromTiming: [
    'source_json_to_machine_generation',
    'machine_cache_write',
    'machine_cache_repair',
    'official_package_install',
    'local_release_build',
  ],
  coldCacheDefinition:
    'first measured user run reads existing validated .machine artifacts; it must not generate them',
  hotCacheDefinition:
    'subsequent measured user runs reuse the same existing validated .machine artifacts',
};

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
  if (sourcePath === selectedPackageJsonFixturePath) {
    return `${JSON.stringify(createPackageJsonArtifactFixture(), null, 2)}\n`;
  }
  return `${sourcePath}\n`;
}

function writeSelectedSourceOwnedFixture(repoRoot) {
  for (const sourcePath of selectedSourceOwnedFixturePaths) {
    const targetPath = path.join(repoRoot, sourcePath);
    fs.mkdirSync(path.dirname(targetPath), { recursive: true });
    fs.writeFileSync(targetPath, sourceReceiptFileContents(sourcePath), 'utf8');
  }
}

function createPackageJsonArtifactFixture() {
  const peerDependencies = {};
  const peerDependenciesMeta = {};
  for (let index = 0; index < 32; index += 1) {
    const name = `@dx-fixture/peer-${String(index).padStart(2, '0')}`;
    peerDependencies[name] = '^1.0.0';
    peerDependenciesMeta[name] = { optional: true };
  }
  return {
    name: '@rolldown-dx/package-json-artifacts-fixture',
    version: '0.0.0',
    private: true,
    peerDependencies,
    peerDependenciesMeta,
    dxFixturePadding: 'x'.repeat(17 * 1024),
  };
}

function readCargoFeatureDependencies(relativeCargoTomlPath, featureName) {
  const cargoTomlPath = path.resolve(dirname, '../../..', relativeCargoTomlPath);
  const lines = fs.readFileSync(cargoTomlPath, 'utf8').split(/\r?\n/);
  const featurePrefix = `${featureName} = [`;
  let inFeaturesSection = false;
  let featureSource = '';

  for (const line of lines) {
    if (/^\[[^\]]+\]/.test(line)) {
      inFeaturesSection = line === '[features]';
      continue;
    }
    if (!inFeaturesSection) {
      continue;
    }
    if (featureSource) {
      featureSource += `\n${line}`;
      if (line.includes(']')) {
        break;
      }
      continue;
    }
    if (line.startsWith(featurePrefix)) {
      featureSource = line;
      if (line.includes(']')) {
        break;
      }
    }
  }

  assert.notEqual(
    featureSource,
    '',
    `${relativeCargoTomlPath} must define feature ${featureName}`,
  );
  return [...featureSource.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

function readCargoDependencyNames(relativeCargoTomlPath) {
  const cargoTomlPath = path.resolve(dirname, '../../..', relativeCargoTomlPath);
  const lines = fs.readFileSync(cargoTomlPath, 'utf8').split(/\r?\n/);
  const dependencyNames = [];
  let inDependenciesSection = false;

  for (const line of lines) {
    if (/^\[[^\]]+\]/.test(line)) {
      inDependenciesSection = line === '[dependencies]';
      continue;
    }
    if (!inDependenciesSection || !line || line.startsWith('#')) {
      continue;
    }
    const dependencyName = line.match(/^([A-Za-z0-9_-]+)\s*=/)?.[1];
    if (dependencyName) {
      dependencyNames.push(dependencyName);
    }
  }

  return dependencyNames;
}

test('source receipt records an unproven three-arm benchmark plan with source hashes', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-'));
  fs.mkdirSync(path.join(repoRoot, 'packages/bench'), { recursive: true });
  for (const sourcePath of selectedSourceOwnedFixturePaths) {
    fs.mkdirSync(path.dirname(path.join(repoRoot, sourcePath)), { recursive: true });
  }
  fs.writeFileSync(path.join(repoRoot, 'PLAN.md'), '# plan\n', 'utf8');
  fs.writeFileSync(path.join(repoRoot, 'packages/bench/package.json'), '{"name":"bench"}\n', 'utf8');
  writeSelectedSourceOwnedFixture(repoRoot);

  const receipt = createSourceBenchmarkReceipt({
    generatedAt: '2026-05-30T00:00:00.000Z',
    gitInfo: {
      branch: 'dev',
      commit: 'abc123',
      dirty: true,
      statusShort: [' M packages/bench/package.json'],
    },
    repoRoot,
    sourceFiles: ['PLAN.md', 'packages/bench/package.json'],
  });

  assert.equal(receipt.schema, 'rolldown.dx.benchmark.source_receipt.v1');
  assert.equal(receipt.claimStatus, SOURCE_RECEIPT_CLAIM_STATUS);
  assert.equal(receipt.benchmarkStatus, 'not_run');
  assert.equal(receipt.upstreamComparison, 'not_measured');
  assert.equal(receipt.speedupClaim, 'none');
  assert.equal(receipt.benchmarkExecution.executed, false);
  assert.equal(receipt.benchmarkExecution.reason, 'source receipt only; no benchmark command executed');
  assert.deepEqual(
    receipt.governedBenchmark.arms.map((arm) => arm.id),
    ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
  );
  assert.equal(
    receipt.governedBenchmark.arms[1].env.ROLLDOWN_DX_JSON_CACHE,
    '0',
  );
  assert.equal(
    receipt.governedBenchmark.arms[2].env.ROLLDOWN_DX_JSON_CACHE,
    '1',
  );
  assert.deepEqual(receipt.governedBenchmark.requiredMetrics, REQUIRED_BENCHMARK_METRICS);
  assert.equal(receipt.governedBenchmark.outputEquivalence.required, true);
  assert.equal(receipt.governedBenchmark.buildOutsideTimedSection, true);
  assert.deepEqual(
    receipt.governedBenchmark.machineArtifactPreparation,
    governedMachineArtifactPreparation,
  );
  assert.deepEqual(receipt.governedBenchmark.officialBaseline, {
    arm: 'upstream-stock',
    packageName: 'rolldown',
    installMode: 'clean_pinned_package_install',
    installRootPolicy: 'realpath_outside_local_repo_and_not_workspace_link',
    globalInstallAllowed: false,
    requiredFieldsBeforeTiming: officialBaselineRequiredFields,
    requiredValuesBeforeTiming: officialBaselineRequiredValues,
    notes: 'global installs are not accepted as official baseline evidence',
  });
  assert.deepEqual(receipt.governedBenchmark.dryRun, governedDryRunReceipt);
  assert.deepEqual(
    receipt.governedBenchmark.executedReceiptRequirements,
    governedExecutedReceiptRequirements,
  );
  assert.deepEqual(
    receipt.governedBenchmark.cacheEvidence.requiredFields,
    receipt.governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.requiredFields,
  );
  assert.ok(receipt.governedBenchmark.cacheEvidence.requiredFields.includes('machineReadHitCounts'));
  assert.ok(
    receipt.governedBenchmark.cacheEvidence.requiredFields.includes('machineHotCacheCounters'),
  );
  assert.deepEqual(receipt.governedBenchmark.requiredEnvironmentFields, [
    'nodeVersion',
    'rustcVersion',
    'platform',
    'cpu',
    'powerThermalNotes',
    'ROLLDOWN_DX_JSON_CACHE',
  ]);
  assert.deepEqual(receipt.governedBenchmark.fixturePlan.sourcePaths, [
    'packages/bench/src/suites/index.ts',
    'packages/bench/src/suites/rome-ts.ts',
    'packages/bench/vue-entry.js',
  ]);
  assert.deepEqual(receipt.governedBenchmark.fixturePlan.sizeGate, {
    minimumBytesExclusive: 16 * 1024,
    unit: 'bytes',
    comparator: 'greater_than',
    scope: 'selected fixture source files',
    requiredBefore: 'benchmark_timing_claim',
  });
  assert.deepEqual(
    receipt.governedBenchmark.fixturePlan.selectedFixtureSources.map((entry) => entry.path),
    selectedSourceOwnedFixturePaths,
  );
  assert.equal(receipt.governedBenchmark.fixturePlan.selectedFixtureTotalBytes > 16 * 1024, true);
  assert.deepEqual(
    receipt.governedBenchmark.fixturePlan.selectedFixtureSources.map((entry) => entry.sourceOwned),
    selectedSourceOwnedFixturePaths.map(() => true),
  );
  assert.ok(receipt.governedBenchmark.requiredMetrics.includes('selectedFixtureBytes'));
  assert.equal(
    receipt.governedBenchmark.successCriteria.coldCachePolicy,
    'cold cache measures the first user run reading existing validated .machine artifacts; source-to-machine generation is excluded from timed measurements',
  );
  assert.deepEqual(receipt.governedBenchmark.arms, GOVERNED_BENCHMARK_ARMS);

  const planHash = crypto.createHash('sha256').update('# plan\n').digest('hex');
  assert.deepEqual(receipt.sourceFiles[0], {
    path: 'PLAN.md',
    bytes: 7,
    sha256: planHash,
  });
  assert.equal(
    receipt.sourceFilesSha256,
    crypto.createHash('sha256').update(`${JSON.stringify(receipt.sourceFiles)}\n`).digest('hex'),
  );
  assert.equal(receipt.repos.local.branch, 'dev');
  assert.equal(receipt.repos.local.dirty, true);
  assert.deepEqual(receipt.repos.local.statusShort, [' M packages/bench/package.json']);
  assert.deepEqual(receipt.repos.upstream, {
    collectionStatus: 'required_before_benchmark_claim',
    branch: null,
    commit: null,
    dirty: null,
  });
  assert.deepEqual(receipt.governedBenchmark.cacheEnvMatrix, [
    { arm: 'local-cache-disabled', ROLLDOWN_DX_JSON_CACHE: '0' },
    { arm: 'local-cache-enabled', ROLLDOWN_DX_JSON_CACHE: '1' },
  ]);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('default receipt source files include benchmark and cache-planning surfaces', () => {
  const groups = sourceReceiptModule.SOURCE_RECEIPT_FILE_GROUPS;
  assert.ok(groups && typeof groups === 'object');
  assert.deepEqual(Object.keys(groups), [
    'planningAndWorkspace',
    'rolldownCacheImplementation',
    'benchmarkWorkflowDelegates',
    'benchmarkRunner',
    'governedFixtures',
    'receiptContract',
  ]);
  assert.deepEqual(DEFAULT_SOURCE_RECEIPT_FILES, Object.values(groups).flat());
  assert.equal(new Set(DEFAULT_SOURCE_RECEIPT_FILES).size, DEFAULT_SOURCE_RECEIPT_FILES.length);

  for (const sourcePath of DEFAULT_SOURCE_RECEIPT_FILES) {
    assert.equal(sourcePath.includes('\\'), false, `${sourcePath} must use POSIX separators`);
    assert.equal(sourcePath.startsWith('./'), false, `${sourcePath} must not start with ./`);
    assert.equal(sourcePath.includes('../'), false, `${sourcePath} must not contain ..`);
    assert.equal(sourcePath.includes(':'), false, `${sourcePath} must not contain colon syntax`);
    assert.ok(fs.existsSync(path.resolve(dirname, '../../..', sourcePath)), `${sourcePath} must exist`);
  }

  assert.ok(groups.planningAndWorkspace.includes('meta/design/dx-machine-cache.md'));
  assert.ok(groups.planningAndWorkspace.includes('PLAN.md'));
  assert.ok(groups.rolldownCacheImplementation.includes('crates/rolldown_utils/src/dx_machine_cache.rs'));
  assert.ok(groups.benchmarkWorkflowDelegates.includes('scripts/misc/setup-benchmark-input/index.js'));
  assert.ok(groups.benchmarkRunner.includes('packages/bench/src/run-bundler.ts'));
  assert.deepEqual(groups.governedFixtures, selectedSourceOwnedFixturePaths);
  assert.ok(groups.receiptContract.includes('packages/bench/receipt/current-governed-benchmark.ts'));
  assert.ok(groups.receiptContract.includes('packages/bench/receipt/current-governed-benchmark.test.ts'));
  assert.ok(groups.receiptContract.includes('packages/bench/receipt/governed-execution-gate.ts'));
  assert.ok(groups.receiptContract.includes('packages/bench/receipt/governed-execution-gate.test.ts'));
  assert.ok(groups.receiptContract.includes('.github/workflows/benchmark-node.yml'));
  assert.ok(groups.receiptContract.includes('.github/workflows/benchmark-receipt.yml'));
});

test('source receipt requires official baseline install-origin proof before timing claims', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-official-'));
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

    const officialBaseline = receipt.governedBenchmark.officialBaseline;
    const officialBaselineEvidence =
      receipt.governedBenchmark.executedReceiptRequirements.officialBaselineEvidence;

    assert.equal(
      officialBaseline.installRootPolicy,
      'realpath_outside_local_repo_and_not_workspace_link',
    );
    assert.deepEqual(
      officialBaseline.requiredFieldsBeforeTiming,
      officialBaselineRequiredFields,
    );
    assert.deepEqual(
      officialBaseline.requiredValuesBeforeTiming,
      officialBaselineRequiredValues,
    );
    assert.deepEqual(
      officialBaselineEvidence.requiredFields,
      officialBaselineRequiredFields,
    );
    assert.deepEqual(
      officialBaselineEvidence.requiredValues,
      officialBaselineRequiredValues,
    );
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test('source receipt pins official baseline registry origins before timing claims', () => {
  const repoRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), 'rolldown-source-receipt-official-origin-'),
  );
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

    const officialBaseline = receipt.governedBenchmark.officialBaseline;
    const officialBaselineEvidence =
      receipt.governedBenchmark.executedReceiptRequirements.officialBaselineEvidence;

    assert.ok(officialBaseline.requiredFieldsBeforeTiming.includes('packageRegistryOrigin'));
    assert.ok(officialBaseline.requiredFieldsBeforeTiming.includes('packageResolvedUrlOrigin'));
    assert.equal(
      officialBaseline.requiredValuesBeforeTiming.packageRegistryOrigin,
      'https://registry.npmjs.org',
    );
    assert.equal(
      officialBaseline.requiredValuesBeforeTiming.packageResolvedUrlOrigin,
      'https://registry.npmjs.org',
    );
    assert.ok(officialBaselineEvidence.requiredFields.includes('packageRegistryOrigin'));
    assert.ok(officialBaselineEvidence.requiredFields.includes('packageResolvedUrlOrigin'));
    assert.equal(
      officialBaselineEvidence.requiredValues.packageRegistryOrigin,
      'https://registry.npmjs.org',
    );
    assert.equal(
      officialBaselineEvidence.requiredValues.packageResolvedUrlOrigin,
      'https://registry.npmjs.org',
    );
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test('default receipt source files include benchmark workflow setup and build delegates', () => {
  const workflowDelegateFiles = [
    'package.json',
    'pnpm-workspace.yaml',
    'pnpm-lock.yaml',
    'rust-toolchain.toml',
    '.node-version',
    'tsconfig.json',
    'justfile',
    'scripts/package.json',
    'scripts/meta/constants.js',
    'scripts/meta/utils.js',
    'scripts/misc/setup-benchmark-input/index.js',
    'scripts/misc/setup-benchmark-input/util.js',
    'scripts/misc/setup-benchmark-input/threejs.js',
    'scripts/misc/setup-benchmark-input/threejs-10x.js',
    'scripts/misc/setup-benchmark-input/rome.js',
    'scripts/misc/setup-benchmark-input/rolldown-benchcases.js',
    'scripts/misc/setup-benchmark-input/antd.js',
    'packages/rolldown/package.json',
    'packages/rolldown/build-binding.ts',
    'packages/rolldown/build.ts',
    'packages/rolldown/copy-addon-plugin.ts',
    'packages/rolldown/tsconfig.json',
    'packages/rolldown/tsconfig.check.json',
  ];

  assert.deepEqual(
    workflowDelegateFiles.filter((sourcePath) => !DEFAULT_SOURCE_RECEIPT_FILES.includes(sourcePath)),
    [],
  );
});

test('binding DX serializer feature only forwards declared dependency features', () => {
  assert.deepEqual(
    readCargoFeatureDependencies('crates/rolldown/Cargo.toml', 'dx-serializer-local'),
    ['dep:rkyv', 'dep:serializer', 'rolldown_common/dx-serializer-local'],
  );
  const bindingFeatureEntries = readCargoFeatureDependencies(
    'crates/rolldown_binding/Cargo.toml',
    'dx-serializer-local',
  );
  const bindingDependencyNames = new Set(
    readCargoDependencyNames('crates/rolldown_binding/Cargo.toml'),
  );

  assert.deepEqual(bindingFeatureEntries, [
    'rolldown/dx-serializer-local',
    'rolldown_common/dx-serializer-local',
  ]);
  assert.deepEqual(
    bindingFeatureEntries
      .filter((entry) => entry.includes('/'))
      .map((entry) => entry.split('/')[0])
      .filter((dependencyName) => !bindingDependencyNames.has(dependencyName)),
    [],
  );
});

test('source receipt module stays source-only and avoids benchmark runner imports', () => {
  const source = fs.readFileSync(modulePath, 'utf8');
  const importSpecifiers = [...source.matchAll(/\bimport(?:\s+[^'"]+\s+from\s+|\s*\(\s*)['"]([^'"]+)['"]/g)]
    .map((match) => match[1]);
  const forbiddenPackageRoots = [
    'rolldown',
    'tinybench',
    'rollup',
    'esbuild',
    '@rollup/plugin-commonjs',
    '@rollup/plugin-node-resolve',
  ];
  const forbiddenRelativeRoots = [
    path.resolve(dirname, '../benches'),
    path.resolve(dirname, '../src/bencher'),
    path.resolve(dirname, '../src/run-bundler'),
    path.resolve(dirname, '../src/suites'),
    path.resolve(dirname, '../src/parallel-babel-plugin'),
  ];

  assert.deepEqual(
    importSpecifiers.filter((specifier) =>
      forbiddenPackageRoots.some(
        (root) => specifier === root || specifier.startsWith(`${root}/`),
      ),
    ),
    [],
  );
  assert.deepEqual(
    importSpecifiers.filter((specifier) => {
      if (!specifier.startsWith('.')) {
        return false;
      }
      const resolvedSpecifier = path.resolve(dirname, specifier);
      return forbiddenRelativeRoots.some(
        (root) => resolvedSpecifier === root || resolvedSpecifier.startsWith(`${root}${path.sep}`),
      );
    }),
    [],
  );
  assert.doesNotMatch(
    source,
    /\b(?:spawn|exec|execFile|execFileSync|execSync)\([^)]*(?:bench|compare\.js|ci\.js|par\.js|cargo bench|run bench)/s,
  );
  assert.deepEqual(
    [...source.matchAll(/\bexecFileSync\(\s*['"]([^'"]+)['"]/g)].map((match) => match[1]),
    ['git'],
  );
  assert.match(source, /\bnode:/);
});

test('bench package exposes a source-only receipt script', () => {
  const packageJson = JSON.parse(
    fs.readFileSync(path.join(dirname, '../package.json'), 'utf8'),
  );
  const script = packageJson.scripts['receipt:source'];

  assert.equal(script, 'node ./receipt/source-receipt.ts');
  assert.doesNotMatch(script, /\boxnode\b/);
  assert.doesNotMatch(script, /\b(?:compare|ci|par)\.js\b/);
});

test('bench package exposes a governed dry-run script without timing benchmarks', () => {
  const packageJson = JSON.parse(
    fs.readFileSync(path.join(dirname, '../package.json'), 'utf8'),
  );
  const script = packageJson.scripts['bench:governed:dry'];

  const receipt = createSourceBenchmarkReceipt({
    generatedAt: '2026-05-30T00:00:00.000Z',
    gitInfo: {
      branch: 'dev',
      commit: 'abc123',
      dirty: false,
      statusShort: [],
    },
    repoRoot: path.resolve(dirname, '../../..'),
  });

  assert.equal(script, receipt.governedBenchmark.dryRun.command);
  assert.doesNotMatch(script, /\boxnode\b/);
  assert.doesNotMatch(script, /\b(?:compare|ci|par)\.js\b/);
  assert.doesNotMatch(script, /\b(?:bench|benchmark|cargo build|just build)\b/);
});

test('source receipt validation rejects governed dry-run weakening', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-dry-run-'));
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

  receipt.governedBenchmark.dryRun.executesBenchmarks = true;
  receipt.governedBenchmark.dryRun.importsBenchmarkRunner = true;
  receipt.governedBenchmark.dryRun.writesBenchmarkResults = true;
  receipt.governedBenchmark.dryRun.blockedCommands =
    receipt.governedBenchmark.dryRun.blockedCommands.filter(
      (command) => command !== 'pnpm --filter bench bench-ci',
    );
  receipt.governedBenchmark.dryRun.allowedClaims.push('local_cache_wins');

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');
  assert.equal(validation.ok, false);
  assert.match(errors, /governedBenchmark\.dryRun mismatch/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('benchmark-node workflow excludes receipt-only bench changes from heavy benchmark runs', () => {
  const workflowPath = path.resolve(dirname, '../../../.github/workflows/benchmark-node.yml');
  const workflow = fs.readFileSync(workflowPath, 'utf8');
  const benchIncludeIndex = workflow.indexOf("      - 'packages/bench/**'");
  const receiptExcludeIndex = workflow.indexOf("      - '!packages/bench/receipt/**'");

  assert.notEqual(benchIncludeIndex, -1);
  assert.notEqual(receiptExcludeIndex, -1);
  assert.ok(receiptExcludeIndex > benchIncludeIndex);
  assert.doesNotMatch(workflow, /^\s*paths-ignore:/m);
});

test('benchmark receipt workflow runs source-only receipt tests without heavy benchmark commands', () => {
  const workflowPath = path.resolve(dirname, '../../../.github/workflows/benchmark-receipt.yml');
  const workflow = fs.readFileSync(workflowPath, 'utf8');

  assert.match(workflow, /^name: Benchmark Receipt$/m);
  assert.match(workflow, /^\s*permissions: \{\}$/m);
  assert.match(workflow, /^\s*- 'packages\/bench\/receipt\/\*\*'$/m);
  assert.match(workflow, /^\s*- 'packages\/bench\/package\.json'$/m);
  assert.match(workflow, /^\s*- '\.github\/workflows\/benchmark-node\.yml'$/m);
  assert.match(workflow, /^\s*- '\.github\/workflows\/benchmark-receipt\.yml'$/m);
  assert.match(
    workflow,
    /^\s*run: node --test packages\/bench\/receipt\/source-receipt\.test\.ts packages\/bench\/receipt\/governed-execution-gate\.test\.ts packages\/bench\/receipt\/lighthouse-package-contract\.test\.ts$/m,
  );
  assert.doesNotMatch(workflow, /\b(?:oxnode|setup-bench|bench-ci|build-rolldown-release)\b/);
  assert.doesNotMatch(workflow, /github-action-benchmark|copy_file_to_another_repo_action/);
  assert.doesNotMatch(workflow, /^\s*paths-ignore:/m);
});

test('source receipt validation accepts a fresh source-only receipt', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-validate-'));
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

  assert.deepEqual(validateSourceBenchmarkReceipt(receipt, { repoRoot }), {
    ok: true,
    errors: [],
  });

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt requires local build provenance before timing claims', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-local-build-'));
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

  assert.deepEqual(receipt.governedBenchmark.executedReceiptRequirements.localBuildEvidence, {
    arm: 'local-cache-enabled',
    buildTimingPolicy: 'built_before_timed_section',
    requiredFields: localBuildRequiredFields,
    requiredValues: localBuildRequiredValues,
  });

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects benchmark overclaim fields', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-overclaim-'));
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

  receipt.claimStatus = 'proven';
  receipt.benchmarkStatus = 'run';
  receipt.upstreamComparison = 'measured';
  receipt.speedupClaim = '2x faster';
  receipt.benchmarkExecution.executed = true;
  receipt.repos.upstream.collectionStatus = 'collected';

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /claimStatus must be not_proven_source_receipt_only/);
  assert.match(errors, /benchmarkStatus must be not_run/);
  assert.match(errors, /upstreamComparison must be not_measured/);
  assert.match(errors, /speedupClaim must be none/);
  assert.match(errors, /benchmarkExecution\.executed must be false/);
  assert.match(errors, /benchmarkExecution mismatch/);
  assert.match(errors, /upstream collection must remain required before benchmark claim/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects stale source hashes and result-like fields', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-stale-'));
  fs.writeFileSync(path.join(repoRoot, 'PLAN.md'), '# plan\n', 'utf8');
  writeSelectedSourceOwnedFixture(repoRoot);
  const receipt = createSourceBenchmarkReceipt({
    generatedAt: '2026-05-30T00:00:00.000Z',
    gitInfo: {
      branch: 'dev',
      commit: 'abc123',
      dirty: false,
      statusShort: [],
    },
    repoRoot,
    sourceFiles: ['PLAN.md'],
  });

  receipt.results = [];
  receipt.winner = 'local-cache-enabled';
  receipt.timings = [];
  receipt.governedBenchmark.extra = 'unexpected';
  fs.writeFileSync(path.join(repoRoot, 'PLAN.md'), '# changed\n', 'utf8');

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  assert.equal(validation.ok, false);
  assert.match(validation.errors.join('\n'), /sourceFiles\[0\]\.sha256 mismatch for PLAN\.md/);
  assert.match(validation.errors.join('\n'), /forbidden source-only result field: results/);
  assert.match(validation.errors.join('\n'), /forbidden source-only result field: winner/);
  assert.match(validation.errors.join('\n'), /forbidden source-only field: timings/);
  assert.match(validation.errors.join('\n'), /forbidden source-only field: governedBenchmark\.extra/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects stale local git provenance', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-git-'));
  writeDefaultSourceReceiptFiles(repoRoot);
  childProcess.execFileSync('git', ['init'], { cwd: repoRoot, stdio: 'ignore' });
  childProcess.execFileSync('git', ['config', 'user.email', 'dx@example.invalid'], {
    cwd: repoRoot,
    stdio: 'ignore',
  });
  childProcess.execFileSync('git', ['config', 'user.name', 'DX Receipt Test'], {
    cwd: repoRoot,
    stdio: 'ignore',
  });
  childProcess.execFileSync('git', ['add', '.'], { cwd: repoRoot, stdio: 'ignore' });
  childProcess.execFileSync('git', ['commit', '-m', 'seed receipt files'], {
    cwd: repoRoot,
    stdio: 'ignore',
  });
  const receipt = createSourceBenchmarkReceipt({
    generatedAt: '2026-05-30T00:00:00.000Z',
    repoRoot,
  });
  assert.equal(validateSourceBenchmarkReceipt(receipt, { repoRoot }).ok, true);

  fs.writeFileSync(path.join(repoRoot, 'untracked.txt'), 'dirty\n', 'utf8');

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');
  assert.equal(validation.ok, false);
  assert.match(errors, /repos\.local\.dirty must match current git status/);
  assert.match(errors, /repos\.local\.statusShort must match current git status/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects nested benchmark result-like fields', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-nested-'));
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
  receipt.governedBenchmark.arms[0].benchmarkResults = [
    { medianMs: 12, outputHash: 'abc123' },
  ];

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(
    errors,
    /forbidden source-only result field: governedBenchmark\.arms\.0\.benchmarkResults/,
  );
  assert.match(
    errors,
    /forbidden source-only result field: governedBenchmark\.arms\.0\.benchmarkResults\.0\.medianMs/,
  );
  assert.match(
    errors,
    /forbidden source-only result field: governedBenchmark\.arms\.0\.benchmarkResults\.0\.outputHash/,
  );

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation requires current repo root and complete source set', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-required-'));
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
  receipt.repos.local.root = path.join(repoRoot, 'elsewhere');
  receipt.sourceFiles = receipt.sourceFiles.slice(0, 2);
  receipt.sourceFiles.push(receipt.sourceFiles[0]);

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  assert.equal(validation.ok, false);
  assert.match(validation.errors.join('\n'), /repos\.local\.root must match validation repo root/);
  assert.match(validation.errors.join('\n'), /sourceFiles missing required path: packages\/bench\/receipt\/source-receipt\.ts/);
  assert.match(validation.errors.join('\n'), /sourceFiles has duplicate path: meta\/design\/dx-machine-cache\.md/);
  assert.match(validation.errors.join('\n'), /sourceFilesSha256 mismatch for sourceFiles/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects unsafe source paths before file IO', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-path-'));
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
  receipt.sourceFiles[0] = { path: 'C:PLAN.md', bytes: 0, sha256: 'x' };
  receipt.sourceFiles[1] = { path: '../Cargo.lock', bytes: 0, sha256: 'x' };
  receipt.sourceFiles[2] = { path: './packages/bench/package.json', bytes: 0, sha256: 'x' };
  receipt.sourceFiles[3] = { path: 'packages/bench/./ci.js', bytes: 0, sha256: 'x' };
  receipt.sourceFiles[4] = { path: 'PLAN.md:Zone.Identifier', bytes: 0, sha256: 'x' };

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');
  assert.equal(validation.ok, false);
  assert.match(errors, /sourceFiles\[0\]\.path must be safe repo-relative: C:PLAN\.md/);
  assert.match(errors, /sourceFiles\[1\]\.path must be safe repo-relative: \.\.\/Cargo\.lock/);
  assert.match(errors, /sourceFiles\[2\]\.path must be safe repo-relative: \.\/packages\/bench\/package\.json/);
  assert.match(errors, /sourceFiles\[3\]\.path must be safe repo-relative: packages\/bench\/\.\/ci\.js/);
  assert.match(errors, /sourceFiles\[4\]\.path must be safe repo-relative: PLAN\.md:Zone\.Identifier/);
  assert.doesNotMatch(errors, /could not be read/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects source-owned fixture realpath escapes', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-realpath-'));
  const outsideRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), 'rolldown-source-receipt-realpath-outside-'),
  );

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

    const inRepoFixtureDir = path.dirname(path.join(repoRoot, selectedSourceOwnedFixturePath));
    const outsideFixtureDir = path.join(outsideRoot, 'side-effects-named');
    fs.mkdirSync(outsideFixtureDir, { recursive: true });
    fs.writeFileSync(
      path.join(outsideFixtureDir, path.basename(selectedSourceOwnedFixturePath)),
      sourceReceiptFileContents(selectedSourceOwnedFixturePath),
      'utf8',
    );
    fs.rmSync(inRepoFixtureDir, { recursive: true, force: true });
    fs.symlinkSync(
      outsideFixtureDir,
      inRepoFixtureDir,
      process.platform === 'win32' ? 'junction' : 'dir',
    );

    const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
    const errors = validation.errors.join('\n');
    assert.equal(validation.ok, false);
    assert.match(errors, /must resolve inside repo root/);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
    fs.rmSync(outsideRoot, { recursive: true, force: true });
  }
});

test('source receipt validation rejects governed benchmark contract drift', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-contract-'));
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

  assert.equal(receipt.governedBenchmark.successCriteria?.minimumIterations, 30);
  receipt.benchmarkExecution.reason = 'benchmarks already ran';
  receipt.governedBenchmark.requiredEnvironmentFields = ['nodeVersion'];
  receipt.governedBenchmark.cacheEnvMatrix[0].ROLLDOWN_DX_JSON_CACHE = '1';
  receipt.governedBenchmark.officialBaseline.installRootPolicy = 'package_root_unknown';
  receipt.governedBenchmark.officialBaseline.globalInstallAllowed = true;
  receipt.governedBenchmark.officialBaseline.requiredValuesBeforeTiming.workspaceLink = true;
  receipt.governedBenchmark.buildOutsideTimedSection = false;
  receipt.governedBenchmark.outputEquivalence.mustMatchAcrossArms = ['local-cache-enabled'];
  receipt.governedBenchmark.fixturePlan.selection = 'ad hoc fixture';
  receipt.governedBenchmark.fixturePlan.sizeGate.minimumBytesExclusive = 1024;
  receipt.governedBenchmark.cacheEvidence.requiredFields = ['fileCount'];
  const officialBaselineEvidence =
    receipt.governedBenchmark.executedReceiptRequirements.officialBaselineEvidence;
  officialBaselineEvidence.requiredValues.workspaceLink = true;
  receipt.governedBenchmark.executedReceiptRequirements.perArmEvidence.arms = [
    'local-cache-enabled',
  ];
  receipt.governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.requiredFields = [
    'dxRolldownFileCount',
  ];
  receipt.governedBenchmark.executedReceiptRequirements.claimPolicy.speedupClaimAllowedOnlyAfter =
    'benchmark_command_exits_zero';
  receipt.governedBenchmark.successCriteria.minimumIterations = 1;

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  assert.equal(validation.ok, false);
  assert.match(validation.errors.join('\n'), /benchmarkExecution mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.requiredEnvironmentFields mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.cacheEnvMatrix mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.officialBaseline mismatch/);
  assert.match(validation.errors.join('\n'), /official baseline must reject global installs/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.buildOutsideTimedSection mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.outputEquivalence mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.fixturePlan mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.cacheEvidence mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.executedReceiptRequirements mismatch/);
  assert.match(validation.errors.join('\n'), /governedBenchmark\.successCriteria mismatch/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt validation rejects statistically weak speedup claim gates', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-speedup-gate-'));
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

    assert.deepEqual(receipt.governedBenchmark.successCriteria.speedupClaimValidation, {
      minimumRelativeImprovementPct: 5,
      minimumAbsoluteImprovementMs: 1,
      confidenceLevel: 0.95,
      requireConfidenceIntervalExcludesZero: true,
    });

    receipt.governedBenchmark.successCriteria.speedupClaimValidation.minimumRelativeImprovementPct = 0.1;
    receipt.governedBenchmark.successCriteria.speedupClaimValidation.minimumAbsoluteImprovementMs = 0;
    receipt.governedBenchmark.successCriteria.speedupClaimValidation.confidenceLevel = 0.5;
    receipt.governedBenchmark.successCriteria.speedupClaimValidation.requireConfidenceIntervalExcludesZero = false;

    const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
    assert.equal(validation.ok, false);
    assert.match(validation.errors.join('\n'), /governedBenchmark\.successCriteria mismatch/);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test('source receipt validation rejects fixture size gate weakening and overclaims', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-size-gate-'));
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

  receipt.governedBenchmark.fixturePlan.sizeGate.minimumBytesExclusive = 1024;
  receipt.governedBenchmark.fixturePlan.sizeGate.extra = 'loose';
  receipt.governedBenchmark.fixturePlan.sizeGate.results = [{ medianMs: 1 }];

  const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
  const errors = validation.errors.join('\n');
  assert.equal(validation.ok, false);
  assert.match(errors, /governedBenchmark\.fixturePlan mismatch/);
  assert.match(errors, /forbidden source-only field: governedBenchmark\.fixturePlan\.sizeGate\.extra/);
  assert.match(errors, /forbidden source-only result field: governedBenchmark\.fixturePlan\.sizeGate\.results/);
  assert.match(errors, /forbidden source-only result field: governedBenchmark\.fixturePlan\.sizeGate\.results\.0\.medianMs/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt CLI keeps output and validation paths inside repo root with clean JSON errors', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-cli-safe-'));
  const outsideRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-outside-'));
  const outsideOutPath = path.join(outsideRoot, 'benchmark-source-receipt.json');
  const outsideValidatePath = path.join(outsideRoot, 'outside-receipt.json');
  const badJsonPath = path.join(repoRoot, '.dx/rolldown/bad.json');
  const largeJsonPath = path.join(repoRoot, '.dx/rolldown/large.json');
  writeDefaultSourceReceiptFiles(repoRoot);
  fs.mkdirSync(path.dirname(badJsonPath), { recursive: true });
  fs.writeFileSync(outsideValidatePath, '{}\n', 'utf8');
  fs.writeFileSync(badJsonPath, '{not json}\n', 'utf8');
  fs.writeFileSync(largeJsonPath, `${' '.repeat(1024 * 1024 + 1)}\n`, 'utf8');

  const outsideOut = childProcess.spawnSync(
    'node',
    [modulePath, '--repo-root', repoRoot, '--out', outsideOutPath],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  assert.notEqual(outsideOut.status, 0);
  assert.match(outsideOut.stderr, /--out must stay inside repo root/);
  assert.equal(fs.existsSync(outsideOutPath), false);

  const outsideValidate = childProcess.spawnSync(
    'node',
    [modulePath, '--repo-root', repoRoot, '--validate', outsideValidatePath],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  assert.notEqual(outsideValidate.status, 0);
  assert.match(outsideValidate.stderr, /--validate must stay inside repo root/);

  const badJson = childProcess.spawnSync(
    'node',
    [modulePath, '--repo-root', repoRoot, '--validate', badJsonPath],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  assert.notEqual(badJson.status, 0);
  assert.match(badJson.stderr, /Invalid source receipt JSON/);
  assert.doesNotMatch(badJson.stderr, /SyntaxError|at .*source-receipt\.ts/);

  const largeJson = childProcess.spawnSync(
    'node',
    [modulePath, '--repo-root', repoRoot, '--validate', largeJsonPath],
    { cwd: repoRoot, encoding: 'utf8' },
  );
  assert.notEqual(largeJson.status, 0);
  assert.match(largeJson.stderr, /source receipt JSON too large/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
  fs.rmSync(outsideRoot, { recursive: true, force: true });
});

test('source receipt CLI validates an existing source-only receipt without running benchmarks', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-cli-'));
  const outPath = path.join(repoRoot, '.dx/rolldown/benchmark-source-receipt.json');
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
  writeSourceBenchmarkReceipt(outPath, receipt);

  const output = childProcess.execFileSync('node', [modulePath, '--repo-root', repoRoot, '--validate', outPath], {
    cwd: repoRoot,
    encoding: 'utf8',
  });

  assert.match(output, /valid source receipt/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('source receipt CLI rejects benchmark overclaims', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-cli-overclaim-'));
  const outPath = path.join(repoRoot, '.dx/rolldown/overclaim-source-receipt.json');
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
  receipt.benchmarkStatus = 'measured';
  receipt.upstreamComparison = 'local_faster';
  receipt.speedupClaim = 'local-cache-enabled faster';
  receipt.benchmarkExecution = { executed: true, reason: 'benchmarks already ran' };
  receipt.results = [{ arm: 'local-cache-enabled', medianMs: 1 }];
  receipt.winner = 'local-cache-enabled';
  writeSourceBenchmarkReceipt(outPath, receipt);

  const overclaim = childProcess.spawnSync(
    'node',
    [modulePath, '--repo-root', repoRoot, '--validate', outPath],
    { cwd: repoRoot, encoding: 'utf8' },
  );

  assert.notEqual(overclaim.status, 0);
  assert.equal(overclaim.stdout, '');
  assert.match(overclaim.stderr, /benchmarkStatus must be not_run/);
  assert.match(overclaim.stderr, /upstreamComparison must be not_measured/);
  assert.match(overclaim.stderr, /speedupClaim must be none/);
  assert.match(overclaim.stderr, /benchmarkExecution\.executed must be false/);
  assert.match(overclaim.stderr, /benchmarkExecution mismatch/);
  assert.match(overclaim.stderr, /forbidden source-only result field: results/);
  assert.match(overclaim.stderr, /forbidden source-only result field: results\.0\.medianMs/);
  assert.match(overclaim.stderr, /forbidden source-only result field: winner/);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});

test('writeSourceBenchmarkReceipt writes pretty JSON with a trailing newline', () => {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rolldown-source-receipt-write-'));
  const outPath = path.join(repoRoot, '.dx/rolldown/benchmark-source-receipt.json');
  fs.writeFileSync(path.join(repoRoot, 'PLAN.md'), '# plan\n', 'utf8');
  writeSelectedSourceOwnedFixture(repoRoot);

  const receipt = createSourceBenchmarkReceipt({
    generatedAt: '2026-05-30T00:00:00.000Z',
    gitInfo: {
      branch: 'dev',
      commit: 'abc123',
      dirty: false,
      statusShort: [],
    },
    repoRoot,
    sourceFiles: ['PLAN.md'],
  });

  writeSourceBenchmarkReceipt(outPath, receipt);

  const written = fs.readFileSync(outPath, 'utf8');
  assert.equal(written.endsWith('\n'), true);
  assert.deepEqual(JSON.parse(written), receipt);

  fs.rmSync(repoRoot, { recursive: true, force: true });
});
