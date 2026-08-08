use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use menvane_domain::{Applicability, MemoryType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "2025-11-25";

pub struct McpServer<'a> {
    menvane: &'a Menvane,
    cwd: PathBuf,
}

impl<'a> McpServer<'a> {
    pub fn new(menvane: &'a Menvane, cwd: impl Into<PathBuf>) -> Self {
        Self {
            menvane,
            cwd: cwd.into(),
        }
    }

    pub fn serve(&self, reader: impl BufRead, mut writer: impl Write) -> Result<()> {
        let mut initialized = false;
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let request: Value = match serde_json::from_str(&line) {
                Ok(request) => request,
                Err(error) => {
                    write_message(
                        &mut writer,
                        rpc_error(Value::Null, -32700, &error.to_string()),
                    )?;
                    continue;
                }
            };
            let id = request.get("id").cloned();
            let method = request.get("method").and_then(Value::as_str);
            if method == Some("notifications/initialized") && id.is_none() {
                initialized = true;
                continue;
            }
            let Some(id) = id else {
                continue;
            };
            let response = match method {
                Some("initialize") => {
                    initialized = true;
                    rpc_result(
                        id,
                        json!({
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": { "tools": {} },
                            "serverInfo": {
                                "name": "menvane",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }),
                    )
                }
                _ if !initialized => rpc_error(id, -32600, "server is not initialized"),
                Some("tools/list") => rpc_result(id, json!({ "tools": tool_definitions() })),
                Some("tools/call") => self.call_tool(id, request.get("params")),
                Some(_) => rpc_error(id, -32601, "method not found"),
                None => rpc_error(id, -32600, "method is required"),
            };
            write_message(&mut writer, response)?;
        }
        Ok(())
    }

    fn call_tool(&self, id: Value, params: Option<&Value>) -> Value {
        let Some(name) = params
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
        else {
            return rpc_error(id, -32602, "tool name is required");
        };
        let arguments = params
            .and_then(|params| params.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = match name {
            "memory_search" => self.memory_search(arguments),
            "memory_read" => self.memory_read(arguments),
            "memory_write" => self.memory_write(arguments),
            "memory_forget" => self.memory_forget(arguments),
            _ => return rpc_error(id, -32602, "unknown tool"),
        };
        match result {
            Ok(value) => rpc_result(
                id,
                json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                    "isError": false
                }),
            ),
            Err(error) => rpc_result(
                id,
                json!({
                    "content": [{ "type": "text", "text": error.to_string() }],
                    "isError": true
                }),
            ),
        }
    }

    fn memory_search(&self, arguments: Value) -> Result<Value> {
        let arguments: SearchArguments = serde_json::from_value(arguments)?;
        let scope = match arguments.scope.as_str() {
            "auto" => ScopeSelection::Auto,
            "project" => ScopeSelection::Project,
            "global" => ScopeSelection::Global,
            value => anyhow::bail!("unsupported scope: {value}"),
        };
        let results = self.menvane.search_with_sessions(
            &self.cwd,
            &arguments.query,
            scope,
            arguments.limit,
            arguments.include_sessions,
        )?;
        Ok(Value::Array(
            results
                .into_iter()
                .map(|memory| {
                    json!({
                        "id": memory.id,
                        "type": memory.memory_type,
                        "scope": memory.scope,
                        "title": memory.title,
                        "score": memory.score,
                        "status": memory.status,
                        "confidence": memory.confidence,
                        "applicability": memory.applicability,
                        "short_excerpt": memory.excerpt
                    })
                })
                .collect(),
        ))
    }

    fn memory_read(&self, arguments: Value) -> Result<Value> {
        let arguments: ReadArguments = serde_json::from_value(arguments)?;
        let memory = self.menvane.read(arguments.id)?;
        let markdown = format!("# {}\n\n{}", memory.title, memory.body);
        let metadata = &memory.metadata;
        Ok(json!({
            "metadata": metadata,
            "full_markdown_body": markdown,
            "source_sessions": metadata.source_sessions,
            "supersession_chain": metadata.supersedes
        }))
    }

    fn memory_write(&self, arguments: Value) -> Result<Value> {
        let arguments: WriteArguments = serde_json::from_value(arguments)?;
        let (title, body) = split_content(&arguments.content);
        let memory_type = match arguments.memory_type.as_str() {
            "auto" => infer_memory_type(&arguments.content),
            value => value.parse()?,
        };
        let scope = match arguments.scope.as_str() {
            "auto" | "project" => Scope::Project,
            "global" => Scope::Global,
            value => anyhow::bail!("unsupported scope: {value}"),
        };
        let memory = self.menvane.write(
            &self.cwd,
            WriteMemory {
                title,
                body,
                memory_type,
                scope,
                confidence: 1.0,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )?;
        Ok(
            json!({ "id": memory.metadata.id, "type": memory.metadata.memory_type, "scope": memory.metadata.scope }),
        )
    }

    fn memory_forget(&self, arguments: Value) -> Result<Value> {
        let arguments: ForgetArguments = serde_json::from_value(arguments)?;
        let memory = self.menvane.forget(arguments.id)?;
        Ok(json!({
            "id": memory.metadata.id,
            "status": memory.metadata.status,
            "reason": arguments.reason
        }))
    }
}

#[derive(Deserialize)]
struct SearchArguments {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_auto")]
    scope: String,
    #[serde(default)]
    include_sessions: bool,
}

#[derive(Deserialize)]
struct ReadArguments {
    id: Uuid,
}

#[derive(Deserialize)]
struct WriteArguments {
    content: String,
    #[serde(default = "default_auto", rename = "type")]
    memory_type: String,
    #[serde(default = "default_auto")]
    scope: String,
}

#[derive(Deserialize)]
struct ForgetArguments {
    id: Uuid,
    #[serde(default)]
    reason: Option<String>,
}

fn default_limit() -> usize {
    10
}

fn default_auto() -> String {
    "auto".to_owned()
}

fn split_content(content: &str) -> (String, String) {
    let content = content.trim();
    if let Some(rest) = content.strip_prefix("# ") {
        let (title, body) = rest.split_once('\n').unwrap_or((rest, ""));
        return (title.trim().to_owned(), body.trim().to_owned());
    }
    let title = content
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Memory")
        .trim()
        .chars()
        .take(80)
        .collect();
    (title, content.to_owned())
}

fn infer_memory_type(content: &str) -> MemoryType {
    if content.contains("## Decision") {
        MemoryType::Decision
    } else if content.contains("## Procedure") {
        MemoryType::Procedure
    } else if content.contains("## Problem") && content.contains("## Resolution") {
        MemoryType::Gotcha
    } else {
        MemoryType::Fact
    }
}

fn write_message(writer: &mut impl Write, message: Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, &message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "memory_search",
            "Search project and global memories",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10, "minimum": 1 },
                    "scope": { "type": "string", "enum": ["auto", "project", "global"], "default": "auto" },
                    "include_sessions": { "type": "boolean", "default": false }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_read",
            "Read a complete durable memory",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string", "format": "uuid" } },
                "required": ["id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_write",
            "Create a durable memory",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "type": { "type": "string", "enum": ["auto", "fact", "decision", "procedure", "gotcha"], "default": "auto" },
                    "scope": { "type": "string", "enum": ["auto", "project", "global"], "default": "auto" }
                },
                "required": ["content"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_forget",
            "Mark a memory forgotten without deleting Markdown",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "reason": { "type": "string" }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn lists_exactly_four_public_tools() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
        );
        let mut output = Vec::new();
        McpServer::new(&menvane, project)
            .serve(Cursor::new(input), &mut output)
            .unwrap();
        let responses = String::from_utf8(output).unwrap();
        let tools_response: Value =
            serde_json::from_str(responses.lines().nth(1).unwrap()).unwrap();
        let tools = tools_response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "memory_search",
                "memory_read",
                "memory_write",
                "memory_forget"
            ]
        );
    }
}
