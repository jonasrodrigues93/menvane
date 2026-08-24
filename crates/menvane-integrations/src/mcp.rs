use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use menvane_domain::{Applicability, KnowledgeMetadata, KnowledgeType, Scope};
use menvane_engine::{Menvane, ScopeSelection, WriteMemory};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MCP_SEARCH_QUERY_BYTES: usize = 4_096;
const MAX_MCP_SEARCH_LIMIT: usize = 50;
const MAX_MCP_SEARCH_RESULTS: usize = 50;
const MAX_MCP_SEARCH_EXCERPT_CHARS: usize = 512;
const DEFAULT_MCP_READ_LENGTH: usize = 4_096;
const MAX_MCP_READ_LENGTH: usize = 8_192;
const MAX_MCP_METADATA_ITEMS: usize = 16;
const MAX_MCP_METADATA_STRING_CHARS: usize = 128;
const MAX_MCP_RESULT_BYTES: usize = 24_000;
const MAX_MCP_RESPONSE_BYTES: usize = 32_768;

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
        let query = truncate_utf8_bytes(&arguments.query, MAX_MCP_SEARCH_QUERY_BYTES);
        let limit = arguments
            .limit
            .clamp(1, MAX_MCP_SEARCH_LIMIT)
            .min(MAX_MCP_SEARCH_RESULTS);
        let scope = match arguments.scope.as_str() {
            "auto" => ScopeSelection::Auto,
            "project" => ScopeSelection::Project,
            "global" => ScopeSelection::Global,
            value => anyhow::bail!("unsupported scope: {value}"),
        };
        let results = if arguments.include_forgotten {
            self.menvane
                .search_including_forgotten(&self.cwd, &query, scope, limit)?
        } else {
            self.menvane.search(&self.cwd, &query, scope, limit)?
        };
        let mut output = Vec::new();
        for memory in results.into_iter().take(MAX_MCP_SEARCH_RESULTS) {
            output.push(json!({
                "id": memory.id,
                "type": memory.knowledge_type,
                "scope": memory.scope,
                "title": truncate_chars(&memory.title, MAX_MCP_METADATA_STRING_CHARS),
                "score": memory.score,
                "status": memory.status,
                "applicability": bounded_applicability(&memory.applicability),
                "short_excerpt": truncate_chars(&memory.excerpt, MAX_MCP_SEARCH_EXCERPT_CHARS)
            }));
            if serialized_pretty_size(&Value::Array(output.clone()))? > MAX_MCP_RESULT_BYTES {
                output.pop();
                break;
            }
        }
        Ok(Value::Array(output))
    }

    fn memory_read(&self, arguments: Value) -> Result<Value> {
        let arguments: ReadArguments = serde_json::from_value(arguments)?;
        let memory = self.menvane.read_from_mcp(arguments.id)?;
        let markdown = format!("# {}\n\n{}", memory.title, memory.body);
        let unit = match arguments.range_unit.as_str() {
            "characters" | "bytes" => arguments.range_unit,
            value => anyhow::bail!("unsupported range unit: {value}"),
        };
        let mut length = arguments.length.clamp(1, MAX_MCP_READ_LENGTH);
        loop {
            let (excerpt, range) = slice_range(&markdown, arguments.offset, length, &unit);
            let value = json!({
                "metadata": bounded_metadata(&memory.metadata),
                "full_markdown_body": excerpt,
                "range": range,
                "source_sessions": bounded_uuid_list(&memory.metadata.source_sessions),
                "supersession_chain": bounded_uuid_list(&memory.metadata.supersedes)
            });
            if serialized_pretty_size(&value)? <= MAX_MCP_RESULT_BYTES {
                return Ok(value);
            }
            if length == 1 {
                anyhow::bail!("bounded memory metadata exceeds MCP response limit");
            }
            length = length.saturating_sub((length / 4).max(1));
        }
    }

    fn memory_write(&self, arguments: Value) -> Result<Value> {
        let arguments: WriteArguments = serde_json::from_value(arguments)?;
        let (title, body) = split_content(&arguments.content);
        let knowledge_type = match arguments.knowledge_type.as_str() {
            "memory" => KnowledgeType::Memory,
            "playbook" => KnowledgeType::Playbook,
            value => anyhow::bail!("unsupported memory type: {value}"),
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
                knowledge_type,
                scope,
                tags: Vec::new(),
                applies_to: Applicability::default(),
            },
        )?;
        Ok(
            json!({ "id": memory.metadata.id, "type": memory.metadata.knowledge_type, "scope": memory.metadata.scope }),
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
    include_forgotten: bool,
}

#[derive(Deserialize)]
struct ReadArguments {
    id: Uuid,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_read_length")]
    length: usize,
    #[serde(default = "default_range_unit")]
    range_unit: String,
}

#[derive(Deserialize)]
struct WriteArguments {
    content: String,
    #[serde(default = "default_memory", rename = "type")]
    knowledge_type: String,
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

fn default_read_length() -> usize {
    DEFAULT_MCP_READ_LENGTH
}

fn default_range_unit() -> String {
    "characters".to_owned()
}

fn default_auto() -> String {
    "auto".to_owned()
}

fn default_memory() -> String {
    "memory".to_owned()
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

fn truncate_utf8_bytes(value: &str, maximum: usize) -> String {
    let end = value.len().min(maximum);
    let end = (0..=end)
        .rev()
        .find(|index| value.is_char_boundary(*index))
        .unwrap_or(0);
    value[..end].to_owned()
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn bounded_strings(values: &[String]) -> Vec<String> {
    values
        .iter()
        .take(MAX_MCP_METADATA_ITEMS)
        .map(|value| truncate_chars(value, MAX_MCP_METADATA_STRING_CHARS))
        .collect()
}

fn bounded_uuid_list(values: &[Uuid]) -> Vec<Uuid> {
    values
        .iter()
        .copied()
        .take(MAX_MCP_METADATA_ITEMS)
        .collect()
}

fn bounded_applicability(applicability: &Applicability) -> Value {
    json!({
        "languages": bounded_strings(&applicability.languages),
        "frameworks": bounded_strings(&applicability.frameworks),
        "tools": bounded_strings(&applicability.tools),
        "databases": bounded_strings(&applicability.databases),
        "platforms": bounded_strings(&applicability.platforms)
    })
}

fn bounded_metadata(metadata: &KnowledgeMetadata) -> Value {
    let mut metadata = metadata.clone();
    metadata.project_id = metadata
        .project_id
        .as_deref()
        .map(|value| truncate_chars(value, MAX_MCP_METADATA_STRING_CHARS));
    metadata.source_sessions = bounded_uuid_list(&metadata.source_sessions);
    metadata.tags = bounded_strings(&metadata.tags);
    metadata.applies_to.languages = bounded_strings(&metadata.applies_to.languages);
    metadata.applies_to.frameworks = bounded_strings(&metadata.applies_to.frameworks);
    metadata.applies_to.tools = bounded_strings(&metadata.applies_to.tools);
    metadata.applies_to.databases = bounded_strings(&metadata.applies_to.databases);
    metadata.applies_to.platforms = bounded_strings(&metadata.applies_to.platforms);
    metadata.supersedes = bounded_uuid_list(&metadata.supersedes);
    metadata.source_project_ids = bounded_strings(&metadata.source_project_ids);
    serde_json::to_value(metadata).unwrap_or_else(|_| json!({}))
}

fn slice_range(
    value: &str,
    requested_offset: usize,
    requested_length: usize,
    unit: &str,
) -> (String, Value) {
    match unit {
        "characters" => {
            let total = value.chars().count();
            let offset = requested_offset.min(total);
            let end = offset.saturating_add(requested_length).min(total);
            let start_byte = value
                .char_indices()
                .nth(offset)
                .map_or(value.len(), |(index, _)| index);
            let end_byte = value
                .char_indices()
                .nth(end)
                .map_or(value.len(), |(index, _)| index);
            (
                value[start_byte..end_byte].to_owned(),
                json!({
                    "unit": unit,
                    "offset": offset,
                    "returned": end - offset,
                    "total": total,
                    "has_more": end < total
                }),
            )
        }
        "bytes" => {
            let total = value.len();
            let requested_offset = requested_offset.min(total);
            let offset = (0..=requested_offset)
                .rev()
                .find(|index| value.is_char_boundary(*index))
                .unwrap_or(0);
            let requested_end = offset.saturating_add(requested_length).min(total);
            let end = (0..=requested_end)
                .rev()
                .find(|index| value.is_char_boundary(*index))
                .unwrap_or(offset);
            (
                value[offset..end].to_owned(),
                json!({
                    "unit": unit,
                    "offset": offset,
                    "returned": end - offset,
                    "total": total,
                    "has_more": end < total
                }),
            )
        }
        _ => (String::new(), json!({})),
    }
}

fn serialized_pretty_size(value: &Value) -> Result<usize> {
    Ok(serde_json::to_string_pretty(value)?.len())
}

fn write_message(writer: &mut impl Write, message: Value) -> Result<()> {
    let mut encoded = serde_json::to_vec(&message)?;
    if encoded.len() > MAX_MCP_RESPONSE_BYTES {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        encoded = serde_json::to_vec(&rpc_error(
            id,
            -32603,
            "serialized MCP response exceeded the hard limit",
        ))?;
    }
    writer.write_all(&encoded)?;
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
            "Search project and global memories; query is capped at 4096 UTF-8 bytes, results and limit at 50, excerpts at 512 characters, and the serialized response at 32768 bytes",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "maxLength": MAX_MCP_SEARCH_QUERY_BYTES },
                    "limit": { "type": "integer", "default": 10, "minimum": 1, "maximum": MAX_MCP_SEARCH_LIMIT },
                    "scope": { "type": "string", "enum": ["auto", "project", "global"], "default": "auto" },
                    "include_forgotten": { "type": "boolean", "default": false },
                },
                "required": ["query"],
                "additionalProperties": false,
                "x-max-query-bytes": MAX_MCP_SEARCH_QUERY_BYTES,
                "x-max-results": MAX_MCP_SEARCH_RESULTS,
                "x-max-excerpt-characters": MAX_MCP_SEARCH_EXCERPT_CHARS,
                "x-max-response-bytes": MAX_MCP_RESPONSE_BYTES
            }),
        ),
        tool(
            "memory_read",
            "Read a bounded UTF-8-safe memory range",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "length": { "type": "integer", "minimum": 1, "maximum": MAX_MCP_READ_LENGTH, "default": DEFAULT_MCP_READ_LENGTH },
                    "range_unit": { "type": "string", "enum": ["characters", "bytes"], "default": "characters" }
                },
                "required": ["id"],
                "additionalProperties": false,
                "x-max-metadata-items": MAX_MCP_METADATA_ITEMS,
                "x-max-provenance-items": MAX_MCP_METADATA_ITEMS,
                "x-max-response-bytes": MAX_MCP_RESPONSE_BYTES
            }),
        ),
        tool(
            "memory_write",
            "Create a durable memory",
            json!({
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
            "type": { "type": "string", "enum": ["memory", "playbook"] },
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

    use jsonschema::validator_for;
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
        let search_schema = &tools[0]["inputSchema"];
        assert_eq!(
            search_schema["properties"]["query"]["maxLength"],
            MAX_MCP_SEARCH_QUERY_BYTES
        );
        assert_eq!(
            search_schema["properties"]["limit"]["maximum"],
            MAX_MCP_SEARCH_LIMIT
        );
        assert_eq!(
            search_schema["x-max-query-bytes"],
            MAX_MCP_SEARCH_QUERY_BYTES
        );
        assert_eq!(search_schema["x-max-results"], MAX_MCP_SEARCH_RESULTS);
        assert_eq!(
            search_schema["properties"]["include_forgotten"]["default"],
            false
        );
        assert_eq!(
            search_schema["x-max-excerpt-characters"],
            MAX_MCP_SEARCH_EXCERPT_CHARS
        );
        assert_eq!(
            search_schema["x-max-response-bytes"],
            MAX_MCP_RESPONSE_BYTES
        );
        let read_schema = &tools[1]["inputSchema"];
        assert_eq!(
            read_schema["properties"]["length"]["default"],
            DEFAULT_MCP_READ_LENGTH
        );
        assert_eq!(
            read_schema["properties"]["length"]["maximum"],
            MAX_MCP_READ_LENGTH
        );
        assert_eq!(read_schema["x-max-metadata-items"], MAX_MCP_METADATA_ITEMS);
        assert_eq!(
            read_schema["x-max-provenance-items"],
            MAX_MCP_METADATA_ITEMS
        );
    }

    #[test]
    fn tools_list_matches_the_versioned_contract() {
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
        let response: Value = serde_json::from_str(responses.lines().nth(1).unwrap()).unwrap();
        let schema: Value = serde_json::from_str(include_str!(
            "../../../contracts/v1/mcp-tools-list.schema.json"
        ))
        .unwrap();
        assert!(validator_for(&schema).unwrap().is_valid(&response));
    }

    #[test]
    fn oversized_search_inputs_are_capped_and_response_is_bounded() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        menvane
            .write(
                &project,
                WriteMemory {
                    title: "Oversized query target".to_owned(),
                    body: "oversized-marker".to_owned(),
                    knowledge_type: KnowledgeType::Memory,
                    scope: Scope::Global,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        let query = format!(
            "oversized-marker {}",
            "noise ".repeat(MAX_MCP_SEARCH_QUERY_BYTES)
        );
        let (value, encoded) = call_tool(
            &menvane,
            &project,
            "memory_search",
            json!({ "query": query, "limit": usize::MAX }),
        );
        let results = value.as_array().unwrap();
        assert!(results.len() <= MAX_MCP_SEARCH_RESULTS);
        assert!(encoded.len() <= MAX_MCP_RESPONSE_BYTES);
        assert!(results.iter().all(|result| {
            result["short_excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.chars().count() <= MAX_MCP_SEARCH_EXCERPT_CHARS)
        }));
    }

    #[test]
    fn large_markdown_can_be_reconstructed_with_character_ranges() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let body = "α😀z\n".repeat(20_000);
        let memory = menvane
            .write(
                &project,
                WriteMemory {
                    title: "Large UTF-8 memory".to_owned(),
                    body,
                    knowledge_type: KnowledgeType::Memory,
                    scope: Scope::Global,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        let expected = format!("# {}\n\n{}", memory.title, memory.body);
        let mut offset = 0;
        let mut reconstructed = String::new();
        loop {
            let (value, encoded) = call_tool(
                &menvane,
                &project,
                "memory_read",
                json!({ "id": memory.metadata.id, "offset": offset, "length": usize::MAX }),
            );
            assert!(encoded.len() <= MAX_MCP_RESPONSE_BYTES);
            let range = &value["range"];
            assert_eq!(range["unit"], "characters");
            let chunk = value["full_markdown_body"].as_str().unwrap();
            assert!(chunk.is_char_boundary(0));
            reconstructed.push_str(chunk);
            let returned = range["returned"].as_u64().unwrap() as usize;
            assert_eq!(returned, chunk.chars().count());
            if !range["has_more"].as_bool().unwrap() {
                break;
            }
            offset = range["offset"].as_u64().unwrap() as usize + returned;
        }
        assert_eq!(reconstructed, expected);
        assert!(menvane.memory_reinforcement(memory.metadata.id).unwrap().0 > 0);
    }

    #[test]
    fn byte_ranges_never_split_utf8() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let menvane = Menvane::new(temporary.path().join("home")).unwrap();
        let memory = menvane
            .write(
                &project,
                WriteMemory {
                    title: "Byte range memory".to_owned(),
                    body: "prefix😀suffix".to_owned(),
                    knowledge_type: KnowledgeType::Memory,
                    scope: Scope::Global,
                    tags: Vec::new(),
                    applies_to: Applicability::default(),
                },
            )
            .unwrap();
        let full = format!("# {}\n\n{}", memory.title, memory.body);
        let emoji_offset = full.find('😀').unwrap() + 1;
        let (value, encoded) = call_tool(
            &menvane,
            &project,
            "memory_read",
            json!({
                "id": memory.metadata.id,
                "offset": emoji_offset,
                "length": 4,
                "range_unit": "bytes"
            }),
        );
        let chunk = value["full_markdown_body"].as_str().unwrap();
        assert_eq!(chunk, "😀");
        assert_eq!(value["range"]["offset"], emoji_offset - 1);
        assert_eq!(value["range"]["returned"], 4);
        assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
        assert!(encoded.len() <= MAX_MCP_RESPONSE_BYTES);
    }

    #[test]
    fn metadata_and_provenance_are_bounded() {
        let mut metadata = KnowledgeMetadata::new(
            KnowledgeType::Memory,
            Scope::Global,
            None,
            vec!["tag".repeat(1_000); MAX_MCP_METADATA_ITEMS * 2],
            Applicability {
                languages: vec!["language".repeat(1_000); MAX_MCP_METADATA_ITEMS * 2],
                frameworks: Vec::new(),
                tools: Vec::new(),
                databases: Vec::new(),
                platforms: Vec::new(),
            },
            menvane_domain::MemoryStatus::Active,
        );
        metadata.source_sessions = (0..MAX_MCP_METADATA_ITEMS * 2)
            .map(|_| Uuid::now_v7())
            .collect();
        metadata.supersedes = metadata.source_sessions.clone();
        let bounded = bounded_metadata(&metadata);
        assert!(serialized_pretty_size(&bounded).unwrap() <= MAX_MCP_RESULT_BYTES);
        assert_eq!(
            bounded["tags"].as_array().unwrap().len(),
            MAX_MCP_METADATA_ITEMS
        );
        assert_eq!(
            bounded["applies_to"]["languages"].as_array().unwrap().len(),
            MAX_MCP_METADATA_ITEMS
        );
        assert_eq!(
            bounded["source_sessions"].as_array().unwrap().len(),
            MAX_MCP_METADATA_ITEMS
        );
        assert_eq!(
            bounded["supersedes"].as_array().unwrap().len(),
            MAX_MCP_METADATA_ITEMS
        );
    }

    fn call_tool(
        menvane: &Menvane,
        project: &std::path::Path,
        name: &str,
        arguments: Value,
    ) -> (Value, String) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let input = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{}}}}\n{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n{}\n",
            serde_json::to_string(&request).unwrap()
        );
        let mut output = Vec::new();
        McpServer::new(menvane, project)
            .serve(Cursor::new(input), &mut output)
            .unwrap();
        let encoded = output
            .split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty())
            .map(|line| String::from_utf8(line.to_vec()))
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(response["result"]["isError"], false);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        (serde_json::from_str(text).unwrap(), encoded)
    }
}
