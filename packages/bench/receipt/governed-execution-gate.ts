import { validateArtifactExerciseMatrix } from './current-governed-benchmark.ts';
import {
  GOVERNED_BENCHMARK_ARMS,
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS,
  GOVERNED_BENCHMARK_SUCCESS_CRITERIA,
  SOURCE_RECEIPT_CLAIM_STATUS,
} from './source-receipt.ts';

export const GOVERNED_BENCHMARK_EXECUTION_ARMS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.perArmEvidence.arms;

const GOVERNED_ARM_CACHE_ENV = new Map(
  GOVERNED_BENCHMARK_ARMS.map((arm) => [arm.id, arm.env?.ROLLDOWN_DX_JSON_CACHE]),
);
const REQUIRED_OFFICIAL_BASELINE_FIELDS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.officialBaselineEvidence.requiredFields;
const REQUIRED_OFFICIAL_BASELINE_VALUES =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.officialBaselineEvidence.requiredValues;
const REQUIRED_LOCAL_BUILD_FIELDS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.localBuildEvidence.requiredFields;
const REQUIRED_LOCAL_BUILD_VALUES =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.localBuildEvidence.requiredValues;
const REQUIRED_ARM_METRIC_FIELDS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.perArmEvidence.requiredFields;
const REQUIRED_SOURCE_RECEIPT_FIELDS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.sourceReceiptEvidence.requiredFields;
const REQUIRED_SOURCE_RECEIPT_VALUES =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.sourceReceiptEvidence.requiredValues;
const REQUIRED_CACHE_ARTIFACT_FIELDS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.requiredFields;
const POSITIVE_TIMING_FIELDS = [
  'meanMs',
  'medianMs',
  'p95Ms',
  'minMs',
  'maxMs',
  'coldCacheMs',
  'hotCacheMs',
];

const MINIMUM_ITERATIONS = GOVERNED_BENCHMARK_SUCCESS_CRITERIA.minimumIterations;
const MINIMUM_WARMUP_ITERATIONS = GOVERNED_BENCHMARK_SUCCESS_CRITERIA.minimumWarmupIterations;
const MINIMUM_FIXTURE_BYTES_EXCLUSIVE = 16 * 1024;
const MINIMUM_RELATIVE_IMPROVEMENT_PCT =
  GOVERNED_BENCHMARK_SUCCESS_CRITERIA.speedupClaimValidation.minimumRelativeImprovementPct;
const MINIMUM_ABSOLUTE_IMPROVEMENT_MS =
  GOVERNED_BENCHMARK_SUCCESS_CRITERIA.speedupClaimValidation.minimumAbsoluteImprovementMs;
const REQUIRED_MACHINE_MAGIC =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.requiredMachineMagic;
const REQUIRED_HOT_CACHE_COUNTERS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence
    .requiredHotCacheCounters ?? [];
const FORBIDDEN_MACHINE_MAGIC =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence
    .forbiddenMachineMagic ?? [];
const REQUIRED_MACHINE_ARTIFACT_BENEFITS =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence
    .requiredMachineArtifactBenefits ?? [];

const REQUIRED_TIMING_WINDOW_VALUES =
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.timingWindowEvidence.requiredValues;
const OFFICIAL_BASELINE_NON_EMPTY_FIELDS = [
  'packageVersion',
  'packageManager',
  'packageRegistry',
  'packageResolvedUrl',
  'packageInstallRoot',
  'packageInstallRootRealpath',
  'lockfilePath',
  'lockfileEntryKey',
  'packageIntegrity',
  'mainEntrypointPath',
  'binEntrypointPath',
  'nativeBindingPath',
];
const OFFICIAL_BASELINE_SHA256_FIELDS = [
  'lockfileSha256',
  'installedPackageJsonSha256',
  'mainEntrypointSha256',
  'binEntrypointSha256',
  'nativeBindingSha256',
];
const LOCAL_BUILD_NON_EMPTY_FIELDS = [
  'gitCommit',
  'packageVersion',
  'packageInstallRoot',
  'packageInstallRootRealpath',
  'binaryPath',
  'binaryRealpath',
  'nativeBindingPath',
  'nativeBindingRealpath',
  'buildCommand',
  'buildProfile',
];
const LOCAL_BUILD_SHA256_FIELDS = ['binarySha256', 'nativeBindingSha256'];
const FREE_TEXT_NUMERIC_SPEEDUP_CLAIM_PATTERN =
  /\b\d+(?:\.\d+)?\s*(?:x|times?|percent(?:age)?|milliseconds?|ms)\b|\b\d+(?:\.\d+)?\s*%/iu;

export function validateGovernedBenchmarkExecutionSpeedupClaimGate(
  receipt,
  validationContext = {},
) {
  if (receipt?.speedupClaim === 'none' && receipt?.claimStatus === 'not_proven_source_receipt_only') {
    return { ok: true, errors: [] };
  }

  const errors = [];
  const benchmarkExecution = objectAt(receipt, 'benchmarkExecution', errors);
  const sourceReceiptEvidence = objectAt(
    benchmarkExecution,
    'sourceReceiptEvidence',
    errors,
  );
  const officialBaselineEvidence = objectAt(
    benchmarkExecution,
    'officialBaselineEvidence',
    errors,
  );
  const localBuildEvidence = objectAt(benchmarkExecution, 'localBuildEvidence', errors);
  const cacheArtifactEvidence = objectAt(
    benchmarkExecution,
    'cacheArtifactEvidence',
    errors,
  );
  const timingWindowEvidence = objectAt(
    benchmarkExecution,
    'timingWindowEvidence',
    errors,
  );
  const perArmEvidence = arrayAt(benchmarkExecution, 'perArmEvidence', errors);

  requireExact(receipt?.receiptKind, 'governed_benchmark_execution', 'receiptKind', errors);
  requireExact(receipt?.claimStatus, 'proven', 'claimStatus', errors);
  requireExact(receipt?.benchmarkStatus, 'measured', 'benchmarkStatus', errors);
  requireExact(receipt?.upstreamComparison, 'measured', 'upstreamComparison', errors);
  requireNonEmptyString(receipt?.speedupClaim, 'speedupClaim', errors);
  validateSpeedupClaimText(receipt?.speedupClaim, errors);
  requireExact(benchmarkExecution?.executed, true, 'benchmarkExecution.executed', errors);

  validateExecutionEnvironment(receipt?.environment, errors);
  validateSourceReceiptEvidence(
    sourceReceiptEvidence,
    localBuildEvidence,
    validationContext,
    errors,
  );
  validateOfficialBaselineEvidence(officialBaselineEvidence, errors);
  validateLocalBuildEvidence(localBuildEvidence, errors);
  validateOfficialBaselineIsolation(
    officialBaselineEvidence,
    localBuildEvidence,
    validationContext?.sourceReceipt,
    errors,
  );
  const armsById = validatePerArmEvidence(perArmEvidence, errors);
  validateSelectedFixtureConsistency(armsById, errors);
  validateCacheArtifactEvidence(cacheArtifactEvidence, armsById, errors);
  validateOutputEquality(benchmarkExecution?.outputEquality, armsById, errors);
  validateStatisticalEvidence(benchmarkExecution?.statisticalEvidence, errors);
  validateTimingWindowEvidence(timingWindowEvidence, errors);
  validateSpeedupMath(armsById, errors);

  return { ok: errors.length === 0, errors };
}

function validateExecutionEnvironment(environment, errors) {
  if (!environment || typeof environment !== 'object' || Array.isArray(environment)) {
    errors.push('environment must be an object');
    return;
  }
  for (const field of ['nodeVersion', 'rustcVersion', 'platform', 'cpu']) {
    requireNonEmptyStringValue(environment[field], `environment.${field}`, errors);
  }
  requireNonEmptyStringValue(
    environment.powerThermalNotes,
    'environment.powerThermalNotes',
    errors,
  );
}

function validateSourceReceiptEvidence(evidence, localBuildEvidence, validationContext, errors) {
  if (!evidence) {
    return;
  }
  requireFields(evidence, REQUIRED_SOURCE_RECEIPT_FIELDS, 'sourceReceiptEvidence', errors);
  requireExact(
    evidence.path,
    REQUIRED_SOURCE_RECEIPT_VALUES.path,
    'sourceReceiptEvidence.path',
    errors,
  );
  requireExact(
    evidence.gitDirty,
    REQUIRED_SOURCE_RECEIPT_VALUES.gitDirty,
    'sourceReceiptEvidence.gitDirty',
    errors,
  );
  requireSha256Hex(evidence.sha256, 'sourceReceiptEvidence.sha256', errors);
  requireSha256Hex(
    evidence.sourceFilesSha256,
    'sourceReceiptEvidence.sourceFilesSha256',
    errors,
  );
  requireNonEmptyStringValue(
    evidence.gitCommit,
    'sourceReceiptEvidence.gitCommit',
    errors,
  );
  if (
    typeof evidence.gitCommit === 'string' &&
    typeof localBuildEvidence?.gitCommit === 'string' &&
    evidence.gitCommit !== localBuildEvidence.gitCommit
  ) {
    errors.push('sourceReceiptEvidence.gitCommit must match localBuildEvidence.gitCommit');
  }
  validateSourceReceiptContextBinding(evidence, localBuildEvidence, validationContext, errors);
}

function validateSourceReceiptContextBinding(
  evidence,
  localBuildEvidence,
  validationContext,
  errors,
) {
  const sourceReceiptSha256 = validationContext?.sourceReceiptSha256;
  const sourceReceipt = validationContext?.sourceReceipt;

  if (typeof sourceReceiptSha256 !== 'string' || sourceReceiptSha256.length === 0) {
    errors.push('validated source receipt sha256 is required');
  } else if (!isSha256Hex(sourceReceiptSha256)) {
    errors.push('validated source receipt sha256 must be a sha256 hex digest');
  } else if (isSha256Hex(evidence?.sha256) && evidence.sha256 !== sourceReceiptSha256) {
    errors.push('sourceReceiptEvidence.sha256 must match validated source receipt sha256');
  }

  if (!sourceReceipt || typeof sourceReceipt !== 'object' || Array.isArray(sourceReceipt)) {
    errors.push('validated source receipt is required');
    return;
  }

  if (
    sourceReceipt.claimStatus !== SOURCE_RECEIPT_CLAIM_STATUS ||
    sourceReceipt.benchmarkStatus !== 'not_run' ||
    sourceReceipt.upstreamComparison !== 'not_measured' ||
    sourceReceipt.speedupClaim !== 'none' ||
    sourceReceipt.benchmarkExecution?.executed !== false
  ) {
    errors.push('validated source receipt must be source-only');
  }

  if (!isSha256Hex(sourceReceipt.sourceFilesSha256)) {
    errors.push('validated source receipt sourceFilesSha256 must be a sha256 hex digest');
  } else if (
    isSha256Hex(evidence?.sourceFilesSha256) &&
    evidence.sourceFilesSha256 !== sourceReceipt.sourceFilesSha256
  ) {
    errors.push(
      'sourceReceiptEvidence.sourceFilesSha256 must match validated source receipt sourceFilesSha256',
    );
  }

  const sourceReceiptCommit = sourceReceipt.repos?.local?.commit;
  if (typeof sourceReceiptCommit !== 'string' || sourceReceiptCommit.length === 0) {
    errors.push('validated source receipt commit is required');
  } else {
    if (typeof evidence?.gitCommit === 'string' && evidence.gitCommit !== sourceReceiptCommit) {
      errors.push('sourceReceiptEvidence.gitCommit must match validated source receipt commit');
    }
    if (
      typeof localBuildEvidence?.gitCommit === 'string' &&
      localBuildEvidence.gitCommit !== sourceReceiptCommit
    ) {
      errors.push('localBuildEvidence.gitCommit must match validated source receipt commit');
    }
  }

  if (sourceReceipt.repos?.local?.dirty !== false) {
    errors.push('validated source receipt dirty must be false');
  }
}

function validateOfficialBaselineIsolation(
  officialEvidence,
  localEvidence,
  sourceReceipt,
  errors,
) {
  if (!officialEvidence || !localEvidence) {
    return;
  }
  const localRoots = [
    ['localBuildEvidence.packageInstallRootRealpath', localEvidence.packageInstallRootRealpath],
    ['localBuildEvidence.packageInstallRoot', localEvidence.packageInstallRoot],
    ['validatedSourceReceipt.repos.local.root', sourceReceipt?.repos?.local?.root],
  ].filter((entry) => typeof entry[1] === 'string');
  const officialPaths = [
    ['officialBaselineEvidence.packageInstallRootRealpath', officialEvidence.packageInstallRootRealpath],
    ['officialBaselineEvidence.packageInstallRoot', officialEvidence.packageInstallRoot],
    ['officialBaselineEvidence.mainEntrypointPath', officialEvidence.mainEntrypointPath],
    ['officialBaselineEvidence.binEntrypointPath', officialEvidence.binEntrypointPath],
    ['officialBaselineEvidence.nativeBindingPath', officialEvidence.nativeBindingPath],
  ].filter((entry) => typeof entry[1] === 'string');

  for (const [officialLabel, officialPath] of officialPaths) {
    for (const [localLabel, localRoot] of localRoots) {
      if (sameOrChildReceiptPath(officialPath, localRoot)) {
        errors.push(`${officialLabel} must stay outside ${localLabel}`);
      }
    }
  }
}

function validateSelectedFixtureConsistency(armsById, errors) {
  const selectedFixtureBytes = GOVERNED_BENCHMARK_EXECUTION_ARMS.map((arm) => [
    arm,
    armsById.get(arm)?.selectedFixtureBytes,
  ]).filter((entry) => Number.isFinite(entry[1]));
  if (selectedFixtureBytes.length !== GOVERNED_BENCHMARK_EXECUTION_ARMS.length) {
    return;
  }
  const uniqueByteCounts = new Set(selectedFixtureBytes.map((entry) => entry[1]));
  if (uniqueByteCounts.size !== 1) {
    errors.push('selectedFixtureBytes must match across governed benchmark arms');
  }
}

function validateOfficialBaselineEvidence(evidence, errors) {
  if (!evidence) {
    return;
  }
  requireFields(evidence, REQUIRED_OFFICIAL_BASELINE_FIELDS, 'officialBaselineEvidence', errors);
  requireNonEmptyStringFields(
    evidence,
    OFFICIAL_BASELINE_NON_EMPTY_FIELDS,
    'officialBaselineEvidence',
    errors,
  );
  requireSha256Fields(
    evidence,
    OFFICIAL_BASELINE_SHA256_FIELDS,
    'officialBaselineEvidence',
    errors,
  );
  requireExact(evidence.arm, 'upstream-stock', 'officialBaselineEvidence.arm', errors);
  requireExact(
    evidence.packageName,
    REQUIRED_OFFICIAL_BASELINE_VALUES.packageName,
    'officialBaselineEvidence.packageName',
    errors,
  );
  requireExact(
    evidence.packageRegistryOrigin,
    REQUIRED_OFFICIAL_BASELINE_VALUES.packageRegistryOrigin,
    'officialBaselineEvidence.packageRegistryOrigin',
    errors,
  );
  requireExact(
    evidence.packageResolvedUrlOrigin,
    REQUIRED_OFFICIAL_BASELINE_VALUES.packageResolvedUrlOrigin,
    'officialBaselineEvidence.packageResolvedUrlOrigin',
    errors,
  );
  requireExact(
    evidence.globalInstall,
    REQUIRED_OFFICIAL_BASELINE_VALUES.globalInstall,
    'officialBaselineEvidence.globalInstall',
    errors,
  );
  requireExact(
    evidence.workspaceLink,
    REQUIRED_OFFICIAL_BASELINE_VALUES.workspaceLink,
    'officialBaselineEvidence.workspaceLink',
    errors,
  );
}

function sameOrChildReceiptPath(childPath, parentPath) {
  const child = normalizeReceiptComparisonPath(childPath);
  const parent = normalizeReceiptComparisonPath(parentPath);
  return child === parent || child.startsWith(`${parent}/`);
}

function normalizeReceiptComparisonPath(inputPath) {
  return inputPath.replaceAll('\\', '/').replace(/\/+$/u, '').toLowerCase();
}

function validateLocalBuildEvidence(evidence, errors) {
  if (!evidence) {
    return;
  }
  requireFields(evidence, REQUIRED_LOCAL_BUILD_FIELDS, 'localBuildEvidence', errors);
  requireNonEmptyStringFields(
    evidence,
    LOCAL_BUILD_NON_EMPTY_FIELDS,
    'localBuildEvidence',
    errors,
  );
  requireSha256Fields(evidence, LOCAL_BUILD_SHA256_FIELDS, 'localBuildEvidence', errors);
  requireExact(evidence.arm, 'local-cache-enabled', 'localBuildEvidence.arm', errors);
  requireExact(
    evidence.gitDirty,
    REQUIRED_LOCAL_BUILD_VALUES.gitDirty,
    'localBuildEvidence.gitDirty',
    errors,
  );
  requireExact(
    evidence.buildProfile,
    REQUIRED_LOCAL_BUILD_VALUES.buildProfile,
    'localBuildEvidence.buildProfile',
    errors,
  );
  requireArrayIncludes(
    evidence.buildFeatures,
    REQUIRED_LOCAL_BUILD_VALUES.buildFeatures,
    'localBuildEvidence.buildFeatures',
    errors,
  );
  requireExact(
    evidence.builtOutsideTimedSection,
    REQUIRED_LOCAL_BUILD_VALUES.builtOutsideTimedSection,
    'localBuildEvidence.builtOutsideTimedSection',
    errors,
  );
  if (!Array.isArray(evidence.gitStatusShort)) {
    errors.push('localBuildEvidence.gitStatusShort must be an array');
  } else if (evidence.gitDirty === false && evidence.gitStatusShort.length !== 0) {
    errors.push('localBuildEvidence.gitStatusShort must be empty');
  }
}

function validatePerArmEvidence(perArmEvidence, errors) {
  const armsById = new Map();
  for (const armEvidence of perArmEvidence) {
    const arm = armEvidence?.arm;
    if (typeof arm !== 'string') {
      errors.push('perArmEvidence arm is required');
      continue;
    }
    if (!GOVERNED_BENCHMARK_EXECUTION_ARMS.includes(arm)) {
      errors.push(`unexpected governed benchmark arm: ${arm}`);
      continue;
    }
    if (armsById.has(arm)) {
      errors.push(`duplicate governed benchmark arm: ${arm}`);
      continue;
    }
    armsById.set(arm, armEvidence);
    requireFields(armEvidence, REQUIRED_ARM_METRIC_FIELDS, `perArmEvidence.${arm}`, errors);
    validateArmMetrics(arm, armEvidence, errors);
  }

  for (const arm of GOVERNED_BENCHMARK_EXECUTION_ARMS) {
    if (!armsById.has(arm)) {
      errors.push(`missing governed benchmark arm: ${arm}`);
    }
  }
  return armsById;
}

function validateArmMetrics(arm, evidence, errors) {
  const expectedCacheEnv = GOVERNED_ARM_CACHE_ENV.get(arm);
  if (expectedCacheEnv !== undefined) {
    requireCacheEnv(evidence, expectedCacheEnv, arm, errors);
  }
  for (const field of REQUIRED_ARM_METRIC_FIELDS) {
    if (field === 'arm' || field === 'outputHash') {
      continue;
    }
    if (!Number.isFinite(evidence?.[field])) {
      errors.push(`perArmEvidence.${arm}.${field} must be a finite number`);
    }
  }
  for (const field of POSITIVE_TIMING_FIELDS) {
    if (Number.isFinite(evidence?.[field]) && evidence[field] <= 0) {
      errors.push(`${arm} ${field} must be > 0`);
    }
  }
  if (Number.isFinite(evidence?.stdDevMs) && evidence.stdDevMs < 0) {
    errors.push(`${arm} stdDevMs must be >= 0`);
  }
  if (Number.isFinite(evidence?.iterations) && evidence.iterations < MINIMUM_ITERATIONS) {
    errors.push(`${arm} iterations must be >= ${MINIMUM_ITERATIONS}`);
  }
  if (
    Number.isFinite(evidence?.warmupIterations) &&
    evidence.warmupIterations < MINIMUM_WARMUP_ITERATIONS
  ) {
    errors.push(`${arm} warmupIterations must be >= ${MINIMUM_WARMUP_ITERATIONS}`);
  }
  if (
    Number.isFinite(evidence?.selectedFixtureBytes) &&
    evidence.selectedFixtureBytes <= MINIMUM_FIXTURE_BYTES_EXCLUSIVE
  ) {
    errors.push(`${arm} selectedFixtureBytes must be > ${MINIMUM_FIXTURE_BYTES_EXCLUSIVE}`);
  }
  if (typeof evidence?.outputHash !== 'string' || !/^[a-f0-9]{64}$/.test(evidence.outputHash)) {
    errors.push(`perArmEvidence.${arm}.outputHash must be a sha256 hex digest`);
  }
  if (
    Number.isFinite(evidence?.minMs) &&
    Number.isFinite(evidence?.medianMs) &&
    evidence.minMs > evidence.medianMs
  ) {
    errors.push(`${arm} minMs must be <= medianMs`);
  }
  if (
    Number.isFinite(evidence?.medianMs) &&
    Number.isFinite(evidence?.p95Ms) &&
    evidence.medianMs > evidence.p95Ms
  ) {
    errors.push(`${arm} medianMs must be <= p95Ms`);
  }
  if (
    Number.isFinite(evidence?.p95Ms) &&
    Number.isFinite(evidence?.maxMs) &&
    evidence.p95Ms > evidence.maxMs
  ) {
    errors.push(`${arm} p95Ms must be <= maxMs`);
  }
}

function requireCacheEnv(evidence, expected, arm, errors) {
  const actual = evidence?.env?.ROLLDOWN_DX_JSON_CACHE;
  if (actual !== expected) {
    errors.push(`${arm} env.ROLLDOWN_DX_JSON_CACHE must be ${expected}`);
  }
}

function validateCacheArtifactEvidence(evidence, armsById, errors) {
  if (!evidence) {
    return;
  }
  requireFields(evidence, REQUIRED_CACHE_ARTIFACT_FIELDS, 'cacheArtifactEvidence', errors);
  requireExact(evidence.arm, 'local-cache-enabled', 'cacheArtifactEvidence.arm', errors);
  requirePositiveNumber(
    evidence.dxRolldownFileCount,
    'cacheArtifactEvidence.dxRolldownFileCount',
    errors,
  );
  requirePositiveNumber(evidence.dxRolldownBytes, 'cacheArtifactEvidence.dxRolldownBytes', errors);
  const machineMagicCounts = objectAt(evidence, 'machineMagicCounts', errors);
  if (machineMagicCounts) {
    for (const requiredMagic of REQUIRED_MACHINE_MAGIC) {
      requireAtLeast(
        machineMagicCounts[requiredMagic.magic],
        requiredMagic.minimumCount,
        `machineMagicCounts.${requiredMagic.magic}`,
        errors,
      );
    }
    for (const forbiddenMagic of FORBIDDEN_MACHINE_MAGIC) {
      if (Number.isFinite(machineMagicCounts[forbiddenMagic.magic])) {
        requireExact(
          machineMagicCounts[forbiddenMagic.magic],
          0,
          `machineMagicCounts.${forbiddenMagic.magic}`,
          errors,
        );
      }
    }
  }
  const machineReadHitCounts = objectAt(evidence, 'machineReadHitCounts', errors);
  if (machineReadHitCounts) {
    const localEnabledIterations = armsById.get('local-cache-enabled')?.iterations;
    for (const requiredMagic of REQUIRED_MACHINE_MAGIC) {
      requireAtLeast(
        machineReadHitCounts[requiredMagic.magic],
        requiredMagic.minimumCount,
        `machineReadHitCounts.${requiredMagic.magic}`,
        errors,
      );
      if (Number.isFinite(localEnabledIterations)) {
        requireAtLeast(
          machineReadHitCounts[requiredMagic.magic],
          localEnabledIterations,
          `machineReadHitCounts.${requiredMagic.magic}`,
          errors,
        );
      }
    }
  }
  validateArtifactExerciseMatrixEvidence(evidence?.artifactExerciseMatrix, errors);
  validateMachineArtifactBenefitEvidence(evidence?.machineArtifactBenefitEvidence, errors);
  validateMachineHotCacheCounters(evidence?.machineHotCacheCounters, errors);
  requireExact(
    evidence.machineWriteCountDuringTiming,
    0,
    'cacheArtifactEvidence.machineWriteCountDuringTiming',
    errors,
  );
  requireExact(
    evidence.machineRepairCountDuringTiming,
    0,
    'cacheArtifactEvidence.machineRepairCountDuringTiming',
    errors,
  );
}

function validateArtifactExerciseMatrixEvidence(artifactExerciseMatrix, errors) {
  const validation = validateArtifactExerciseMatrix(artifactExerciseMatrix);
  if (!validation.ok) {
    errors.push(...validation.errors);
  }
}

function validateMachineArtifactBenefitEvidence(benefitEvidence, errors) {
  if (!benefitEvidence || typeof benefitEvidence !== 'object' || Array.isArray(benefitEvidence)) {
    for (const requiredBenefit of REQUIRED_MACHINE_ARTIFACT_BENEFITS) {
      errors.push(`machineArtifactBenefitEvidence.${requiredBenefit.magic} is required`);
    }
    return;
  }

  for (const requiredBenefit of REQUIRED_MACHINE_ARTIFACT_BENEFITS) {
    const benefit = benefitEvidence[requiredBenefit.magic];
    if (!benefit || typeof benefit !== 'object' || Array.isArray(benefit)) {
      errors.push(`machineArtifactBenefitEvidence.${requiredBenefit.magic} is required`);
      continue;
    }

    requirePositiveNumber(
      benefit.sourceBytes,
      `machineArtifactBenefitEvidence.${requiredBenefit.magic}.sourceBytes`,
      errors,
    );
    requirePositiveNumber(
      benefit.machineBytes,
      `machineArtifactBenefitEvidence.${requiredBenefit.magic}.machineBytes`,
      errors,
    );
    if (
      requiredBenefit.machineBytesMustBeLessThanSourceBytes &&
      Number.isFinite(benefit.machineBytes) &&
      Number.isFinite(benefit.sourceBytes) &&
      benefit.machineBytes >= benefit.sourceBytes
    ) {
      errors.push(
        `machineArtifactBenefitEvidence.${requiredBenefit.magic}.machineBytes must be < sourceBytes`,
      );
    }
    if (!requiredBenefit.allowedSourceShapes?.includes(benefit.sourceShape)) {
      errors.push(
        `machineArtifactBenefitEvidence.${requiredBenefit.magic}.sourceShape must be one of ${requiredBenefit.allowedSourceShapes.join(', ')}`,
      );
    }
  }
}

function validateMachineHotCacheCounters(machineHotCacheCounters, errors) {
  if (
    !machineHotCacheCounters ||
    typeof machineHotCacheCounters !== 'object' ||
    Array.isArray(machineHotCacheCounters)
  ) {
    for (const requiredCounters of REQUIRED_HOT_CACHE_COUNTERS) {
      errors.push(`machineHotCacheCounters.${requiredCounters.magic} is required`);
    }
    return;
  }

  for (const requiredCounters of REQUIRED_HOT_CACHE_COUNTERS) {
    const counters = machineHotCacheCounters[requiredCounters.magic];
    if (!counters || typeof counters !== 'object' || Array.isArray(counters)) {
      errors.push(`machineHotCacheCounters.${requiredCounters.magic} is required`);
      continue;
    }

    for (const [counterName, expected] of Object.entries(requiredCounters.exact ?? {})) {
      requireExact(
        counters[counterName],
        expected,
        `machineHotCacheCounters.${requiredCounters.magic}.${counterName}`,
        errors,
      );
    }
    for (const [counterName, minimum] of Object.entries(requiredCounters.minimum ?? {})) {
      requireAtLeast(
        counters[counterName],
        minimum,
        `machineHotCacheCounters.${requiredCounters.magic}.${counterName}`,
        errors,
      );
    }
  }
}

function validateOutputEquality(outputEquality, armsById, errors) {
  const hashes = GOVERNED_BENCHMARK_EXECUTION_ARMS.map((arm) => armsById.get(arm)?.outputHash);
  if (hashes.some((hash) => typeof hash !== 'string')) {
    return;
  }
  if (new Set(hashes).size !== 1) {
    errors.push('perArmEvidence outputHash values must match across governed arms');
  }
  if (!outputEquality || typeof outputEquality !== 'object' || Array.isArray(outputEquality)) {
    errors.push('outputEquality must be an object');
    return;
  }
  requireExact(outputEquality.hashAlgorithm, 'sha256', 'outputEquality.hashAlgorithm', errors);
  requireArrayMembers(
    outputEquality.matchedArms,
    GOVERNED_BENCHMARK_EXECUTION_ARMS,
    'outputEquality.matchedArms',
    errors,
  );
  requireExact(outputEquality.matchingOutputHash, hashes[0], 'outputEquality.matchingOutputHash', errors);
}

function validateStatisticalEvidence(evidence, errors) {
  if (!evidence || typeof evidence !== 'object' || Array.isArray(evidence)) {
    errors.push('statisticalEvidence must be an object');
    return;
  }
  requireExact(
    evidence.confidenceLevel,
    GOVERNED_BENCHMARK_SUCCESS_CRITERIA.speedupClaimValidation.confidenceLevel,
    'statisticalEvidence.confidenceLevel',
    errors,
  );
  requireExact(
    evidence.confidenceIntervalExcludesZero,
    GOVERNED_BENCHMARK_SUCCESS_CRITERIA.speedupClaimValidation
      .requireConfidenceIntervalExcludesZero,
    'statisticalEvidence.confidenceIntervalExcludesZero',
    errors,
  );
}

function validateTimingWindowEvidence(evidence, errors) {
  if (!evidence) {
    return;
  }
  for (const [field, expected] of Object.entries(REQUIRED_TIMING_WINDOW_VALUES)) {
    if (!Object.hasOwn(evidence, field)) {
      errors.push(`timingWindowEvidence.${field} is required`);
      continue;
    }
    requireExact(evidence[field], expected, `timingWindowEvidence.${field}`, errors);
  }
}

function validateSpeedupMath(armsById, errors) {
  const enabled = armsById.get('local-cache-enabled');
  const baselines = [
    armsById.get('upstream-stock'),
    armsById.get('local-cache-disabled'),
  ].filter(Boolean);
  if (!enabled || baselines.length !== 2) {
    return;
  }

  for (const baseline of baselines) {
    validateMetricBeatsBaseline(enabled, baseline, 'medianMs', errors);
    validateMetricBeatsBaseline(enabled, baseline, 'p95Ms', errors);
  }
}

function validateSpeedupClaimText(speedupClaim, errors) {
  if (typeof speedupClaim !== 'string' || speedupClaim.length === 0) {
    return;
  }
  if (FREE_TEXT_NUMERIC_SPEEDUP_CLAIM_PATTERN.test(speedupClaim)) {
    errors.push('speedupClaim must not contain free-text numeric speed claims');
  }
}

function validateMetricBeatsBaseline(enabled, baseline, metric, errors) {
  if (!Number.isFinite(enabled?.[metric]) || !Number.isFinite(baseline?.[metric])) {
    return;
  }
  if (enabled[metric] >= baseline[metric]) {
    errors.push(`local-cache-enabled ${metric} must beat ${baseline.arm}`);
  }
  const absoluteImprovement = baseline[metric] - enabled[metric];
  if (absoluteImprovement < MINIMUM_ABSOLUTE_IMPROVEMENT_MS) {
    errors.push(
      `local-cache-enabled ${metric} must improve by at least ${MINIMUM_ABSOLUTE_IMPROVEMENT_MS}ms over ${baseline.arm}`,
    );
  }
  const relativeImprovementPct = (absoluteImprovement / baseline[metric]) * 100;
  if (relativeImprovementPct < MINIMUM_RELATIVE_IMPROVEMENT_PCT) {
    errors.push(
      `local-cache-enabled ${metric} must improve by at least ${MINIMUM_RELATIVE_IMPROVEMENT_PCT}% over ${baseline.arm}`,
    );
  }
}

function requireFields(value, fields, label, errors) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    errors.push(`${label} must be an object`);
    return;
  }
  for (const field of fields) {
    if (!Object.hasOwn(value, field)) {
      errors.push(`${label}.${field} is required`);
    }
  }
}

function requireNonEmptyStringFields(value, fields, label, errors) {
  for (const field of fields) {
    requireNonEmptyStringValue(value?.[field], `${label}.${field}`, errors);
  }
}

function requireSha256Fields(value, fields, label, errors) {
  for (const field of fields) {
    requireSha256Hex(value?.[field], `${label}.${field}`, errors);
  }
}

function requireExact(actual, expected, label, errors) {
  if (!Object.is(actual, expected)) {
    errors.push(`${label} must be ${String(expected)}`);
  }
}

function requireNonEmptyString(value, label, errors) {
  if (typeof value !== 'string' || value.length === 0 || value === 'none') {
    errors.push(`${label} must be a governed benchmark speedup claim`);
  }
}

function requireNonEmptyStringValue(value, label, errors) {
  if (typeof value !== 'string' || value.length === 0) {
    errors.push(`${label} is required`);
  }
}

function requirePositiveNumber(value, label, errors) {
  if (!Number.isFinite(value) || value <= 0) {
    errors.push(`${label} is required`);
  }
}

function requireSha256Hex(value, label, errors) {
  if (!isSha256Hex(value)) {
    errors.push(`${label} must be a sha256 hex digest`);
  }
}

function isSha256Hex(value) {
  return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
}

function requireAtLeast(value, minimum, label, errors) {
  if (!Number.isFinite(value) || value < minimum) {
    errors.push(`${label} must be >= ${minimum}`);
  }
}

function requireArrayMembers(actual, expected, label, errors) {
  if (!Array.isArray(actual)) {
    errors.push(`${label} must be an array`);
    return;
  }
  for (const entry of expected) {
    if (!actual.includes(entry)) {
      errors.push(`${label} must include ${entry}`);
    }
  }
  for (const entry of actual) {
    if (!expected.includes(entry)) {
      errors.push(`${label} contains unexpected arm ${entry}`);
    }
  }
  const seen = new Set();
  for (const entry of actual) {
    if (seen.has(entry)) {
      errors.push(`${label} contains duplicate arm ${entry}`);
      continue;
    }
    seen.add(entry);
  }
}

function requireArrayIncludes(actual, expected, label, errors) {
  if (!Array.isArray(actual)) {
    errors.push(`${label} must be an array`);
    return;
  }
  for (const entry of expected) {
    if (!actual.includes(entry)) {
      errors.push(`${label} must include ${entry}`);
    }
  }
}

function objectAt(parent, field, errors) {
  const value = parent?.[field];
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    errors.push(`${field} must be an object`);
    return {};
  }
  return value;
}

function arrayAt(parent, field, errors) {
  const value = parent?.[field];
  if (!Array.isArray(value)) {
    errors.push(`${field} must be an array`);
    return [];
  }
  return value;
}
