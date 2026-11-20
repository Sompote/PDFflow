//! Typed hook points with the Cordis dispatch modes that DeepSeek Harness uses.
//!
//! - [`Emit`]: observers, called in registration order, no result.
//! - [`Waterfall`]: around-middleware. A listener gets the argument and `next`; it calls
//!   `next` to delegate (possibly after changing the argument) or returns without it to
//!   short-circuit. The innermost `next` is the default behaviour supplied at dispatch.
//! - [`Bail`]: listeners in order until one returns `Some`.
//!
//! Every registration returns a [`Disposer`]; plugins record it as an effect on their
//! context so unloading the plugin removes the listener.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Undoes one registration.
pub type Disposer = Box<dyn FnOnce() + Send + Sync>;

struct Slots<F: ?Sized> {
    list: Arc<RwLock<Vec<(u64, Arc<F>)>>>,
}

impl<F: ?Sized + Send + Sync + 'static> Slots<F> {
    fn new() -> Self {
        Slots { list: Arc::new(RwLock::new(vec![])) }
    }
    fn add(&self, f: Arc<F>, prepend: bool) -> Disposer {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut l) = self.list.write() {
            if prepend {
                l.insert(0, (id, f));
            } else {
                l.push((id, f));
            }
        }
        let list = self.list.clone();
        Box::new(move || {
            if let Ok(mut l) = list.write() {
                l.retain(|(i, _)| *i != id);
            }
        })
    }
    fn snapshot(&self) -> Vec<Arc<F>> {
        self.list.read().map(|l| l.iter().map(|(_, f)| f.clone()).collect()).unwrap_or_default()
    }
    fn len(&self) -> usize {
        self.list.read().map(|l| l.len()).unwrap_or(0)
    }
}

type EmitFn<A> = dyn Fn(&A) + Send + Sync;

/// Observers of an event (`emit`).
pub struct Emit<A: 'static> {
    pub name: &'static str,
    slots: Slots<EmitFn<A>>,
}

impl<A: 'static> Emit<A> {
    pub fn new(name: &'static str) -> Self {
        Emit { name, slots: Slots::new() }
    }
    pub fn on(&self, f: impl Fn(&A) + Send + Sync + 'static) -> Disposer {
        self.slots.add(Arc::new(f), false)
    }
    pub fn emit(&self, a: &A) {
        for f in self.slots.snapshot() {
            f(a);
        }
    }
    pub fn listeners(&self) -> usize {
        self.slots.len()
    }
}

type WaterfallFn<A, R> = dyn Fn(&mut A, &dyn Fn(&mut A) -> R) -> R + Send + Sync;

/// Around-middleware (`waterfall`).
pub struct Waterfall<A: 'static, R: 'static> {
    pub name: &'static str,
    slots: Slots<WaterfallFn<A, R>>,
}

impl<A: 'static, R: 'static> Waterfall<A, R> {
    pub fn new(name: &'static str) -> Self {
        Waterfall { name, slots: Slots::new() }
    }
    /// Registers a listener; `prepend` puts it before the ordinary ones (outermost).
    pub fn on(&self, prepend: bool, f: impl Fn(&mut A, &dyn Fn(&mut A) -> R) -> R + Send + Sync + 'static) -> Disposer {
        self.slots.add(Arc::new(f), prepend)
    }
    /// Runs the chain with `base` as the innermost behaviour.
    pub fn dispatch(&self, a: &mut A, base: &dyn Fn(&mut A) -> R) -> R {
        let chain = self.slots.snapshot();
        fn go<A, R>(chain: &[Arc<WaterfallFn<A, R>>], a: &mut A, base: &dyn Fn(&mut A) -> R) -> R {
            match chain.split_first() {
                None => base(a),
                Some((f, rest)) => f(a, &|a: &mut A| go(rest, a, base)),
            }
        }
        go(&chain, a, base)
    }
    pub fn listeners(&self) -> usize {
        self.slots.len()
    }
}

type BailFn<A, R> = dyn Fn(&A) -> Option<R> + Send + Sync;

/// Listeners in order until one answers (`bail`).
pub struct Bail<A: 'static, R: 'static> {
    pub name: &'static str,
    slots: Slots<BailFn<A, R>>,
}

impl<A: 'static, R: 'static> Bail<A, R> {
    pub fn new(name: &'static str) -> Self {
        Bail { name, slots: Slots::new() }
    }
    pub fn on(&self, f: impl Fn(&A) -> Option<R> + Send + Sync + 'static) -> Disposer {
        self.slots.add(Arc::new(f), false)
    }
    pub fn bail(&self, a: &A) -> Option<R> {
        self.slots.snapshot().into_iter().find_map(|f| f(a))
    }
    pub fn listeners(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waterfall_wraps_in_order_and_can_short_circuit() {
        let w: Waterfall<Vec<&'static str>, usize> = Waterfall::new("t");
        let _a = w.on(false, |v, next| {
            v.push("a");
            next(v) + 1
        });
        let d = w.on(false, |v, next| {
            v.push("b");
            next(v) * 10
        });
        let mut v = vec![];
        assert_eq!(w.dispatch(&mut v, &|v| v.len()), 21);
        assert_eq!(v, ["a", "b"]);
        d();
        let _p = w.on(true, |_, _| 99);
        assert_eq!(w.dispatch(&mut vec![], &|_| 0), 99);
    }

    #[test]
    fn bail_stops_at_first_answer() {
        let b: Bail<i32, &'static str> = Bail::new("t");
        let _1 = b.on(|x| (*x > 5).then_some("big"));
        let _2 = b.on(|_| Some("any"));
        assert_eq!(b.bail(&9), Some("big"));
        assert_eq!(b.bail(&1), Some("any"));
    }
}
