// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::{AmanResult, Error};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationMode {
    Namespace,
    Physical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteConsistency {
    LastWriteWins,
    OptimisticLock,
    PessimisticLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Retain,
    DeleteOnDisable,
    DeleteOnUninstall,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateRecord {
    pub value: Value,
    pub version: u64,
}

pub trait StateStore: Send + Sync {
    fn get(&self, namespace: &str, key: &str) -> AmanResult<Option<StateRecord>>;
    fn put(&self, namespace: &str, key: &str, value: Value) -> AmanResult<StateRecord>;
    fn put_cas(
        &self,
        namespace: &str,
        key: &str,
        value: Value,
        expected_version: u64,
    ) -> AmanResult<StateRecord>;
    fn delete(&self, namespace: &str, key: &str) -> AmanResult<()>;
    fn scan(&self, namespace: &str, prefix: Option<&str>) -> AmanResult<Vec<(String, StateRecord)>>;
    fn read_committed(&self, namespace: &str, key: &str) -> AmanResult<Option<StateRecord>>;
    fn lock(&self, namespace: &str, key: &str, owner: &str) -> AmanResult<()>;
    fn unlock(&self, namespace: &str, key: &str, owner: &str) -> AmanResult<()>;
    fn isolation_mode(&self) -> IsolationMode;
    fn write_consistency(&self) -> WriteConsistency;
    fn cleanup_policy(&self) -> CleanupPolicy;
    fn allow_shared_namespace(&self, namespace: &str) -> AmanResult<()>;
    fn is_shared_namespace(&self, namespace: &str) -> bool;
}

#[derive(Debug)]
pub struct SledStore {
    data: Mutex<StoreData>,
    locks: Mutex<BTreeMap<String, String>>,
    shared_namespaces: Mutex<HashSet<String>>,
    isolation_mode: IsolationMode,
    write_consistency: WriteConsistency,
    cleanup_policy: CleanupPolicy,
}

#[derive(Debug, Default)]
struct StoreData {
    namespaced: BTreeMap<String, StateRecord>,
    physical: BTreeMap<String, BTreeMap<String, StateRecord>>,
}

impl Default for SledStore {
    fn default() -> Self {
        Self::new(IsolationMode::Namespace, WriteConsistency::OptimisticLock)
    }
}

impl SledStore {
    #[must_use]
    pub fn new(isolation_mode: IsolationMode, write_consistency: WriteConsistency) -> Self {
        Self::with_policy(isolation_mode, write_consistency, CleanupPolicy::Retain)
    }

    #[must_use]
    pub fn with_policy(
        isolation_mode: IsolationMode,
        write_consistency: WriteConsistency,
        cleanup_policy: CleanupPolicy,
    ) -> Self {
        Self {
            data: Mutex::new(StoreData::default()),
            locks: Mutex::new(BTreeMap::new()),
            shared_namespaces: Mutex::new(HashSet::new()),
            isolation_mode,
            write_consistency,
            cleanup_policy,
        }
    }
}

impl StateStore for SledStore {
    fn get(&self, namespace: &str, key: &str) -> AmanResult<Option<StateRecord>> {
        let namespace = normalize_token(namespace, "namespace")?;
        let key = normalize_token(key, "key")?;
        let data = self
            .data
            .lock()
            .expect("state store mutex should not be poisoned");
        let value = match self.isolation_mode {
            IsolationMode::Namespace => data.namespaced.get(&format!("{namespace}:{key}")).cloned(),
            IsolationMode::Physical => data
                .physical
                .get(&namespace)
                .and_then(|bucket| bucket.get(&key))
                .cloned(),
        };
        Ok(value)
    }

    fn put(&self, namespace: &str, key: &str, value: Value) -> AmanResult<StateRecord> {
        let namespace = normalize_token(namespace, "namespace")?;
        let key = normalize_token(key, "key")?;
        let mut data = self
            .data
            .lock()
            .expect("state store mutex should not be poisoned");
        let next_version = match self.isolation_mode {
            IsolationMode::Namespace => data
                .namespaced
                .get(&format!("{namespace}:{key}"))
                .map_or(1, |record| record.version.saturating_add(1)),
            IsolationMode::Physical => data
                .physical
                .get(&namespace)
                .and_then(|bucket| bucket.get(&key))
                .map_or(1, |record| record.version.saturating_add(1)),
        };
        let record = StateRecord {
            value,
            version: next_version,
        };
        match self.isolation_mode {
            IsolationMode::Namespace => {
                data.namespaced.insert(format!("{namespace}:{key}"), record.clone());
            }
            IsolationMode::Physical => {
                data.physical
                    .entry(namespace)
                    .or_default()
                    .insert(key, record.clone());
            }
        }
        Ok(record)
    }

    fn put_cas(
        &self,
        namespace: &str,
        key: &str,
        value: Value,
        expected_version: u64,
    ) -> AmanResult<StateRecord> {
        let namespace = normalize_token(namespace, "namespace")?;
        let key = normalize_token(key, "key")?;
        let mut data = self
            .data
            .lock()
            .expect("state store mutex should not be poisoned");
        let current_version = match self.isolation_mode {
            IsolationMode::Namespace => data
                .namespaced
                .get(&format!("{namespace}:{key}"))
                .map_or(0, |record| record.version),
            IsolationMode::Physical => data
                .physical
                .get(&namespace)
                .and_then(|bucket| bucket.get(&key))
                .map_or(0, |record| record.version),
        };
        if current_version != expected_version {
            return Err(Error::VersionMismatch {
                expected: expected_version.to_string(),
                found: current_version.to_string(),
            });
        }
        let next = StateRecord {
            value,
            version: current_version.saturating_add(1),
        };
        match self.isolation_mode {
            IsolationMode::Namespace => {
                data.namespaced.insert(format!("{namespace}:{key}"), next.clone());
            }
            IsolationMode::Physical => {
                data.physical
                    .entry(namespace)
                    .or_default()
                    .insert(key, next.clone());
            }
        }
        Ok(next)
    }

    fn delete(&self, namespace: &str, key: &str) -> AmanResult<()> {
        let namespace = normalize_token(namespace, "namespace")?;
        let key = normalize_token(key, "key")?;
        let mut data = self
            .data
            .lock()
            .expect("state store mutex should not be poisoned")
            ;
        match self.isolation_mode {
            IsolationMode::Namespace => {
                data.namespaced.remove(&format!("{namespace}:{key}"));
            }
            IsolationMode::Physical => {
                if let Some(bucket) = data.physical.get_mut(&namespace) {
                    bucket.remove(&key);
                    if bucket.is_empty() {
                        data.physical.remove(&namespace);
                    }
                }
            }
        }
        Ok(())
    }

    fn scan(&self, namespace: &str, prefix: Option<&str>) -> AmanResult<Vec<(String, StateRecord)>> {
        let namespace = normalize_token(namespace, "namespace")?;
        let prefix = prefix
            .map(|value| normalize_token(value, "prefix"))
            .transpose()?
            .unwrap_or_default();
        let data = self
            .data
            .lock()
            .expect("state store mutex should not be poisoned");
        let items = match self.isolation_mode {
            IsolationMode::Namespace => {
                let key_prefix = format!("{namespace}:{prefix}");
                data.namespaced
                    .iter()
                    .filter(|(key, _)| key.starts_with(&key_prefix))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            }
            IsolationMode::Physical => {
                let mut items = Vec::new();
                if let Some(bucket) = data.physical.get(&namespace) {
                    for (key, value) in bucket {
                        if key.starts_with(&prefix) {
                            items.push((format!("{namespace}:{key}"), value.clone()));
                        }
                    }
                }
                items
            }
        };
        Ok(items)
    }

    fn read_committed(&self, namespace: &str, key: &str) -> AmanResult<Option<StateRecord>> {
        self.get(namespace, key)
    }

    fn lock(&self, namespace: &str, key: &str, owner: &str) -> AmanResult<()> {
        let namespaced = self.lock_key(namespace, key)?;
        let owner = normalize_token(owner, "owner")?;
        let mut locks = self
            .locks
            .lock()
            .expect("state lock mutex should not be poisoned");
        if let Some(current_owner) = locks.get(&namespaced) {
            if current_owner != &owner {
                return Err(Error::Unrecoverable {
                    message: format!("state lock for `{namespaced}` is held by `{current_owner}`"),
                });
            }
            return Ok(());
        }
        locks.insert(namespaced, owner);
        Ok(())
    }

    fn unlock(&self, namespace: &str, key: &str, owner: &str) -> AmanResult<()> {
        let namespaced = self.lock_key(namespace, key)?;
        let owner = normalize_token(owner, "owner")?;
        let mut locks = self
            .locks
            .lock()
            .expect("state lock mutex should not be poisoned");
        if let Some(current_owner) = locks.get(&namespaced) {
            if current_owner != &owner {
                return Err(Error::PermissionDenied {
                    message: format!("state lock for `{namespaced}` is owned by `{current_owner}`"),
                });
            }
            locks.remove(&namespaced);
            return Ok(());
        }
        Err(Error::NotFound {
            name: format!("state_lock:{namespaced}"),
        })
    }

    fn isolation_mode(&self) -> IsolationMode {
        self.isolation_mode
    }

    fn write_consistency(&self) -> WriteConsistency {
        self.write_consistency
    }

    fn cleanup_policy(&self) -> CleanupPolicy {
        self.cleanup_policy
    }

    fn allow_shared_namespace(&self, namespace: &str) -> AmanResult<()> {
        let namespace = normalize_token(namespace, "namespace")?;
        self.shared_namespaces
            .lock()
            .expect("shared namespace mutex should not be poisoned")
            .insert(namespace);
        Ok(())
    }

    fn is_shared_namespace(&self, namespace: &str) -> bool {
        let namespace = match normalize_token(namespace, "namespace") {
            Ok(namespace) => namespace,
            Err(_) => return false,
        };
        self.shared_namespaces
            .lock()
            .expect("shared namespace mutex should not be poisoned")
            .contains(&namespace)
    }
}

impl SledStore {
    fn lock_key(&self, namespace: &str, key: &str) -> AmanResult<String> {
        let namespace = normalize_token(namespace, "namespace")?;
        let key = normalize_token(key, "key")?;
        Ok(match self.isolation_mode {
            IsolationMode::Namespace => format!("{namespace}:{key}"),
            IsolationMode::Physical => format!("physical:{namespace}:{key}"),
        })
    }
}

fn normalize_token(value: &str, field: &str) -> AmanResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::ConfigInvalid {
            message: format!("state store {field} cannot be empty"),
        });
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{CleanupPolicy, IsolationMode, SledStore, StateStore, WriteConsistency};
    use kernel::Error;
    use serde_json::json;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn put_get_and_delete_roundtrip() {
        let store = SledStore::default();
        let inserted = store
            .put("workflow", "instance-1", json!({"state": "pending"}))
            .expect("put should succeed");
        assert_eq!(inserted.version, 1);

        let fetched = store
            .get("workflow", "instance-1")
            .expect("get should succeed")
            .expect("record should exist");
        assert_eq!(fetched.value["state"], "pending");

        store
            .delete("workflow", "instance-1")
            .expect("delete should succeed");
        assert!(
            store
                .get("workflow", "instance-1")
                .expect("get should succeed")
                .is_none()
        );
    }

    #[test]
    fn cas_rejects_version_conflict() {
        let store = SledStore::default();
        let _ = store
            .put("workflow", "instance-1", json!({"state": "pending"}))
            .expect("put should succeed");
        let error = store
            .put_cas("workflow", "instance-1", json!({"state": "reviewing"}), 0)
            .expect_err("cas should fail on stale version");
        assert!(matches!(error, Error::VersionMismatch { .. }));
    }

    #[test]
    fn scan_filters_by_namespace_and_prefix() {
        let store = SledStore::new(IsolationMode::Namespace, WriteConsistency::OptimisticLock);
        let _ = store.put("workflow", "a-1", json!({"v": 1}));
        let _ = store.put("workflow", "a-2", json!({"v": 2}));
        let _ = store.put("workflow", "b-1", json!({"v": 3}));
        let _ = store.put("plugin", "a-1", json!({"v": 4}));

        let scanned = store
            .scan("workflow", Some("a-"))
            .expect("scan should succeed");
        assert_eq!(scanned.len(), 2);
        assert!(scanned.iter().all(|(key, _)| key.starts_with("workflow:a-")));
    }

    #[test]
    fn pessimistic_lock_owner_controls_unlock() {
        let store = SledStore::default();
        store
            .lock("workflow", "instance-1", "worker-a")
            .expect("first lock should succeed");
        let locked = store.lock("workflow", "instance-1", "worker-b");
        assert!(locked.is_err());
        let wrong_unlock = store.unlock("workflow", "instance-1", "worker-b");
        assert!(matches!(wrong_unlock, Err(Error::PermissionDenied { .. })));
        store
            .unlock("workflow", "instance-1", "worker-a")
            .expect("owner unlock should succeed");
    }

    #[test]
    fn cleanup_policy_and_shared_namespace_flags_are_kept() {
        let store = SledStore::with_policy(
            IsolationMode::Physical,
            WriteConsistency::LastWriteWins,
            CleanupPolicy::DeleteOnDisable,
        );
        assert_eq!(store.cleanup_policy(), CleanupPolicy::DeleteOnDisable);
        assert!(!store.is_shared_namespace("shared"));
        store
            .allow_shared_namespace("shared")
            .expect("allow shared namespace");
        assert!(store.is_shared_namespace("shared"));
    }

    #[test]
    fn physical_isolation_keeps_namespace_buckets_separate() {
        let store = SledStore::new(IsolationMode::Physical, WriteConsistency::OptimisticLock);
        let _ = store.put("ns-a", "same-key", json!({"v": 1})).expect("put a");
        let _ = store.put("ns-b", "same-key", json!({"v": 2})).expect("put b");

        let a = store
            .get("ns-a", "same-key")
            .expect("get a")
            .expect("a exists");
        let b = store
            .get("ns-b", "same-key")
            .expect("get b")
            .expect("b exists");
        assert_eq!(a.value["v"], 1);
        assert_eq!(b.value["v"], 2);

        let scanned_a = store.scan("ns-a", Some("same")).expect("scan ns-a");
        assert_eq!(scanned_a.len(), 1);
        assert_eq!(scanned_a[0].0, "ns-a:same-key");
    }

    #[test]
    fn cas_competition_only_one_writer_succeeds() {
        let store = Arc::new(SledStore::default());
        let _ = store
            .put("workflow", "instance-1", json!({"state": "pending"}))
            .expect("seed record");

        let mut handles = Vec::new();
        for suffix in ["a", "b"] {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store.put_cas(
                    "workflow",
                    "instance-1",
                    json!({"state": format!("reviewing-{suffix}")}),
                    1,
                )
            }));
        }

        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("join thread"))
            .collect::<Vec<_>>();
        let success_count = outcomes.iter().filter(|result| result.is_ok()).count();
        let failure_count = outcomes
            .iter()
            .filter(|result| matches!(result, Err(Error::VersionMismatch { .. })))
            .count();
        assert_eq!(success_count, 1);
        assert_eq!(failure_count, 1);
    }
}
