// Loads svelte-check's bundled CLI (svelte-language-server + vscode-css-languageservice +
// vscode-html-languageservice) as a module without running the CLI, and exposes a few
// internals. Requires are resolved relative to the real bundle.

const fs = require('fs');
const path = require('path');
const Module = require('module');

const DEFAULT_BUNDLE =
    '/path/to/rusvelte/experiments/private-app/node_modules/svelte-check/dist/src/index.js.orig';

function loadSvelteCheck(bundlePath = process.env.SVELTE_CHECK || DEFAULT_BUNDLE) {
    let src = fs.readFileSync(bundlePath, 'utf8');
    const cli = 'prog.parse(process.argv, {';
    if (!src.includes(cli)) throw new Error('unexpected bundle: no CLI entry');
    src = src.replace(cli, 'if (false) prog.parse(process.argv, {');
    src += `
module.exports.__SvelteCheck = svelteCheck.SvelteCheck;
module.exports.__cssLanguageService = cssLanguageService;
module.exports.__getDefaultHTMLDataProvider = getDefaultHTMLDataProvider;
module.exports.__svelteSelectors = svelteSelectors;
module.exports.__svelteCustomDataProvider = customDataProvider;
`;
    const filename = bundlePath.replace(/\.orig$/, '');
    const m = new Module(filename, module);
    m.filename = filename;
    m.paths = Module._nodeModulePaths(path.dirname(filename));
    m._compile(src, filename);
    return {
        SvelteCheck: m.exports.__SvelteCheck,
        cssLanguageService: m.exports.__cssLanguageService,
        getDefaultHTMLDataProvider: m.exports.__getDefaultHTMLDataProvider,
        svelteSelectors: m.exports.__svelteSelectors,
        svelteCustomDataProvider: m.exports.__svelteCustomDataProvider
    };
}

module.exports = { loadSvelteCheck, DEFAULT_BUNDLE };
