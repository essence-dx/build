import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN,
  buildArtifactExerciseMatrixFromCacheScan,
  buildCurrentGovernedCacheArtifactEvidence,
  buildMachineMagicCounts,
  buildMachineArtifactBenefitEvidence,
  buildMachineReadHitCounts,
  validateArtifactExerciseMatrix,
} from './current-governed-benchmark.ts';
import { governedBenchmarkPlan } from './governed-benchmark.ts';

const validArtifactMatrix = [
  artifactExercise('core-json', 'RDXCJSR2'),
  artifactExercise('vite-json', 'RDXJSON2'),
  artifactExercise('css-package-name', 'RDXCSSRK'),
  artifactExercise('optional-peer-deps', 'RDXOPDRK'),
];

test('current governed artifact exercise plan covers every required magic family once', () => {
  assert.deepEqual(
    CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN.map((entry) => entry.expectedMagic),
    governedBenchmarkPlan.requiredCurrentMagic,
  );
  assert.equal(
    new Set(CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN.map((entry) => entry.expectedMagic)).size,
    governedBenchmarkPlan.requiredCurrentMagic.length,
  );
  assert.deepEqual(
    CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN.map((entry) => entry.family),
    ['core-json', 'vite-json', 'css-package-name', 'optional-peer-deps'],
  );
  assert.equal(
    CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN.every(
      (entry) => entry.mustRunInEveryLocalCacheEnabledSample,
    ),
    true,
  );
});

test('artifact exercise matrix rejects missing required magic families', () => {
  const validation = validateArtifactExerciseMatrix(validArtifactMatrix.slice(0, 2));

  assert.equal(validation.ok, false);
  assert.match(validation.errors.join('\n'), /artifactExerciseMatrix\.RDXCSSRK is required/);
  assert.match(validation.errors.join('\n'), /artifactExerciseMatrix\.RDXOPDRK is required/);
});

test('machine magic counts include required current magic and forbidden legacy zeros', () => {
  const counts = buildMachineMagicCounts([
    ...validArtifactMatrix,
    artifactExercise('core-json-copy', 'RDXCJSR2'),
  ]);

  assert.deepEqual(counts, {
    RDXCJSR2: 2,
    RDXJSON2: 1,
    RDXCSSRK: 1,
    RDXOPDRK: 1,
    RDXCJSRK: 0,
    RDXCJSN3: 0,
  });
});

test('machine magic counts record forbidden legacy artifacts when present', () => {
  const counts = buildMachineMagicCounts([
    ...validArtifactMatrix,
    artifactExercise('legacy-core-json', 'RDXCJSRK'),
  ]);

  assert.equal(counts.RDXCJSRK, 1);
  assert.equal(counts.RDXCJSN3, 0);
});

test('artifact exercise matrix rejects legacy and non-beneficial JSON artifacts', () => {
  const matrix = [
    artifactExercise('core-json', 'RDXCJSR2', { machineBytes: 16 * 1024, sourceBytes: 16 * 1024 }),
    artifactExercise('vite-json', 'RDXJSON2', { sourceShape: 'scalar' }),
    artifactExercise('css-package-name', 'RDXCSSRK'),
    artifactExercise('optional-peer-deps', 'RDXOPDRK'),
    artifactExercise('legacy-core-json', 'RDXCJSRK'),
  ];

  const validation = validateArtifactExerciseMatrix(matrix);
  const errors = validation.errors.join('\n');

  assert.equal(validation.ok, false);
  assert.match(errors, /artifactExerciseMatrix\.RDXCJSRK must not use legacy magic/);
  assert.match(errors, /artifactExerciseMatrix\.RDXCJSR2\.machineBytes must be < sourceBytes/);
  assert.match(errors, /artifactExerciseMatrix\.RDXJSON2\.sourceShape must be object or array/);
});

test('machine read-hit counts are granted only for stable artifacts used in every enabled sample', () => {
  const counts = buildMachineReadHitCounts({
    enabledIterations: 30,
    artifactExerciseMatrix: [
      artifactExercise('core-json', 'RDXCJSR2'),
      artifactExercise('vite-json', 'RDXJSON2', { exercisedInEveryLocalCacheEnabledSample: false }),
      artifactExercise('css-package-name', 'RDXCSSRK', { cacheFingerprintStable: false }),
      artifactExercise('optional-peer-deps', 'RDXOPDRK', { producedMachinePaths: [] }),
    ],
    machineWriteCountDuringTiming: 0,
    machineRepairCountDuringTiming: 0,
  });

  assert.deepEqual(counts, {
    RDXCJSR2: 30,
    RDXJSON2: 0,
    RDXCSSRK: 0,
    RDXOPDRK: 0,
  });
});

test('machine read-hit counts stay zero when timing writes or repairs occur', () => {
  const counts = buildMachineReadHitCounts({
    enabledIterations: 30,
    artifactExerciseMatrix: validArtifactMatrix,
    machineWriteCountDuringTiming: 1,
    machineRepairCountDuringTiming: 0,
  });

  assert.deepEqual(counts, {
    RDXCJSR2: 0,
    RDXJSON2: 0,
    RDXCSSRK: 0,
    RDXOPDRK: 0,
  });
});

test('machine artifact benefit evidence includes only exercised JSON machine families', () => {
  const evidence = buildMachineArtifactBenefitEvidence([
    artifactExercise('core-json', 'RDXCJSR2', { sourceBytes: 48 * 1024, machineBytes: 20 * 1024 }),
    artifactExercise('css-package-name', 'RDXCSSRK'),
    artifactExercise('optional-peer-deps', 'RDXOPDRK'),
  ]);

  assert.deepEqual(evidence, {
    RDXCJSR2: {
      artifactKind: 'core-json',
      sourceShape: 'object',
      sourceBytes: 48 * 1024,
      machineBytes: 20 * 1024,
    },
  });
});

test('cache artifact evidence composer keeps an RDXCJSR2-only run honest', () => {
  const evidence = buildCurrentGovernedCacheArtifactEvidence({
    dxRolldownFileCount: 2,
    dxRolldownBytes: 24 * 1024,
    enabledIterations: 30,
    artifactExerciseMatrix: [
      artifactExercise('core-json', 'RDXCJSR2', { sourceBytes: 48 * 1024, machineBytes: 20 * 1024 }),
    ],
    machineHotCacheCounters: {
      RDXCJSR2: {
        jsonParseCount: 0,
        astNumberJsonParseCount: 0,
        dxSerializerBodyVecDecodeCount: 0,
        alignedCopyCount: 0,
      },
    },
    machineWriteCountDuringTiming: 0,
    machineRepairCountDuringTiming: 0,
    cacheFingerprintBeforeTiming: 'before',
    cacheFingerprintAfterTiming: 'before',
  });

  assert.deepEqual(evidence.machineMagicCounts, {
    RDXCJSR2: 1,
    RDXJSON2: 0,
    RDXCSSRK: 0,
    RDXOPDRK: 0,
    RDXCJSRK: 0,
    RDXCJSN3: 0,
  });
  assert.deepEqual(evidence.machineReadHitCounts, {
    RDXCJSR2: 30,
    RDXJSON2: 0,
    RDXCSSRK: 0,
    RDXOPDRK: 0,
  });
  assert.deepEqual(Object.keys(evidence.machineArtifactBenefitEvidence), ['RDXCJSR2']);
  assert.match(
    validateArtifactExerciseMatrix(evidence.artifactExerciseMatrix).errors.join('\n'),
    /artifactExerciseMatrix\.RDXJSON2 is required/,
  );
});

test('cache scan conversion emits only rows backed by produced machine files', () => {
  const matrix = buildArtifactExerciseMatrixFromCacheScan({
    cacheScan: cacheScan({
      RDXCJSR2: [
        { path: 'G:/bench/.dx/rolldown/core-a.machine', bytes: 8 * 1024 },
        { path: 'G:/bench/.dx/rolldown/core-b.machine', bytes: 12 * 1024 },
      ],
      RDXJSON2: [],
    }),
    artifactMetadataByMagic: {
      RDXCJSR2: { sourceBytes: 48 * 1024, sourceShape: 'object' },
    },
    exercisedMagicInEveryLocalCacheEnabledSample: ['RDXCJSR2'],
    cacheFingerprintStable: true,
  });

  assert.deepEqual(matrix, [
    {
      family: 'core-json',
      expectedMagic: 'RDXCJSR2',
      producedMachinePaths: [
        'G:/bench/.dx/rolldown/core-a.machine',
        'G:/bench/.dx/rolldown/core-b.machine',
      ],
      sourceBytes: 48 * 1024,
      machineBytes: 20 * 1024,
      sourceShape: 'object',
      exercisedInEveryLocalCacheEnabledSample: true,
      cacheFingerprintStable: true,
    },
  ]);
  assert.match(
    validateArtifactExerciseMatrix(matrix).errors.join('\n'),
    /artifactExerciseMatrix\.RDXJSON2 is required/,
  );
});

test('cache scan conversion does not emit JSON rows without source metadata', () => {
  const matrix = buildArtifactExerciseMatrixFromCacheScan({
    cacheScan: cacheScan({
      RDXJSON2: [{ path: 'G:/bench/.dx/rolldown/vite-json.machine', bytes: 10 * 1024 }],
      RDXCSSRK: [{ path: 'G:/bench/.dx/rolldown/css.machine', bytes: 4 * 1024 }],
    }),
    artifactMetadataByMagic: {},
    exercisedMagicInEveryLocalCacheEnabledSample: ['RDXJSON2', 'RDXCSSRK'],
    cacheFingerprintStable: true,
  });

  assert.deepEqual(matrix, [
    {
      family: 'css-package-name',
      expectedMagic: 'RDXCSSRK',
      producedMachinePaths: ['G:/bench/.dx/rolldown/css.machine'],
      machineBytes: 4 * 1024,
      exercisedInEveryLocalCacheEnabledSample: true,
      cacheFingerprintStable: true,
    },
  ]);
});

test('cache scan conversion rejects machine rows without positive byte evidence', () => {
  const matrix = buildArtifactExerciseMatrixFromCacheScan({
    cacheScan: cacheScan({
      RDXCSSRK: [
        { path: 'G:/bench/.dx/rolldown/css-zero.machine', bytes: 0 },
        { path: 'G:/bench/.dx/rolldown/css-negative.machine', bytes: -1 },
      ],
      RDXOPDRK: [
        { path: 'G:/bench/.dx/rolldown/optional-peer-missing-bytes.machine' },
      ],
    }),
    exercisedMagicInEveryLocalCacheEnabledSample: ['RDXCSSRK', 'RDXOPDRK'],
    cacheFingerprintStable: true,
  });

  assert.deepEqual(matrix, []);
});

function artifactExercise(family, expectedMagic, overrides = {}) {
  return {
    family,
    expectedMagic,
    producedMachinePaths: [`G:/Dx/build/.dx/rolldown/${expectedMagic}.machine`],
    sourceBytes: 32 * 1024,
    machineBytes: 16 * 1024,
    sourceShape: 'object',
    exercisedInEveryLocalCacheEnabledSample: true,
    cacheFingerprintStable: true,
    ...overrides,
  };
}

function cacheScan(machineFilesByMagic) {
  return { machineFilesByMagic };
}
