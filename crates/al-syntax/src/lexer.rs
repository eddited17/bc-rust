//! AL lexer with an integrated preprocessor (`#if`/`#elif`/`#else`/`#endif`,
//! `#define`/`#undef`; `#pragma` and `#region` are dropped).

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tk {
    Ident,
    QIdent,
    Int,
    BigInt,
    Dec,
    Str,
    Date,
    Time,
    DateTime,
    LParen,
    RParen,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    Semi,
    Colon,
    ColonColon,
    Comma,
    Dot,
    DotDot,
    Assign,
    PlusAssign,
    MinusAssign,
    MulAssign,
    DivAssign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Question,
    At,
    Pipe,
    Amp,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Tk,
    /// Identifier name (unquoted), string value (unescaped) or literal digits.
    pub text: Box<str>,
    pub line: u32,
    pub col: u32,
}

impl Token {
    #[inline]
    pub fn is_kw(&self, kw: &str) -> bool {
        self.kind == Tk::Ident && self.text.eq_ignore_ascii_case(kw)
    }
    #[inline]
    pub fn is_name(&self) -> bool {
        matches!(self.kind, Tk::Ident | Tk::QIdent)
    }
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

struct Cond {
    active: bool,
    parent_active: bool,
    taken: bool,
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    line_start: usize,
    symbols: HashSet<String>,
    conds: Vec<Cond>,
    out: Vec<Token>,
}

pub fn lex(src: &str, symbols: &HashSet<String>) -> Result<Vec<Token>, LexError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut lx = Lexer {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        line_start: 0,
        symbols: symbols.iter().map(|s| s.to_ascii_uppercase()).collect(),
        conds: Vec::new(),
        out: Vec::with_capacity(src.len() / 5),
    };
    lx.run()?;
    Ok(lx.out)
}

impl<'a> Lexer<'a> {
    fn err(&self, msg: impl Into<String>) -> LexError {
        LexError { msg: msg.into(), line: self.line, col: (self.pos - self.line_start + 1) as u32 }
    }

    fn active(&self) -> bool {
        self.conds.last().map_or(true, |c| c.active)
    }

    #[inline]
    fn peek(&self, off: usize) -> u8 {
        *self.src.get(self.pos + off).unwrap_or(&0)
    }

    fn at_line_start(&self) -> bool {
        self.src[self.line_start..self.pos].iter().all(|c| *c == b' ' || *c == b'\t')
    }

    fn newline(&mut self) {
        self.line += 1;
        self.line_start = self.pos;
    }

    fn rest_of_line(&mut self) -> &'a str {
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        // strip trailing // comment
        let s = match s.find("//") {
            Some(i) => &s[..i],
            None => s,
        };
        s.trim()
    }

    fn directive(&mut self) -> Result<(), LexError> {
        self.pos += 1; // '#'
        let line = self.rest_of_line();
        let (word, arg) = match line.find(|c: char| c.is_whitespace()) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        match word.to_ascii_lowercase().as_str() {
            "if" => {
                let parent = self.active();
                let v = parent && self.eval(arg)?;
                self.conds.push(Cond { active: v, parent_active: parent, taken: v });
            }
            "elif" => {
                let syms = &self.symbols;
                let v = eval_pp(arg, syms).map_err(|m| self.err(m))?;
                let top = self.conds.last_mut().ok_or_else(|| LexError { msg: "#elif without #if".into(), line: 0, col: 0 })?;
                if top.taken {
                    top.active = false;
                } else {
                    top.active = top.parent_active && v;
                    top.taken = top.active;
                }
            }
            "else" => {
                let top = self.conds.last_mut().ok_or_else(|| LexError { msg: "#else without #if".into(), line: 0, col: 0 })?;
                top.active = top.parent_active && !top.taken;
                top.taken = true;
            }
            "endif" => {
                if self.conds.pop().is_none() {
                    return Err(self.err("#endif without #if"));
                }
            }
            "define" if self.active() => {
                self.symbols.insert(arg.to_ascii_uppercase());
            }
            "undef" if self.active() => {
                self.symbols.remove(&arg.to_ascii_uppercase());
            }
            _ => {} // pragma, region, endregion
        }
        Ok(())
    }

    fn eval(&self, expr: &str) -> Result<bool, LexError> {
        eval_pp(expr, &self.symbols).map_err(|m| self.err(m))
    }

    fn push(&mut self, kind: Tk, text: &str, start: usize) {
        self.out.push(Token {
            kind,
            text: text.into(),
            line: self.line,
            col: (start - self.line_start + 1) as u32,
        });
    }

    fn run(&mut self) -> Result<(), LexError> {
        let n = self.src.len();
        while self.pos < n {
            if !self.active() {
                // skip line by line, looking for directives
                let ls = self.pos;
                let mut p = ls;
                while p < n && (self.src[p] == b' ' || self.src[p] == b'\t' || self.src[p] == b'\r') {
                    p += 1;
                }
                if p < n && self.src[p] == b'#' {
                    self.pos = p;
                    self.directive()?;
                } else {
                    while p < n && self.src[p] != b'\n' {
                        p += 1;
                    }
                    self.pos = p;
                }
                if self.pos < n && self.src[self.pos] == b'\n' {
                    self.pos += 1;
                    self.newline();
                }
                continue;
            }
            let c = self.src[self.pos];
            match c {
                b'\n' => {
                    self.pos += 1;
                    self.newline();
                }
                b' ' | b'\t' | b'\r' | 0x0c => self.pos += 1,
                0xEF if self.peek(1) == 0xBB && self.peek(2) == 0xBF => self.pos += 3,
                b'#' if self.at_line_start() => self.directive()?,
                b'/' if self.peek(1) == b'/' => {
                    while self.pos < n && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                b'/' if self.peek(1) == b'*' => {
                    self.pos += 2;
                    loop {
                        if self.pos >= n {
                            return Err(self.err("unterminated block comment"));
                        }
                        if self.src[self.pos] == b'*' && self.peek(1) == b'/' {
                            self.pos += 2;
                            break;
                        }
                        if self.src[self.pos] == b'\n' {
                            self.pos += 1;
                            self.newline();
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                b'\'' => self.string()?,
                b'"' => self.qident()?,
                b'0'..=b'9' => self.number()?,
                c if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 => self.ident(),
                _ => self.punct()?,
            }
        }
        if !self.conds.is_empty() {
            return Err(self.err("unterminated #if"));
        }
        let line = self.line;
        self.out.push(Token { kind: Tk::Eof, text: "".into(), line, col: 1 });
        Ok(())
    }

    fn string(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        let (sl, sls) = (self.line, self.line_start);
        self.pos += 1;
        let mut s = Vec::new();
        loop {
            if self.pos >= self.src.len() {
                return Err(self.err("unterminated string"));
            }
            let c = self.src[self.pos];
            if c == b'\'' {
                if self.peek(1) == b'\'' {
                    s.push(b'\'');
                    self.pos += 2;
                    continue;
                }
                self.pos += 1;
                break;
            }
            if c == b'\n' {
                s.push(c);
                self.pos += 1;
                self.newline();
                continue;
            }
            // CRLF inside a multi-line literal reads as LF, so a Windows (autocrlf) checkout of the
            // AL sources gives the same text as a Linux one
            if c == b'\r' && self.peek(1) == b'\n' {
                self.pos += 1;
                continue;
            }
            s.push(c);
            self.pos += 1;
        }
        let text = String::from_utf8_lossy(&s);
        self.out.push(Token { kind: Tk::Str, text: text.as_ref().into(), line: sl, col: (start - sls + 1) as u32 });
        Ok(())
    }

    fn qident(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 1;
        let mut s = Vec::new();
        loop {
            if self.pos >= self.src.len() || self.src[self.pos] == b'\n' {
                return Err(self.err("unterminated quoted identifier"));
            }
            let c = self.src[self.pos];
            if c == b'"' {
                if self.peek(1) == b'"' {
                    s.push(b'"');
                    self.pos += 2;
                    continue;
                }
                self.pos += 1;
                break;
            }
            s.push(c);
            self.pos += 1;
        }
        let text = String::from_utf8_lossy(&s).into_owned();
        self.push(Tk::QIdent, &text, start);
        Ok(())
    }

    fn ident(&mut self) {
        let start = self.pos;
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'_' || c.is_ascii_alphanumeric() || c >= 0x80 {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("?").to_string();
        self.push(Tk::Ident, &text, start);
    }

    fn number(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        while self.peek(0).is_ascii_digit() {
            self.pos += 1;
        }
        let mut kind = Tk::Int;
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            kind = Tk::Dec;
            self.pos += 1;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        if (self.peek(0) == b'e' || self.peek(0) == b'E')
            && (self.peek(1).is_ascii_digit() || ((self.peek(1) == b'-' || self.peek(1) == b'+') && self.peek(2).is_ascii_digit()))
        {
            kind = Tk::Dec;
            self.pos += 2;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        let digits_end = self.pos;
        let ss = self.pos;
        while self.peek(0).is_ascii_alphabetic() {
            self.pos += 1;
        }
        let suffix = std::str::from_utf8(&self.src[ss..self.pos]).unwrap().to_ascii_lowercase();
        let kind = match suffix.as_str() {
            "" => kind,
            "d" => Tk::Date,
            "t" => Tk::Time,
            "dt" => Tk::DateTime,
            "l" => Tk::BigInt,
            _ => return Err(self.err(format!("bad numeric suffix '{suffix}'"))),
        };
        let text = std::str::from_utf8(&self.src[start..digits_end]).unwrap().to_string();
        self.push(kind, &text, start);
        Ok(())
    }

    fn punct(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        let c = self.src[self.pos];
        let c1 = self.peek(1);
        let (kind, len) = match (c, c1) {
            (b'(', _) => (Tk::LParen, 1),
            (b')', _) => (Tk::RParen, 1),
            (b'[', _) => (Tk::LBrack, 1),
            (b']', _) => (Tk::RBrack, 1),
            (b'{', _) => (Tk::LBrace, 1),
            (b'}', _) => (Tk::RBrace, 1),
            (b';', _) => (Tk::Semi, 1),
            (b':', b':') => (Tk::ColonColon, 2),
            (b':', b'=') => (Tk::Assign, 2),
            (b':', _) => (Tk::Colon, 1),
            (b',', _) => (Tk::Comma, 1),
            (b'.', b'.') => (Tk::DotDot, 2),
            (b'.', _) => (Tk::Dot, 1),
            (b'+', b'=') => (Tk::PlusAssign, 2),
            (b'-', b'=') => (Tk::MinusAssign, 2),
            (b'*', b'=') => (Tk::MulAssign, 2),
            (b'/', b'=') => (Tk::DivAssign, 2),
            (b'=', _) => (Tk::Eq, 1),
            (b'<', b'>') => (Tk::Ne, 2),
            (b'<', b'=') => (Tk::Le, 2),
            (b'<', _) => (Tk::Lt, 1),
            (b'>', b'=') => (Tk::Ge, 2),
            (b'>', _) => (Tk::Gt, 1),
            (b'+', _) => (Tk::Plus, 1),
            (b'-', _) => (Tk::Minus, 1),
            (b'*', _) => (Tk::Star, 1),
            (b'/', _) => (Tk::Slash, 1),
            (b'?', _) => (Tk::Question, 1),
            (b'@', _) => (Tk::At, 1),
            (b'|', _) => (Tk::Pipe, 1),
            (b'&', _) => (Tk::Amp, 1),
            _ => return Err(self.err(format!("unexpected character '{}'", c as char))),
        };
        self.pos += len;
        self.push(kind, "", start);
        Ok(())
    }
}

/// Evaluate a preprocessor condition: identifiers, `not`, `and`, `or`, parentheses.
pub fn eval_pp(expr: &str, syms: &HashSet<String>) -> Result<bool, String> {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in expr.chars() {
        if ch == '(' || ch == ')' || ch == '!' {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
            toks.push(ch.to_string());
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    let mut p = 0usize;
    fn or(t: &[String], p: &mut usize, s: &HashSet<String>) -> Result<bool, String> {
        let mut v = and(t, p, s)?;
        while *p < t.len() && (t[*p].eq_ignore_ascii_case("or") || t[*p] == "||") {
            *p += 1;
            let r = and(t, p, s)?;
            v = v || r;
        }
        Ok(v)
    }
    fn and(t: &[String], p: &mut usize, s: &HashSet<String>) -> Result<bool, String> {
        let mut v = un(t, p, s)?;
        while *p < t.len() && (t[*p].eq_ignore_ascii_case("and") || t[*p] == "&&") {
            *p += 1;
            let r = un(t, p, s)?;
            v = v && r;
        }
        Ok(v)
    }
    fn un(t: &[String], p: &mut usize, s: &HashSet<String>) -> Result<bool, String> {
        let tok = t.get(*p).ok_or("unexpected end of #if expression")?;
        *p += 1;
        if tok.eq_ignore_ascii_case("not") || tok == "!" {
            return Ok(!un(t, p, s)?);
        }
        if tok == "(" {
            let v = or(t, p, s)?;
            if t.get(*p).map(|x| x.as_str()) != Some(")") {
                return Err("missing ) in #if expression".into());
            }
            *p += 1;
            return Ok(v);
        }
        Ok(s.contains(&tok.to_ascii_uppercase()))
    }
    let v = or(&toks, &mut p, syms)?;
    if p != toks.len() {
        return Err(format!("trailing tokens in #if expression: {expr}"));
    }
    Ok(v)
}
