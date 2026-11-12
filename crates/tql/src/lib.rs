//! TQL, the TaskCraft query language.
//!
//! ```text
//! project = WEB and status in ("To do", "In progress") and assignee = me() order by rank
//! type = Bug and priority >= high and created > -14d
//! labels is empty or due < endOfWeek()
//! login timeout                      (bare words search the text)
//! ```
//!
//! The grammar is forgiving: words that don't form a clause become a text search, clauses next
//! to each other are joined with `and`, keywords are case-insensitive. This crate only parses;
//! the server turns the tree into SQL (it knows the fields, people and permissions).

use std::fmt;

use serde::Serialize;

/// A parsed query: an optional filter and the sort order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Query {
    pub filter: Option<Expr>,
    pub order: Vec<Order>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Order {
    pub field: String,
    pub desc: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Expr {
    And {
        items: Vec<Expr>,
    },
    Or {
        items: Vec<Expr>,
    },
    Not {
        item: Box<Expr>,
    },
    /// `field op value`
    Compare {
        field: String,
        op: Op,
        value: Value,
    },
    /// `field [not] in (a, b)`
    In {
        field: String,
        values: Vec<Value>,
        negated: bool,
    },
    /// `field is [not] empty`
    Empty {
        field: String,
        negated: bool,
    },
    /// a bare word or quoted string: full-text search
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Op {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    /// contains (text)
    #[serde(rename = "~")]
    Contains,
    #[serde(rename = "!~")]
    NotContains,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Contains => "~",
            Op::NotContains => "!~",
        }
    }

    /// The same comparison with the sides swapped negated (`not (a > b)` is `a <= b`).
    pub fn negate(self) -> Op {
        match self {
            Op::Eq => Op::Ne,
            Op::Ne => Op::Eq,
            Op::Gt => Op::Le,
            Op::Ge => Op::Lt,
            Op::Lt => Op::Ge,
            Op::Le => Op::Gt,
            Op::Contains => Op::NotContains,
            Op::NotContains => Op::Contains,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Value {
    /// a word or a quoted string (`"In progress"`, `WEB-12`, `-7d`, `2026-11-01`)
    Text {
        text: String,
    },
    Number {
        value: f64,
    },
    /// `me()`, `now("-1d")`, `openSprints()`
    Function {
        name: String,
        args: Vec<Value>,
    },
}

impl Value {
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text { text: s.into() }
    }

    /// The value as text (numbers formatted), None for functions.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Text { text } => Some(text.clone()),
            Value::Number { value } => Some(if value.fract() == 0.0 { format!("{}", *value as i64) } else { value.to_string() }),
            Value::Function { .. } => None,
        }
    }
}

/// A syntax error with the character offset where it was noticed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.offset + 1)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(f64),
    Op(Op),
    LParen,
    RParen,
    Comma,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    at: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '+' | ':' | '/' | '#')
}

fn lex(input: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let char_at = |i: usize| chars.get(i).map(|c| c.1);
    while i < chars.len() {
        let (at, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let at = input[..at].chars().count();
        match c {
            '(' => {
                out.push(Token { tok: Tok::LParen, at });
                i += 1;
            }
            ')' => {
                out.push(Token { tok: Tok::RParen, at });
                i += 1;
            }
            ',' => {
                out.push(Token { tok: Tok::Comma, at });
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                loop {
                    match char_at(i) {
                        None => return Err(ParseError { message: "unclosed quote".into(), offset: at }),
                        Some('\\') if matches!(char_at(i + 1), Some(q) if q == quote || q == '\\') => {
                            s.push(char_at(i + 1).unwrap_or(quote));
                            i += 2;
                        }
                        Some(ch) if ch == quote => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(Token { tok: Tok::Str(s), at });
            }
            '=' => {
                out.push(Token { tok: Tok::Op(Op::Eq), at });
                i += if char_at(i + 1) == Some('=') { 2 } else { 1 };
            }
            '!' if char_at(i + 1) == Some('=') => {
                out.push(Token { tok: Tok::Op(Op::Ne), at });
                i += 2;
            }
            '!' if char_at(i + 1) == Some('~') => {
                out.push(Token { tok: Tok::Op(Op::NotContains), at });
                i += 2;
            }
            '~' => {
                out.push(Token { tok: Tok::Op(Op::Contains), at });
                i += 1;
            }
            '<' if char_at(i + 1) == Some('>') => {
                out.push(Token { tok: Tok::Op(Op::Ne), at });
                i += 2;
            }
            '<' | '>' => {
                let eq = char_at(i + 1) == Some('=');
                let op = match (c, eq) {
                    ('<', false) => Op::Lt,
                    ('<', true) => Op::Le,
                    ('>', false) => Op::Gt,
                    _ => Op::Ge,
                };
                out.push(Token { tok: Tok::Op(op), at });
                i += if eq { 2 } else { 1 };
            }
            c if is_word_char(c) => {
                let mut s = String::new();
                while let Some(ch) = char_at(i).filter(|c| is_word_char(*c)) {
                    s.push(ch);
                    i += 1;
                }
                // plain numbers are numbers; `-7d`, `2026-11-01`, `WEB-12` stay words, and so do
                // numbers that wouldn't read back the same (`1.0`, `007`: version names, codes)
                let tok = match s.parse::<f64>() {
                    Ok(n) if n.is_finite() && (Value::Number { value: n }).as_text().as_deref() == Some(s.as_str()) => Tok::Num(n),
                    _ => Tok::Word(s),
                };
                out.push(Token { tok, at });
            }
            other => return Err(ParseError { message: format!("unexpected character {other:?}"), offset: at }),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    end: usize,
}

fn kw(t: Option<&Token>, word: &str) -> bool {
    matches!(t, Some(Token { tok: Tok::Word(w), .. }) if w.eq_ignore_ascii_case(word))
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Token> {
        self.toks.get(self.pos + n)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn offset(&self) -> usize {
        self.peek().map(|t| t.at).unwrap_or(self.end)
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError { message: message.into(), offset: self.offset() })
    }

    fn at_order_by(&self) -> bool {
        kw(self.peek(), "order") && kw(self.peek_at(1), "by")
    }

    /// Whether the next token can start another clause (implicit `and`).
    fn starts_primary(&self) -> bool {
        match self.peek() {
            None => false,
            Some(Token { tok: Tok::Word(w), .. }) => !(w.eq_ignore_ascii_case("or") || w.eq_ignore_ascii_case("and") || self.at_order_by()),
            Some(Token { tok: Tok::Str(_) | Tok::Num(_) | Tok::LParen, .. }) => true,
            _ => false,
        }
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut items = vec![self.and_expr()?];
        while kw(self.peek(), "or") {
            self.next();
            items.push(self.and_expr()?);
        }
        Ok(if items.len() == 1 { items.pop().expect("one item") } else { Expr::Or { items } })
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut items = vec![self.not_expr()?];
        loop {
            if kw(self.peek(), "and") {
                self.next();
                items.push(self.not_expr()?);
            } else if self.starts_primary() {
                items.push(self.not_expr()?);
            } else {
                break;
            }
        }
        Ok(if items.len() == 1 { items.pop().expect("one item") } else { Expr::And { items } })
    }

    fn not_expr(&mut self) -> Result<Expr, ParseError> {
        // `not` followed by an operator is a field called "not" (nobody does that) — treat as keyword
        if kw(self.peek(), "not") && !matches!(self.peek_at(1).map(|t| &t.tok), Some(Tok::Op(_)) | None) {
            self.next();
            return Ok(Expr::Not { item: Box::new(self.not_expr()?) });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().map(|t| t.tok.clone()) {
            None => self.err("a condition is missing"),
            Some(Tok::LParen) => {
                self.next();
                let e = self.or_expr()?;
                match self.next() {
                    Some(Token { tok: Tok::RParen, .. }) => Ok(e),
                    _ => {
                        self.pos = self.pos.saturating_sub(1);
                        self.err("a closing parenthesis is missing")
                    }
                }
            }
            Some(Tok::RParen) => self.err("unexpected closing parenthesis"),
            Some(Tok::Comma) => self.err("unexpected comma"),
            Some(Tok::Op(op)) => self.err(format!("a field name is missing before {}", op.as_str())),
            Some(Tok::Num(n)) => {
                self.next();
                Ok(Expr::Text { text: Value::Number { value: n }.as_text().unwrap_or_default() })
            }
            Some(Tok::Str(s)) => {
                self.next();
                // a quoted field name followed by an operator (custom fields: "Customer" = Acme)
                if self.continues_clause() {
                    return self.clause(s);
                }
                Ok(Expr::Text { text: s })
            }
            Some(Tok::Word(w)) => {
                self.next();
                if self.continues_clause() {
                    return self.clause(w);
                }
                Ok(Expr::Text { text: w })
            }
        }
    }

    /// After a field name: an operator, `is`, `in` or `not in` follows.
    fn continues_clause(&self) -> bool {
        match self.peek() {
            Some(Token { tok: Tok::Op(_), .. }) => true,
            t if kw(t, "is") || kw(t, "in") => true,
            t if kw(t, "not") => kw(self.peek_at(1), "in"),
            _ => false,
        }
    }

    fn clause(&mut self, field: String) -> Result<Expr, ParseError> {
        if kw(self.peek(), "is") {
            self.next();
            let negated = if kw(self.peek(), "not") {
                self.next();
                true
            } else {
                false
            };
            if kw(self.peek(), "empty") || kw(self.peek(), "null") {
                self.next();
                return Ok(Expr::Empty { field, negated });
            }
            return self.err("expected empty after is");
        }
        let negated = if kw(self.peek(), "not") {
            self.next();
            true
        } else {
            false
        };
        if kw(self.peek(), "in") {
            self.next();
            let values = self.value_list()?;
            return Ok(Expr::In { field, values, negated });
        }
        let Some(Token { tok: Tok::Op(op), .. }) = self.next() else {
            self.pos = self.pos.saturating_sub(1);
            return self.err("expected an operator (=, !=, >, <, ~, in, is)");
        };
        let value = self.value()?;
        Ok(Expr::Compare { field, op, value })
    }

    fn value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        if !matches!(self.peek().map(|t| &t.tok), Some(Tok::LParen)) {
            // `status in todo` — a single value works too
            return Ok(vec![self.value()?]);
        }
        self.next();
        let mut values = Vec::new();
        loop {
            if matches!(self.peek().map(|t| &t.tok), Some(Tok::RParen)) {
                self.next();
                break;
            }
            values.push(self.value()?);
            match self.next() {
                Some(Token { tok: Tok::Comma, .. }) => continue,
                Some(Token { tok: Tok::RParen, .. }) => break,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return self.err("expected , or ) in the list");
                }
            }
        }
        if values.is_empty() {
            return self.err("the list is empty");
        }
        Ok(values)
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.next() {
            Some(Token { tok: Tok::Str(s), .. }) => Ok(Value::Text { text: s }),
            Some(Token { tok: Tok::Num(n), .. }) => Ok(Value::Number { value: n }),
            Some(Token { tok: Tok::Word(w), .. }) => {
                if matches!(self.peek().map(|t| &t.tok), Some(Tok::LParen)) {
                    self.next();
                    let mut args = Vec::new();
                    loop {
                        if matches!(self.peek().map(|t| &t.tok), Some(Tok::RParen)) {
                            self.next();
                            break;
                        }
                        args.push(self.value()?);
                        match self.next() {
                            Some(Token { tok: Tok::Comma, .. }) => continue,
                            Some(Token { tok: Tok::RParen, .. }) => break,
                            _ => {
                                self.pos = self.pos.saturating_sub(1);
                                return self.err(format!("expected ) after the arguments of {w}()"));
                            }
                        }
                    }
                    return Ok(Value::Function { name: w, args });
                }
                Ok(Value::Text { text: w })
            }
            _ => {
                self.pos = self.pos.saturating_sub(1);
                self.err("a value is missing")
            }
        }
    }

    fn order(&mut self) -> Result<Vec<Order>, ParseError> {
        let mut order = Vec::new();
        if !self.at_order_by() {
            return Ok(order);
        }
        self.pos += 2;
        loop {
            let field = match self.next() {
                Some(Token { tok: Tok::Word(w) | Tok::Str(w), .. }) => w,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return self.err("a field to sort by is missing");
                }
            };
            let desc = if kw(self.peek(), "desc") {
                self.next();
                true
            } else {
                if kw(self.peek(), "asc") {
                    self.next();
                }
                false
            };
            order.push(Order { field, desc });
            if matches!(self.peek().map(|t| &t.tok), Some(Tok::Comma)) {
                self.next();
                continue;
            }
            break;
        }
        Ok(order)
    }
}

/// Parses a query. An empty string is a query without filter.
pub fn parse(input: &str) -> Result<Query, ParseError> {
    let toks = lex(input)?;
    let mut p = Parser { toks, pos: 0, end: input.chars().count() };
    let filter = if p.peek().is_none() || p.at_order_by() { None } else { Some(p.or_expr()?) };
    let order = p.order()?;
    if p.peek().is_some() {
        return p.err("unexpected text at the end (missing and / or?)");
    }
    Ok(Query { filter, order })
}

/// Quotes a value for a query when needed (`In progress` → `"In progress"`).
pub fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars().all(is_word_char)
        && !["and", "or", "not", "in", "is", "empty", "null", "order", "by"].iter().any(|k| s.eq_ignore_ascii_case(k));
    if plain { s.to_string() } else { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) }
}

/// Every field name the query uses (lower case), for validation and autocompletion.
pub fn fields(q: &Query) -> Vec<String> {
    fn walk(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::And { items } | Expr::Or { items } => items.iter().for_each(|i| walk(i, out)),
            Expr::Not { item } => walk(item, out),
            Expr::Compare { field, .. } | Expr::In { field, .. } | Expr::Empty { field, .. } => out.push(field.to_lowercase()),
            Expr::Text { .. } => {}
        }
    }
    let mut out = Vec::new();
    if let Some(f) = &q.filter {
        walk(f, &mut out);
    }
    out.extend(q.order.iter().map(|o| o.field.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(field: &str, op: Op, value: Value) -> Expr {
        Expr::Compare { field: field.into(), op, value }
    }

    #[test]
    fn simple_and_or() {
        let q = parse("project = WEB and status != Done or assignee = me()").unwrap();
        assert_eq!(
            q.filter.unwrap(),
            Expr::Or {
                items: vec![
                    Expr::And { items: vec![cmp("project", Op::Eq, Value::text("WEB")), cmp("status", Op::Ne, Value::text("Done"))] },
                    cmp("assignee", Op::Eq, Value::Function { name: "me".into(), args: vec![] }),
                ]
            }
        );
    }

    #[test]
    fn in_lists_and_empty() {
        let q = parse(r#"status in ("To do", 'In progress') and labels is not empty and type not in (Bug)"#).unwrap();
        let Expr::And { items } = q.filter.unwrap() else { panic!() };
        assert_eq!(items[0], Expr::In { field: "status".into(), values: vec![Value::text("To do"), Value::text("In progress")], negated: false });
        assert_eq!(items[1], Expr::Empty { field: "labels".into(), negated: true });
        assert_eq!(items[2], Expr::In { field: "type".into(), values: vec![Value::text("Bug")], negated: true });
    }

    #[test]
    fn bare_words_are_text() {
        let q = parse("login timeout project = WEB").unwrap();
        assert_eq!(
            q.filter.unwrap(),
            Expr::And { items: vec![Expr::Text { text: "login".into() }, Expr::Text { text: "timeout".into() }, cmp("project", Op::Eq, Value::text("WEB"))] }
        );
        assert_eq!(parse("\"exact phrase\"").unwrap().filter.unwrap(), Expr::Text { text: "exact phrase".into() });
    }

    #[test]
    fn order_by_and_relative_dates() {
        let q = parse("created > -7d and points >= 3 ORDER BY priority DESC, rank").unwrap();
        assert_eq!(q.order, vec![Order { field: "priority".into(), desc: true }, Order { field: "rank".into(), desc: false }]);
        let Expr::And { items } = q.filter.unwrap() else { panic!() };
        assert_eq!(items[0], cmp("created", Op::Gt, Value::text("-7d")));
        assert_eq!(items[1], cmp("points", Op::Ge, Value::Number { value: 3.0 }));
        assert_eq!(parse("order by updated desc").unwrap().filter, None);
        assert_eq!(parse("").unwrap(), Query { filter: None, order: vec![] });
    }

    #[test]
    fn keys_dates_and_functions_with_args() {
        let q = parse("parent = WEB-12 and due < now(\"+2w\") and created = 2026-11-01").unwrap();
        let Expr::And { items } = q.filter.unwrap() else { panic!() };
        assert_eq!(items[0], cmp("parent", Op::Eq, Value::text("WEB-12")));
        assert_eq!(items[1], cmp("due", Op::Lt, Value::Function { name: "now".into(), args: vec![Value::text("+2w")] }));
        assert_eq!(items[2], cmp("created", Op::Eq, Value::text("2026-11-01")));
    }

    #[test]
    fn not_and_parentheses() {
        let q = parse("not (status = Done or type = Epic) and title ~ \"crash\"").unwrap();
        let Expr::And { items } = q.filter.unwrap() else { panic!() };
        assert!(matches!(&items[0], Expr::Not { item } if matches!(**item, Expr::Or { .. })));
        assert_eq!(items[1], cmp("title", Op::Contains, Value::text("crash")));
    }

    #[test]
    fn quoted_custom_field_names() {
        let q = parse("\"Customer tier\" = Gold").unwrap();
        assert_eq!(q.filter.unwrap(), cmp("Customer tier", Op::Eq, Value::text("Gold")));
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse("status = ").unwrap_err();
        assert_eq!(e.offset, 9);
        assert!(parse("(status = Done").unwrap_err().message.contains("closing parenthesis"));
        assert!(parse("status in ()").is_err());
        assert!(parse("\"open").unwrap_err().message.contains("quote"));
        assert!(parse("= 3").is_err());
        assert!(parse("status = a )").is_err());
    }

    #[test]
    fn number_like_names_keep_their_text() {
        let q = parse("version = 1.0 and points >= 2.5 and code = 007").unwrap();
        let Expr::And { items } = q.filter.unwrap() else { panic!() };
        assert_eq!(items[0], cmp("version", Op::Eq, Value::text("1.0")));
        assert_eq!(items[1], cmp("points", Op::Ge, Value::Number { value: 2.5 }));
        assert_eq!(items[2], cmp("code", Op::Eq, Value::text("007")));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("WEB"), "WEB");
        assert_eq!(quote("In progress"), "\"In progress\"");
        assert_eq!(quote("or"), "\"or\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        // round trip
        let q = parse(&format!("status = {}", quote("a \"b\" c"))).unwrap();
        assert_eq!(q.filter.unwrap(), cmp("status", Op::Eq, Value::text("a \"b\" c")));
    }

    #[test]
    fn field_listing() {
        let q = parse("project = A and (status = B or labels is empty) order by rank").unwrap();
        assert_eq!(fields(&q), vec!["project", "status", "labels", "rank"]);
    }
}
