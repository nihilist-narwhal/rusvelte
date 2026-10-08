//! oxc AST → [`Node`], in the shapes acorn produces, with TypeScript removed the way
//! `phases/1-parse/remove_typescript_nodes.js` removes it.
//!
//! - `Literal`s keep `raw` (the source text), `bigint` literals their digits.
//! - oxc's object, binding and assignment-target properties all become `Property`, rest
//!   elements are appended to `properties`/`elements`, parameters with defaults become
//!   `AssignmentPattern`s, a function body is a `BlockStatement` whose first statements are
//!   the directives, `import.meta`/`new.target` are `MetaProperty`s.
//! - `ParenthesizedExpression`s are dropped (acorn without `preserveParens`, or Svelte's
//!   `remove_parens`), unless [`Converter::preserve_parens`] is set.
//! - TypeScript: `as`/`satisfies`/`!`/`<T>x`/`x<T>` become their expression, type-only
//!   declarations, `declare`d declarations, overload signatures, abstract methods, `declare`
//!   fields and type-only imports/exports become `EmptyStatement`s (or are dropped from class
//!   bodies), type annotations, `this` parameters and modifiers disappear. What
//!   `remove_typescript_nodes` leaves in place is kept: `import x = require(...)`,
//!   `export =`, `export as namespace`, and `abstract` on class fields. Enums, namespaces with
//!   values, decorators and accessor fields are errors in Svelte (reported by the analysis);
//!   they become `EmptyStatement`s or are dropped here.
//!
//! Every converted node gets `span` (byte offsets), `loc` (acorn's line/column, from the
//! [`Locator`]) and `origin`, the address of the oxc node (what `AstKind::address()` returns
//! for it). ESTree nodes with no oxc counterpart get the address of the oxc node they are made
//! from, plus one: the `AssignmentPattern` of a parameter with a default value (from the
//! `FormalParameter`), and of `{ a = 1 } = x` (from the `AssignmentTargetPropertyIdentifier`),
//! matching `P::key()` in `analyze/nodes.rs`. The identifiers of `MetaProperty` use the
//! address of the `ImportMeta`/`NewTarget` plus one and two.

use oxc_ast::ast as ox;
use oxc_span::GetSpan;

use super::*;
use crate::locator::Locator;

pub struct Converter<'l> {
    locator: &'l Locator<'l>,
    /// The source is TypeScript: apply `remove_typescript_nodes`'s rules that also change
    /// JavaScript (`export {}` is removed) and give bindings TS-ESTree's spans (which include
    /// type annotations)
    pub ts: bool,
    /// Keep `ParenthesizedExpression`s (acorn's `preserveParens`)
    pub preserve_parens: bool,
}

#[inline]
fn addr<T>(r: &T) -> usize {
    r as *const T as usize
}

fn is_empty(node: &Node) -> bool {
    matches!(node.kind, NodeKind::EmptyStatement)
}

impl<'l> Converter<'l> {
    /// `locator` must be over the source the oxc spans point into
    pub fn new(locator: &'l Locator<'l>, ts: bool) -> Self {
        Converter { locator, ts, preserve_parens: false }
    }

    fn pos(&self, offset: u32) -> Position {
        let (line, column) = self.locator.acorn_line_column(offset as usize);
        Position { line: line as u32, column: column as u32 }
    }

    pub fn location(&self, span: oxc_span::Span) -> SourceLocation {
        SourceLocation { start: self.pos(span.start), end: self.pos(span.end) }
    }

    fn node(&self, kind: NodeKind, span: oxc_span::Span, origin: usize) -> Node {
        Node {
            kind,
            span: Some(Span::new(span.start, span.end)),
            loc: Some(self.location(span)),
            origin: Some(origin),
            comments: None,
        }
    }

    fn boxed(&self, kind: NodeKind, span: oxc_span::Span, origin: usize) -> BoxNode {
        Box::new(self.node(kind, span, origin))
    }

    fn empty(&self, span: oxc_span::Span, origin: usize) -> Node {
        self.node(NodeKind::EmptyStatement, span, origin)
    }

    fn source(&self, span: oxc_span::Span) -> &'l str {
        self.locator.source().get(span.start as usize..span.end as usize).unwrap_or("")
    }

    // ---------------------------------------------------------------------------------------
    // Programs and statements

    /// A whole program. Acorn's `Program` spans the whole source; oxc's span is used as is.
    pub fn program(&self, p: &ox::Program) -> Node {
        let mut body = Vec::with_capacity(p.directives.len() + p.body.len());
        for d in &p.directives {
            body.push(self.directive(d));
        }
        self.statements_into(&p.body, &mut body);
        let source_type = if p.source_type.is_module() { SourceType::Module } else { SourceType::Script };
        self.node(NodeKind::Program(Program { body, source_type }), p.span, addr(p))
    }

    fn directive(&self, d: &ox::Directive) -> Node {
        let lit = self.string_literal(&d.expression);
        self.node(
            NodeKind::ExpressionStatement(ExpressionStatement {
                expression: Box::new(lit),
                directive: Some(Atom::from(d.directive.as_str())),
            }),
            d.span,
            addr(d),
        )
    }

    fn statements_into(&self, list: &[ox::Statement], out: &mut Vec<Node>) {
        out.reserve(list.len());
        for s in list {
            out.push(self.statement(s));
        }
    }

    fn statements(&self, list: &[ox::Statement]) -> Vec<Node> {
        let mut out = Vec::new();
        self.statements_into(list, &mut out);
        out
    }

    fn block(&self, b: &ox::BlockStatement) -> Node {
        self.node(NodeKind::BlockStatement(Body { body: self.statements(&b.body) }), b.span, addr(b))
    }

    fn function_body(&self, b: &ox::FunctionBody) -> Node {
        let mut body = Vec::with_capacity(b.directives.len() + b.statements.len());
        for d in &b.directives {
            body.push(self.directive(d));
        }
        self.statements_into(&b.statements, &mut body);
        self.node(NodeKind::BlockStatement(Body { body }), b.span, addr(b))
    }

    /// A statement. TypeScript-only statements become `EmptyStatement`s.
    pub fn statement(&self, s: &ox::Statement) -> Node {
        use ox::Statement as S;
        match s {
            S::BlockStatement(b) => self.block(b),
            S::BreakStatement(b) => self.node(
                NodeKind::BreakStatement(Jump { label: b.label.as_ref().map(|l| Box::new(self.label(l))) }),
                b.span,
                addr(&**b),
            ),
            S::ContinueStatement(b) => self.node(
                NodeKind::ContinueStatement(Jump { label: b.label.as_ref().map(|l| Box::new(self.label(l))) }),
                b.span,
                addr(&**b),
            ),
            S::DebuggerStatement(d) => self.node(NodeKind::DebuggerStatement, d.span, addr(&**d)),
            S::DoWhileStatement(d) => self.node(
                NodeKind::DoWhileStatement(WhileStatement {
                    test: Box::new(self.expression(&d.test)),
                    body: Box::new(self.statement(&d.body)),
                }),
                d.span,
                addr(&**d),
            ),
            S::EmptyStatement(e) => self.node(NodeKind::EmptyStatement, e.span, addr(&**e)),
            S::ExpressionStatement(e) => self.node(
                NodeKind::ExpressionStatement(ExpressionStatement {
                    expression: Box::new(self.expression(&e.expression)),
                    directive: None,
                }),
                e.span,
                addr(&**e),
            ),
            S::ForInStatement(f) => self.node(
                NodeKind::ForInStatement(ForInStatement {
                    left: Box::new(self.for_left(&f.left)),
                    right: Box::new(self.expression(&f.right)),
                    body: Box::new(self.statement(&f.body)),
                    is_await: false,
                }),
                f.span,
                addr(&**f),
            ),
            S::ForOfStatement(f) => self.node(
                NodeKind::ForOfStatement(ForInStatement {
                    left: Box::new(self.for_left(&f.left)),
                    right: Box::new(self.expression(&f.right)),
                    body: Box::new(self.statement(&f.body)),
                    is_await: f.r#await,
                }),
                f.span,
                addr(&**f),
            ),
            S::ForStatement(f) => self.node(
                NodeKind::ForStatement(ForStatement {
                    init: f.init.as_ref().map(|i| {
                        Box::new(match i {
                            ox::ForStatementInit::VariableDeclaration(d) => self.variable_declaration(d),
                            e => self.expression(e.as_expression().unwrap()),
                        })
                    }),
                    test: f.test.as_ref().map(|e| Box::new(self.expression(e))),
                    update: f.update.as_ref().map(|e| Box::new(self.expression(e))),
                    body: Box::new(self.statement(&f.body)),
                }),
                f.span,
                addr(&**f),
            ),
            S::IfStatement(i) => self.node(
                NodeKind::IfStatement(IfStatement {
                    test: Box::new(self.expression(&i.test)),
                    consequent: Box::new(self.statement(&i.consequent)),
                    alternate: i.alternate.as_ref().map(|a| Box::new(self.statement(a))),
                }),
                i.span,
                addr(&**i),
            ),
            S::LabeledStatement(l) => self.node(
                NodeKind::LabeledStatement(LabeledStatement {
                    label: Box::new(self.label(&l.label)),
                    body: Box::new(self.statement(&l.body)),
                }),
                l.span,
                addr(&**l),
            ),
            S::ReturnStatement(r) => self.node(
                NodeKind::ReturnStatement(ReturnStatement {
                    argument: r.argument.as_ref().map(|e| Box::new(self.expression(e))),
                }),
                r.span,
                addr(&**r),
            ),
            S::SwitchStatement(s) => self.node(
                NodeKind::SwitchStatement(SwitchStatement {
                    discriminant: Box::new(self.expression(&s.discriminant)),
                    cases: s
                        .cases
                        .iter()
                        .map(|c| {
                            self.node(
                                NodeKind::SwitchCase(SwitchCase {
                                    test: c.test.as_ref().map(|e| Box::new(self.expression(e))),
                                    consequent: self.statements(&c.consequent),
                                }),
                                c.span,
                                addr(c),
                            )
                        })
                        .collect(),
                }),
                s.span,
                addr(&**s),
            ),
            S::ThrowStatement(t) => self.node(
                NodeKind::ThrowStatement(ThrowStatement { argument: Box::new(self.expression(&t.argument)) }),
                t.span,
                addr(&**t),
            ),
            S::TryStatement(t) => self.node(
                NodeKind::TryStatement(TryStatement {
                    block: Box::new(self.block(&t.block)),
                    handler: t.handler.as_ref().map(|h| {
                        self.boxed(
                            NodeKind::CatchClause(CatchClause {
                                param: h.param.as_ref().map(|p| {
                                    let mut node = self.binding_pattern(&p.pattern);
                                    if let Some(t) = &p.type_annotation {
                                        self.extend_span(&mut node, t.span.end);
                                    }
                                    Box::new(node)
                                }),
                                body: Box::new(self.block(&h.body)),
                            }),
                            h.span,
                            addr(&**h),
                        )
                    }),
                    finalizer: t.finalizer.as_ref().map(|f| Box::new(self.block(f))),
                }),
                t.span,
                addr(&**t),
            ),
            S::WhileStatement(w) => self.node(
                NodeKind::WhileStatement(WhileStatement {
                    test: Box::new(self.expression(&w.test)),
                    body: Box::new(self.statement(&w.body)),
                }),
                w.span,
                addr(&**w),
            ),
            S::WithStatement(w) => self.node(
                NodeKind::WithStatement(WithStatement {
                    object: Box::new(self.expression(&w.object)),
                    body: Box::new(self.statement(&w.body)),
                }),
                w.span,
                addr(&**w),
            ),
            S::VariableDeclaration(d) => self.variable_declaration(d),
            S::FunctionDeclaration(f) => self.function(f, true),
            S::ClassDeclaration(c) => self.class(c, true),
            S::TSImportEqualsDeclaration(d) => self.import_equals(d),
            S::TSTypeAliasDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::TSInterfaceDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::TSEnumDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::TSExternalModuleDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::TSNamespaceDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::TSGlobalDeclaration(d) => self.empty(d.span, addr(&**d)),
            S::ImportDeclaration(d) => self.import_declaration(d),
            S::ExportAllDeclaration(d) => {
                if d.export_kind.is_type() {
                    return self.empty(d.span, addr(&**d));
                }
                self.node(
                    NodeKind::ExportAllDeclaration(ExportAllDeclaration {
                        exported: d.exported.as_ref().map(|e| Box::new(self.module_export_name(e))),
                        source: Box::new(self.string_literal(&d.source)),
                        attributes: self.with_clause(d.with_clause.as_deref()),
                    }),
                    d.span,
                    addr(&**d),
                )
            }
            S::ExportDefaultDeclaration(d) => {
                let declaration = match &d.declaration {
                    ox::ExportDefaultDeclarationKind::FunctionDeclaration(f) => self.function(f, true),
                    ox::ExportDefaultDeclarationKind::ClassDeclaration(c) => self.class(c, true),
                    ox::ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
                        return self.empty(d.span, addr(&**d));
                    }
                    e => self.expression(e.as_expression().unwrap()),
                };
                self.node(
                    NodeKind::ExportDefaultDeclaration(ExportDefaultDeclaration { declaration: Box::new(declaration) }),
                    d.span,
                    addr(&**d),
                )
            }
            S::ExportDeclaration(d) => {
                let declaration = self.declaration(&d.declaration);
                if is_empty(&declaration) {
                    return self.empty(d.span, addr(&**d));
                }
                self.node(
                    NodeKind::ExportNamedDeclaration(ExportNamedDeclaration {
                        declaration: Some(Box::new(declaration)),
                        specifiers: Vec::new(),
                        source: None,
                        attributes: Vec::new(),
                    }),
                    d.span,
                    addr(&**d),
                )
            }
            S::ExportNamedDeclaration(d) => {
                match self.export_specifiers(d.export_kind, &d.specifiers) {
                    None => self.empty(d.span, addr(&**d)),
                    Some(specifiers) => self.node(
                        NodeKind::ExportNamedDeclaration(ExportNamedDeclaration {
                            declaration: None,
                            specifiers,
                            source: None,
                            attributes: Vec::new(),
                        }),
                        d.span,
                        addr(&**d),
                    ),
                }
            }
            S::ExportFromDeclaration(d) => match self.export_specifiers(d.export_kind, &d.specifiers) {
                None => self.empty(d.span, addr(&**d)),
                Some(specifiers) => self.node(
                    NodeKind::ExportNamedDeclaration(ExportNamedDeclaration {
                        declaration: None,
                        specifiers,
                        source: Some(Box::new(self.string_literal(&d.source))),
                        attributes: self.with_clause(d.with_clause.as_deref()),
                    }),
                    d.span,
                    addr(&**d),
                ),
            },
            S::TSExportAssignment(d) => self.node(
                NodeKind::TSExportAssignment(ExpressionWrapper { expression: Box::new(self.expression(&d.expression)) }),
                d.span,
                addr(&**d),
            ),
            S::TSNamespaceExportDeclaration(d) => self.node(
                NodeKind::TSNamespaceExportDeclaration(TSNamespaceExportDeclaration {
                    id: Box::new(self.identifier_name(&d.id)),
                }),
                d.span,
                addr(&**d),
            ),
        }
    }

    fn declaration(&self, d: &ox::Declaration) -> Node {
        use ox::Declaration as D;
        match d {
            D::VariableDeclaration(d) => self.variable_declaration(d),
            D::FunctionDeclaration(f) => self.function(f, true),
            D::ClassDeclaration(c) => self.class(c, true),
            D::TSImportEqualsDeclaration(d) => self.import_equals(d),
            D::TSTypeAliasDeclaration(d) => self.empty(d.span, addr(&**d)),
            D::TSInterfaceDeclaration(d) => self.empty(d.span, addr(&**d)),
            D::TSEnumDeclaration(d) => self.empty(d.span, addr(&**d)),
            D::TSExternalModuleDeclaration(d) => self.empty(d.span, addr(&**d)),
            D::TSNamespaceDeclaration(d) => self.empty(d.span, addr(&**d)),
            D::TSGlobalDeclaration(d) => self.empty(d.span, addr(&**d)),
        }
    }

    /// `remove_typescript_nodes`'s `ExportNamedDeclaration` for specifier lists: `None` when the
    /// export goes away
    fn export_specifiers(&self, kind: ox::ImportOrExportKind, list: &[ox::ExportSpecifier]) -> Option<Vec<Node>> {
        if kind.is_type() {
            return None;
        }
        let specifiers: Vec<Node> = list
            .iter()
            .filter(|s| !s.export_kind.is_type())
            .map(|s| {
                self.node(
                    NodeKind::ExportSpecifier(ExportSpecifier {
                        local: Box::new(self.module_export_name(&s.local)),
                        exported: Box::new(self.module_export_name(&s.exported)),
                    }),
                    s.span,
                    addr(s),
                )
            })
            .collect();
        if self.ts && specifiers.is_empty() {
            return None;
        }
        Some(specifiers)
    }

    fn import_declaration(&self, d: &ox::ImportDeclaration) -> Node {
        if d.import_kind.is_type() {
            return self.empty(d.span, addr(d));
        }
        let mut specifiers = Vec::new();
        if let Some(list) = &d.specifiers {
            for s in list {
                use ox::ImportDeclarationSpecifier as I;
                specifiers.push(match s {
                    I::ImportSpecifier(s) => {
                        if s.import_kind.is_type() {
                            continue;
                        }
                        self.node(
                            NodeKind::ImportSpecifier(ImportSpecifier {
                                imported: Box::new(self.module_export_name(&s.imported)),
                                local: Box::new(self.binding_identifier(&s.local)),
                            }),
                            s.span,
                            addr(&**s),
                        )
                    }
                    I::ImportDefaultSpecifier(s) => self.node(
                        NodeKind::ImportDefaultSpecifier(LocalSpecifier { local: Box::new(self.binding_identifier(&s.local)) }),
                        s.span,
                        addr(&**s),
                    ),
                    I::ImportNamespaceSpecifier(s) => self.node(
                        NodeKind::ImportNamespaceSpecifier(LocalSpecifier {
                            local: Box::new(self.binding_identifier(&s.local)),
                        }),
                        s.span,
                        addr(&**s),
                    ),
                });
            }
            if specifiers.is_empty() && !list.is_empty() {
                return self.empty(d.span, addr(d));
            }
        }
        self.node(
            NodeKind::ImportDeclaration(ImportDeclaration {
                specifiers,
                source: Box::new(self.string_literal(&d.source)),
                attributes: self.with_clause(d.with_clause.as_deref()),
            }),
            d.span,
            addr(d),
        )
    }

    fn with_clause(&self, w: Option<&ox::WithClause>) -> Vec<Node> {
        let Some(w) = w else { return Vec::new() };
        w.with_entries
            .iter()
            .map(|a| {
                let key = match &a.key {
                    ox::ImportAttributeKey::Identifier(id) => self.identifier_name(id),
                    ox::ImportAttributeKey::StringLiteral(s) => self.string_literal(s),
                };
                self.node(
                    NodeKind::ImportAttribute(ImportAttribute {
                        key: Box::new(key),
                        value: Box::new(self.string_literal(&a.value)),
                    }),
                    a.span,
                    addr(a),
                )
            })
            .collect()
    }

    fn import_equals(&self, d: &ox::TSImportEqualsDeclaration) -> Node {
        let module_reference = match &d.module_reference {
            ox::TSModuleReference::ExternalModuleReference(r) => self.node(
                NodeKind::TSExternalModuleReference(ExpressionWrapper { expression: Box::new(self.string_literal(&r.expression)) }),
                r.span,
                addr(&**r),
            ),
            ox::TSModuleReference::IdentifierReference(id) => self.identifier_reference(id),
            ox::TSModuleReference::QualifiedName(q) => self.qualified_name(q),
        };
        self.node(
            NodeKind::TSImportEqualsDeclaration(TSImportEqualsDeclaration {
                id: Box::new(self.binding_identifier(&d.id)),
                module_reference: Box::new(module_reference),
                is_type: d.import_kind.is_type(),
            }),
            d.span,
            addr(d),
        )
    }

    fn qualified_name(&self, q: &ox::TSQualifiedName) -> Node {
        let left = match &q.left {
            ox::TSTypeName::IdentifierReference(id) => self.identifier_reference(id),
            ox::TSTypeName::QualifiedName(q) => self.qualified_name(q),
            ox::TSTypeName::ThisExpression(t) => self.node(NodeKind::ThisExpression, t.span, addr(&**t)),
        };
        self.node(
            NodeKind::TSQualifiedName(TSQualifiedName {
                left: Box::new(left),
                right: Box::new(self.identifier_name(&q.right)),
            }),
            q.span,
            addr(q),
        )
    }

    fn module_export_name(&self, n: &ox::ModuleExportName) -> Node {
        match n {
            ox::ModuleExportName::IdentifierName(id) => self.identifier_name(id),
            ox::ModuleExportName::IdentifierReference(id) => self.identifier_reference(id),
            ox::ModuleExportName::StringLiteral(s) => self.string_literal(s),
        }
    }

    fn variable_declaration(&self, d: &ox::VariableDeclaration) -> Node {
        if d.declare {
            return self.empty(d.span, addr(d));
        }
        let kind = match d.kind {
            ox::VariableDeclarationKind::Var => VariableKind::Var,
            ox::VariableDeclarationKind::Let => VariableKind::Let,
            ox::VariableDeclarationKind::Const => VariableKind::Const,
            ox::VariableDeclarationKind::Using => VariableKind::Using,
            ox::VariableDeclarationKind::AwaitUsing => VariableKind::AwaitUsing,
        };
        let declarations = d
            .declarations
            .iter()
            .map(|d| {
                let mut id = self.binding_pattern(&d.id);
                if let Some(t) = &d.type_annotation {
                    self.extend_span(&mut id, t.span.end);
                }
                self.node(
                    NodeKind::VariableDeclarator(VariableDeclarator {
                        id: Box::new(id),
                        init: d.init.as_ref().map(|e| Box::new(self.expression(e))),
                    }),
                    d.span,
                    addr(d),
                )
            })
            .collect();
        self.node(NodeKind::VariableDeclaration(VariableDeclaration { kind, declarations }), d.span, addr(d))
    }

    /// TS-ESTree spans a binding with a type annotation up to the annotation's end
    fn extend_span(&self, node: &mut Node, end: u32) {
        if !self.ts {
            return;
        }
        if let Some(span) = &mut node.span {
            span.end = end;
            node.loc = Some(SourceLocation { start: node.loc.unwrap().start, end: self.pos(end) });
        }
    }

    fn for_left(&self, l: &ox::ForStatementLeft) -> Node {
        match l {
            ox::ForStatementLeft::VariableDeclaration(d) => self.variable_declaration(d),
            t => self.assignment_target(t.as_assignment_target().unwrap()),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Functions and classes

    /// A `Function` as a declaration or expression. Bodyless functions (overloads, `declare`)
    /// become `EmptyStatement`s.
    fn function(&self, f: &ox::Function, declaration: bool) -> Node {
        let Some(body) = &f.body else {
            return self.empty(f.span, addr(f));
        };
        if f.declare {
            return self.empty(f.span, addr(f));
        }
        let function = Function {
            id: f.id.as_ref().map(|id| Box::new(self.binding_identifier(id))),
            params: self.params(&f.params),
            body: Box::new(self.function_body(body)),
            generator: f.generator,
            is_async: f.r#async,
        };
        let kind = if declaration && f.is_declaration() {
            NodeKind::FunctionDeclaration(function)
        } else {
            NodeKind::FunctionExpression(function)
        };
        self.node(kind, f.span, addr(f))
    }

    fn params(&self, p: &ox::FormalParameters) -> Vec<Node> {
        let mut out = Vec::with_capacity(p.items.len() + p.rest.is_some() as usize);
        for param in &p.items {
            let mut pattern = self.binding_pattern(&param.pattern);
            if let Some(init) = &param.initializer {
                if let Some(t) = &param.type_annotation {
                    self.extend_span(&mut pattern, t.span.end);
                }
                let span = if self.ts && param.pattern.span().start != param.span.start {
                    oxc_span::Span::new(param.pattern.span().start, init.span().end)
                } else {
                    param.span
                };
                out.push(self.node(
                    NodeKind::AssignmentPattern(AssignmentPattern {
                        left: Box::new(pattern),
                        right: Box::new(self.expression(init)),
                    }),
                    span,
                    addr(param) + 1,
                ));
            } else {
                if self.ts {
                    if param.optional {
                        let end = param.type_annotation.as_ref().map_or(param.span.end, |t| t.span.end);
                        let start = pattern.span.unwrap().start;
                        pattern.span = Some(Span::new(start, end));
                        pattern.loc = Some(SourceLocation { start: pattern.loc.unwrap().start, end: self.pos(end) });
                    } else if let Some(t) = &param.type_annotation {
                        self.extend_span(&mut pattern, t.span.end);
                    }
                }
                out.push(pattern);
            }
        }
        if let Some(rest) = &p.rest {
            let mut node = self.rest_element(&rest.rest);
            if let Some(t) = &rest.type_annotation {
                self.extend_span(&mut node, t.span.end);
            }
            out.push(node);
        }
        out
    }

    fn rest_element(&self, r: &ox::BindingRestElement) -> Node {
        self.node(
            NodeKind::RestElement(Argument { argument: Box::new(self.binding_pattern(&r.argument)) }),
            r.span,
            addr(r),
        )
    }

    fn class(&self, c: &ox::Class, declaration: bool) -> Node {
        if c.declare {
            return self.empty(c.span, addr(c));
        }
        let mut body = Vec::with_capacity(c.body.body.len());
        for element in &c.body.body {
            use ox::ClassElement as E;
            match element {
                E::StaticBlock(b) => body.push(self.node(
                    NodeKind::StaticBlock(Body { body: self.statements(&b.body) }),
                    b.span,
                    addr(&**b),
                )),
                E::MethodDefinition(m) => {
                    if m.r#type == ox::MethodDefinitionType::TSAbstractMethodDefinition {
                        body.push(self.empty(m.span, addr(&**m)));
                        continue;
                    }
                    if m.value.body.is_none() {
                        // an overload signature
                        continue;
                    }
                    let kind = match m.kind {
                        ox::MethodDefinitionKind::Constructor => MethodKind::Constructor,
                        ox::MethodDefinitionKind::Method => MethodKind::Method,
                        ox::MethodDefinitionKind::Get => MethodKind::Get,
                        ox::MethodDefinitionKind::Set => MethodKind::Set,
                    };
                    body.push(self.node(
                        NodeKind::MethodDefinition(MethodDefinition {
                            key: Box::new(self.property_key(&m.key)),
                            value: Box::new(self.function(&m.value, false)),
                            kind,
                            computed: m.computed,
                            is_static: m.r#static,
                        }),
                        m.span,
                        addr(&**m),
                    ));
                }
                E::PropertyDefinition(p) => {
                    if p.declare {
                        continue;
                    }
                    body.push(self.node(
                        NodeKind::PropertyDefinition(PropertyDefinition {
                            key: Box::new(self.property_key(&p.key)),
                            value: p.value.as_ref().map(|v| Box::new(self.expression(v))),
                            computed: p.computed,
                            is_static: p.r#static,
                            is_abstract: p.r#type == ox::PropertyDefinitionType::TSAbstractPropertyDefinition,
                        }),
                        p.span,
                        addr(&**p),
                    ));
                }
                // accessor fields are an error in Svelte, index signatures crash it
                E::AccessorProperty(_) | E::TSIndexSignature(_) => {}
            }
        }
        let class = Class {
            id: c.id.as_ref().map(|id| Box::new(self.binding_identifier(id))),
            super_class: c.heritage.as_ref().map(|h| Box::new(self.expression(&h.expression))),
            body: self.boxed(NodeKind::ClassBody(Body { body }), c.body.span, addr(&*c.body)),
        };
        let kind = if declaration && c.is_declaration() {
            NodeKind::ClassDeclaration(class)
        } else {
            NodeKind::ClassExpression(class)
        };
        self.node(kind, c.span, addr(c))
    }

    // ---------------------------------------------------------------------------------------
    // Identifiers, literals, keys

    fn ident(&self, name: &str, span: oxc_span::Span, origin: usize) -> Node {
        self.node(NodeKind::Identifier(Identifier { name: Atom::from(name) }), span, origin)
    }

    pub fn identifier_reference(&self, id: &ox::IdentifierReference) -> Node {
        self.ident(id.name.as_str(), id.span, addr(id))
    }

    pub fn binding_identifier(&self, id: &ox::BindingIdentifier) -> Node {
        self.ident(id.name.as_str(), id.span, addr(id))
    }

    fn identifier_name(&self, id: &ox::IdentifierName) -> Node {
        self.ident(id.name.as_str(), id.span, addr(id))
    }

    fn label(&self, id: &ox::LabelIdentifier) -> Node {
        self.ident(id.name.as_str(), id.span, addr(id))
    }

    fn private_identifier(&self, id: &ox::PrivateIdentifier) -> Node {
        self.node(NodeKind::PrivateIdentifier(PrivateIdentifier { name: Atom::from(id.name.as_str()) }), id.span, addr(id))
    }

    fn string_literal(&self, s: &ox::StringLiteral) -> Node {
        let raw = match &s.raw {
            Some(raw) => Atom::from(raw.as_str()),
            None => Atom::from(self.source(s.span)),
        };
        self.node(
            NodeKind::Literal(Literal { value: LiteralValue::String(Atom::from(s.value.as_str())), raw: Some(raw) }),
            s.span,
            addr(s),
        )
    }

    fn property_key(&self, k: &ox::PropertyKey) -> Node {
        match k {
            ox::PropertyKey::StaticIdentifier(id) => self.identifier_name(id),
            ox::PropertyKey::PrivateIdentifier(id) => self.private_identifier(id),
            e => self.expression(e.as_expression().unwrap()),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Patterns

    pub fn binding_pattern(&self, p: &ox::BindingPattern) -> Node {
        match p {
            ox::BindingPattern::BindingIdentifier(id) => self.binding_identifier(id),
            ox::BindingPattern::ObjectPattern(o) => {
                let mut properties: Vec<Node> = o
                    .properties
                    .iter()
                    .map(|p| {
                        self.node(
                            NodeKind::Property(Property {
                                key: Box::new(self.property_key(&p.key)),
                                value: Box::new(self.binding_pattern(&p.value)),
                                kind: PropertyKind::Init,
                                method: false,
                                shorthand: p.shorthand,
                                computed: p.computed,
                            }),
                            p.span,
                            addr(p),
                        )
                    })
                    .collect();
                if let Some(rest) = &o.rest {
                    properties.push(self.rest_element(rest));
                }
                self.node(NodeKind::ObjectPattern(ObjectExpression { properties }), o.span, addr(&**o))
            }
            ox::BindingPattern::ArrayPattern(a) => {
                let mut elements: Vec<Option<Node>> =
                    a.elements.iter().map(|e| e.as_ref().map(|e| self.binding_pattern(e))).collect();
                if let Some(rest) = &a.rest {
                    elements.push(Some(self.rest_element(rest)));
                }
                self.node(NodeKind::ArrayPattern(ArrayExpression { elements }), a.span, addr(&**a))
            }
            ox::BindingPattern::AssignmentPattern(a) => self.node(
                NodeKind::AssignmentPattern(AssignmentPattern {
                    left: Box::new(self.binding_pattern(&a.left)),
                    right: Box::new(self.expression(&a.right)),
                }),
                a.span,
                addr(&**a),
            ),
        }
    }

    pub fn assignment_target(&self, t: &ox::AssignmentTarget) -> Node {
        use ox::AssignmentTarget as T;
        match t {
            T::ArrayAssignmentTarget(a) => {
                let mut elements: Vec<Option<Node>> =
                    a.elements.iter().map(|e| e.as_ref().map(|e| self.assignment_target_maybe_default(e))).collect();
                if let Some(rest) = &a.rest {
                    elements.push(Some(self.assignment_target_rest(rest)));
                }
                self.node(NodeKind::ArrayPattern(ArrayExpression { elements }), a.span, addr(&**a))
            }
            T::ObjectAssignmentTarget(o) => {
                let mut properties: Vec<Node> = o
                    .properties
                    .iter()
                    .map(|p| match p {
                        ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                            let key = self.identifier_reference(&p.binding);
                            let value = match &p.init {
                                None => self.identifier_reference(&p.binding),
                                Some(init) => self.node(
                                    NodeKind::AssignmentPattern(AssignmentPattern {
                                        left: Box::new(self.identifier_reference(&p.binding)),
                                        right: Box::new(self.expression(init)),
                                    }),
                                    p.span,
                                    addr(&**p) + 1,
                                ),
                            };
                            self.node(
                                NodeKind::Property(Property {
                                    key: Box::new(key),
                                    value: Box::new(value),
                                    kind: PropertyKind::Init,
                                    method: false,
                                    shorthand: true,
                                    computed: false,
                                }),
                                p.span,
                                addr(&**p),
                            )
                        }
                        ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => self.node(
                            NodeKind::Property(Property {
                                key: Box::new(self.property_key(&p.name)),
                                value: Box::new(self.assignment_target_maybe_default(&p.binding)),
                                kind: PropertyKind::Init,
                                method: false,
                                shorthand: false,
                                computed: p.computed,
                            }),
                            p.span,
                            addr(&**p),
                        ),
                    })
                    .collect();
                if let Some(rest) = &o.rest {
                    properties.push(self.assignment_target_rest(rest));
                }
                self.node(NodeKind::ObjectPattern(ObjectExpression { properties }), o.span, addr(&**o))
            }
            t => self.simple_assignment_target(t.as_simple_assignment_target().unwrap()),
        }
    }

    fn simple_assignment_target(&self, t: &ox::SimpleAssignmentTarget) -> Node {
        use ox::SimpleAssignmentTarget as T;
        match t {
            T::AssignmentTargetIdentifier(id) => self.identifier_reference(id),
            T::TSAsExpression(e) => self.expression(&e.expression),
            T::TSSatisfiesExpression(e) => self.expression(&e.expression),
            T::TSNonNullExpression(e) => self.expression(&e.expression),
            T::TSTypeAssertion(e) => self.expression(&e.expression),
            m => self.member_expression(m.as_member_expression().unwrap()),
        }
    }

    fn assignment_target_rest(&self, r: &ox::AssignmentTargetRest) -> Node {
        self.node(
            NodeKind::RestElement(Argument { argument: Box::new(self.assignment_target(&r.target)) }),
            r.span,
            addr(r),
        )
    }

    fn assignment_target_maybe_default(&self, t: &ox::AssignmentTargetMaybeDefault) -> Node {
        match t {
            ox::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => self.node(
                NodeKind::AssignmentPattern(AssignmentPattern {
                    left: Box::new(self.assignment_target(&d.binding)),
                    right: Box::new(self.expression(&d.init)),
                }),
                d.span,
                addr(&**d),
            ),
            t => self.assignment_target(t.as_assignment_target().unwrap()),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Expressions

    fn member_expression(&self, m: &ox::MemberExpression) -> Node {
        match m {
            ox::MemberExpression::ComputedMemberExpression(m) => self.node(
                NodeKind::MemberExpression(MemberExpression {
                    object: Box::new(self.expression(&m.object)),
                    property: Box::new(self.expression(&m.expression)),
                    computed: true,
                    optional: m.optional,
                }),
                m.span,
                addr(&**m),
            ),
            ox::MemberExpression::StaticMemberExpression(m) => self.node(
                NodeKind::MemberExpression(MemberExpression {
                    object: Box::new(self.expression(&m.object)),
                    property: Box::new(self.identifier_name(&m.property)),
                    computed: false,
                    optional: m.optional,
                }),
                m.span,
                addr(&**m),
            ),
            ox::MemberExpression::PrivateFieldExpression(m) => self.node(
                NodeKind::MemberExpression(MemberExpression {
                    object: Box::new(self.expression(&m.object)),
                    property: Box::new(self.private_identifier(&m.field)),
                    computed: false,
                    optional: m.optional,
                }),
                m.span,
                addr(&**m),
            ),
        }
    }

    fn arguments(&self, args: &[ox::Argument]) -> Vec<Node> {
        args.iter()
            .map(|a| match a {
                ox::Argument::SpreadElement(s) => self.spread(s),
                e => self.expression(e.as_expression().unwrap()),
            })
            .collect()
    }

    fn spread(&self, s: &ox::SpreadElement) -> Node {
        self.node(NodeKind::SpreadElement(Argument { argument: Box::new(self.expression(&s.argument)) }), s.span, addr(s))
    }

    fn call(&self, c: &ox::CallExpression) -> Node {
        self.node(
            NodeKind::CallExpression(CallExpression {
                callee: Box::new(self.expression(&c.callee)),
                arguments: self.arguments(&c.arguments),
                optional: c.optional,
            }),
            c.span,
            addr(c),
        )
    }

    fn template_literal(&self, t: &ox::TemplateLiteral) -> Node {
        let quasis = t
            .quasis
            .iter()
            .map(|q| {
                self.node(
                    NodeKind::TemplateElement(TemplateElement {
                        raw: Atom::from(q.value.raw.as_str()),
                        cooked: q.value.cooked.as_ref().map(|c| Atom::from(c.as_str())),
                        tail: q.tail,
                    }),
                    q.span,
                    addr(q),
                )
            })
            .collect();
        self.node(
            NodeKind::TemplateLiteral(TemplateLiteral {
                quasis,
                expressions: t.expressions.iter().map(|e| self.expression(e)).collect(),
            }),
            t.span,
            addr(t),
        )
    }

    /// An expression. TypeScript wrappers (`as`, `satisfies`, `!`, `<T>x`, `x<T>`) become their
    /// expression.
    pub fn expression(&self, e: &ox::Expression) -> Node {
        use ox::Expression as E;
        match e {
            E::BooleanLiteral(l) => self.node(
                NodeKind::Literal(Literal {
                    value: LiteralValue::Boolean(l.value),
                    raw: Some(Atom::from(if l.value { "true" } else { "false" })),
                }),
                l.span,
                addr(&**l),
            ),
            E::NullLiteral(l) => self.node(
                NodeKind::Literal(Literal { value: LiteralValue::Null, raw: Some(Atom::from("null")) }),
                l.span,
                addr(&**l),
            ),
            E::NumericLiteral(l) => {
                let raw = match &l.raw {
                    Some(raw) => Atom::from(raw.as_str()),
                    None => Atom::from(self.source(l.span)),
                };
                self.node(
                    NodeKind::Literal(Literal { value: LiteralValue::Number(l.value), raw: Some(raw) }),
                    l.span,
                    addr(&**l),
                )
            }
            E::BigIntLiteral(l) => {
                let raw = match &l.raw {
                    Some(raw) => Atom::from(raw.as_str()),
                    None => Atom::from(self.source(l.span)),
                };
                self.node(
                    NodeKind::Literal(Literal { value: LiteralValue::BigInt(Atom::from(l.value.as_str())), raw: Some(raw) }),
                    l.span,
                    addr(&**l),
                )
            }
            E::RegExpLiteral(l) => {
                let raw = match &l.raw {
                    Some(raw) => raw.as_str(),
                    None => self.source(l.span),
                };
                let slash = raw.rfind('/').unwrap_or(raw.len());
                let pattern = raw.get(1..slash).unwrap_or("");
                let flags = raw.get(slash + 1..).unwrap_or("");
                self.node(
                    NodeKind::Literal(Literal {
                        value: LiteralValue::RegExp(Box::new(RegExpValue { pattern: Atom::from(pattern), flags: Atom::from(flags) })),
                        raw: Some(Atom::from(raw)),
                    }),
                    l.span,
                    addr(&**l),
                )
            }
            E::StringLiteral(s) => self.string_literal(s),
            E::TemplateLiteral(t) => self.template_literal(t),
            E::Identifier(id) => self.identifier_reference(id),
            E::Super(s) => self.node(NodeKind::Super, s.span, addr(&**s)),
            E::ArrayExpression(a) => {
                let elements = a
                    .elements
                    .iter()
                    .map(|e| match e {
                        ox::ArrayExpressionElement::SpreadElement(s) => Some(self.spread(s)),
                        ox::ArrayExpressionElement::Elision(_) => None,
                        e => Some(self.expression(e.as_expression().unwrap())),
                    })
                    .collect();
                self.node(NodeKind::ArrayExpression(ArrayExpression { elements }), a.span, addr(&**a))
            }
            E::ArrowFunctionExpression(a) => {
                let (body, expression) = match &a.body {
                    ox::ArrowFunctionBody::FunctionBody(b) => (self.function_body(b), false),
                    e => (self.expression(e.as_expression().unwrap()), true),
                };
                self.node(
                    NodeKind::ArrowFunctionExpression(ArrowFunctionExpression {
                        params: self.params(&a.params),
                        body: Box::new(body),
                        expression,
                        is_async: a.r#async,
                    }),
                    a.span,
                    addr(&**a),
                )
            }
            E::AssignmentExpression(a) => self.node(
                NodeKind::AssignmentExpression(AssignmentExpression {
                    operator: a.operator,
                    left: Box::new(self.assignment_target(&a.left)),
                    right: Box::new(self.expression(&a.right)),
                }),
                a.span,
                addr(&**a),
            ),
            E::AwaitExpression(a) => self.node(
                NodeKind::AwaitExpression(Argument { argument: Box::new(self.expression(&a.argument)) }),
                a.span,
                addr(&**a),
            ),
            E::BinaryExpression(b) => self.node(
                NodeKind::BinaryExpression(BinaryExpression {
                    operator: b.operator,
                    left: Box::new(self.expression(&b.left)),
                    right: Box::new(self.expression(&b.right)),
                }),
                b.span,
                addr(&**b),
            ),
            E::PrivateInExpression(p) => self.node(
                NodeKind::BinaryExpression(BinaryExpression {
                    operator: BinaryOperator::In,
                    left: Box::new(self.private_identifier(&p.left)),
                    right: Box::new(self.expression(&p.right)),
                }),
                p.span,
                addr(&**p),
            ),
            E::CallExpression(c) => self.call(c),
            E::ChainExpression(c) => {
                let expression = match &c.expression {
                    ox::ChainElement::CallExpression(call) => self.call(call),
                    ox::ChainElement::TSNonNullExpression(e) => self.expression(&e.expression),
                    m => self.member_expression(m.as_member_expression().unwrap()),
                };
                self.node(NodeKind::ChainExpression(ExpressionWrapper { expression: Box::new(expression) }), c.span, addr(&**c))
            }
            E::ClassExpression(c) => self.class(c, false),
            E::ConditionalExpression(c) => self.node(
                NodeKind::ConditionalExpression(ConditionalExpression {
                    test: Box::new(self.expression(&c.test)),
                    consequent: Box::new(self.expression(&c.consequent)),
                    alternate: Box::new(self.expression(&c.alternate)),
                }),
                c.span,
                addr(&**c),
            ),
            E::FunctionExpression(f) => self.function(f, false),
            E::ImportExpression(i) => self.node(
                NodeKind::ImportExpression(ImportExpression {
                    source: Box::new(self.expression(&i.source)),
                    options: i.options.as_ref().map(|o| Box::new(self.expression(o))),
                }),
                i.span,
                addr(&**i),
            ),
            E::LogicalExpression(l) => self.node(
                NodeKind::LogicalExpression(LogicalExpression {
                    operator: l.operator,
                    left: Box::new(self.expression(&l.left)),
                    right: Box::new(self.expression(&l.right)),
                }),
                l.span,
                addr(&**l),
            ),
            E::NewExpression(n) => self.node(
                NodeKind::NewExpression(NewExpression {
                    callee: Box::new(self.expression(&n.callee)),
                    arguments: self.arguments(&n.arguments),
                }),
                n.span,
                addr(&**n),
            ),
            E::ObjectExpression(o) => {
                let properties = o
                    .properties
                    .iter()
                    .map(|p| match p {
                        ox::ObjectPropertyKind::SpreadProperty(s) => self.spread(s),
                        ox::ObjectPropertyKind::ObjectProperty(p) => self.node(
                            NodeKind::Property(Property {
                                key: Box::new(self.property_key(&p.key)),
                                value: Box::new(self.expression(&p.value)),
                                kind: match p.kind {
                                    ox::PropertyKind::Init => PropertyKind::Init,
                                    ox::PropertyKind::Get => PropertyKind::Get,
                                    ox::PropertyKind::Set => PropertyKind::Set,
                                },
                                method: p.method,
                                shorthand: p.shorthand,
                                computed: p.computed,
                            }),
                            p.span,
                            addr(&**p),
                        ),
                    })
                    .collect();
                self.node(NodeKind::ObjectExpression(ObjectExpression { properties }), o.span, addr(&**o))
            }
            E::ParenthesizedExpression(p) => {
                if self.preserve_parens {
                    self.node(
                        NodeKind::ParenthesizedExpression(ExpressionWrapper { expression: Box::new(self.expression(&p.expression)) }),
                        p.span,
                        addr(&**p),
                    )
                } else {
                    self.expression(&p.expression)
                }
            }
            E::SequenceExpression(s) => self.node(
                NodeKind::SequenceExpression(SequenceExpression {
                    expressions: s.expressions.iter().map(|e| self.expression(e)).collect(),
                }),
                s.span,
                addr(&**s),
            ),
            E::TaggedTemplateExpression(t) => self.node(
                NodeKind::TaggedTemplateExpression(TaggedTemplateExpression {
                    tag: Box::new(self.expression(&t.tag)),
                    quasi: Box::new(self.template_literal(&t.quasi)),
                }),
                t.span,
                addr(&**t),
            ),
            E::ThisExpression(t) => self.node(NodeKind::ThisExpression, t.span, addr(&**t)),
            E::UnaryExpression(u) => self.node(
                NodeKind::UnaryExpression(UnaryExpression {
                    operator: u.operator,
                    argument: Box::new(self.expression(&u.argument)),
                }),
                u.span,
                addr(&**u),
            ),
            E::UpdateExpression(u) => self.node(
                NodeKind::UpdateExpression(UpdateExpression {
                    operator: u.operator,
                    prefix: u.prefix,
                    argument: Box::new(self.simple_assignment_target(&u.argument)),
                }),
                u.span,
                addr(&**u),
            ),
            E::YieldExpression(y) => self.node(
                NodeKind::YieldExpression(YieldExpression {
                    argument: y.argument.as_ref().map(|e| Box::new(self.expression(e))),
                    delegate: y.delegate,
                }),
                y.span,
                addr(&**y),
            ),
            E::ImportMeta(m) => {
                let a = addr(&**m);
                let meta = self.ident("import", oxc_span::Span::new(m.span.start, m.span.start + 6), a + 1);
                let property = self.ident("meta", oxc_span::Span::new(m.span.end - 4, m.span.end), a + 2);
                self.node(NodeKind::MetaProperty(MetaProperty { meta: Box::new(meta), property: Box::new(property) }), m.span, a)
            }
            E::NewTarget(m) => {
                let a = addr(&**m);
                let meta = self.ident("new", oxc_span::Span::new(m.span.start, m.span.start + 3), a + 1);
                let property = self.ident("target", oxc_span::Span::new(m.span.end - 6, m.span.end), a + 2);
                self.node(NodeKind::MetaProperty(MetaProperty { meta: Box::new(meta), property: Box::new(property) }), m.span, a)
            }
            E::TSAsExpression(e) => self.expression(&e.expression),
            E::TSSatisfiesExpression(e) => self.expression(&e.expression),
            E::TSTypeAssertion(e) => self.expression(&e.expression),
            E::TSNonNullExpression(e) => self.expression(&e.expression),
            E::TSInstantiationExpression(e) => self.expression(&e.expression),
            E::JSXElement(_) | E::JSXFragment(_) => panic!("JSX can't be converted to Svelte's ESTree"),
            E::V8IntrinsicExpression(_) => panic!("V8 intrinsics can't be converted to Svelte's ESTree"),
            m => self.member_expression(m.as_member_expression().unwrap()),
        }
    }
}
