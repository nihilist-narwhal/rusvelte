// Oracle for svelte-check's "css" diagnostics source.
//
// Loads svelte-check's bundled CLI (which contains svelte-language-server and
// vscode-css-languageservice) without running the CLI, creates a
// `SvelteCheck` with `diagnosticSources: ['css']` and asks it for the
// diagnostics of every `.svelte` file under the given directories, one file at a
// time. That's the code path `svelte-check --diagnostic-sources css` uses
// (`SvelteCheck.getDiagnosticsForFile` -> `PluginHost.getDiagnostics` ->
// `CSSPlugin.getDiagnostics`).
//
// usage: node oracle/css_oracle.cjs <out.json> <dir-or-file>...
//   SVELTE_CHECK=/path/to/svelte-check/dist/src/index.js (default: private-app's 4.7.6)
//
// Output: { "<path relative to its input dir>": [ {range, severity, message, code, source}, ... ], ... }
// for every file (files without diagnostics map to []).

const fs = require('fs');
const path = require('path');
const { loadSvelteCheck } = require('./load_svelte_check.cjs');
const { pathToFileURL } = require('url');

function walk(p, out) {
    const st = fs.statSync(p);
    if (st.isFile()) {
        if (p.endsWith('.svelte')) out.push(p);
        return out;
    }
    for (const e of fs.readdirSync(p, { withFileTypes: true }).sort((a, b) =>
        a.name < b.name ? -1 : a.name > b.name ? 1 : 0
    )) {
        if (e.name === 'node_modules' || e.name.startsWith('.')) continue;
        const c = path.join(p, e.name);
        if (e.isDirectory()) walk(c, out);
        else if (e.isFile() && e.name.endsWith('.svelte')) out.push(c);
    }
    return out;
}

async function main() {
    const [outArg, ...inputs] = process.argv.slice(2);
    const outFile = outArg && path.resolve(outArg);
    if (!outFile || !inputs.length) {
        console.error('usage: node css_oracle.cjs <out.json> <dir-or-file>...');
        process.exit(2);
    }
    const { SvelteCheck } = loadSvelteCheck();
    const files = [];
    const roots = new Map();
    for (const i of inputs) {
        const root = path.resolve(i);
        const base = fs.statSync(root).isDirectory() ? root : path.dirname(root);
        for (const f of walk(root, [])) {
            files.push(f);
            roots.set(f, base);
        }
    }
    const workspace = path.resolve(inputs[0]);
    const sc = new SvelteCheck(fs.statSync(workspace).isDirectory() ? workspace : path.dirname(workspace), {
        diagnosticSources: ['css']
    });
    const result = {};
    const t0 = process.hrtime.bigint();
    for (const file of files) {
        const text = fs.readFileSync(file, 'utf-8');
        const uri = pathToFileURL(file).href;
        await sc.upsertDocument({ uri, text }, true);
        const { diagnostics } = await sc.getDiagnosticsForFile(uri);
        await sc.removeDocument(uri);
        result[path.relative(roots.get(file), file)] = diagnostics.map((d) => ({
            range: d.range,
            severity: d.severity,
            message: d.message,
            code: d.code,
            source: d.source
        }));
    }
    const ms = Number(process.hrtime.bigint() - t0) / 1e6;
    fs.writeFileSync(outFile, JSON.stringify(result, null, 1));
    const withDiags = Object.values(result).filter((d) => d.length).length;
    console.error(`${files.length} files, ${withDiags} with diagnostics, ${ms.toFixed(0)} ms`);
}

main().catch((e) => {
    console.error(e);
    process.exit(1);
});
