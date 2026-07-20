use open_computer_use::{
    contract::{TOOL_NAMES, tool_definitions},
    runtime::ToolOutput,
};
use serde_json::{Value, json};

#[test]
fn tools_have_exact_order_and_schemas() {
    let tools = tool_definitions();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, TOOL_NAMES);

    let expected = expected_schemas();
    for (tool, schema) in tools.iter().zip(expected) {
        assert_eq!(tool.schema_as_json_value(), schema, "{} schema", tool.name);
    }
}

#[test]
fn annotations_match_the_inherited_contract() {
    let tools = tool_definitions();
    for tool in tools {
        let annotations = serde_json::to_value(tool.annotations).expect("serialize annotations");
        let expected = if tool.name.as_ref() == "list_applications" {
            json!({
                "openWorldHint": true,
                "readOnlyHint": true,
            })
        } else if tool.name.as_ref() == "observe" {
            json!({
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
                "readOnlyHint": false,
            })
        } else {
            json!({
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true,
                "readOnlyHint": false,
            })
        };
        assert_eq!(annotations, expected, "{} annotations", tool.name);
    }
}

#[test]
fn output_schemas_have_expected_shapes() {
    let tools = tool_definitions();
    let output_schema = |name: &str| {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .expect("tool definition");
        Value::Object(
            tool.output_schema
                .as_ref()
                .expect("output schema")
                .as_ref()
                .clone(),
        )
    };

    let list_schema = output_schema("list_applications");
    let list = &list_schema["oneOf"][0];
    assert_eq!(list["type"], "object");
    assert_eq!(list["required"], json!(["scope", "applications"]));
    assert_eq!(list["properties"]["scope"]["type"], "string");
    assert_eq!(list["properties"]["applications"]["type"], "array");

    let launch_schema = output_schema("launch_application");
    let launch = &launch_schema["oneOf"][0];
    assert_eq!(launch["type"], "object");
    assert_eq!(launch["required"], json!(["status", "desktop_id", "name"]));
    assert_eq!(launch["properties"]["status"]["const"], "requested");
    assert_eq!(launch["properties"]["desktop_id"]["type"], "string");
    assert_eq!(launch["properties"]["name"]["type"], "string");

    let observation_schema = output_schema("observe");
    let observation = &observation_schema["oneOf"][0];
    assert_eq!(observation["type"], "object");
    assert_eq!(
        observation["required"],
        json!([
            "state_id",
            "target",
            "screenshot",
            "coordinate_spaces",
            "elements"
        ])
    );
    assert_eq!(observation["properties"]["state_id"], state_id());
    assert_eq!(observation["properties"]["target"]["type"], "object");
    assert_eq!(
        observation["properties"]["screenshot"]["properties"]["coordinate_space"]["const"],
        "screenshot_png_pixels"
    );
    assert_eq!(
        observation["properties"]["coordinate_spaces"]["type"],
        "object"
    );
    assert_eq!(observation["properties"]["elements"]["type"], "array");

    for name in TOOL_NAMES {
        let schema = output_schema(name);
        let error = &schema["oneOf"][1];
        assert_eq!(error["type"], "object", "{name} error type");
        assert_eq!(error["additionalProperties"], false, "{name} error closure");
        assert_eq!(
            error["required"],
            json!(["code", "message", "outcome", "retryable", "recovery"]),
            "{name} error fields"
        );
    }
}

#[test]
fn image_output_serializes_as_mcp_text_then_png() {
    let result = ToolOutput::text("state")
        .with_png_base64("cG5n")
        .into_mcp_result();
    assert_eq!(
        serde_json::to_value(result).expect("serialize result"),
        json!({
            "content": [
                {"type": "text", "text": "state"},
                {"type": "image", "data": "cG5n", "mimeType": "image/png"},
            ],
            "isError": false,
        })
    );
}

fn expected_schemas() -> Vec<Value> {
    vec![
        object(
            json!({"scope": {"type": "string", "enum": ["running", "installed"]}}),
            &["scope"],
        ),
        object(
            json!({"desktop_id": {"type": "string", "pattern": "^[^\\s]+\\.desktop$"}}),
            &["desktop_id"],
        ),
        object(
            json!({
                "target": {"type": "string", "pattern": ".*\\S.*"},
                "text_limit": {"anyOf": [{"type": "integer", "minimum": 0, "maximum": 100_000}, {"const": "max"}], "default": 500, "description": "Per-element text limit; max means the server cap."},
                "max_tree_nodes": {"type": "integer", "minimum": 1, "maximum": 5_000, "default": 1200},
                "max_tree_depth": {"type": "integer", "minimum": 1, "maximum": 128, "default": 64},
            }),
            &["target"],
        ),
        object(
            json!({
                "state_id": state_id(),
                "element_id": {"anyOf": [{"type": "string", "pattern": "^(?:[0-9]{1,3}|[0-4][0-9]{3})$"}, {"type": "integer", "minimum": 0, "maximum": 4_999}]},
                "action": {"description": "Object, never a string. Use {\"type\":\"invoke\"}, {\"type\":\"focus\"}, {\"type\":\"named\",\"name\":\"...\"}, or {\"type\":\"set_value\",\"value\":\"...\"}.", "oneOf": [
                    action("invoke", json!({}), &[]),
                    action("focus", json!({}), &[]),
                    action("named", json!({"name": {"type": "string", "pattern": ".*\\S.*"}}), &["name"]),
                    action("set_value", json!({"value": {"type": "string"}}), &["value"]),
                ]},
            }),
            &["state_id", "element_id", "action"],
        ),
        object(
            json!({
                "state_id": state_id(),
                "action": {"description": "Object with type move, click, drag, or scroll.", "oneOf": [
                    action("move", coordinates(&["x", "y"]), &["x", "y"]),
                    action("click", merge(coordinates(&["x", "y"]), json!({"button": {"type": "string", "enum": ["left", "right", "middle"], "default": "left"}, "count": {"type": "integer", "minimum": 1, "maximum": 3, "default": 1}})), &["x", "y"]),
                    action("drag", coordinates(&["from_x", "from_y", "to_x", "to_y"]), &["from_x", "from_y", "to_x", "to_y"]),
                    action("scroll", merge(coordinates(&["x", "y"]), json!({"direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "steps": {"type": "integer", "minimum": 1, "maximum": 100, "default": 1}})), &["x", "y", "direction"]),
                ]},
            }),
            &["state_id", "action"],
        ),
        object(
            json!({
                "state_id": state_id(),
                "focus": object(coordinates(&["x", "y"]), &["x", "y"]),
                "action": {"description": "Object: {\"type\":\"press\",\"key\":\"Ctrl+L\"} or {\"type\":\"type\",\"text\":\"...\"}.", "oneOf": [
                    action("press", json!({"key": {"type": "string", "pattern": "^(?!(?=.*(?:^|\\+)\\s*[Aa][Ll][Tt]\\s*(?:\\+|$))(?=.*(?:^|\\+)\\s*[Tt][Aa][Bb]\\s*(?:\\+|$))).*\\S.*$", "description": "Examples: Ctrl+L, Enter, F5, é. Chords containing both Alt and Tab are rejected."}}), &["key"]),
                    action("type", json!({"text": {"type": "string"}}), &["text"]),
                ]},
            }),
            &["state_id", "focus", "action"],
        ),
    ]
}

fn object(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn state_id() -> Value {
    json!({"type": "string", "pattern": "^s-[0-9a-f]{16}$"})
}

fn coordinates(names: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    for name in names {
        properties.insert((*name).to_owned(), json!({"type": "number", "minimum": 0}));
    }
    Value::Object(properties)
}

fn action(kind: &str, properties: Value, required: &[&str]) -> Value {
    let mut properties = properties.as_object().cloned().expect("action properties");
    properties.insert("type".into(), json!({"const": kind}));
    let mut required = required
        .iter()
        .map(|value| json!(value))
        .collect::<Vec<_>>();
    required.insert(0, json!("type"));
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn merge(left: Value, right: Value) -> Value {
    let mut merged = left.as_object().cloned().expect("left object");
    merged.extend(right.as_object().cloned().expect("right object"));
    Value::Object(merged)
}
