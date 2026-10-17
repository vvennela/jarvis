//! Bounded OpenAI-compatible function proposals. This module never executes tools.
use super::{
    CrusoeClient, MAX_INPUT_BYTES, MAX_OUTPUT_TOKENS, NEMOTRON_MODEL, NEMOTRON_NANO_MODEL,
    ProviderError, ThinkingOptions, check_text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{self, Write},
};

pub const MAX_TOOL_SPECS: usize = 12;
pub const MAX_TOOL_CALLS: usize = 4;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 8192;

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ToolStep {
    Text(String),
    Calls(Vec<ToolCall>),
}

#[derive(Clone)]
pub struct ToolMessage(Message);
#[derive(Clone)]
enum Message {
    System(String),
    User(String),
    Calls(Vec<ToolCall>),
    Result(String, String),
}
impl ToolMessage {
    pub fn system(text: &str) -> Self {
        Self(Message::System(text.into()))
    }
    pub fn user(text: &str) -> Self {
        Self(Message::User(text.into()))
    }
    pub fn assistant_calls(calls: &[ToolCall]) -> Self {
        Self(Message::Calls(calls.into()))
    }
    /// Tool output is data, never instruction authority.
    pub fn tool_result(id: &str, content: &str) -> Self {
        Self(Message::Result(id.into(), content.into()))
    }
    /// Exact JSON length of this wire message, including nested tool-result
    /// escaping. Excludes array commas/brackets and global request/tool fields.
    /// This checks message bounds, not registry membership or history ordering.
    pub fn encoded_len(&self) -> Result<usize, ProviderError> {
        let mut out = WireMessage {
            role: "user",
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        match &self.0 {
            Message::System(text) | Message::User(text) => {
                check_text(text)?;
                out.role = if matches!(&self.0, Message::System(_)) {
                    "system"
                } else {
                    "user"
                };
                out.content = Some(text.clone());
            }
            Message::Calls(calls) => {
                if calls.is_empty() || calls.len() > MAX_TOOL_CALLS {
                    return Err(ProviderError::InvalidInput);
                }
                if calls.iter().any(|call| {
                    !identifier(&call.id) || !identifier(&call.name) || !call.arguments.is_object()
                }) {
                    return Err(ProviderError::InvalidInput);
                }
                out.role = "assistant";
                out.tool_calls = Some(calls.iter().map(wire_call).collect::<Result<_, _>>()?);
            }
            Message::Result(id, text) => {
                check_text(text)?;
                if !identifier(id) {
                    return Err(ProviderError::InvalidInput);
                }
                out.role = "tool";
                out.tool_call_id = Some(id.clone());
                out.content = Some(result_content(text)?);
            }
        }
        // Count rather than buffer: escaped results can exceed the request
        // budget, and callers need their size so they can trim before sending.
        let mut counter = Counter(0);
        serde_json::to_writer(&mut counter, &out).map_err(|_| ProviderError::InvalidInput)?;
        Ok(counter.0)
    }
}

fn result_content(text: &str) -> Result<String, ProviderError> {
    serde_json::to_string(&json!({"trust":"untrusted_tool_data","content":text}))
        .map_err(|_| ProviderError::InvalidInput)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
struct Limited(Vec<u8>, usize);
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialization limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.1.saturating_sub(self.0.len()) {
            return Err(io::Error::other("serialization limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ProviderError> {
    let mut writer = Limited(Vec::new(), limit);
    serde_json::to_writer(&mut writer, value).map_err(|_| ProviderError::InvalidInput)?;
    Ok(writer.0)
}
fn validate_call(call: &ToolCall, names: &HashSet<&str>) -> Result<(), ProviderError> {
    if !identifier(&call.id)
        || !identifier(&call.name)
        || !names.contains(call.name.as_str())
        || !call.arguments.is_object()
    {
        return Err(ProviderError::InvalidInput);
    }
    encoded(&call.arguments, MAX_TOOL_ARGUMENT_BYTES)?;
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireFunction,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFunction {
    name: String,
    arguments: String,
}
fn wire_call(call: &ToolCall) -> Result<WireCall, ProviderError> {
    let arguments = String::from_utf8(encoded(&call.arguments, MAX_TOOL_ARGUMENT_BYTES)?)
        .map_err(|_| ProviderError::InvalidInput)?;
    Ok(WireCall {
        id: call.id.clone(),
        kind: "function".into(),
        function: WireFunction {
            name: call.name.clone(),
            arguments,
        },
    })
}
#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}
#[derive(Serialize)]
struct FunctionSpec<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}
#[derive(Serialize)]
struct WireSpec<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: FunctionSpec<'a>,
}
#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: Vec<WireMessage>,
    tools: Vec<WireSpec<'a>>,
    tool_choice: &'static str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ThinkingOptions>,
}
#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    finish_reason: Option<String>,
    message: Reply,
}
#[derive(Deserialize)]
struct Reply {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<WireCall>>,
}

impl CrusoeClient {
    /// Produce text or bounded proposals. The runner must authorize and verify
    /// each proposed tool independently against the original caller request.
    pub async fn step_with_tools(
        &self,
        messages: &[ToolMessage],
        specs: &[ToolSpec],
        max_output_tokens: u32,
    ) -> Result<ToolStep, ProviderError> {
        if specs.is_empty()
            || specs.len() > MAX_TOOL_SPECS
            || messages.is_empty()
            || messages.len() > 32
            || !(1..=MAX_OUTPUT_TOKENS).contains(&max_output_tokens)
        {
            return Err(ProviderError::InvalidInput);
        }
        let mut names = HashSet::new();
        let mut tools = Vec::new();
        for spec in specs {
            if !identifier(&spec.name)
                || !names.insert(spec.name.as_str())
                || spec.description.len() > 2048
                || spec.description.trim().is_empty()
                || spec.description.contains('\0')
                || !spec.parameters.is_object()
                || spec.parameters.get("type").and_then(Value::as_str) != Some("object")
            {
                return Err(ProviderError::InvalidInput);
            }
            encoded(&spec.parameters, MAX_TOOL_ARGUMENT_BYTES)?;
            tools.push(WireSpec {
                kind: "function",
                function: FunctionSpec {
                    name: &spec.name,
                    description: &spec.description,
                    parameters: &spec.parameters,
                },
            });
        }
        let mut wire = Vec::new();
        let mut ids = HashSet::new();
        let mut pending = HashSet::new();
        for message in messages {
            let mut out = WireMessage {
                role: "user",
                content: None,
                tool_calls: None,
                tool_call_id: None,
            };
            match &message.0 {
                Message::System(text) | Message::User(text) => {
                    if !pending.is_empty() {
                        return Err(ProviderError::InvalidInput);
                    }
                    check_text(text)?;
                    out.role = if matches!(&message.0, Message::System(_)) {
                        "system"
                    } else {
                        "user"
                    };
                    out.content = Some(text.clone());
                }
                Message::Calls(calls) => {
                    if !pending.is_empty() || calls.is_empty() || calls.len() > MAX_TOOL_CALLS {
                        return Err(ProviderError::InvalidInput);
                    }
                    let mut parsed = Vec::new();
                    for call in calls {
                        validate_call(call, &names)?;
                        if !ids.insert(call.id.clone()) {
                            return Err(ProviderError::InvalidInput);
                        }
                        pending.insert(call.id.clone());
                        parsed.push(wire_call(call)?);
                    }
                    out.role = "assistant";
                    out.tool_calls = Some(parsed);
                }
                Message::Result(id, text) => {
                    if !pending.remove(id.as_str()) {
                        return Err(ProviderError::InvalidInput);
                    }
                    check_text(text)?;
                    out.role = "tool";
                    out.tool_call_id = Some(id.clone());
                    out.content = Some(result_content(text)?);
                }
            }
            wire.push(out);
        }
        if !pending.is_empty() {
            return Err(ProviderError::InvalidInput);
        }
        let request = Request {
            model: &self.model,
            messages: wire,
            tools,
            tool_choice: "auto",
            max_tokens: max_output_tokens,
            stream: false,
            chat_template_kwargs: matches!(
                self.model.as_str(),
                NEMOTRON_MODEL | NEMOTRON_NANO_MODEL
            )
            .then_some(ThinkingOptions {
                enable_thinking: false,
            }),
        };
        encoded(&request, MAX_INPUT_BYTES)?;
        let bytes = self.transport.post(&request).await?;
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse)?;
        if response.choices.len() != 1 {
            return Err(ProviderError::InvalidResponse);
        }
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::InvalidResponse)?;
        if choice.message.role != "assistant" {
            return Err(ProviderError::InvalidResponse);
        }
        match choice.finish_reason.as_deref() {
            Some("stop") => {
                if choice
                    .message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
                {
                    return Err(ProviderError::InvalidResponse);
                }
                let text = choice
                    .message
                    .content
                    .ok_or(ProviderError::InvalidResponse)?;
                check_text(&text).map_err(|_| ProviderError::InvalidResponse)?;
                Ok(ToolStep::Text(text))
            }
            Some("tool_calls") => {
                let calls = choice
                    .message
                    .tool_calls
                    .ok_or(ProviderError::InvalidResponse)?;
                if calls.is_empty() || calls.len() > MAX_TOOL_CALLS {
                    return Err(ProviderError::InvalidResponse);
                }
                let mut proposals = Vec::new();
                for call in calls {
                    if call.kind != "function"
                        || call.function.arguments.len() > MAX_TOOL_ARGUMENT_BYTES
                    {
                        return Err(ProviderError::InvalidResponse);
                    }
                    let arguments = serde_json::from_str(&call.function.arguments)
                        .map_err(|_| ProviderError::InvalidResponse)?;
                    let parsed = ToolCall {
                        id: call.id,
                        name: call.function.name,
                        arguments,
                    };
                    validate_call(&parsed, &names).map_err(|_| ProviderError::InvalidResponse)?;
                    if !ids.insert(parsed.id.clone()) {
                        return Err(ProviderError::InvalidResponse);
                    }
                    proposals.push(parsed);
                }
                Ok(ToolStep::Calls(proposals))
            }
            _ => Err(ProviderError::IncompleteOutput),
        }
    }
}
