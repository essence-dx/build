import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import url from 'node:url';

import {
  buildDxLighthousePackageContract,
  parseDxLighthousePackageContractArgs,
} from './lighthouse-package-contract.ts';

const dirname = path.dirname(url.fileURLToPath(import.meta.url));
const repoRoot = path.resolve(dirname, '..', '..', '..');
const scriptPath = path.join(dirname, 'lighthouse-package-contract.ts');
const rootPackageJsonPath = path.join(repoRoot, 'package.json');
const benchPackageJsonPath = path.join(repoRoot, 'packages', 'bench', 'package.json');
const packageScript = 'receipt:lighthouse-package-contract';
const packageScriptCommand = 'node ./receipt/lighthouse-package-contract.ts';
const rootPackageScript = 'dx:lighthouse:package-contract';
const rootPackageScriptCommand = 'node ./packages/bench/receipt/lighthouse-package-contract.ts';
const contractUsage = 'node packages/bench/receipt/lighthouse-package-contract.ts --contract --json';
const packageProofTableColumns = [
  'runtime_id',
  'provider',
  'package_name',
  'status',
  'build_receipt_hash_blake3',
  'package_assets_filesystem_addressable',
  'dynamic_imports_runtime_compatible',
  'node_builtins_runtime_compatible',
  'chrome_launcher_unstubbed',
];

test('describes a metadata-only DX Build Lighthouse package contract without verified proof claims', () => {
  const existingFiles = new Set([
    slash(rootPackageJsonPath),
    slash(benchPackageJsonPath),
    slash(scriptPath),
    slash(path.join(repoRoot, 'packages', 'rolldown', 'bin', 'cli.mjs')),
  ]);
  const existingDirectories = new Set([slash(repoRoot)]);

  const contract = buildDxLighthousePackageContract({
    repoRoot,
    fileExists: candidate => existingFiles.has(slash(candidate)),
    directoryExists: candidate => existingDirectories.has(slash(candidate)),
    readText: candidate => manifestFor(candidate),
  });

  assert.equal(contract.schema_name, 'dx.build.lighthouse_package_contract');
  assert.equal(contract.schema_revision, 1);
  assert.equal(contract.command, 'dx build lighthouse --contract --json');
  assert.equal(contract.package_script, packageScript);
  assert.equal(contract.package_script_command, packageScriptCommand);
  assert.equal(contract.package_script_usage, contractUsage);
  assert.equal(contract.root_package_script, rootPackageScript);
  assert.equal(contract.root_package_script_command, rootPackageScriptCommand);
  assert.equal(contract.source_script, 'packages/bench/receipt/lighthouse-package-contract.ts');
  assert.equal(contract.status, 'not_ready');
  assert.equal(contract.build_engine.id, 'dx-build');
  assert.equal(contract.build_engine.present, true);
  assert.deepEqual(contract.lighthouse_package, {
    package_name: 'lighthouse',
    package_present: false,
    node_engine_required: true,
    chrome_headless_required: true,
    assets_must_remain_filesystem_addressable: true,
    dynamic_imports_must_remain_runtime_compatible: true,
    node_builtins_must_remain_runtime_compatible: true,
    chrome_launcher_must_not_be_stubbed: true,
  });
  assert.deepEqual(contract.check_package_proof_gate, {
    equivalence_receipt_schema: 'dx.check.web_lighthouse_equivalence.v1',
    source_receipt: '.dx/check/web-lighthouse-equivalence.sr',
    machine_receipt: '.dx/serializer/check-web-lighthouse-equivalence.machine',
    package_proof_table: 'web_lighthouse_package_proofs',
    package_proof_table_columns: packageProofTableColumns,
    required_provider: 'dx-build',
    required_package_name: 'lighthouse',
    required_package_status: 'verified',
    required_hash_algorithm: 'blake3',
    required_true_fields: [
      'package_assets_filesystem_addressable',
      'dynamic_imports_runtime_compatible',
      'node_builtins_runtime_compatible',
      'chrome_launcher_unstubbed',
    ],
  });
  assert.deepEqual(contract.availability, {
    available: false,
    package_present: false,
    package_script_available: true,
    root_package_script_available: true,
    build_cli_present: true,
    package_proof_receipt_available: false,
    machine_receipt_available: false,
  });
  assert.deepEqual(contract.proof_generation, {
    emits_verified_proof: false,
    writes_receipts: false,
    executes_lighthouse: false,
    launches_chrome: false,
  });
  assert.deepEqual(contract.redaction, {
    metadata_only: true,
    stores_lhr_json: false,
    stores_traces: false,
    stores_screenshots: false,
    launches_chrome: false,
  });
  assert.deepEqual(contract.output_contract, {
    stdout: 'json',
    stderr: 'errors_only',
    verified_rows_emitted: false,
  });
  assert.ok(contract.blockers.includes('official_lighthouse_package_not_installed'));
  assert.ok(contract.blockers.includes('dx_build_lighthouse_package_proof_not_generated'));
});

test('keeps the package contract unavailable when Lighthouse is installed without package proof receipts', () => {
  const lighthousePackage = path.join(repoRoot, 'node_modules', 'lighthouse', 'package.json');
  const existingFiles = new Set([
    slash(rootPackageJsonPath),
    slash(benchPackageJsonPath),
    slash(scriptPath),
    slash(lighthousePackage),
    slash(path.join(repoRoot, 'packages', 'rolldown', 'bin', 'cli.mjs')),
  ]);

  const contract = buildDxLighthousePackageContract({
    repoRoot,
    fileExists: candidate => existingFiles.has(slash(candidate)),
    directoryExists: candidate => slash(candidate) === slash(repoRoot),
    readText: candidate => manifestFor(candidate),
  });

  assert.equal(contract.status, 'not_ready');
  assert.equal(contract.availability.available, false);
  assert.equal(contract.availability.package_present, true);
  assert.equal(contract.lighthouse_package.package_present, true);
  assert.equal(contract.proof_generation.emits_verified_proof, false);
  assert.equal(contract.proof_generation.executes_lighthouse, false);
  assert.equal(contract.proof_generation.launches_chrome, false);
  assert.ok(!contract.blockers.includes('official_lighthouse_package_not_installed'));
  assert.ok(contract.blockers.includes('dx_build_lighthouse_package_proof_not_generated'));
});

test('reports package-script routing as unavailable when package.json or script wiring drifts', () => {
  const existingFiles = new Set([slash(rootPackageJsonPath), slash(benchPackageJsonPath), slash(scriptPath)]);

  const missingPackageScript = buildDxLighthousePackageContract({
    repoRoot,
    fileExists: candidate => existingFiles.has(slash(candidate)),
    directoryExists: candidate => slash(candidate) === slash(repoRoot),
    readText: candidate =>
      slash(candidate) === slash(rootPackageJsonPath)
        ? JSON.stringify({ scripts: { [rootPackageScript]: rootPackageScriptCommand } })
        : slash(candidate) === slash(benchPackageJsonPath)
          ? JSON.stringify({ scripts: {} })
          : undefined,
  });
  assert.equal(missingPackageScript.availability.package_script_available, false);
  assert.ok(missingPackageScript.blockers.includes('dx_build_package_script_missing'));

  const wrongRootPackageScript = buildDxLighthousePackageContract({
    repoRoot,
    fileExists: candidate => existingFiles.has(slash(candidate)),
    directoryExists: candidate => slash(candidate) === slash(repoRoot),
    readText: candidate =>
      slash(candidate) === slash(rootPackageJsonPath)
        ? JSON.stringify({ scripts: { [rootPackageScript]: 'node ./scripts/other-contract.mjs' } })
        : slash(candidate) === slash(benchPackageJsonPath)
          ? JSON.stringify({ scripts: { [packageScript]: packageScriptCommand } })
          : undefined,
  });
  assert.equal(wrongRootPackageScript.availability.root_package_script_available, false);
  assert.ok(wrongRootPackageScript.blockers.includes('dx_build_root_package_script_missing'));
});

test('parses exactly the metadata JSON contract flags', () => {
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--contract', '--json']), {
    ok: true,
    mode: 'contract_json',
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--json', '--contract']), {
    ok: true,
    mode: 'contract_json',
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs([]), {
    ok: false,
    message: `DX Build Lighthouse packaging is not ready. Inspect \`${contractUsage}\` for metadata.`,
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--contract']), {
    ok: false,
    message: `${contractUsage} requires --json.`,
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--json']), {
    ok: false,
    message: `${contractUsage} requires exactly one --contract flag.`,
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--contract', '--json', '--score']), {
    ok: false,
    message: `Unsupported DX Build Lighthouse package contract argument \`--score\`; expected \`${contractUsage}\`.`,
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--contract', '--contract', '--json']), {
    ok: false,
    message: `${contractUsage} requires exactly one --contract flag.`,
  });
  assert.deepEqual(parseDxLighthousePackageContractArgs(['--contract', '--json', '--json']), {
    ok: false,
    message: `${contractUsage} requires exactly one --json flag.`,
  });
});

test('contract CLI prints JSON only for the explicit metadata command', () => {
  const result = childProcess.spawnSync(process.execPath, [scriptPath, '--contract', '--json'], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  });

  assert.equal(result.status, 0);
  assert.equal(result.stderr, '');
  assert.doesNotThrow(() => JSON.parse(result.stdout));
  const contract = JSON.parse(result.stdout);
  assert.equal(contract.package_script, packageScript);
  assert.equal(contract.status, 'not_ready');
  assert.equal(contract.proof_generation.emits_verified_proof, false);
});

test('contract CLI rejects unsupported execution modes without stdout JSON', () => {
  const result = childProcess.spawnSync(process.execPath, [scriptPath, '--score'], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  });

  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /Unsupported DX Build Lighthouse package contract argument `--score`/);
});

test('contract CLI sanitizes unsupported arguments before writing stderr', () => {
  const result = childProcess.spawnSync(process.execPath, [scriptPath, '--score\u001b[31m& calc'], {
    cwd: repoRoot,
    encoding: 'utf8',
    windowsHide: true,
  });

  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.doesNotMatch(result.stderr, /\u001b/);
  assert.match(result.stderr, /--score\?\[31m& calc/);
});

test('contract implementation remains metadata-only and execution-free', () => {
  const source = fs.readFileSync(scriptPath, 'utf8');

  assert.doesNotMatch(source, /from\s+['"](?:node:)?child_process['"]/);
  assert.doesNotMatch(source, /\b(?:spawn|spawnSync|exec|execFile|fork)\s*\(/);
  assert.doesNotMatch(source, /\bshell\s*:\s*true\b/);
  assert.doesNotMatch(source, /from\s+['"](?:lighthouse|chrome-launcher|puppeteer|playwright)['"]/);
  assert.doesNotMatch(source, /import\s*\(\s*['"](?:lighthouse|chrome-launcher|puppeteer|playwright)['"]\s*\)/);
});

test('package manifests expose the receipt contract without adding Lighthouse as a dependency', () => {
  const rootPackageJson = JSON.parse(fs.readFileSync(rootPackageJsonPath, 'utf8'));
  const benchPackageJson = JSON.parse(fs.readFileSync(benchPackageJsonPath, 'utf8'));

  assert.equal(rootPackageJson.scripts?.[rootPackageScript], rootPackageScriptCommand);
  assert.equal(benchPackageJson.scripts?.[packageScript], packageScriptCommand);
  for (const manifest of [rootPackageJson, benchPackageJson]) {
    assert.equal(manifest.dependencies?.lighthouse, undefined);
    assert.equal(manifest.devDependencies?.lighthouse, undefined);
    assert.equal(manifest.optionalDependencies?.lighthouse, undefined);
    assert.equal(manifest.peerDependencies?.lighthouse, undefined);
  }
});

function manifestFor(candidate) {
  if (slash(candidate) === slash(rootPackageJsonPath)) {
    return JSON.stringify({ scripts: { [rootPackageScript]: rootPackageScriptCommand } });
  }
  if (slash(candidate) === slash(benchPackageJsonPath)) {
    return JSON.stringify({ scripts: { [packageScript]: packageScriptCommand } });
  }
  return undefined;
}

function slash(candidate) {
  return path.resolve(candidate).replaceAll(path.sep, '/');
}
