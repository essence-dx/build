import assert from 'node:assert/strict';
import test from 'node:test';

import { governedBenchmarkPlan } from './governed-benchmark.ts';
import { GOVERNED_BENCHMARK_SUCCESS_CRITERIA } from './source-receipt.ts';

test('governed benchmark plan mirrors current machine magic and success gate', () => {
  assert.deepEqual(governedBenchmarkPlan.requiredCurrentMagic, [
    'RDXCJSR2',
    'RDXJSON2',
    'RDXCSSRK',
    'RDXOPDRK',
  ]);
  assert.deepEqual(governedBenchmarkPlan.forbiddenLegacyMagic, ['RDXCJSRK', 'RDXCJSN3']);
  assert.deepEqual(governedBenchmarkPlan.successCriteria, GOVERNED_BENCHMARK_SUCCESS_CRITERIA);
  assert.equal(governedBenchmarkPlan.successCriteria.minimumIterations, 30);
  assert.equal(governedBenchmarkPlan.successCriteria.minimumWarmupIterations, 5);
  assert.deepEqual(governedBenchmarkPlan.successCriteria.p95MustBeat, [
    'upstream-stock',
    'local-cache-disabled',
  ]);
  assert.equal(
    governedBenchmarkPlan.successCriteria.speedupClaimValidation
      .requireConfidenceIntervalExcludesZero,
    true,
  );
});
