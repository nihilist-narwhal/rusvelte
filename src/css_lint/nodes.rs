//! The AST of vscode-css-languageservice (`parser/cssNodes.ts`) as an arena. Nodes keep the JS
//! class they were created with (for `instanceof` checks and the fixed `type` getters), their
//! children in order (re-parenting removes a node from its old parent like `adoptChild`), the
//! named fields the parser sets via `setNode`, and the parse issues attached to them.

use super::parser::ParseError;

pub type NodeId = u32;

/// `NodeType` (complete, like the JS enum)
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeType {
    Undefined,
    Identifier,
    Stylesheet,
    Ruleset,
    Selector,
    SimpleSelector,
    SelectorInterpolation,
    SelectorCombinator,
    SelectorCombinatorParent,
    SelectorCombinatorSibling,
    SelectorCombinatorAllSiblings,
    SelectorCombinatorShadowPiercingDescendant,
    Page,
    PageBoxMarginBox,
    ClassSelector,
    IdentifierSelector,
    ElementNameSelector,
    PseudoSelector,
    AttributeSelector,
    Declaration,
    Declarations,
    Property,
    Expression,
    BinaryExpression,
    Term,
    Operator,
    Value,
    StringLiteral,
    URILiteral,
    EscapedValue,
    Function,
    NumericValue,
    HexColorValue,
    RatioValue,
    MixinDeclaration,
    MixinReference,
    VariableName,
    VariableDeclaration,
    Prio,
    Interpolation,
    NestedProperties,
    ExtendsReference,
    SelectorPlaceholder,
    Debug,
    If,
    Else,
    For,
    Each,
    While,
    MixinContentReference,
    MixinContentDeclaration,
    Media,
    Scope,
    Keyframe,
    FontFace,
    Import,
    Namespace,
    Invocation,
    FunctionDeclaration,
    ReturnStatement,
    MediaQuery,
    MediaCondition,
    MediaFeature,
    FunctionParameter,
    FunctionArgument,
    KeyframeSelector,
    ViewPort,
    Document,
    AtApplyRule,
    CustomPropertyDeclaration,
    CustomPropertySet,
    ListEntry,
    Supports,
    SupportsCondition,
    NamespacePrefix,
    GridLine,
    Plugin,
    UnknownAtRule,
    Use,
    ModuleConfiguration,
    Forward,
    ForwardVisibility,
    Module,
    UnicodeRange,
    Layer,
    LayerNameList,
    LayerName,
    PropertyAtRule,
    Container,
    ModuleConfig,
    SelectorList,
    StartingStyleAtRule,
}

/// The JS class a node was constructed with.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Node,
    Nodelist,
    UnicodeRange,
    Identifier,
    Stylesheet,
    Declarations,
    BodyDeclaration,
    RuleSet,
    Selector,
    SimpleSelector,
    AbstractDeclaration,
    CustomPropertySet,
    Declaration,
    CustomPropertyDeclaration,
    Property,
    Invocation,
    Function,
    FunctionParameter,
    FunctionArgument,
    IfStatement,
    ForStatement,
    EachStatement,
    WhileStatement,
    ElseStatement,
    FunctionDeclaration,
    ViewPort,
    FontFace,
    NestedProperties,
    Keyframe,
    KeyframeSelector,
    Import,
    Use,
    ModuleConfiguration,
    Forward,
    ForwardVisibility,
    Namespace,
    Media,
    Scope,
    ScopeLimits,
    Supports,
    Layer,
    PropertyAtRule,
    StartingStyleAtRule,
    Document,
    Container,
    Medialist,
    MediaQuery,
    MediaCondition,
    MediaFeature,
    SupportsCondition,
    Page,
    PageBoxMarginBox,
    Expression,
    BinaryExpression,
    Term,
    AttributeSelector,
    HexColorValue,
    RatioValue,
    NumericValue,
    VariableDeclaration,
    Interpolation,
    Variable,
    ExtendsReference,
    MixinContentReference,
    MixinContentDeclaration,
    MixinReference,
    MixinDeclaration,
    UnknownAtRule,
    ListEntry,
    LessGuard,
    GuardCondition,
    Module,
}

impl Class {
    /// The `type` getter a class overrides, if any
    fn fixed_type(self) -> Option<NodeType> {
        use Class as C;
        use NodeType as T;
        Some(match self {
            C::UnicodeRange => T::UnicodeRange,
            C::Identifier => T::Identifier,
            C::Stylesheet => T::Stylesheet,
            C::Declarations => T::Declarations,
            C::RuleSet => T::Ruleset,
            C::Selector => T::Selector,
            C::SimpleSelector => T::SimpleSelector,
            C::CustomPropertySet => T::CustomPropertySet,
            C::Declaration => T::Declaration,
            C::CustomPropertyDeclaration => T::CustomPropertyDeclaration,
            C::Property => T::Property,
            C::Invocation => T::Invocation,
            C::Function => T::Function,
            C::FunctionParameter => T::FunctionParameter,
            C::FunctionArgument => T::FunctionArgument,
            C::IfStatement => T::If,
            C::ForStatement => T::For,
            C::EachStatement => T::Each,
            C::WhileStatement => T::While,
            C::ElseStatement => T::Else,
            C::FunctionDeclaration => T::FunctionDeclaration,
            C::ViewPort => T::ViewPort,
            C::FontFace => T::FontFace,
            C::NestedProperties => T::NestedProperties,
            C::Keyframe => T::Keyframe,
            C::KeyframeSelector => T::KeyframeSelector,
            C::Import => T::Import,
            C::Use => T::Use,
            C::ModuleConfiguration => T::ModuleConfiguration,
            C::Forward => T::Forward,
            C::ForwardVisibility => T::ForwardVisibility,
            C::Namespace => T::Namespace,
            C::Media => T::Media,
            C::Scope | C::ScopeLimits => T::Scope,
            C::Supports => T::Supports,
            C::Layer => T::Layer,
            C::PropertyAtRule => T::PropertyAtRule,
            C::StartingStyleAtRule => T::StartingStyleAtRule,
            C::Document => T::Document,
            C::Container => T::Container,
            C::MediaQuery => T::MediaQuery,
            C::MediaCondition => T::MediaCondition,
            C::MediaFeature => T::MediaFeature,
            C::SupportsCondition => T::SupportsCondition,
            C::Page => T::Page,
            C::PageBoxMarginBox => T::PageBoxMarginBox,
            C::Expression => T::Expression,
            C::BinaryExpression => T::BinaryExpression,
            C::Term => T::Term,
            C::AttributeSelector => T::AttributeSelector,
            C::HexColorValue => T::HexColorValue,
            C::RatioValue => T::RatioValue,
            C::NumericValue => T::NumericValue,
            C::VariableDeclaration => T::VariableDeclaration,
            C::Interpolation => T::Interpolation,
            C::Variable => T::VariableName,
            C::ExtendsReference => T::ExtendsReference,
            C::MixinContentReference => T::MixinContentReference,
            C::MixinContentDeclaration => T::MixinContentDeclaration,
            C::MixinReference => T::MixinReference,
            C::MixinDeclaration => T::MixinDeclaration,
            C::UnknownAtRule => T::UnknownAtRule,
            C::ListEntry => T::ListEntry,
            C::Module => T::Module,
            C::Node
            | C::Nodelist
            | C::BodyDeclaration
            | C::AbstractDeclaration
            | C::Medialist
            | C::LessGuard
            | C::GuardCondition => return None,
        })
    }

    /// `instanceof Declaration`
    pub fn is_declaration(self) -> bool {
        matches!(self, Class::Declaration | Class::CustomPropertyDeclaration)
    }
}

/// Named node references set with `setNode(field, node)` (or the equivalent setters)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Selectors,
    Declarations,
    Property,
    Value,
    NestedProperties,
    PropertySet,
    Identifier,
    Arguments,
    Parameters,
    Variables,
    Keyword,
    Expression,
    ElseClause,
    Variable,
    DefaultValue,
    Left,
    Right,
    Operator,
    NamespacePrefix,
    ScopeStart,
    ScopeEnd,
    Names,
    Name,
    Content,
    Namespaces,
    Key,
    Conditions,
    Guard,
}


/// A parse error marker (`Marker` with `Level.Error`)
#[derive(Clone, Copy, Debug)]
pub struct Issue {
    pub error: ParseError,
    pub offset: i32,
    pub length: i32,
}

const NONE: NodeId = NodeId::MAX;
const MAX_FIELDS: usize = 4;

fn opt(id: NodeId) -> Option<NodeId> {
    if id == NONE { None } else { Some(id) }
}

/// A node. Children are an intrusive doubly linked list (no allocation per node).
pub struct NodeData {
    pub class: Class,
    pub ty: NodeType,
    n_fields: u8,
    has_issues: bool,
    /// `VariableDeclaration.needsSemicolon`
    pub needs_semicolon: bool,
    pub offset: i32,
    pub length: i32,
    parent: NodeId,
    first_child: NodeId,
    last_child: NodeId,
    next: NodeId,
    prev: NodeId,
    field_names: [Field; MAX_FIELDS],
    field_values: [NodeId; MAX_FIELDS],
    /// `Declaration.colonPosition`
    pub colon_position: Option<i32>,
}

impl NodeData {
    pub fn end(&self) -> i32 {
        self.offset + self.length
    }

    pub fn parent(&self) -> Option<NodeId> {
        opt(self.parent)
    }
}

pub struct Ast {
    nodes: Vec<NodeData>,
    /// issues in the order they were added (`node.issues` of all nodes)
    issues: Vec<(NodeId, Issue)>,
}

pub struct Children<'a> {
    ast: &'a Ast,
    next: NodeId,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = opt(self.next)?;
        self.next = self.ast.get(cur).next;
        Some(cur)
    }
}

type Arena = (Vec<NodeData>, Vec<(NodeId, Issue)>);

thread_local! {
    /// the arena of the previous parse on this thread, reused to avoid reallocating
    static CACHE: std::cell::RefCell<Arena> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

impl Ast {
    /// An empty AST, reusing this thread's cached arena
    pub fn new() -> Self {
        let (mut nodes, mut issues) = CACHE.with(|c| std::mem::take(&mut *c.borrow_mut()));
        nodes.clear();
        issues.clear();
        Ast { nodes, issues }
    }

    /// Give the arena back for the next parse on this thread
    pub fn recycle(mut self) {
        if self.nodes.capacity() > 1 << 20 {
            return;
        }
        self.nodes.clear();
        self.issues.clear();
        CACHE.with(|c| *c.borrow_mut() = (self.nodes, self.issues));
    }

    pub fn alloc(&mut self, class: Class, ty: NodeType, offset: i32, length: i32) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(NodeData {
            class,
            ty: class.fixed_type().unwrap_or(ty),
            n_fields: 0,
            has_issues: false,
            needs_semicolon: true,
            offset,
            length,
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            next: NONE,
            prev: NONE,
            field_names: [Field::Selectors; MAX_FIELDS],
            field_values: [NONE; MAX_FIELDS],
            colon_position: None,
        });
        id
    }

    #[inline]
    pub fn get(&self, id: NodeId) -> &NodeData {
        &self.nodes[id as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id as usize]
    }

    pub fn ty(&self, id: NodeId) -> NodeType {
        self.get(id).ty
    }

    pub fn class(&self, id: NodeId) -> Class {
        self.get(id).class
    }

    pub fn children(&self, node: NodeId) -> Children<'_> {
        Children { ast: self, next: self.get(node).first_child }
    }

    pub fn child_count(&self, node: NodeId) -> usize {
        self.children(node).count()
    }

    pub fn add_issue(&mut self, node: NodeId, issue: Issue) {
        self.get_mut(node).has_issues = true;
        self.issues.push((node, issue));
    }

    /// every `node.issues` entry, in the order they were added
    pub fn all_issues(&self) -> &[(NodeId, Issue)] {
        &self.issues
    }

    /// The child indices from `root` down to `node`, or `None` if `node` isn't in that tree
    pub fn tree_path(&self, root: NodeId, node: NodeId) -> Option<Vec<u32>> {
        let mut path = Vec::new();
        let mut cur = node;
        while cur != root {
            let n = self.get(cur);
            if n.parent == NONE {
                return None;
            }
            let mut index = 0;
            let mut prev = n.prev;
            while prev != NONE {
                index += 1;
                prev = self.get(prev).prev;
            }
            path.push(index);
            cur = n.parent;
        }
        path.reverse();
        Some(path)
    }

    /// `new Nodelist(parent)`
    pub fn new_nodelist(&mut self, parent: NodeId) -> NodeId {
        let id = self.alloc(Class::Nodelist, NodeType::Undefined, -1, -1);
        self.adopt_child(parent, id, -1);
        let n = self.get_mut(id);
        n.offset = -1;
        n.length = -1;
        id
    }

    fn unlink(&mut self, node: NodeId) {
        let (parent, prev, next) = {
            let n = self.get(node);
            (n.parent, n.prev, n.next)
        };
        if parent == NONE {
            return;
        }
        if prev == NONE {
            self.get_mut(parent).first_child = next;
        } else {
            self.get_mut(prev).next = next;
        }
        if next == NONE {
            self.get_mut(parent).last_child = prev;
        } else {
            self.get_mut(next).prev = prev;
        }
        let n = self.get_mut(node);
        n.prev = NONE;
        n.next = NONE;
    }

    /// `parent.adoptChild(node, index)`
    pub fn adopt_child(&mut self, parent: NodeId, node: NodeId, index: i32) {
        self.unlink(node);
        self.get_mut(node).parent = parent;
        // `splice(index, 0, node)` for index >= 0 (clamped to the length), `push` for -1
        let mut before = NONE;
        if index >= 0 {
            before = self.get(parent).first_child;
            for _ in 0..index {
                if before == NONE {
                    break;
                }
                before = self.get(before).next;
            }
        }
        if before == NONE {
            let last = self.get(parent).last_child;
            {
                let n = self.get_mut(node);
                n.prev = last;
                n.next = NONE;
            }
            if last == NONE {
                self.get_mut(parent).first_child = node;
            } else {
                self.get_mut(last).next = node;
            }
            self.get_mut(parent).last_child = node;
        } else {
            let prev = self.get(before).prev;
            {
                let n = self.get_mut(node);
                n.prev = prev;
                n.next = before;
            }
            self.get_mut(before).prev = node;
            if prev == NONE {
                self.get_mut(parent).first_child = node;
            } else {
                self.get_mut(prev).next = node;
            }
        }
    }

    /// `node.setNode(field, child, index)`
    pub fn set_node(&mut self, node: NodeId, field: Field, child: Option<NodeId>, index: i32) -> bool {
        let Some(child) = child else { return false };
        self.adopt_child(node, child, index);
        self.set_field(node, field, child);
        true
    }

    /// Assign `node[field] = child` without attaching
    pub fn set_field(&mut self, node: NodeId, field: Field, child: NodeId) {
        let n = self.get_mut(node);
        let len = n.n_fields as usize;
        if let Some(i) = n.field_names[..len].iter().position(|&f| f == field) {
            n.field_values[i] = child;
        } else {
            n.field_names[len] = field;
            n.field_values[len] = child;
            n.n_fields += 1;
        }
    }

    pub fn field(&self, node: NodeId, field: Field) -> Option<NodeId> {
        let n = self.get(node);
        n.field_names[..n.n_fields as usize].iter().position(|&f| f == field).map(|i| n.field_values[i])
    }

    /// `node.addChild(child)`
    pub fn add_child(&mut self, node: NodeId, child: Option<NodeId>) -> bool {
        let Some(child) = child else { return false };
        self.adopt_child(node, child, -1);
        self.update_offset_and_length(node, child);
        true
    }

    fn update_offset_and_length(&mut self, node: NodeId, child: NodeId) {
        let (c_off, c_end) = {
            let c = self.get(child);
            (c.offset, c.end())
        };
        let n = self.get_mut(node);
        if c_off < n.offset || n.offset == -1 {
            n.offset = c_off;
        }
        if c_end > n.end() || n.length == -1 {
            n.length = c_end - n.offset;
        }
    }

    /// `node.getChild(0)`
    pub fn first_child(&self, node: NodeId) -> Option<NodeId> {
        opt(self.get(node).first_child)
    }

    pub fn has_children(&self, node: NodeId) -> bool {
        self.get(node).first_child != NONE
    }

    /// `node.isErroneous(recursive)`
    pub fn is_erroneous(&self, node: NodeId, recursive: bool) -> bool {
        if self.get(node).has_issues {
            return true;
        }
        recursive && self.children(node).any(|c| self.is_erroneous(c, true))
    }

    /// `node.getParent()`: the parent, skipping `Nodelist`s
    pub fn get_parent(&self, node: NodeId) -> Option<NodeId> {
        let mut result = self.get(node).parent();
        while let Some(r) = result {
            if self.class(r) != Class::Nodelist {
                break;
            }
            result = self.get(r).parent();
        }
        result
    }
}
