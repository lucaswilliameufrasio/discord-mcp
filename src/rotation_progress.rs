//! In-memory rotation evidence model. This does not resolve or store secrets.
//!
//! Callers must independently authenticate evidence before operational use;
//! persistence does not establish proof. Not wired to the CLI/server.

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Publication {
    Unknown,
    Confirmed,
    Revoked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Consumer {
    OAuth,
    Admin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressError {
    UnknownInstance,
    StaleRevision,
    PublicationUnconfirmed,
    InvalidTransition,
    InvalidInstances,
}

#[derive(Clone, Copy, Debug)]
pub enum ProgressEvent {
    Uncertain,
    PublicationConfirmed,
    AdoptionReported {
        instance: [u8; 16],
        consumer: Consumer,
    },
    Revoked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Instance {
    id: [u8; 16],
    oauth: bool,
    admin: bool,
}

/// Tracks claims about one candidate, never the credential itself.
/// IDs are opaque operation/instance identifiers, not credential fingerprints.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RotationProgress {
    revision: [u8; 16],
    publication: Publication,
    instances: Vec<Instance>,
}

impl RotationProgress {
    pub(crate) fn accepts_instance(&self, instance: [u8; 16]) -> bool {
        self.publication != Publication::Revoked
            && self.instances.iter().any(|entry| entry.id == instance)
    }
    pub(crate) fn apply(
        &mut self,
        revision: [u8; 16],
        event: ProgressEvent,
    ) -> Result<(), ProgressError> {
        match event {
            ProgressEvent::Uncertain => self.mark_uncertain(),
            ProgressEvent::PublicationConfirmed => self.confirm_publication(revision),
            ProgressEvent::AdoptionReported { instance, consumer } => {
                self.record_adoption(revision, instance, consumer)
            }
            ProgressEvent::Revoked => self.mark_revoked(revision),
        }
    }
    pub(crate) fn valid_for(&self, revision: [u8; 16]) -> bool {
        if self.revision != revision
            || self.revision == [0; 16]
            || self.instances.is_empty()
            || self.instances.len() > 128
        {
            return false;
        }
        for (index, entry) in self.instances.iter().enumerate() {
            if entry.id == [0; 16]
                || self.instances[..index]
                    .iter()
                    .any(|other| other.id == entry.id)
            {
                return false;
            }
            if self.publication != Publication::Confirmed && (entry.oauth || entry.admin) {
                return false;
            }
        }
        true
    }
    pub fn new(revision: [u8; 16], instances: &[[u8; 16]]) -> Result<Self, ProgressError> {
        if revision == [0; 16] || instances.is_empty() || instances.len() > 128 {
            return Err(ProgressError::InvalidInstances);
        }
        let mut entries = Vec::with_capacity(instances.len());
        for id in instances {
            if *id == [0; 16] || entries.iter().any(|entry: &Instance| entry.id == *id) {
                return Err(ProgressError::InvalidInstances);
            }
            entries.push(Instance {
                id: *id,
                oauth: false,
                admin: false,
            });
        }
        Ok(Self {
            revision,
            publication: Publication::Unknown,
            instances: entries,
        })
    }

    pub fn publication(&self) -> Publication {
        self.publication
    }

    /// An uncertain outcome invalidates prior adoption claims.
    pub fn mark_uncertain(&mut self) -> Result<(), ProgressError> {
        if self.publication == Publication::Revoked {
            return Err(ProgressError::InvalidTransition);
        }
        self.publication = Publication::Unknown;
        self.clear_adoptions();
        Ok(())
    }

    /// Caller must have independently verified the same operation's publication.
    pub fn confirm_publication(&mut self, revision: [u8; 16]) -> Result<(), ProgressError> {
        self.check_revision(revision)?;
        if self.publication == Publication::Revoked {
            return Err(ProgressError::InvalidTransition);
        }
        self.publication = Publication::Confirmed;
        Ok(())
    }

    /// Record a caller-verified successful login for one consumer and instance.
    pub fn record_adoption(
        &mut self,
        revision: [u8; 16],
        instance: [u8; 16],
        consumer: Consumer,
    ) -> Result<(), ProgressError> {
        self.check_revision(revision)?;
        if self.publication != Publication::Confirmed {
            return Err(ProgressError::PublicationUnconfirmed);
        }
        let entry = self
            .instances
            .iter_mut()
            .find(|entry| entry.id == instance)
            .ok_or(ProgressError::UnknownInstance)?;
        match consumer {
            Consumer::OAuth => entry.oauth = true,
            Consumer::Admin => entry.admin = true,
        }
        Ok(())
    }

    /// Revocation is terminal for this candidate; no fallback is performed.
    pub fn mark_revoked(&mut self, revision: [u8; 16]) -> Result<(), ProgressError> {
        self.check_revision(revision)?;
        self.publication = Publication::Revoked;
        self.clear_adoptions();
        Ok(())
    }

    pub fn all_consumers_reported(&self) -> bool {
        self.publication == Publication::Confirmed
            && self
                .instances
                .iter()
                .all(|entry| entry.oauth && entry.admin)
    }

    fn check_revision(&self, revision: [u8; 16]) -> Result<(), ProgressError> {
        if self.revision != revision {
            return Err(ProgressError::StaleRevision);
        }
        Ok(())
    }

    fn clear_adoptions(&mut self) {
        for entry in &mut self.instances {
            entry.oauth = false;
            entry.admin = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertainty_stale_evidence_and_revocation_prevent_completion() {
        let revision = [1; 16];
        let first = [2; 16];
        let second = [3; 16];
        let mut tracking = RotationProgress::new(revision, &[first, second]).unwrap();
        assert_eq!(
            tracking.record_adoption(revision, first, Consumer::OAuth),
            Err(ProgressError::PublicationUnconfirmed)
        );
        assert_eq!(
            tracking.confirm_publication([9; 16]),
            Err(ProgressError::StaleRevision)
        );
        tracking.confirm_publication(revision).unwrap();
        assert_eq!(
            tracking.record_adoption(revision, [4; 16], Consumer::OAuth),
            Err(ProgressError::UnknownInstance)
        );
        for instance in [first, second] {
            tracking
                .record_adoption(revision, instance, Consumer::OAuth)
                .unwrap();
            tracking
                .record_adoption(revision, instance, Consumer::Admin)
                .unwrap();
        }
        assert!(tracking.all_consumers_reported());
        tracking.mark_uncertain().unwrap();
        assert!(!tracking.all_consumers_reported());
        tracking.confirm_publication(revision).unwrap();
        assert!(!tracking.all_consumers_reported());
        tracking.mark_revoked(revision).unwrap();
        assert_eq!(
            tracking.confirm_publication(revision),
            Err(ProgressError::InvalidTransition)
        );
        assert_eq!(
            tracking.mark_uncertain(),
            Err(ProgressError::InvalidTransition)
        );
        assert_eq!(
            tracking.record_adoption(revision, first, Consumer::Admin),
            Err(ProgressError::PublicationUnconfirmed)
        );
        assert!(!tracking.all_consumers_reported());
    }
}
