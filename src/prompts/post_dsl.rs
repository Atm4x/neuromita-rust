use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use super::{PromptError, expression};
use crate::generation::StructuredResponse;

#[derive(Debug, Clone)]
pub struct PostDslProgram {
    rules: Vec<Rule>,
    debug_display: BTreeMap<String, String>,
    defaults: BTreeMap<String, Value>,
    custom_params: Vec<CustomParam>,
}

#[derive(Debug, Clone)]
struct Rule {
    name: String,
    matcher: Matcher,
    actions: Vec<Action>,
}

#[derive(Debug, Clone)]
enum Matcher {
    Text(String),
    Regex {
        pattern: Regex,
        captures: Vec<String>,
    },
    Field(String),
}

#[derive(Debug, Clone)]
enum Action {
    Set {
        name: String,
        expression: String,
        local: bool,
    },
    Log(String),
    Remove,
    Replace(String),
}

#[derive(Debug, Clone, Deserialize)]
struct CustomParam {
    name: String,
    change_command: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    op: Option<String>,
    formula: Option<String>,
    change_min: Option<f64>,
    change_max: Option<f64>,
    max_change: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    initial: Option<Value>,
}

impl CustomParam {
    fn command(&self) -> &str {
        self.change_command.as_deref().unwrap_or(&self.name)
    }

    fn apply(
        &self,
        raw: &Value,
        vars: &mut BTreeMap<String, Value>,
    ) -> Result<Option<Value>, PromptError> {
        if raw.is_null() {
            return Ok(None);
        }
        let mut value = match self.kind.as_deref().unwrap_or("float") {
            "float" | "double" | "number" => Value::from(as_number(raw)?),
            "int" | "integer" => {
                let num = as_number(raw)?;
                if !num.is_finite() || num < i64::MIN as f64 || num >= i64::MAX as f64 {
                    return Err(error("custom integer out of range"));
                }
                Value::from(num.trunc() as i64)
            }
            _ => raw.clone(),
        };
        if value.is_number() {
            value = bounded(value, self.change_min, self.change_max)?;
            if self.op.as_deref() == Some("add")
                && let Some(max) = self.max_change
            {
                if !max.is_finite() || max < 0.0 {
                    return Err(error("invalid max_change"));
                }
                value = bounded(value, Some(-max), Some(max))?;
            }
        }
        let current = vars.get(&self.name).cloned().unwrap_or(Value::from(0));
        let result = if let Some(formula) = &self.formula {
            let mut scope = vars.clone();
            scope.insert("current".into(), current.clone());
            scope.insert("value".into(), value.clone());
            scope.insert(self.command().into(), value.clone());
            scope.insert(self.name.clone(), current);
            expression::evaluate(formula, &scope)?
        } else {
            match self.op.as_deref() {
                Some("set") => value,
                Some("add") => {
                    if current.is_number() && value.is_number() {
                        let result = as_number(&current)? + as_number(&value)?;
                        Value::from(result)
                    } else {
                        return Err(error("non-numeric custom parameter add"));
                    }
                }
                _ => return Err(error("custom parameter needs op or formula")),
            }
        };
        Ok(Some(if result.is_number() {
            bounded(result, self.min, self.max)?
        } else {
            result
        }))
    }
}

fn as_number(value: &Value) -> Result<f64, PromptError> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|number: &f64| number.is_finite())
        .ok_or_else(|| error("custom parameter requires finite numeric value"))
}

fn bounded(value: Value, min: Option<f64>, max: Option<f64>) -> Result<Value, PromptError> {
    let mut number = as_number(&value)?;
    if let Some(min) = min {
        if !min.is_finite() {
            return Err(error("non-finite custom minimum"));
        }
        number = number.max(min);
    }
    if let Some(max) = max {
        if !max.is_finite() {
            return Err(error("non-finite custom maximum"));
        }
        number = number.min(max);
    }
    serde_json::Number::from_f64(number)
        .map(Value::Number)
        .ok_or_else(|| error("non-finite custom parameter result"))
}

#[derive(Debug, Clone)]
pub struct PostDslResult {
    pub response: StructuredResponse,
    pub variables: BTreeMap<String, Value>,
    pub logs: Vec<String>,
}

impl PostDslProgram {
    pub fn parse(script: &str, defaults: BTreeMap<String, Value>) -> Result<Self, PromptError> {
        if script.len() > 128 * 1024 {
            return Err(error("post-script exceeds size limit"));
        }
        let mut rules = Vec::new();
        let mut debug_display = BTreeMap::new();
        let mut builder: Option<(String, Option<Matcher>, Vec<Action>, bool)> = None;
        let mut debug = false;
        for (index, original) in script.lines().enumerate() {
            let line = original.split("//").next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let upper = line.to_ascii_uppercase();
            if upper == "DEBUG_DISPLAY" {
                if builder.is_some() {
                    return Err(error("DEBUG_DISPLAY inside RULE"));
                }
                debug = true;
                continue;
            }
            if upper == "END_DEBUG_DISPLAY" {
                if !debug {
                    return Err(error("unexpected END_DEBUG_DISPLAY"));
                }
                debug = false;
                continue;
            }
            if debug {
                let (label, name) = line
                    .split_once(':')
                    .ok_or_else(|| error("invalid DEBUG_DISPLAY entry"))?;
                let label = label.trim().trim_matches('"');
                let name = name.trim();
                if label.is_empty() || !valid_name(name) {
                    return Err(error("invalid DEBUG_DISPLAY name"));
                }
                debug_display.insert(label.into(), name.into());
                continue;
            }
            if let Some(name) = strip_prefix_ci(line, "RULE ") {
                if builder.is_some() {
                    return Err(error("nested RULE"));
                }
                if rules.len() >= 128 || name.trim().is_empty() {
                    return Err(error("invalid rule declaration"));
                }
                builder = Some((name.trim().to_owned(), None, Vec::new(), false));
                continue;
            }
            if upper == "END_RULE" {
                let (name, matcher, actions, in_actions) =
                    builder.take().ok_or_else(|| error("orphan END_RULE"))?;
                if in_actions {
                    return Err(error("missing END_ACTIONS"));
                }
                let matcher = matcher.ok_or_else(|| error("RULE missing MATCH"))?;
                rules.push(Rule {
                    name,
                    matcher,
                    actions,
                });
                continue;
            }
            let (name, matcher, actions, in_actions) = builder
                .as_mut()
                .ok_or_else(|| error(&format!("unexpected line {}", index + 1)))?;
            if let Some(rest) = strip_prefix_ci(line, "MATCH ") {
                if matcher.is_some() || *in_actions {
                    return Err(error("duplicate MATCH"));
                }
                let (kind, raw) = rest
                    .split_once(' ')
                    .ok_or_else(|| error("MATCH missing type or pattern"))?;
                let (pattern, captures) = if let Some((before, after)) = split_capture(raw) {
                    let names = after
                        .split(',')
                        .map(str::trim)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if names.iter().any(|name| !valid_name(name)) {
                        return Err(error("invalid CAPTURE name"));
                    }
                    (before, names)
                } else {
                    (raw, Vec::new())
                };
                let pattern = pattern.trim();
                if !pattern.starts_with('"') || !pattern.ends_with('"') || pattern.len() < 2 {
                    return Err(error("MATCH pattern must be quoted"));
                }
                let pattern = &pattern[1..pattern.len() - 1];
                if pattern.len() > 8192 {
                    return Err(error("MATCH pattern exceeds size limit"));
                }
                *matcher = Some(match kind.to_ascii_uppercase().as_str() {
                    "TEXT" if !pattern.is_empty() && captures.is_empty() => {
                        Matcher::Text(pattern.into())
                    }
                    "REGEX" => {
                        let compiled =
                            Regex::new(pattern).map_err(|e| error(&format!("rule {name}: {e}")))?;
                        if !captures.is_empty() && compiled.captures_len() - 1 != captures.len() {
                            return Err(error("CAPTURE names must match regex group count"));
                        }
                        Matcher::Regex {
                            pattern: compiled,
                            captures,
                        }
                    }
                    "FIELD" if !pattern.is_empty() && captures.is_empty() => {
                        Matcher::Field(pattern.into())
                    }
                    _ => return Err(error("invalid MATCH type or arguments")),
                });
            } else if upper == "ACTIONS" {
                if matcher.is_none() || *in_actions {
                    return Err(error("ACTIONS before MATCH or nested"));
                }
                *in_actions = true;
            } else if upper == "END_ACTIONS" {
                if !*in_actions {
                    return Err(error("unexpected END_ACTIONS"));
                }
                *in_actions = false;
            } else if *in_actions {
                if actions.len() >= 64 {
                    return Err(error("too many rule actions"));
                }
                actions.push(parse_action(line)?);
            } else {
                return Err(error(&format!(
                    "unexpected rule content on line {}",
                    index + 1
                )));
            }
        }
        if builder.is_some() || debug {
            return Err(error("unterminated post-script block"));
        }
        Ok(Self {
            rules,
            debug_display,
            defaults,
            custom_params: Vec::new(),
        })
    }

    pub fn with_custom_params(mut self, params: &[Value]) -> Result<Self, PromptError> {
        if params.len() > 64 {
            return Err(error("too many custom params"));
        }
        for raw in params {
            let param: CustomParam = serde_json::from_value(raw.clone())?;
            if !valid_name(&param.name)
                || !valid_name(param.command())
                || matches!(
                    param.name.as_str(),
                    "CHARACTER_ID"
                        | "CURRENT_INPUT"
                        | "ACCEPTED_REPLIES"
                        | "attitude"
                        | "boredom"
                        | "stress"
                )
            {
                return Err(error("invalid custom parameter name"));
            }
            if let Some(initial) = &param.initial {
                self.defaults
                    .entry(param.name.clone())
                    .or_insert(initial.clone());
            }
            self.custom_params.push(param);
        }
        Ok(self)
    }

    pub fn debug_display(&self) -> &BTreeMap<String, String> {
        &self.debug_display
    }

    pub fn debug_values(&self, context: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        let mut vars = self.defaults.clone();
        vars.extend(context.clone());
        self.debug_display
            .iter()
            .map(|(label, name)| {
                (
                    label.clone(),
                    vars.get(name)
                        .cloned()
                        .unwrap_or(Value::String("N/A".into())),
                )
            })
            .collect()
    }

    pub fn apply(
        &self,
        mut response: StructuredResponse,
        context: &BTreeMap<String, Value>,
    ) -> Result<PostDslResult, PromptError> {
        let mut vars = self.defaults.clone();
        vars.extend(context.clone());
        let mut updates = BTreeMap::new();
        let mut logs = Vec::new();
        let mut remaining_matches = 10_000;
        for segment in &mut response.segments {
            let mut locals = BTreeMap::new();
            let mut declared = BTreeSet::new();
            for rule in &self.rules {
                match &rule.matcher {
                    Matcher::Text(needle) => {
                        let mut output = String::new();
                        let mut remaining = segment.text.as_str();
                        while let Some(pos) = remaining.find(needle) {
                            output.push_str(&remaining[..pos]);
                            let replacement = run(
                                rule,
                                needle,
                                BTreeMap::new(),
                                &mut vars,
                                &mut locals,
                                &mut declared,
                                &mut updates,
                                &mut logs,
                                &mut remaining_matches,
                            )?;
                            output.push_str(&replacement);
                            remaining = &remaining[pos + needle.len()..];
                        }
                        output.push_str(remaining);
                        segment.text = output;
                    }
                    Matcher::Regex { pattern, captures } => {
                        let mut output = String::new();
                        let mut last = 0;
                        for match_info in pattern.captures_iter(&segment.text) {
                            let m = match_info
                                .get(0)
                                .ok_or_else(|| error("regex match missing group 0"))?;
                            output.push_str(&segment.text[last..m.start()]);
                            let mut captured = BTreeMap::new();
                            for idx in 1..match_info.len() {
                                let key = if match_info.len() - 1 == captures.len() {
                                    captures[idx - 1].clone()
                                } else {
                                    format!("capture_{idx}")
                                };
                                let val = match_info.get(idx).map(|m| m.as_str()).unwrap_or("");
                                captured.insert(key, Value::String(val.into()));
                            }
                            output.push_str(&run(
                                rule,
                                m.as_str(),
                                captured,
                                &mut vars,
                                &mut locals,
                                &mut declared,
                                &mut updates,
                                &mut logs,
                                &mut remaining_matches,
                            )?);
                            last = m.end();
                        }
                        output.push_str(&segment.text[last..]);
                        segment.text = output;
                    }
                    Matcher::Field(_) => {}
                }
            }
            if segment.text.len() > 1024 * 1024 {
                return Err(error("post-DSL result exceeds text budget"));
            }
        }
        if let Some(fields) = response.custom_fields.take() {
            let mut locals = BTreeMap::new();
            let mut declared = BTreeSet::new();
            let mut matched = BTreeSet::new();
            for param in &self.custom_params {
                if let Some(value) = fields.get(param.command()) {
                    matched.insert(param.command());
                    if let Some(result) = param.apply(value, &mut vars)? {
                        vars.insert(param.name.clone(), result.clone());
                        updates.insert(param.name.clone(), result);
                    }
                }
            }
            for rule in &self.rules {
                if let Matcher::Field(name) = &rule.matcher
                    && let Some(value) = fields.get(name)
                {
                    matched.insert(name.as_str());
                    run(
                        rule,
                        "",
                        BTreeMap::from([(name.clone(), value.clone())]),
                        &mut vars,
                        &mut locals,
                        &mut declared,
                        &mut updates,
                        &mut logs,
                        &mut remaining_matches,
                    )?;
                }
            }
            if let Some(name) = fields.keys().find(|name| !matched.contains(name.as_str())) {
                return Err(error(&format!("unhandled structured custom field: {name}")));
            }
        }
        Ok(PostDslResult {
            response,
            variables: updates,
            logs,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    rule: &Rule,
    original: &str,
    captured: BTreeMap<String, Value>,
    vars: &mut BTreeMap<String, Value>,
    locals: &mut BTreeMap<String, Value>,
    declared: &mut BTreeSet<String>,
    updates: &mut BTreeMap<String, Value>,
    logs: &mut Vec<String>,
    remaining_matches: &mut usize,
) -> Result<String, PromptError> {
    if *remaining_matches == 0 {
        return Err(error("post-DSL match budget exceeded"));
    }
    *remaining_matches -= 1;
    let mut captured = captured;
    let mut replacement = original.to_owned();
    let mut remove = false;
    let mut replacement_expr = None;
    for action in &rule.actions {
        match action {
            Action::Set {
                name,
                expression,
                local,
            } => {
                let mut scope = vars.clone();
                scope.extend(locals.clone());
                scope.extend(captured.clone());
                let value = expression::evaluate(expression, &scope)
                    .map_err(|e| error(&format!("{}: {e}", rule.name)))?;
                if *local && declared.contains(name) {
                    continue;
                }
                if *local || declared.contains(name) {
                    if *local {
                        declared.insert(name.clone());
                    }
                    locals.insert(name.clone(), value.clone());
                } else {
                    if matches!(
                        name.as_str(),
                        "CHARACTER_ID"
                            | "CURRENT_INPUT"
                            | "ACCEPTED_REPLIES"
                            | "attitude"
                            | "boredom"
                            | "stress"
                    ) {
                        return Err(error(&format!("protected character variable: {name}")));
                    }
                    vars.insert(name.clone(), value.clone());
                    updates.insert(name.clone(), value.clone());
                }
                captured.insert(name.clone(), value);
            }
            Action::Log(expression) => {
                let mut scope = vars.clone();
                scope.extend(locals.clone());
                scope.extend(captured.clone());
                let value = expression::evaluate(expression, &scope)
                    .map_err(|e| error(&format!("{}: {e}", rule.name)))?;
                if logs.len() < 256 {
                    let message = expression::text(&value)
                        .chars()
                        .take(512)
                        .collect::<String>();
                    logs.push(format!("{}: {message}", rule.name));
                }
            }
            Action::Remove => remove = true,
            Action::Replace(expression) => replacement_expr = Some(expression),
        }
    }
    if remove {
        replacement.clear();
    } else if let Some(expression) = replacement_expr {
        let mut scope = vars.clone();
        scope.extend(locals.clone());
        scope.extend(captured.clone());
        replacement = expression::text(&expression::evaluate(expression, &scope)?);
    }
    Ok(replacement)
}

fn parse_action(line: &str) -> Result<Action, PromptError> {
    let parse_set = |rest: &str, local: bool| -> Result<Action, PromptError> {
        let (name, expression) = rest
            .split_once('=')
            .ok_or_else(|| error("SET missing '='"))?;
        let name = name.trim();
        if !valid_name(name) || expression.trim().is_empty() {
            return Err(error("invalid SET variable or expression"));
        }
        Ok(Action::Set {
            name: name.into(),
            expression: expression.trim().into(),
            local,
        })
    };
    if let Some(rest) = strip_prefix_ci(line, "SET LOCAL ") {
        return parse_set(rest, true);
    }
    if let Some(rest) = strip_prefix_ci(line, "SET ") {
        return parse_set(rest, false);
    }
    if let Some(rest) = strip_prefix_ci(line, "LOG ") {
        return Ok(Action::Log(rest.to_owned()));
    }
    if line.eq_ignore_ascii_case("REMOVE_MATCH") {
        return Ok(Action::Remove);
    }
    if let Some(rest) = strip_prefix_ci(line, "REPLACE_MATCH WITH ") {
        return Ok(Action::Replace(rest.to_owned()));
    }
    Err(error(&format!("unsupported post-script action: {line}")))
}

fn split_capture(value: &str) -> Option<(&str, &str)> {
    let start = value.to_ascii_uppercase().rfind(" CAPTURE (")?;
    let (front, tail) = value.split_at(start);
    let names = tail.get(10..)?.strip_suffix(')')?;
    Some((front, names))
}
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && name.len() <= 80
}
fn error(msg: &str) -> PromptError {
    PromptError::Dsl(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_regex_field_and_locals() {
        let script = r#"
            RULE LoveTag
              MATCH REGEX "love_change: ([+-]?\d+)" CAPTURE (amount)
              ACTIONS
                SET LOCAL delta = int(amount)
                SET Love = Love + delta
                REMOVE_MATCH
              END_ACTIONS
            END_RULE
            RULE Greeting
              MATCH TEXT "Hello."
              ACTIONS
                REPLACE_MATCH WITH "Greetings!"
              END_ACTIONS
            END_RULE
            RULE Field
              MATCH FIELD "secret_trial"
              ACTIONS
                SET secretTrial = secret_trial
              END_ACTIONS
            END_RULE
            DEBUG_DISPLAY
                "Love": Love
            END_DEBUG_DISPLAY
        "#;
        let program =
            PostDslProgram::parse(script, BTreeMap::from([("Love".into(), Value::from(12))]))
                .unwrap();
        let mut response = StructuredResponse::spoken("Hello. love_change: +3");
        response.custom_fields = Some(serde_json::Map::from_iter([(
            "secret_trial".into(),
            Value::Bool(true),
        )]));
        let result = program.apply(response, &BTreeMap::new()).unwrap();
        assert_eq!(result.response.full_text().as_ref(), "Greetings!");
        assert_eq!(result.variables["Love"], 15.0);
        assert_eq!(result.variables["secretTrial"], true);
        assert_eq!(program.debug_display()["Love"], "Love");
    }

    #[test]
    fn replacement_runs_after_set_and_field_rules_follow_rule_order() {
        let script = r#"
            RULE first
                MATCH FIELD "beta"
                ACTIONS
                    SET answer = beta * 2
                END_ACTIONS
            END_RULE
            RULE second
                MATCH FIELD "alpha"
                ACTIONS
                    SET answer = answer + alpha
                END_ACTIONS
            END_RULE
            RULE text
                MATCH TEXT "<code>"
                ACTIONS
                    REPLACE_MATCH WITH str(answer)
                    SET answer = answer + 1
                END_ACTIONS
            END_RULE
        "#;
        let program =
            PostDslProgram::parse(script, BTreeMap::from([("answer".into(), Value::from(0))]))
                .unwrap();
        let mut response = StructuredResponse::spoken("<code>");
        response.custom_fields = Some(serde_json::Map::from_iter([
            ("alpha".into(), Value::from(5)),
            ("beta".into(), Value::from(2)),
        ]));
        let result = program.apply(response, &BTreeMap::new()).unwrap();
        assert_eq!(result.response.full_text().as_ref(), "1");
        assert_eq!(result.variables["answer"], 9.0);
    }

    #[test]
    fn python_crazy_default_rules_are_supported() {
        let script = r#"
            RULE LoveHandler
                MATCH FIELD "love_change"
                ACTIONS
                    SET Love = max(1, min(Love + love_change, 300))
                    LOG "Updated " + str(Love)
                END_ACTIONS
            END_RULE
            RULE RemoveAsterisks
                MATCH REGEX "(\*{1,3})"
                ACTIONS
                    REMOVE_MATCH
                END_ACTIONS
            END_RULE
            DEBUG_DISPLAY
                "Love": Love
            END_DEBUG_DISPLAY
        "#;
        let program =
            PostDslProgram::parse(script, BTreeMap::from([("Love".into(), Value::from(10))]))
                .unwrap();
        let mut response = StructuredResponse::spoken("**Hello**");
        response.custom_fields = Some(serde_json::Map::from_iter([(
            "love_change".into(),
            Value::from(2),
        )]));
        let processed = program.apply(response, &BTreeMap::new()).unwrap();
        assert_eq!(processed.response.full_text().as_ref(), "Hello");
        assert_eq!(processed.variables["Love"], 12.0);
    }

    #[test]
    fn custom_params_apply_before_field_rules_with_python_config_bounds() {
        let rules = r#"
            RULE love
                MATCH FIELD "love_change"
                ACTIONS
                    SET Love = Love + love_change
                END_ACTIONS
            END_RULE
        "#;
        let config = serde_json::json!([
            {
                "name":"Love", "change_command":"love_change", "type":"float",
                "change_min":-5, "change_max":5, "initial":1,
                "formula":"max(1, min(Love + love_change, 300))"
            },
            {"name":"secret_trial", "op":"set", "type":"bool"}
        ]);
        let program = PostDslProgram::parse(rules, BTreeMap::new())
            .unwrap()
            .with_custom_params(config.as_array().unwrap())
            .unwrap();
        let mut response = StructuredResponse::spoken("hi");
        response.custom_fields = Some(serde_json::Map::from_iter([
            ("love_change".into(), Value::from(100)),
            ("secret_trial".into(), Value::from(true)),
        ]));
        let result = program.apply(response, &BTreeMap::new()).unwrap();
        assert_eq!(result.variables["Love"], Value::from(106.0));
        assert_eq!(result.variables["secret_trial"], Value::Bool(true));
    }

    #[test]
    fn bounded_log_and_debug_values() {
        let program = PostDslProgram::parse(
            "RULE info\n MATCH TEXT \"hello\"\n ACTIONS\n LOG str(Love)\n END_ACTIONS\nEND_RULE\nDEBUG_DISPLAY\n \"Affection\": Love\nEND_DEBUG_DISPLAY\n",
            BTreeMap::from([("Love".into(), Value::from(4))]),
        ).unwrap();
        assert_eq!(
            program.debug_values(&BTreeMap::from([("Love".into(), Value::from(7))]))["Affection"],
            Value::from(7)
        );
        let result = program
            .apply(StructuredResponse::spoken("hello"), &BTreeMap::new())
            .unwrap();
        assert_eq!(result.logs, ["info: 4"]);
    }

    #[test]
    fn rejects_regex_capture_mismatch_and_unbounded_match_count() {
        assert!(PostDslProgram::parse(
            "RULE bad\n MATCH REGEX \"(foo)\" CAPTURE (first, second)\n ACTIONS\n REMOVE_MATCH\n END_ACTIONS\nEND_RULE\n", BTreeMap::new(),
        ).is_err());
        let program = PostDslProgram::parse(
            "RULE many\n MATCH TEXT \"a\"\n ACTIONS\n REMOVE_MATCH\n END_ACTIONS\nEND_RULE\n",
            BTreeMap::new(),
        )
        .unwrap();
        assert!(
            program
                .apply(
                    StructuredResponse::spoken("a".repeat(10_001)),
                    &BTreeMap::new()
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_unsafe_or_unhandled_rules() {
        assert!(
            PostDslProgram::parse(
                "RULE x\n MATCH REGEX \"[\"\n ACTIONS\n END_ACTIONS\n END_RULE",
                BTreeMap::new()
            )
            .is_err()
        );
        let rules = PostDslProgram::parse("RULE x\n MATCH TEXT \"x\"\n ACTIONS\n SET Y = __import__('os')\n END_ACTIONS\n END_RULE", BTreeMap::new()).unwrap();
        assert!(
            rules
                .apply(StructuredResponse::spoken("x"), &BTreeMap::new())
                .is_err()
        );
    }
}
