#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use std::collections::VecDeque;
use std::sync::Mutex;

use crate::model::{Notification, Severity};

/// In-memory ring-buffer store for notifications.
///
/// Default capacity: 500 entries. Oldest entries are evicted when the buffer is full.
pub struct NotificationStore {
    inner: Mutex<Inner>,
}

struct Inner {
    entries: VecDeque<Notification>,
    cap: usize,
}

impl NotificationStore {
    /// Create a new store with the given capacity.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                cap: cap.max(1),
            }),
        }
    }

    /// Push a new notification. Evicts oldest if at capacity.
    pub fn push(&self, notif: Notification) {
        let mut inner = self.inner.lock().expect("notification store lock");
        inner.entries.push_back(notif);
        while inner.entries.len() > inner.cap {
            inner.entries.pop_front();
        }
    }

    /// Mark a single notification as dismissed.
    /// Returns `true` if the notification was found and was dismissible.
    pub fn dismiss(&self, id: &str) -> bool {
        let mut inner = self.inner.lock().expect("notification store lock");
        if let Some(n) = inner.entries.iter_mut().find(|n| n.id == id) {
            if n.dismissible {
                n.dismissed = true;
                return true;
            }
        }
        false
    }

    /// Mark a single notification as acknowledged (for critical / non-dismissible).
    /// Returns `true` if the notification was found.
    pub fn acknowledge(&self, id: &str) -> bool {
        let mut inner = self.inner.lock().expect("notification store lock");
        if let Some(n) = inner.entries.iter_mut().find(|n| n.id == id) {
            n.dismissed = true;
            return true;
        }
        false
    }

    /// Mark all dismissible notifications as dismissed.
    pub fn dismiss_all(&self) {
        let mut inner = self.inner.lock().expect("notification store lock");
        for n in inner.entries.iter_mut() {
            if n.dismissible {
                n.dismissed = true;
            }
        }
    }

    /// List notifications with optional filtering.
    #[must_use]
    pub fn list(
        &self,
        active_only: bool,
        severity: Option<Severity>,
        limit: usize,
        offset: usize,
    ) -> Vec<Notification> {
        let inner = self.inner.lock().expect("notification store lock");
        inner
            .entries
            .iter()
            .filter(|n| !active_only || n.is_active())
            .filter(|n| severity.is_none_or(|s| n.severity == s))
            .rev()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Count un-dismissed / unacknowledged notifications.
    #[must_use]
    pub fn unread_count(&self) -> usize {
        let inner = self.inner.lock().expect("notification store lock");
        inner.entries.iter().filter(|n| n.is_active()).count()
    }
}
