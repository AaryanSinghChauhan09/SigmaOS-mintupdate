//! Distro-neutral update policy and lifecycle tracking.
//!
//! Repository access, downloads, package installation, and privilege handling
//! belong to injected backend components. This module only validates state
//! transitions and returns explicit policy plans.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateType {
    System,
    Security,
    Kernel,
    Flatpak,
    Spice,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStage {
    Download,
    Install,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Available,
    Downloading,
    Downloaded,
    Installing,
    Installed,
    Failed { stage: UpdateStage, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub id: String,
    pub name: String,
    pub version: String,
    pub update_type: UpdateType,
    pub state: UpdateState,
    pub size: u64,
    pub description: String,
    pub priority: u32,
    pub reboot_required: bool,
}

impl Update {
    pub fn new(id: String, name: String, update_type: UpdateType) -> Self {
        Self {
            id,
            name,
            version: String::new(),
            update_type,
            state: UpdateState::Available,
            size: 0,
            description: String::new(),
            priority: 0,
            reboot_required: false,
        }
    }

    pub fn is_installed(&self) -> bool {
        self.state == UpdateState::Installed
    }

    /// An available update must be downloaded before it can be installed.
    pub fn is_ready_to_install(&self) -> bool {
        self.state == UpdateState::Downloaded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationPolicy {
    None,
    DownloadOnly,
    Automatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutomationPlan {
    pub download_ids: Vec<String>,
    pub install_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    InvalidId,
    InvalidName,
    DuplicateId(String),
    NotFound(String),
    InvalidTransition {
        id: String,
        state: UpdateState,
        operation: &'static str,
    },
    EmptyFailureReason,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => write!(f, "update ID must be nonempty and contain no whitespace"),
            Self::InvalidName => write!(f, "update name must not be empty"),
            Self::DuplicateId(id) => write!(f, "update ID is already registered: {id}"),
            Self::NotFound(id) => write!(f, "update was not found: {id}"),
            Self::InvalidTransition {
                id,
                state,
                operation,
            } => {
                write!(f, "cannot {operation} update {id} while it is {state:?}")
            }
            Self::EmptyFailureReason => write!(f, "failure reason must not be empty"),
        }
    }
}

impl std::error::Error for UpdateError {}

#[derive(Debug, Clone)]
pub struct MintUpdateManager {
    updates: BTreeMap<String, Update>,
    automation_policy: AutomationPolicy,
    auto_refresh_interval: u32,
    history: Vec<String>,
}

impl MintUpdateManager {
    pub fn new() -> Self {
        Self {
            updates: BTreeMap::new(),
            automation_policy: AutomationPolicy::None,
            auto_refresh_interval: 15,
            history: Vec::new(),
        }
    }

    /// Add an update discovered by a repository backend.
    pub fn add_update(&mut self, update: Update) -> Result<(), UpdateError> {
        if update.id.trim().is_empty() || update.id.chars().any(char::is_whitespace) {
            return Err(UpdateError::InvalidId);
        }
        if update.name.trim().is_empty() {
            return Err(UpdateError::InvalidName);
        }
        if self.updates.contains_key(&update.id) {
            return Err(UpdateError::DuplicateId(update.id));
        }
        if update.state != UpdateState::Available {
            return Err(UpdateError::InvalidTransition {
                id: update.id,
                state: update.state,
                operation: "register",
            });
        }
        self.updates.insert(update.id.clone(), update);
        Ok(())
    }

    pub fn update_count(&self) -> usize {
        self.updates.len()
    }

    pub fn get_update(&self, id: &str) -> Option<&Update> {
        self.updates.get(id)
    }

    /// Record backend progress only after the corresponding operation begins.
    pub fn begin_download(&mut self, id: &str) -> Result<(), UpdateError> {
        self.transition(
            id,
            "begin download",
            UpdateState::Available,
            UpdateState::Downloading,
        )
    }

    pub fn mark_downloaded(&mut self, id: &str) -> Result<(), UpdateError> {
        self.transition(
            id,
            "complete download",
            UpdateState::Downloading,
            UpdateState::Downloaded,
        )
    }

    pub fn begin_install(&mut self, id: &str) -> Result<(), UpdateError> {
        self.transition(
            id,
            "begin install",
            UpdateState::Downloaded,
            UpdateState::Installing,
        )
    }

    pub fn mark_installed(&mut self, id: &str) -> Result<(), UpdateError> {
        self.transition(
            id,
            "complete install",
            UpdateState::Installing,
            UpdateState::Installed,
        )?;
        if let Some(update) = self.updates.get(id) {
            self.history.push(format!(
                "Installed {} version {}",
                update.name, update.version
            ));
        }
        Ok(())
    }

    pub fn mark_failed(&mut self, id: &str, reason: &str) -> Result<(), UpdateError> {
        if reason.trim().is_empty() {
            return Err(UpdateError::EmptyFailureReason);
        }
        let update = self.update_mut(id)?;
        let stage = match &update.state {
            UpdateState::Downloading => UpdateStage::Download,
            UpdateState::Installing => UpdateStage::Install,
            _ => {
                return Err(UpdateError::InvalidTransition {
                    id: id.to_string(),
                    state: update.state.clone(),
                    operation: "mark failed",
                });
            }
        };
        update.state = UpdateState::Failed {
            stage,
            reason: reason.trim().to_string(),
        };
        Ok(())
    }

    /// Reset a failed operation to the last safe retry point.
    pub fn retry_failed(&mut self, id: &str) -> Result<(), UpdateError> {
        let update = self.update_mut(id)?;
        let next_state = match &update.state {
            UpdateState::Failed {
                stage: UpdateStage::Download,
                ..
            } => UpdateState::Available,
            UpdateState::Failed {
                stage: UpdateStage::Install,
                ..
            } => UpdateState::Downloaded,
            _ => {
                return Err(UpdateError::InvalidTransition {
                    id: id.to_string(),
                    state: update.state.clone(),
                    operation: "retry",
                });
            }
        };
        update.state = next_state;
        Ok(())
    }

    fn transition(
        &mut self,
        id: &str,
        operation: &'static str,
        expected: UpdateState,
        next: UpdateState,
    ) -> Result<(), UpdateError> {
        let update = self.update_mut(id)?;
        if update.state != expected {
            return Err(UpdateError::InvalidTransition {
                id: id.to_string(),
                state: update.state.clone(),
                operation,
            });
        }
        update.state = next;
        Ok(())
    }

    fn update_mut(&mut self, id: &str) -> Result<&mut Update, UpdateError> {
        self.updates
            .get_mut(id)
            .ok_or_else(|| UpdateError::NotFound(id.to_string()))
    }

    /// Return a plan only; callers remain responsible for user consent and backend execution.
    pub fn automation_plan(&self) -> AutomationPlan {
        let mut plan = AutomationPlan::default();
        for update in self.updates.values() {
            match (self.automation_policy, &update.state) {
                (
                    AutomationPolicy::DownloadOnly | AutomationPolicy::Automatic,
                    UpdateState::Available,
                ) => {
                    plan.download_ids.push(update.id.clone());
                }
                (AutomationPolicy::Automatic, UpdateState::Downloaded) => {
                    plan.install_ids.push(update.id.clone());
                }
                _ => {}
            }
        }
        plan
    }

    pub fn automation_policy(&self) -> AutomationPolicy {
        self.automation_policy
    }

    pub fn set_automation_policy(&mut self, policy: AutomationPolicy) {
        self.automation_policy = policy;
    }

    /// Zero disables timed refresh; nonzero intervals are measured in minutes.
    pub fn set_auto_refresh_interval(&mut self, interval: u32) {
        self.auto_refresh_interval = interval;
    }

    pub fn refresh_due(&self, elapsed_minutes: u32) -> bool {
        self.auto_refresh_interval != 0 && elapsed_minutes >= self.auto_refresh_interval
    }

    pub fn get_by_type(&self, update_type: &UpdateType) -> Vec<&Update> {
        self.updates
            .values()
            .filter(|update| &update.update_type == update_type)
            .collect()
    }

    pub fn get_security_updates(&self) -> Vec<&Update> {
        self.get_by_type(&UpdateType::Security)
    }

    pub fn get_kernel_updates(&self) -> Vec<&Update> {
        self.get_by_type(&UpdateType::Kernel)
    }

    pub fn get_available_updates(&self) -> Vec<&Update> {
        self.updates
            .values()
            .filter(|update| {
                update.state == UpdateState::Available || update.state == UpdateState::Downloaded
            })
            .collect()
    }

    pub fn get_history(&self) -> &[String] {
        &self.history
    }

    pub fn is_reboot_required(&self) -> bool {
        self.updates
            .values()
            .any(|update| update.reboot_required && update.is_installed())
    }
}

impl Default for MintUpdateManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_creation() {
        let update = Update::new(
            "test-update".into(),
            "Test Update".into(),
            UpdateType::System,
        );
        assert_eq!(update.name, "Test Update");
        assert!(!update.is_installed());
        assert!(!update.is_ready_to_install());
    }

    #[test]
    fn test_automation_policy() {
        let mut manager = MintUpdateManager::new();
        assert_eq!(manager.automation_policy(), AutomationPolicy::None);
        manager.set_automation_policy(AutomationPolicy::Automatic);
        assert_eq!(manager.automation_policy(), AutomationPolicy::Automatic);
    }

    #[test]
    fn test_add_update() {
        let mut manager = MintUpdateManager::new();
        let update = Update::new(
            "sys-upgrade".into(),
            "System Upgrade".into(),
            UpdateType::System,
        );
        manager.add_update(update).unwrap();
        assert_eq!(manager.update_count(), 1);
    }

    #[test]
    fn test_refresh_interval_zero_disables_refresh() {
        let mut manager = MintUpdateManager::new();
        manager.set_auto_refresh_interval(0);
        assert!(!manager.refresh_due(u32::MAX));
    }

    #[test]
    fn test_backend_reported_download_install_transitions() {
        let mut manager = MintUpdateManager::new();
        let update = Update::new("test-pkg".into(), "Test Package".into(), UpdateType::System);
        manager.add_update(update).unwrap();
        manager.begin_download("test-pkg").unwrap();
        manager.mark_downloaded("test-pkg").unwrap();
        manager.begin_install("test-pkg").unwrap();
        manager.mark_installed("test-pkg").unwrap();
        assert!(manager.get_update("test-pkg").unwrap().is_installed());
    }

    #[test]
    fn test_automation_plan_does_not_change_update_state() {
        let mut manager = MintUpdateManager::new();
        manager
            .add_update(Update::new(
                "pkg-a".into(),
                "Package A".into(),
                UpdateType::System,
            ))
            .unwrap();
        manager.set_automation_policy(AutomationPolicy::Automatic);

        let plan = manager.automation_plan();

        assert_eq!(plan.download_ids, vec!["pkg-a"]);
        assert!(plan.install_ids.is_empty());
        assert_eq!(
            manager.get_update("pkg-a").unwrap().state,
            UpdateState::Available
        );
    }

    #[test]
    fn test_install_failure_retries_from_downloaded_state() {
        let mut manager = MintUpdateManager::new();
        manager
            .add_update(Update::new(
                "pkg-b".into(),
                "Package B".into(),
                UpdateType::Security,
            ))
            .unwrap();
        manager.begin_download("pkg-b").unwrap();
        manager.mark_downloaded("pkg-b").unwrap();
        assert!(manager.begin_install("pkg-b").is_ok());
        manager
            .mark_failed("pkg-b", "backend rejected transaction")
            .unwrap();

        manager.retry_failed("pkg-b").unwrap();

        assert_eq!(
            manager.get_update("pkg-b").unwrap().state,
            UpdateState::Downloaded
        );
    }

    #[test]
    fn test_get_by_type() {
        let mut manager = MintUpdateManager::new();
        manager
            .add_update(Update::new(
                "sec-update".into(),
                "Security Update".into(),
                UpdateType::Security,
            ))
            .unwrap();
        assert_eq!(manager.get_security_updates().len(), 1);
    }
}
