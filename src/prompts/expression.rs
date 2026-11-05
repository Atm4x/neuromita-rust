use std::collections::BTreeMap;

use serde_json::{Number, Value};

use super::PromptError;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Name(String),
    Literal(Value),
    Operator(String),
    Left,
    Right,
    Comma,
    End,
}

fn lex(input: &str) -> Result<Vec<Token>, PromptError> {
    let mut out = Vec::new();
    let mut it = input.char_indices().peekable();
    while let Some((pos, ch)) = it.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut end = pos + ch.len_utf8();
            while let Some(&(idx, next)) = it.peek() {
                if !next.is_ascii_alphanumeric() && next != '_' {
                    break;
                }
                it.next();
                end = idx + next.len_utf8();
            }
            let text = &input[pos..end];
            out.push(match text {
                "True" | "true" => Token::Literal(Value::Bool(true)),
                "False" | "false" => Token::Literal(Value::Bool(false)),
                "None" | "none" => Token::Literal(Value::Null),
                "and" | "AND" | "or" | "OR" | "not" | "NOT" => Token::Operator(text.to_lowercase()),
                _ => Token::Name(text.to_owned()),
            });
            continue;
        }
        if ch.is_ascii_digit() {
            let mut end = pos + 1;
            while let Some(&(idx, next)) = it.peek() {
                if !next.is_ascii_digit() && next != '.' {
                    break;
                }
                it.next();
                end = idx + next.len_utf8();
            }
            let value = serde_json::from_str(&input[pos..end])
                .map_err(|_| PromptError::Dsl(format!("invalid number: {}", &input[pos..end])))?;
            out.push(Token::Literal(value));
            continue;
        }
        if ch == '\'' || ch == '"' {
            let mut ahead = it.clone();
            let multiline = ahead.next().is_some_and(|(_, c)| c == ch)
                && ahead.next().is_some_and(|(_, c)| c == ch);
            if multiline {
                it.next();
                it.next();
            }
            let mut escaped = false;
            let mut value = String::new();
            let mut closed = false;
            while let Some((_, next)) = it.next() {
                if escaped {
                    value.push(match next {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == ch {
                    if !multiline {
                        closed = true;
                        break;
                    }
                    let mut ahead = it.clone();
                    if ahead.next().is_some_and(|(_, c)| c == ch)
                        && ahead.next().is_some_and(|(_, c)| c == ch)
                    {
                        it.next();
                        it.next();
                        closed = true;
                        break;
                    }
                    value.push(ch);
                } else {
                    value.push(next);
                }
            }
            if !closed {
                return Err(PromptError::Dsl("unterminated string literal".into()));
            }
            out.push(Token::Literal(Value::String(value)));
            continue;
        }
        match ch {
            '(' => out.push(Token::Left),
            ')' => out.push(Token::Right),
            ',' => out.push(Token::Comma),
            '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' => {
                let mut text = ch.to_string();
                if let Some(&(_, '=')) = it.peek() {
                    if matches!(ch, '<' | '>' | '=' | '!') {
                        it.next();
                        text.push('=');
                    }
                }
                out.push(Token::Operator(text));
            }
            _ => {
                return Err(PromptError::Dsl(format!(
                    "invalid expression token at {pos}: {ch}"
                )));
            }
        }
    }
    out.push(Token::End);
    Ok(out)
}

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => {
            if *value {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => value.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

pub(super) fn evaluate(expr: &str, vars: &BTreeMap<String, Value>) -> Result<Value, PromptError> {
    let trimmed = expr.trim();
    if trimmed.starts_with("f\"") || trimmed.starts_with("f'") {
        return f_string(&trimmed[1..], vars).map(Value::String);
    }
    let tokens = lex(trimmed)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        vars,
    };
    let result = parser.parse(0)?;
    if parser.current() != &Token::End {
        return Err(PromptError::Dsl(format!("unconsumed expression: {expr}")));
    }
    Ok(result)
}

fn f_string(quoted: &str, vars: &BTreeMap<String, Value>) -> Result<String, PromptError> {
    let literal = match lex(quoted)?.first() {
        Some(Token::Literal(Value::String(value))) => value.clone(),
        _ => return Err(PromptError::Dsl("invalid f-string".into())),
    };
    let mut result = String::new();
    let mut chars = literal.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            result.push(ch);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            result.push('{');
            continue;
        }
        let mut expr = String::new();
        let mut finished = false;
        for next in chars.by_ref() {
            if next == '}' {
                finished = true;
                break;
            }
            expr.push(next);
        }
        if !finished {
            return Err(PromptError::Dsl("unterminated f-string variable".into()));
        }
        result.push_str(&text(&evaluate(&expr, vars)?));
    }
    Ok(result.replace("}}", "}"))
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    vars: &'a BTreeMap<String, Value>,
}
impl Parser<'_> {
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn next(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }
    fn parse(&mut self, min_bp: u8) -> Result<Value, PromptError> {
        let mut left =
            match self.next() {
                Token::Literal(value) => value,
                Token::Name(name) => {
                    if self.current() == &Token::Left {
                        self.next();
                        let mut args = Vec::new();
                        if self.current() != &Token::Right {
                            loop {
                                args.push(self.parse(0)?);
                                if self.current() != &Token::Comma {
                                    break;
                                }
                                self.next();
                            }
                        }
                        if self.next() != Token::Right {
                            return Err(PromptError::Dsl("function call missing ')'".into()));
                        }
                        if name == "default" && args.len() == 2 {
                            let key = args[0].as_str().ok_or_else(|| {
                                PromptError::Dsl("default() requires a string variable name".into())
                            })?;
                            self.vars
                                .get(key)
                                .cloned()
                                .unwrap_or_else(|| args[1].clone())
                        } else {
                            call(&name, args)?
                        }
                    } else {
                        self.vars.get(&name).cloned().unwrap_or(Value::Null)
                    }
                }
                Token::Left => {
                    let value = self.parse(0)?;
                    if self.next() != Token::Right {
                        return Err(PromptError::Dsl("missing ')'".into()));
                    }
                    value
                }
                Token::Operator(op) if op == "not" || op == "-" || op == "+" => {
                    let value = self.parse(11)?;
                    match op.as_str() {
                        "not" => Value::Bool(!truthy(&value)),
                        "+" => value,
                        _ => number(-value.as_f64().ok_or_else(|| {
                            PromptError::Dsl("unary '-' requires number".into())
                        })?)?,
                    }
                }
                other => {
                    return Err(PromptError::Dsl(format!(
                        "expected expression, found {other:?}"
                    )));
                }
            };
        loop {
            let Token::Operator(op) = self.current() else {
                break;
            };
            let (left_bp, right_bp) = match op.as_str() {
                "or" => (1, 2),
                "and" => (3, 4),
                "==" | "!=" | "<" | ">" | "<=" | ">=" => (5, 6),
                "+" | "-" => (7, 8),
                "*" | "/" | "%" => (9, 10),
                _ => return Err(PromptError::Dsl(format!("unknown operator: {op}"))),
            };
            if left_bp < min_bp {
                break;
            }
            let op = op.clone();
            self.next();
            let right = self.parse(right_bp)?;
            left = binary(&op, left, right)?;
        }
        Ok(left)
    }
}
fn number(value: f64) -> Result<Value, PromptError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| PromptError::Dsl("non-finite numeric result".into()))
}
fn binary(op: &str, left: Value, right: Value) -> Result<Value, PromptError> {
    if op == "and" {
        return Ok(Value::Bool(truthy(&left) && truthy(&right)));
    }
    if op == "or" {
        return Ok(Value::Bool(truthy(&left) || truthy(&right)));
    }
    match op {
        "==" => {
            return Ok(Value::Bool(if left.is_number() && right.is_number() {
                left.as_f64() == right.as_f64()
            } else {
                left == right
            }));
        }
        "!=" => {
            return Ok(Value::Bool(if left.is_number() && right.is_number() {
                left.as_f64() != right.as_f64()
            } else {
                left != right
            }));
        }
        _ => {}
    }
    if op == "+" && left.is_string() && right.is_string() {
        return Ok(Value::String(format!("{}{}", text(&left), text(&right))));
    }
    if let (Some(a), Some(b)) = (left.as_i64(), right.as_i64()) {
        let result = match op {
            "+" => a.checked_add(b),
            "-" => a.checked_sub(b),
            "*" => a.checked_mul(b),
            "%" if b != 0 => Some(a.rem_euclid(b)),
            _ => None,
        };
        if let Some(result) = result {
            return Ok(Value::from(result));
        }
    }
    let a = left
        .as_f64()
        .ok_or_else(|| PromptError::Dsl(format!("{op} requires numeric arguments")))?;
    let b = right
        .as_f64()
        .ok_or_else(|| PromptError::Dsl(format!("{op} requires numeric arguments")))?;
    Ok(match op {
        "<" => Value::Bool(a < b),
        ">" => Value::Bool(a > b),
        "<=" => Value::Bool(a <= b),
        ">=" => Value::Bool(a >= b),
        "+" => number(a + b)?,
        "-" => number(a - b)?,
        "*" => number(a * b)?,
        "/" => {
            if b == 0.0 {
                return Err(PromptError::Dsl("division by zero".into()));
            }
            number(a / b)?
        }
        "%" => {
            if b == 0.0 {
                return Err(PromptError::Dsl("modulo by zero".into()));
            }
            number(a % b)?
        }
        _ => return Err(PromptError::Dsl(format!("invalid operator: {op}"))),
    })
}
fn call(name: &str, args: Vec<Value>) -> Result<Value, PromptError> {
    match (name, args.as_slice()) {
        ("str", [v]) => Ok(Value::String(text(v))),
        ("int", [v]) => v
            .as_i64()
            .or_else(|| {
                if let Value::Number(n) = v {
                    n.as_f64().and_then(|v| {
                        if v.is_finite() && v >= i64::MIN as f64 && v < i64::MAX as f64 {
                            Some(v.trunc() as i64)
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            })
            .or_else(|| text(v).parse().ok())
            .map(|v| Value::Number(v.into()))
            .ok_or_else(|| PromptError::Dsl("int() conversion failed".into())),
        ("float", [v]) => v
            .as_f64()
            .or_else(|| text(v).parse().ok())
            .ok_or_else(|| PromptError::Dsl("float() conversion failed".into()))
            .and_then(number),
        ("len", [Value::String(v)]) => Ok(Value::Number(v.chars().count().into())),
        ("len", [Value::Array(v)]) => Ok(Value::Number(v.len().into())),
        ("abs", [v]) => number(
            v.as_f64()
                .ok_or_else(|| PromptError::Dsl("abs() expects number".into()))?
                .abs(),
        ),
        ("round", [v]) => v
            .as_f64()
            .and_then(|v| {
                let rounded = v.round();
                (rounded >= i64::MIN as f64 && rounded < i64::MAX as f64)
                    .then_some(Value::from(rounded as i64))
            })
            .ok_or_else(|| PromptError::Dsl("round() expects finite bounded number".into())),
        ("round", [v, digits]) => {
            let digits = digits
                .as_i64()
                .ok_or_else(|| PromptError::Dsl("round() digits must be integer".into()))?;
            let factor = 10_f64.powi(
                i32::try_from(digits)
                    .map_err(|_| PromptError::Dsl("round() digits out of range".into()))?,
            );
            let rounded = (v
                .as_f64()
                .ok_or_else(|| PromptError::Dsl("round() expects number".into()))?
                * factor)
                .round()
                / factor;
            if v.is_i64()
                && rounded.is_finite()
                && rounded >= i64::MIN as f64
                && rounded < i64::MAX as f64
            {
                Ok(Value::from(rounded as i64))
            } else {
                number(rounded)
            }
        }
        ("random", []) => number(uuid::Uuid::new_v4().as_u128() as f64 / u128::MAX as f64),
        ("random", [a, b]) => {
            let min = a
                .as_i64()
                .ok_or_else(|| PromptError::Dsl("random bounds must be integers".into()))?;
            let max = b
                .as_i64()
                .ok_or_else(|| PromptError::Dsl("random bounds must be integers".into()))?;
            let span = max
                .checked_sub(min)
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| PromptError::Dsl("random bounds invalid".into()))?;
            let sample = (uuid::Uuid::new_v4().as_u128() % (span as u128)) as i64;
            Ok(Value::from(min + sample))
        }
        ("min", [a, b]) => binary("<", a.clone(), b.clone()).map(|v| {
            if v == Value::Bool(true) {
                a.clone()
            } else {
                b.clone()
            }
        }),
        ("max", [a, b]) => binary(">", a.clone(), b.clone()).map(|v| {
            if v == Value::Bool(true) {
                a.clone()
            } else {
                b.clone()
            }
        }),
        _ => Err(PromptError::Dsl(format!(
            "unsupported function or arguments: {name}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evaluates_python_like_basics() {
        let vars = BTreeMap::from([
            ("attitude".into(), Value::from(34)),
            ("name".into(), Value::from("Mita")),
        ]);
        assert_eq!(
            evaluate("attitude < 40 AND name == 'Mita'", &vars).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate("'hello ' + name", &vars).unwrap(),
            Value::String("hello Mita".into())
        );
        assert_eq!(
            evaluate("f'Hey {name}: {attitude}'", &vars).unwrap(),
            Value::String("Hey Mita: 34".into())
        );
        assert!(evaluate("__import__('os')", &vars).is_err());
        assert_eq!(
            evaluate("undefined == None", &vars).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate("f\"\"\"Hi {name}\nBye\"\"\"", &vars).unwrap(),
            Value::String("Hi Mita\nBye".into())
        );
    }
}
