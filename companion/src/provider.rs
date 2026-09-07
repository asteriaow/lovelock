use std::fmt;
use std::thread;
use std::time::Duration;

use crate::action::ResolvedVibrateAction;
use lovense::{Connection as LovenseConnection, Error as LovenseError, LovenseClient, Toy};
use thiserror::Error;

pub const TEST_VIBRATE_STRENGTH: u8 = 8;
pub const TEST_VIBRATE_DURATION_SECS: u32 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct LovenseSetup {
    pub domain: String,
    pub http_port: u16,
}
impl Default for LovenseSetup {
    fn default() -> Self {
        let connection = LovenseConnection::default();
        Self {
            domain: connection.domain().to_owned(),
            http_port: connection.http_port(),
        }
    }
}
impl fmt::Debug for LovenseSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LovenseSetup")
            .field("domain", &self.domain)
            .field("http_port", &self.http_port)
            .finish()
    }
}
impl LovenseSetup {
    pub fn normalized(&self) -> Self {
        Self {
            domain: self.domain.trim().to_owned(),
            http_port: self.http_port,
        }
    }
    pub fn present(&self) -> bool {
        !self.domain.trim().is_empty() && self.http_port != 0
    }
    fn connection(&self) -> LovenseConnection {
        LovenseConnection::new(self.domain.trim(), self.http_port)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSettings {
    pub lovense: LovenseSetup,
}
impl ProviderSettings {
    pub fn present(&self) -> bool {
        self.lovense.present()
    }
    pub fn normalize(&mut self) {
        self.lovense = self.lovense.normalized();
    }
}

pub type TargetId = String;

#[derive(Clone)]
pub struct ProviderTarget {
    id: TargetId,
    name: String,
    toy: Option<Toy>,
}
impl fmt::Debug for ProviderTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderTarget")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}
impl PartialEq for ProviderTarget {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name
    }
}
impl Eq for ProviderTarget {}
impl ProviderTarget {
    pub fn id(&self) -> &TargetId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    #[cfg(test)]
    pub(crate) fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            toy: None,
        }
    }
}
fn target_from_lovense(toy: Toy) -> ProviderTarget {
    ProviderTarget {
        id: toy.id.clone(),
        name: toy.name.clone(),
        toy: Some(toy),
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Lovense: {0}")]
    Lovense(#[from] LovenseError),
    #[error("Lovense setup is invalid")]
    InvalidSetup,
    #[error("Lovense is not connected")]
    NotConnected,
}
impl ProviderError {
    /// A plain-language explanation suitable for display in the UI. The
    /// `Display` impl above (via `#[error(...)]`) stays technical and is
    /// what gets logged.
    pub fn user_message(&self) -> String {
        match self {
            Self::Lovense(LovenseError::EmptyDomain) => "Enter a connection domain first.".to_owned(),
            Self::Lovense(LovenseError::Transport(_)) => {
                "Can't reach the Lovense app. Is Game Mode on?".to_owned()
            }
            Self::Lovense(LovenseError::HttpStatus(_)) => "Lovense app didn't respond as expected.".to_owned(),
            Self::Lovense(LovenseError::Decode { .. }) => "Got an unexpected response from Lovense.".to_owned(),
            Self::Lovense(LovenseError::CommandRejected { message }) => {
                format!("Lovense rejected the request: {message}")
            }
            Self::Lovense(LovenseError::InvalidStrength) | Self::Lovense(LovenseError::InvalidDuration) => {
                self.to_string()
            }
            Self::InvalidSetup => "Enter a domain and port first.".to_owned(),
            Self::NotConnected => "Not connected yet.".to_owned(),
        }
    }
}

fn whole_second_duration(duration_secs: f32) -> Option<u32> {
    (duration_secs >= 1.0 && duration_secs.fract() == 0.0).then_some(duration_secs as u32)
}

pub struct ConnectedProvider(LovenseClient);
impl ConnectedProvider {
    pub fn connect(setup: &LovenseSetup) -> Result<Self, ProviderError> {
        if !setup.present() {
            return Err(ProviderError::InvalidSetup);
        }
        Ok(Self(LovenseClient::connect(setup.connection())?))
    }
    pub fn list_targets(&self) -> Result<Vec<ProviderTarget>, ProviderError> {
        Ok(self
            .0
            .list_toys()?
            .into_iter()
            .map(target_from_lovense)
            .collect())
    }
    pub fn test_action(&self, target: Option<&ProviderTarget>) -> Result<(), ProviderError> {
        let toy = target.and_then(|target| target.toy.as_ref());
        self.0
            .vibrate(toy, TEST_VIBRATE_STRENGTH, TEST_VIBRATE_DURATION_SECS)?;
        Ok(())
    }
    /// Runs one resolved trigger action. A whole-second duration is sent as
    /// the Standard API's own timed `Vibrate` command, so the toy runs it
    /// independently and this returns immediately. A sub-second duration
    /// cannot be expressed that way (`timeSec` only takes whole seconds), so
    /// instead this holds the toy steady and blocks the calling thread for
    /// the (well under one second) duration before sending an explicit stop.
    pub fn execute(
        &self,
        target: Option<&ProviderTarget>,
        action: ResolvedVibrateAction,
    ) -> Result<(), ProviderError> {
        let toy = target.and_then(|target| target.toy.as_ref());
        match whole_second_duration(action.duration_secs) {
            Some(duration_secs) => {
                self.0.vibrate(toy, action.strength, duration_secs)?;
            }
            None => {
                self.0.vibrate_steady(toy, action.strength)?;
                thread::sleep(Duration::from_secs_f32(action.duration_secs.max(0.0)));
                self.0.stop(toy)?;
            }
        }
        Ok(())
    }
    /// Holds the toy at `strength` with no time limit between trigger effects.
    /// A strength of 0 stops the toy instead of sending a zero-strength
    /// vibration.
    pub fn set_resting(
        &self,
        target: Option<&ProviderTarget>,
        strength: u8,
    ) -> Result<(), ProviderError> {
        let toy = target.and_then(|target| target.toy.as_ref());
        if strength == 0 {
            self.0.stop(toy)?;
        } else {
            self.0.vibrate_steady(toy, strength)?;
        }
        Ok(())
    }
    pub fn disconnect(self) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_second_durations_are_recognized_and_sub_second_ones_are_not() {
        assert_eq!(whole_second_duration(1.0), Some(1));
        assert_eq!(whole_second_duration(60.0), Some(60));
        assert_eq!(whole_second_duration(0.25), None);
        assert_eq!(whole_second_duration(0.75), None);
        assert_eq!(whole_second_duration(0.0), None);
    }

    #[test]
    fn setup_present_requires_domain_and_port() {
        let mut setup = LovenseSetup::default();
        assert!(setup.present());
        setup.domain = "  ".to_owned();
        assert!(!setup.present());
        setup.domain = "127-0-0-1.lovense.club".to_owned();
        setup.http_port = 0;
        assert!(!setup.present());
    }

    #[test]
    fn setup_normalizes_whitespace() {
        let setup = LovenseSetup {
            domain: "  example.lan  ".to_owned(),
            http_port: 30010,
        };
        assert_eq!(setup.normalized().domain, "example.lan");
    }

    #[test]
    fn connect_rejects_incomplete_setup_before_any_request() {
        let setup = LovenseSetup {
            domain: String::new(),
            http_port: 30010,
        };
        assert!(matches!(
            ConnectedProvider::connect(&setup),
            Err(ProviderError::InvalidSetup)
        ));
    }
}
