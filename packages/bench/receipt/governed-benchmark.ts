import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

import { validateGovernedBenchmarkExecutionSpeedupClaimGate } from './governed-execution-gate.ts';
import {
  GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS,
  GOVERNED_BENCHMARK_SUCCESS_CRITERIA,
  validateSourceBenchmarkReceipt,
} from './source-receipt.ts';

const dirname = path.dirname(url.fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '../../..');
const defaultExecutionReceiptPath = '.dx/rolldown/governed-benchmark-execution.json';
const defaultSourceReceiptPath = '.dx/rolldown/benchmark-source-receipt.json';

export const governedBenchmarkPlan = {
  arms: ['upstream-stock', 'local-cache-disabled', 'local-cache-enabled'],
  requiredCurrentMagic:
    GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.requiredMachineMagic.map(
      (entry) => entry.magic,
    ),
  forbiddenLegacyMagic:
    GOVERNED_BENCHMARK_EXECUTED_RECEIPT_REQUIREMENTS.cacheArtifactEvidence.forbiddenMachineMagic.map(
      (entry) => entry.magic,
    ),
  jsonMachineArtifactPolicy: {
    allowedSourceShapes: ['object', 'array'],
    machineBytesMustBeLessThanSourceBytes: true,
    excludedProfiles: ['top-level-scalar', 'stringify-only'],
  },
  timingPolicy: {
    machineGenerationIncludedInTimedSection: false,
    machineWritesDuringTiming: 0,
    machineRepairsDuringTiming: 0,
  },
  successCriteria: GOVERNED_BENCHMARK_SUCCESS_CRITERIA,
};

function runCli(args) {
  if (args.includes('--plan') || args.length === 0) {
    console.log(JSON.stringify(governedBenchmarkPlan, null, 2));
    return;
  }

  if (!args.includes('--validate')) {
    throw new Error('Use --plan or --validate [--receipt <path>] [--source-receipt <path>]');
  }

  const receiptPath = resolveRepoPath(
    readFlagValue(args, '--receipt') ?? defaultExecutionReceiptPath,
  );
  const sourceReceiptPath = resolveRepoPath(
    readFlagValue(args, '--source-receipt') ?? defaultSourceReceiptPath,
  );
  const receipt = readJson(receiptPath);
  const sourceReceiptText = fs.readFileSync(sourceReceiptPath, 'utf8');
  const sourceReceipt = JSON.parse(sourceReceiptText);
  const sourceValidation = validateSourceBenchmarkReceipt(sourceReceipt, { repoRoot });
  const sourceReceiptSha256 = crypto.createHash('sha256').update(sourceReceiptText).digest('hex');
  const gate = validateGovernedBenchmarkExecutionSpeedupClaimGate(receipt, {
    sourceReceipt,
    sourceReceiptSha256,
  });
  const summary = {
    ok: sourceValidation.ok && gate.ok,
    sourceReceipt: sourceValidation,
    governedGate: gate,
    comparison: summarizeMedianComparison(receipt),
  };

  console.log(JSON.stringify(summary, null, 2));
  if (!summary.ok) {
    process.exitCode = 1;
  }
}

function summarizeMedianComparison(receipt) {
  const arms = new Map(
    (receipt?.benchmarkExecution?.perArmEvidence ?? []).map((arm) => [arm.arm, arm]),
  );
  const official = arms.get('upstream-stock');
  const disabled = arms.get('local-cache-disabled');
  const enabled = arms.get('local-cache-enabled');
  if (!official || !disabled || !enabled) {
    return null;
  }

  return {
    medianMs: {
      upstreamStock: official.medianMs,
      localCacheDisabled: disabled.medianMs,
      localCacheEnabled: enabled.medianMs,
    },
    speedupVsOfficial: ratio(official.medianMs, enabled.medianMs),
    speedupVsLocalCacheDisabled: ratio(disabled.medianMs, enabled.medianMs),
    winner: lowestMedianArm([official, disabled, enabled]),
  };
}

function ratio(baseline, subject) {
  if (!Number.isFinite(baseline) || !Number.isFinite(subject) || subject <= 0) {
    return null;
  }
  return baseline / subject;
}

function lowestMedianArm(arms) {
  return arms
    .filter((arm) => Number.isFinite(arm.medianMs))
    .toSorted((left, right) => left.medianMs - right.medianMs)[0]?.arm ?? null;
}

function readFlagValue(args, flag) {
  const index = args.indexOf(flag);
  if (index === -1) {
    return null;
  }
  const value = args[index + 1];
  if (!value) {
    throw new Error(`Missing value after ${flag}`);
  }
  return value;
}

function resolveRepoPath(inputPath) {
  const resolved = path.resolve(repoRoot, inputPath);
  const relative = path.relative(repoRoot, resolved);
  if (relative.startsWith('..') || path.isAbsolute(relative)) {
    throw new Error('receipt paths must stay inside the repo root');
  }
  return resolved;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

if (process.argv[1] && path.resolve(process.argv[1]) === url.fileURLToPath(import.meta.url)) {
  try {
    runCli(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
