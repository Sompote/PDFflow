//! The shared task board of an agent team (dsh-agent-team): one board per root session,
//! a DAG of tasks (`blocked_by` edges, no cycles) that every agent of the tree can read and
//! update. Each change is a complete task snapshot with a revision number, logged as a
//! `team/task` custom event in the root session so the board survives a restart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::{Agent, AgentRegistry};
use crate::context::{Context, Plugin};
use crate::keys;
use crate::session::{EventBody, Session};
use crate::tools::{ToolDef, ToolOutput, ToolRegistry};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    /// pending | in_progress | completed | cancelled
    pub status: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    pub revision: u64,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Default)]
pub struct TeamBoard {
    boards: Mutex<HashMap<String, Vec<Task>>>,
}

const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

impl TeamBoard {
    /// The root agent of `a`'s tree (the board's owner).
    fn root(ctx: &Context, a: &Arc<Agent>) -> Arc<Agent> {
        let Some(reg) = ctx.get::<AgentRegistry>(keys::AGENTS) else { return a.clone() };
        let mut cur = a.clone();
        while let Some(p) = cur.parent.as_ref().and_then(|p| reg.get(p)) {
            cur = p;
        }
        cur
    }

    fn load(session: &Session) -> Vec<Task> {
        let mut tasks: Vec<Task> = vec![];
        for e in session.events() {
            if let EventBody::Custom { kind, data } = e.body {
                if kind == "team/task" {
                    if let Ok(t) = serde_json::from_value::<Task>(data) {
                        match tasks.iter_mut().find(|x| x.id == t.id) {
                            Some(x) => *x = t,
                            None => tasks.push(t),
                        }
                    }
                }
            }
        }
        tasks
    }

    fn with<R>(&self, root: &Arc<Agent>, f: impl FnOnce(&mut Vec<Task>) -> R) -> R {
        let mut b = self.boards.lock().expect("board");
        let tasks = b.entry(root.session.id().to_string()).or_insert_with(|| Self::load(&root.session));
        f(tasks)
    }

    pub fn tasks(&self, ctx: &Context, a: &Arc<Agent>) -> Vec<Task> {
        let root = Self::root(ctx, a);
        self.with(&root, |t| t.iter().filter(|t| !t.deleted).cloned().collect())
    }

    fn save(root: &Arc<Agent>, t: &Task) {
        root.session.append(EventBody::Custom { kind: "team/task".into(), data: serde_json::to_value(t).unwrap_or(Value::Null) });
    }

    fn cycle(tasks: &[Task], id: &str, deps: &[String]) -> bool {
        let mut stack: Vec<String> = deps.to_vec();
        let mut seen = std::collections::HashSet::new();
        while let Some(d) = stack.pop() {
            if d == id {
                return true;
            }
            if seen.insert(d.clone()) {
                if let Some(t) = tasks.iter().find(|t| t.id == d) {
                    stack.extend(t.blocked_by.iter().cloned());
                }
            }
        }
        false
    }

    pub fn create(&self, ctx: &Context, a: &Arc<Agent>, title: &str, detail: &str, owner: &str, blocked_by: Vec<String>) -> Result<Task, String> {
        let root = Self::root(ctx, a);
        self.with(&root, |tasks| {
            for d in &blocked_by {
                if !tasks.iter().any(|t| &t.id == d && !t.deleted) {
                    return Err(format!("no task '{d}'"));
                }
            }
            let t = Task { id: format!("task-{}", tasks.len() + 1), title: title.into(), detail: detail.into(), status: "pending".into(), owner: owner.into(), blocked_by, notes: vec![], revision: 1, deleted: false };
            Self::save(&root, &t);
            tasks.push(t.clone());
            Ok(t)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(&self, ctx: &Context, a: &Arc<Agent>, id: &str, revision: Option<u64>, status: Option<&str>, owner: Option<&str>, note: Option<&str>, blocked_by: Option<Vec<String>>) -> Result<Task, String> {
        let root = Self::root(ctx, a);
        let author = a.preset.name.clone();
        self.with(&root, |tasks| {
            let snapshot = tasks.clone();
            let t = tasks.iter_mut().find(|t| t.id == id && !t.deleted).ok_or_else(|| format!("no task '{id}'"))?;
            if let Some(r) = revision {
                if r != t.revision {
                    return Err(format!("{id} changed meanwhile (revision {} now, you sent {r}); read it with task_list and try again", t.revision));
                }
            }
            let before = (t.status.clone(), t.owner.clone(), t.blocked_by.clone(), t.notes.len());
            if let Some(s) = status {
                if !STATUSES.contains(&s) {
                    return Err(format!("status must be one of {}", STATUSES.join(", ")));
                }
                t.status = s.into();
            }
            if let Some(o) = owner {
                t.owner = o.into();
            }
            if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
                t.notes.push(format!("{author}: {n}"));
            }
            if let Some(deps) = blocked_by {
                if let Some(bad) = deps.iter().find(|d| !snapshot.iter().any(|x| &x.id == *d && !x.deleted)) {
                    return Err(format!("no task '{bad}'"));
                }
                if Self::cycle(&snapshot, id, &deps) {
                    return Err("that dependency would make a cycle".into());
                }
                t.blocked_by = deps;
            }
            // an update that changes nothing is not a new revision
            if (t.status.clone(), t.owner.clone(), t.blocked_by.clone(), t.notes.len()) == before {
                return Ok(t.clone());
            }
            t.revision += 1;
            let out = t.clone();
            Self::save(&root, &out);
            Ok(out)
        })
    }

    pub fn render(tasks: &[Task]) -> String {
        if tasks.is_empty() {
            return "The task board is empty.".into();
        }
        let done = |id: &String| tasks.iter().any(|t| &t.id == id && t.status == "completed");
        let mut s = String::new();
        for t in tasks {
            let waiting: Vec<&String> = t.blocked_by.iter().filter(|d| !done(d)).collect();
            s.push_str(&format!("- {} [{}] {} (rev {}{}{})\n", t.id, t.status, t.title, t.revision, if t.owner.is_empty() { String::new() } else { format!(", owner {}", t.owner) }, if waiting.is_empty() { String::new() } else { format!(", waits for {}", waiting.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")) }));
            if !t.detail.is_empty() {
                s.push_str(&format!("  {}\n", crate::clip(&t.detail, 300)));
            }
            for n in t.notes.iter().rev().take(3).rev() {
                s.push_str(&format!("  · {}\n", crate::clip(n, 300)));
            }
        }
        s
    }
}

pub struct TeamBoardPlugin;

fn strs(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
}

impl Plugin for TeamBoardPlugin {
    fn name(&self) -> String {
        "hive-team-board".into()
    }
    fn describe(&self) -> String {
        "task_create / task_update / task_list, shared by the agent tree".into()
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS, keys::AGENTS]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let board = Arc::new(TeamBoard::default());
        ctx.provide(keys::TEAM_BOARD, board.clone())?;
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        let b = board.clone();
        tools.register(
            ctx,
            ToolDef::new(
                "task_create",
                "Add a task to the team's shared task board (seen by every agent of this job). Use it to lay out a design job's tasks and their order.",
                json!({"type":"object","properties":{"title":{"type":"string"},"detail":{"type":"string"},"owner":{"type":"string","description":"Agent id that should do it"},"blocked_by":{"type":"array","items":{"type":"string"},"description":"Task ids that must finish first"}},"required":["title"]}),
                move |t, a| {
                    let s = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let task = b.create(&t.ctx, &t.agent, &s("title"), &s("detail"), &s("owner"), strs(a.get("blocked_by")).unwrap_or_default())?;
                    Ok(ToolOutput::ok(format!("Created {} (rev {}).", task.id, task.revision)))
                },
            )
            .always(),
        )?;
        let b = board.clone();
        tools.register(
            ctx,
            ToolDef::new(
                "task_update",
                "Change a task on the shared board: status (pending, in_progress, completed, cancelled), owner, dependencies, or add a note with results. Pass the revision you read to avoid overwriting another agent's change.",
                json!({"type":"object","properties":{"id":{"type":"string"},"revision":{"type":"integer"},"status":{"type":"string","enum":STATUSES},"owner":{"type":"string"},"note":{"type":"string"},"blocked_by":{"type":"array","items":{"type":"string"}}},"required":["id"]}),
                move |t, a| {
                    let s = |k: &str| a.get(k).and_then(|v| v.as_str());
                    let task = b.update(&t.ctx, &t.agent, s("id").ok_or("missing 'id'")?, a.get("revision").and_then(|v| v.as_u64()), s("status"), s("owner"), s("note"), strs(a.get("blocked_by")))?;
                    let changed = a.get("revision").and_then(|v| v.as_u64()).map(|r| task.revision > r).unwrap_or(true);
                    if !changed {
                        let deps = if task.blocked_by.is_empty() { "no dependencies".to_string() } else { format!("blocked by {}", task.blocked_by.join(", ")) };
                        let owner = if task.owner.is_empty() { "no owner".to_string() } else { format!("owner {}", task.owner) };
                        return Ok(ToolOutput::ok(format!("No change: {} is already {} with {owner} and {deps} (rev {}). Updating it again does nothing; the board only records work. To get the work done, delegate it to a team member.", task.id, task.status, task.revision)));
                    }
                    Ok(ToolOutput::ok(format!("{} is now {} (rev {}).", task.id, task.status, task.revision)))
                },
            )
            .always(),
        )?;
        let b = board;
        tools.register(
            ctx,
            ToolDef::new("task_list", "Read the team's shared task board.", json!({"type":"object","properties":{}}), move |t, _| Ok(ToolOutput::ok(TeamBoard::render(&b.tasks(&t.ctx, &t.agent)))))
                .concurrent()
                .always(),
        )?;
        Ok(())
    }
}
