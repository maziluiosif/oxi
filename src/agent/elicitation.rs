//! ACP `elicitation/create`: an agent asking the user for something mid-turn. Claude Code uses it
//! for `AskUserQuestion`, for forms and OAuth logins of MCP servers, and to offer another model
//! after a refusal.
//!
//! Requests travel between agent, server and UI as their raw params and are only parsed here, so
//! a remote chat needs nothing beyond passing them on. Form schemas hold a subset of JSON Schema:
//! flat objects of strings, numbers, booleans and string lists.

use std::cmp::Ordering;

use serde_json::{Map, Value, json};

use super::approval::ApprovalDecision;

/// What the agent asks, with the answers typed so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Elicitation {
    pub message: String,
    pub kind: ElicitationKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElicitationKind {
    Form(Vec<Field>),
    /// A page for the user to visit, usually an OAuth login.
    Url(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub key: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub input: FieldInput,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldInput {
    Text {
        value: String,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
    Number {
        /// As typed; read when the form is submitted.
        value: String,
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Boolean(bool),
    Choice {
        options: Vec<Choice>,
        selected: Option<usize>,
    },
    Multi {
        options: Vec<Choice>,
        selected: Vec<bool>,
        min_items: Option<usize>,
        max_items: Option<usize>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub description: String,
    /// Mockup or snippet for this option (Claude Code's `AskUserQuestion` previews).
    pub preview: String,
}

/// Whether a request held with the permission requests is an elicitation: only those have a mode.
pub fn is_request(params: &Value) -> bool {
    params.get("mode").is_some()
}

/// The id a URL elicitation is later completed under.
pub fn id_of(params: &Value) -> Option<&str> {
    params["elicitationId"].as_str()
}

/// Only web pages are opened: the URL comes from an agent or an MCP server.
pub fn is_openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

pub fn parse(params: &Value) -> Elicitation {
    let message = text(params, "message");
    if params["mode"].as_str() == Some("url") {
        return Elicitation {
            message,
            kind: ElicitationKind::Url(text(params, "url")),
        };
    }
    let schema = &params["requestedSchema"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|keys| keys.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut fields: Vec<Field> = schema["properties"]
        .as_object()
        .map(|properties| {
            properties
                .iter()
                .map(|(key, schema)| Field {
                    key: key.clone(),
                    title: text(schema, "title"),
                    description: text(schema, "description"),
                    required: required.contains(&key.as_str()),
                    input: parse_input(schema),
                })
                .collect()
        })
        .unwrap_or_default();
    // JSON objects arrive sorted here; this keeps `question_10` after `question_2`.
    fields.sort_by(|a, b| natural_cmp(&a.key, &b.key));
    Elicitation {
        message,
        kind: ElicitationKind::Form(fields),
    }
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

fn count(schema: &Value, key: &str) -> Option<usize> {
    schema[key].as_u64().map(|n| n as usize)
}

fn parse_input(schema: &Value) -> FieldInput {
    match schema["type"].as_str() {
        Some("boolean") => FieldInput::Boolean(schema["default"].as_bool().unwrap_or(false)),
        Some(kind @ ("number" | "integer")) => FieldInput::Number {
            value: match &schema["default"] {
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            },
            integer: kind == "integer",
            minimum: schema["minimum"].as_f64(),
            maximum: schema["maximum"].as_f64(),
        },
        Some("array") => {
            let options = parse_choices(&schema["items"]);
            let defaults: Vec<&str> = schema["default"]
                .as_array()
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            FieldInput::Multi {
                selected: options
                    .iter()
                    .map(|o| defaults.contains(&o.value.as_str()))
                    .collect(),
                options,
                min_items: count(schema, "minItems"),
                max_items: count(schema, "maxItems"),
            }
        }
        _ => {
            let options = parse_choices(schema);
            if options.is_empty() {
                FieldInput::Text {
                    value: text(schema, "default"),
                    min_length: count(schema, "minLength"),
                    max_length: count(schema, "maxLength"),
                }
            } else {
                let default = schema["default"].as_str();
                FieldInput::Choice {
                    selected: options.iter().position(|o| Some(&*o.value) == default),
                    options,
                }
            }
        }
    }
}

/// The values of a titled `oneOf` / `anyOf`, or of a plain `enum` (with its `enumNames`).
fn parse_choices(schema: &Value) -> Vec<Choice> {
    if let Some(items) = schema["oneOf"]
        .as_array()
        .or_else(|| schema["anyOf"].as_array())
    {
        return items
            .iter()
            .filter_map(|item| {
                let value = item["const"].as_str()?.to_owned();
                let title = text(item, "title");
                Some(Choice {
                    label: if title.is_empty() {
                        value.clone()
                    } else {
                        title
                    },
                    value,
                    description: text(item, "description"),
                    preview: text(&item["_meta"]["_claude/askUserQuestionOption"], "preview"),
                })
            })
            .collect();
    }
    let names = schema["enumNames"].as_array();
    schema["enum"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .enumerate()
                .filter_map(|(i, value)| {
                    let value = value.as_str()?.to_owned();
                    let label = names
                        .and_then(|n| n.get(i))
                        .and_then(Value::as_str)
                        .map_or_else(|| value.clone(), str::to_owned);
                    Some(Choice {
                        value,
                        label,
                        description: String::new(),
                        preview: String::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Order strings by their text, with runs of digits compared as numbers.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn digits(s: &str) -> &str {
        &s[..s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len())]
    }
    let (mut a, mut b) = (a, b);
    loop {
        let (x, y) = (digits(a), digits(b));
        if !x.is_empty() && !y.is_empty() {
            let (short_x, short_y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
            let order = short_x
                .len()
                .cmp(&short_y.len())
                .then_with(|| short_x.cmp(short_y));
            if order != Ordering::Equal {
                return order;
            }
            (a, b) = (&a[x.len()..], &b[y.len()..]);
            continue;
        }
        let (mut xs, mut ys) = (a.chars(), b.chars());
        match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x != y => return x.cmp(&y),
            _ => (a, b) = (xs.as_str(), ys.as_str()),
        }
    }
}

impl Field {
    /// The answer to send, `None` when left empty, or what is wrong with it.
    fn answer(&self) -> Result<Option<Value>, String> {
        let name = if self.title.is_empty() {
            "This field"
        } else {
            &self.title
        };
        let answer = match &self.input {
            FieldInput::Boolean(on) => Some(json!(on)),
            FieldInput::Text {
                value,
                min_length,
                max_length,
            } => {
                let length = value.chars().count();
                if value.trim().is_empty() {
                    None
                } else if min_length.is_some_and(|min| length < min) {
                    return Err(format!(
                        "{name} needs at least {} characters",
                        min_length.unwrap_or(0)
                    ));
                } else if max_length.is_some_and(|max| length > max) {
                    return Err(format!(
                        "{name} takes at most {} characters",
                        max_length.unwrap_or(0)
                    ));
                } else {
                    Some(json!(value))
                }
            }
            FieldInput::Number {
                value,
                integer,
                minimum,
                maximum,
            } => {
                let typed = value.trim();
                if typed.is_empty() {
                    None
                } else {
                    let number = typed
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| format!("{name} must be a number"))?;
                    if *integer && number.fract() != 0.0 {
                        return Err(format!("{name} must be a whole number"));
                    }
                    if minimum.is_some_and(|min| number < min)
                        || maximum.is_some_and(|max| number > max)
                    {
                        return Err(format!("{name} is out of range"));
                    }
                    Some(if *integer {
                        json!(number as i64)
                    } else {
                        json!(number)
                    })
                }
            }
            FieldInput::Choice { options, selected } => selected
                .and_then(|i| options.get(i))
                .map(|o| json!(o.value)),
            FieldInput::Multi {
                options,
                selected,
                min_items,
                max_items,
            } => {
                let picks: Vec<&str> = options
                    .iter()
                    .zip(selected)
                    .filter(|(_, on)| **on)
                    .map(|(o, _)| o.value.as_str())
                    .collect();
                if picks.is_empty() {
                    None
                } else if min_items.is_some_and(|min| picks.len() < min) {
                    return Err(format!(
                        "{name} needs at least {} picks",
                        min_items.unwrap_or(0)
                    ));
                } else if max_items.is_some_and(|max| picks.len() > max) {
                    return Err(format!(
                        "{name} takes at most {} picks",
                        max_items.unwrap_or(0)
                    ));
                } else {
                    Some(json!(picks))
                }
            }
        };
        if answer.is_none() && self.required {
            return Err(format!("{name} is required"));
        }
        Ok(answer)
    }
}

impl Elicitation {
    /// The `content` of an accepted form, or why it can't be sent yet.
    pub fn content(&self) -> Result<Value, String> {
        let mut content = Map::new();
        if let ElicitationKind::Form(fields) = &self.kind {
            for field in fields {
                if let Some(answer) = field.answer()? {
                    content.insert(field.key.clone(), answer);
                }
            }
        }
        Ok(Value::Object(content))
    }
}

pub fn accepted(content: Value) -> ApprovalDecision {
    ApprovalDecision::Elicitation(json!({"action": "accept", "content": content}))
}

pub fn declined() -> ApprovalDecision {
    ApprovalDecision::Elicitation(json!({"action": "decline"}))
}

/// The `elicitation/create` response for a decision, `None` being a turn that ended first. A
/// client that shows the request as a plain approval (the phone, an editor plugin) answers with
/// Allow and Deny; those mean accepting nothing and declining.
pub fn response(decision: Option<ApprovalDecision>) -> Value {
    match decision {
        None => json!({"action": "cancel"}),
        Some(ApprovalDecision::Elicitation(response)) => response,
        Some(ApprovalDecision::Deny) => json!({"action": "decline"}),
        Some(_) => json!({"action": "accept", "content": {}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As `askUserQuestionsToCreateRequest` sends two questions.
    fn questions() -> Value {
        let question = |n: u32, multi: bool| {
            let options = json!([
                {"const": "Rust", "title": "Rust", "description": "Fast",
                 "_meta": {"_claude/askUserQuestionOption": {"preview": "fn main() {}"}}},
                {"const": "Go", "title": "Go"},
            ]);
            let field = if multi {
                json!({"type": "array", "title": "Pick", "items": {"anyOf": options}})
            } else {
                json!({"type": "string", "title": "Pick", "oneOf": options})
            };
            (format!("question_{n}"), field)
        };
        let mut properties = Map::new();
        for (key, field) in [question(0, false), question(2, true), question(10, false)] {
            properties.insert(
                format!("{key}_custom"),
                json!({"type": "string", "title": "Other"}),
            );
            properties.insert(key, field);
        }
        json!({
            "mode": "form", "sessionId": "s", "toolCallId": "t",
            "message": "Please answer the following questions.",
            "requestedSchema": {"type": "object", "properties": properties},
        })
    }

    fn form(request: &Value) -> Vec<Field> {
        match parse(request).kind {
            ElicitationKind::Form(fields) => fields,
            ElicitationKind::Url(_) => panic!("expected a form"),
        }
    }

    #[test]
    fn questions_keep_their_order_with_the_custom_box_after_each() {
        let keys: Vec<String> = form(&questions()).into_iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            [
                "question_0",
                "question_0_custom",
                "question_2",
                "question_2_custom",
                "question_10",
                "question_10_custom",
            ]
        );
    }

    #[test]
    fn answers_are_read_back_by_field_key() {
        let mut request = parse(&questions());
        let ElicitationKind::Form(fields) = &mut request.kind else {
            panic!("expected a form");
        };
        let FieldInput::Choice { options, selected } = &mut fields[0].input else {
            panic!("expected a choice");
        };
        assert_eq!(options[0].preview, "fn main() {}");
        assert_eq!(options[0].description, "Fast");
        *selected = Some(1);
        let FieldInput::Multi { selected, .. } = &mut fields[2].input else {
            panic!("expected a multi-select");
        };
        selected[0] = true;
        selected[1] = true;
        let FieldInput::Text { value, .. } = &mut fields[5].input else {
            panic!("expected a text box");
        };
        *value = "Zig".into();
        assert_eq!(
            request.content().unwrap(),
            json!({
                "question_0": "Go",
                "question_2": ["Rust", "Go"],
                "question_10_custom": "Zig",
            })
        );
    }

    #[test]
    fn nothing_answered_is_an_empty_accept_when_nothing_is_required() {
        assert_eq!(parse(&questions()).content().unwrap(), json!({}));
    }

    #[test]
    fn mcp_form_types_defaults_and_limits() {
        let request = json!({
            "mode": "form", "sessionId": "s", "message": "Configure",
            "requestedSchema": {
                "type": "object",
                "required": ["name", "port"],
                "properties": {
                    "name": {"type": "string", "title": "Name", "minLength": 2, "default": "x"},
                    "port": {"type": "integer", "title": "Port", "minimum": 1, "maximum": 100},
                    "tls": {"type": "boolean", "default": true},
                    "zone": {"type": "string", "enum": ["a", "b"], "enumNames": ["A", "B"],
                             "default": "b"},
                },
            },
        });
        let mut parsed = parse(&request);
        assert_eq!(
            parsed.content().unwrap_err(),
            "Name needs at least 2 characters"
        );
        let ElicitationKind::Form(fields) = &mut parsed.kind else {
            panic!("expected a form");
        };
        let set = |field: &mut Field, typed: &str| match &mut field.input {
            FieldInput::Text { value, .. } | FieldInput::Number { value, .. } => {
                *value = typed.into()
            }
            _ => panic!("expected a typed field"),
        };
        set(&mut fields[0], "web");
        let mut check = |typed: &str| {
            let ElicitationKind::Form(fields) = &mut parsed.kind else {
                panic!("expected a form");
            };
            set(&mut fields[1], typed);
            parsed.content()
        };
        assert_eq!(check("").unwrap_err(), "Port is required");
        assert_eq!(check("x").unwrap_err(), "Port must be a number");
        assert_eq!(check("1.5").unwrap_err(), "Port must be a whole number");
        assert_eq!(check("101").unwrap_err(), "Port is out of range");
        assert_eq!(
            check("80").unwrap(),
            json!({"name": "web", "port": 80, "tls": true, "zone": "b"})
        );
    }

    #[test]
    fn url_requests_and_what_may_be_opened() {
        let request = json!({
            "mode": "url", "sessionId": "s", "message": "Authenticate with MCP server linear",
            "url": "https://mcp.linear.app/authorize", "elicitationId": "mcp-oauth-1",
        });
        assert!(is_request(&request));
        assert_eq!(id_of(&request), Some("mcp-oauth-1"));
        assert_eq!(
            parse(&request).kind,
            ElicitationKind::Url("https://mcp.linear.app/authorize".into())
        );
        assert!(is_openable(" HTTPS://example.com "));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_request(&json!({"toolCall": {}, "options": []})));
    }

    #[test]
    fn responses_for_every_way_a_request_can_end() {
        assert_eq!(response(None), json!({"action": "cancel"}));
        assert_eq!(
            response(Some(ApprovalDecision::Deny)),
            json!({"action": "decline"})
        );
        assert_eq!(
            response(Some(ApprovalDecision::Approve)),
            json!({"action": "accept", "content": {}})
        );
        assert_eq!(
            response(Some(accepted(json!({"a": 1})))),
            json!({"action": "accept", "content": {"a": 1}})
        );
        assert_eq!(response(Some(declined())), json!({"action": "decline"}));
    }
}
