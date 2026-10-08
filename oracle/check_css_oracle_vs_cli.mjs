// Cross-checks oracle/css_oracle.cjs against the real svelte-check CLI: runs
// `svelte-check --no-tsconfig --diagnostic-sources css --output machine-verbose` on a directory
// and compares the reported diagnostics (per file, in order) with the oracle's JSON.
//
// usage: node oracle/check_css_oracle_vs_cli.mjs <dir> <oracle.json> [svelte-check bin]
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const [dir, oracleFile, bin = '/path/to/rusvelte/experiments/windmill/frontend/node_modules/svelte-check/bin/svelte-check'] =
    process.argv.slice(2);
const out = spawnSync(
    'node',
    [bin, '--workspace', path.resolve(dir), '--no-tsconfig', '--diagnostic-sources', 'css', '--output', 'machine-verbose'],
    { encoding: 'utf8', maxBuffer: 1 << 30 }
).stdout;
const cli = {};
for (const line of out.split('\n')) {
    const m = line.match(/^\d+ (\{.*\})$/);
    if (!m) continue;
    const d = JSON.parse(m[1]);
    (cli[d.filename] ??= []).push({
        range: { start: d.start, end: d.end },
        severity: d.type === 'ERROR' ? 1 : 2,
        message: d.message,
        code: d.code,
        source: d.source
    });
}
const oracle = JSON.parse(fs.readFileSync(oracleFile, 'utf8'));
let same = 0,
    diff = 0;
for (const [file, diags] of Object.entries(oracle)) {
    const a = JSON.stringify(diags);
    const b = JSON.stringify(cli[file] ?? []);
    if (a === b) same++;
    else {
        diff++;
        if (diff <= 5) console.log('DIFF', file, '\n oracle:', a.slice(0, 400), '\n cli:   ', b.slice(0, 400));
    }
}
const extra = Object.keys(cli).filter((f) => !(f in oracle));
console.log(`${same} same, ${diff} different, ${extra.length} files only in CLI output`);
