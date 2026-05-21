#![forbid(unsafe_code)]
#![doc = "Hook registry for the Aman agent framework."]

use kernel::context::HookContext;
use kernel::error::AmanResult;
use kernel::hook::{Hook, HookPoint};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Thread-safe registry for plugin-provided hooks.
///
/// Hooks are stored by name. Each hook can register for multiple [`HookPoint`]s.
/// When executing, hooks are sorted by priority (high to low).
pub struct HookRegistry {
    hooks: RwLock<HashMap<String, Arc<dyn Hook>>>,
}

impl HookRegistry {
    /// Create an empty hook registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
        }
    }

    /// Register a hook. Returns an error if a hook with the same name already exists.
    pub fn register(&self, hook: Arc<dyn Hook>) -> AmanResult<()> {
        let mut hooks = self.hooks.write().expect("hook registry write lock");
        let name = hook.name().to_owned();
        if hooks.contains_key(&name) {
            return Err(kernel::Error::AlreadyExists {
                name: format!("hook:{name}"),
            });
        }
        hooks.insert(name, hook);
        Ok(())
    }

    /// Unregister a hook by name. Returns true if the hook was removed.
    pub fn unregister(&self, hook_name: &str) -> bool {
        let mut hooks = self.hooks.write().expect("hook registry write lock");
        hooks.remove(hook_name).is_some()
    }

    /// Return all hooks that fire at the given hook point, sorted by priority (descending).
    pub fn hooks_at(&self, point: HookPoint) -> Vec<Arc<dyn Hook>> {
        let hooks = self.hooks.read().expect("hook registry read lock");
        let mut matched: Vec<Arc<dyn Hook>> = hooks
            .values()
            .filter(|h| h.hook_points().contains(&point))
            .cloned()
            .collect();
        matched.sort_by(|a, b| b.priority().cmp(&a.priority()));
        matched
    }

    /// Execute all hooks registered for the given hook point in priority order.
    pub async fn execute(&self, point: HookPoint, ctx: HookContext) -> AmanResult<()> {
        let hooks = self.hooks_at(point);
        for hook in hooks {
            hook.execute(point, ctx.clone()).await?;
        }
        Ok(())
    }

    /// Return the number of registered hooks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hooks.read().expect("hook registry read lock").len()
    }

    /// Return true if no hooks are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return all registered hook names, sorted.
    #[must_use]
    pub fn hook_names(&self) -> Vec<String> {
        let hooks = self.hooks.read().expect("hook registry read lock");
        let mut names: Vec<String> = hooks.keys().cloned().collect();
        names.sort();
        names
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::*;
    use kernel::hook::Hook;
    use async_trait::async_trait;

    struct TestHook {
        name: String,
        priority: i32,
        points: Vec<HookPoint>,
    }

    #[async_trait]
    impl Hook for TestHook {
        fn name(&self) -> &str { &self.name }
        fn priority(&self) -> i32 { self.priority }
        fn hook_points(&self) -> &[HookPoint] { &self.points }
        async fn execute(&self, _point: HookPoint, _ctx: HookContext) -> AmanResult<()> {
            Ok(())
        }
    }

    #[test]
    fn test_register_and_unregister() {
        let registry = HookRegistry::new();
        let hook = Arc::new(TestHook {
            name: "test-hook".to_owned(),
            priority: 0,
            points: vec![HookPoint::AgentReady],
        });
        assert!(registry.register(hook.clone()).is_ok());
        assert_eq!(registry.len(), 1);

        let duplicate = registry.register(hook.clone());
        assert!(duplicate.is_err());

        assert!(registry.unregister("test-hook"));
        assert!(registry.is_empty());
    }

    #[test]
    fn test_hooks_at_returns_priority_sorted() {
        let registry = HookRegistry::new();
        let low = Arc::new(TestHook {
            name: "low-prio".to_owned(),
            priority: -10,
            points: vec![HookPoint::AgentReady],
        });
        let high = Arc::new(TestHook {
            name: "high-prio".to_owned(),
            priority: 100,
            points: vec![HookPoint::AgentReady],
        });
        registry.register(high.clone()).unwrap();
        registry.register(low.clone()).unwrap();

        let hooks = registry.hooks_at(HookPoint::AgentReady);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name(), "high-prio");
        assert_eq!(hooks[1].name(), "low-prio");
    }

    #[test]
    fn test_hooks_at_filters_by_point() {
        let registry = HookRegistry::new();
        let for_ready = Arc::new(TestHook {
            name: "ready-hook".to_owned(),
            priority: 0,
            points: vec![HookPoint::AgentReady],
        });
        let for_shutdown = Arc::new(TestHook {
            name: "shutdown-hook".to_owned(),
            priority: 0,
            points: vec![HookPoint::AgentShuttingDown],
        });
        registry.register(for_ready).unwrap();
        registry.register(for_shutdown).unwrap();

        assert_eq!(registry.hooks_at(HookPoint::AgentReady).len(), 1);
        assert_eq!(registry.hooks_at(HookPoint::AgentShuttingDown).len(), 1);
        assert_eq!(registry.hooks_at(HookPoint::ToolExecuting).len(), 0);
    }

    #[test]
    fn test_hook_names() {
        let registry = HookRegistry::new();
        let a = Arc::new(TestHook {
            name: "a-hook".to_owned(),
            priority: 0,
            points: vec![HookPoint::AgentReady],
        });
        let b = Arc::new(TestHook {
            name: "b-hook".to_owned(),
            priority: 0,
            points: vec![HookPoint::AgentReady],
        });
        registry.register(b).unwrap();
        registry.register(a).unwrap();

        assert_eq!(registry.hook_names(), vec!["a-hook", "b-hook"]);
    }
}
