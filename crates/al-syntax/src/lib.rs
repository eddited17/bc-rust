//! AL (Business Central) syntax: lexer, preprocessor, parser and AST.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use ast::*;
pub use lexer::{lex, Tk, Token};
pub use parser::{parse_expr, parse_type, ParseError, Parser};

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

/// Lex (with preprocessing against `symbols`) and parse one AL source file.
pub fn parse_file(src: &str, symbols: &HashSet<String>) -> Result<SourceFile, SyntaxError> {
    let toks = lex(src, symbols).map_err(|e| SyntaxError { msg: e.msg, line: e.line, col: e.col })?;
    Parser::new(&toks).file().map_err(|e| SyntaxError { msg: e.msg, line: e.line, col: e.col })
}
