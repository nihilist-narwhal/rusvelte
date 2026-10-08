# svelte-rs

An experimental Rust port of Svelte 5 tooling, aimed at a faster `svelte-check`. JavaScript
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
| svelte-check-rs, CSS diagnostics | not yet |

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
- `--diagnostic-sources js,svelte`, `--compiler-warnings code:ignore|error,...`
- `--incremental`, `--watch`, `--preserveWatchOutput`

`--timings` prints where the time went.

## Analysis

`svelte_rs::analyze::compile_diagnostics(source, filename)` reproduces the `warnings` of
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
- **The checker**
  - `src/check/`: the overlay, tsconfig handling, tsgo, diagnostic mapping and filtering,
    output writers, watch mode.
