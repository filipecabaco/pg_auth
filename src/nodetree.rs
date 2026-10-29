//! Parser for Postgres' text serialization of node trees (`pg_node_tree`, as stored in
//! pg_policy.polqual), so expressions can be analysed as plain Rust values without touching
//! Postgres memory.
//!
//! Grammar (src/backend/nodes/outfuncs.c):
//! - node: `{TAG :field value ...}`
//! - list: `( ... )`, optionally typed `(b 1 2)`, `(i ...)`, `(o ...)`
//! - NULL: `<>`
//! - string: `"..."` or a bare token, with `\` escaping the next character
//! - datum: a length followed by `[ b0 b1 ... ]`

#![forbid(unsafe_code)]

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Node(Node),
    List(Vec<Value>),
    Atom(String),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub tag: String,
    pub fields: Vec<(String, Value)>,
}

impl Node {
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == field).map(|(_, v)| v)
    }

    pub fn int(&self, field: &str) -> Option<i64> {
        match self.get(field)? {
            Value::Atom(a) => a.parse().ok(),
            _ => None,
        }
    }
}

impl Value {
    pub fn node(&self) -> Option<&Node> {
        match self {
            Value::Node(n) => Some(n),
            _ => None,
        }
    }

    pub fn items(&self) -> &[Value] {
        match self {
            Value::List(items) => items,
            _ => &[],
        }
    }

    /// Every direct child value: a node's field values or a list's items.
    pub fn children(&self) -> Vec<&Value> {
        match self {
            Value::Node(n) => n.fields.iter().map(|(_, v)| v).collect(),
            Value::List(items) => items.iter().collect(),
            _ => Vec::new(),
        }
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { chars: text.chars().collect(), pos: 0 };
    let v = p.value()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(format!("trailing input at {}", p.pos));
    }
    Ok(v)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {c:?} at {}", self.pos))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.node().map(Value::Node),
            Some('(') => self.list(),
            Some('<') if self.chars.get(self.pos + 1) == Some(&'>') => {
                self.pos += 2;
                Ok(Value::Null)
            }
            Some('[') => self.bytes(),
            Some('"') => self.quoted(),
            Some(_) => self.token().map(Value::Atom),
            None => Err("unexpected end of input".into()),
        }
    }

    fn node(&mut self) -> Result<Node, String> {
        self.expect('{')?;
        let tag = self.token()?;
        let mut fields: Vec<(String, Value)> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    return Ok(Node { tag, fields });
                }
                Some(':') => {
                    self.pos += 1;
                    let name = self.token()?;
                    let value = self.value()?;
                    fields.push((name, value));
                }
                // A datum's bytes follow its length: keep both under the same field.
                Some('[') => {
                    let bytes = self.bytes()?;
                    if let Some((_, last)) = fields.last_mut() {
                        *last = Value::List(vec![last.clone(), bytes]);
                    }
                }
                _ => return Err(format!("expected field or '}}' at {}", self.pos)),
            }
        }
    }

    fn list(&mut self) -> Result<Value, String> {
        self.expect('(')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(')') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                None => return Err("unterminated list".into()),
                _ => items.push(self.value()?),
            }
        }
    }

    fn bytes(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                None => return Err("unterminated datum".into()),
                _ => items.push(Value::Atom(self.token()?)),
            }
        }
    }

    fn quoted(&mut self) -> Result<Value, String> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".into()),
                Some('\\') => {
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        s.push(c);
                        self.pos += 1;
                    }
                }
                Some('"') => {
                    self.pos += 1;
                    return Ok(Value::Atom(s));
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn token(&mut self) -> Result<String, String> {
        self.skip_ws();
        let mut s = String::new();
        while let Some(c) = self.peek() {
            match c {
                '\\' => {
                    self.pos += 1;
                    if let Some(next) = self.peek() {
                        s.push(next);
                        self.pos += 1;
                    }
                }
                c if c.is_whitespace() || matches!(c, '(' | ')' | '{' | '}' | '[' | ']') => break,
                c => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
        if s.is_empty() { Err(format!("expected a token at {}", self.pos)) } else { Ok(s) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_policy_expression() {
        let text = r"{OPEXPR :opno 98 :args ({VAR :varno 1 :varattno 2 :varnullingrels (b) :varlevelsup 0} {CONST :consttype 25 :constisnull false :constvalue 6 [ 24 0 0 0 104 105 ]}) :location -1}";
        let v = parse(text).unwrap();
        let op = v.node().unwrap();
        assert_eq!(op.tag, "OPEXPR");
        assert_eq!(op.int("opno"), Some(98));
        let args = op.get("args").unwrap().items();
        assert_eq!(args[0].node().unwrap().int("varattno"), Some(2));
        assert_eq!(args[1].node().unwrap().tag, "CONST");
    }

    #[test]
    fn parses_nulls_quoted_strings_and_escapes() {
        let v = parse(r#"{ALIAS :aliasname my\ table :colnames ("id" "a\"b") :x <>}"#).unwrap();
        let n = v.node().unwrap();
        assert_eq!(n.get("aliasname"), Some(&Value::Atom("my table".into())));
        assert_eq!(n.get("colnames").unwrap().items()[1], Value::Atom("a\"b".into()));
        assert_eq!(n.get("x"), Some(&Value::Null));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{OPEXPR :opno").is_err());
        assert!(parse("(1 2").is_err());
    }
}
