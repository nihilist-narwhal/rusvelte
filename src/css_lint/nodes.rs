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

pub struct NodeData {
    pub class: Class,
    pub ty: NodeType,
    pub offset: i32,
    pub length: i32,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub fields: Vec<(Field, NodeId)>,
    pub issues: Vec<Issue>,
    /// `Declaration.colonPosition`
    pub colon_position: Option<i32>,
    /// `VariableDeclaration.needsSemicolon`
    pub needs_semicolon: bool,
}

impl NodeData {
    pub fn end(&self) -> i32 {
        self.offset + self.length
    }
}

pub struct Ast {
    pub nodes: Vec<NodeData>,
}

impl Ast {
    pub fn new() -> Self {
        Ast { nodes: Vec::with_capacity(256) }
    }

    pub fn alloc(&mut self, class: Class, ty: NodeType, offset: i32, length: i32) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(NodeData {
            class,
            ty: class.fixed_type().unwrap_or(ty),
            offset,
            length,
            parent: None,
            children: Vec::new(),
            fields: Vec::new(),
            issues: Vec::new(),
            colon_position: None,
            needs_semicolon: true,
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

    /// `new Nodelist(parent)`
    pub fn new_nodelist(&mut self, parent: NodeId) -> NodeId {
        let id = self.alloc(Class::Nodelist, NodeType::Undefined, -1, -1);
        self.adopt_child(parent, id, -1);
        let n = self.get_mut(id);
        n.offset = -1;
        n.length = -1;
        id
    }

    /// `parent.adoptChild(node, index)`
    pub fn adopt_child(&mut self, parent: NodeId, node: NodeId, index: i32) {
        if let Some(old) = self.get(node).parent {
            let siblings = &mut self.get_mut(old).children;
            if let Some(i) = siblings.iter().position(|&c| c == node) {
                siblings.remove(i);
            }
        }
        self.get_mut(node).parent = Some(parent);
        let children = &mut self.get_mut(parent).children;
        if index != -1 {
            // `splice(index, 0, node)` (index is always 0 or 1 here)
            let i = (index.max(0) as usize).min(children.len());
            children.insert(i, node);
        } else {
            children.push(node);
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
        let fields = &mut self.get_mut(node).fields;
        if let Some(slot) = fields.iter_mut().find(|(f, _)| *f == field) {
            slot.1 = child;
        } else {
            fields.push((field, child));
        }
    }

    pub fn field(&self, node: NodeId, field: Field) -> Option<NodeId> {
        self.get(node).fields.iter().find(|(f, _)| *f == field).map(|&(_, c)| c)
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
    pub fn child(&self, node: NodeId, index: usize) -> Option<NodeId> {
        self.get(node).children.get(index).copied()
    }

    pub fn has_children(&self, node: NodeId) -> bool {
        !self.get(node).children.is_empty()
    }

    /// `node.isErroneous(recursive)`
    pub fn is_erroneous(&self, node: NodeId, recursive: bool) -> bool {
        let n = self.get(node);
        if !n.issues.is_empty() {
            return true;
        }
        recursive && n.children.iter().any(|&c| self.is_erroneous(c, true))
    }

    /// `node.getParent()`: the parent, skipping `Nodelist`s
    pub fn get_parent(&self, node: NodeId) -> Option<NodeId> {
        let mut result = self.get(node).parent;
        while let Some(r) = result {
            if self.class(r) != Class::Nodelist {
                break;
            }
            result = self.get(r).parent;
        }
        result
    }
}
