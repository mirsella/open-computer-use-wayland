use std::sync::Arc;

use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Map as JsonObject, Value, json};

use crate::validation::{
    MAX_CLICK_COUNT, MAX_ELEMENT_ID, MAX_SCROLL_STEPS, MAX_TEXT_LIMIT, MAX_TREE_DEPTH,
    MAX_TREE_NODES,
};

pub const TOOL_NAMES: [&str; 6] = [
    "list_applications",
    "launch_application",
    "observe",
    "act_on_element",
    "pointer",
    "keyboard",
];

pub const SERVER_INSTRUCTIONS: &str = "Use `list_applications` to discover running targets or exact installed desktop IDs. Use `observe` before acting; it returns an opaque `state_id`, current AT-SPI state, and the full approved-monitor PNG. Every element, pointer, and keyboard action requires that exact `state_id`; stale state is rejected and successful actions return a new observation. Every `action` argument is an object with a required `type` field, never a string. Element actions use AT-SPI. Pointer and keyboard coordinates use `screenshot_png_pixels`, never AT-SPI frames. Keyboard actions first left-click `focus`; choose a visible point inside the target app. If the target is not visibly reachable, stop instead of using a desktop focus-switch shortcut.";

pub fn tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "list_applications",
            "List running AT-SPI applications or installed desktop entries.",
            object(
                json!({"scope": {"type": "string", "enum": ["running", "installed"]}}),
                &["scope"],
            ),
            true,
            false,
        ),
        tool(
            "launch_application",
            "Launch an exact case-sensitive desktop_id returned by list_applications(installed).",
            object(
                json!({"desktop_id": {"type": "string", "pattern": "^[^\\s]+\\.desktop$"}}),
                &["desktop_id"],
            ),
            false,
            true,
        ),
        tool(
            "observe",
            "Observe one running target by PID, app name, window title, or unique substring.",
            object(
                json!({
                    "target": {"type": "string", "pattern": ".*\\S.*"},
                    "text_limit": {"anyOf": [{"type": "integer", "minimum": 0, "maximum": MAX_TEXT_LIMIT}, {"const": "max"}], "default": 500, "description": "Per-element text limit; max means the server cap."},
                    "max_tree_nodes": {"type": "integer", "minimum": 1, "maximum": MAX_TREE_NODES, "default": 1200},
                    "max_tree_depth": {"type": "integer", "minimum": 1, "maximum": MAX_TREE_DEPTH, "default": 64}
                }),
                &["target"],
            ),
            false,
            false,
        ),
        tool(
            "act_on_element",
            "Invoke, focus, set, or run a named AT-SPI action on an observed element.",
            object(
                json!({
                    "state_id": state_id(),
                    "element_id": {"anyOf": [{"type": "string", "pattern": "^(?:[0-9]{1,3}|[0-4][0-9]{3})$"}, {"type": "integer", "minimum": 0, "maximum": MAX_ELEMENT_ID}]},
                    "action": {"description": "Object, never a string. Use {\"type\":\"invoke\"}, {\"type\":\"focus\"}, {\"type\":\"named\",\"name\":\"...\"}, or {\"type\":\"set_value\",\"value\":\"...\"}.", "oneOf": [
                        action_object("invoke", json!({}), &[]),
                        action_object("focus", json!({}), &[]),
                        action_object("named", json!({"name": {"type": "string", "pattern": ".*\\S.*"}}), &["name"]),
                        action_object("set_value", json!({"value": {"type": "string"}}), &["value"])
                    ]}
                }),
                &["state_id", "element_id", "action"],
            ),
            false,
            true,
        ),
        tool(
            "pointer",
            "Move, click, drag, or scroll in current screenshot pixels.",
            object(
                json!({
                    "state_id": state_id(),
                    "action": {"description": "Object with type move, click, drag, or scroll.", "oneOf": [
                        action_object("move", coordinates(&["x", "y"]), &["x", "y"]),
                        action_object("click", merge(coordinates(&["x", "y"]), json!({
                            "button": {"type": "string", "enum": ["left", "right", "middle"], "default": "left"},
                            "count": {"type": "integer", "minimum": 1, "maximum": MAX_CLICK_COUNT, "default": 1}
                        })), &["x", "y"]),
                        action_object("drag", coordinates(&["from_x", "from_y", "to_x", "to_y"]), &["from_x", "from_y", "to_x", "to_y"]),
                        action_object("scroll", merge(coordinates(&["x", "y"]), json!({
                            "direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
                            "steps": {"type": "integer", "minimum": 1, "maximum": MAX_SCROLL_STEPS, "default": 1}
                        })), &["x", "y", "direction"])
                    ]}
                }),
                &["state_id", "action"],
            ),
            false,
            true,
        ),
        tool(
            "keyboard",
            "Left-click focus in current screenshot pixels, then press a key/chord or type literal text.",
            object(
                json!({
                    "state_id": state_id(),
                    "focus": object(coordinates(&["x", "y"]), &["x", "y"]),
                    "action": {"description": "Object: {\"type\":\"press\",\"key\":\"Ctrl+L\"} or {\"type\":\"type\",\"text\":\"...\"}.", "oneOf": [
                        action_object("press", json!({"key": {"type": "string", "pattern": "^(?!(?=.*(?:^|\\+)\\s*[Aa][Ll][Tt]\\s*(?:\\+|$))(?=.*(?:^|\\+)\\s*[Tt][Aa][Bb]\\s*(?:\\+|$))).*\\S.*$", "description": "Examples: Ctrl+L, Enter, F5, é. Chords containing both Alt and Tab are rejected."}}), &["key"]),
                        action_object("type", json!({"text": {"type": "string"}}), &["text"])
                    ]}
                }),
                &["state_id", "focus", "action"],
            ),
            false,
            true,
        ),
    ]
}

fn tool(
    name: &'static str,
    description: &'static str,
    schema: Value,
    read_only: bool,
    destructive: bool,
) -> Tool {
    let mut annotations = ToolAnnotations::new().read_only(read_only).open_world(true);
    if destructive {
        annotations = annotations.destructive(true).idempotent(false);
    } else if !read_only {
        annotations = annotations.destructive(false).idempotent(false);
    }
    Tool::new(name, description, Arc::new(into_object(schema)))
        .with_raw_output_schema(Arc::new(into_object(output_schema(name))))
        .annotate(annotations)
}

fn output_schema(name: &str) -> Value {
    let success = match name {
        "list_applications" => object(
            json!({
                "scope": {"type": "string", "enum": ["running", "installed"]},
                "applications": {"type": "array", "items": {"type": "object"}}
            }),
            &["scope", "applications"],
        ),
        "launch_application" => object(
            json!({
                "status": {"const": "requested"},
                "desktop_id": {"type": "string"},
                "name": {"type": "string"}
            }),
            &["status", "desktop_id", "name"],
        ),
        "observe" | "act_on_element" | "pointer" | "keyboard" => object(
            json!({
                "state_id": state_id(),
                "target": {"type": "object"},
                "screenshot": {"type": "object", "properties": {
                    "ready": {"type": "boolean"},
                    "reason": {"type": ["string", "null"]},
                    "width": {"type": ["integer", "null"]},
                    "height": {"type": ["integer", "null"]},
                    "coordinate_space": {"const": "screenshot_png_pixels"}
                }, "required": ["ready", "reason", "width", "height", "coordinate_space"]},
                "coordinate_spaces": {"type": "object"},
                "elements": {"type": "array", "items": {"type": "object"}}
            }),
            &[
                "state_id",
                "target",
                "screenshot",
                "coordinate_spaces",
                "elements",
            ],
        ),
        _ => unreachable!("all tools have a structured output schema"),
    };
    json!({"oneOf": [success, error_output_schema()]})
}

fn error_output_schema() -> Value {
    object(
        json!({
            "code": {"type": "string"},
            "message": {"type": "string"},
            "outcome": {"type": "string", "enum": ["not_started", "unknown", "completed"]},
            "retryable": {"type": "boolean"},
            "recovery": {"type": "string"}
        }),
        &["code", "message", "outcome", "retryable", "recovery"],
    )
}

fn state_id() -> Value {
    json!({"type": "string", "pattern": "^s-[0-9a-f]{16}$"})
}

fn coordinates(names: &[&str]) -> Value {
    let properties = names
        .iter()
        .map(|name| ((*name).to_owned(), json!({"type": "number", "minimum": 0})))
        .collect();
    Value::Object(properties)
}

fn action_object(kind: &str, properties: Value, required: &[&str]) -> Value {
    let mut properties = into_object(properties);
    properties.insert("type".into(), json!({"const": kind}));
    let required = std::iter::once("type")
        .chain(required.iter().copied())
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": into_object(properties),
        "required": required,
        "additionalProperties": false
    })
}

fn merge(left: Value, right: Value) -> Value {
    let mut merged = into_object(left);
    merged.extend(into_object(right));
    Value::Object(merged)
}

fn into_object(value: Value) -> JsonObject<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => panic!("schema helper requires an object"),
    }
}
