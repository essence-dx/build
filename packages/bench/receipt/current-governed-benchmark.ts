import { GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS } from './source-receipt.ts';

const requiredMachineMagic =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.requiredMachineMagic;
const forbiddenMachineMagicEntries =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.forbiddenMachineMagic;
const forbiddenMachineMagic = new Set(forbiddenMachineMagicEntries.map((entry) => entry.magic));
const requiredArtifactBenefits = new Map(
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence
    .requiredMachineArtifactBenefits.map((entry) => [entry.magic, entry]),
);

export const CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN = requiredMachineMagic.map((entry) => ({
  family: entry.artifactKind,
  expectedMagic: entry.magic,
  mustRunInEveryLocalCacheEnabledSample: true,
}));

export function validateArtifactExerciseMatrix(artifactExerciseMatrix) {
  const errors = [];
  if (!Array.isArray(artifactExerciseMatrix)) {
    return { ok: false, errors: ['artifactExerciseMatrix must be an array'] };
  }

  const entriesByMagic = new Map();
  for (const entry of artifactExerciseMatrix) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      errors.push('artifactExerciseMatrix entry must be an object');
      continue;
    }

    if (forbiddenMachineMagic.has(entry.expectedMagic)) {
      errors.push(`artifactExerciseMatrix.${entry.expectedMagic} must not use legacy magic`);
    }
    if (entriesByMagic.has(entry.expectedMagic)) {
      errors.push(`artifactExerciseMatrix.${entry.expectedMagic} must be unique`);
      continue;
    }
    entriesByMagic.set(entry.expectedMagic, entry);
  }

  for (const planEntry of CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN) {
    const entry = entriesByMagic.get(planEntry.expectedMagic);
    if (!entry) {
      errors.push(`artifactExerciseMatrix.${planEntry.expectedMagic} is required`);
      continue;
    }
    validateRequiredArtifactEntry(entry, planEntry, errors);
  }

  return { ok: errors.length === 0, errors };
}

export function buildMachineMagicCounts(artifactExerciseMatrix) {
  const counts = Object.fromEntries([
    ...requiredMachineMagic.map((entry) => [entry.magic, 0]),
    ...forbiddenMachineMagicEntries.map((entry) => [entry.magic, 0]),
  ]);
  if (!Array.isArray(artifactExerciseMatrix)) {
    return counts;
  }

  for (const entry of artifactExerciseMatrix) {
    if (Object.hasOwn(counts, entry?.expectedMagic)) {
      counts[entry.expectedMagic] += 1;
    }
  }

  return counts;
}

export function buildArtifactExerciseMatrixFromCacheScan({
  cacheScan,
  artifactMetadataByMagic = {},
  exercisedMagicInEveryLocalCacheEnabledSample = [],
  cacheFingerprintStable,
}) {
  const exercisedMagic = new Set(exercisedMagicInEveryLocalCacheEnabledSample);
  const machineFilesByMagic = cacheScan?.machineFilesByMagic ?? {};
  const matrix = [];

  for (const planEntry of CURRENT_GOVERNED_ARTIFACT_EXERCISE_PLAN) {
    const machineFiles = normalizedMachineFiles(machineFilesByMagic[planEntry.expectedMagic]);
    if (machineFiles.length === 0) {
      continue;
    }

    const metadata = artifactMetadataByMagic[planEntry.expectedMagic] ?? {};
    if (requiredArtifactBenefits.has(planEntry.expectedMagic) && !hasJsonArtifactMetadata(metadata)) {
      continue;
    }

    matrix.push({
      family: planEntry.family,
      expectedMagic: planEntry.expectedMagic,
      producedMachinePaths: machineFiles.map((file) => file.path),
      ...metadata,
      machineBytes: sumMachineFileBytes(machineFiles),
      exercisedInEveryLocalCacheEnabledSample: exercisedMagic.has(planEntry.expectedMagic),
      cacheFingerprintStable: cacheFingerprintStable === true,
    });
  }

  return matrix;
}

export function buildMachineReadHitCounts({
  enabledIterations,
  artifactExerciseMatrix,
  machineWriteCountDuringTiming,
  machineRepairCountDuringTiming,
}) {
  const counts = Object.fromEntries(requiredMachineMagic.map((entry) => [entry.magic, 0]));
  if (
    !Number.isInteger(enabledIterations) ||
    enabledIterations <= 0 ||
    machineWriteCountDuringTiming !== 0 ||
    machineRepairCountDuringTiming !== 0 ||
    !Array.isArray(artifactExerciseMatrix)
  ) {
    return counts;
  }

  for (const entry of artifactExerciseMatrix) {
    if (!Object.hasOwn(counts, entry?.expectedMagic)) {
      continue;
    }
    if (
      hasProducedMachineArtifact(entry) &&
      entry.exercisedInEveryLocalCacheEnabledSample === true &&
      entry.cacheFingerprintStable === true
    ) {
      counts[entry.expectedMagic] = enabledIterations;
    }
  }

  return counts;
}

export function buildMachineArtifactBenefitEvidence(artifactExerciseMatrix) {
  const evidence = {};
  if (!Array.isArray(artifactExerciseMatrix)) {
    return evidence;
  }

  const entriesByMagic = new Map(
    artifactExerciseMatrix
      .filter((entry) => entry && typeof entry === 'object' && !Array.isArray(entry))
      .map((entry) => [entry.expectedMagic, entry]),
  );
  for (const benefitPolicy of requiredArtifactBenefits.values()) {
    const entry = entriesByMagic.get(benefitPolicy.magic);
    if (!entry) {
      continue;
    }
    evidence[benefitPolicy.magic] = {
      artifactKind: benefitPolicy.artifactKind,
      sourceShape: entry.sourceShape,
      sourceBytes: entry.sourceBytes,
      machineBytes: entry.machineBytes,
    };
  }

  return evidence;
}

export function buildCurrentGovernedCacheArtifactEvidence({
  dxRolldownFileCount,
  dxRolldownBytes,
  enabledIterations,
  artifactExerciseMatrix,
  machineHotCacheCounters,
  machineWriteCountDuringTiming,
  machineRepairCountDuringTiming,
  cacheFingerprintBeforeTiming,
  cacheFingerprintAfterTiming,
}) {
  return {
    arm: 'local-cache-enabled',
    dxRolldownFileCount,
    dxRolldownBytes,
    artifactExerciseMatrix,
    machineMagicCounts: buildMachineMagicCounts(artifactExerciseMatrix),
    machineReadHitCounts: buildMachineReadHitCounts({
      enabledIterations,
      artifactExerciseMatrix,
      machineWriteCountDuringTiming,
      machineRepairCountDuringTiming,
    }),
    machineHotCacheCounters,
    machineArtifactBenefitEvidence: buildMachineArtifactBenefitEvidence(artifactExerciseMatrix),
    machineWriteCountDuringTiming,
    machineRepairCountDuringTiming,
    cacheFingerprintBeforeTiming,
    cacheFingerprintAfterTiming,
  };
}

function validateRequiredArtifactEntry(entry, planEntry, errors) {
  if (entry.family !== planEntry.family) {
    errors.push(`artifactExerciseMatrix.${planEntry.expectedMagic}.family must be ${planEntry.family}`);
  }
  if (!hasProducedMachineArtifact(entry)) {
    errors.push(`artifactExerciseMatrix.${planEntry.expectedMagic}.producedMachinePaths is required`);
  }
  if (entry.exercisedInEveryLocalCacheEnabledSample !== true) {
    errors.push(
      `artifactExerciseMatrix.${planEntry.expectedMagic}.exercisedInEveryLocalCacheEnabledSample must be true`,
    );
  }
  if (entry.cacheFingerprintStable !== true) {
    errors.push(`artifactExerciseMatrix.${planEntry.expectedMagic}.cacheFingerprintStable must be true`);
  }

  const benefitPolicy = requiredArtifactBenefits.get(planEntry.expectedMagic);
  if (benefitPolicy) {
    validateArtifactBenefitEntry(entry, benefitPolicy, errors);
  }
}

function validateArtifactBenefitEntry(entry, benefitPolicy, errors) {
  requirePositiveNumber(
    entry.sourceBytes,
    `artifactExerciseMatrix.${benefitPolicy.magic}.sourceBytes`,
    errors,
  );
  requirePositiveNumber(
    entry.machineBytes,
    `artifactExerciseMatrix.${benefitPolicy.magic}.machineBytes`,
    errors,
  );
  if (
    benefitPolicy.machineBytesMustBeLessThanSourceBytes &&
    Number.isFinite(entry.machineBytes) &&
    Number.isFinite(entry.sourceBytes) &&
    entry.machineBytes >= entry.sourceBytes
  ) {
    errors.push(`artifactExerciseMatrix.${benefitPolicy.magic}.machineBytes must be < sourceBytes`);
  }
  if (!benefitPolicy.allowedSourceShapes.includes(entry.sourceShape)) {
    errors.push(`artifactExerciseMatrix.${benefitPolicy.magic}.sourceShape must be object or array`);
  }
}

function hasProducedMachineArtifact(entry) {
  return (
    Array.isArray(entry?.producedMachinePaths) &&
    entry.producedMachinePaths.some((machinePath) => typeof machinePath === 'string' && machinePath.length > 0)
  );
}

function normalizedMachineFiles(machineFiles) {
  if (!Array.isArray(machineFiles)) {
    return [];
  }
  return machineFiles
    .filter((file) => file && typeof file === 'object' && !Array.isArray(file))
    .filter(
      (file) =>
        typeof file.path === 'string' &&
        file.path.length > 0 &&
        Number.isFinite(file.bytes) &&
        file.bytes > 0,
    );
}

function sumMachineFileBytes(machineFiles) {
  return machineFiles.reduce(
    (total, file) => total + (Number.isFinite(file.bytes) && file.bytes > 0 ? file.bytes : 0),
    0,
  );
}

function hasJsonArtifactMetadata(metadata) {
  return (
    Number.isFinite(metadata.sourceBytes) &&
    metadata.sourceBytes > 0 &&
    ['object', 'array'].includes(metadata.sourceShape)
  );
}

function requirePositiveNumber(value, label, errors) {
  if (!Number.isFinite(value) || value <= 0) {
    errors.push(`${label} must be a positive number`);
  }
}
