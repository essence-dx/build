import childProcess from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

const dirname = path.dirname(url.fileURLToPath(import.meta.url));

export const SOURCE_RECEIPT_CLAIM_STATUS = 'not_proven_source_receipt_only';

export const SOURCE_RECEIPT_FILE_GROUPS = {
  planningAndWorkspace: [
    'meta/design/dx-machine-cache.md',
    'PLAN.md',
    '.gitignore',
    'Cargo.toml',
    'Cargo.lock',
    'package.json',
    'pnpm-workspace.yaml',
    'pnpm-lock.yaml',
    'rust-toolchain.toml',
    '.node-version',
    'tsconfig.json',
    'justfile',
  ],
  rolldownCacheImplementation: [
    'crates/rolldown/Cargo.toml',
    'crates/rolldown/src/utils/dx_machine_cache.rs',
    'crates/rolldown/src/utils/load_source.rs',
    'crates/rolldown/src/utils/mod.rs',
    'crates/rolldown/src/utils/parse_to_ecma_ast.rs',
    'crates/rolldown_binding/Cargo.toml',
    'crates/rolldown_binding/src/transform_cache.rs',
    'crates/rolldown_common/Cargo.toml',
    'crates/rolldown_common/src/lib.rs',
    'crates/rolldown_common/src/types/package_json.rs',
    'crates/rolldown_common/src/types/str_or_bytes.rs',
    'crates/rolldown_fs/src/file_system.rs',
    'crates/rolldown_fs/src/memory.rs',
    'crates/rolldown_plugin_utils/Cargo.toml',
    'crates/rolldown_plugin_utils/src/file_to_url.rs',
    'crates/rolldown_plugin_vite_asset/Cargo.toml',
    'crates/rolldown_plugin_vite_asset/src/lib.rs',
    'crates/rolldown_plugin_vite_asset/tests/raw_asset_loader_source.rs',
    'crates/rolldown_plugin_vite_css_post/Cargo.toml',
    'crates/rolldown_plugin_vite_css_post/src/utils.rs',
    'crates/rolldown_plugin_vite_json/Cargo.toml',
    'crates/rolldown_plugin_vite_json/src/lib.rs',
    'crates/rolldown_plugin_vite_resolve/Cargo.toml',
    'crates/rolldown_plugin_vite_resolve/src/package_json_cache.rs',
    'crates/rolldown_plugin_vite_resolve/src/resolver.rs',
    'crates/rolldown_plugin_vite_resolve/src/vite_resolve_plugin.rs',
    'crates/rolldown_resolver/Cargo.toml',
    'crates/rolldown_resolver/src/resolver.rs',
    'crates/rolldown_resolver/tests/package_json_machine_cache.rs',
    'crates/rolldown_utils/Cargo.toml',
    'crates/rolldown_utils/src/dx_machine_cache.rs',
    'crates/rolldown_utils/src/lib.rs',
    'crates/bench/src/lib.rs',
  ],
  benchmarkWorkflowDelegates: [
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
  ],
  benchmarkRunner: [
    'packages/bench/package.json',
    'packages/bench/benches/ci.js',
    'packages/bench/benches/compare.js',
    'packages/bench/benches/par.js',
    'packages/bench/src/bencher.ts',
    'packages/bench/src/parallel-babel-plugin/impl.ts',
    'packages/bench/src/parallel-babel-plugin/index.ts',
    'packages/bench/src/types.d.ts',
    'packages/bench/src/utils.ts',
    'packages/bench/src/suites/index.ts',
    'packages/bench/src/suites/rome-ts.ts',
    'packages/bench/src/run-bundler.ts',
    'packages/bench/tsconfig.json',
    'packages/bench/vue-entry.js',
  ],
  governedFixtures: [
    'packages/rolldown/tests/fixtures/builtin-plugin/json/side-effects-named/large.json',
    'packages/bench/fixtures/dx-machine-cache/package-json-artifacts/package.json',
  ],
  receiptContract: [
    'packages/bench/receipt/current-governed-benchmark.ts',
    'packages/bench/receipt/current-governed-benchmark.test.ts',
    'packages/bench/receipt/governed-execution-gate.ts',
    'packages/bench/receipt/governed-execution-gate.test.ts',
    'packages/bench/receipt/governed-benchmark.ts',
    'packages/bench/receipt/source-receipt.ts',
    'packages/bench/receipt/source-receipt.test.ts',
    '.github/workflows/benchmark-node.yml',
    '.github/workflows/benchmark-receipt.yml',
  ],
};

export const DEFAULT_SOURCE_RECEIPT_FILES = [
  ...SOURCE_RECEIPT_FILE_GROUPS.planningAndWorkspace,
  ...SOURCE_RECEIPT_FILE_GROUPS.rolldownCacheImplementation,
  ...SOURCE_RECEIPT_FILE_GROUPS.benchmarkWorkflowDelegates,
  ...SOURCE_RECEIPT_FILE_GROUPS.benchmarkRunner,
  ...SOURCE_RECEIPT_FILE_GROUPS.governedFixtures,
  ...SOURCE_RECEIPT_FILE_GROUPS.receiptContract,
];

export const GOVERNED_BENCHMARK_ARMS = [
  {
    id: 'upstream-stock',
    subject: 'upstream',
    env: {},
    cacheMode: 'not_applicable',
  },
  {
    id: 'local-cache-disabled',
    subject: 'local',
    env: { ROLLDOWN_DX_JSON_CACHE: '0' },
    cacheMode: 'disabled',
  },
  {
    id: 'local-cache-enabled',
    subject: 'local',
    env: { ROLLDOWN_DX_JSON_CACHE: '1' },
    cacheMode: 'enabled',
  },
];

export const REQUIRED_BENCHMARK_METRICS = [
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
  'dxRolldownFileCount',
  'dxRolldownBytes',
];

const REQUIRED_BENCHMARK_ENVIRONMENT_FIELDS = [
  'nodeVersion',
  'rustcVersion',
  'platform',
  'cpu',
  'powerThermalNotes',
  'ROLLDOWN_DX_JSON_CACHE',
];

const GOVERNED_BENCHMARK_CACHE_ENV_MATRIX = [
  { arm: 'local-cache-disabled', ROLLDOWN_DX_JSON_CACHE: '0' },
  { arm: 'local-cache-enabled', ROLLDOWN_DX_JSON_CACHE: '1' },
];

const GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_FIELDS = [
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

const GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_VALUES = {
  packageName: 'rolldown',
  packageRegistryOrigin: 'https://registry.npmjs.org',
  packageResolvedUrlOrigin: 'https://registry.npmjs.org',
  globalInstall: false,
  workspaceLink: false,
};

const GOVERNED_BENCHMARK_LOCAL_BUILD_REQUIRED_FIELDS = [
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

const GOVERNED_BENCHMARK_LOCAL_BUILD_REQUIRED_VALUES = {
  gitDirty: false,
  buildProfile: 'release',
  buildFeatures: ['dx_json_cache'],
  builtOutsideTimedSection: true,
};

const GOVERNED_BENCHMARK_OFFICIAL_BASELINE = {
  arm: 'upstream-stock',
  packageName: 'rolldown',
  installMode: 'clean_pinned_package_install',
  installRootPolicy: 'realpath_outside_local_repo_and_not_workspace_link',
  globalInstallAllowed: false,
  requiredFieldsBeforeTiming: GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_FIELDS,
  requiredValuesBeforeTiming: GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_VALUES,
  notes: 'global installs are not accepted as official baseline evidence',
};

const GOVERNED_BENCHMARK_DRY_RUN = {
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

const GOVERNED_BENCHMARK_MACHINE_ARTIFACT_PREPARATION = {
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

const GOVERNED_BENCHMARK_SOURCE_RECEIPT_REQUIRED_FIELDS = [
  'path',
  'sha256',
  'sourceFilesSha256',
  'gitCommit',
  'gitDirty',
];
const GOVERNED_BENCHMARK_SOURCE_RECEIPT_REQUIRED_VALUES = {
  path: '.dx/rolldown/benchmark-source-receipt.json',
  gitDirty: false,
};

export const GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS = {
  receiptKind: 'governed_benchmark_execution',
  sourceReceiptEvidence: {
    requiredFields: GOVERNED_BENCHMARK_SOURCE_RECEIPT_REQUIRED_FIELDS,
    requiredValues: GOVERNED_BENCHMARK_SOURCE_RECEIPT_REQUIRED_VALUES,
    sourceFilesHashAlgorithm: 'sha256',
  },
  officialBaselineEvidence: {
    arm: 'upstream-stock',
    installMode: 'clean_pinned_package_install',
    requiredFields: GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_FIELDS,
    requiredValues: GOVERNED_BENCHMARK_OFFICIAL_BASELINE_REQUIRED_VALUES,
  },
  localBuildEvidence: {
    arm: 'local-cache-enabled',
    buildTimingPolicy: 'built_before_timed_section',
    requiredFields: GOVERNED_BENCHMARK_LOCAL_BUILD_REQUIRED_FIELDS,
    requiredValues: GOVERNED_BENCHMARK_LOCAL_BUILD_REQUIRED_VALUES,
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

const GOVERNED_BENCHMARK_OUTPUT_EQUIVALENCE = {
  required: true,
  hashAlgorithm: 'sha256',
  scope: 'normalized output tree',
  mustMatchAcrossArms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
};

const GOVERNED_BENCHMARK_FIXTURE_PLAN = {
  sourcePaths: [
    'packages/bench/src/suites/index.ts',
    'packages/bench/src/suites/rome-ts.ts',
    'packages/bench/vue-entry.js',
  ],
  selectedFixtureSourcePaths: [
    'packages/rolldown/tests/fixtures/builtin-plugin/json/side-effects-named/large.json',
    'packages/bench/fixtures/dx-machine-cache/package-json-artifacts/package.json',
  ],
  selection: 'future governed timing window chooses exact fixture subset before execution',
  sizeGate: {
    minimumBytesExclusive: 16 * 1024,
    unit: 'bytes',
    comparator: 'greater_than',
    scope: 'selected fixture source files',
    requiredBefore: 'benchmark_timing_claim',
  },
};

const GOVERNED_BENCHMARK_CACHE_EVIDENCE = {
  root: '.dx/rolldown',
  requiredFields:
    GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.requiredFields,
};

export const GOVERNED_BENCHMARK_SUCCESS_CRITERIA = {
  minimumIterations: 30,
  minimumWarmupIterations: 5,
  hotCacheMetric: 'medianMs',
  hotCacheMustBeat: ['upstream-stock', 'local-cache-disabled'],
  p95MustBeat: ['upstream-stock', 'local-cache-disabled'],
  outputHashMustMatchAcrossArms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
  coldCachePolicy:
    'cold cache measures the first user run reading existing validated .machine artifacts; source-to-machine generation is excluded from timed measurements',
  noisePolicy: 'stdDevMs and powerThermalNotes must be recorded before any speedup claim',
  speedupClaimValidation: {
    minimumRelativeImprovementPct: 5,
    minimumAbsoluteImprovementMs: 1,
    confidenceLevel: 0.95,
    requireConfidenceIntervalExcludesZero: true,
  },
};

const SOURCE_ONLY_BENCHMARK_EXECUTION = {
  executed: false,
  reason: 'source receipt only; no benchmark command executed',
};

const MAX_SOURCE_RECEIPT_JSON_BYTES = 1024 * 1024;

const FORBIDDEN_RESULT_FIELDS = new Set([
  'results',
  'winner',
  'benchmarkResults',
  'measurements',
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
  'durationMs',
  'samples',
  'outputHash',
  'dxRolldownFileCount',
  'dxRolldownBytes',
  'evidence',
  'benchmarkPayload',
]);

const KNOWN_SOURCE_RECEIPT_FIELDS = new Map([
  [
    '',
    new Set([
      'schema',
      'claimStatus',
      'benchmarkStatus',
      'upstreamComparison',
      'speedupClaim',
      'generatedAt',
      'repos',
      'sourceFiles',
      'sourceFilesSha256',
      'governedBenchmark',
      'benchmarkExecution',
      'environment',
    ]),
  ],
  ['repos', new Set(['local', 'upstream'])],
  ['repos.local', new Set(['root', 'branch', 'commit', 'dirty', 'statusShort'])],
  ['repos.upstream', new Set(['collectionStatus', 'branch', 'commit', 'dirty'])],
  ['sourceFiles.*', new Set(['path', 'bytes', 'sha256'])],
  [
    'governedBenchmark',
    new Set([
      'arms',
      'requiredMetrics',
      'requiredEnvironmentFields',
      'cacheEnvMatrix',
      'officialBaseline',
      'dryRun',
      'buildOutsideTimedSection',
      'outputEquivalence',
      'machineArtifactPreparation',
      'fixturePlan',
      'cacheEvidence',
      'executedReceiptRequirements',
      'successCriteria',
    ]),
  ],
  ['governedBenchmark.arms.*', new Set(['id', 'subject', 'env', 'cacheMode'])],
  ['governedBenchmark.arms.*.env', new Set(['ROLLDOWN_DX_JSON_CACHE'])],
  ['governedBenchmark.cacheEnvMatrix.*', new Set(['arm', 'ROLLDOWN_DX_JSON_CACHE'])],
  [
    'governedBenchmark.officialBaseline',
    new Set([
      'arm',
      'packageName',
      'installMode',
      'installRootPolicy',
      'globalInstallAllowed',
      'requiredFieldsBeforeTiming',
      'requiredValuesBeforeTiming',
      'notes',
    ]),
  ],
  [
    'governedBenchmark.officialBaseline.requiredValuesBeforeTiming',
    new Set([
      'packageName',
      'packageRegistryOrigin',
      'packageResolvedUrlOrigin',
      'globalInstall',
      'workspaceLink',
    ]),
  ],
  [
    'governedBenchmark.dryRun',
    new Set([
      'id',
      'script',
      'command',
      'executesBenchmarks',
      'importsBenchmarkRunner',
      'writesBenchmarkResults',
      'allowedOutputPath',
      'blockedCommands',
      'allowedClaims',
      'forbiddenClaims',
    ]),
  ],
  [
    'governedBenchmark.outputEquivalence',
    new Set(['required', 'hashAlgorithm', 'scope', 'mustMatchAcrossArms']),
  ],
  [
    'governedBenchmark.machineArtifactPreparation',
    new Set([
      'model',
      'countedInTimedSection',
      'requiredBeforeTiming',
      'excludedFromTiming',
      'coldCacheDefinition',
      'hotCacheDefinition',
    ]),
  ],
  [
    'governedBenchmark.fixturePlan',
    new Set([
      'sourcePaths',
      'selectedFixtureSourcePaths',
      'selectedFixtureSources',
      'selectedFixtureTotalBytes',
      'selection',
      'sizeGate',
    ]),
  ],
  [
    'governedBenchmark.fixturePlan.selectedFixtureSources.*',
    new Set(['path', 'bytes', 'sha256', 'sourceOwned']),
  ],
  [
    'governedBenchmark.fixturePlan.sizeGate',
    new Set(['minimumBytesExclusive', 'unit', 'comparator', 'scope', 'requiredBefore']),
  ],
  ['governedBenchmark.cacheEvidence', new Set(['root', 'requiredFields'])],
  [
    'governedBenchmark.executedReceiptRequirements',
    new Set([
      'receiptKind',
      'sourceReceiptEvidence',
      'officialBaselineEvidence',
      'localBuildEvidence',
      'perArmEvidence',
      'cacheArtifactEvidence',
      'outputEquality',
      'timingWindowEvidence',
      'claimPolicy',
    ]),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.sourceReceiptEvidence',
    new Set(['requiredFields', 'requiredValues', 'sourceFilesHashAlgorithm']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.sourceReceiptEvidence.requiredValues',
    new Set(['path', 'gitDirty']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.officialBaselineEvidence',
    new Set(['arm', 'installMode', 'requiredFields', 'requiredValues']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.officialBaselineEvidence.requiredValues',
    new Set([
      'packageName',
      'packageRegistryOrigin',
      'packageResolvedUrlOrigin',
      'globalInstall',
      'workspaceLink',
    ]),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.localBuildEvidence',
    new Set(['arm', 'buildTimingPolicy', 'requiredFields', 'requiredValues']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.localBuildEvidence.requiredValues',
    new Set(['gitDirty', 'buildProfile', 'buildFeatures', 'builtOutsideTimedSection']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.perArmEvidence',
    new Set(['arms', 'requiredFields']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence',
    new Set([
      'arms',
      'root',
      'requiredFields',
      'requiredMachineMagic',
      'requiredHotCacheCounters',
      'forbiddenMachineMagic',
      'requiredMachineArtifactBenefits',
    ]),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.requiredMachineMagic.*',
    new Set(['artifactKind', 'magic', 'minimumCount']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.requiredHotCacheCounters.*',
    new Set(['artifactKind', 'magic', 'exact', 'minimum']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.forbiddenMachineMagic.*',
    new Set(['artifactKind', 'magic']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.cacheArtifactEvidence.requiredMachineArtifactBenefits.*',
    new Set([
      'artifactKind',
      'magic',
      'allowedSourceShapes',
      'machineBytesMustBeLessThanSourceBytes',
    ]),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.outputEquality',
    new Set(['hashAlgorithm', 'mustMatchAcrossArms']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.timingWindowEvidence',
    new Set(['requiredFields', 'requiredValues']),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.timingWindowEvidence.requiredValues',
    new Set([
      'machineArtifactsGeneratedBeforeTiming',
      'machineArtifactsValidatedBeforeTiming',
      'machineGenerationIncludedInTimedSection',
      'timedSectionStartsAfterArtifactValidation',
    ]),
  ],
  [
    'governedBenchmark.executedReceiptRequirements.claimPolicy',
    new Set(['speedupClaimAllowedOnlyAfter']),
  ],
  [
    'governedBenchmark.successCriteria',
    new Set([
      'minimumIterations',
      'minimumWarmupIterations',
      'hotCacheMetric',
      'hotCacheMustBeat',
      'p95MustBeat',
      'outputHashMustMatchAcrossArms',
      'coldCachePolicy',
      'noisePolicy',
      'speedupClaimValidation',
    ]),
  ],
  [
    'governedBenchmark.successCriteria.speedupClaimValidation',
    new Set([
      'minimumRelativeImprovementPct',
      'minimumAbsoluteImprovementMs',
      'confidenceLevel',
      'requireConfidenceIntervalExcludesZero',
    ]),
  ],
  ['benchmarkExecution', new Set(['executed', 'reason'])],
  [
    'environment',
    new Set([
      'nodeVersion',
      'platform',
      'arch',
      'cpu',
      'rustcVersion',
      'powerThermalNotes',
    ]),
  ],
]);

export function createSourceBenchmarkReceipt({
  generatedAt = new Date().toISOString(),
  gitInfo,
  repoRoot = path.resolve(dirname, '../../..'),
  sourceFiles = DEFAULT_SOURCE_RECEIPT_FILES,
} = {}) {
  const resolvedGitInfo = gitInfo ?? readGitInfo(repoRoot);
  const hashedSourceFiles = sourceFiles.map((sourcePath) => hashSourceFile(repoRoot, sourcePath));
  return {
    schema: 'rolldown.dx.benchmark.source_receipt.v1',
    claimStatus: SOURCE_RECEIPT_CLAIM_STATUS,
    benchmarkStatus: 'not_run',
    upstreamComparison: 'not_measured',
    speedupClaim: 'none',
    generatedAt,
    repos: {
      local: {
        root: path.resolve(repoRoot),
        branch: resolvedGitInfo.branch,
        commit: resolvedGitInfo.commit,
        dirty: resolvedGitInfo.dirty,
        statusShort: normalizedStatusLines(resolvedGitInfo.statusShort),
      },
      upstream: {
        collectionStatus: 'required_before_benchmark_claim',
        branch: null,
        commit: null,
        dirty: null,
      },
    },
    sourceFiles: hashedSourceFiles,
    sourceFilesSha256: hashSourceFilesDigest(hashedSourceFiles),
    governedBenchmark: {
      arms: cloneJson(GOVERNED_BENCHMARK_ARMS),
      requiredMetrics: cloneJson(REQUIRED_BENCHMARK_METRICS),
      requiredEnvironmentFields: cloneJson(REQUIRED_BENCHMARK_ENVIRONMENT_FIELDS),
      cacheEnvMatrix: cloneJson(GOVERNED_BENCHMARK_CACHE_ENV_MATRIX),
      officialBaseline: cloneJson(GOVERNED_BENCHMARK_OFFICIAL_BASELINE),
      dryRun: cloneJson(GOVERNED_BENCHMARK_DRY_RUN),
      buildOutsideTimedSection: true,
      outputEquivalence: cloneJson(GOVERNED_BENCHMARK_OUTPUT_EQUIVALENCE),
      machineArtifactPreparation: cloneJson(GOVERNED_BENCHMARK_MACHINE_ARTIFACT_PREPARATION),
      fixturePlan: createGovernedBenchmarkFixturePlan(repoRoot),
      cacheEvidence: cloneJson(GOVERNED_BENCHMARK_CACHE_EVIDENCE),
      executedReceiptRequirements: cloneJson(GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS),
      successCriteria: cloneJson(GOVERNED_BENCHMARK_SUCCESS_CRITERIA),
    },
    benchmarkExecution: cloneJson(SOURCE_ONLY_BENCHMARK_EXECUTION),
    environment: {
      nodeVersion: process.version,
      platform: process.platform,
      arch: process.arch,
      cpu: null,
      rustcVersion: null,
      powerThermalNotes: null,
    },
  };
}

export function writeSourceBenchmarkReceipt(outPath, receipt) {
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(receipt, null, 2)}\n`, 'utf8');
}

export function validateSourceBenchmarkReceipt(receipt, { repoRoot = path.resolve(dirname, '../../..') } = {}) {
  const errors = [];
  const resolvedRepoRoot = path.resolve(repoRoot);
  const expect = (condition, message) => {
    if (!condition) {
      errors.push(message);
    }
  };

  expect(receipt?.schema === 'rolldown.dx.benchmark.source_receipt.v1', 'schema mismatch');
  expect(
    receipt?.claimStatus === SOURCE_RECEIPT_CLAIM_STATUS,
    `claimStatus must be ${SOURCE_RECEIPT_CLAIM_STATUS}`,
  );
  expect(receipt?.benchmarkStatus === 'not_run', 'benchmarkStatus must be not_run');
  expect(receipt?.upstreamComparison === 'not_measured', 'upstreamComparison must be not_measured');
  expect(receipt?.speedupClaim === 'none', 'speedupClaim must be none');
  expect(
    typeof receipt?.repos?.local?.root === 'string' &&
      sameResolvedPath(receipt.repos.local.root, resolvedRepoRoot),
    'repos.local.root must match validation repo root',
  );
  expect(receipt?.benchmarkExecution?.executed === false, 'benchmarkExecution.executed must be false');
  expectJsonEqual(receipt?.benchmarkExecution, SOURCE_ONLY_BENCHMARK_EXECUTION, 'benchmarkExecution mismatch', errors);
  expect(
    receipt?.repos?.upstream?.collectionStatus === 'required_before_benchmark_claim',
    'upstream collection must remain required before benchmark claim',
  );
  expectJsonEqual(receipt?.governedBenchmark?.arms, GOVERNED_BENCHMARK_ARMS, 'governedBenchmark.arms mismatch', errors);
  expectJsonEqual(
    receipt?.governedBenchmark?.requiredMetrics,
    REQUIRED_BENCHMARK_METRICS,
    'governedBenchmark.requiredMetrics mismatch',
    errors,
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.requiredEnvironmentFields,
    REQUIRED_BENCHMARK_ENVIRONMENT_FIELDS,
    'governedBenchmark.requiredEnvironmentFields mismatch',
    errors,
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.cacheEnvMatrix,
    GOVERNED_BENCHMARK_CACHE_ENV_MATRIX,
    'governedBenchmark.cacheEnvMatrix mismatch',
    errors,
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.officialBaseline,
    GOVERNED_BENCHMARK_OFFICIAL_BASELINE,
    'governedBenchmark.officialBaseline mismatch',
    errors,
  );
  expect(
    receipt?.governedBenchmark?.officialBaseline?.globalInstallAllowed === false,
    'official baseline must reject global installs',
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.dryRun,
    GOVERNED_BENCHMARK_DRY_RUN,
    'governedBenchmark.dryRun mismatch',
    errors,
  );
  expect(
    receipt?.governedBenchmark?.dryRun?.executesBenchmarks === false,
    'governedBenchmark.dryRun must not execute benchmarks',
  );
  expect(
    receipt?.governedBenchmark?.buildOutsideTimedSection === true,
    'governedBenchmark.buildOutsideTimedSection mismatch',
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.machineArtifactPreparation,
    GOVERNED_BENCHMARK_MACHINE_ARTIFACT_PREPARATION,
    'governedBenchmark.machineArtifactPreparation mismatch',
    errors,
  );
  expect(
    receipt?.governedBenchmark?.outputEquivalence?.required === true,
    'output equivalence must be required',
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.outputEquivalence,
    GOVERNED_BENCHMARK_OUTPUT_EQUIVALENCE,
    'governedBenchmark.outputEquivalence mismatch',
    errors,
  );
  try {
    expectJsonEqual(
      receipt?.governedBenchmark?.fixturePlan,
      createGovernedBenchmarkFixturePlan(resolvedRepoRoot),
      'governedBenchmark.fixturePlan mismatch',
      errors,
    );
  } catch (error) {
    errors.push(`governedBenchmark.fixturePlan selected fixture source could not be read: ${error.message}`);
  }
  expectJsonEqual(
    receipt?.governedBenchmark?.cacheEvidence,
    GOVERNED_BENCHMARK_CACHE_EVIDENCE,
    'governedBenchmark.cacheEvidence mismatch',
    errors,
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.executedReceiptRequirements,
    GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS,
    'governedBenchmark.executedReceiptRequirements mismatch',
    errors,
  );
  expectJsonEqual(
    receipt?.governedBenchmark?.successCriteria,
    GOVERNED_BENCHMARK_SUCCESS_CRITERIA,
    'governedBenchmark.successCriteria mismatch',
    errors,
  );

  validateNoResultFields(receipt, errors);
  validateKnownFields(receipt, errors);
  validateLocalGitProvenance(receipt, resolvedRepoRoot, errors);
  validateSourceFileHashes(receipt, resolvedRepoRoot, errors);
  validateSourceFilesDigest(receipt, errors);
  validateSelectedFixtureSourceProof(receipt, resolvedRepoRoot, errors);

  return { ok: errors.length === 0, errors };
}

function createGovernedBenchmarkFixturePlan(repoRoot) {
  const selectedFixtureSources = GOVERNED_BENCHMARK_FIXTURE_PLAN.selectedFixtureSourcePaths.map(
    (sourcePath) => ({ ...hashSourceFile(repoRoot, sourcePath), sourceOwned: true }),
  );
  const selectedFixtureTotalBytes = selectedFixtureSources.reduce(
    (total, source) => total + source.bytes,
    0,
  );
  return {
    ...cloneJson(GOVERNED_BENCHMARK_FIXTURE_PLAN),
    selectedFixtureSources,
    selectedFixtureTotalBytes,
  };
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

function expectJsonEqual(actual, expected, message, errors) {
  if (!jsonValuesEqual(actual, expected)) {
    errors.push(message);
  }
}

function jsonValuesEqual(actual, expected) {
  if (Object.is(actual, expected)) {
    return true;
  }
  if (Array.isArray(expected)) {
    return (
      Array.isArray(actual) &&
      actual.length === expected.length &&
      expected.every((expectedItem, index) => jsonValuesEqual(actual[index], expectedItem))
    );
  }
  if (expected && typeof expected === 'object') {
    if (!actual || typeof actual !== 'object' || Array.isArray(actual)) {
      return false;
    }
    const expectedKeys = Object.keys(expected);
    const actualKeys = Object.keys(actual);
    return (
      expectedKeys.length === actualKeys.length &&
      expectedKeys.every(
        (key) => Object.hasOwn(actual, key) && jsonValuesEqual(actual[key], expected[key]),
      )
    );
  }
  return false;
}

function validateLocalGitProvenance(receipt, repoRoot, errors) {
  if (!repoRootIsGitWorktree(repoRoot)) {
    return;
  }

  const recorded = receipt?.repos?.local;
  if (!recorded || typeof recorded !== 'object' || Array.isArray(recorded)) {
    errors.push('repos.local must record local git provenance');
    return;
  }

  const current = readGitInfo(repoRoot);
  if (recorded.commit !== current.commit) {
    errors.push('repos.local.commit must match current HEAD');
  }
  if (current.branch !== 'HEAD' && recorded.branch !== current.branch) {
    errors.push('repos.local.branch must match current branch');
  }
  if (recorded.dirty !== current.dirty) {
    errors.push('repos.local.dirty must match current git status');
  }
  if (!jsonValuesEqual(normalizedStatusLines(recorded.statusShort), current.statusShort)) {
    errors.push('repos.local.statusShort must match current git status');
  }
}

function hashSourceFile(repoRoot, sourcePath) {
  const { normalizedPath, absolutePath } = resolveSourceOwnedRepoRelativePath(repoRoot, sourcePath);
  const bytes = fs.readFileSync(absolutePath);
  return {
    path: normalizedPath,
    bytes: bytes.length,
    sha256: crypto.createHash('sha256').update(bytes).digest('hex'),
  };
}

function hashSourceFilesDigest(sourceFiles) {
  return crypto.createHash('sha256').update(`${JSON.stringify(sourceFiles)}\n`).digest('hex');
}

function validateSourceFileHashes(receipt, repoRoot, errors) {
  if (!Array.isArray(receipt?.sourceFiles)) {
    errors.push('sourceFiles must be an array');
    return;
  }
  if (receipt.sourceFiles.length === 0) {
    errors.push('sourceFiles must not be empty');
  }

  const seenPaths = new Set();
  const presentPaths = new Set();
  receipt.sourceFiles.forEach((entry, index) => {
    if (typeof entry?.path !== 'string') {
      errors.push(`sourceFiles[${index}].path must be a string`);
      return;
    }

    let normalizedPath;
    try {
      normalizedPath = normalizeReceiptPath(entry.path);
      resolveRepoRelativePath(repoRoot, normalizedPath);
    } catch (error) {
      errors.push(`sourceFiles[${index}].path must be safe repo-relative: ${entry.path}`);
      return;
    }
    if (seenPaths.has(normalizedPath)) {
      errors.push(`sourceFiles has duplicate path: ${normalizedPath}`);
    }
    seenPaths.add(normalizedPath);
    presentPaths.add(normalizedPath);

    let current;
    try {
      current = hashSourceFile(repoRoot, normalizedPath);
    } catch (error) {
      errors.push(`sourceFiles[${index}] could not be read: ${normalizedPath}: ${error.message}`);
      return;
    }

    if (entry.bytes !== current.bytes) {
      errors.push(`sourceFiles[${index}].bytes mismatch for ${normalizedPath}`);
    }
    if (entry.sha256 !== current.sha256) {
      errors.push(`sourceFiles[${index}].sha256 mismatch for ${normalizedPath}`);
    }
  });

  DEFAULT_SOURCE_RECEIPT_FILES.forEach((requiredPath) => {
    if (!presentPaths.has(requiredPath)) {
      errors.push(`sourceFiles missing required path: ${requiredPath}`);
    }
  });
}

function validateSourceFilesDigest(receipt, errors) {
  if (
    typeof receipt?.sourceFilesSha256 !== 'string' ||
    !/^[a-f0-9]{64}$/u.test(receipt.sourceFilesSha256)
  ) {
    errors.push('sourceFilesSha256 must be a sha256 hex digest');
    return;
  }
  if (!Array.isArray(receipt?.sourceFiles)) {
    return;
  }
  if (receipt.sourceFilesSha256 !== hashSourceFilesDigest(receipt.sourceFiles)) {
    errors.push('sourceFilesSha256 mismatch for sourceFiles');
  }
}

function validateSelectedFixtureSourceProof(receipt, repoRoot, errors) {
  const fixturePlan = receipt?.governedBenchmark?.fixturePlan;
  const selectedFixtureSources = fixturePlan?.selectedFixtureSources;
  if (!Array.isArray(selectedFixtureSources)) {
    errors.push('governedBenchmark.fixturePlan.selectedFixtureSources must be an array');
    return;
  }

  const expectedPaths = GOVERNED_BENCHMARK_FIXTURE_PLAN.selectedFixtureSourcePaths;
  const actualPaths = selectedFixtureSources.map((entry) => entry?.path);
  if (!jsonValuesEqual(actualPaths, expectedPaths)) {
    errors.push('governedBenchmark.fixturePlan.selectedFixtureSources paths mismatch');
  }

  let totalBytes = 0;
  selectedFixtureSources.forEach((entry, index) => {
    if (entry?.sourceOwned !== true) {
      errors.push(`selectedFixtureSources[${index}].sourceOwned must be true`);
    }
    if (typeof entry?.path !== 'string') {
      errors.push(`selectedFixtureSources[${index}].path must be a string`);
      return;
    }

    let current;
    try {
      current = hashSourceFile(repoRoot, entry.path);
    } catch (error) {
      errors.push(`selectedFixtureSources[${index}] could not be read: ${entry.path}: ${error.message}`);
      return;
    }

    totalBytes += current.bytes;
    if (entry.bytes !== current.bytes) {
      errors.push(`selectedFixtureSources[${index}].bytes mismatch for ${current.path}`);
    }
    if (entry.sha256 !== current.sha256) {
      errors.push(`selectedFixtureSources[${index}].sha256 mismatch for ${current.path}`);
    }
  });

  if (fixturePlan?.selectedFixtureTotalBytes !== totalBytes) {
    errors.push('governedBenchmark.fixturePlan.selectedFixtureTotalBytes mismatch');
  }

  const minimumBytes = fixturePlan?.sizeGate?.minimumBytesExclusive;
  if (typeof minimumBytes === 'number' && totalBytes <= minimumBytes) {
    errors.push(`selected source-owned fixture bytes must be > ${minimumBytes}, got ${totalBytes}`);
  }
}

function normalizeReceiptPath(sourcePath) {
  if (sourcePath.includes('\0')) {
    throw new Error('path contains NUL');
  }
  if (
    path.isAbsolute(sourcePath) ||
    path.win32.isAbsolute(sourcePath) ||
    path.posix.isAbsolute(sourcePath) ||
    /^[A-Za-z]:/.test(sourcePath)
  ) {
    throw new Error('path must be relative');
  }

  const normalizedPath = sourcePath.replaceAll('\\', '/');
  if (normalizedPath.includes(':')) {
    throw new Error('path must not contain colon segments');
  }
  const pathParts = normalizedPath.split('/');
  if (pathParts.some((part) => part === '' || part === '.' || part === '..')) {
    throw new Error('path must not contain empty, dot, or parent segments');
  }
  return normalizedPath;
}

function resolveRepoRelativePath(repoRoot, sourcePath) {
  const resolvedRepoRoot = path.resolve(repoRoot);
  const normalizedPath = normalizeReceiptPath(sourcePath);
  const absolutePath = path.resolve(resolvedRepoRoot, normalizedPath);
  const relativePath = path.relative(resolvedRepoRoot, absolutePath);
  if (relativePath.startsWith('..') || path.isAbsolute(relativePath)) {
    throw new Error('path escapes repo root');
  }
  return { normalizedPath, absolutePath };
}

function resolveSourceOwnedRepoRelativePath(repoRoot, sourcePath) {
  const { normalizedPath, absolutePath } = resolveRepoRelativePath(repoRoot, sourcePath);
  const realRepoRoot = fs.realpathSync.native(path.resolve(repoRoot));
  const realAbsolutePath = fs.realpathSync.native(absolutePath);
  if (!sameOrChildPath(realRepoRoot, realAbsolutePath)) {
    throw new Error('path must resolve inside repo root');
  }
  return { normalizedPath, absolutePath: realAbsolutePath };
}

function sameOrChildPath(parent, child) {
  const relativePath = path.relative(comparableResolvedPath(parent), comparableResolvedPath(child));
  return relativePath === '' || (!relativePath.startsWith('..') && !path.isAbsolute(relativePath));
}

function comparableResolvedPath(value) {
  const resolved = path.resolve(value);
  return process.platform === 'win32' ? resolved.toLowerCase() : resolved;
}

function sameResolvedPath(left, right) {
  const resolvedLeft = path.resolve(left);
  const resolvedRight = path.resolve(right);
  if (process.platform === 'win32') {
    return resolvedLeft.toLowerCase() === resolvedRight.toLowerCase();
  }
  return resolvedLeft === resolvedRight;
}

function validateNoResultFields(value, errors, pathParts = []) {
  if (!value || typeof value !== 'object') {
    return;
  }

  for (const [key, child] of Object.entries(value)) {
    if (FORBIDDEN_RESULT_FIELDS.has(key)) {
      errors.push(`forbidden source-only result field: ${[...pathParts, key].join('.')}`);
    }
    validateNoResultFields(child, errors, [...pathParts, key]);
  }
}

function validateKnownFields(value, errors, pathParts = []) {
  if (!value || typeof value !== 'object') {
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((child, index) => validateKnownFields(child, errors, [...pathParts, index]));
    return;
  }

  const knownFields = knownFieldsForPath(pathParts);
  for (const [key, child] of Object.entries(value)) {
    const childPathParts = [...pathParts, key];
    if (knownFields && !knownFields.has(key) && !FORBIDDEN_RESULT_FIELDS.has(key)) {
      errors.push(`forbidden source-only field: ${formatReceiptPath(childPathParts)}`);
      continue;
    }
    validateKnownFields(child, errors, childPathParts);
  }
}

function knownFieldsForPath(pathParts) {
  const fieldPath = pathParts.map((part) => (typeof part === 'number' ? '*' : part)).join('.');
  return KNOWN_SOURCE_RECEIPT_FIELDS.get(fieldPath);
}

function formatReceiptPath(pathParts) {
  return pathParts.join('.');
}

function readGitInfo(repoRoot) {
  const branch = readGit(repoRoot, ['rev-parse', '--abbrev-ref', 'HEAD']) || 'unknown';
  const commit = readGit(repoRoot, ['rev-parse', 'HEAD']) || 'unknown';
  const statusShort = readGit(repoRoot, ['status', '--short']) || '';
  return {
    branch,
    commit,
    dirty: normalizedStatusLines(statusShort).length > 0,
    statusShort: normalizedStatusLines(statusShort),
  };
}

function repoRootIsGitWorktree(repoRoot) {
  return readGit(repoRoot, ['rev-parse', '--is-inside-work-tree']) === 'true';
}

function normalizedStatusLines(statusShort) {
  if (Array.isArray(statusShort)) {
    return statusShort;
  }
  return statusShort.split(/\r?\n/).filter(Boolean);
}

function readGit(repoRoot, args) {
  try {
    return childProcess.execFileSync('git', args, {
      cwd: repoRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return '';
  }
}

function parseOutPath(args, repoRoot) {
  const outIndex = args.indexOf('--out');
  if (outIndex === -1) {
    return path.join(repoRoot, '.dx/rolldown/benchmark-source-receipt.json');
  }
  const outPath = args[outIndex + 1];
  if (!outPath) {
    throw new Error('Missing value after --out');
  }
  return resolveCliPathUnderRepoRoot(repoRoot, outPath, '--out');
}

function parseRepoRoot(args, defaultRepoRoot) {
  const repoRootIndex = args.indexOf('--repo-root');
  if (repoRootIndex === -1) {
    return defaultRepoRoot;
  }
  const repoRoot = args[repoRootIndex + 1];
  if (!repoRoot) {
    throw new Error('Missing value after --repo-root');
  }
  return path.resolve(repoRoot);
}

function resolveCliPathUnderRepoRoot(repoRoot, inputPath, flagName) {
  const resolvedRepoRoot = path.resolve(repoRoot);
  const resolvedPath = path.resolve(resolvedRepoRoot, inputPath);
  const relativePath = path.relative(resolvedRepoRoot, resolvedPath);
  if (relativePath.startsWith('..') || path.isAbsolute(relativePath)) {
    throw new Error(`${flagName} must stay inside repo root`);
  }
  return resolvedPath;
}

function readSourceReceiptJson(receiptPath) {
  const stat = fs.statSync(receiptPath);
  if (!stat.isFile()) {
    throw new Error('source receipt JSON path must be a file');
  }
  if (stat.size > MAX_SOURCE_RECEIPT_JSON_BYTES) {
    throw new Error('source receipt JSON too large');
  }
  const source = fs.readFileSync(receiptPath, 'utf8');
  try {
    return JSON.parse(source);
  } catch (error) {
    throw new Error(`Invalid source receipt JSON: ${error.message}`);
  }
}

function runCli(args) {
  const repoRoot = parseRepoRoot(args, path.resolve(dirname, '../../..'));
  const validateIndex = args.indexOf('--validate');
  if (validateIndex !== -1) {
    const receiptPath = args[validateIndex + 1];
    if (!receiptPath) {
      throw new Error('Missing value after --validate');
    }
    const resolvedReceiptPath = resolveCliPathUnderRepoRoot(repoRoot, receiptPath, '--validate');
    const receipt = readSourceReceiptJson(resolvedReceiptPath);
    const validation = validateSourceBenchmarkReceipt(receipt, { repoRoot });
    if (!validation.ok) {
      console.error(validation.errors.join('\n'));
      process.exitCode = 1;
    } else {
      console.log(`valid source receipt: ${resolvedReceiptPath}`);
    }
  } else {
    const outPath = parseOutPath(args, repoRoot);
    const receipt = createSourceBenchmarkReceipt({ repoRoot });
    writeSourceBenchmarkReceipt(outPath, receipt);
    console.log(outPath);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === url.fileURLToPath(import.meta.url)) {
  try {
    runCli(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
