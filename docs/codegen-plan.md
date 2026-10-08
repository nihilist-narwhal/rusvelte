# Plan: a Rust Svelte compiler (phase 3, code generation)

Goal: `compile(source, options)` producing the same `js.code` and `css.code` as Svelte 5.57.2
(`svelte-upstream` at 707c281), with identical output, verified by diffing against the JS compiler
over large corpora, as every earlier layer was.

What exists: parser, legacy AST, analysis diagnostics (`src/analyze/`), CSS pruning,
magic-string. What doesn't: the output side, i.e. `phases/3-transform` (about 12,700 lines of
JS in 109 files), the JS printer it uses (esrap 2.4.0's `ts` language, about 3,000 lines),
`utils/builders.js` (700 lines), and the analysis results code generation reads but
diagnostics don't (binding kinds, reassignment/mutation, expression metadata, element
metadata).

## Milestones

Each one ends in a compare binary that reports exact-match counts, like `compare_compile`.

### A. Foundations

- **A1. Oracle.** `oracle/gen_codegen.mjs <corpus> <out> [client|server]`: one JSON per
  component with `js.code` and `css.code` from `compile()`. The test samples use their
  `_config.js` `compileOptions`; real-world corpora use defaults. Corpora: `runtime-runes`
  (1,060), `runtime-legacy` (1,209), `server-side-rendering` (132), `css` (184), `hydration`
  (85), `snapshot` (34, which also has checked-in expected output), the private app, Windmill.
  `compare_codegen` diffs and groups by the first differing line.
- **A2. Output AST.** An owned ESTree (`src/estree/`): every node Svelte emits or copies from
  the input, with spans for source maps and leading/trailing comments. Conversion from oxc
  (TypeScript already stripped, like `remove_typescript_nodes`). Port of `builders.js`.
- **A3. Printer.** Port of esrap's printer for that AST, including comment placement.
  Verified on its own: oxc parse, convert, print, against esrap printing acorn's AST, over
  thousands of JS files (the snapshot expected outputs, `node_modules`, Svelte's own
  sources).
- **A4. CSS output.** Port of `css/index.js` (`render_stylesheet`): scoping classes,
  `:global` removal, pruned selectors commented out, keyframes. Compare `css.code` over the
  `css` samples and the real-world corpora.

### B. Analysis for code generation

- **B1.** Extend `src/analyze` to produce what the transform reads: `ComponentAnalysis`
  fields (`needs_props`, `uses_props`, `exports`, `css.hash`, `instance`/`module` scopes,
  `reactive_statements` in order, ...), binding metadata (`kind`, `reassigned`, `mutated`,
  `updated`, `initial`, `metadata.inside_rest`), expression metadata (`has_state`,
  `has_call`, `has_await`, `dependencies`, `references`), node metadata (element
  `has_spread`, `scoped`, `svelte_element` namespaces, each-block `keyed`/`contains_group_binding`,
  snippets, `{@const}`). Do it field by field as the transform needs them, keeping the
  diagnostics parity (`compare_compile`) green.

### C. Server transform (first, it's smaller and string-based)

- **C1.** `transform-server.js`, `server/visitors/*`, `shared/*`. Compare
  `generate: 'server'` over `server-side-rendering`, `runtime-*` and real-world corpora.

### D. Client transform

- **D1.** `transform-client.js`, `transform-template/*` (the template string and DOM
  traversal), `client/visitors/*`. Compare `generate: 'client'`.
- **D2.** `dev: true`, `hmr`, `customElement`, `accessors`, `immutable`, legacy mode
  options, `discloseVersion`, `fragments: 'tree'`, `experimental.async`.

### E. Modules, source maps, integration

- **E1.** `compile_module` (`.svelte.js`/`.svelte.ts`).
- **E2.** Source maps (esrap's mappings, merged with the preprocessor map).
- **E3.** Behavioural check: run Svelte's own runtime test suite with our output plugged in
  (a Node binding or a CLI the test harness calls). That checks behaviour, not text.
- **E4.** A Vite plugin path (`vite-plugin-svelte` calling the native compiler).

## Order of work

1. A1 oracle and harness, A2/A3 printer, A4 CSS in parallel (independent).
2. B1 alongside C1: grow the analysis as the server transform needs it.
3. D1, then D2, then E.

## Conventions

- Port structure: one Rust function per JS function, same names, `// svelte: path:line`
  where helpful. Match key-order-dependent behaviour (visit order, object key order) exactly.
- Every milestone lands with its compare binary and counts in the README status table.
- Parallel work happens in git worktrees under `.claude/worktrees/` on separate branches,
  merged into master when its compare numbers are in.
