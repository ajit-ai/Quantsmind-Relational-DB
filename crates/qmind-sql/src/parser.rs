//! E5 — Handwritten SQL tokenizer + recursive-descent parser.
//!
//! Parses the QuantsMind SQL subset into a self-contained AST. Feature-gated
//! behind `handwritten-parser`; when the feature is off, `sqlparser-rs` is
//! used instead (E5b integrates this into engine.rs).

// ── Tokens ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    // keywords (lowercased during normalization)
    Keyword(&'static str),
    Ident(String),
    Int(i64),
    Str(String),
    // punctuation
    LParen,
    RParen,
    Comma,
    Star,
    Semicolon,
    // comparison operators
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eof,
}

// ── AST ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    CreateTable {
        name: String,
        columns: Vec<Column>,
        if_not_exists: bool,
    },
    Insert {
        table: String,
        rows: Vec<Vec<Expr>>,
    },
    Select(Select),
    ShowTables,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub not_null: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Integer,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Select {
    pub projection: Vec<SelectItem>,
    pub from: TableRef,
    pub selection: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectItem {
    Star,
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableRef {
    Table(String),
    Join {
        left: Box<TableRef>,
        right: String,
        on: Expr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Identifier(String),
    Literal(SqlValue),
    BinaryOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    Function {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
}

use crate::codec::SqlValue;

// ── Tokenizer ───────────────────────────────────────────────────────────────

const KEYWORDS: &[(&str, &str)] = &[
    ("CREATE", "CREATE"),
    ("TABLE", "TABLE"),
    ("INSERT", "INSERT"),
    ("INTO", "INTO"),
    ("VALUES", "VALUES"),
    ("SELECT", "SELECT"),
    ("FROM", "FROM"),
    ("WHERE", "WHERE"),
    ("AND", "AND"),
    ("OR", "OR"),
    ("LIMIT", "LIMIT"),
    ("JOIN", "JOIN"),
    ("INNER", "INNER"),
    ("ON", "ON"),
    ("GROUP", "GROUP"),
    ("BY", "BY"),
    ("SHOW", "SHOW"),
    ("TABLES", "TABLES"),
    ("IF", "IF"),
    ("NOT", "NOT"),
    ("EXISTS", "EXISTS"),
    ("INTEGER", "INTEGER"),
    ("INT", "INTEGER"),
    ("BIGINT", "INTEGER"),
    ("TEXT", "TEXT"),
    ("VARCHAR", "TEXT"),
    ("STRING", "TEXT"),
    ("NULL", "NULL"),
    ("COUNT", "COUNT"),
    ("SUM", "SUM"),
    ("AVG", "AVG"),
    ("MIN", "MIN"),
    ("MAX", "MAX"),
];

pub fn tokenize(sql: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            c if c.is_ascii_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                i += 1;
            }
            '=' => {
                tokens.push(Token::Eq);
                i += 1;
            }
            '!' if i + 1 < len && chars[i + 1] == '=' => {
                tokens.push(Token::NotEq);
                i += 2;
            }
            '<' if i + 1 < len && chars[i + 1] == '=' => {
                tokens.push(Token::LtEq);
                i += 2;
            }
            '<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            '>' if i + 1 < len && chars[i + 1] == '=' => {
                tokens.push(Token::GtEq);
                i += 2;
            }
            '>' => {
                tokens.push(Token::Gt);
                i += 1;
            }
            '\'' => {
                i += 1;
                let mut s = String::new();
                while i < len && chars[i] != '\'' {
                    if chars[i] == '\'' && i + 1 < len && chars[i + 1] == '\'' {
                        s.push('\'');
                        i += 2;
                    } else {
                        s.push(chars[i]);
                        i += 1;
                    }
                }
                if i >= len {
                    return Err("unterminated string literal".into());
                }
                i += 1; // closing '
                tokens.push(Token::Str(s));
            }
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < len && chars[i + 1].is_ascii_digit()) =>
            {
                let negative = c == '-';
                if negative {
                    i += 1;
                }
                let start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let num_str: String = chars[start..i].iter().collect();
                let mut n: i64 = num_str
                    .parse()
                    .map_err(|_| format!("invalid number: {num_str}"))?;
                if negative {
                    n = -n;
                }
                tokens.push(Token::Int(n));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let upper = word.to_uppercase();
                if let Some((_, kw)) = KEYWORDS.iter().find(|(k, _)| *k == upper.as_str()) {
                    tokens.push(Token::Keyword(kw));
                } else {
                    tokens.push(Token::Ident(word));
                }
            }
            other => return Err(format!("unexpected character: '{other}'")),
        }
    }

    tokens.push(Token::Eof);
    Ok(tokens)
}

// ── Parser ──────────────────────────────────────────────────────────────────

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn parse(sql: &str) -> Result<Vec<Statement>, String> {
        let tokens = tokenize(sql)?;
        let mut p = Parser { tokens, pos: 0 };
        let mut stmts = Vec::new();

        while !p.at_end() {
            stmts.push(p.parse_statement()?);
            if p.peek() == &Token::Semicolon {
                p.advance();
            }
        }

        if stmts.is_empty() {
            return Err("empty statement".into());
        }
        Ok(stmts)
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        if !self.at_end() {
            self.pos += 1;
        }
        t
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.tokens[self.pos] == Token::Eof
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), String> {
        match self.advance() {
            Token::Keyword(k) if k == kw => Ok(()),
            t => Err(format!("expected '{kw}', got {t:?}")),
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.advance() {
            Token::Ident(s) => Ok(s),
            t => Err(format!("expected identifier, got {t:?}")),
        }
    }

    // ── statement ────────────────────────────────────────────────────────

    fn parse_statement(&mut self) -> Result<Statement, String> {
        match self.peek() {
            Token::Keyword("CREATE") => self.parse_create_table(),
            Token::Keyword("INSERT") => self.parse_insert(),
            Token::Keyword("SELECT") => self.parse_select().map(Statement::Select),
            Token::Keyword("SHOW") => self.parse_show_tables(),
            t => Err(format!("unsupported statement, got {t:?}")),
        }
    }

    // ── CREATE TABLE ─────────────────────────────────────────────────────

    fn parse_create_table(&mut self) -> Result<Statement, String> {
        self.expect_keyword("CREATE")?;
        self.expect_keyword("TABLE")?;
        let mut if_not_exists = false;
        if self.peek() == &Token::Keyword("IF") {
            self.advance();
            self.expect_keyword("NOT")?;
            self.expect_keyword("EXISTS")?;
            if_not_exists = true;
        }
        let name = self.expect_ident()?;
        self.expect_paren_open()?;
        let columns = self.parse_comma_separated(Self::parse_column_def)?;
        self.expect_paren_close()?;
        Ok(Statement::CreateTable {
            name,
            columns,
            if_not_exists,
        })
    }

    fn parse_column_def(&mut self) -> Result<Column, String> {
        let name = self.expect_ident()?;
        let data_type = self.parse_data_type()?;
        let mut not_null = false;
        if self.peek() == &Token::Keyword("NOT") {
            self.advance();
            self.expect_keyword("NULL")?;
            not_null = true;
        }
        Ok(Column {
            name,
            data_type,
            not_null,
        })
    }

    fn parse_data_type(&mut self) -> Result<DataType, String> {
        match self.advance() {
            Token::Keyword("INTEGER") | Token::Keyword("INT") | Token::Keyword("BIGINT") => {
                Ok(DataType::Integer)
            }
            Token::Keyword("TEXT") | Token::Keyword("VARCHAR") | Token::Keyword("STRING") => {
                Ok(DataType::Text)
            }
            t => Err(format!("unsupported data type, got {t:?}")),
        }
    }

    // ── INSERT ───────────────────────────────────────────────────────────

    fn parse_insert(&mut self) -> Result<Statement, String> {
        self.expect_keyword("INSERT")?;
        self.expect_keyword("INTO")?;
        let table = self.expect_ident()?;
        self.expect_keyword("VALUES")?;
        let rows = self.parse_comma_separated(|p| {
            p.expect_paren_open()?;
            let row = p.parse_comma_separated(Self::parse_expr)?;
            p.expect_paren_close()?;
            Ok(row)
        })?;
        Ok(Statement::Insert { table, rows })
    }

    // ── SELECT ───────────────────────────────────────────────────────────

    fn parse_select(&mut self) -> Result<Select, String> {
        self.expect_keyword("SELECT")?;
        let projection = self.parse_select_list()?;
        let from = self.parse_from()?;

        let mut selection = None;
        if self.peek() == &Token::Keyword("WHERE") {
            self.advance();
            selection = Some(self.parse_and_expr()?);
        }

        let mut group_by = Vec::new();
        if self.peek() == &Token::Keyword("GROUP") {
            self.advance();
            self.expect_keyword("BY")?;
            group_by = self.parse_comma_separated(Self::parse_expr)?;
        }

        let mut limit = None;
        if self.peek() == &Token::Keyword("LIMIT") {
            self.advance();
            limit = Some(self.parse_limit_value()?);
        }

        Ok(Select {
            projection,
            from,
            selection,
            group_by,
            limit,
        })
    }

    fn parse_select_list(&mut self) -> Result<Vec<SelectItem>, String> {
        if self.peek() == &Token::Star {
            self.advance();
            return Ok(vec![SelectItem::Star]);
        }
        self.parse_comma_separated(Self::parse_select_item)
    }

    fn parse_select_item(&mut self) -> Result<SelectItem, String> {
        // Check for aggregate function: FUNC(...)
        if let Token::Keyword(k @ ("COUNT" | "SUM" | "AVG" | "MIN" | "MAX")) = self.peek().clone() {
            self.advance();
            self.expect_paren_open()?;
            let arg = if self.peek() == &Token::Star {
                self.advance();
                Expr::Identifier("*".into())
            } else {
                self.parse_expr()?
            };
            self.expect_paren_close()?;
            return Ok(SelectItem::Expr(Expr::Function {
                name: k.to_string(),
                args: vec![arg],
            }));
        }
        Ok(SelectItem::Expr(self.parse_expr()?))
    }

    fn parse_from(&mut self) -> Result<TableRef, String> {
        self.expect_keyword("FROM")?;
        let mut left = TableRef::Table(self.expect_ident()?);

        loop {
            if self.peek() == &Token::Keyword("INNER") || self.peek() == &Token::Keyword("JOIN") {
                if self.peek() == &Token::Keyword("INNER") {
                    self.advance();
                }
                self.expect_keyword("JOIN")?;
                let right = self.expect_ident()?;
                self.expect_keyword("ON")?;
                let on = self.parse_expr()?;
                left = TableRef::Join {
                    left: Box::new(left),
                    right,
                    on,
                };
            } else {
                break;
            }
        }

        Ok(left)
    }

    fn parse_limit_value(&mut self) -> Result<usize, String> {
        match self.advance() {
            Token::Int(n) if n >= 0 => Ok(n as usize),
            t => Err(format!("LIMIT must be a non-negative integer, got {t:?}")),
        }
    }

    // ── SHOW TABLES ──────────────────────────────────────────────────────

    fn parse_show_tables(&mut self) -> Result<Statement, String> {
        self.expect_keyword("SHOW")?;
        self.expect_keyword("TABLES")?;
        Ok(Statement::ShowTables)
    }

    // ── expressions ──────────────────────────────────────────────────────

    fn parse_and_expr(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        while self.peek() == &Token::Keyword("AND") {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let left = self.parse_primary()?;
        let op = match self.peek() {
            Token::Eq => BinOp::Eq,
            Token::NotEq => BinOp::NotEq,
            Token::Lt => BinOp::Lt,
            Token::LtEq => BinOp::LtEq,
            Token::Gt => BinOp::Gt,
            Token::GtEq => BinOp::GtEq,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_primary()?;
        Ok(Expr::BinaryOp {
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                // Check for function call: name(...)
                if self.peek() == &Token::LParen {
                    self.advance();
                    let args = if self.peek() == &Token::RParen {
                        Vec::new()
                    } else {
                        self.parse_comma_separated(Self::parse_expr)?
                    };
                    self.expect_paren_close()?;
                    return Ok(Expr::Function { name, args });
                }
                Ok(Expr::Identifier(name))
            }
            Token::Keyword("COUNT" | "SUM" | "AVG" | "MIN" | "MAX") => {
                let name = match self.advance() {
                    Token::Keyword(k) => k.to_string(),
                    _ => unreachable!(),
                };
                self.expect_paren_open()?;
                let arg = if self.peek() == &Token::Star {
                    self.advance();
                    Expr::Identifier("*".into())
                } else {
                    self.parse_expr()?
                };
                self.expect_paren_close()?;
                Ok(Expr::Function {
                    name,
                    args: vec![arg],
                })
            }
            Token::Int(n) => {
                self.advance();
                Ok(Expr::Literal(SqlValue::Int(n)))
            }
            Token::Str(s) => {
                self.advance();
                Ok(Expr::Literal(SqlValue::Text(s)))
            }
            Token::Keyword("NULL") => {
                self.advance();
                Ok(Expr::Literal(SqlValue::Null))
            }
            Token::LParen => {
                self.advance();
                let e = self.parse_and_expr()?;
                self.expect_paren_close()?;
                Ok(e)
            }
            t => Err(format!("unexpected token in expression: {t:?}")),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_and_expr()
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn expect_paren_open(&mut self) -> Result<(), String> {
        match self.advance() {
            Token::LParen => Ok(()),
            t => Err(format!("expected '(', got {t:?}")),
        }
    }

    fn expect_paren_close(&mut self) -> Result<(), String> {
        match self.advance() {
            Token::RParen => Ok(()),
            t => Err(format!("expected ')', got {t:?}")),
        }
    }

    fn parse_comma_separated<F, T>(&mut self, mut parse_fn: F) -> Result<Vec<T>, String>
    where
        F: FnMut(&mut Self) -> Result<T, String>,
    {
        let mut items = vec![parse_fn(self)?];
        while self.peek() == &Token::Comma {
            self.advance();
            items.push(parse_fn(self)?);
        }
        Ok(items)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basics() {
        let toks = tokenize("SELECT * FROM t WHERE a = 1").unwrap();
        assert!(matches!(toks[0], Token::Keyword("SELECT")));
        assert_eq!(toks[1], Token::Star);
        assert!(matches!(toks[2], Token::Keyword("FROM")));
        assert!(matches!(toks[3], Token::Ident(ref s) if s == "t"));
        assert!(matches!(toks[4], Token::Keyword("WHERE")));
        assert!(matches!(toks[5], Token::Ident(ref s) if s == "a"));
        assert_eq!(toks[6], Token::Eq);
        assert_eq!(toks[7], Token::Int(1));
    }

    #[test]
    fn tokenize_string_with_escape() {
        let toks = tokenize("INSERT INTO t VALUES ('it''s ok')").unwrap();
        assert_eq!(toks.last().unwrap(), &Token::Eof);
    }

    #[test]
    fn parse_create_table() {
        let stmts = Parser::parse("CREATE TABLE t (id INTEGER NOT NULL, name TEXT)").unwrap();
        match &stmts[0] {
            Statement::CreateTable {
                name,
                columns,
                if_not_exists,
            } => {
                assert_eq!(name, "t");
                assert!(!if_not_exists);
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].name, "id");
                assert_eq!(columns[0].data_type, DataType::Integer);
                assert!(columns[0].not_null);
                assert_eq!(columns[1].name, "name");
                assert_eq!(columns[1].data_type, DataType::Text);
                assert!(!columns[1].not_null);
            }
            other => panic!("expected CreateTable, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_table_if_not_exists() {
        let stmts = Parser::parse("CREATE TABLE IF NOT EXISTS t (x INT)").unwrap();
        match &stmts[0] {
            Statement::CreateTable { if_not_exists, .. } => assert!(if_not_exists),
            other => panic!("expected CreateTable, got {other:?}"),
        }
    }

    #[test]
    fn parse_insert_multiple_rows() {
        let stmts = Parser::parse("INSERT INTO t VALUES (1, 'a'), (2, 'b')").unwrap();
        match &stmts[0] {
            Statement::Insert { table, rows } => {
                assert_eq!(table, "t");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0][0], Expr::Literal(SqlValue::Int(1)));
                assert_eq!(rows[0][1], Expr::Literal(SqlValue::Text("a".into())));
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn parse_select_star() {
        let stmts = Parser::parse("SELECT * FROM users").unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection, vec![SelectItem::Star]);
                assert!(matches!(&sel.from, TableRef::Table(s) if s == "users"));
                assert!(sel.selection.is_none());
                assert!(sel.group_by.is_empty());
                assert!(sel.limit.is_none());
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn parse_select_with_where_and_limit() {
        let stmts = Parser::parse("SELECT a, b FROM t WHERE a > 5 AND b = 'x' LIMIT 10").unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection.len(), 2);
                assert!(sel.selection.is_some());
                assert_eq!(sel.limit, Some(10));
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn parse_inner_join() {
        let stmts = Parser::parse(
            "SELECT name, amount FROM customers INNER JOIN orders ON id = cid WHERE amount > 200",
        )
        .unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert!(matches!(&sel.from, TableRef::Join { right, .. } if right == "orders"));
                assert!(sel.selection.is_some());
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by() {
        let stmts = Parser::parse("SELECT tag, COUNT(*), SUM(v) FROM s GROUP BY tag").unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection.len(), 3);
                assert_eq!(sel.group_by.len(), 1);
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn parse_aggregate_functions() {
        let stmts = Parser::parse("SELECT COUNT(*), AVG(x), MIN(y), MAX(z) FROM t").unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection.len(), 4);
                for item in &sel.projection {
                    assert!(matches!(item, SelectItem::Expr(Expr::Function { .. })));
                }
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn parse_show_tables() {
        let stmts = Parser::parse("SHOW TABLES").unwrap();
        assert!(matches!(stmts[0], Statement::ShowTables));
    }

    #[test]
    fn parse_negative_integer() {
        let stmts = Parser::parse("INSERT INTO t VALUES (-42)").unwrap();
        match &stmts[0] {
            Statement::Insert { rows, .. } => {
                assert_eq!(rows[0][0], Expr::Literal(SqlValue::Int(-42)));
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn parse_null_literal() {
        let stmts = Parser::parse("INSERT INTO t VALUES (NULL)").unwrap();
        match &stmts[0] {
            Statement::Insert { rows, .. } => {
                assert_eq!(rows[0][0], Expr::Literal(SqlValue::Null));
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn syntax_error_on_bad_input() {
        assert!(Parser::parse("SELEKT x").is_err());
        assert!(Parser::parse("").is_err());
        assert!(Parser::parse("CREATE TABLE").is_err());
    }

    #[test]
    fn case_insensitive_keywords() {
        let stmts = Parser::parse("select * from t").unwrap();
        assert!(matches!(stmts[0], Statement::Select(_)));
    }

    #[test]
    fn parse_nested_parens_in_where() {
        let stmts = Parser::parse("SELECT * FROM t WHERE (a = 1 AND b = 2)").unwrap();
        match &stmts[0] {
            Statement::Select(sel) => assert!(sel.selection.is_some()),
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn e2e_create_insert_select() {
        // Full round-trip parse: parse SQL, verify AST structure
        let stmts =
            Parser::parse("CREATE TABLE users (id INTEGER NOT NULL, name TEXT, salary INTEGER)")
                .unwrap();
        assert_eq!(stmts.len(), 1);

        let stmts =
            Parser::parse("INSERT INTO users VALUES (1, 'Alice', 90000), (2, 'Bob', 85000)")
                .unwrap();
        match &stmts[0] {
            Statement::Insert { rows, .. } => assert_eq!(rows.len(), 2),
            other => panic!("expected Insert, got {other:?}"),
        }

        let stmts = Parser::parse(
            "SELECT name FROM users INNER JOIN dept ON id = dept_id WHERE salary > 80000 LIMIT 5",
        )
        .unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection.len(), 1);
                assert!(matches!(&sel.from, TableRef::Join { .. }));
                assert!(sel.selection.is_some());
                assert_eq!(sel.limit, Some(5));
            }
            other => panic!("expected Select, got {other:?}"),
        }

        let stmts = Parser::parse(
            "SELECT dept, COUNT(*), AVG(salary) FROM employees GROUP BY dept LIMIT 10",
        )
        .unwrap();
        match &stmts[0] {
            Statement::Select(sel) => {
                assert_eq!(sel.projection.len(), 3);
                assert_eq!(sel.group_by.len(), 1);
                assert_eq!(sel.limit, Some(10));
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }
}
