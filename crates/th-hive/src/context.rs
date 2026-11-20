//! A Cordis-style plugin context: every part of the harness is a plugin.
//!
//! A [`Context`] is a repository of services (`ctx.get("tools")`) shared by a plugin tree.
//! A plugin names the services it needs (`inject`); it is applied once they all exist and
//! waits otherwise. Everything a plugin registers (services, hook listeners, tools, prompt
//! sections) is recorded as an effect on the plugin's own scope, so unloading the plugin
//! unwinds its registrations in reverse order, and a plugin whose service disappears is
//! unloaded with it and goes back to waiting.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::events::Disposer;

/// A unit of the harness.
pub trait Plugin: Send + Sync {
    /// Unique name in the tree (`dsh-tool-todo` style).
    fn name(&self) -> String;
    /// Services that must exist before `apply`.
    fn inject(&self) -> Vec<&'static str> {
        vec![]
    }
    /// Registers the plugin's services, hooks and tools on `ctx` (its own scope).
    fn apply(&self, ctx: &Context) -> Result<(), String>;
    /// One line for `plugins` listings.
    fn describe(&self) -> String {
        String::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    Active,
    /// Waiting for these services.
    Pending(Vec<String>),
    Failed(String),
    Disabled,
}

struct Scope {
    name: String,
    effects: Mutex<Vec<Disposer>>,
}

impl Scope {
    fn dispose(&self) {
        let effects: Vec<Disposer> = self.effects.lock().map(|mut e| e.drain(..).collect()).unwrap_or_default();
        for d in effects.into_iter().rev() {
            d();
        }
    }
}

struct Entry {
    plugin: Arc<dyn Plugin>,
    scope: Arc<Scope>,
    status: PluginStatus,
    /// Services this plugin provides (so their loss unloads dependants).
    provides: Vec<&'static str>,
}

struct Root {
    services: RwLock<HashMap<&'static str, (Arc<dyn Any + Send + Sync>, String)>>,
    entries: Mutex<Vec<Entry>>,
}

/// The shared plugin context, seen through one plugin's scope.
#[derive(Clone)]
pub struct Context {
    root: Arc<Root>,
    scope: Arc<Scope>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    pub fn new() -> Context {
        Context {
            root: Arc::new(Root { services: RwLock::new(HashMap::new()), entries: Mutex::new(vec![]) }),
            scope: Arc::new(Scope { name: "root".into(), effects: Mutex::new(vec![]) }),
        }
    }

    /// The scope name (the plugin this view belongs to).
    pub fn owner(&self) -> &str {
        &self.scope.name
    }

    /// Records a disposer that runs when this scope's plugin unloads.
    pub fn effect(&self, d: Disposer) {
        if let Ok(mut e) = self.scope.effects.lock() {
            e.push(d);
        }
    }

    /// Provides a service under `key`. The registration is an effect of this scope.
    pub fn provide<T: Any + Send + Sync>(&self, key: &'static str, svc: Arc<T>) -> Result<(), String> {
        {
            let mut s = self.root.services.write().map_err(|_| "poisoned")?;
            if let Some((_, owner)) = s.get(key) {
                return Err(format!("service '{key}' is already provided by {owner}"));
            }
            s.insert(key, (svc, self.scope.name.clone()));
        }
        if let Ok(mut entries) = self.root.entries.lock() {
            if let Some(e) = entries.iter_mut().find(|e| Arc::ptr_eq(&e.scope, &self.scope)) {
                e.provides.push(key);
            }
        }
        let root = Arc::downgrade(&self.root);
        self.effect(Box::new(move || {
            if let Some(r) = root.upgrade() {
                if let Ok(mut s) = r.services.write() {
                    s.remove(key);
                }
                Context::cascade(&r, key);
            }
        }));
        self.retry_pending();
        Ok(())
    }

    pub fn get<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        let s = self.root.services.read().ok()?;
        s.get(key).and_then(|(svc, _)| svc.clone().downcast::<T>().ok())
    }

    pub fn expect<T: Any + Send + Sync>(&self, key: &str) -> Result<Arc<T>, String> {
        self.get(key).ok_or_else(|| format!("service '{key}' is not available"))
    }

    pub fn has(&self, key: &str) -> bool {
        self.root.services.read().map(|s| s.contains_key(key)).unwrap_or(false)
    }

    /// Mounts a plugin in its own scope: applied now when its services exist, else pending.
    pub fn plugin(&self, p: Arc<dyn Plugin>) {
        let name = p.name();
        let scope = Arc::new(Scope { name: name.clone(), effects: Mutex::new(vec![]) });
        if let Ok(mut entries) = self.root.entries.lock() {
            entries.push(Entry { plugin: p, scope, status: PluginStatus::Pending(vec![]), provides: vec![] });
        }
        self.retry_pending();
    }

    /// Lists the mounted plugins with their state (the `--dump-config` view).
    pub fn plugins(&self) -> Vec<(String, PluginStatus, String)> {
        self.root.entries.lock().map(|e| e.iter().map(|e| (e.plugin.name(), e.status.clone(), e.plugin.describe())).collect()).unwrap_or_default()
    }

    /// Unloads a plugin by name: its effects unwind in reverse order.
    pub fn unload(&self, name: &str) {
        let scope = self.root.entries.lock().ok().and_then(|mut entries| {
            let i = entries.iter().position(|e| e.plugin.name() == name)?;
            Some(entries.remove(i).scope)
        });
        if let Some(s) = scope {
            s.dispose();
        }
    }

    /// Unloads every plugin, newest first.
    pub fn dispose(&self) {
        let scopes: Vec<Arc<Scope>> = self.root.entries.lock().map(|mut e| e.drain(..).map(|e| e.scope).collect()).unwrap_or_default();
        for s in scopes.into_iter().rev() {
            s.dispose();
        }
        self.scope.dispose();
    }

    /// Applies every pending plugin whose services now exist (repeats until stable).
    fn retry_pending(&self) {
        loop {
            // pick one ready plugin at a time: applying it may provide services or mount more
            let ready = {
                let Ok(mut entries) = self.root.entries.lock() else { return };
                let services: Vec<&'static str> = self.root.services.read().map(|s| s.keys().copied().collect()).unwrap_or_default();
                let mut pick = None;
                for e in entries.iter_mut() {
                    if !matches!(e.status, PluginStatus::Pending(_)) {
                        continue;
                    }
                    let missing: Vec<String> = e.plugin.inject().into_iter().filter(|k| !services.contains(k)).map(String::from).collect();
                    if missing.is_empty() {
                        e.status = PluginStatus::Active;
                        pick = Some((e.plugin.clone(), e.scope.clone()));
                        break;
                    }
                    e.status = PluginStatus::Pending(missing);
                }
                pick
            };
            let Some((plugin, scope)) = ready else { return };
            let view = Context { root: self.root.clone(), scope: scope.clone() };
            if let Err(err) = plugin.apply(&view) {
                scope.dispose();
                if let Ok(mut entries) = self.root.entries.lock() {
                    if let Some(e) = entries.iter_mut().find(|e| Arc::ptr_eq(&e.scope, &scope)) {
                        e.status = PluginStatus::Failed(err);
                    }
                }
            }
        }
    }

    /// A service went away: plugins that inject it unload and wait again.
    fn cascade(root: &Arc<Root>, key: &'static str) {
        let hit: Vec<Arc<Scope>> = root
            .entries
            .lock()
            .map(|mut entries| {
                entries
                    .iter_mut()
                    .filter(|e| e.status == PluginStatus::Active && e.plugin.inject().contains(&key))
                    .map(|e| {
                        e.status = PluginStatus::Pending(vec![key.to_string()]);
                        e.provides.clear();
                        e.scope.clone()
                    })
                    .collect()
            })
            .unwrap_or_default();
        for s in hit {
            s.dispose();
        }
    }
}

/// A plugin made from a closure, for small compositions and tests.
pub struct FnPlugin<F: Fn(&Context) -> Result<(), String> + Send + Sync> {
    pub name: String,
    pub inject: Vec<&'static str>,
    pub apply: F,
}

impl<F: Fn(&Context) -> Result<(), String> + Send + Sync> Plugin for FnPlugin<F> {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn inject(&self) -> Vec<&'static str> {
        self.inject.clone()
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        (self.apply)(ctx)
    }
}

pub fn fn_plugin(name: &str, inject: Vec<&'static str>, apply: impl Fn(&Context) -> Result<(), String> + Send + Sync + 'static) -> Arc<dyn Plugin> {
    Arc::new(FnPlugin { name: name.into(), inject, apply })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn plugins_wait_for_services_and_unwind() {
        let ctx = Context::new();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l = log.clone();
        ctx.plugin(fn_plugin("user", vec!["counter"], move |c| {
            let n = c.expect::<AtomicUsize>("counter")?;
            n.fetch_add(1, Ordering::Relaxed);
            let l2 = l.clone();
            c.effect(Box::new(move || l2.lock().unwrap().push("user down".into())));
            Ok(())
        }));
        assert!(matches!(ctx.plugins()[0].1, PluginStatus::Pending(_)));
        ctx.plugin(fn_plugin("provider", vec![], |c| c.provide("counter", Arc::new(AtomicUsize::new(0)))));
        assert_eq!(ctx.plugins()[0].1, PluginStatus::Active);
        assert_eq!(ctx.expect::<AtomicUsize>("counter").unwrap().load(Ordering::Relaxed), 1);
        // the provider goes: its dependant unloads and waits again
        ctx.unload("provider");
        assert!(!ctx.has("counter"));
        assert_eq!(log.lock().unwrap().as_slice(), ["user down"]);
        assert!(matches!(ctx.plugins()[0].1, PluginStatus::Pending(_)));
        // a new provider brings it back
        ctx.plugin(fn_plugin("provider2", vec![], |c| c.provide("counter", Arc::new(AtomicUsize::new(10)))));
        assert_eq!(ctx.expect::<AtomicUsize>("counter").unwrap().load(Ordering::Relaxed), 11);
    }

    #[test]
    fn failed_apply_rolls_back() {
        let ctx = Context::new();
        ctx.plugin(fn_plugin("bad", vec![], |c| {
            c.provide("x", Arc::new(1u8))?;
            Err("boom".into())
        }));
        assert!(!ctx.has("x"));
        assert_eq!(ctx.plugins()[0].1, PluginStatus::Failed("boom".into()));
    }
}
