// Generates synthetic .svelte corpora for the css_lint parity check (then run
// oracle/css_oracle.cjs on each and compare with `compare_css`).
//
// usage: [SEED=n] node oracle/gen_css_samples.mjs <out root> [which...]
//   nm-css / nm-scss / nm-less: stylesheets from installed node_modules wrapped in <style>
//   mut-css / mut-scss / mut-less: random mutations of real style contents (parser recovery)
//   html: mutations of the markup around <style> tags (tag extraction)
//   hand: hand-written cases for each lint rule and parser feature
import fs from 'node:fs';
import path from 'node:path';

const [outRoot, ...which] = process.argv.slice(2);
if (!outRoot) {
    console.error('usage: node gen_css_samples.mjs <out root> [which...]');
    process.exit(2);
}
const want = (k) => which.length === 0 || which.includes(k);

const NODE_MODULES = [
    '/path/to/rusvelte/experiments/windmill/frontend/node_modules',
    '/path/to/rusvelte/experiments/private-app/node_modules',
    '/path/to/rusvelte/oracle/node_modules'
];
const SVELTE_DIRS = [
    '/path/to/rusvelte/svelte-upstream/packages/svelte/tests',
    '/path/to/rusvelte/experiments/windmill/frontend/src',
    '/path/to/rusvelte/experiments/private-app/src',
    '/path/to/rusvelte/language-tools-upstream/packages'
];

// deterministic PRNG (mulberry32)
const SEED = Number(process.env.SEED || 0);
function rng(seed) {
    seed += SEED * 1000;
    let a = seed >>> 0;
    return () => {
        a = (a + 0x6d2b79f5) >>> 0;
        let t = a;
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
}

function walk(dir, exts, out = [], skipNodeModules = true) {
    let ents;
    try {
        ents = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
        return out;
    }
    ents.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    for (const e of ents) {
        if (e.isSymbolicLink()) continue;
        if (skipNodeModules && e.name === 'node_modules') continue;
        const p = path.join(dir, e.name);
        if (e.isDirectory()) walk(p, exts, out, skipNodeModules);
        else if (exts.includes(path.extname(e.name))) out.push(p);
    }
    return out;
}

function writeCorpus(name, files) {
    const dir = path.join(outRoot, 'syn-' + name);
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    const manifest = {};
    files.forEach(({ text, from }, i) => {
        const f = String(i).padStart(5, '0') + '.svelte';
        fs.writeFileSync(path.join(dir, f), text);
        manifest[f] = from;
    });
    fs.writeFileSync(path.join(outRoot, `syn-${name}.manifest.json`), JSON.stringify(manifest, null, 1));
    console.error(`syn-${name}: ${files.length} files`);
}

const nmSheets = (ext) => {
    const seen = new Set();
    const out = [];
    for (const d of NODE_MODULES) {
        for (const f of walk(d, [ext], [], false)) {
            const text = fs.readFileSync(f, 'utf8');
            if (text.length > 300_000 || seen.has(text)) continue;
            seen.add(text);
            out.push({ text, from: f });
        }
    }
    return out;
};

const wrap = (css, lang) => `<script>\n\tlet x = 1;\n</script>\n\n<div>{x}</div>\n\n<style${lang ? ` lang="${lang}"` : ''}>\n${css}\n</style>\n`;

if (want('nm-css')) writeCorpus('nm-css', nmSheets('.css').map((s) => ({ text: wrap(s.text, ''), from: s.from })));
if (want('nm-scss')) writeCorpus('nm-scss', nmSheets('.scss').map((s) => ({ text: wrap(s.text, 'scss'), from: s.from })));
if (want('nm-less')) {
    const r = rng(7);
    const sheets = [...nmSheets('.css').filter(() => r() < 0.3), ...nmSheets('.scss')];
    writeCorpus('nm-less', sheets.map((s) => ({ text: wrap(s.text, 'less'), from: s.from })));
}

// --- style contents of real components ---
function styleContents() {
    const out = [];
    for (const d of SVELTE_DIRS) {
        for (const f of walk(d, ['.svelte'])) {
            const t = fs.readFileSync(f, 'utf8');
            for (const m of t.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)) {
                if (m[1].trim()) out.push({ css: m[1], from: f });
            }
        }
    }
    return out;
}

const INSERTS = [
    '{', '}', '(', ')', ';', ':', '::', '[', ']', '"', "'", '@', '#', '\\', '/*', '*/', '!', '!important', ',', '-', '--',
    'url(', 'url( x', '@media', '@media (', '@supports', '@import', '@keyframes', '@-webkit-keyframes x {', '@font-face {',
    '@layer', '@container', '@scope', '@page', '@property --x', '@apply', '@include', '@mixin', '@if', '@else', '@each',
    '@for', '@function', '@return', '@use', '@forward', '@extend', '@at-root', '@content', '@debug', '@plugin',
    '&', '$', '$x', '#{', '`', '~', '%', '>', '<', '<=', '>=', '==', '!=', '...', '\n', ' ', '\t', 'u+', 'U+0025-00FF',
    '\\31 ', '\\\n', '"\\\n', 'calc(', 'var(--', 'rgb(', 'hsl(1,2', '#abc', '#abcd5', '#12', '0px', '1e3', '.5', '+.', 'when',
    ' and ', ' not ', ' or ', 'only ', 'from', 'to', 'through', 'in', ':global(', ':is(', ':not(', ':nth-child(2n+1 of', '::before',
    '-webkit-', '-moz-', 'display: inline-block;', 'float: left;', 'vertical-align: top;', 'display:block;', 'foo: bar;',
    '*zoom: 1;', '_height: 1px;', '--x: {a:b};', '--y: ;', 'src: url(x);', 'font-family: x;', '<!--', '-->', '//', '.mixin();',
    '@var: 1;', '@{var}', '.a when (@x > 1)', '&:extend(.b);', '~"esc"', 'e(%("%d", 1))', '@r: { color: red };', '@r();',
    'progid:DX.Y(', 'U+?', 'é', '\u{1F600}', 'K', 'İ', '\u0000'
];

function mutate(css, r) {
    let s = css;
    const n = 1 + Math.floor(r() * 4);
    for (let k = 0; k < n; k++) {
        const pos = Math.floor(r() * (s.length + 1));
        const op = r();
        if (op < 0.35) {
            const len = 1 + Math.floor(r() * 6);
            s = s.slice(0, pos) + s.slice(pos + len);
        } else if (op < 0.8) {
            const ins = INSERTS[Math.floor(r() * INSERTS.length)];
            s = s.slice(0, pos) + ins + s.slice(pos);
        } else if (op < 0.9) {
            const len = Math.floor(r() * 40);
            const from = Math.floor(r() * (s.length + 1));
            s = s.slice(0, pos) + s.slice(from, from + len) + s.slice(pos);
        } else {
            s = s.slice(0, pos);
        }
    }
    return s;
}

if (want('mut-css') || want('mut-scss') || want('mut-less')) {
    const contents = styleContents();
    // plus small node_modules stylesheets
    const r0 = rng(1);
    for (const ext of ['.css', '.scss']) {
        for (const s of nmSheets(ext)) if (s.text.length < 4000 && r0() < 0.5) contents.push({ css: s.text, from: s.from });
    }
    for (const [lang, seed] of [
        ['css', 11],
        ['scss', 12],
        ['less', 13]
    ]) {
        if (!want('mut-' + lang)) continue;
        const r = rng(seed);
        const files = [];
        for (const { css, from } of contents) {
            const k = css.length > 2000 ? 1 : 3;
            for (let i = 0; i < k; i++) files.push({ text: wrap(mutate(css, r), lang === 'css' ? '' : lang), from });
        }
        writeCorpus('mut-' + lang, files);
    }
}

// heavier mutations (more edits per file) with other seeds
if (want('mut2-css') || want('mut2-scss') || want('mut2-less')) {
    const contents = styleContents();
    for (const [lang, seed] of [
        ['css', 31],
        ['scss', 32],
        ['less', 33]
    ]) {
        if (!want('mut2-' + lang)) continue;
        const r = rng(seed);
        const files = [];
        for (const { css, from } of contents) {
            if (css.length > 3000) continue;
            for (let i = 0; i < 3; i++) {
                let s = css;
                const rounds = 2 + Math.floor(r() * 4);
                for (let k = 0; k < rounds; k++) s = mutate(s, r);
                files.push({ text: wrap(s, lang === 'css' ? '' : lang), from });
            }
        }
        writeCorpus('mut2-' + lang, files);
    }
}

// random token soups
const SOUP_EXTRA = [
    'a', 'b', '.c', '#d', 'div', 'color', 'red', '1px', '2', '50%', 'x-y', '--v', 'var(--v)', '"s"', "'t'", ' ', ' ', '\n',
    '{', '}', '{', '}', ';', ';', ':', '(', ')', ',', 'and', 'not', '@media', '@if', '@include', '@mixin', '.m(', '@x:', '$y:', '&'
];
if (want('soup-css') || want('soup-scss') || want('soup-less')) {
    const pool = [...INSERTS, ...SOUP_EXTRA, ...SOUP_EXTRA];
    for (const [lang, seed] of [
        ['css', 41],
        ['scss', 42],
        ['less', 43]
    ]) {
        if (!want('soup-' + lang)) continue;
        const r = rng(seed);
        const files = [];
        for (let i = 0; i < 4000; i++) {
            const n = 1 + Math.floor(r() * 40);
            let s = '';
            for (let k = 0; k < n; k++) s += pool[Math.floor(r() * pool.length)];
            files.push({ text: wrap(s, lang === 'css' ? '' : lang), from: 'soup' });
        }
        writeCorpus('soup-' + lang, files);
    }
}

// --- markup around the style tag ---
const HTML_INSERTS = [
    '{#if a}', '{/if}', '{#each xs as x}', '{/each}', '{#await p}', '{/await}', '{@html x}', '{@html "<style>"}', '{', '}',
    '<!--', '-->', '<script>', '</script>', '<div>', '</div>', '<br>', '<style>', '</style>', '<STYLE>', '<style/>',
    '<style lang="scss">', '<style lang=less>', "<style type='text/scss'>", '<style lang>', '<style lang="">',
    '<style lang={x}>', '<style {...rest}>', '<style lang="postcss">', '<style lang="sass">', '<style lang="text/less">',
    '<style lang="stylus">', '<style type="text/css">', '<style global>', '<svelte:head><style>a{}</style></svelte:head>',
    '<style>a{ color: red }</style>', '<style>.x{}</style>', '<template>', '</template>', '<a href="{x}">', '<input value={x}>',
    "<p title='{'>", '<p {x}>', '"', "'", '<', '>', '/', '/>', ' ', '\n', '<!doctype html>', '<style\n>', '< style>', '</ style>',
    '<style a=b>', '<style a="{x}" b=\'}\'>', '<svelte:options />', '{x}', '{"}"}', '{`${a}`}', '{/* } */}', '<style // c\n>'
];

if (want('html')) {
    const r = rng(21);
    const files = [];
    for (const d of SVELTE_DIRS) {
        for (const f of walk(d, ['.svelte'])) {
            const t = fs.readFileSync(f, 'utf8');
            const has = t.includes('<style');
            if (!has && r() > 0.05) continue;
            const k = has ? 3 : 1;
            for (let i = 0; i < k; i++) {
                let s = t;
                const n = 1 + Math.floor(r() * 3);
                for (let j = 0; j < n; j++) {
                    // bias positions towards the style tag
                    const idx = s.indexOf('<style');
                    const pos =
                        idx >= 0 && r() < 0.6
                            ? Math.max(0, Math.min(s.length, idx + Math.floor((r() - 0.5) * 80)))
                            : Math.floor(r() * (s.length + 1));
                    if (r() < 0.3) s = s.slice(0, pos) + s.slice(pos + 1 + Math.floor(r() * 5));
                    else s = s.slice(0, pos) + HTML_INSERTS[Math.floor(r() * HTML_INSERTS.length)] + s.slice(pos);
                }
                if (!s.includes('<style')) s += '\n<style>\n.a { foo: bar; } .b {}\n</style>\n';
                files.push({ text: s, from: f });
            }
        }
    }
    writeCorpus('html', files);
}

// heavier markup mutations anywhere in the file
if (want('html2')) {
    const r = rng(22);
    const pool = [...HTML_INSERTS, ...HTML_INSERTS, ...INSERTS];
    const files = [];
    for (const d of SVELTE_DIRS) {
        for (const f of walk(d, ['.svelte'])) {
            const t = fs.readFileSync(f, 'utf8');
            if (!t.includes('<style') && r() > 0.1) continue;
            for (let i = 0; i < 2; i++) {
                let s = t;
                const n = 3 + Math.floor(r() * 6);
                for (let j = 0; j < n; j++) {
                    const pos = Math.floor(r() * (s.length + 1));
                    const op = r();
                    if (op < 0.3) s = s.slice(0, pos) + s.slice(pos + 1 + Math.floor(r() * 8));
                    else if (op < 0.9) s = s.slice(0, pos) + pool[Math.floor(r() * pool.length)] + s.slice(pos);
                    else {
                        const from = Math.floor(r() * s.length);
                        s = s.slice(0, pos) + s.slice(from, from + Math.floor(r() * 60)) + s.slice(pos);
                    }
                }
                if (!s.includes('<style')) s = '<style>.a { foo: bar; } .b {}</style>\n' + s;
                files.push({ text: s, from: f });
            }
        }
    }
    writeCorpus('html2', files);
}

// --- hand-written ---
const HAND_CSS = [
    // unknown properties / at-rules
    '.a { colr: red; color: red; -webkit-foo: 1; *zoom: 1; _height: 1px; *colr: 1; COLOR: red; Colr: x }',
    '@apply x; @tailwind base; @unknown { a { b: c } } @screen md { .a { color: red } } @custom-media --x (min-width: 1px);',
    '.a { @apply font-bold; } @media (min-width: 1px) { @apply x; }',
    '@MEDIA screen { .a { color: red } } @Media print {} @-moz-document url-prefix() { .a {} }',
    // empty rules
    '.a {} .b { } .c { /* c */ } .d, .e {} :global(.f) {} .g { .h {} }',
    // font-face
    '@font-face { src: url(x); } @font-face { font-family: x; src: url(y) } @font-face {} @font-face { $x: 1; }',
    '@font-face { font-family: x; --y: 1; } @font-face { FONT-FAMILY: x; SRC: url(a) }',
    // hex colors
    '.a { color: #abc; color: #abcd; color: #aabbcc; color: #aabbccdd; color: #ab; color: #abcde; color: #\\61 bcd; }',
    // display
    '.a { display: inline-block; float: left; } .b { display: inline-block; float: none } .c { display: block; vertical-align: top; } .d { display: flex block; vertical-align: x }',
    '.a { display: inline-block !important; float: right; vertical-align: top; display: block }',
    // vendor prefixes
    '.a { -webkit-transition: x; } .b { -webkit-transition: x; transition: x } .c { -moz-box-sizing: x; -webkit-box-sizing: y }',
    '.a::-webkit-scrollbar { -webkit-appearance: none } .b { -webkit-appearance: none } input::-moz-range-thumb { -moz-appearance: none }',
    '.a { -webkit-user-select: none; -ms-user-select: none; } .b { -webkit-mask: x; --v: 1 } .c { -webkit-Transition: x }',
    '@-webkit-keyframes a { from {} } @keyframes b { to {} } @-moz-keyframes b {} @-o-keyframes c { 50% { color: red } } @KEYFRAMES d {} @-webkit-keyframes d {}',
    '@-ms-keyframes x { } @-webkit-keyframes { } @keyframes { from { top: 0 } }',
    // custom properties
    '.a { --x: { a: b }; --y: [1]; --z: ); --w: } } .b { --q: ; --r:; --s: !important; --t: 1 !important }',
    ':root { --a: 1px; --b: calc(1px + 2px); --c: "x"; --d: { color: red } }',
    // selectors
    'a > b + c ~ d e /deep/ f >>> g {} :is(.a, .b) :where(x) :not(.c) li:nth-child(2n + 1 of .x) {} [a="b" i] [c|d] [*|e] {} *|* {} ns|a {}',
    ':global(.a) :global .b {} & .c {} .d { &:hover {} & + .e {} > .f {} }',
    // at-rules
    '@import url("a.css") layer(base) supports(display: grid) screen and (orientation: landscape); @import "b.css"; @import;',
    '@layer a, b.c; @layer d { .a { color: red } } @layer { .b {} } @container sidebar (min-width: 400px) and style(--x: 1) { .a { top: 0 } }',
    '@supports (display: grid) and (not (display: inline-grid)) { .a {} } @supports selector(:has(a)) {} @supports not (x) {}',
    '@scope (.a) to (.b) { .c { color: red } } @starting-style { .a { opacity: 0 } } @property --x { syntax: "<length>"; inherits: false; initial-value: 0px }',
    '@page :first { margin: 1in; @top-left { content: "x" } @foo {} } @namespace svg url(http://www.w3.org/2000/svg); @charset "utf-8";',
    '@media screen and (min-width: 100px), print and (max-width: 200px) { .a {} } @media (400px <= width <= 700px) { .a { color: red } } @media (min-resolution: 2dppx) {}',
    '@media not all and (monochrome) {} @media only screen {} @media (width >= 600px) and (orientation: landscape) {} @media x and y {}',
    '@view-transition { navigation: auto; } @position-try --x { top: 0 } @font-palette-values --x { font-family: a } @counter-style x { system: cyclic }',
    // values
    '.a { width: calc(100% - 10px); background: url(data:image/png;base64,abc) no-repeat; font: 12px/1.5 "A", sans-serif; grid-template-areas: "a b" [x] 1fr; unicode-range: U+0025-00FF, u+4??; }',
    '.a { filter: progid:DXImageTransform.Microsoft.gradient(startColorstr=#000, endColorstr=#fff); color: rgb(1 2 3 / 50%); color: rgba(1,2,3); x: 1e3px; }',
    '.a { margin: -1px +2px .5em 0; transform: rotate(45deg) translate(1cqw, 2cqh); transition: all 1s, color 2ms; aspect-ratio: 16 / 9; }',
    // errors
    '.a { color red; } .b { color: ; } .c { : x } .d { color: red', '.a { color: red }}', '.a { color: (1 }', '.a { color: [1 }', 'a b c', '@media { }', '.a { b: c d: e }',
    '.a {{ }', '} .a {}', '.a { color: red; ;; }', '@import url(x) a b {', '.a::{}', '.a:{}', '#{}', '. a {}', '# a {}', '.a { @media x }',
    '.a { color: "unterminated }', ".a { content: 'x\\\ny' }", '.a { b: url(x y) }', '.a { b: url( "x" ) }', '.a { b: url(x', '/* unterminated',
    '<!-- .a { color: red } -->', '.a { b: c !imp }', '.a { b: c ! important }', '.a { -: x; --: y; ---: z }', 'a[b {}', '@media (min-width: ) {}',
    '.a { b: 1 +; }', ':root{--x:{}}', '.a{color:red;}.b{}', '\\61 {}', '.\\31 a {}', '.a { b\\: c }',
    // JS Object.prototype quirks
    '.a { constructor: x } .b { -webkit-toString: x } .c { -webkit-Constructor: x } .d { __proto__: 1 }',
    '@keyframes toString { } .a { color: red }', '@keyframes constructor { from {} } .a { x: y }', '.a { width: 10constructor; height: 1__proto__ }',
    '.a { -webkit-hasOwnProperty: 1; color: blue }', '.a { valueof: 1 }',
    // tokens
    '.a { width: 1E3px; height: 1.5.5px; top: --1px; left: +-1px; content: "\\""; quotes: "\\201C" "\\201D" }',
    '.a { b: \\0 } .c { d: "\\0" } .e\\ f {} .g { content: "\\10FFFF" "\\110000" "\\1234567" }',
    // nesting
    '.a { .b { color: red } &.c { d: e } @media x { color: red } color: blue; .e & {} }',
    '.parent { color: blue; & > .child { color: red; } .x { y: z } }',
    // unicode
    '.é { colör: red; } .😀 { c: d } .a { content: "😀" }',
];

const HAND_SCSS = [
    '$a: 1; $b: 2 !default; $c: 3 !global; $d: 4 !foo; .a { color: $a; width: $b + 1px; }',
    '@mixin m($a, $b: 1px, $rest...) { width: $a; } .a { @include m(1px, $b: 2px); @include x.y; @include z { color: red } @include w using ($p) { top: $p } }',
    '@function f($x) { @if $x > 1 { @return 1; } @else if $x < 0 { @return 2 } @else { @return 3 } } .a { width: f(2); }',
    '@each $k, $v in (a: 1, b: 2) { .#{$k} { width: $v } } @for $i from 1 through 3 { .m-#{$i} { margin: $i * 1px } } @while $i > 0 { .w { x: y } }',
    '@use "sass:math" as m; @use "x" with ($a: 1, $b: 2 !default); @forward "y" as z-* hide a, $b; @use "q" foo;',
    '.a { &-suffix { x: y } &__el { a: b } & + & { c: d } %placeholder { e: f } @extend %placeholder; @extend .b !optional; }',
    '.a { font: { family: x; size: 1px; } margin: 1px { top: 2px } }',
    '@debug "x"; @warn "y"; @error "z"; .a { @at-root .b { c: d } } @at-root (without: media) { .e {} }',
    '// line comment\n.a { b: c; // trailing\n d: e }',
    '.a { width: math.div(1, 2); height: m.$x; color: map.get($m, "k"); }',
    '#{$sel} { x: y } .a-#{$b} { #{$prop}: 1; #{$p}-top: 2; }',
    '@media #{$q} { .a {} } @media screen and ($x <= 1) {} @supports #{$s} {}',
    '.a { b: c; @content; @content(1, 2); }',
    '$map: (key1: value1, key2: value2, key3: value3); $list: 1px 2px, 3px;',
    '@if $a == 1 { .b {} } @else { .c {} } .d { @if not $x { e: f } }',
    '.a { $local: 1; width: $local }', '@import "a", "b";', '$x: ;', '$y', '.a { $z: }', '@include ;', '@mixin {}', '@function f() {}',
    '.a { colr: red; -webkit-foo: 1 } .b {} @apply x;', '.a { b: c !important !default }', '.a{ width: calc(#{$a} + 1px) }',
    '@for $i from 1 {}', '@each in x {}', '@if {}', '@use;', '@forward "a" as b;', '.a { @extend; }', '%p {} .b { @extend %p }',
];

const HAND_LESS = [
    '@a: 1px; @b: @a * 2; .a { width: @b; height: ~"calc(100% - @{a})"; }',
    '.mixin(@a; @b: 2) { width: @a } .a { .mixin(1px; 2px); .mixin(1px) !important; #ns > .m(); #ns.m(); }',
    '.guard when (@mode = huge) { width: 100% } .m(@a) when (lightness(@a) >= 50%) { color: black } .n() when not (@x) {}',
    '@plugin "my-plugin"; @import (reference, optional) "foo.less"; @import (css) url("x.css") screen;',
    '.a:extend(.b all) { } .c { &:extend(.d); } .e:extend(.f, .g) {}',
    '@detached: { background: red; }; .a { @detached(); } @r: { .x { y: z } }',
    '.a { @media (min-width: 768px) { color: red } } @my-ruleset: { .my-selector { background-color: black; } };',
    '.@{name} { color: red } .a { @{prop}: 1; background-@{p}: x; }',
    '@x: { a: b }; .a { @x(); } .b { width: e("x"); filter: ~"ms:alwaysHasItsOwnSyntax.For.Stuff()"; }',
    '.m(@rest...) {} .n(...) {} .o(@a; @rest...) {} .p(@a, ...) {}', '.a { .b; } .c { #d; }', '@var: ~`"hello".toUpperCase()`;',
    '.a { width: (@a + 5) * 2; color: if((@mode = dark), white, black); x: @@name; y: $color; }',
    '.lazy-eval { width: @var; @var: @a; @a: 9%; }', '@media @phone { .a {} } @min768: ~"(min-width: 768px)"; @media @min768 {}',
    '.a { colr: red; -webkit-foo: 1 } .b {} @apply x;', '.a when {}', '.m( {}', '@a: ;', '@b', '.a { @c: }', '.x { .y() !important; }',
    '@import (x) ;', '@plugin;', '.a { b: `x` }', '.a { b: `x }', '.a { @r(); }', '#ns { .m() { c: d } }', '.a { .mixin() !important }',
    '@rules: { a { b: c } }', '.a(@x: { b: c }) {}', '.guard() when (default()) {}', '@media screen { @media (min-width: 768px) { .a {} } }',
    // bootstrap-3 style
    '.clearfix() { &:before, &:after { content: " "; display: table; } &:after { clear: both; } }\n.row { .clearfix(); margin-left: (@gutter / -2); }',
    '.button-variant(@color; @background; @border) { color: @color; background-color: @background; &:focus, &.focus { color: @color; background-color: darken(@background, 10%); border-color: darken(@border, 25%); } }\n.btn-default { .button-variant(@btn-default-color; @btn-default-bg; @btn-default-border); }',
    '.make-grid-columns() { .col(@index) { @item: ~".col-xs-@{index}, .col-sm-@{index}"; .col((@index + 1), @item); } .col(@index, @list) when (@index =< @grid-columns) { @item: ~".col-xs-@{index}"; .col((@index + 1), ~"@{list}, @{item}"); } .col(@index, @list) when (@index > @grid-columns) { @{list} { position: relative; } } .col(1); }',
    '.loop(@counter) when (@counter > 0) { .loop((@counter - 1)); width: (10px * @counter); }\ndiv { .loop(5); }',
    '@min768: ~"(min-width: 768px)";\n.element { @media @min768 { font-size: 1.2rem; } }\n@media @phone, @tablet and (orientation: landscape) { .x { y: z } }',
    '.mixin(@color) when (iscolor(@color)) { color: @color } .mixin(@a) when (isnumber(@a)) and (@a > 0) { width: @a } .mixin(@b) when not (@b > 0) { width: 0 }',
    'button when (@my-option = true) { color: white; } & when (@mode = dark) { .x { color: black } }',
    '.a { .mixin(#008000); width: ~"calc(100% - @{w})"; @r: { color: red }; filter: ~"progid:DXImageTransform.Microsoft.Alpha(opacity=50)"; }',
    '.b { background+: url(1.png); background+_: url(2.png); .c; #namespace > .mixin(); #namespace.mixin(); }',
    '@selector: ~".my-class"; @{selector} { color: red; } @property: color; .widget { @{property}: #0ee; background-@{property}: #999; }',
    '@primary: blue; @secondary: @primary; @var: "primary"; .x { color: @@var; } .y { color: $color; color: red; }',
    '@config: { option1: true; option2: false; } .mixin() when (@config[option1] = true) { selected: value; } .z { width: @config[width]; c: .mixin[@result]; }',
    '@plugin "plugin"; .test { width: pi(); } @import (less) "foo.css"; @import (inline) "not-less-compatible.css"; @import (once, optional) "x";',
    '.e(@rules) { @media screen { @rules(); } } @detached-ruleset: { background: red; }; .top { .e(@detached-ruleset); }',
    '.m(@a; @b: 2) when (default()) { x: @a @b } .m(@a, @rest...) { y: @rest } .n(...) {} .o(@arguments) { box-shadow: @arguments; }',
    '#main { width: ~`"@{str}".toUpperCase() + "!"`; height: `1 + 1`; @var: ~`"hello".toUpperCase()`; }',
    '.a:hover when (@enabled) { x: y } .b:extend(.c all) {} .d { &:extend(.e all); &-f { g: h } & + & { i: j } && { k: l } }',
    '.guard1() when (@a) and (@b), (@c) { x: y } .guard2() when not (@a), (@b) {} .lazy { @var: @a; @a: 1; width: @var; }',
    '.x { .y !important; .z() !important; @w(); color: if((iscolor(@c)), @c, black); width: percentage(0.5); height: unit(5, px); }',
    '@media (min-width: @screen-sm-min) and (max-width: @screen-sm-max) { .visible-sm { display: block !important; } }',
    '.mixin(dark; @color) { color: darken(@color, 10%); } .mixin(light; @color) { color: lighten(@color, 10%); } .mixin(@_; @color) { display: block; }',
    '@my-ruleset: { .my-selector { @media tv { background-color: black; } } }; @media (orientation:portrait) { @my-ruleset(); }',
    '.a { @import "b"; } .c { @plugin "d"; } @import url(x.less) screen; @import "e" print;',
];

if (want('hand')) {
    const files = [];
    const add = (css, lang, from) => files.push({ text: wrap(css, lang), from });
    HAND_CSS.forEach((c, i) => {
        add(c, '', 'css#' + i);
        add(c, 'scss', 'css-as-scss#' + i);
        add(c, 'less', 'css-as-less#' + i);
    });
    HAND_SCSS.forEach((c, i) => {
        add(c, 'scss', 'scss#' + i);
        add(c, '', 'scss-as-css#' + i);
    });
    HAND_LESS.forEach((c, i) => {
        add(c, 'less', 'less#' + i);
        add(c, '', 'less-as-css#' + i);
    });
    // mutations of the hand-written cases, in every language
    const r = rng(51);
    for (const c of [...HAND_CSS, ...HAND_SCSS, ...HAND_LESS]) {
        for (const lang of ['', 'scss', 'less']) {
            for (let i = 0; i < 4; i++) add(mutate(c, r), lang, 'hand-mut');
        }
    }
    // whole-file shapes
    const shapes = [
        '<style>\r\n.a {\r\n  colr: red;\r\n}\r\n</style>',
        '﻿<style>.a { colr: red }</style>',
        '<style>.a { colr: red }',
        '<style lang="scss">.a { colr: red }</style><style>.b { colr: red }</style>',
        '{#if x}<style>.a { colr: red }</style>{/if}<style>.b { colr: red }</style>',
        '{@html x}<style>.a { colr: red }</style>',
        '<div><style>.a { colr: red }</style></div>',
        '<svelte:head><style>.a { colr: red }</style></svelte:head>',
        '<script>let s = "<style>.a{colr:red}</style>";</script><style>.b { colr: red }</style>',
        '<!-- <style>.a { colr: red }</style> --><style>.b { colr: red }</style>',
        '<style lang="scss" lang>.a { colr: red }</style>',
        '<style lang="">.a { colr: red }</style>',
        '<style type="text/less">.a { colr: red }</style>',
        '<style lang="text/text/scss">.a { colr: red } $x: 1;</style>',
        '<style lang="SCSS">.a { colr: red } $x: 1;</style>',
        '<style lang="postcss">.a { colr: red }</style>',
        '<style lang={x}>.a { colr: red } a > b {}</style>',
        '<style\n  lang="less"\n>.a { colr: red }</style>',
        '<p>{"</style>"}</p><style>.a { colr: red }</style>',
        '<style>.a { colr: red }</STYLE><style>.b {}</style>',
        '<STYLE>.a { colr: red }</STYLE>',
        '<style/>.a { colr: red }',
        '<style>\n</style>',
        '<style></style>',
        '<template><style>.a {colr:red}</style></template>',
        'text\n<style>.a { colr: red }</style>',
        '{#each xs as x}\n{/each}\n<style>.a { colr: red }</style>',
        '{#await p}{:then}{/await}<style>.a { colr: red }</style>{/if}',
        '<div {...props}><style>.a{colr:red}</style></div><style>.b{}</style>',
        '<style a={"}"}>.a { colr: red }</style>',
        "<style a='{'>.a { colr: red }</style>",
        '<style lang="scss" // c\n>.a { colr: red }</style>',
        '<style /* c */ lang="less">.a { colr: red }</style>',
        '<p>é😀</p>\n<style>\n.é😀 { colr: red; } .b { co😀lr: 1 }\n</style>',
        '<style>.a { colr: red }</style >',
        '<style>.a{}</style><style>.b{}</style>',
        '<style>\r.a {\r colr: red\r}\r</style>',
        '<style>\n.a { color: red; }\n.b {\n\tdisplay: inline-block;\n\tfloat: left;\n}\n</style>',
    ];
    shapes.forEach((s, i) => files.push({ text: s, from: 'shape#' + i }));
    writeCorpus('hand', files);
}
