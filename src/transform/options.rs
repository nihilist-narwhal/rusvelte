//! `compile()` options (`validate-options.js`), combined with `<svelte:options>` the way
//! `compile` combines them.

use serde_json::Value;

/// `generate`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Generate {
    #[default]
    Client,
    Server,
    /// `false`: analysis only
    None,
}

/// `cssHash`: the default (`svelte-${hash(filename)}`), a constant, or a function
pub enum CssHash {
    Default,
    Constant(String),
    Function(Box<dyn Fn(&super::CssHashInput) -> String>),
}

/// Validated compile options
pub struct CompileOptions {
    /// `filename` (`(unknown)` when absent)
    pub filename: String,
    /// `rootDir`: `filename` is made relative to it
    pub root_dir: Option<String>,
    pub dev: bool,
    pub generate: Generate,
    /// `experimental.async`
    pub experimental_async: bool,
    pub accessors: bool,
    /// `css: 'injected'`
    pub css_injected: bool,
    pub css_hash: CssHash,
    pub custom_element: bool,
    pub disclose_version: bool,
    pub immutable: bool,
    /// `compatibility.componentApi === 4`
    pub component_api_4: bool,
    /// `name`
    pub name: Option<String>,
    /// `namespace` (`html` when absent)
    pub namespace: String,
    pub preserve_comments: bool,
    /// `fragments: 'tree'`
    pub fragments_tree: bool,
    pub preserve_whitespace: bool,
    /// `runes` (`None`: inferred)
    pub runes: Option<bool>,
    pub hmr: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            filename: "(unknown)".into(),
            root_dir: None,
            dev: false,
            generate: Generate::Client,
            experimental_async: false,
            accessors: false,
            css_injected: false,
            css_hash: CssHash::Default,
            custom_element: false,
            disclose_version: true,
            immutable: false,
            component_api_4: false,
            name: None,
            namespace: "html".into(),
            preserve_comments: false,
            fragments_tree: false,
            preserve_whitespace: false,
            runes: None,
            hmr: false,
        }
    }
}

impl CompileOptions {
    /// Options from their JSON form (as the oracle records them; a `cssHash` function is
    /// `{ "fn": <source> }` and only constant ones, `() => 'x'`, are understood)
    pub fn from_json(v: &Value) -> CompileOptions {
        let mut o = CompileOptions::default();
        let bool_of = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        if let Some(f) = v.get("filename").and_then(Value::as_str) {
            o.filename = f.into();
        }
        o.root_dir = v.get("rootDir").and_then(Value::as_str).map(str::to_string);
        o.dev = bool_of("dev", false);
        o.generate = match v.get("generate") {
            Some(Value::String(s)) if s == "server" || s == "ssr" => Generate::Server,
            Some(Value::Bool(false)) => Generate::None,
            _ => Generate::Client,
        };
        o.experimental_async = v.pointer("/experimental/async").and_then(Value::as_bool).unwrap_or(false);
        o.accessors = bool_of("accessors", false);
        o.css_injected = v.get("css").and_then(Value::as_str) == Some("injected");
        o.css_hash = match v.get("cssHash") {
            Some(Value::Object(f)) => match f.get("fn").and_then(Value::as_str).and_then(constant_function) {
                Some(c) => CssHash::Constant(c),
                None => CssHash::Default,
            },
            _ => CssHash::Default,
        };
        o.custom_element = bool_of("customElement", false);
        o.disclose_version = bool_of("discloseVersion", true);
        o.immutable = bool_of("immutable", false);
        o.component_api_4 = v.pointer("/compatibility/componentApi").and_then(Value::as_u64) == Some(4);
        o.name = v.get("name").and_then(Value::as_str).map(str::to_string);
        if let Some(ns) = v.get("namespace").and_then(Value::as_str) {
            o.namespace = ns.into();
        }
        o.preserve_comments = bool_of("preserveComments", false);
        o.fragments_tree = v.get("fragments").and_then(Value::as_str) == Some("tree");
        o.preserve_whitespace = bool_of("preserveWhitespace", false);
        o.runes = v.get("runes").and_then(Value::as_bool);
        o.hmr = bool_of("hmr", false);
        o
    }
}

/// The string a function like `() => 'svelte-xyz'` returns
fn constant_function(source: &str) -> Option<String> {
    let body = source.split_once("=>")?.1.trim();
    let quote = body.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let inner = body.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.contains(quote)).then(|| inner.to_string())
}
