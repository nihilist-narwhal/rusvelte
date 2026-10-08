# rusvelte

An experimental Rust port of the Svelte 5 compiler and tooling, including a faster `svelte-check`. Not affiliated with the Svelte team; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the projects it ports. The project page lives in `site/` and deploys to GitHub Pages. JavaScript
inside components is parsed with [oxc](https://oxc.rs). Every layer is checked against the JS
original by diffing outputs over large corpora.

- **Parser**: `parse(source, { modern: true })` and the legacy AST from `svelte/compiler`
  5.57.2.
- **svelte2tsx**: the component-to-TypeScript transform from `svelte2tsx` 0.7.61, which
  svelte-check type-checks.
- **svelte-check-rs**: a native `svelte-check --tsgo`. It converts every component in
  parallel, writes svelte-check's overlay, runs TypeScript 7 (tsgo) and maps the
  diagnostics back.

## Status

| Part | Parity with the JS original |
|---|---|
| Parser | 4,567/4,567 Svelte test files (3 differ only in a JS error's position), the private app 171/171 |
| Legacy AST | 4,567/4,567, 171/171 |
| Analysis (compiler warnings and errors, `compile` with `generate: false`) | Svelte tests 4,564/4,567 (the rest word a JS syntax error differently), the private app 171/171, language-tools 787/803 |
| svelte2tsx | Svelte tests 4,566/4,567, svelte2tsx's samples 247/250 (`dts` mode skipped), the private app 171/171, Windmill 1,982/1,982 |
| SvelteKit route/hook/param files (`upsertKitFile`) | all real-world files; 118/121 synthetic samples |
| svelte-check-rs, TypeScript diagnostics | identical to `svelte-check --tsgo` 4.7.6 on the private app (2,477 diagnostics under strict options) and Windmill (7,922) |
| svelte-check-rs, compiler warnings | identical to svelte-check on the private app and Windmill (89 warnings) |
| svelte-check-rs, CSS diagnostics (`css`/`scss`/`less`) | identical to svelte-check on Windmill (96 warnings in all); the CSS linter matches svelte-check's on 4,567/4,567 Svelte tests, Windmill, the private app and 47,072 synthetic files |
| CSS output (`css.code`, `css.hasGlobal`, injected styles) | byte-identical on every component of the code generation oracle (Svelte test suites 6,929 compilations, the private app 342, Windmill 3,964), plus `dev`/`css: 'injected'` variants and the CSS linter's 31,418 synthetic files (1 differs: oxc rejects a mangled TypeScript expression acorn accepts) |
| Client code generation (`compile`, `generate: 'client'`, `js.code`) | byte-identical on every client compilation of the code generation oracle: Svelte test suites (runtime-runes 1,408, runtime-legacy 1,657, hydration 98, css 187/188: a `cssHash` function the harness can't call, snapshot 33), the private app 171, Windmill 1,982, bits-ui 226, immich 411, language-tools 668; also with `dev`, `hmr`, `fragments: 'tree'`, `experimental.async`, `customElement`, `css: 'injected'`, `compatibility.componentApi: 4` variants, and the runes/legacy/TypeScript/custom-element variants of the test suites and Windmill |
| Module code generation (`compileModule`, client and server, `js.code`) | byte-identical on every module of the code generation oracle, both modes: bits-ui 58, immich 45 (7 more are `.svelte.ts` files type stripping can't handle), Windmill 116, runtime-runes 20, runtime-legacy 1, snapshot 3, language-tools 8, the private app 1, also with `dev` and the other variants; the error code matches on the 34 modules of Svelte's compiler-errors and validator samples that fail |
| JS printer (esrap 2.4.0 `ts({ comments })`) | byte-identical code and source map mappings on 16,470 JS files: Svelte's snapshot expectations 74/74, Svelte's sources 368/368, `node_modules` 4,747/4,747 (oracle and a Windmill sample, minified files included), and the compiler's output for every component of the code generation oracle re-parsed (Svelte test suites 6,975, the private app 342, Windmill 3,964) |

The remaining svelte2tsx mismatches are 2 scripts that oxc can't parse but TypeScript
recovers from, and 2 samples where npm 0.7.61 throws but the current language-tools source
(followed here) doesn't.

### Speed

| | JS | Rust |
|---|---|---|
| Parse the Svelte test corpus | 157 ms | 19 ms |
| svelte2tsx, Windmill (1,982 files, warm) | 3,425 ms | 539 ms, 91 ms on 12 threads |
| Full check, the private app (`svelte-check` vs `svelte-check-rs`) | 10.6 s | 1.7 s |
| Full check, Windmill | 73.5 s (needs an 8 GB heap) | 9.3 s |
| Incremental re-run, Windmill, no changes | 2.4 s (svelte-fast-check) | 2.0 s |
| Print Windmill's compiled JS (3,964 files, 34 MB) with esrap / `estree::print` | 4,268 ms | 600 ms (360 ms without source maps) |

On Windmill the type-check itself (tsgo, about 8 s) is now most of the time; the project's
PostCSS/Melt UI preprocessors run alongside it in Node.

### Differences from svelte-check 4.7 `--tsgo`

These are deliberate fixes:

- **Excludes covering the cache directory are dropped.** A tsconfig that excludes
  `.svelte-kit` made svelte-check exclude its own output, so no component got type-checked.
- **package.json subpath imports (`#lib/*`) get overlay `paths`** that try the generated
  files first. svelte-check only redirects relative imports and `paths`, so `#lib/X.svelte`
  became an untyped module.
- **The overlay keeps the project's `rootDir` default.** With `outDir` and no `rootDir`,
  TypeScript 6+ would make the overlay's own directory the root, report only TS6059 errors
  and skip type-checking.
- **Compiler errors are reported for components svelte2tsx can't convert.** svelte-check
  drops such files silently.

## svelte-check-rs

```sh
cargo build --release --bin svelte-check-rs
cd my-app && /path/to/svelte-check-rs --tsconfig ./tsconfig.json
```

The project needs TypeScript 7 (`@typescript/native`, an alias of `typescript@7`, or
`@typescript/native-preview`), as with `svelte-check --tsgo`. Compiler warnings come from the Rust analysis. Node is
only started when the project's config has preprocessors (which then run in Node, and the
analysis maps positions back through their source maps) or compiler options the analysis
doesn't support (then the project's `svelte/compiler` does the whole job).

Options follow svelte-check:
- `--workspace`, `--tsconfig`
- `--output human|human-verbose|machine|machine-verbose`
- `--threshold`, `--ignore`, `--fail-on-warnings`
- `--diagnostic-sources js,svelte,css`, `--compiler-warnings code:ignore|error,...`
- `--incremental`, `--watch`, `--preserveWatchOutput`

`--timings` prints where the time went.

`SVELTE_CHECK_TSGO=/path/to/tsgo` uses another TypeScript 7 binary, such as
[tsc-rs](https://github.com/pingdotgg/ts-rust), and `SVELTE_CHECK_TSGO_ARGS` passes it extra
flags (`--singleThreaded` makes results independent of how files are split between checkers).

## CSS output

`rusvelte::compile_css(source, filename, &CssOptions)` returns `result.css` of `compile`
(`code` and `has_global`), and `transform::compile_styles` also the stylesheet injected into
the JS with `css: 'injected'` or custom elements. It is a port of `render_stylesheet`
(`src/transform/css.rs`), with the `cssHash`, `css`, `dev`, `customElement` and `rootDir`
options. Source maps aren't produced yet.

## Analysis

`rusvelte::analyze::compile_diagnostics(source, filename)` reproduces the `warnings` of
`compile(source, { dev: true, generate: false, filename })`, or the error it throws. That
covers scopes, runes and legacy analysis, a11y, CSS pruning (`css_unused_selector`) and
svelte-ignore. `compile_diagnostics_with` takes the `runes`, `customElement` and
`experimental.async` options. It takes about 51 ms for Svelte's test corpus against 361 ms
for `compile` on Node.

`oracle/gen_variants.mjs` makes runes, legacy, TypeScript and custom-element variants of a
corpus, to push the analysis down other paths.

## Setup for the parity checks

```sh
git clone https://github.com/sveltejs/svelte.git svelte-upstream
git -C svelte-upstream checkout 707c28146b0f0a6d5404a1bd4769874c3c24851a
git clone https://github.com/sveltejs/language-tools.git language-tools-upstream
cd oracle && npm install && cd ..
```

Each oracle script runs the JS original over a corpus and writes one JSON file per
component. The matching binary compares and groups failures, and `VERBOSE=1` prints the
first difference.

| Oracle | Compare binary |
|---|---|
| `oracle/gen.mjs` | `compare` (parser, legacy AST) |
| `oracle/gen_compile.mjs` | `compare_compile` (analysis) |
| `oracle/gen_htmlx2jsx.mjs` | `compare_s2t` (template half) |
| `oracle/gen_svelte2tsx.mjs <corpus> <out> [check\|samples\|svelte-check]` | `compare_s2t` (full svelte2tsx) |
| `oracle/gen_kit.mjs` | `compare_kit` (SvelteKit files) |
| `oracle/css_oracle.cjs` (and `gen_css_samples.mjs` for synthetic corpora) | `compare_css` (CSS diagnostics) |
| `oracle/gen_css_output.mjs <corpus> <out> [base options]`, or `gen_codegen.mjs` | `compare_css_output` (CSS output) |
| `oracle/gen_esrap.mjs [--keep-minified] <dir\|@list> <out.json> [base]` | `compare_esrap` (JS printer; `BENCH=n` times it) |
| `oracle/gen_builders.mjs` | expected output for the builder tests in `src/estree/tests.rs` |

`tools/check_sanity.py <language-tools> <node_modules>` runs svelte-check's own sanity
fixtures against svelte-check-rs.

## Layout

- **Parser**
  - `src/parser/`: port of `phases/1-parse`.
  - `src/js.rs`: oxc integration and acorn-compatible JSON (`ToJson`).
  - `src/ast.rs`, `src/css.rs`: the typed template and CSS ASTs.
  - `src/legacy.rs`: the Svelte 4-shaped AST that svelte2tsx consumes.
  - `src/errors.rs`, `src/warning_codes.rs`: generated from Svelte's messages.
- **Analysis**
  - `src/analyze/`: port of `phases/2-analyze` and `phases/scope.js`. `nodes.rs` views the
    template AST and oxc's AST as the ESTree/Svelte tree the JS walks.
  - `warnings.rs` and `src/errors.rs` are generated by `tools/gen_messages.mjs`, and
    `a11y_data.rs` by `tools/gen_a11y.mjs`.
  - `acorn.rs` adds the parse errors acorn raises that oxc's parser doesn't.
- **svelte2tsx**
  - `src/magic_string.rs`: port of magic-string.
  - `src/svelte2tsx/template.rs`, `elements.rs`, `slots.rs`, `periscope.rs`: the template
    half (`htmlxtojsx_v2`).
  - `src/svelte2tsx/script/`: the script half, which walks oxc's AST the way svelte2tsx
    walks TypeScript's.
  - `src/svelte2tsx/kit.rs`, `rewrite_imports.rs`: SvelteKit files and external imports.
- **Code generation**
  - `src/estree/`: the owned ESTree AST the transform builds (`Node`/`NodeKind`), with
    `convert.rs` (oxc → acorn-shaped ESTree, TypeScript removed like
    `remove_typescript_nodes`), `builders.rs` (port of `utils/builders.js`) and `print.rs`
    (port of esrap 2.4.0 with its `ts` language, including comments and source map mappings).
  - `src/transform/`: port of `phases/3-transform`: `css/index.js` (`render_stylesheet`), the
    client transform (`client/`: `transform-client.js`, `transform-template/*`, `visitors/*`).
- **CSS diagnostics**
  - `src/css_lint/`: port of vscode-css-languageservice's CSS/SCSS/LESS parsers and lint
    rules, and of svelte-language-server's `<style>` extraction. `tools/gen_css_data.mjs`
    generates its property and at-rule data.
- **The checker**
  - `src/check/`: the overlay, tsconfig handling, tsgo, diagnostic mapping and filtering,
    output writers, watch mode.

## License

MIT, see [LICENSE](LICENSE). The ported code keeps its original notices in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
