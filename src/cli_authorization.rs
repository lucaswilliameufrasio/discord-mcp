//! Building block for a future browser-authorized administrative CLI.
//!
//! This module does not expose routes or authenticate an administrator. The
//! caller must verify the Discord identity and obtain explicit browser consent
//! before approving a request. It must never expose the verifier to the browser.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const MAX_PENDING: usize = 1024;
const LIFETIME: Duration = Duration::from_secs(300);

/// An exact, immutable operation approved by the administrator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionChange {
    pub user_id: String,
    pub channel: String,
    pub read: bool,
    pub send: bool,
}

struct Entry {
    challenge: [u8; 32],
    operation: PermissionChange,
    expires: Instant,
    approved: bool,
}

/// In-memory, bounded and restart-invalidated requests; not an authentication provider.
#[derive(Default)]
pub struct CliAuthorizations {
    entries: Mutex<HashMap<String, Entry>>,
}

impl CliAuthorizations {
    #[cfg(test)]
    pub(crate) fn expire_for_test(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap();
        entries.get_mut(id).unwrap().expires = Instant::now();
    }
    /// The challenge is SHA-256 of a high-entropy verifier held only by the CLI.
    pub fn begin(
        &self,
        operation: PermissionChange,
        challenge: [u8; 32],
        now: Instant,
    ) -> Result<String, &'static str> {
        if operation.user_id.is_empty()
            || operation.user_id.len() > 20
            || !operation.user_id.bytes().all(|b| b.is_ascii_digit())
            || operation.channel.is_empty()
            || operation.channel.len() > 64
            || !operation
                .channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid operation");
        }
        let mut entries = self.entries.lock().map_err(|_| "unavailable")?;
        entries.retain(|_, entry| entry.expires > now);
        if entries.len() >= MAX_PENDING {
            return Err("capacity");
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| "unavailable")?;
        let id = hex::encode(random);
        if entries.contains_key(&id) {
            return Err("unavailable");
        }
        let expires = now.checked_add(LIFETIME).ok_or("unavailable")?;
        entries.insert(
            id.clone(),
            Entry {
                challenge,
                operation,
                expires,
                approved: false,
            },
        );
        Ok(id)
    }

    /// Return the precise pending operation to show on the consent screen.
    pub fn pending(&self, id: &str, now: Instant) -> Option<PermissionChange> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(id)?;
        if entry.expires <= now || entry.approved {
            return None;
        }
        Some(entry.operation.clone())
    }

    /// Only call after a fresh authenticated administrator has consented to this exact operation.
    pub fn approve(&self, id: &str, now: Instant) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let Some(entry) = entries.get_mut(id) else {
            return false;
        };
        if entry.expires <= now || entry.approved {
            return false;
        }
        entry.approved = true;
        true
    }

    /// Atomically consume an approved request; failures never return an operation.
    pub fn consume(&self, id: &str, verifier: &[u8], now: Instant) -> Option<PermissionChange> {
        if verifier.len() != 32 {
            return None;
        }
        let hash = Sha256::digest(verifier);
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get(id)?;
        if entry.expires <= now
            || !entry.approved
            || !bool::from(entry.challenge.as_slice().ct_eq(hash.as_slice()))
        {
            return None;
        }
        entries.remove(id).map(|entry| entry.operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn operation() -> PermissionChange {
        PermissionChange {
            user_id: "101".into(),
            channel: "alerts".into(),
            read: true,
            send: false,
        }
    }

    #[test]
    fn approval_verifier_expiry_and_replay_are_enforced() {
        let registry = CliAuthorizations::default();
        let now = Instant::now();
        let verifier = [7; 32];
        let challenge = Sha256::digest(verifier).into();
        let id = registry.begin(operation(), challenge, now).unwrap();
        assert_eq!(registry.pending(&id, now), Some(operation()));
        assert!(registry.consume(&id, &verifier, now).is_none());
        assert!(registry.approve(&id, now));
        assert_eq!(registry.pending(&id, now), None);
        assert!(!registry.approve(&id, now));
        assert!(registry.consume(&id, &[8; 32], now).is_none());
        assert_eq!(registry.consume(&id, &verifier, now), Some(operation()));
        assert!(registry.consume(&id, &verifier, now).is_none());
        let expired = registry.begin(operation(), challenge, now).unwrap();
        assert!(!registry.approve(&expired, now + LIFETIME));
        assert!(
            registry
                .consume(&expired, &verifier, now + LIFETIME)
                .is_none()
        );
        let approved = registry.begin(operation(), challenge, now).unwrap();
        assert!(registry.approve(&approved, now));
        registry.expire_for_test(&approved);
        assert!(
            registry
                .consume(&approved, &verifier, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn concurrent_consumers_have_one_winner() {
        let registry = Arc::new(CliAuthorizations::default());
        let now = Instant::now();
        let verifier = [9; 32];
        let id = registry
            .begin(operation(), Sha256::digest(verifier).into(), now)
            .unwrap();
        assert!(registry.approve(&id, now));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let id = id.clone();
            workers.push(std::thread::spawn(move || {
                registry.consume(&id, &verifier, now).is_some()
            }));
        }
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }
}
