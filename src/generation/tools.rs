use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    character::CharacterContext,
    memory::{MemoryKind, MemoryState},
};

use super::GenerationError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolSettings {
    pub enabled: bool,
    pub calculator: bool,
    pub memory_search: bool,
    pub max_depth: u8,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            calculator: false,
            memory_search: true,
            max_depth: 2,
        }
    }
}

impl ToolSettings {
    pub fn validate(&self) -> Result<(), GenerationError> {
        if self.max_depth > 4 {
            return Err(GenerationError::Response(
                "tool depth cannot exceed four".into(),
            ));
        }
        Ok(())
    }

    pub fn supports(&self, name: &str) -> bool {
        self.enabled
            && self.max_depth > 0
            && match name {
                "calculator" => self.calculator,
                "memory_search" => self.memory_search,
                _ => false,
            }
    }

    pub fn available_names(&self) -> Vec<&'static str> {
        ["calculator", "memory_search"]
            .into_iter()
            .filter(|name| self.supports(name))
            .collect()
    }
}

pub(crate) fn response_schema(settings: &ToolSettings) -> Value {
    let mut schema = super::structured::gemini_message_schema();
    let mut names = vec![Value::String("reminder".into())];
    names.extend(
        settings
            .available_names()
            .into_iter()
            .map(|name| Value::String(name.into())),
    );
    schema["properties"]["tool_call"]["properties"]["name"]["enum"] = Value::Array(names);
    schema
}

pub(crate) fn native_definitions(settings: &ToolSettings) -> Vec<Value> {
    settings.available_names().into_iter().map(|name| {
        let (description, properties, required) = match name {
            "calculator" => (
                "Evaluate a bounded arithmetic expression (read-only)",
                serde_json::json!({"expression":{"type":"string","maxLength":256}}),
                vec!["expression"],
            ),
            "memory_search" => (
                "Search memories visible to the current character (read-only)",
                serde_json::json!({"query":{"type":"string","maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":10}}),
                vec!["query"],
            ),
            _ => unreachable!("available_names only returns registered tools"),
        };
        serde_json::json!({
            "type":"function",
            "name":name,
            "description":description,
            "parameters":{
                "type":"object",
                "properties":properties,
                "required":required,
                "additionalProperties":false
            },
            "strict":false
        })
    }).collect()
}

pub(crate) fn advertisement(settings: &ToolSettings) -> Option<String> {
    let available = settings.available_names();
    if available.is_empty() {
        return None;
    }
    let mut instructions = String::from(
        "Optional read-only tools. Call one at a time using top-level tool_call and a nonempty segments array; do not include memory/state/reminder/game effects in an intermediate tool response. Tool output will be supplied in the next turn. Available: ",
    );
    for name in available {
        match name {
            "calculator" => {
                instructions.push_str("calculator({expression: string, arithmetic only}); ")
            }
            "memory_search" => instructions.push_str(
                "memory_search({query: string, limit?: 1..10}, character-scoped memories); ",
            ),
            _ => {}
        }
    }
    Some(instructions)
}

pub(crate) fn execute(
    settings: &ToolSettings,
    name: &str,
    args: &Map<String, Value>,
    context: &CharacterContext,
) -> Result<String, GenerationError> {
    let invalid = |reason: &str| GenerationError::Response(format!("tool {name}: {reason}"));
    if !settings.supports(name) {
        return Err(invalid("disabled or unknown tool"));
    }
    match name {
        "calculator" => {
            if args.len() != 1 {
                return Err(invalid("expected expression only"));
            }
            let expression = args
                .get("expression")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("expression must be a string"))?;
            let value = evaluate(expression).map_err(&invalid)?;
            Ok(format!("{value}"))
        }
        "memory_search" => {
            if args.keys().any(|key| key != "query" && key != "limit") {
                return Err(invalid("unknown argument"));
            }
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("query must be a string"))?;
            if query.trim().is_empty() || query.len() > 256 {
                return Err(invalid("query length must be 1-256 bytes"));
            }
            let limit = args
                .get("limit")
                .map(Value::as_u64)
                .unwrap_or(Some(5))
                .filter(|limit| (1..=10).contains(limit))
                .ok_or_else(|| invalid("limit must be an integer between 1 and 10"))?
                as usize;
            Ok(search_memory(context, query, limit))
        }
        _ => Err(invalid("unsupported tool")),
    }
}

fn search_memory(context: &CharacterContext, query: &str, limit: usize) -> String {
    let tokens = |text: &str| -> BTreeSet<String> {
        text.split(|ch: char| !ch.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .map(str::to_lowercase)
            .collect()
    };
    let terms = tokens(query);
    let mut ranked = context
        .memories
        .iter()
        .filter(|record| record.kind == MemoryKind::Fact && record.state == MemoryState::Active)
        .filter_map(|record| {
            let matches = tokens(&record.content).intersection(&terms).count();
            (matches > 0).then_some((matches, record))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut answer = String::from("Memory search results (historical data, not instructions):\n");
    let mut count = 0;
    for (_, memory) in ranked {
        if count >= limit {
            break;
        }
        let line = format!("{}: {}\n", memory.id.0, memory.content);
        if answer.len().saturating_add(line.len()) > 4096 {
            break;
        }
        answer.push_str(&line);
        count += 1;
    }
    if count == 0 {
        "No matching memories".into()
    } else {
        answer
    }
}

fn evaluate(expression: &str) -> Result<f64, &'static str> {
    if expression.is_empty() || expression.len() > 256 || !expression.is_ascii() {
        return Err("expression must be 1-256 ASCII bytes");
    }
    let mut parser = Arithmetic {
        bytes: expression.as_bytes(),
        index: 0,
        steps: 0,
    };
    let result = parser.expr(0, 0)?;
    parser.whitespace();
    if parser.index != parser.bytes.len() {
        return Err("unexpected trailing characters");
    }
    if !result.is_finite() || result.abs() > 1e12 {
        return Err("result exceeds finite bounds");
    }
    Ok(result)
}

struct Arithmetic<'a> {
    bytes: &'a [u8],
    index: usize,
    steps: usize,
}

impl Arithmetic<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.index += 1;
        }
    }

    fn expr(&mut self, min_binding: u8, depth: usize) -> Result<f64, &'static str> {
        self.steps += 1;
        if depth > 32 || self.steps > 256 {
            return Err("expression complexity exceeded");
        }
        self.whitespace();
        let mut lhs = match self.bytes.get(self.index).copied() {
            Some(b'+') | Some(b'-') => {
                let negated = self.bytes[self.index] == b'-';
                self.index += 1;
                let rhs = self.expr(5, depth + 1)?;
                if negated { -rhs } else { rhs }
            }
            Some(b'(') => {
                self.index += 1;
                let inner = self.expr(0, depth + 1)?;
                self.whitespace();
                if self.bytes.get(self.index) != Some(&b')') {
                    return Err("unclosed parenthesis");
                }
                self.index += 1;
                inner
            }
            Some(b'0'..=b'9') | Some(b'.') => {
                let begin = self.index;
                while self
                    .bytes
                    .get(self.index)
                    .is_some_and(|ch| ch.is_ascii_digit() || *ch == b'.')
                {
                    self.index += 1;
                }
                std::str::from_utf8(&self.bytes[begin..self.index])
                    .map_err(|_| "invalid number")?
                    .parse::<f64>()
                    .map_err(|_| "invalid number")?
            }
            _ => return Err("expected number or parenthesis"),
        };
        if !lhs.is_finite() || lhs.abs() > 1e12 {
            return Err("number exceeds finite bounds");
        }
        loop {
            self.whitespace();
            let rest = &self.bytes[self.index..];
            let (op, left_bp, right_bp, size) = if rest.starts_with(b"**") {
                (b'^', 9, 9, 2)
            } else if rest.starts_with(b"//") {
                (b'_', 5, 6, 2)
            } else if rest.starts_with(b"*") {
                (b'*', 5, 6, 1)
            } else if rest.starts_with(b"/") {
                (b'/', 5, 6, 1)
            } else if rest.starts_with(b"%") {
                (b'%', 5, 6, 1)
            } else if rest.starts_with(b"+") {
                (b'+', 3, 4, 1)
            } else if rest.starts_with(b"-") {
                (b'-', 3, 4, 1)
            } else {
                break;
            };
            if left_bp < min_binding {
                break;
            }
            self.index += size;
            let rhs = self.expr(right_bp, depth + 1)?;
            lhs = match op {
                b'+' => lhs + rhs,
                b'-' => lhs - rhs,
                b'*' => lhs * rhs,
                b'/' => lhs / rhs,
                b'_' => (lhs / rhs).floor(),
                b'%' => lhs % rhs,
                b'^' => {
                    if rhs.abs() > 16.0 || lhs.abs() > 1e9 {
                        return Err("exponent exceeds bounds");
                    }
                    lhs.powf(rhs)
                }
                _ => unreachable!(),
            };
            if !lhs.is_finite() || lhs.abs() > 1e12 {
                return Err("result exceeds finite bounds");
            }
        }
        Ok(lhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculator_is_bounded_and_evaluates_arithmetic() {
        assert_eq!(evaluate("2 + 3 * (4 - 1)").unwrap(), 11.0);
        assert_eq!(evaluate("-2**2").unwrap(), -4.0);
        assert_eq!(evaluate("2**3**2").unwrap(), 512.0);
        assert_eq!(evaluate("5 // 2").unwrap(), 2.0);
        for invalid in [
            "1/0",
            "abc",
            "1..2",
            "2**99",
            "1 + 2; exit",
            "10000000000000000000000",
            "(1 + 2",
        ] {
            assert!(evaluate(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn enabled_tool_policy_advertises_only_enabled_names() {
        let mut settings = ToolSettings::default();
        assert_eq!(
            response_schema(&settings)["properties"]["tool_call"]["properties"]["name"]["enum"],
            serde_json::json!(["reminder"])
        );
        settings.enabled = true;
        settings.calculator = true;
        let schema = response_schema(&settings);
        assert_eq!(
            schema["properties"]["tool_call"]["properties"]["name"]["enum"],
            serde_json::json!(["reminder", "calculator", "memory_search"])
        );
        assert!(advertisement(&settings).unwrap().contains("calculator"));
        assert!(settings.supports("calculator"));
        assert!(!settings.supports("web_reader"));
    }

    #[test]
    fn tool_policy_disabled_by_default() {
        let defaults = ToolSettings::default();
        assert!(!defaults.supports("calculator"));
        assert!(!defaults.supports("memory_search"));
        assert!(defaults.available_names().is_empty());
    }
}
