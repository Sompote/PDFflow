//! `todo_write` (dsh-tool-todo): the agent replaces its whole todo list; the list is a
//! durable `todo/write` event. It is not repeated in the system prompt, which stays
//! stable so the provider's prefix cache keeps working; the model sees the list in its own
//! tool call.

use serde_json::{json, Value};

use crate::context::{Context, Plugin};
use crate::keys;
use crate::session::{EventBody, TodoItem};
use crate::tools::{ToolDef, ToolOutput, ToolRegistry};

pub struct Todo;

impl Plugin for Todo {
    fn name(&self) -> String {
        "hive-tool-todo".into()
    }
    fn describe(&self) -> String {
        "todo_write + todo/write events".into()
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        let def = ToolDef::new(
            "todo_write",
            "Replace your whole todo list. Use it for work with three or more steps: write the plan, mark one item in_progress while you work on it and completed as soon as it is done. Statuses: pending, in_progress, completed.",
            json!({"type":"object","properties":{"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"]}}},"required":["todos"]}),
            |t, args: Value| {
                let todos: Vec<TodoItem> = serde_json::from_value(args.get("todos").cloned().unwrap_or(json!([]))).map_err(|e| format!("todos: {e}"))?;
                if let Some(bad) = todos.iter().find(|t| !["pending", "in_progress", "completed"].contains(&t.status.as_str())) {
                    return Err(format!("unknown status '{}'", bad.status));
                }
                let done = todos.iter().filter(|t| t.status == "completed").count();
                let n = todos.len();
                t.agent.session.append(EventBody::TodoWrite { todos });
                Ok(ToolOutput::ok(format!("Todo list saved: {n} items, {done} completed.")))
            },
        )
        .always();
        tools.register(ctx, def)?;
        Ok(())
    }
}
