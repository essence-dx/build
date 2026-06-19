import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

export const packageScript = 'receipt:lighthouse-package-contract';
export const packageScriptCommand = 'node ./receipt/lighthouse-package-contract.ts';
export const rootPackageScript = 'dx:lighthouse:package-contract';
export const rootPackageScriptCommand = 'node ./packages/bench/receipt/lighthouse-package-contract.ts';
export const contractUsage = 'node packages/bench/receipt/lighthouse-package-contract.ts --contract --json';
export const sourceScript = 'packages/bench/receipt/lighthouse-package-contract.ts';
export const packageProofTableColumns = [
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
export const packageProofRequiredTrueFields = [
  'package_assets_filesystem_addressable',
  'dynamic_imports_runtime_compatible',
  'node_builtins_runtime_compatible',
  'chrome_launcher_unstubbed',
];

const dirname = path.dirname(url.fileURLToPath(import.meta.url));
const defaultRepoRoot = path.resolve(dirname, '..', '..', '..');

export function buildDxLighthousePackageContract(options = {}) {
  const repoRoot = path.resolve(options.repoRoot ?? defaultRepoRoot);
  const fileExists = options.fileExists ?? isFile;
  const directoryExists = options.directoryExists ?? isDirectory;
  const readText = options.readText ?? readTextFile;
  const rootPackageJson = path.join(repoRoot, 'package.json');
  const benchPackageJson = path.join(repoRoot, 'packages', 'bench', 'package.json');
  const contractScript = path.join(repoRoot, ...sourceScript.split('/'));
  const buildCli = path.join(repoRoot, 'packages', 'rolldown', 'bin', 'cli.mjs');
  const lighthousePackage = path.join(repoRoot, 'node_modules', 'lighthouse', 'package.json');
  const packageProofReceipt = path.join(repoRoot, '.dx', 'check', 'web-lighthouse-equivalence.sr');
  const packageProofMachineReceipt = path.join(
    repoRoot,
    '.dx',
    'serializer',
    'check-web-lighthouse-equivalence.machine',
  );
  const packagePresent = fileExists(lighthousePackage);
  const packageScriptAvailable =
    fileExists(contractScript) &&
    packageJsonDeclaresScript(benchPackageJson, packageScript, packageScriptCommand, readText);
  const rootPackageScriptAvailable = packageJsonDeclaresScript(
    rootPackageJson,
    rootPackageScript,
    rootPackageScriptCommand,
    readText,
  );
  const buildCliPresent = fileExists(buildCli);
  const packageProofReceiptAvailable = fileExists(packageProofReceipt);
  const machineReceiptAvailable = fileExists(packageProofMachineReceipt);

  return {
    schema_name: 'dx.build.lighthouse_package_contract',
    schema_revision: 1,
    command: 'dx build lighthouse --contract --json',
    package_script: packageScript,
    package_script_command: packageScriptCommand,
    package_script_usage: contractUsage,
    root_package_script: rootPackageScript,
    root_package_script_command: rootPackageScriptCommand,
    source_script: sourceScript,
    status: 'not_ready',
    summary:
      'DX Build does not emit verified Google Lighthouse package proof until official Lighthouse packaging, Chrome launcher behavior, runtime compatibility, and Check equivalence receipts are proven.',
    build_engine: workspaceStatus('dx-build', repoRoot, directoryExists, fileExists, [
      rootPackageJson,
      benchPackageJson,
      contractScript,
      buildCli,
    ]),
    lighthouse_package: {
      package_name: 'lighthouse',
      package_present: packagePresent,
      node_engine_required: true,
      chrome_headless_required: true,
      assets_must_remain_filesystem_addressable: true,
      dynamic_imports_must_remain_runtime_compatible: true,
      node_builtins_must_remain_runtime_compatible: true,
      chrome_launcher_must_not_be_stubbed: true,
    },
    check_package_proof_gate: {
      equivalence_receipt_schema: 'dx.check.web_lighthouse_equivalence.v1',
      source_receipt: '.dx/check/web-lighthouse-equivalence.sr',
      machine_receipt: '.dx/serializer/check-web-lighthouse-equivalence.machine',
      package_proof_table: 'web_lighthouse_package_proofs',
      package_proof_table_columns: packageProofTableColumns,
      required_provider: 'dx-build',
      required_package_name: 'lighthouse',
      required_package_status: 'verified',
      required_hash_algorithm: 'blake3',
      required_true_fields: packageProofRequiredTrueFields,
    },
    availability: {
      available: false,
      package_present: packagePresent,
      package_script_available: packageScriptAvailable,
      root_package_script_available: rootPackageScriptAvailable,
      build_cli_present: buildCliPresent,
      package_proof_receipt_available: packageProofReceiptAvailable,
      machine_receipt_available: machineReceiptAvailable,
    },
    proof_generation: {
      emits_verified_proof: false,
      writes_receipts: false,
      executes_lighthouse: false,
      launches_chrome: false,
    },
    output_contract: {
      stdout: 'json',
      stderr: 'errors_only',
      verified_rows_emitted: false,
    },
    next_actions: [
      'Install or vendor the official lighthouse package under a governed DX Build package receipt.',
      'Prove package assets remain filesystem-addressable after packaging.',
      'Prove dynamic imports and Node built-ins run under the DX JS runtime contract.',
      'Prove Chrome launcher behavior is real and not stubbed.',
      'Generate Check source and serializer receipts only after package proof and score equivalence are verified.',
    ],
    blockers: lighthousePackageBlockers({
      packagePresent,
      packageScriptAvailable,
      rootPackageScriptAvailable,
      buildCliPresent,
      packageProofReceiptAvailable,
      machineReceiptAvailable,
    }),
    redaction: {
      metadata_only: true,
      stores_lhr_json: false,
      stores_traces: false,
      stores_screenshots: false,
      launches_chrome: false,
    },
  };
}

export function parseDxLighthousePackageContractArgs(args) {
  if (args.length === 0) {
    return {
      ok: false,
      message: `DX Build Lighthouse packaging is not ready. Inspect \`${contractUsage}\` for metadata.`,
    };
  }

  const unsupported = args.find(arg => arg !== '--contract' && arg !== '--json');
  if (unsupported !== undefined) {
    return {
      ok: false,
      message: `Unsupported DX Build Lighthouse package contract argument \`${sanitizeCliArgument(unsupported)}\`; expected \`${contractUsage}\`.`,
    };
  }

  const contractFlags = args.filter(arg => arg === '--contract').length;
  if (contractFlags !== 1) {
    return {
      ok: false,
      message: `${contractUsage} requires exactly one --contract flag.`,
    };
  }

  const jsonFlags = args.filter(arg => arg === '--json').length;
  if (jsonFlags === 0) {
    return {
      ok: false,
      message: `${contractUsage} requires --json.`,
    };
  }
  if (jsonFlags !== 1) {
    return {
      ok: false,
      message: `${contractUsage} requires exactly one --json flag.`,
    };
  }

  return { ok: true, mode: 'contract_json' };
}

function lighthousePackageBlockers({
  packagePresent,
  packageScriptAvailable,
  rootPackageScriptAvailable,
  buildCliPresent,
  packageProofReceiptAvailable,
  machineReceiptAvailable,
}) {
  const blockers = [];
  if (!packageScriptAvailable) {
    blockers.push('dx_build_package_script_missing');
  }
  if (!rootPackageScriptAvailable) {
    blockers.push('dx_build_root_package_script_missing');
  }
  if (!buildCliPresent) {
    blockers.push('dx_build_cli_missing');
  }
  if (!packagePresent) {
    blockers.push('official_lighthouse_package_not_installed');
  }
  if (!packageProofReceiptAvailable || !machineReceiptAvailable) {
    blockers.push('dx_build_lighthouse_package_proof_not_generated');
  }
  return blockers;
}

function workspaceStatus(id, root, directoryExists, fileExists, requiredPaths) {
  return {
    id,
    path: root,
    present: directoryExists(root),
    required_files: requiredPaths.map(requiredPath => ({
      path: normalizeRelativePath(root, requiredPath),
      present: fileExists(requiredPath),
    })),
  };
}

function packageJsonDeclaresScript(packageJson, scriptName, expectedCommand, readText) {
  const text = readText(packageJson);
  if (text === undefined) {
    return false;
  }

  try {
    const parsed = JSON.parse(text);
    return parsed?.scripts?.[scriptName] === expectedCommand;
  } catch {
    return false;
  }
}

function normalizeRelativePath(root, candidate) {
  return path.relative(root, candidate).split(path.sep).join('/');
}

function isFile(candidate) {
  try {
    return fs.statSync(candidate).isFile();
  } catch {
    return false;
  }
}

function isDirectory(candidate) {
  try {
    return fs.statSync(candidate).isDirectory();
  } catch {
    return false;
  }
}

function readTextFile(candidate) {
  try {
    return fs.readFileSync(candidate, 'utf8');
  } catch {
    return undefined;
  }
}

function sanitizeCliArgument(arg) {
  return arg
    .replaceAll(/[^\x20-\x7e]/g, '?')
    .slice(0, 120);
}

function runCli(args) {
  const parsed = parseDxLighthousePackageContractArgs(args);
  if (!parsed.ok) {
    process.stderr.write(`${parsed.message}\n`);
    process.exitCode = 1;
    return;
  }

  process.stdout.write(`${JSON.stringify(buildDxLighthousePackageContract(), null, 2)}\n`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === url.fileURLToPath(import.meta.url)) {
  runCli(process.argv.slice(2));
}
