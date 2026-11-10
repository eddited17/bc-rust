//! AST for AL source files.
//!
//! Object bodies use a generic member tree (properties, nested elements,
//! triggers, procedures, var sections) because AL's declarative sections
//! (fields, keys, layout, actions, dataset, ...) all share one block shape.
//! Code (triggers/procedures) is parsed into a full statement/expression AST.

use crate::lexer::Token;

#[derive(Debug, Clone, Default)]
pub struct SourceFile {
    pub namespace: Option<String>,
    pub usings: Vec<String>,
    pub objects: Vec<Object>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    TableExtension,
    Page,
    PageExtension,
    PageCustomization,
    Codeunit,
    Report,
    ReportExtension,
    Query,
    XmlPort,
    Enum,
    EnumExtension,
    Interface,
    PermissionSet,
    PermissionSetExtension,
    Entitlement,
    Profile,
    ProfileExtension,
    ControlAddIn,
    DotNet,
}

impl ObjectKind {
    pub fn from_kw(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "table" => Self::Table,
            "tableextension" => Self::TableExtension,
            "page" => Self::Page,
            "pageextension" => Self::PageExtension,
            "pagecustomization" => Self::PageCustomization,
            "codeunit" => Self::Codeunit,
            "report" => Self::Report,
            "reportextension" => Self::ReportExtension,
            "query" => Self::Query,
            "xmlport" => Self::XmlPort,
            "enum" => Self::Enum,
            "enumextension" => Self::EnumExtension,
            "interface" => Self::Interface,
            "permissionset" => Self::PermissionSet,
            "permissionsetextension" => Self::PermissionSetExtension,
            "entitlement" => Self::Entitlement,
            "profile" => Self::Profile,
            "profileextension" => Self::ProfileExtension,
            "controladdin" => Self::ControlAddIn,
            "dotnet" => Self::DotNet,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::TableExtension => "tableextension",
            Self::Page => "page",
            Self::PageExtension => "pageextension",
            Self::PageCustomization => "pagecustomization",
            Self::Codeunit => "codeunit",
            Self::Report => "report",
            Self::ReportExtension => "reportextension",
            Self::Query => "query",
            Self::XmlPort => "xmlport",
            Self::Enum => "enum",
            Self::EnumExtension => "enumextension",
            Self::Interface => "interface",
            Self::PermissionSet => "permissionset",
            Self::PermissionSetExtension => "permissionsetextension",
            Self::Entitlement => "entitlement",
            Self::Profile => "profile",
            Self::ProfileExtension => "profileextension",
            Self::ControlAddIn => "controladdin",
            Self::DotNet => "dotnet",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Object {
    pub kind: ObjectKind,
    pub id: Option<i64>,
    pub name: String,
    pub extends: Vec<String>,
    pub implements: Vec<String>,
    pub customizes: Option<String>,
    pub body: Body,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Body {
    pub members: Vec<Member>,
}

#[derive(Debug, Clone)]
pub enum Member {
    Property(Property),
    Element(Element),
    Procedure(Procedure),
    Vars { protected: bool, vars: Vec<VarDecl> },
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    /// Raw value tokens (interpreted on demand by the model layer).
    pub value: Vec<Token>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Element {
    /// Lower-cased element keyword: `field`, `key`, `group`, `action`, `fields`, ...
    pub kind: String,
    /// Arguments split at `;`, as raw tokens.
    pub args: Vec<Vec<Token>>,
    pub body: Body,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Local,
    Internal,
    Protected,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct Procedure {
    pub name: String,
    pub kind: ProcKind,
    pub access: Access,
    pub attributes: Vec<Attribute>,
    pub params: Vec<Param>,
    pub ret_name: Option<String>,
    pub ret: Option<TypeRef>,
    pub vars: Vec<VarDecl>,
    /// `None` for declarations without a body (interfaces, control add-ins).
    pub body: Option<Vec<Stmt>>,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
    Procedure,
    Trigger,
    Event,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub by_ref: bool,
    pub ty: TypeRef,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub ty: TypeRef,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub enum TypeRef {
    /// Built-in type: `Integer`, `Decimal`, `Code[20]`, `Text`, `Boolean`, ...
    Simple { name: String, len: Option<u32> },
    /// Object-typed: `Record Customer temporary`, `Codeunit "Sales-Post"`, `Enum X`, ...
    Object { kind: String, target: String, temporary: bool },
    Array { dims: Vec<u32>, elem: Box<TypeRef> },
    List(Box<TypeRef>),
    Dictionary(Box<TypeRef>, Box<TypeRef>),
    Option(Vec<String>),
    Label { text: String, locked: bool, max_len: Option<u32> },
    TextConst(String),
}

impl TypeRef {
    pub fn base_name(&self) -> String {
        match self {
            TypeRef::Simple { name, .. } => name.to_ascii_lowercase(),
            TypeRef::Object { kind, .. } => kind.clone(),
            TypeRef::Array { .. } => "array".into(),
            TypeRef::List(_) => "list".into(),
            TypeRef::Dictionary(..) => "dictionary".into(),
            TypeRef::Option(_) => "option".into(),
            TypeRef::Label { .. } => "label".into(),
            TypeRef::TextConst(_) => "textconst".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone)]
pub struct CaseBranch {
    pub values: Vec<Expr>,
    pub body: Stmt,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Empty,
    Block(Vec<Stmt>),
    Assign { target: Expr, op: AssignOp, value: Expr },
    Expr(Expr),
    If { cond: Expr, then: Box<Stmt>, els: Option<Box<Stmt>> },
    Case { subject: Expr, branches: Vec<CaseBranch>, els: Option<Vec<Stmt>> },
    While { cond: Expr, body: Box<Stmt> },
    Repeat { body: Vec<Stmt>, until: Expr },
    For { var: Expr, from: Expr, to: Expr, down: bool, body: Box<Stmt> },
    ForEach { var: Expr, iter: Expr, body: Box<Stmt> },
    With { rec: Expr, body: Box<Stmt> },
    Exit(Option<Expr>),
    Break,
    Continue,
    AssertError(Box<Stmt>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    And,
    Or,
    Xor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    Plus,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    BigInt(i64),
    Dec(Box<str>),
    Str(Box<str>),
    Bool(bool),
    Date(Box<str>),
    Time(Box<str>),
    DateTime(Box<str>),
    Ident(Box<str>),
    This,
    Member(Box<Expr>, Box<str>),
    Scope(Box<Expr>, Box<str>),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Range(Option<Box<Expr>>, Option<Box<Expr>>),
    Set(Vec<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `x is "Interface"`
    Is(Box<Expr>, Box<str>),
    /// `x as "Interface"`
    As(Box<Expr>, Box<str>),
}
