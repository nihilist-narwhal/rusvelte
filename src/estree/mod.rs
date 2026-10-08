//! An owned ESTree AST for the JavaScript that Svelte's code generation builds and prints.
//!
//! Svelte's transform (`phases/3-transform`) works on acorn ESTree nodes: it copies nodes out
//! of the component's scripts and template expressions, creates new ones with
//! `utils/builders.js`, mutates and re-parents them freely and finally prints the program with
//! esrap. This module holds that tree:
//!
//! - [`convert`] turns oxc's AST into it, in acorn's shapes, with TypeScript removed the way
//!   `phases/1-parse/remove_typescript_nodes.js` removes it.
//! - [`builders`] is a port of `utils/builders.js` (`b.*`).
//! - [`print`] is a port of esrap 2.4.0 with its `ts` language (`print(ast, ts({ comments }))`).
//!
//! # Representation
//!
//! A [`Node`] is a [`NodeKind`] (one variant per ESTree `type`, each holding a struct with that
//! type's fields) plus the fields every ESTree node can carry: `start`/`end` ([`Node::span`]),
//! `loc`, `leadingComments`/`trailingComments`, and [`Node::origin`], the identity of the oxc
//! node it was converted from. Children are `Box<Node>` / `Vec<Node>`, so the tree is owned,
//! `Clone` copies a subtree, and moving a child elsewhere is a `std::mem::replace`.
//!
//! Why one `Node` type rather than separate `Expression`/`Statement`/`Pattern` enums: the
//! transform is dynamically typed. Visitors return a different kind of node than they were
//! given (a statement visitor returns `b.empty`, `TSAsExpression` returns its expression), the
//! same slot holds expressions or patterns depending on context (`AssignmentExpression.left`,
//! `ForOfStatement.left`), and checks are written as `node.type === 'Identifier'` across
//! categories. A single enum with struct variants keeps those checks a `match` and makes a
//! line-by-line port possible, at the cost of the static distinction between expression and
//! statement slots (which ESTree itself doesn't enforce at runtime either).
//!
//! Strings are [`Atom`]s (`compact_str`): names up to 24 bytes are stored inline, so cloning
//! an identifier doesn't allocate. Operators reuse `oxc_syntax`'s operator enums.
//!
//! # Positions
//!
//! [`Node::span`] holds `start`/`end` as **byte offsets** into the source the node was parsed
//! from (for a component, the whole `.svelte` file), like the rest of this crate's analysis.
//! acorn's (and so Svelte's) `start`/`end` are UTF-16 offsets; convert with
//! [`crate::locator::Locator::utf16`] where a JS-visible offset is needed. [`Node::loc`] is
//! acorn's `loc`: 1-based lines (acorn's line breaks: `\n`, `\r\n`, `\r`, U+2028, U+2029) and
//! 0-based columns in UTF-16 code units. The printer only reads `loc` (for comment placement
//! and source map mappings), never `span`.

pub mod builders;
pub mod convert;
pub mod print;
#[cfg(test)]
mod tests;

pub use compact_str::CompactString as Atom;
pub use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator};

/// A line/column pair: 1-based line, 0-based column in UTF-16 code units
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub fn new(line: u32, column: u32) -> Self {
        Position { line, column }
    }
}

/// ESTree's `loc`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct SourceLocation {
    pub start: Position,
    pub end: Position,
}

/// `start`/`end`, as byte offsets (see the module docs)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Line,
    Block,
}

/// A comment, as in Svelte's `root.comments` / `analysis.comments` (`{ type, value, start, end, loc }`)
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub kind: CommentKind,
    pub value: Atom,
    pub span: Option<Span>,
    pub loc: Option<SourceLocation>,
}

impl Comment {
    pub fn line(value: impl Into<Atom>) -> Self {
        Comment { kind: CommentKind::Line, value: value.into(), span: None, loc: None }
    }
    pub fn block(value: impl Into<Atom>) -> Self {
        Comment { kind: CommentKind::Block, value: value.into(), span: None, loc: None }
    }
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            CommentKind::Line => "Line",
            CommentKind::Block => "Block",
        }
    }
}

/// `leadingComments` / `trailingComments` set on a node
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachedComments {
    pub leading: Vec<Comment>,
    pub trailing: Vec<Comment>,
}

/// An ESTree node
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// `start`/`end` (byte offsets), `None` for nodes made by builders
    pub span: Option<Span>,
    /// acorn's `loc`, `None` for nodes made by builders
    pub loc: Option<SourceLocation>,
    /// The address of the oxc node this was converted from (`AstKind::address()`), which is
    /// what the analysis keys scopes and bindings by. `None` for nodes made by builders.
    pub origin: Option<usize>,
    /// `leadingComments` / `trailingComments`
    pub comments: Option<Box<AttachedComments>>,
}

impl From<NodeKind> for Node {
    fn from(kind: NodeKind) -> Self {
        Node::new(kind)
    }
}

impl Node {
    pub fn new(kind: NodeKind) -> Self {
        Node { kind, span: None, loc: None, origin: None, comments: None }
    }

    pub fn with_span(mut self, span: Option<Span>, loc: Option<SourceLocation>) -> Self {
        self.span = span;
        self.loc = loc;
        self
    }

    /// `start` (a byte offset)
    pub fn start(&self) -> Option<u32> {
        self.span.map(|s| s.start)
    }

    /// `end` (a byte offset)
    pub fn end(&self) -> Option<u32> {
        self.span.map(|s| s.end)
    }

    pub fn leading_comments(&self) -> &[Comment] {
        self.comments.as_ref().map_or(&[], |c| &c.leading)
    }

    pub fn trailing_comments(&self) -> &[Comment] {
        self.comments.as_ref().map_or(&[], |c| &c.trailing)
    }

    /// `node.leadingComments = comments`
    pub fn set_leading_comments(&mut self, comments: Vec<Comment>) {
        self.comments.get_or_insert_with(Default::default).leading = comments;
    }

    /// `node.trailingComments = comments`
    pub fn set_trailing_comments(&mut self, comments: Vec<Comment>) {
        self.comments.get_or_insert_with(Default::default).trailing = comments;
    }

    /// The ESTree `type`
    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    /// `node.type === ty`
    pub fn is(&self, ty: &str) -> bool {
        self.kind.type_name() == ty
    }

    /// The name of an `Identifier`
    pub fn identifier_name(&self) -> Option<&Atom> {
        match &self.kind {
            NodeKind::Identifier(id) => Some(&id.name),
            _ => None,
        }
    }

    /// Visit the child nodes in ESTree key order (what zimmerframe's `next()` walks)
    pub fn for_each_child<'n>(&'n self, f: &mut dyn FnMut(&'n Node)) {
        self.kind.for_each_child(f)
    }

    /// Visit the child nodes mutably, in ESTree key order
    pub fn for_each_child_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        self.kind.for_each_child_mut(f)
    }
}

// -------------------------------------------------------------------------------------------
// Node payloads

pub type BoxNode = Box<Node>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Module,
    Script,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub body: Vec<Node>,
    pub source_type: SourceType,
}

#[derive(Debug, Clone)]
pub struct Identifier {
    pub name: Atom,
}

#[derive(Debug, Clone)]
pub struct PrivateIdentifier {
    pub name: Atom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegExpValue {
    pub pattern: Atom,
    pub flags: Atom,
}

/// The `value` of a `Literal` (`bigint` literals keep acorn's `bigint` string, without `n`
/// and without numeric separators)
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    String(Atom),
    Number(f64),
    Boolean(bool),
    Null,
    RegExp(Box<RegExpValue>),
    BigInt(Atom),
}

#[derive(Debug, Clone)]
pub struct Literal {
    pub value: LiteralValue,
    /// `raw`, `None` for literals made by builders
    pub raw: Option<Atom>,
}

/// `ArrayExpression` and `ArrayPattern` (holes are `None`)
#[derive(Debug, Clone)]
pub struct ArrayExpression {
    pub elements: Vec<Option<Node>>,
}

/// `ObjectExpression` and `ObjectPattern`
#[derive(Debug, Clone)]
pub struct ObjectExpression {
    pub properties: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Init,
    Get,
    Set,
}

impl PropertyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PropertyKind::Init => "init",
            PropertyKind::Get => "get",
            PropertyKind::Set => "set",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Property {
    pub key: BoxNode,
    pub value: BoxNode,
    pub kind: PropertyKind,
    pub method: bool,
    pub shorthand: bool,
    pub computed: bool,
}

/// `FunctionDeclaration` and `FunctionExpression`
#[derive(Debug, Clone)]
pub struct Function {
    pub id: Option<BoxNode>,
    pub params: Vec<Node>,
    /// a `BlockStatement`
    pub body: BoxNode,
    pub generator: bool,
    pub is_async: bool,
}

#[derive(Debug, Clone)]
pub struct ArrowFunctionExpression {
    pub params: Vec<Node>,
    pub body: BoxNode,
    /// `body` is an expression rather than a block
    pub expression: bool,
    pub is_async: bool,
}

/// `ClassDeclaration` and `ClassExpression`
#[derive(Debug, Clone)]
pub struct Class {
    pub id: Option<BoxNode>,
    pub super_class: Option<BoxNode>,
    /// a `ClassBody`
    pub body: BoxNode,
}

/// `ClassBody`, `BlockStatement`, `StaticBlock`
#[derive(Debug, Clone)]
pub struct Body {
    pub body: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Constructor,
    Method,
    Get,
    Set,
}

impl MethodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MethodKind::Constructor => "constructor",
            MethodKind::Method => "method",
            MethodKind::Get => "get",
            MethodKind::Set => "set",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MethodDefinition {
    pub key: BoxNode,
    /// a `FunctionExpression`
    pub value: BoxNode,
    pub kind: MethodKind,
    pub computed: bool,
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct PropertyDefinition {
    pub key: BoxNode,
    pub value: Option<BoxNode>,
    pub computed: bool,
    pub is_static: bool,
    /// acorn-typescript's `abstract`, which `remove_typescript_nodes` leaves on class fields
    pub is_abstract: bool,
}

#[derive(Debug, Clone)]
pub struct UnaryExpression {
    pub operator: UnaryOperator,
    pub argument: BoxNode,
}

#[derive(Debug, Clone)]
pub struct UpdateExpression {
    pub operator: UpdateOperator,
    pub prefix: bool,
    pub argument: BoxNode,
}

#[derive(Debug, Clone)]
pub struct BinaryExpression {
    pub operator: BinaryOperator,
    pub left: BoxNode,
    pub right: BoxNode,
}

#[derive(Debug, Clone)]
pub struct LogicalExpression {
    pub operator: LogicalOperator,
    pub left: BoxNode,
    pub right: BoxNode,
}

#[derive(Debug, Clone)]
pub struct AssignmentExpression {
    pub operator: AssignmentOperator,
    pub left: BoxNode,
    pub right: BoxNode,
}

#[derive(Debug, Clone)]
pub struct AssignmentPattern {
    pub left: BoxNode,
    pub right: BoxNode,
}

/// `RestElement`, `SpreadElement`
#[derive(Debug, Clone)]
pub struct Argument {
    pub argument: BoxNode,
}

#[derive(Debug, Clone)]
pub struct MemberExpression {
    pub object: BoxNode,
    pub property: BoxNode,
    pub computed: bool,
    pub optional: bool,
}

/// `ChainExpression`, `ParenthesizedExpression`, `ExpressionStatement` (with `directive`),
/// `TSExternalModuleReference`, `TSExportAssignment`
#[derive(Debug, Clone)]
pub struct ExpressionWrapper {
    pub expression: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ExpressionStatement {
    pub expression: BoxNode,
    /// the directive (`'use strict'` → `use strict`), for directive prologues
    pub directive: Option<Atom>,
}

#[derive(Debug, Clone)]
pub struct CallExpression {
    pub callee: BoxNode,
    pub arguments: Vec<Node>,
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct NewExpression {
    pub callee: BoxNode,
    pub arguments: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct ConditionalExpression {
    pub test: BoxNode,
    pub consequent: BoxNode,
    pub alternate: BoxNode,
}

#[derive(Debug, Clone)]
pub struct SequenceExpression {
    pub expressions: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct YieldExpression {
    pub argument: Option<BoxNode>,
    pub delegate: bool,
}

#[derive(Debug, Clone)]
pub struct TemplateLiteral {
    /// `TemplateElement`s
    pub quasis: Vec<Node>,
    pub expressions: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct TemplateElement {
    pub raw: Atom,
    pub cooked: Option<Atom>,
    pub tail: bool,
}

#[derive(Debug, Clone)]
pub struct TaggedTemplateExpression {
    pub tag: BoxNode,
    /// a `TemplateLiteral`
    pub quasi: BoxNode,
}

#[derive(Debug, Clone)]
pub struct MetaProperty {
    pub meta: BoxNode,
    pub property: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ImportExpression {
    pub source: BoxNode,
    pub options: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct ReturnStatement {
    pub argument: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct LabeledStatement {
    pub label: BoxNode,
    pub body: BoxNode,
}

/// `BreakStatement`, `ContinueStatement`
#[derive(Debug, Clone)]
pub struct Jump {
    pub label: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct IfStatement {
    pub test: BoxNode,
    pub consequent: BoxNode,
    pub alternate: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct SwitchStatement {
    pub discriminant: BoxNode,
    pub cases: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct SwitchCase {
    pub test: Option<BoxNode>,
    pub consequent: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct ThrowStatement {
    pub argument: BoxNode,
}

#[derive(Debug, Clone)]
pub struct TryStatement {
    pub block: BoxNode,
    pub handler: Option<BoxNode>,
    pub finalizer: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct CatchClause {
    pub param: Option<BoxNode>,
    pub body: BoxNode,
}

/// `WhileStatement`, `DoWhileStatement`
#[derive(Debug, Clone)]
pub struct WhileStatement {
    pub test: BoxNode,
    pub body: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ForStatement {
    pub init: Option<BoxNode>,
    pub test: Option<BoxNode>,
    pub update: Option<BoxNode>,
    pub body: BoxNode,
}

/// `ForInStatement`, `ForOfStatement` (`is_await` only for the latter)
#[derive(Debug, Clone)]
pub struct ForInStatement {
    pub left: BoxNode,
    pub right: BoxNode,
    pub body: BoxNode,
    pub is_await: bool,
}

#[derive(Debug, Clone)]
pub struct WithStatement {
    pub object: BoxNode,
    pub body: BoxNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
}

impl VariableKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VariableKind::Var => "var",
            VariableKind::Let => "let",
            VariableKind::Const => "const",
            VariableKind::Using => "using",
            VariableKind::AwaitUsing => "await using",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VariableDeclaration {
    pub kind: VariableKind,
    pub declarations: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct VariableDeclarator {
    pub id: BoxNode,
    pub init: Option<BoxNode>,
}

#[derive(Debug, Clone)]
pub struct ImportDeclaration {
    pub specifiers: Vec<Node>,
    pub source: BoxNode,
    pub attributes: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct ImportSpecifier {
    pub imported: BoxNode,
    pub local: BoxNode,
}

/// `ImportDefaultSpecifier`, `ImportNamespaceSpecifier`
#[derive(Debug, Clone)]
pub struct LocalSpecifier {
    pub local: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ImportAttribute {
    pub key: BoxNode,
    pub value: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ExportNamedDeclaration {
    pub declaration: Option<BoxNode>,
    pub specifiers: Vec<Node>,
    pub source: Option<BoxNode>,
    pub attributes: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct ExportSpecifier {
    pub local: BoxNode,
    pub exported: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ExportDefaultDeclaration {
    pub declaration: BoxNode,
}

#[derive(Debug, Clone)]
pub struct ExportAllDeclaration {
    pub exported: Option<BoxNode>,
    pub source: BoxNode,
    pub attributes: Vec<Node>,
}

/// `import x = require('y')`, which `remove_typescript_nodes` leaves in place
#[derive(Debug, Clone)]
pub struct TSImportEqualsDeclaration {
    pub id: BoxNode,
    pub module_reference: BoxNode,
    /// `import type x = ...`
    pub is_type: bool,
}

#[derive(Debug, Clone)]
pub struct TSQualifiedName {
    pub left: BoxNode,
    pub right: BoxNode,
}

#[derive(Debug, Clone)]
pub struct TSNamespaceExportDeclaration {
    pub id: BoxNode,
}

/// One variant per ESTree `type`
#[derive(Debug, Clone)]
pub enum NodeKind {
    Program(Program),
    Identifier(Identifier),
    PrivateIdentifier(PrivateIdentifier),
    Literal(Literal),
    ThisExpression,
    Super,
    ArrayExpression(ArrayExpression),
    ArrayPattern(ArrayExpression),
    ObjectExpression(ObjectExpression),
    ObjectPattern(ObjectExpression),
    Property(Property),
    FunctionDeclaration(Function),
    FunctionExpression(Function),
    ArrowFunctionExpression(ArrowFunctionExpression),
    ClassDeclaration(Class),
    ClassExpression(Class),
    ClassBody(Body),
    MethodDefinition(MethodDefinition),
    PropertyDefinition(PropertyDefinition),
    StaticBlock(Body),
    UnaryExpression(UnaryExpression),
    UpdateExpression(UpdateExpression),
    BinaryExpression(BinaryExpression),
    LogicalExpression(LogicalExpression),
    AssignmentExpression(AssignmentExpression),
    AssignmentPattern(AssignmentPattern),
    RestElement(Argument),
    SpreadElement(Argument),
    MemberExpression(MemberExpression),
    ChainExpression(ExpressionWrapper),
    CallExpression(CallExpression),
    NewExpression(NewExpression),
    ConditionalExpression(ConditionalExpression),
    SequenceExpression(SequenceExpression),
    YieldExpression(YieldExpression),
    AwaitExpression(Argument),
    TemplateLiteral(TemplateLiteral),
    TemplateElement(TemplateElement),
    TaggedTemplateExpression(TaggedTemplateExpression),
    MetaProperty(MetaProperty),
    ImportExpression(ImportExpression),
    ParenthesizedExpression(ExpressionWrapper),
    ExpressionStatement(ExpressionStatement),
    BlockStatement(Body),
    EmptyStatement,
    DebuggerStatement,
    ReturnStatement(ReturnStatement),
    LabeledStatement(LabeledStatement),
    BreakStatement(Jump),
    ContinueStatement(Jump),
    IfStatement(IfStatement),
    SwitchStatement(SwitchStatement),
    SwitchCase(SwitchCase),
    ThrowStatement(ThrowStatement),
    TryStatement(TryStatement),
    CatchClause(CatchClause),
    WhileStatement(WhileStatement),
    DoWhileStatement(WhileStatement),
    ForStatement(ForStatement),
    ForInStatement(ForInStatement),
    ForOfStatement(ForInStatement),
    WithStatement(WithStatement),
    VariableDeclaration(VariableDeclaration),
    VariableDeclarator(VariableDeclarator),
    ImportDeclaration(ImportDeclaration),
    ImportSpecifier(ImportSpecifier),
    ImportDefaultSpecifier(LocalSpecifier),
    ImportNamespaceSpecifier(LocalSpecifier),
    ImportAttribute(ImportAttribute),
    ExportNamedDeclaration(ExportNamedDeclaration),
    ExportSpecifier(ExportSpecifier),
    ExportDefaultDeclaration(ExportDefaultDeclaration),
    ExportAllDeclaration(ExportAllDeclaration),
    TSImportEqualsDeclaration(TSImportEqualsDeclaration),
    TSExternalModuleReference(ExpressionWrapper),
    TSQualifiedName(TSQualifiedName),
    TSExportAssignment(ExpressionWrapper),
    TSNamespaceExportDeclaration(TSNamespaceExportDeclaration),
}

impl NodeKind {
    pub fn type_name(&self) -> &'static str {
        use NodeKind::*;
        match self {
            Program(_) => "Program",
            Identifier(_) => "Identifier",
            PrivateIdentifier(_) => "PrivateIdentifier",
            Literal(_) => "Literal",
            ThisExpression => "ThisExpression",
            Super => "Super",
            ArrayExpression(_) => "ArrayExpression",
            ArrayPattern(_) => "ArrayPattern",
            ObjectExpression(_) => "ObjectExpression",
            ObjectPattern(_) => "ObjectPattern",
            Property(_) => "Property",
            FunctionDeclaration(_) => "FunctionDeclaration",
            FunctionExpression(_) => "FunctionExpression",
            ArrowFunctionExpression(_) => "ArrowFunctionExpression",
            ClassDeclaration(_) => "ClassDeclaration",
            ClassExpression(_) => "ClassExpression",
            ClassBody(_) => "ClassBody",
            MethodDefinition(_) => "MethodDefinition",
            PropertyDefinition(_) => "PropertyDefinition",
            StaticBlock(_) => "StaticBlock",
            UnaryExpression(_) => "UnaryExpression",
            UpdateExpression(_) => "UpdateExpression",
            BinaryExpression(_) => "BinaryExpression",
            LogicalExpression(_) => "LogicalExpression",
            AssignmentExpression(_) => "AssignmentExpression",
            AssignmentPattern(_) => "AssignmentPattern",
            RestElement(_) => "RestElement",
            SpreadElement(_) => "SpreadElement",
            MemberExpression(_) => "MemberExpression",
            ChainExpression(_) => "ChainExpression",
            CallExpression(_) => "CallExpression",
            NewExpression(_) => "NewExpression",
            ConditionalExpression(_) => "ConditionalExpression",
            SequenceExpression(_) => "SequenceExpression",
            YieldExpression(_) => "YieldExpression",
            AwaitExpression(_) => "AwaitExpression",
            TemplateLiteral(_) => "TemplateLiteral",
            TemplateElement(_) => "TemplateElement",
            TaggedTemplateExpression(_) => "TaggedTemplateExpression",
            MetaProperty(_) => "MetaProperty",
            ImportExpression(_) => "ImportExpression",
            ParenthesizedExpression(_) => "ParenthesizedExpression",
            ExpressionStatement(_) => "ExpressionStatement",
            BlockStatement(_) => "BlockStatement",
            EmptyStatement => "EmptyStatement",
            DebuggerStatement => "DebuggerStatement",
            ReturnStatement(_) => "ReturnStatement",
            LabeledStatement(_) => "LabeledStatement",
            BreakStatement(_) => "BreakStatement",
            ContinueStatement(_) => "ContinueStatement",
            IfStatement(_) => "IfStatement",
            SwitchStatement(_) => "SwitchStatement",
            SwitchCase(_) => "SwitchCase",
            ThrowStatement(_) => "ThrowStatement",
            TryStatement(_) => "TryStatement",
            CatchClause(_) => "CatchClause",
            WhileStatement(_) => "WhileStatement",
            DoWhileStatement(_) => "DoWhileStatement",
            ForStatement(_) => "ForStatement",
            ForInStatement(_) => "ForInStatement",
            ForOfStatement(_) => "ForOfStatement",
            WithStatement(_) => "WithStatement",
            VariableDeclaration(_) => "VariableDeclaration",
            VariableDeclarator(_) => "VariableDeclarator",
            ImportDeclaration(_) => "ImportDeclaration",
            ImportSpecifier(_) => "ImportSpecifier",
            ImportDefaultSpecifier(_) => "ImportDefaultSpecifier",
            ImportNamespaceSpecifier(_) => "ImportNamespaceSpecifier",
            ImportAttribute(_) => "ImportAttribute",
            ExportNamedDeclaration(_) => "ExportNamedDeclaration",
            ExportSpecifier(_) => "ExportSpecifier",
            ExportDefaultDeclaration(_) => "ExportDefaultDeclaration",
            ExportAllDeclaration(_) => "ExportAllDeclaration",
            TSImportEqualsDeclaration(_) => "TSImportEqualsDeclaration",
            TSExternalModuleReference(_) => "TSExternalModuleReference",
            TSQualifiedName(_) => "TSQualifiedName",
            TSExportAssignment(_) => "TSExportAssignment",
            TSNamespaceExportDeclaration(_) => "TSNamespaceExportDeclaration",
        }
    }
}

/// Generates `for_each_child` / `for_each_child_mut` from one list of child fields per kind,
/// in acorn's key order
macro_rules! children {
    ($self:ident, $f:ident, $($ref:tt)*) => {{
        use NodeKind::*;
        macro_rules! one { ($e:expr) => { $f($($ref)* **$e) }; }
        macro_rules! opt { ($e:expr) => { if let Some(x) = $e { $f($($ref)* **x) } }; }
        macro_rules! list { ($e:expr) => { for x in $e { $f(x) } }; }
        macro_rules! holes { ($e:expr) => { for x in $e { if let Some(x) = x { $f(x) } } }; }
        match $self {
            Program(n) => list!($($ref)* n.body),
            Identifier(_) | PrivateIdentifier(_) | Literal(_) | ThisExpression | Super | EmptyStatement
            | DebuggerStatement | TemplateElement(_) => {}
            ArrayExpression(n) | ArrayPattern(n) => holes!($($ref)* n.elements),
            ObjectExpression(n) | ObjectPattern(n) => list!($($ref)* n.properties),
            Property(n) => { one!($($ref)* n.key); one!($($ref)* n.value); }
            FunctionDeclaration(n) | FunctionExpression(n) => {
                opt!($($ref)* n.id);
                list!($($ref)* n.params);
                one!($($ref)* n.body);
            }
            ArrowFunctionExpression(n) => { list!($($ref)* n.params); one!($($ref)* n.body); }
            ClassDeclaration(n) | ClassExpression(n) => {
                opt!($($ref)* n.id);
                opt!($($ref)* n.super_class);
                one!($($ref)* n.body);
            }
            ClassBody(n) | StaticBlock(n) | BlockStatement(n) => list!($($ref)* n.body),
            MethodDefinition(n) => { one!($($ref)* n.key); one!($($ref)* n.value); }
            PropertyDefinition(n) => { one!($($ref)* n.key); opt!($($ref)* n.value); }
            UnaryExpression(n) => one!($($ref)* n.argument),
            UpdateExpression(n) => one!($($ref)* n.argument),
            BinaryExpression(n) => { one!($($ref)* n.left); one!($($ref)* n.right); }
            LogicalExpression(n) => { one!($($ref)* n.left); one!($($ref)* n.right); }
            AssignmentExpression(n) => { one!($($ref)* n.left); one!($($ref)* n.right); }
            AssignmentPattern(n) => { one!($($ref)* n.left); one!($($ref)* n.right); }
            RestElement(n) | SpreadElement(n) | AwaitExpression(n) => one!($($ref)* n.argument),
            MemberExpression(n) => { one!($($ref)* n.object); one!($($ref)* n.property); }
            ChainExpression(n) | ParenthesizedExpression(n) | TSExternalModuleReference(n) | TSExportAssignment(n) => {
                one!($($ref)* n.expression)
            }
            CallExpression(n) => { one!($($ref)* n.callee); list!($($ref)* n.arguments); }
            NewExpression(n) => { one!($($ref)* n.callee); list!($($ref)* n.arguments); }
            ConditionalExpression(n) => {
                one!($($ref)* n.test);
                one!($($ref)* n.consequent);
                one!($($ref)* n.alternate);
            }
            SequenceExpression(n) => list!($($ref)* n.expressions),
            YieldExpression(n) => opt!($($ref)* n.argument),
            TemplateLiteral(n) => { list!($($ref)* n.quasis); list!($($ref)* n.expressions); }
            TaggedTemplateExpression(n) => { one!($($ref)* n.tag); one!($($ref)* n.quasi); }
            MetaProperty(n) => { one!($($ref)* n.meta); one!($($ref)* n.property); }
            ImportExpression(n) => { one!($($ref)* n.source); opt!($($ref)* n.options); }
            ExpressionStatement(n) => one!($($ref)* n.expression),
            ReturnStatement(n) => opt!($($ref)* n.argument),
            LabeledStatement(n) => { one!($($ref)* n.label); one!($($ref)* n.body); }
            BreakStatement(n) | ContinueStatement(n) => opt!($($ref)* n.label),
            IfStatement(n) => {
                one!($($ref)* n.test);
                one!($($ref)* n.consequent);
                opt!($($ref)* n.alternate);
            }
            SwitchStatement(n) => { one!($($ref)* n.discriminant); list!($($ref)* n.cases); }
            SwitchCase(n) => { opt!($($ref)* n.test); list!($($ref)* n.consequent); }
            ThrowStatement(n) => one!($($ref)* n.argument),
            TryStatement(n) => {
                one!($($ref)* n.block);
                opt!($($ref)* n.handler);
                opt!($($ref)* n.finalizer);
            }
            CatchClause(n) => { opt!($($ref)* n.param); one!($($ref)* n.body); }
            WhileStatement(n) => { one!($($ref)* n.test); one!($($ref)* n.body); }
            DoWhileStatement(n) => { one!($($ref)* n.body); one!($($ref)* n.test); }
            ForStatement(n) => {
                opt!($($ref)* n.init);
                opt!($($ref)* n.test);
                opt!($($ref)* n.update);
                one!($($ref)* n.body);
            }
            ForInStatement(n) | ForOfStatement(n) => {
                one!($($ref)* n.left);
                one!($($ref)* n.right);
                one!($($ref)* n.body);
            }
            WithStatement(n) => { one!($($ref)* n.object); one!($($ref)* n.body); }
            VariableDeclaration(n) => list!($($ref)* n.declarations),
            VariableDeclarator(n) => { one!($($ref)* n.id); opt!($($ref)* n.init); }
            ImportDeclaration(n) => {
                list!($($ref)* n.specifiers);
                one!($($ref)* n.source);
                list!($($ref)* n.attributes);
            }
            ImportSpecifier(n) => { one!($($ref)* n.imported); one!($($ref)* n.local); }
            ImportDefaultSpecifier(n) | ImportNamespaceSpecifier(n) => one!($($ref)* n.local),
            ImportAttribute(n) => { one!($($ref)* n.key); one!($($ref)* n.value); }
            ExportNamedDeclaration(n) => {
                opt!($($ref)* n.declaration);
                list!($($ref)* n.specifiers);
                opt!($($ref)* n.source);
                list!($($ref)* n.attributes);
            }
            ExportSpecifier(n) => { one!($($ref)* n.local); one!($($ref)* n.exported); }
            ExportDefaultDeclaration(n) => one!($($ref)* n.declaration),
            ExportAllDeclaration(n) => {
                opt!($($ref)* n.exported);
                one!($($ref)* n.source);
                list!($($ref)* n.attributes);
            }
            TSImportEqualsDeclaration(n) => { one!($($ref)* n.id); one!($($ref)* n.module_reference); }
            TSQualifiedName(n) => { one!($($ref)* n.left); one!($($ref)* n.right); }
            TSNamespaceExportDeclaration(n) => one!($($ref)* n.id),
        }
    }};
}

impl NodeKind {
    pub fn for_each_child<'n>(&'n self, f: &mut dyn FnMut(&'n Node)) {
        children!(self, f, &)
    }

    pub fn for_each_child_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        children!(self, f, &mut)
    }
}

// -------------------------------------------------------------------------------------------
// Operators from their source text

/// `UnaryOperator` from its text (`"!"`, `"typeof"`, ...)
pub fn unary_operator(op: &str) -> Option<UnaryOperator> {
    use UnaryOperator::*;
    [UnaryPlus, UnaryNegation, LogicalNot, BitwiseNot, Typeof, Void, Delete].into_iter().find(|o| o.as_str() == op)
}

/// `UpdateOperator` from its text
pub fn update_operator(op: &str) -> Option<UpdateOperator> {
    use UpdateOperator::*;
    [Increment, Decrement].into_iter().find(|o| o.as_str() == op)
}

/// `BinaryOperator` from its text
pub fn binary_operator(op: &str) -> Option<BinaryOperator> {
    use BinaryOperator::*;
    [
        Equality,
        Inequality,
        StrictEquality,
        StrictInequality,
        LessThan,
        LessEqualThan,
        GreaterThan,
        GreaterEqualThan,
        Addition,
        Subtraction,
        Multiplication,
        Division,
        Remainder,
        Exponential,
        ShiftLeft,
        ShiftRight,
        ShiftRightZeroFill,
        BitwiseOR,
        BitwiseXOR,
        BitwiseAnd,
        In,
        Instanceof,
    ]
    .into_iter()
    .find(|o| o.as_str() == op)
}

/// `LogicalOperator` from its text
pub fn logical_operator(op: &str) -> Option<LogicalOperator> {
    use LogicalOperator::*;
    [Or, And, Coalesce].into_iter().find(|o| o.as_str() == op)
}

/// `AssignmentOperator` from its text
pub fn assignment_operator(op: &str) -> Option<AssignmentOperator> {
    use AssignmentOperator::*;
    [
        Assign,
        Addition,
        Subtraction,
        Multiplication,
        Division,
        Remainder,
        Exponential,
        ShiftLeft,
        ShiftRight,
        ShiftRightZeroFill,
        BitwiseOR,
        BitwiseXOR,
        BitwiseAnd,
        LogicalOr,
        LogicalAnd,
        LogicalNullish,
    ]
    .into_iter()
    .find(|o| o.as_str() == op)
}

// -------------------------------------------------------------------------------------------
// Rebuilding a node from transformed children

type MapFn<'f> = &'f mut dyn FnMut(&Node) -> Node;

fn m1(f: MapFn, n: &Node) -> BoxNode {
    Box::new(f(n))
}

fn mo(f: MapFn, n: &Option<BoxNode>) -> Option<BoxNode> {
    n.as_ref().map(|n| Box::new(f(n)))
}

fn ml(f: MapFn, list: &[Node]) -> Vec<Node> {
    list.iter().map(|n| f(n)).collect()
}

fn mh(f: MapFn, list: &[Option<Node>]) -> Vec<Option<Node>> {
    list.iter().map(|n| n.as_ref().map(|n| f(n))).collect()
}

impl Node {
    /// A node of the same kind and metadata (`span`, `loc`, `origin`, comments) whose children
    /// are `f(child)`, called in [`Node::for_each_child`] order. Only scalar fields are copied.
    pub fn map_children(&self, f: &mut dyn FnMut(&Node) -> Node) -> Node {
        use NodeKind::*;
        let kind = match &self.kind {
            Program(n) => Program(self::Program { body: ml(f, &n.body), source_type: n.source_type }),
            Identifier(_) | PrivateIdentifier(_) | Literal(_) | ThisExpression | Super | EmptyStatement
            | DebuggerStatement | TemplateElement(_) => self.kind.clone(),
            ArrayExpression(n) => ArrayExpression(self::ArrayExpression { elements: mh(f, &n.elements) }),
            ArrayPattern(n) => ArrayPattern(self::ArrayExpression { elements: mh(f, &n.elements) }),
            ObjectExpression(n) => ObjectExpression(self::ObjectExpression { properties: ml(f, &n.properties) }),
            ObjectPattern(n) => ObjectPattern(self::ObjectExpression { properties: ml(f, &n.properties) }),
            Property(n) => Property(self::Property {
                key: m1(f, &n.key),
                value: m1(f, &n.value),
                kind: n.kind,
                method: n.method,
                shorthand: n.shorthand,
                computed: n.computed,
            }),
            FunctionDeclaration(n) => FunctionDeclaration(map_function(f, n)),
            FunctionExpression(n) => FunctionExpression(map_function(f, n)),
            ArrowFunctionExpression(n) => ArrowFunctionExpression(self::ArrowFunctionExpression {
                params: ml(f, &n.params),
                body: m1(f, &n.body),
                expression: n.expression,
                is_async: n.is_async,
            }),
            ClassDeclaration(n) => ClassDeclaration(map_class(f, n)),
            ClassExpression(n) => ClassExpression(map_class(f, n)),
            ClassBody(n) => ClassBody(Body { body: ml(f, &n.body) }),
            StaticBlock(n) => StaticBlock(Body { body: ml(f, &n.body) }),
            BlockStatement(n) => BlockStatement(Body { body: ml(f, &n.body) }),
            MethodDefinition(n) => MethodDefinition(self::MethodDefinition {
                key: m1(f, &n.key),
                value: m1(f, &n.value),
                kind: n.kind,
                computed: n.computed,
                is_static: n.is_static,
            }),
            PropertyDefinition(n) => PropertyDefinition(self::PropertyDefinition {
                key: m1(f, &n.key),
                value: mo(f, &n.value),
                computed: n.computed,
                is_static: n.is_static,
                is_abstract: n.is_abstract,
            }),
            UnaryExpression(n) => UnaryExpression(self::UnaryExpression { operator: n.operator, argument: m1(f, &n.argument) }),
            UpdateExpression(n) => UpdateExpression(self::UpdateExpression {
                operator: n.operator,
                prefix: n.prefix,
                argument: m1(f, &n.argument),
            }),
            BinaryExpression(n) => BinaryExpression(self::BinaryExpression {
                operator: n.operator,
                left: m1(f, &n.left),
                right: m1(f, &n.right),
            }),
            LogicalExpression(n) => LogicalExpression(self::LogicalExpression {
                operator: n.operator,
                left: m1(f, &n.left),
                right: m1(f, &n.right),
            }),
            AssignmentExpression(n) => AssignmentExpression(self::AssignmentExpression {
                operator: n.operator,
                left: m1(f, &n.left),
                right: m1(f, &n.right),
            }),
            AssignmentPattern(n) => AssignmentPattern(self::AssignmentPattern { left: m1(f, &n.left), right: m1(f, &n.right) }),
            RestElement(n) => RestElement(Argument { argument: m1(f, &n.argument) }),
            SpreadElement(n) => SpreadElement(Argument { argument: m1(f, &n.argument) }),
            AwaitExpression(n) => AwaitExpression(Argument { argument: m1(f, &n.argument) }),
            MemberExpression(n) => MemberExpression(self::MemberExpression {
                object: m1(f, &n.object),
                property: m1(f, &n.property),
                computed: n.computed,
                optional: n.optional,
            }),
            ChainExpression(n) => ChainExpression(ExpressionWrapper { expression: m1(f, &n.expression) }),
            ParenthesizedExpression(n) => ParenthesizedExpression(ExpressionWrapper { expression: m1(f, &n.expression) }),
            TSExternalModuleReference(n) => TSExternalModuleReference(ExpressionWrapper { expression: m1(f, &n.expression) }),
            TSExportAssignment(n) => TSExportAssignment(ExpressionWrapper { expression: m1(f, &n.expression) }),
            CallExpression(n) => CallExpression(self::CallExpression {
                callee: m1(f, &n.callee),
                arguments: ml(f, &n.arguments),
                optional: n.optional,
            }),
            NewExpression(n) => NewExpression(self::NewExpression { callee: m1(f, &n.callee), arguments: ml(f, &n.arguments) }),
            ConditionalExpression(n) => ConditionalExpression(self::ConditionalExpression {
                test: m1(f, &n.test),
                consequent: m1(f, &n.consequent),
                alternate: m1(f, &n.alternate),
            }),
            SequenceExpression(n) => SequenceExpression(self::SequenceExpression { expressions: ml(f, &n.expressions) }),
            YieldExpression(n) => YieldExpression(self::YieldExpression { argument: mo(f, &n.argument), delegate: n.delegate }),
            TemplateLiteral(n) => TemplateLiteral(self::TemplateLiteral {
                quasis: ml(f, &n.quasis),
                expressions: ml(f, &n.expressions),
            }),
            TaggedTemplateExpression(n) => TaggedTemplateExpression(self::TaggedTemplateExpression {
                tag: m1(f, &n.tag),
                quasi: m1(f, &n.quasi),
            }),
            MetaProperty(n) => MetaProperty(self::MetaProperty { meta: m1(f, &n.meta), property: m1(f, &n.property) }),
            ImportExpression(n) => ImportExpression(self::ImportExpression { source: m1(f, &n.source), options: mo(f, &n.options) }),
            ExpressionStatement(n) => ExpressionStatement(self::ExpressionStatement {
                expression: m1(f, &n.expression),
                directive: n.directive.clone(),
            }),
            ReturnStatement(n) => ReturnStatement(self::ReturnStatement { argument: mo(f, &n.argument) }),
            LabeledStatement(n) => LabeledStatement(self::LabeledStatement { label: m1(f, &n.label), body: m1(f, &n.body) }),
            BreakStatement(n) => BreakStatement(Jump { label: mo(f, &n.label) }),
            ContinueStatement(n) => ContinueStatement(Jump { label: mo(f, &n.label) }),
            IfStatement(n) => IfStatement(self::IfStatement {
                test: m1(f, &n.test),
                consequent: m1(f, &n.consequent),
                alternate: mo(f, &n.alternate),
            }),
            SwitchStatement(n) => SwitchStatement(self::SwitchStatement {
                discriminant: m1(f, &n.discriminant),
                cases: ml(f, &n.cases),
            }),
            SwitchCase(n) => SwitchCase(self::SwitchCase { test: mo(f, &n.test), consequent: ml(f, &n.consequent) }),
            ThrowStatement(n) => ThrowStatement(self::ThrowStatement { argument: m1(f, &n.argument) }),
            TryStatement(n) => TryStatement(self::TryStatement {
                block: m1(f, &n.block),
                handler: mo(f, &n.handler),
                finalizer: mo(f, &n.finalizer),
            }),
            CatchClause(n) => CatchClause(self::CatchClause { param: mo(f, &n.param), body: m1(f, &n.body) }),
            WhileStatement(n) => WhileStatement(self::WhileStatement { test: m1(f, &n.test), body: m1(f, &n.body) }),
            DoWhileStatement(n) => DoWhileStatement(self::WhileStatement { body: m1(f, &n.body), test: m1(f, &n.test) }),
            ForStatement(n) => ForStatement(self::ForStatement {
                init: mo(f, &n.init),
                test: mo(f, &n.test),
                update: mo(f, &n.update),
                body: m1(f, &n.body),
            }),
            ForInStatement(n) => ForInStatement(map_for_in(f, n)),
            ForOfStatement(n) => ForOfStatement(map_for_in(f, n)),
            WithStatement(n) => WithStatement(self::WithStatement { object: m1(f, &n.object), body: m1(f, &n.body) }),
            VariableDeclaration(n) => VariableDeclaration(self::VariableDeclaration {
                kind: n.kind,
                declarations: ml(f, &n.declarations),
            }),
            VariableDeclarator(n) => VariableDeclarator(self::VariableDeclarator { id: m1(f, &n.id), init: mo(f, &n.init) }),
            ImportDeclaration(n) => ImportDeclaration(self::ImportDeclaration {
                specifiers: ml(f, &n.specifiers),
                source: m1(f, &n.source),
                attributes: ml(f, &n.attributes),
            }),
            ImportSpecifier(n) => ImportSpecifier(self::ImportSpecifier { imported: m1(f, &n.imported), local: m1(f, &n.local) }),
            ImportDefaultSpecifier(n) => ImportDefaultSpecifier(LocalSpecifier { local: m1(f, &n.local) }),
            ImportNamespaceSpecifier(n) => ImportNamespaceSpecifier(LocalSpecifier { local: m1(f, &n.local) }),
            ImportAttribute(n) => ImportAttribute(self::ImportAttribute { key: m1(f, &n.key), value: m1(f, &n.value) }),
            ExportNamedDeclaration(n) => ExportNamedDeclaration(self::ExportNamedDeclaration {
                declaration: mo(f, &n.declaration),
                specifiers: ml(f, &n.specifiers),
                source: mo(f, &n.source),
                attributes: ml(f, &n.attributes),
            }),
            ExportSpecifier(n) => ExportSpecifier(self::ExportSpecifier { local: m1(f, &n.local), exported: m1(f, &n.exported) }),
            ExportDefaultDeclaration(n) => ExportDefaultDeclaration(self::ExportDefaultDeclaration { declaration: m1(f, &n.declaration) }),
            ExportAllDeclaration(n) => ExportAllDeclaration(self::ExportAllDeclaration {
                exported: mo(f, &n.exported),
                source: m1(f, &n.source),
                attributes: ml(f, &n.attributes),
            }),
            TSImportEqualsDeclaration(n) => TSImportEqualsDeclaration(self::TSImportEqualsDeclaration {
                id: m1(f, &n.id),
                module_reference: m1(f, &n.module_reference),
                is_type: n.is_type,
            }),
            TSQualifiedName(n) => TSQualifiedName(self::TSQualifiedName { left: m1(f, &n.left), right: m1(f, &n.right) }),
            TSNamespaceExportDeclaration(n) => TSNamespaceExportDeclaration(self::TSNamespaceExportDeclaration { id: m1(f, &n.id) }),
        };
        Node { kind, span: self.span, loc: self.loc, origin: self.origin, comments: self.comments.clone() }
    }
}

fn map_function(f: MapFn, n: &Function) -> Function {
    Function {
        id: mo(f, &n.id),
        params: ml(f, &n.params),
        body: m1(f, &n.body),
        generator: n.generator,
        is_async: n.is_async,
    }
}

fn map_class(f: MapFn, n: &Class) -> Class {
    Class { id: mo(f, &n.id), super_class: mo(f, &n.super_class), body: m1(f, &n.body) }
}

fn map_for_in(f: MapFn, n: &ForInStatement) -> ForInStatement {
    ForInStatement { left: m1(f, &n.left), right: m1(f, &n.right), body: m1(f, &n.body), is_await: n.is_await }
}
