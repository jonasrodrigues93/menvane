use menvane_domain::{HandoffItem, NormalizedEvent, NormalizedEventKind, NormalizedEventOrigin};

pub const MAX_SESSION_MARKDOWN_BYTES: usize = 32_768;

pub fn render_session_markdown(events: &[NormalizedEvent], max_bytes: usize) -> String {
    let mut events = events.to_vec();
    events.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    bounded_string(
        &events
            .iter()
            .map(render_event)
            .collect::<Vec<_>>()
            .join("\n"),
        max_bytes,
    )
}

pub fn render_handoff_items(items: &[HandoffItem], max_bytes: usize) -> String {
    let mut items = items.to_vec();
    items.sort_by_key(|item| item.id);
    let body = items
        .iter()
        .map(|item| {
            format!(
                "- [{}] {}{}{}",
                serde_json::to_string(&item.kind).unwrap_or_else(|_| "\"in-progress\"".to_owned()),
                item.state,
                item.next_step
                    .as_deref()
                    .map_or(String::new(), |value| format!(" Next: {value}")),
                item.blocker
                    .as_deref()
                    .map_or(String::new(), |value| format!(" Blocked by: {value}"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    bounded_string(&body, max_bytes)
}

fn render_event(event: &NormalizedEvent) -> String {
    let mut line = format!(
        "- `{}` [event:{}] {} ({})",
        event.timestamp.to_rfc3339(),
        event.event_id,
        event_label(event.kind),
        origin_label(event.origin)
    );
    if let Some(tool) = event.tool_family.as_deref() {
        line.push_str(&format!(" tool `{}`", bounded_string(tool, 1_024)));
    }
    if let Some(success) = event.success {
        line.push_str(if success { " succeeded" } else { " failed" });
    }
    if let Some(path) = event.attributed_path.as_deref() {
        line.push_str(&format!(" on {}", bounded_string(path, 1_024)));
    }
    if let Some(input) = event
        .bounded_input
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        line.push_str(&format!("\n  input: {}", input.replace('\n', "\n  ")));
    }
    if let Some(output) = event
        .bounded_output
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        line.push_str(&format!("\n  output: {}", output.replace('\n', "\n  ")));
    }
    line
}

fn event_label(kind: NormalizedEventKind) -> &'static str {
    match kind {
        NormalizedEventKind::SessionStarted => "session-started",
        NormalizedEventKind::UserPrompt => "user-prompt",
        NormalizedEventKind::ToolCompleted => "tool-completed",
        NormalizedEventKind::ContextCompacted => "context-compacted",
        NormalizedEventKind::TurnStopped => "turn-stopped",
        NormalizedEventKind::SessionEnded => "session-ended",
    }
}

fn origin_label(origin: NormalizedEventOrigin) -> &'static str {
    match origin {
        NormalizedEventOrigin::User => "user",
        NormalizedEventOrigin::System => "system",
        NormalizedEventOrigin::Agent => "agent",
        NormalizedEventOrigin::Compaction => "compaction",
        NormalizedEventOrigin::Tool => "tool",
        NormalizedEventOrigin::Importer => "importer",
    }
}

fn bounded_string(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
