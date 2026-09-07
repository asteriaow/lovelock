use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::action::{
    IntensityCurve, IntensityPoint, MAX_INTENSITY_DAMAGE, MAX_INTENSITY_WINDOW_SECS,
    MAX_VIBRATE_STRENGTH, ResolvedVibrateAction, VibrateActionSettings, portable_vibrate_duration,
};
use crate::bridge_listener::{
    AbilityTrigger, BridgeEvent, ConsoleLogListener, CountTrigger, ListenerPhase, ListenerStatus,
    LocalPlayerDeath, LocalPlayerRespawn, ModVersionObservation, VitalsTrigger,
};
use crate::deadlock_path::{self, Detection, DetectionError};
use crate::logging::{LogSnapshot, LogStore};
use crate::persistence::{PersistedState, Persistence, default_state_path};
use crate::provider::{ConnectedProvider, ProviderError, ProviderSettings, ProviderTarget, TargetId};
use crate::version_check::{
    COMPANION_RELEASE_URL, LATEST_RELEASE_URL, MOD_RELEASE_URL, VersionCheckOwner,
    VersionCheckState, WarningSelection, app_version, select_warnings,
};
use egui::{Color32, TextEdit, Ui};

pub(crate) const ACTION_QUEUE_CAPACITY: usize = 10;
pub(crate) const MAX_ACTION_QUEUE_AGE: Duration = Duration::from_secs(30);
const PROVIDER_LABEL: &str = "Lovense";
const ACTION_KIND_LABEL: &str = "vibrate";
const RESTING_QUEUE_CAPACITY: usize = 4;
/// How often the resting level is re-sent even when nothing changed, so a toy
/// that quietly drops an open-ended command still gets held at the baseline.
const RESTING_REASSERT_INTERVAL: Duration = Duration::from_secs(120);
/// Minimum wait between resting sends when the last one has not been confirmed
/// yet, so a failing send does not spin every frame.
const RESTING_RETRY_BACKOFF: Duration = Duration::from_secs(2);
/// Upper bound on how long a "hold until respawn" death effect keeps the toy,
/// in case the respawn event never arrives (older mod, missed line).
pub(crate) const RESPAWN_HOLD_SAFETY_CAP_SECS: u32 = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerSettings {
    pub enabled: bool,
    pub actions: VibrateActionSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AbilityTriggerSettings {
    pub trigger: TriggerSettings,
    pub ability_filter: AbilityFilter,
}

/// Lowest amount, accumulated by [`AmountLedger`] within `window_seconds`,
/// before this trigger fires. Mirrors the shape of [`AbilityTriggerSettings`]
/// but gates on a rolling sum instead of an ability slot.
#[derive(Clone, Debug, PartialEq)]
pub struct AmountTriggerSettings {
    pub trigger: TriggerSettings,
    pub threshold: f32,
    pub window_seconds: f32,
}
impl AmountTriggerSettings {
    fn new(threshold: f32, window_seconds: f32) -> Self {
        Self {
            trigger: TriggerSettings {
                enabled: false,
                actions: VibrateActionSettings::default(),
            },
            threshold,
            window_seconds,
        }
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AbilityFilter {
    #[default]
    All,
    Selected(BTreeSet<u32>),
}

impl AbilityFilter {
    pub fn accepts(&self, ability_slot: u32) -> bool {
        match self {
            Self::All => true,
            Self::Selected(slots) => slots.contains(&ability_slot),
        }
    }
}

/// Trigger kinds from highest to lowest priority. When two effects would
/// otherwise overlap on the toy, the higher-priority one keeps it and the
/// lower-priority one is skipped until the first finishes. Also doubles as
/// the canonical "every trigger kind" list for UI iteration (trigger cards,
/// copy-source lists) that has no real ordering to respect, so those places
/// don't keep a second array in sync with this one by hand.
/// Reading-measure caps for the main content area. Single-column tabs stay
/// tighter; the Effects tab is a two-pane layout so it gets more room.
const COLUMN_CONTENT_MAX_WIDTH: f32 = 880.0;
const EFFECTS_CONTENT_MAX_WIDTH: f32 = 1180.0;

/// Notice text colour, matching `theme::ACCENT_BRIGHT` (the light pink) so
/// warnings sit inside the palette instead of clashing amber.
const NOTICE_PINK: [f32; 4] = [1.0, 0.765, 0.878, 1.0];

/// The trigger kinds shown in the UI, in default overlap-priority order. This
/// is the single source of truth for "which kinds exist": a kind left out here
/// is pruned from every profile's saved order by [`normalize_priority_order`],
/// so retiring a trigger only means deleting its line below (the enum variant,
/// its settings field and its persistence stay for backward compatibility).
/// `SoulSecure` stays retired: the HUD shows one flat gold number for every
/// soul gain, so "you shot the orb" is not detectable. `SoulDeny` is live
/// again -- the HUD has a dedicated `deny` combat indicator for a denied soul.
/// `DamageTakenIntensity` was folded into `DamageTaken`, which now runs the
/// intensity curve itself.
pub(crate) const PRIORITY_ORDER_DEFAULT: [TriggerKind; 19] = [
    TriggerKind::Death,
    TriggerKind::Kill,
    TriggerKind::Assist,
    TriggerKind::AbilityUse,
    TriggerKind::AbilityCooldownReady,
    TriggerKind::DamageTaken,
    TriggerKind::HealingReceived,
    TriggerKind::AllyHealed,
    TriggerKind::AllyShielded,
    TriggerKind::DamageGiven,
    TriggerKind::SoulDeny,
    TriggerKind::ParrySuccess,
    TriggerKind::ParryFail,
    TriggerKind::ObjectiveGuardian,
    TriggerKind::ObjectiveWalker,
    TriggerKind::ObjectiveBaseGuardian,
    TriggerKind::ObjectiveShrine,
    TriggerKind::ObjectivePatronWeakened,
    TriggerKind::GameWon,
];

/// The name every session starts with and that a state file from before
/// profiles existed is migrated into.
pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// A named, switchable bundle of every effect setting: all trigger configs,
/// their overlap-priority order, the death-hold / suppress-while-dead flags,
/// and the resting vibration level. Connection settings (device, Lovense
/// endpoint, log path) are global and stay outside profiles.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectProfile {
    pub name: String,
    pub triggers: TriggerSettingsSet,
    pub resting_strength: u8,
}
impl EffectProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            triggers: TriggerSettingsSet::default(),
            resting_strength: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerSettingsSet {
    pub death: TriggerSettings,
    pub kill: TriggerSettings,
    pub assist: TriggerSettings,
    pub ability_use: AbilityTriggerSettings,
    pub ability_cooldown_ready: AbilityTriggerSettings,
    pub damage_taken: AmountTriggerSettings,
    pub healing_received: AmountTriggerSettings,
    /// Fires once each time the mod sees you heal a teammate (an impact-popup
    /// instance carrying a heal marker). Event based, like [`Self::assist`].
    pub ally_healed: TriggerSettings,
    /// Fires once each time the mod sees you give a teammate a barrier.
    pub ally_shielded: TriggerSettings,
    /// Rolling-window sum of the damage numbers the game shows for your hits,
    /// gated by a threshold. Amount based, like [`Self::damage_taken`].
    pub damage_given: AmountTriggerSettings,
    /// Fires when you deny a soul orb (a `deny` combat indicator).
    pub soul_deny: TriggerSettings,
    /// Fires when you secure souls (a `gold` combat indicator).
    pub soul_secure: TriggerSettings,
    /// Fires when a parry you started resolved without you being stunned.
    pub parry_success: TriggerSettings,
    /// Fires when you were stunned right after starting a parry.
    pub parry_fail: TriggerSettings,
    /// Objective destroyed on the enemy side. Each is a discrete fire-once
    /// event with its own vibration profile so they can be toggled and tuned
    /// independently. `guardian`/`walker` come from the persistent objectives
    /// minimap; `base_guardian`/`shrine`/`patron_weakened` are best-effort
    /// from the centre-screen objective-health bar.
    pub objective_guardian: TriggerSettings,
    pub objective_walker: TriggerSettings,
    pub objective_base_guardian: TriggerSettings,
    pub objective_shrine: TriggerSettings,
    pub objective_patron_weakened: TriggerSettings,
    /// Fires once when your team wins the match.
    pub game_won: TriggerSettings,
    /// Retired standalone enable flag for the old health-band intensity
    /// trigger. Kept so `get`/`get_mut` stay exhaustive and an old profile
    /// still deserializes; not shown in the UI.
    pub damage_taken_intensity: TriggerSettings,
    /// The curve that [`TriggerKind::DamageTaken`] runs: damage summed over a
    /// rolling window maps to a vibration level, so a heavier beating drives
    /// the toy harder. Replaces the old fixed strength + threshold.
    pub damage_taken_curve: IntensityCurve,
    /// Highest-priority trigger kind first; see [`PRIORITY_ORDER_DEFAULT`].
    pub priority_order: Vec<TriggerKind>,
    /// When set, the death effect ignores its configured duration and holds the
    /// toy until the mod reports the local player respawning (bounded by
    /// [`RESPAWN_HOLD_SAFETY_CAP_SECS`]).
    pub death_hold_until_respawn: bool,
    /// When set, ability and assist triggers are ignored between the local
    /// player's death and respawn, so a spectated teammate's play cannot cut
    /// the death effect short.
    pub suppress_triggers_while_dead: bool,
}
impl Default for TriggerSettingsSet {
    fn default() -> Self {
        let actions = VibrateActionSettings::default();
        Self {
            priority_order: PRIORITY_ORDER_DEFAULT.to_vec(),
            death_hold_until_respawn: true,
            suppress_triggers_while_dead: true,
            death: TriggerSettings {
                enabled: true,
                actions: actions.clone(),
            },
            kill: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            assist: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            ally_healed: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            ally_shielded: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            soul_deny: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            soul_secure: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            parry_success: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            parry_fail: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            objective_guardian: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            objective_walker: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            objective_base_guardian: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            objective_shrine: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            objective_patron_weakened: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            game_won: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            damage_taken_intensity: TriggerSettings {
                enabled: false,
                actions: actions.clone(),
            },
            damage_taken_curve: IntensityCurve::default(),
            ability_use: AbilityTriggerSettings {
                trigger: TriggerSettings {
                    enabled: false,
                    actions: actions.clone(),
                },
                ability_filter: AbilityFilter::All,
            },
            ability_cooldown_ready: AbilityTriggerSettings {
                trigger: TriggerSettings {
                    enabled: false,
                    actions,
                },
                ability_filter: AbilityFilter::All,
            },
            damage_taken: AmountTriggerSettings::new(200.0, 3.0),
            healing_received: AmountTriggerSettings::new(150.0, 3.0),
            damage_given: AmountTriggerSettings::new(400.0, 3.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CredentialState {
    #[default]
    Unknown,
    Testing,
    Valid,
    Invalid,
}
impl CredentialState {
    fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Not tested yet",
            Self::Testing => "Testing connection…",
            Self::Valid => "Connected",
            Self::Invalid => "Connection failed",
        }
    }
    fn tone(self) -> crate::theme::BadgeTone {
        match self {
            Self::Unknown | Self::Testing => crate::theme::BadgeTone::Neutral,
            Self::Valid => crate::theme::BadgeTone::Success,
            Self::Invalid => crate::theme::BadgeTone::Danger,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LogDetectionStatus {
    Found,
    NotCreated,
    Failed(String),
}
impl LogDetectionStatus {
    fn label(&self) -> &str {
        match self {
            Self::Found => "Found Deadlock console.log.",
            Self::NotCreated => {
                "Deadlock is installed, but console.log has not been created. Add -condebug to Deadlock's Steam launch options, then launch the game."
            }
            Self::Failed(message) => message,
        }
    }
    fn tone(&self) -> crate::theme::BadgeTone {
        match self {
            Self::Found => crate::theme::BadgeTone::Success,
            Self::NotCreated => crate::theme::BadgeTone::Warning,
            Self::Failed(_) => crate::theme::BadgeTone::Danger,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogListenerStartContext {
    Manual,
    Startup,
}

impl LogListenerStartContext {
    fn label(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Startup => "startup",
        }
    }
}

type ConnectionResult = Result<(ConnectedProvider, Vec<ProviderTarget>), ProviderError>;
type TestActionResult = Result<(), ProviderError>;
fn provider_error_kind(error: &ProviderError) -> &'static str {
    match error {
        ProviderError::Lovense(_) => "lovense",
        ProviderError::InvalidSetup => "invalid_setup",
        ProviderError::NotConnected => "not_connected",
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum TestActionStatus {
    Sending,
    Sent,
    Failed(String),
}
impl TestActionStatus {
    fn label(&self) -> &str {
        match self {
            Self::Sending => "Sending test vibration…",
            Self::Sent => "Test vibration sent.",
            Self::Failed(message) => message,
        }
    }
    fn tone(&self) -> crate::theme::BadgeTone {
        match self {
            Self::Sending => crate::theme::BadgeTone::Neutral,
            Self::Sent => crate::theme::BadgeTone::Success,
            Self::Failed(_) => crate::theme::BadgeTone::Danger,
        }
    }
}

/// Drag-and-drop payload for the trigger card list (which doubles as the
/// overlap-priority order): which card, by its position before the drag
/// started, is being moved.
#[derive(Clone, Copy, Debug)]
struct PriorityDragIndex(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerKind {
    Death,
    Kill,
    Assist,
    AbilityUse,
    AbilityCooldownReady,
    DamageTaken,
    HealingReceived,
    AllyHealed,
    AllyShielded,
    DamageGiven,
    SoulDeny,
    SoulSecure,
    ParrySuccess,
    ParryFail,
    ObjectiveGuardian,
    ObjectiveWalker,
    ObjectiveBaseGuardian,
    ObjectiveShrine,
    ObjectivePatronWeakened,
    GameWon,
    DamageTakenIntensity,
}

impl TriggerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Death => "death",
            Self::Kill => "kill",
            Self::Assist => "assist",
            Self::AbilityUse => "ability use",
            Self::AbilityCooldownReady => "ability cooldown ready",
            Self::DamageTaken => "damage taken",
            Self::HealingReceived => "healing received",
            Self::AllyHealed => "ally healed",
            Self::AllyShielded => "ally shielded",
            Self::DamageGiven => "damage given",
            Self::SoulDeny => "soul deny",
            Self::SoulSecure => "soul secure",
            Self::ParrySuccess => "parry success",
            Self::ParryFail => "got parried",
            Self::ObjectiveGuardian => "guardian destroyed",
            Self::ObjectiveWalker => "walker destroyed",
            Self::ObjectiveBaseGuardian => "base guardian destroyed",
            Self::ObjectiveShrine => "shrine destroyed",
            Self::ObjectivePatronWeakened => "patron weakened",
            Self::GameWon => "game won",
            Self::DamageTakenIntensity => "damage intensity",
        }
    }

    /// Whether this trigger gates on an [`AmountLedger`] rolling sum passing a
    /// threshold (and so gets the threshold editor). `DamageTaken` is excluded:
    /// it is amount-driven but through the intensity curve, not a threshold.
    fn is_amount_based(self) -> bool {
        matches!(self, Self::HealingReceived | Self::DamageGiven)
    }

    /// Whether this trigger's strength comes from the damage-taken intensity
    /// curve instead of a Fixed/Random vibrate setting.
    fn is_intensity_curve(self) -> bool {
        matches!(self, Self::DamageTaken)
    }
}

impl TriggerSettingsSet {
    /// Position of `kind` in the overlap-priority order; lower is higher
    /// priority. A kind missing from the order (only possible from a
    /// hand-edited state file) sorts last.
    fn priority_rank(&self, kind: TriggerKind) -> usize {
        self.priority_order
            .iter()
            .position(|candidate| *candidate == kind)
            .unwrap_or(usize::MAX)
    }

    /// Drops duplicates and any kind not in [`PRIORITY_ORDER_DEFAULT`] (a
    /// retired trigger a saved order still lists), then appends any default
    /// kind missing from a restored order so every live kind has a rank.
    pub(crate) fn normalize_priority_order(&mut self) {
        let mut ordered = Vec::with_capacity(PRIORITY_ORDER_DEFAULT.len());
        for kind in self.priority_order.drain(..) {
            if PRIORITY_ORDER_DEFAULT.contains(&kind) && !ordered.contains(&kind) {
                ordered.push(kind);
            }
        }
        for kind in PRIORITY_ORDER_DEFAULT {
            if !ordered.contains(&kind) {
                ordered.push(kind);
            }
        }
        self.priority_order = ordered;
    }

    fn get(&self, kind: TriggerKind) -> &TriggerSettings {
        match kind {
            TriggerKind::Death => &self.death,
            TriggerKind::Kill => &self.kill,
            TriggerKind::Assist => &self.assist,
            TriggerKind::AbilityUse => &self.ability_use.trigger,
            TriggerKind::AbilityCooldownReady => &self.ability_cooldown_ready.trigger,
            TriggerKind::DamageTaken => &self.damage_taken.trigger,
            TriggerKind::HealingReceived => &self.healing_received.trigger,
            TriggerKind::AllyHealed => &self.ally_healed,
            TriggerKind::AllyShielded => &self.ally_shielded,
            TriggerKind::DamageGiven => &self.damage_given.trigger,
            TriggerKind::SoulDeny => &self.soul_deny,
            TriggerKind::SoulSecure => &self.soul_secure,
            TriggerKind::ParrySuccess => &self.parry_success,
            TriggerKind::ParryFail => &self.parry_fail,
            TriggerKind::ObjectiveGuardian => &self.objective_guardian,
            TriggerKind::ObjectiveWalker => &self.objective_walker,
            TriggerKind::ObjectiveBaseGuardian => &self.objective_base_guardian,
            TriggerKind::ObjectiveShrine => &self.objective_shrine,
            TriggerKind::ObjectivePatronWeakened => &self.objective_patron_weakened,
            TriggerKind::GameWon => &self.game_won,
            TriggerKind::DamageTakenIntensity => &self.damage_taken_intensity,
        }
    }

    fn get_mut(&mut self, kind: TriggerKind) -> &mut TriggerSettings {
        match kind {
            TriggerKind::Death => &mut self.death,
            TriggerKind::Kill => &mut self.kill,
            TriggerKind::Assist => &mut self.assist,
            TriggerKind::AbilityUse => &mut self.ability_use.trigger,
            TriggerKind::AbilityCooldownReady => &mut self.ability_cooldown_ready.trigger,
            TriggerKind::DamageTaken => &mut self.damage_taken.trigger,
            TriggerKind::HealingReceived => &mut self.healing_received.trigger,
            TriggerKind::AllyHealed => &mut self.ally_healed,
            TriggerKind::AllyShielded => &mut self.ally_shielded,
            TriggerKind::DamageGiven => &mut self.damage_given.trigger,
            TriggerKind::SoulDeny => &mut self.soul_deny,
            TriggerKind::SoulSecure => &mut self.soul_secure,
            TriggerKind::ParrySuccess => &mut self.parry_success,
            TriggerKind::ParryFail => &mut self.parry_fail,
            TriggerKind::ObjectiveGuardian => &mut self.objective_guardian,
            TriggerKind::ObjectiveWalker => &mut self.objective_walker,
            TriggerKind::ObjectiveBaseGuardian => &mut self.objective_base_guardian,
            TriggerKind::ObjectiveShrine => &mut self.objective_shrine,
            TriggerKind::ObjectivePatronWeakened => &mut self.objective_patron_weakened,
            TriggerKind::GameWon => &mut self.game_won,
            TriggerKind::DamageTakenIntensity => &mut self.damage_taken_intensity,
        }
    }

    /// The threshold + rolling-window settings for an amount-based trigger,
    /// or `None` for a trigger kind that fires per discrete event instead.
    fn amount_settings(&self, kind: TriggerKind) -> Option<&AmountTriggerSettings> {
        match kind {
            TriggerKind::DamageTaken => Some(&self.damage_taken),
            TriggerKind::HealingReceived => Some(&self.healing_received),
            TriggerKind::DamageGiven => Some(&self.damage_given),
            _ => None,
        }
    }

    fn amount_settings_mut(&mut self, kind: TriggerKind) -> Option<&mut AmountTriggerSettings> {
        match kind {
            TriggerKind::DamageTaken => Some(&mut self.damage_taken),
            TriggerKind::HealingReceived => Some(&mut self.healing_received),
            TriggerKind::DamageGiven => Some(&mut self.damage_given),
            _ => None,
        }
    }

    fn ability_filter(&self, kind: TriggerKind) -> Option<&AbilityFilter> {
        match kind {
            TriggerKind::Death
            | TriggerKind::Kill
            | TriggerKind::Assist
            | TriggerKind::AllyHealed
            | TriggerKind::AllyShielded
            | TriggerKind::DamageGiven
            | TriggerKind::SoulDeny
            | TriggerKind::SoulSecure
            | TriggerKind::ParrySuccess
            | TriggerKind::ParryFail
            | TriggerKind::ObjectiveGuardian
            | TriggerKind::ObjectiveWalker
            | TriggerKind::ObjectiveBaseGuardian
            | TriggerKind::ObjectiveShrine
            | TriggerKind::ObjectivePatronWeakened
            | TriggerKind::GameWon
            | TriggerKind::DamageTakenIntensity
            | TriggerKind::DamageTaken
            | TriggerKind::HealingReceived => None,
            TriggerKind::AbilityUse => Some(&self.ability_use.ability_filter),
            TriggerKind::AbilityCooldownReady => Some(&self.ability_cooldown_ready.ability_filter),
        }
    }

    fn ability_filter_mut(&mut self, kind: TriggerKind) -> Option<&mut AbilityFilter> {
        match kind {
            TriggerKind::Death
            | TriggerKind::Kill
            | TriggerKind::Assist
            | TriggerKind::AllyHealed
            | TriggerKind::AllyShielded
            | TriggerKind::DamageGiven
            | TriggerKind::SoulDeny
            | TriggerKind::SoulSecure
            | TriggerKind::ParrySuccess
            | TriggerKind::ParryFail
            | TriggerKind::ObjectiveGuardian
            | TriggerKind::ObjectiveWalker
            | TriggerKind::ObjectiveBaseGuardian
            | TriggerKind::ObjectiveShrine
            | TriggerKind::ObjectivePatronWeakened
            | TriggerKind::GameWon
            | TriggerKind::DamageTakenIntensity
            | TriggerKind::DamageTaken
            | TriggerKind::HealingReceived => None,
            TriggerKind::AbilityUse => Some(&mut self.ability_use.ability_filter),
            TriggerKind::AbilityCooldownReady => {
                Some(&mut self.ability_cooldown_ready.ability_filter)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AppSection {
    #[default]
    Setup,
    Effects,
    GameConnection,
    Donate,
}

impl AppSection {
    fn label(self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Effects => "Effects",
            Self::GameConnection => "Game connection",
            Self::Donate => "Donate",
        }
    }
}

const KOFI_URL: &str = "https://ko-fi.com/asteriaxo";
const VOLC_PROFILE_URL: &str = "https://gamebanana.com/members/5741016";

#[derive(Clone, Debug, PartialEq)]
struct TriggerIdentity {
    kind: TriggerKind,
    session_id: String,
    sequence: u64,
    client_time_ms: u64,
    detection: String,
    ability_slot: Option<u32>,
    ability_name: Option<String>,
    charges_before: Option<u64>,
    charges_after: Option<u64>,
    /// Total accumulated within the trigger's rolling window, for an
    /// amount-based trigger kind (see [`TriggerKind::is_amount_based`]).
    amount_total: Option<f32>,
    /// When set, [`AppState::queue_trigger_action`] plays exactly this action
    /// instead of resolving the trigger's own settings, and the identity does
    /// not take part in the monotonic novelty gate. Used by the damage-taken
    /// intensity curve, whose strength is computed from live damage, not from
    /// stored vibrate settings.
    override_action: Option<ResolvedVibrateAction>,
}

impl TriggerIdentity {
    fn from_death(death: LocalPlayerDeath) -> Self {
        Self {
            kind: TriggerKind::Death,
            session_id: death.session_id,
            sequence: death.sequence,
            client_time_ms: death.client_time_ms,
            detection: death.detection,
            ability_slot: None,
            ability_name: None,
            charges_before: None,
            charges_after: None,
            amount_total: None,
            override_action: None,
        }
    }

    fn from_ability(kind: TriggerKind, ability: AbilityTrigger) -> Self {
        Self {
            kind,
            session_id: ability.session_id,
            sequence: ability.sequence,
            client_time_ms: ability.client_time_ms,
            detection: ability.detection,
            ability_slot: Some(ability.ability_slot),
            ability_name: ability.ability_name,
            charges_before: ability.charges_before,
            charges_after: ability.charges_after,
            amount_total: None,
            override_action: None,
        }
    }

    fn from_count(kind: TriggerKind, count: CountTrigger) -> Self {
        Self {
            kind,
            session_id: count.session_id,
            sequence: count.sequence,
            client_time_ms: count.client_time_ms,
            detection: count.detection,
            ability_slot: None,
            ability_name: None,
            charges_before: count.count_before,
            charges_after: count.count_after,
            amount_total: None,
            override_action: None,
        }
    }

    /// Builds the identity for an amount-based trigger once its rolling
    /// window has cleared the configured threshold. `total` is what the
    /// window held at that moment, not just the single sample that tipped it
    /// over.
    fn from_amount(kind: TriggerKind, vitals: &VitalsTrigger, total: f32) -> Self {
        Self {
            kind,
            session_id: vitals.session_id.clone(),
            sequence: vitals.sequence,
            client_time_ms: vitals.client_time_ms,
            detection: vitals.detection.clone(),
            ability_slot: None,
            ability_name: None,
            charges_before: None,
            charges_after: None,
            amount_total: Some(total),
            override_action: None,
        }
    }

    fn status_description(&self) -> String {
        if matches!(
            self.kind,
            TriggerKind::Death
                | TriggerKind::Kill
                | TriggerKind::Assist
                | TriggerKind::AllyHealed
                | TriggerKind::AllyShielded
                | TriggerKind::SoulDeny
                | TriggerKind::SoulSecure
                | TriggerKind::ParrySuccess
                | TriggerKind::ParryFail
                | TriggerKind::ObjectiveGuardian
                | TriggerKind::ObjectiveWalker
                | TriggerKind::ObjectiveBaseGuardian
                | TriggerKind::ObjectiveShrine
                | TriggerKind::ObjectivePatronWeakened
                | TriggerKind::GameWon
                | TriggerKind::DamageTakenIntensity
        ) {
            return format!("{} {}#{}", self.kind.label(), self.session_id, self.sequence);
        }
        if let Some(total) = self.amount_total {
            return format!(
                "{} of {total:.0} within the window, detection {}, {}#{}",
                self.kind.label(),
                self.detection,
                self.session_id,
                self.sequence
            );
        }
        let name = self
            .ability_name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let charges = match (self.charges_before, self.charges_after) {
            (Some(before), Some(after)) => format!(", charges {before}→{after}"),
            _ => String::new(),
        };
        format!(
            "{} slot {}{name}, detection {}{charges}, {}#{}",
            self.kind.label(),
            self.ability_slot.unwrap_or_default(),
            self.detection,
            self.session_id,
            self.sequence
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ActionRequest {
    target: Option<ProviderTarget>,
    resolved: ResolvedVibrateAction,
    trigger: TriggerIdentity,
    queued_at: Instant,
}

/// The action currently expected to be running on the toy, so a
/// lower-priority trigger that fires while it plays can be held back instead
/// of cutting it short. `ends_at` is the optimistic finish time computed
/// from the resolved duration when the action was queued.
#[derive(Clone, Copy, Debug)]
struct ActiveAction {
    priority_rank: usize,
    ends_at: Instant,
}

#[derive(Clone, Debug, PartialEq)]
struct ActionSnapshot {
    target: Option<ProviderTarget>,
    resolved: Option<ResolvedVibrateAction>,
    trigger: TriggerIdentity,
}
impl ActionSnapshot {
    fn from_request(request: &ActionRequest) -> Self {
        Self {
            target: request.target.clone(),
            resolved: Some(request.resolved),
            trigger: request.trigger.clone(),
        }
    }
}

struct ActionJob {
    client: Arc<ConnectedProvider>,
    request: ActionRequest,
}

#[derive(Debug)]
enum ActionCompletionResult {
    Completed(Result<(), ProviderError>),
    Skipped { reason: &'static str },
}

struct ActionCompletion {
    request: ActionRequest,
    result: ActionCompletionResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionEnqueueResult {
    Accepted,
    Full,
    Disconnected,
}

fn action_job_expired_at(queued_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(queued_at) >= MAX_ACTION_QUEUE_AGE
}

/// Applies the death trigger's "hold until respawn" override: the effect keeps
/// its resolved strength but runs for [`RESPAWN_HOLD_SAFETY_CAP_SECS`] instead
/// of its configured duration, so it covers the whole time spent dead.
fn resolved_for_trigger(
    kind: TriggerKind,
    hold_until_respawn: bool,
    resolved: ResolvedVibrateAction,
) -> ResolvedVibrateAction {
    if kind == TriggerKind::Death && hold_until_respawn {
        ResolvedVibrateAction {
            duration_secs: RESPAWN_HOLD_SAFETY_CAP_SECS as f32,
            ..resolved
        }
    } else {
        resolved
    }
}

/// Whether `maintain_resting` should drive the toy at all. With the baseline
/// disabled (`desired == 0`) and the toy already idle or never touched, it
/// stays hands-off so a partner's phone-driven vibration is not stopped.
fn resting_should_act(desired: u8, applied: Option<u8>) -> bool {
    desired != 0 || !matches!(applied, None | Some(0))
}

/// Whether the toy is already sitting where the baseline wants it (so no send
/// and no forced repaint are needed).
fn resting_at_rest(desired: u8, applied: Option<u8>) -> bool {
    applied == Some(desired) || !resting_should_act(desired, applied)
}

/// Whether the resting baseline should be (re)sent now: as soon as the retry
/// backoff has passed when it is not yet at the desired level, otherwise only
/// on the slow heartbeat.
fn resting_send_due(applied: Option<u8>, desired: u8, since_sent: Duration) -> bool {
    if applied == Some(desired) {
        since_sent >= RESTING_REASSERT_INTERVAL
    } else {
        since_sent >= RESTING_RETRY_BACKOFF
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ActionStatus {
    Sending(ActionRequest),
    Sent(ActionRequest),
    Failed {
        request: ActionRequest,
        error: String,
    },
    Skipped {
        snapshot: ActionSnapshot,
        reason: String,
    },
}
impl ActionStatus {
    fn snapshot(&self) -> ActionSnapshot {
        match self {
            Self::Sending(request) | Self::Sent(request) => ActionSnapshot::from_request(request),
            Self::Failed { request, .. } => ActionSnapshot::from_request(request),
            Self::Skipped { snapshot, .. } => snapshot.clone(),
        }
    }
    fn label(&self) -> String {
        let snapshot = self.snapshot();
        let target = snapshot
            .target
            .as_ref()
            .map(|target| target.name())
            .unwrap_or("no target");
        let action = ACTION_KIND_LABEL;
        let resolved = snapshot
            .resolved
            .map(ResolvedVibrateAction::summary)
            .unwrap_or_else(|| "settings unavailable".to_owned());
        let trigger = snapshot.trigger.status_description();
        match self {
            Self::Sending(_) => {
                format!("{PROVIDER_LABEL}: Sending {action} to {target} at {resolved} ({trigger})…")
            }
            Self::Sent(_) => {
                format!("{PROVIDER_LABEL}: {action} sent to {target} at {resolved} ({trigger}).")
            }
            Self::Failed { error, .. } => format!(
                "{PROVIDER_LABEL}: {action} failed for {target} at {resolved} ({trigger}): {error}"
            ),
            Self::Skipped { reason, .. } => format!(
                "{PROVIDER_LABEL}: {action} skipped for {target} at {resolved} ({trigger}): {reason}"
            ),
        }
    }
    fn color(&self) -> [f32; 4] {
        match self {
            Self::Sending(_) => [0.65, 0.65, 0.65, 1.0],
            Self::Sent(_) => [0.30, 0.78, 0.42, 1.0],
            Self::Failed { .. } => [0.92, 0.32, 0.28, 1.0],
            Self::Skipped { .. } => [0.92, 0.68, 0.22, 1.0],
        }
    }
}

fn spawn_action_worker() -> (SyncSender<ActionJob>, Receiver<ActionCompletion>) {
    let (job_sender, job_receiver) = mpsc::sync_channel::<ActionJob>(ACTION_QUEUE_CAPACITY);
    let (completion_sender, completion_receiver) = mpsc::channel::<ActionCompletion>();
    thread::spawn(move || {
        while let Ok(job) = job_receiver.recv() {
            let result = if action_job_expired_at(job.request.queued_at, Instant::now()) {
                ActionCompletionResult::Skipped { reason: "expired" }
            } else {
                ActionCompletionResult::Completed(
                    job.client
                        .execute(job.request.target.as_ref(), job.request.resolved),
                )
            };
            let _ = completion_sender.send(ActionCompletion {
                request: job.request,
                result,
            });
        }
    });
    (job_sender, completion_receiver)
}

struct RestingJob {
    client: Arc<ConnectedProvider>,
    target: Option<ProviderTarget>,
    strength: u8,
}

struct RestingCompletion {
    strength: u8,
    result: Result<(), ProviderError>,
}

/// A dedicated worker for the between-triggers resting level, kept separate
/// from the trigger action worker so a slow baseline send never delays a
/// trigger and its outcome never overwrites the user-facing trigger status.
fn spawn_resting_worker() -> (SyncSender<RestingJob>, Receiver<RestingCompletion>) {
    let (job_sender, job_receiver) = mpsc::sync_channel::<RestingJob>(RESTING_QUEUE_CAPACITY);
    let (completion_sender, completion_receiver) = mpsc::channel::<RestingCompletion>();
    thread::spawn(move || {
        while let Ok(job) = job_receiver.recv() {
            let result = job.client.set_resting(job.target.as_ref(), job.strength);
            let _ = completion_sender.send(RestingCompletion {
                strength: job.strength,
                result,
            });
        }
    });
    (job_sender, completion_receiver)
}

/// A rolling sum of an amount-carrying stat (damage, healing) over its own
/// trailing window: how much of it landed within the last `window_seconds`.
/// Samples age out of a ring buffer as they fall outside that window rather
/// than a growable list pruned from the front on every insert, and a running
/// total is kept incrementally instead of re-summed on each add.
#[derive(Default)]
struct AmountLedger {
    samples: VecDeque<(Instant, f32)>,
    running_total: f32,
}
impl AmountLedger {
    fn reset(&mut self) {
        self.samples.clear();
        self.running_total = 0.0;
    }

    /// Records `amount` at `now`, drops anything older than `window_seconds`,
    /// and returns the total left standing within the window.
    fn record(&mut self, amount: f32, window_seconds: f32, now: Instant) -> f32 {
        if amount > 0.0 {
            self.samples.push_back((now, amount));
            self.running_total += amount;
        }
        let window = Duration::from_secs_f32(window_seconds.max(0.1));
        while let Some(&(stamp, value)) = self.samples.front() {
            if now.saturating_duration_since(stamp) <= window {
                break;
            }
            self.running_total = (self.running_total - value).max(0.0);
            self.samples.pop_front();
        }
        self.running_total
    }
}

pub struct AppState {
    pub provider_settings: ProviderSettings,
    pub credential_state: CredentialState,
    pub devices: Vec<ProviderTarget>,
    pub selected_device: Option<TargetId>,
    pub preferred_target: Option<TargetId>,
    pub triggers: TriggerSettingsSet,
    /// Every saved effect bundle. `profiles[active_profile]` is the one being
    /// edited; its contents are mirrored live in `triggers` / `resting_strength`
    /// and copied back on a profile switch or a save.
    pub profiles: Vec<EffectProfile>,
    pub active_profile: usize,
    pub log_path: String,
    client: Option<Arc<ConnectedProvider>>,
    connection_error: Option<String>,
    connection_result: Option<Receiver<ConnectionResult>>,
    device_refresh_result: Option<Receiver<Result<Vec<ProviderTarget>, ProviderError>>>,
    test_action_result: Option<Receiver<TestActionResult>>,
    test_action_status: Option<TestActionStatus>,
    action_sender: SyncSender<ActionJob>,
    action_result: Receiver<ActionCompletion>,
    action_in_flight: usize,
    action_status: Option<ActionStatus>,
    /// User-set baseline the toy holds between triggers (0-20; 0 disables it and
    /// keeps the pre-baseline behaviour of stopping after each effect).
    pub resting_strength: u8,
    resting_sender: SyncSender<RestingJob>,
    resting_result: Receiver<RestingCompletion>,
    /// Strength last confirmed as applied to the toy as a resting level, so an
    /// unchanged baseline is not re-sent every frame.
    resting_applied: Option<u8>,
    resting_last_sent: Option<Instant>,
    resting_in_flight: bool,
    /// True between a death effect starting in "hold until respawn" mode and the
    /// respawn event (or the safety cap) clearing it.
    awaiting_respawn: bool,
    log_detection_status: Option<LogDetectionStatus>,
    bridge_listener: ConsoleLogListener,
    bridge_events: Option<Receiver<BridgeEvent>>,
    last_bridge_event: Option<BridgeEvent>,
    last_sequence: Option<(String, u64)>,
    active_action: Option<ActiveAction>,
    ability_catalog: BTreeMap<u32, Option<String>>,
    damage_ledger: AmountLedger,
    healing_ledger: AmountLedger,
    damage_given_ledger: AmountLedger,
    /// Rolling window of damage taken, fed to the intensity curve. Separate
    /// from `damage_ledger` because it uses its own (curve-configured) window.
    damage_intensity_ledger: AmountLedger,
    /// When the last intensity pulse was queued, so pulses are throttled while
    /// damage keeps arriving every tick.
    damage_intensity_last_pulse: Option<Instant>,
    listener_action_error: Option<String>,
    selected_section: AppSection,
    selected_effect: TriggerKind,
    copy_source: TriggerKind,
    copy_feedback: Option<String>,
    /// UI-only: the profile chip currently swapped for an inline rename field
    /// (entered from its right-click menu), and whether that field still needs
    /// its initial keyboard focus.
    renaming_profile: Option<usize>,
    renaming_needs_focus: bool,
}
impl Default for AppState {
    fn default() -> Self {
        let (action_sender, action_result) = spawn_action_worker();
        let (resting_sender, resting_result) = spawn_resting_worker();
        Self {
            provider_settings: ProviderSettings::default(),
            credential_state: CredentialState::default(),
            devices: Vec::new(),
            selected_device: None,
            preferred_target: None,
            triggers: TriggerSettingsSet::default(),
            profiles: vec![EffectProfile::new(DEFAULT_PROFILE_NAME)],
            active_profile: 0,
            log_path: String::new(),
            client: None,
            connection_error: None,
            connection_result: None,
            device_refresh_result: None,
            test_action_result: None,
            test_action_status: None,
            action_sender,
            action_result,
            action_in_flight: 0,
            action_status: None,
            resting_strength: 0,
            resting_sender,
            resting_result,
            resting_applied: None,
            resting_last_sent: None,
            resting_in_flight: false,
            awaiting_respawn: false,
            log_detection_status: None,
            bridge_listener: ConsoleLogListener::default(),
            bridge_events: None,
            last_bridge_event: None,
            last_sequence: None,
            active_action: None,
            ability_catalog: BTreeMap::new(),
            damage_ledger: AmountLedger::default(),
            healing_ledger: AmountLedger::default(),
            damage_given_ledger: AmountLedger::default(),
            damage_intensity_ledger: AmountLedger::default(),
            damage_intensity_last_pulse: None,
            listener_action_error: None,
            selected_section: AppSection::default(),
            selected_effect: TriggerKind::Death,
            copy_source: TriggerKind::AbilityUse,
            copy_feedback: None,
            renaming_profile: None,
            renaming_needs_focus: false,
        }
    }
}
impl AppState {
    pub(crate) fn effective_provider_settings(&self) -> ProviderSettings {
        self.provider_settings.clone()
    }
    pub fn credentials_present(&self) -> bool {
        self.provider_settings.present()
    }
    pub fn selected_device(&self) -> Option<&ProviderTarget> {
        let selected = self.selected_device.as_ref()?;
        self.devices.iter().find(|device| device.id() == selected)
    }
    fn connection_in_progress(&self) -> bool {
        self.connection_result.is_some()
    }
    fn device_refresh_in_progress(&self) -> bool {
        self.device_refresh_result.is_some()
    }
    fn test_action_in_progress(&self) -> bool {
        self.test_action_result.is_some()
    }
    fn action_in_progress(&self) -> bool {
        self.action_in_flight != 0
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.connection_in_progress()
            || self.device_refresh_in_progress()
            || self.test_action_in_progress()
            || self.action_in_progress()
    }
    pub(crate) fn reset_saved_state(&mut self) -> bool {
        if self.is_busy() {
            log::warn!(target: "companion::app", "settings_reset_skipped reason=busy");
            return false;
        }

        self.bridge_listener.stop();
        self.reset_connection();
        self.provider_settings = ProviderSettings::default();
        self.preferred_target = None;
        self.triggers = TriggerSettingsSet::default();
        self.profiles = vec![EffectProfile::new(DEFAULT_PROFILE_NAME)];
        self.active_profile = 0;
        self.renaming_profile = None;
        self.renaming_needs_focus = false;
        self.log_path.clear();
        self.last_sequence = None;
        self.active_action = None;
        self.ability_catalog.clear();
        self.damage_ledger.reset();
        self.healing_ledger.reset();
        self.damage_given_ledger.reset();
        self.damage_intensity_ledger.reset();
        self.damage_intensity_last_pulse = None;
        self.listener_action_error = None;
        self.log_detection_status = None;
        self.bridge_listener = ConsoleLogListener::default();
        self.bridge_events = None;
        self.last_bridge_event = None;
        let (action_sender, action_result) = spawn_action_worker();
        self.action_sender = action_sender;
        self.action_result = action_result;
        let (resting_sender, resting_result) = spawn_resting_worker();
        self.resting_sender = resting_sender;
        self.resting_result = resting_result;
        self.resting_strength = 0;
        self.resting_applied = None;
        self.resting_last_sent = None;
        self.resting_in_flight = false;
        self.awaiting_respawn = false;
        self.copy_feedback = None;
        self.action_status = None;
        self.action_in_flight = 0;
        log::info!(target: "companion::app", "settings_reset_applied provider={PROVIDER_LABEL}");
        true
    }
    /// Copies the live effect settings back into the profile slot they came
    /// from, so a switch or a save does not lose in-progress edits.
    fn sync_active_profile(&mut self) {
        if let Some(profile) = self.profiles.get_mut(self.active_profile) {
            profile.triggers = self.triggers.clone();
            profile.resting_strength = self.resting_strength;
        }
    }

    /// Loads `profiles[active_profile]` into the live settings and clears the
    /// per-effect runtime state so the new profile's rules take over cleanly.
    fn load_active_profile(&mut self) {
        self.renaming_profile = None;
        self.renaming_needs_focus = false;
        if let Some(profile) = self.profiles.get(self.active_profile).cloned() {
            self.triggers = profile.triggers;
            self.resting_strength = profile.resting_strength;
        }
        self.damage_ledger.reset();
        self.healing_ledger.reset();
        self.damage_given_ledger.reset();
        self.damage_intensity_ledger.reset();
        self.damage_intensity_last_pulse = None;
        self.active_action = None;
        self.awaiting_respawn = false;
        self.resting_applied = None;
        self.resting_last_sent = None;
        self.copy_feedback = None;
        if self.copy_source == self.selected_effect {
            self.copy_source = first_copy_source(self.selected_effect);
        }
    }

    pub(crate) fn switch_profile(&mut self, index: usize) {
        if index >= self.profiles.len() || index == self.active_profile {
            return;
        }
        self.sync_active_profile();
        log::info!(
            target: "companion::app",
            "effect_profile_switched from={:?} to={:?}",
            self.profiles[self.active_profile].name,
            self.profiles[index].name
        );
        self.active_profile = index;
        self.load_active_profile();
    }

    /// `base` if it is free, otherwise the first free "`base` 2", "`base` 3"...
    fn unique_profile_name(&self, base: &str) -> String {
        let taken = |candidate: &str| self.profiles.iter().any(|p| p.name == candidate);
        if !taken(base) {
            return base.to_owned();
        }
        (2..).map(|n| format!("{base} {n}")).find(|c| !taken(c)).unwrap()
    }

    fn push_profile_and_activate(&mut self, profile: EffectProfile) {
        self.sync_active_profile();
        self.profiles.push(profile);
        self.active_profile = self.profiles.len() - 1;
        self.load_active_profile();
    }

    /// Adds a fresh profile, switches to it, and drops straight into renaming
    /// it. A new profile starts with a usable death effect enabled (a fixed
    /// mid-strength buzz) and everything else off.
    pub(crate) fn add_blank_profile(&mut self) {
        let name = self.unique_profile_name("New profile");
        let mut profile = EffectProfile::new(name);
        profile.triggers.death.enabled = true;
        profile.triggers.death.actions = VibrateActionSettings::starter_death();
        self.push_profile_and_activate(profile);
        self.begin_rename_profile(self.active_profile);
    }

    /// Adds a copy of profile `index` and switches to it.
    pub(crate) fn duplicate_profile(&mut self, index: usize) {
        // So duplicating the active profile captures unsaved live edits.
        self.sync_active_profile();
        let Some(source) = self.profiles.get(index).cloned() else {
            return;
        };
        let profile = EffectProfile {
            name: self.unique_profile_name(&format!("{} copy", source.name)),
            triggers: source.triggers,
            resting_strength: source.resting_strength,
        };
        self.push_profile_and_activate(profile);
    }

    /// Removes profile `index` (a no-op when it is the last one) and keeps the
    /// same profile selected, shifting the active index if the removal was
    /// above it.
    pub(crate) fn delete_profile(&mut self, index: usize) {
        if self.profiles.len() <= 1 || index >= self.profiles.len() {
            return;
        }
        let removed = self.profiles.remove(index);
        log::info!(target: "companion::app", "effect_profile_deleted name={:?}", removed.name);
        if self.active_profile > index {
            self.active_profile -= 1;
        }
        self.active_profile = self.active_profile.min(self.profiles.len() - 1);
        self.load_active_profile();
    }

    /// Puts profile `index` into inline-rename mode in the switcher.
    pub(crate) fn begin_rename_profile(&mut self, index: usize) {
        if index < self.profiles.len() {
            self.renaming_profile = Some(index);
            self.renaming_needs_focus = true;
        }
    }

    /// Ends inline-rename mode, trimming the name and falling back to a fresh
    /// unique placeholder if it was left blank.
    fn commit_profile_rename(&mut self, index: usize) {
        if index < self.profiles.len() {
            let trimmed = self.profiles[index].name.trim().to_owned();
            let name = if trimmed.is_empty() {
                self.unique_profile_name("New profile")
            } else {
                trimmed
            };
            self.profiles[index].name = name;
        }
        self.renaming_profile = None;
        self.renaming_needs_focus = false;
    }

    #[cfg(test)]
    pub(crate) fn listener_is_running(&self) -> bool {
        self.bridge_listener.status().phase != ListenerPhase::Stopped
    }
    #[cfg(test)]
    pub(crate) fn runtime_trigger_and_action_state_is_clear(&self) -> bool {
        self.bridge_events.is_none()
            && self.last_bridge_event.is_none()
            && self.last_sequence.is_none()
            && self.active_action.is_none()
            && self.ability_catalog.is_empty()
            && self.damage_ledger.samples.is_empty()
            && self.healing_ledger.samples.is_empty()
            && self.damage_given_ledger.samples.is_empty()
            && self.damage_intensity_ledger.samples.is_empty()
            && self.action_status.is_none()
            && self.action_in_flight == 0
            && !self.awaiting_respawn
            && self.resting_applied.is_none()
            && !self.resting_in_flight
    }

    fn reset_connection(&mut self) {
        if let Some(client) = self.client.take() {
            match Arc::try_unwrap(client) {
                Ok(client) => match client.disconnect() {
                    Ok(()) => log::info!(
                        target: "companion::app",
                        "provider_disconnected provider={PROVIDER_LABEL} outcome=success"
                    ),
                    Err(error) => log::warn!(
                        target: "companion::app",
                        "provider_disconnected provider={PROVIDER_LABEL} outcome=failed error_kind={}",
                        provider_error_kind(&error)
                    ),
                },
                Err(_) => log::debug!(
                    target: "companion::app",
                    "provider_disconnect_skipped provider={PROVIDER_LABEL} reason=shared_client"
                ),
            }
        }
        self.credential_state = CredentialState::Unknown;
        self.connection_result = None;
        self.device_refresh_result = None;
        self.devices.clear();
        self.selected_device = None;
        self.connection_error = None;
        self.test_action_result = None;
        self.test_action_status = None;
    }
    fn start_connection_test(&mut self, context: egui::Context) {
        let config = self.provider_settings.lovense.normalized();
        log::info!(
            target: "companion::app",
            "connection_test_started provider={PROVIDER_LABEL}"
        );
        self.reset_connection();
        let (sender, receiver) = mpsc::channel();
        self.credential_state = CredentialState::Testing;
        self.connection_error = None;
        self.connection_result = Some(receiver);
        thread::spawn(move || {
            let result = ConnectedProvider::connect(&config).and_then(|client| {
                let devices = client.list_targets()?;
                Ok((client, devices))
            });
            let _ = sender.send(result);
            context.request_repaint();
        });
    }
    fn poll_connection_test(&mut self) {
        let Some(receiver) = &self.connection_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.connection_result = None;
                self.apply_connection_result(result);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                log::error!(
                    target: "companion::app",
                    "connection_worker_failed provider={PROVIDER_LABEL} reason=channel_closed"
                );
                self.connection_result = None;
                self.apply_connection_error(ProviderError::NotConnected);
            }
        }
    }

    fn start_device_refresh(&mut self, context: egui::Context) {
        let Some(client) = self.client.clone() else {
            log::warn!(target: "companion::app", "device_refresh_skipped outcome=skipped error_kind=not_connected");
            return;
        };
        log::info!(target: "companion::app", "device_refresh_started provider={PROVIDER_LABEL}");
        let (sender, receiver) = mpsc::channel();
        self.device_refresh_result = Some(receiver);
        thread::spawn(move || {
            let result = client.list_targets();
            let _ = sender.send(result);
            context.request_repaint();
        });
    }
    fn poll_device_refresh(&mut self) {
        let Some(receiver) = &self.device_refresh_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(devices)) => {
                self.device_refresh_result = None;
                log::info!(
                    target: "companion::app",
                    "device_refresh_succeeded provider={PROVIDER_LABEL} targets={}",
                    devices.len()
                );
                self.apply_devices(devices);
            }
            Ok(Err(error)) => {
                self.device_refresh_result = None;
                log::warn!(
                    target: "companion::app",
                    "device_refresh_failed provider={PROVIDER_LABEL} error_kind={}",
                    provider_error_kind(&error)
                );
                self.connection_error = Some(error.user_message());
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                log::error!(
                    target: "companion::app",
                    "device_refresh_worker_failed provider={PROVIDER_LABEL} reason=channel_closed"
                );
                self.device_refresh_result = None;
            }
        }
    }

    fn apply_connection_result(&mut self, result: ConnectionResult) {
        match result {
            Ok((client, devices)) => {
                log::info!(
                    target: "companion::app",
                    "connection_test_succeeded provider={PROVIDER_LABEL} targets={}",
                    devices.len()
                );
                self.client = Some(Arc::new(client));
                self.apply_devices(devices);
            }
            Err(error) => {
                log::warn!(
                    target: "companion::app",
                    "connection_test_failed provider={PROVIDER_LABEL} error_kind={}",
                    provider_error_kind(&error)
                );
                self.apply_connection_error(error);
            }
        }
    }
    fn apply_connection_error(&mut self, error: ProviderError) {
        self.client = None;
        self.test_action_result = None;
        self.test_action_status = None;
        self.devices.clear();
        self.selected_device = None;
        self.credential_state = CredentialState::Invalid;
        self.connection_error = Some(error.user_message());
    }
    fn apply_devices(&mut self, devices: Vec<ProviderTarget>) {
        let selected = self
            .preferred_target
            .as_ref()
            .filter(|preferred| devices.iter().any(|device| device.id() == *preferred))
            .cloned()
            .or_else(|| devices.first().map(|device| device.id().clone()));
        self.preferred_target = selected.clone();
        self.selected_device = selected;
        self.devices = devices;
        self.credential_state = CredentialState::Valid;
        self.test_action_status = None;
        self.connection_error = None;
    }
    fn select_device(&mut self, target: TargetId) -> bool {
        if !self.devices.iter().any(|device| device.id() == &target) {
            return false;
        }
        self.selected_device = Some(target.clone());
        self.preferred_target = Some(target);
        self.test_action_status = None;
        true
    }

    fn start_test_action(&mut self, context: egui::Context) {
        let Some(client) = self.client.clone() else {
            log::warn!(target: "companion::app", "test_action_skipped outcome=skipped error_kind=not_connected");
            return;
        };
        let target = self.selected_device().cloned();
        log::info!(
            target: "companion::app",
            "test_action_started outcome=started provider={PROVIDER_LABEL} target={:?}",
            target.as_ref().map(ProviderTarget::id)
        );
        let (sender, receiver) = mpsc::channel();
        self.test_action_status = Some(TestActionStatus::Sending);
        self.test_action_result = Some(receiver);
        thread::spawn(move || {
            let result = client.test_action(target.as_ref());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn poll_test_action(&mut self) {
        let Some(receiver) = &self.test_action_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.test_action_result = None;
                self.apply_test_action_result(result);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                log::error!(
                    target: "companion::app",
                    "test_action_worker_failed outcome=failed error_kind=not_connected reason=channel_closed"
                );
                self.test_action_result = None;
                self.apply_test_action_error(ProviderError::NotConnected);
            }
        }
    }

    fn apply_test_action_result(&mut self, result: TestActionResult) {
        match result {
            Ok(()) => {
                log::info!(
                    target: "companion::app",
                    "test_action_succeeded outcome=sent error_kind=none"
                );
                self.test_action_status = Some(TestActionStatus::Sent);
            }
            Err(error) => {
                log::warn!(
                    target: "companion::app",
                    "test_action_failed outcome=failed error_kind={}",
                    provider_error_kind(&error)
                );
                self.apply_test_action_error(error);
            }
        }
    }
    fn apply_test_action_error(&mut self, error: ProviderError) {
        self.test_action_status = Some(TestActionStatus::Failed(format!(
            "Test vibration failed: {}",
            error.user_message()
        )));
    }
    fn copy_action_settings(&mut self, source: TriggerKind, destination: TriggerKind) -> bool {
        if source == destination {
            return false;
        }
        let source_settings = self.triggers.get(source).actions.clone();
        self.triggers
            .get_mut(destination)
            .actions
            .copy_active_from(&source_settings);
        self.copy_feedback = Some(format!(
            "Copied {} {ACTION_KIND_LABEL} settings to {}.",
            source.label(),
            destination.label()
        ));
        true
    }
    fn select_effect(&mut self, kind: TriggerKind) {
        self.selected_effect = kind;
        if self.copy_source == kind {
            self.copy_source = first_copy_source(kind);
        }
        self.copy_feedback = None;
    }

    fn replace_ability_catalog(&mut self, catalog: crate::bridge_listener::AbilityCatalog) {
        self.ability_catalog = catalog
            .abilities
            .into_iter()
            .filter(|ability| ability.ability_slot > 0)
            .map(|ability| (ability.ability_slot, ability.ability_name))
            .collect();
    }
    fn poll_action(&mut self) {
        loop {
            match self.action_result.try_recv() {
                Ok(completion) => {
                    self.action_in_flight = self.action_in_flight.saturating_sub(1);
                    let trigger = &completion.request.trigger;
                    let action_summary = completion.request.resolved.summary();
                    match completion.result {
                        ActionCompletionResult::Skipped { reason } => {
                            log::warn!(
                                target: "companion::app",
                                "action_skipped outcome=skipped error_kind=none trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} reason={}",
                                trigger.kind.label(),
                                completion.request.target.as_ref().map(ProviderTarget::id),
                                action_summary,
                                trigger.session_id,
                                trigger.sequence,
                                reason
                            );
                            self.action_status = Some(ActionStatus::Skipped {
                                snapshot: ActionSnapshot::from_request(&completion.request),
                                reason: reason.to_owned(),
                            });
                        }
                        ActionCompletionResult::Completed(Ok(())) => {
                            log::info!(
                                target: "companion::app",
                                "action_sent outcome=sent error_kind=none trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={}",
                                trigger.kind.label(),
                                completion.request.target.as_ref().map(ProviderTarget::id),
                                action_summary,
                                trigger.session_id,
                                trigger.sequence
                            );
                            self.action_status = Some(ActionStatus::Sent(completion.request));
                        }
                        ActionCompletionResult::Completed(Err(error)) => {
                            log::warn!(
                                target: "companion::app",
                                "action_failed outcome=failed trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} error_kind={}",
                                trigger.kind.label(),
                                completion.request.target.as_ref().map(ProviderTarget::id),
                                action_summary,
                                trigger.session_id,
                                trigger.sequence,
                                provider_error_kind(&error)
                            );
                            self.action_status = Some(ActionStatus::Failed {
                                request: completion.request,
                                error: error.to_string(),
                            });
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::error!(
                        target: "companion::app",
                        "action_worker_channel_failed reason=disconnected in_flight={}",
                        self.action_in_flight
                    );
                    self.action_in_flight = 0;
                    break;
                }
            }
        }
    }

    fn poll_resting(&mut self) {
        loop {
            match self.resting_result.try_recv() {
                Ok(completion) => {
                    self.resting_in_flight = false;
                    match completion.result {
                        Ok(()) => {
                            self.resting_applied = Some(completion.strength);
                            log::debug!(
                                target: "companion::app",
                                "resting_applied strength={}",
                                completion.strength
                            );
                        }
                        Err(error) => {
                            log::warn!(
                                target: "companion::app",
                                "resting_failed strength={} error_kind={}",
                                completion.strength,
                                provider_error_kind(&error)
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.resting_in_flight = false;
                    break;
                }
            }
        }
    }

    /// Keeps the toy at [`resting_strength`](Self::resting_strength) whenever no
    /// trigger effect is playing, re-sending the level after each effect and on
    /// a slow heartbeat so an open-ended command that the toy dropped is
    /// restored.
    fn maintain_resting(&mut self) {
        // If the respawn event never arrives (older mod, dropped log line), the
        // death hold's safety-capped window still bounds the suppression: once
        // it lapses, stop ignoring ability and assist triggers.
        if self.awaiting_respawn
            && self
                .active_action
                .is_none_or(|active| active.ends_at <= Instant::now())
        {
            self.awaiting_respawn = false;
        }
        let Some(client) = self.client.clone() else {
            self.resting_applied = None;
            self.resting_last_sent = None;
            return;
        };
        if self.resting_in_flight {
            return;
        }
        let now = Instant::now();
        let gate_open = self
            .active_action
            .is_none_or(|active| active.ends_at <= now);
        if !gate_open {
            return;
        }
        let desired = self.resting_strength.min(20);
        // With the baseline disabled and nothing to wind down from, leave the
        // toy alone entirely: a trigger's own timed command already stops it,
        // and a partner may be driving it from their phone.
        if !resting_should_act(desired, self.resting_applied) {
            return;
        }
        let since_sent = self
            .resting_last_sent
            .map_or(Duration::MAX, |sent| now.saturating_duration_since(sent));
        if !resting_send_due(self.resting_applied, desired, since_sent) {
            return;
        }
        let target = self.selected_device().cloned();
        match self.resting_sender.try_send(RestingJob {
            client,
            target,
            strength: desired,
        }) {
            Ok(()) => {
                self.resting_in_flight = true;
                self.resting_last_sent = Some(now);
            }
            Err(_) => {
                self.resting_last_sent = Some(now);
            }
        }
    }

    fn handle_respawn(&mut self, respawn: &LocalPlayerRespawn) {
        log::info!(
            target: "companion::app",
            "respawn_received session_id={:?} client_time_ms={} awaiting_respawn={}",
            respawn.session_id,
            respawn.client_time_ms,
            self.awaiting_respawn
        );
        // Only a "hold until respawn" death effect ends on this signal. A death
        // effect running its own configured duration (or any other effect) is
        // left to finish as the user set it up.
        if !self.awaiting_respawn {
            return;
        }
        // A "hold until respawn" death effect was sent to the toy as a single
        // timed command capped at RESPAWN_HOLD_SAFETY_CAP_SECS, since the toy
        // has no way to know when the player actually respawns. Respawning
        // sooner than that only updates this app's own bookkeeping - it does
        // not by itself reach the toy, which would otherwise keep running the
        // original command for the rest of its cap. Force a stop so respawning
        // actually silences it.
        self.force_stop_toy("respawn_ends_hold");
    }

    /// Sends an explicit stop straight to the connected device and clears
    /// whatever this app thought was active, regardless of the resting
    /// baseline heuristic that otherwise stays hands-off when no baseline is
    /// configured (that heuristic assumes nothing the app started is still
    /// running - not true here, since the app knows a hold-until-respawn
    /// effect or a user-requested stop needs to end right now). A resting
    /// baseline, if the user has one configured, reasserts itself on its own
    /// cadence afterward as usual.
    fn force_stop_toy(&mut self, reason: &'static str) {
        self.active_action = None;
        self.awaiting_respawn = false;
        let Some(client) = self.client.clone() else {
            return;
        };
        let target = self.selected_device().cloned();
        log::info!(
            target: "companion::app",
            "toy_force_stopped reason={reason} provider={PROVIDER_LABEL} target={:?}",
            target.as_ref().map(ProviderTarget::id)
        );
        match self.resting_sender.try_send(RestingJob {
            client,
            target,
            strength: 0,
        }) {
            Ok(()) => {
                self.resting_in_flight = true;
                self.resting_last_sent = Some(Instant::now());
            }
            Err(_) => {
                log::warn!(
                    target: "companion::app",
                    "toy_force_stop_send_failed reason={reason} outcome=queue_unavailable"
                );
            }
        }
    }

    fn trigger_is_new(&mut self, trigger: &TriggerIdentity) -> bool {
        // Intensity pulses are self-throttled and reuse the damage_taken
        // sequence numbers, so they sit outside the monotonic novelty gate
        // (and must not advance it, or the real damage_taken trigger on the
        // same event would be swallowed).
        if trigger.override_action.is_some() {
            return true;
        }
        if let Some((session_id, sequence)) = &self.last_sequence
            && session_id == &trigger.session_id
            && trigger.sequence <= *sequence
        {
            log::debug!(
                target: "companion::app",
                "action_skipped outcome=skipped error_kind=duplicate_or_out_of_order trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} session={} sequence={}",
                trigger.kind.label(),
                self.selected_device().map(ProviderTarget::id),
                trigger.session_id,
                trigger.sequence
            );
            return false;
        }
        self.last_sequence = Some((trigger.session_id.clone(), trigger.sequence));
        true
    }

    fn apply_action_enqueue_result(&mut self, request: ActionRequest, result: ActionEnqueueResult) {
        let trigger = &request.trigger;
        match result {
            ActionEnqueueResult::Accepted => {
                self.action_status = Some(ActionStatus::Sending(request.clone()));
                self.action_in_flight = self.action_in_flight.saturating_add(1);
                log::info!(
                    target: "companion::app",
                    "action_queued outcome=queued error_kind=none trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={}",
                    trigger.kind.label(),
                    request.target.as_ref().map(ProviderTarget::id),
                    request.resolved.summary(),
                    trigger.session_id,
                    trigger.sequence
                );
            }
            ActionEnqueueResult::Full => {
                log::warn!(
                    target: "companion::app",
                    "action_skipped outcome=skipped error_kind=none trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} reason=queue_capacity",
                    trigger.kind.label(),
                    request.target.as_ref().map(ProviderTarget::id),
                    request.resolved.summary(),
                    trigger.session_id,
                    trigger.sequence
                );
                self.action_status = Some(ActionStatus::Skipped {
                    snapshot: ActionSnapshot::from_request(&request),
                    reason: "action queue is full".to_owned(),
                });
            }
            ActionEnqueueResult::Disconnected => {
                log::error!(
                    target: "companion::app",
                    "action_failed outcome=failed trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} reason=worker_unavailable error_kind=worker_unavailable",
                    trigger.kind.label(),
                    request.target.as_ref().map(ProviderTarget::id),
                    request.resolved.summary(),
                    trigger.session_id,
                    trigger.sequence
                );
                self.action_status = Some(ActionStatus::Failed {
                    request,
                    error: "action worker is unavailable".to_owned(),
                });
            }
        }
    }

    fn queue_trigger_action(&mut self, trigger: TriggerIdentity) {
        if !self.trigger_is_new(&trigger) {
            return;
        }
        let settings = self.triggers.get(trigger.kind);
        if !settings.enabled {
            log::info!(
                target: "companion::app",
                "trigger_disabled trigger={} session_id={:?} sequence={} ability_slot={:?} detection={:?}",
                trigger.kind.label(),
                trigger.session_id,
                trigger.sequence,
                trigger.ability_slot,
                trigger.detection
            );
            return;
        }
        if let (Some(filter), Some(ability_slot)) = (
            self.triggers.ability_filter(trigger.kind),
            trigger.ability_slot,
        ) && !filter.accepts(ability_slot)
        {
            log::info!(
                target: "companion::app",
                "trigger_filtered reason=ability_not_selected trigger={} session_id={:?} sequence={} ability_slot={} detection={:?}",
                trigger.kind.label(),
                trigger.session_id,
                trigger.sequence,
                ability_slot,
                trigger.detection
            );
            return;
        }

        if self.awaiting_respawn
            && self.triggers.suppress_triggers_while_dead
            && matches!(
                trigger.kind,
                TriggerKind::AbilityUse
                    | TriggerKind::AbilityCooldownReady
                    | TriggerKind::Assist
                    | TriggerKind::AllyHealed
                    | TriggerKind::AllyShielded
                    | TriggerKind::SoulDeny
                    | TriggerKind::SoulSecure
                    | TriggerKind::ParrySuccess
                    | TriggerKind::ParryFail
                    | TriggerKind::DamageTakenIntensity
            )
        {
            log::info!(
                target: "companion::app",
                "action_skipped outcome=skipped error_kind=none trigger={} provider={PROVIDER_LABEL} session={} sequence={} reason=awaiting_respawn",
                trigger.kind.label(),
                trigger.session_id,
                trigger.sequence
            );
            let target = self.selected_device().cloned();
            self.action_status = Some(ActionStatus::Skipped {
                snapshot: ActionSnapshot {
                    target,
                    resolved: None,
                    trigger,
                },
                reason: "ignored while you're dead".to_owned(),
            });
            return;
        }

        let target = self.selected_device().cloned();
        let resolved = match trigger.override_action {
            Some(resolved) => resolved,
            None => match settings.actions.resolve_checked() {
                Ok(resolved) => resolved,
                Err(error) => {
                    log::warn!(
                        target: "companion::app",
                        "action_skipped outcome=skipped trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} session={} sequence={} reason=invalid_settings error_kind=invalid_settings validation={}",
                        trigger.kind.label(),
                        target.as_ref().map(ProviderTarget::id),
                        trigger.session_id,
                        trigger.sequence,
                        error
                    );
                    self.action_status = Some(ActionStatus::Skipped {
                        snapshot: ActionSnapshot {
                            target,
                            resolved: None,
                            trigger,
                        },
                        reason: format!("invalid action settings: {error}"),
                    });
                    return;
                }
            },
        };
        let hold_until_respawn =
            trigger.kind == TriggerKind::Death && self.triggers.death_hold_until_respawn;
        let resolved = resolved_for_trigger(trigger.kind, hold_until_respawn, resolved);
        let now = Instant::now();
        let priority_rank = self.triggers.priority_rank(trigger.kind);
        if let Some(active) = self.active_action
            && active.ends_at > now
            && priority_rank > active.priority_rank
        {
            log::info!(
                target: "companion::app",
                "action_skipped outcome=skipped error_kind=none trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} reason=lower_priority",
                trigger.kind.label(),
                target.as_ref().map(ProviderTarget::id),
                resolved.summary(),
                trigger.session_id,
                trigger.sequence
            );
            self.action_status = Some(ActionStatus::Skipped {
                snapshot: ActionSnapshot {
                    target,
                    resolved: Some(resolved),
                    trigger,
                },
                reason: "a higher-priority effect is still playing".to_owned(),
            });
            return;
        }
        let request = ActionRequest {
            target,
            resolved,
            trigger,
            queued_at: now,
        };
        let Some(client) = self.client.clone() else {
            log::warn!(
                target: "companion::app",
                "action_skipped outcome=skipped trigger={} provider={PROVIDER_LABEL} target={:?} action_kind={ACTION_KIND_LABEL} action_summary={:?} session={} sequence={} reason=provider_not_connected error_kind=not_connected",
                request.trigger.kind.label(),
                request.target.as_ref().map(ProviderTarget::id),
                request.resolved.summary(),
                request.trigger.session_id,
                request.trigger.sequence
            );
            self.action_status = Some(ActionStatus::Skipped {
                snapshot: ActionSnapshot::from_request(&request),
                reason: "provider is not connected".to_owned(),
            });
            return;
        };
        let enqueue_result = match self.action_sender.try_send(ActionJob {
            client,
            request: request.clone(),
        }) {
            Ok(()) => ActionEnqueueResult::Accepted,
            Err(TrySendError::Full(_job)) => ActionEnqueueResult::Full,
            Err(TrySendError::Disconnected(_job)) => ActionEnqueueResult::Disconnected,
        };
        if enqueue_result == ActionEnqueueResult::Accepted {
            self.active_action = Some(ActiveAction {
                priority_rank,
                ends_at: now + Duration::from_secs_f32(request.resolved.duration_secs),
            });
            if hold_until_respawn {
                self.awaiting_respawn = true;
            }
            // Re-assert the resting baseline once this effect's window closes,
            // instead of leaving the toy silent after it.
            self.resting_applied = None;
            self.resting_last_sent = None;
        }
        self.apply_action_enqueue_result(request, enqueue_result);
    }

    /// Folds one damage/healing report into its trigger's rolling window and
    /// returns a fireable [`TriggerIdentity`] once the configured threshold
    /// is cleared. The window is drained on a fire so a single sustained
    /// fight does not immediately re-trigger on the next tick.
    fn accumulate_amount_trigger(
        &mut self,
        kind: TriggerKind,
        vitals: VitalsTrigger,
    ) -> Option<TriggerIdentity> {
        let settings = self.triggers.amount_settings(kind)?;
        if !settings.trigger.enabled {
            return None;
        }
        let threshold = settings.threshold;
        let window_seconds = settings.window_seconds;
        let ledger = match kind {
            TriggerKind::DamageTaken => &mut self.damage_ledger,
            TriggerKind::HealingReceived => &mut self.healing_ledger,
            TriggerKind::DamageGiven => &mut self.damage_given_ledger,
            _ => return None,
        };
        let total = ledger.record(vitals.amount as f32, window_seconds, Instant::now());
        if total < threshold {
            return None;
        }
        ledger.reset();
        Some(TriggerIdentity::from_amount(kind, &vitals, total))
    }

    /// The minimum gap between intensity pulses while damage keeps landing, so
    /// a sustained fight ramps rather than machine-guns the toy.
    const INTENSITY_PULSE_GAP: Duration = Duration::from_millis(450);

    /// The Damage Taken effect: folds one damage report into the rolling
    /// window and, if the intensity curve puts the windowed total at level 1
    /// or more, queues a pulse at that level (throttled by
    /// [`Self::INTENSITY_PULSE_GAP`] so a sustained beating ramps rather than
    /// machine-guns the toy).
    fn evaluate_damage_intensity(&mut self, vitals: &VitalsTrigger) {
        if !self.triggers.damage_taken.trigger.enabled {
            return;
        }
        let curve = self.triggers.damage_taken_curve.clone();
        let now = Instant::now();
        let total = self
            .damage_intensity_ledger
            .record(vitals.amount as f32, curve.window_seconds, now);
        let level = curve.level_for(total);
        if level < 1 {
            return;
        }
        if let Some(last) = self.damage_intensity_last_pulse
            && now.duration_since(last) < Self::INTENSITY_PULSE_GAP
        {
            return;
        }
        self.damage_intensity_last_pulse = Some(now);
        let duration_secs = portable_vibrate_duration(curve.pulse_seconds).unwrap_or(1.0);
        let mut trigger = TriggerIdentity::from_amount(TriggerKind::DamageTaken, vitals, total);
        trigger.detection = format!("damage_window:{total:.0} level:{level}");
        trigger.override_action = Some(ResolvedVibrateAction {
            strength: level,
            duration_secs,
        });
        self.queue_trigger_action(trigger);
    }

    fn ensure_bridge_subscription(&mut self) {
        if self.bridge_events.is_none() {
            log::debug!(target: "companion::app", "bridge_subscription_created");
            self.bridge_events = Some(self.bridge_listener.subscribe());
        }
    }
    fn start_log_listener(&mut self, path: PathBuf) -> std::io::Result<()> {
        self.ensure_bridge_subscription();
        self.bridge_listener.start(path)
    }

    fn start_listener_at(
        &mut self,
        path: PathBuf,
        context: LogListenerStartContext,
    ) -> std::io::Result<()> {
        self.listener_action_error = None;
        log::info!(
            target: "companion::app",
            "log_listener_start_requested context={} path={:?}",
            context.label(),
            path
        );
        let result = self.start_log_listener(path.clone());
        match &result {
            Ok(()) => log::info!(
                target: "companion::app",
                "log_listener_started context={} path={:?}",
                context.label(),
                path
            ),
            Err(error) => {
                log::warn!(
                    target: "companion::app",
                    "log_listener_start_failed context={} path={:?} error={:?}",
                    context.label(),
                    path,
                    error
                );
                self.listener_action_error = Some(format!("Could not start listener: {error}"));
            }
        }
        result
    }
    fn poll_bridge_events(&mut self) {
        while let Some(result) = self.bridge_events.as_ref().map(Receiver::try_recv) {
            match result {
                Ok(event) => {
                    match &event {
                        BridgeEvent::HookReady(ready) => log::info!(
                            target: "companion::app",
                            "bridge_hook_ready session_id={:?} client_time_ms={} poll_interval_ms={}",
                            ready.session_id,
                            ready.client_time_ms,
                            ready.poll_interval_ms
                        ),
                        BridgeEvent::AbilityCatalog(catalog) => log::info!(
                            target: "companion::app",
                            "bridge_ability_catalog session_id={:?} client_time_ms={} abilities={}",
                            catalog.session_id,
                            catalog.client_time_ms,
                            catalog.abilities.len()
                        ),
                        BridgeEvent::LocalPlayerDeath(death) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=death session_id={:?} sequence={} client_time_ms={} detection={:?}",
                            death.session_id,
                            death.sequence,
                            death.client_time_ms,
                            death.detection
                        ),
                        BridgeEvent::LocalPlayerRespawn(respawn) => log::info!(
                            target: "companion::app",
                            "bridge_respawn_received session_id={:?} client_time_ms={}",
                            respawn.session_id,
                            respawn.client_time_ms
                        ),
                        BridgeEvent::LocalPlayerKill(kill) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=kill session_id={:?} sequence={} client_time_ms={} detection={:?} count_before={:?} count_after={:?}",
                            kill.session_id,
                            kill.sequence,
                            kill.client_time_ms,
                            kill.detection,
                            kill.count_before,
                            kill.count_after
                        ),
                        BridgeEvent::LocalPlayerAssist(assist) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=assist session_id={:?} sequence={} client_time_ms={} detection={:?} count_before={:?} count_after={:?}",
                            assist.session_id,
                            assist.sequence,
                            assist.client_time_ms,
                            assist.detection,
                            assist.count_before,
                            assist.count_after
                        ),
                        BridgeEvent::AllyHealed(count) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=ally_healed session_id={:?} sequence={} client_time_ms={} detection={:?}",
                            count.session_id,
                            count.sequence,
                            count.client_time_ms,
                            count.detection
                        ),
                        BridgeEvent::AllyShielded(count) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=ally_shielded session_id={:?} sequence={} client_time_ms={} detection={:?}",
                            count.session_id,
                            count.sequence,
                            count.client_time_ms,
                            count.detection
                        ),
                        BridgeEvent::AbilityUsed(ability) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=ability_use session_id={:?} sequence={} client_time_ms={} ability_slot={} detection={:?} charges_before={:?} charges_after={:?}",
                            ability.session_id,
                            ability.sequence,
                            ability.client_time_ms,
                            ability.ability_slot,
                            ability.detection,
                            ability.charges_before,
                            ability.charges_after
                        ),
                        BridgeEvent::AbilityCooldownReady(ability) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger=ability_cooldown_ready session_id={:?} sequence={} client_time_ms={} ability_slot={} detection={:?} charges_before={:?} charges_after={:?}",
                            ability.session_id,
                            ability.sequence,
                            ability.client_time_ms,
                            ability.ability_slot,
                            ability.detection,
                            ability.charges_before,
                            ability.charges_after
                        ),
                        BridgeEvent::DamageTaken(vitals) => log::info!(
                            target: "companion::app",
                            "bridge_amount_received trigger=damage_taken session_id={:?} sequence={} client_time_ms={} detection={:?} amount={} health_after={:?}",
                            vitals.session_id,
                            vitals.sequence,
                            vitals.client_time_ms,
                            vitals.detection,
                            vitals.amount,
                            vitals.health_after
                        ),
                        BridgeEvent::HealingReceived(vitals) => log::info!(
                            target: "companion::app",
                            "bridge_amount_received trigger=healing_received session_id={:?} sequence={} client_time_ms={} detection={:?} amount={} health_after={:?}",
                            vitals.session_id,
                            vitals.sequence,
                            vitals.client_time_ms,
                            vitals.detection,
                            vitals.amount,
                            vitals.health_after
                        ),
                        BridgeEvent::DamageGiven(vitals) => log::info!(
                            target: "companion::app",
                            "bridge_amount_received trigger=damage_given session_id={:?} sequence={} client_time_ms={} detection={:?} amount={} health_after={:?}",
                            vitals.session_id,
                            vitals.sequence,
                            vitals.client_time_ms,
                            vitals.detection,
                            vitals.amount,
                            vitals.health_after
                        ),
                        BridgeEvent::SoulDeny(count)
                        | BridgeEvent::SoulSecure(count)
                        | BridgeEvent::ParrySuccess(count)
                        | BridgeEvent::ParryFail(count)
                        | BridgeEvent::ObjectiveGuardian(count)
                        | BridgeEvent::ObjectiveWalker(count)
                        | BridgeEvent::ObjectiveBaseGuardian(count)
                        | BridgeEvent::ObjectiveShrine(count)
                        | BridgeEvent::ObjectivePatronWeakened(count)
                        | BridgeEvent::GameWon(count)
                        | BridgeEvent::DamageTakenIntensity(count) => log::info!(
                            target: "companion::app",
                            "bridge_trigger_received trigger={} session_id={:?} sequence={} client_time_ms={} detection={:?}",
                            event.event_name(),
                            count.session_id,
                            count.sequence,
                            count.client_time_ms,
                            count.detection
                        ),
                    }
                    self.last_bridge_event = Some(event.clone());
                    let trigger = match event {
                        BridgeEvent::HookReady(_) => {
                            self.ability_catalog.clear();
                            self.damage_ledger.reset();
                            self.healing_ledger.reset();
                            self.damage_given_ledger.reset();
                            self.damage_intensity_ledger.reset();
                            self.damage_intensity_last_pulse = None;
                            None
                        }
                        BridgeEvent::AbilityCatalog(catalog) => {
                            self.replace_ability_catalog(catalog);
                            None
                        }
                        BridgeEvent::LocalPlayerDeath(death) => {
                            Some(TriggerIdentity::from_death(death))
                        }
                        BridgeEvent::LocalPlayerRespawn(respawn) => {
                            self.handle_respawn(&respawn);
                            None
                        }
                        BridgeEvent::LocalPlayerKill(kill) => {
                            Some(TriggerIdentity::from_count(TriggerKind::Kill, kill))
                        }
                        BridgeEvent::LocalPlayerAssist(assist) => {
                            Some(TriggerIdentity::from_count(TriggerKind::Assist, assist))
                        }
                        BridgeEvent::AllyHealed(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::AllyHealed, count))
                        }
                        BridgeEvent::AllyShielded(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::AllyShielded, count))
                        }
                        BridgeEvent::AbilityUsed(ability) => Some(TriggerIdentity::from_ability(
                            TriggerKind::AbilityUse,
                            ability,
                        )),
                        BridgeEvent::AbilityCooldownReady(ability) => {
                            Some(TriggerIdentity::from_ability(
                                TriggerKind::AbilityCooldownReady,
                                ability,
                            ))
                        }
                        BridgeEvent::DamageTaken(vitals) => {
                            // Damage Taken is now the intensity curve; it queues
                            // its own pulse and returns nothing to the generic
                            // dispatch below.
                            self.evaluate_damage_intensity(&vitals);
                            None
                        }
                        BridgeEvent::HealingReceived(vitals) => {
                            self.accumulate_amount_trigger(TriggerKind::HealingReceived, vitals)
                        }
                        BridgeEvent::DamageGiven(vitals) => {
                            self.accumulate_amount_trigger(TriggerKind::DamageGiven, vitals)
                        }
                        BridgeEvent::SoulDeny(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::SoulDeny, count))
                        }
                        BridgeEvent::SoulSecure(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::SoulSecure, count))
                        }
                        BridgeEvent::ParrySuccess(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::ParrySuccess, count))
                        }
                        BridgeEvent::ParryFail(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::ParryFail, count))
                        }
                        BridgeEvent::ObjectiveGuardian(count) => Some(TriggerIdentity::from_count(
                            TriggerKind::ObjectiveGuardian,
                            count,
                        )),
                        BridgeEvent::ObjectiveWalker(count) => Some(TriggerIdentity::from_count(
                            TriggerKind::ObjectiveWalker,
                            count,
                        )),
                        BridgeEvent::ObjectiveBaseGuardian(count) => Some(
                            TriggerIdentity::from_count(TriggerKind::ObjectiveBaseGuardian, count),
                        ),
                        BridgeEvent::ObjectiveShrine(count) => Some(TriggerIdentity::from_count(
                            TriggerKind::ObjectiveShrine,
                            count,
                        )),
                        BridgeEvent::ObjectivePatronWeakened(count) => Some(
                            TriggerIdentity::from_count(TriggerKind::ObjectivePatronWeakened, count),
                        ),
                        BridgeEvent::GameWon(count) => {
                            Some(TriggerIdentity::from_count(TriggerKind::GameWon, count))
                        }
                        // The mod's health-band signal is ignored; intensity is
                        // now computed from the damage-taken amount stream and
                        // its user-set curve (see evaluate_damage_intensity).
                        BridgeEvent::DamageTakenIntensity(_) => None,
                    };
                    if let Some(trigger) = trigger {
                        self.queue_trigger_action(trigger);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::warn!(
                        target: "companion::app",
                        "bridge_subscription_failed reason=channel_closed"
                    );
                    self.bridge_events = None;
                    break;
                }
            }
        }
    }

    fn start_listener_from_input(&mut self) {
        self.start_configured_listener(LogListenerStartContext::Manual);
    }

    fn start_configured_listener(&mut self, context: LogListenerStartContext) -> bool {
        let trimmed_path = self.log_path.trim().to_owned();
        if trimmed_path.is_empty() {
            log::warn!(
                target: "companion::app",
                "log_listener_start_skipped context={} reason=empty_path",
                context.label()
            );
            self.listener_action_error =
                Some("Enter a console.log path before starting the listener.".to_owned());
            return false;
        }
        let path = PathBuf::from(trimmed_path);
        self.start_listener_at(path, context).is_ok()
    }

    fn initialize_log_listener<F>(&mut self, detector: F)
    where
        F: FnOnce() -> Result<Detection, DetectionError>,
    {
        let trimmed_path = self.log_path.trim().to_owned();
        if !trimmed_path.is_empty() {
            self.log_path = trimmed_path;
            log::info!(
                target: "companion::app",
                "log_listener_saved_path_selected context=startup path={:?}",
                self.log_path
            );
            self.start_configured_listener(LogListenerStartContext::Startup);
            return;
        }

        self.bridge_listener.stop();
        self.listener_action_error = None;
        log::info!(
            target: "companion::app",
            "log_path_auto_detection_started context=startup"
        );
        self.apply_log_detection_with_context(detector(), LogListenerStartContext::Startup);
    }

    fn auto_detect_log_path(&mut self) {
        self.listener_action_error = None;
        log::info!(
            target: "companion::app",
            "log_path_auto_detection_started context=manual"
        );
        self.apply_log_detection(deadlock_path::detect());
    }

    fn apply_log_detection(&mut self, result: Result<Detection, DetectionError>) {
        self.apply_log_detection_with_context(result, LogListenerStartContext::Manual);
    }

    fn apply_log_detection_with_context(
        &mut self,
        result: Result<Detection, DetectionError>,
        context: LogListenerStartContext,
    ) {
        match result {
            Ok(Detection::Ready { path }) => {
                log::info!(
                    target: "companion::app",
                    "log_path_auto_detection_found context={} path={:?}",
                    context.label(),
                    path
                );
                self.log_path = path.display().to_string();
                self.log_detection_status = match self.start_listener_at(path, context) {
                    Ok(()) => Some(LogDetectionStatus::Found),
                    Err(error) => Some(LogDetectionStatus::Failed(format!(
                        "Deadlock console.log was found, but the listener could not start: {error}"
                    ))),
                };
            }
            Ok(Detection::NotCreated { path }) => {
                log::info!(
                    target: "companion::app",
                    "log_path_auto_detection_not_created context={} path={:?}",
                    context.label(),
                    path
                );
                self.log_path = path.display().to_string();
                self.log_detection_status = match self.start_listener_at(path, context) {
                    Ok(()) => Some(LogDetectionStatus::NotCreated),
                    Err(error) => Some(LogDetectionStatus::Failed(format!(
                        "Deadlock was found, but the listener could not start: {error}"
                    ))),
                };
            }
            Err(error) => {
                log::warn!(
                    target: "companion::app",
                    "log_path_auto_detection_failed context={} error={:?}",
                    context.label(),
                    error
                );
                if context == LogListenerStartContext::Startup {
                    self.log_path.clear();
                    self.bridge_listener.stop();
                }
                self.log_detection_status = Some(LogDetectionStatus::Failed(format!(
                    "Auto-detect failed: {error}"
                )));
            }
        }
    }

    pub fn draw(&mut self, ui: &mut Ui) {
        self.poll_test_action();
        self.poll_connection_test();
        self.poll_device_refresh();
        self.poll_action();
        self.poll_resting();
        self.poll_bridge_events();
        self.maintain_resting();
        let busy = self.is_busy();

        ui.vertical_centered(|ui| {
        ui.horizontal(|ui| {
            for section in [
                AppSection::Setup,
                AppSection::Effects,
                AppSection::GameConnection,
                AppSection::Donate,
            ] {
                let selected = self.selected_section == section;
                ui.vertical(|ui| {
                    let wing_top = ui.cursor().top();
                    ui.add_space(8.0);
                    let button = egui::Button::new(crate::theme::heading_text(section.label(), 16.0))
                        .fill(if selected {
                            crate::theme::ACCENT_DIM
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            egui::Stroke::new(1.0, crate::theme::ACCENT)
                        } else {
                            egui::Stroke::NONE
                        });
                    let response = ui.add(button);
                    if selected {
                        let center_x = response.rect.center().x;
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(center_x, wing_top),
                                egui::pos2(center_x - 7.0, wing_top + 8.0),
                                egui::pos2(center_x + 7.0, wing_top + 8.0),
                            ],
                            crate::theme::ACCENT,
                            egui::Stroke::NONE,
                        ));
                    }
                    if response.clicked() {
                        self.selected_section = section;
                    }
                });
                ui.add_space(6.0);
            }
        });
        });
        ui.add_space(10.0);

        // The Effects tab lays out its own scroll regions (the trigger list and
        // the editor each scroll independently); every other tab is a single
        // scrolling column. Both are held to a comfortable centered measure so
        // nothing sprawls edge to edge on a wide window.
        if self.selected_section == AppSection::Effects {
            Self::centered_body(ui, EFFECTS_CONTENT_MAX_WIDTH, |ui| {
                self.draw_effects(ui, busy);
            });
        } else {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Self::centered_body(ui, COLUMN_CONTENT_MAX_WIDTH, |ui| {
                        match self.selected_section {
                            AppSection::Setup => self.draw_setup(ui, busy),
                            AppSection::GameConnection => self.draw_game_connection(ui),
                            AppSection::Donate => Self::draw_donate(ui),
                            AppSection::Effects => {}
                        }
                    });
                });
        }

        let listener_status = self.bridge_listener.status();
        let resting_desired = self.resting_strength.min(20);
        let resting_pending = self.client.is_some()
            && (self.resting_in_flight
                || !resting_at_rest(resting_desired, self.resting_applied));
        if listener_status.phase != ListenerPhase::Stopped
            || self.action_in_progress()
            || resting_pending
        {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Runs `add` inside a horizontally centered column no wider than
    /// `max_width`, keeping the full available height so any scroll regions
    /// inside still get room to lay out. Content stays at a comfortable
    /// centered measure rather than stretched across a wide window; on a
    /// narrow window it is an effective passthrough.
    fn centered_body(ui: &mut Ui, max_width: f32, add: impl FnOnce(&mut Ui)) {
        let target = ui.available_width().min(max_width);
        if ui.available_width() - target < 1.0 {
            add(ui);
            return;
        }
        let height = ui.available_height().max(1.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(target, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(target);
                    ui.set_max_width(target);
                    add(ui);
                },
            );
        });
    }

    fn draw_setup(&mut self, ui: &mut Ui, busy: bool) {
        self.draw_connection_card(ui, busy);
        ui.add_space(10.0);
        self.draw_toy_card(ui, busy);
    }

    fn draw_connection_card(&mut self, ui: &mut Ui, busy: bool) {
        crate::theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::theme::colored_text_nudged_down(
                    ui,
                    egui_phosphor::regular::HEART,
                    crate::theme::ACCENT,
                    4.0,
                );
                ui.label(crate::theme::heading_text("Lovense connection", 19.0));
            });
            ui.add_space(4.0);
            ui.label(
                "This connects the companion to the Lovense Remote app on your PC, which talks to your toy. Most people won't need to change any settings here.",
            );
            ui.add_space(14.0);

            ui.label(egui::RichText::new("1. Open the Lovense Remote app").strong());
            ui.small("Make sure your toy already shows as connected in it.");
            ui.add_space(10.0);
            ui.label(egui::RichText::new("2. Turn on Game Mode").strong());
            ui.small(
                "Find it in the Remote app's menu. This lets other apps, like this one, send vibration commands to your toy.",
            );
            ui.add_space(10.0);
            ui.label(egui::RichText::new("3. Test the connection").strong());
            ui.small("Click the button below. You're done once it says \u{201c}Connected\u{201d}.");
            ui.add_space(14.0);

            let can_test = self.credentials_present() && !busy;
            if ui
                .add_enabled(
                    can_test,
                    egui::Button::new(egui::RichText::new("Test connection").size(15.0))
                        .min_size(egui::vec2(ui.available_width(), 34.0)),
                )
                .clicked()
            {
                self.start_connection_test(ui.ctx().clone());
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                crate::theme::badge(ui, self.credential_state.label(), self.credential_state.tone());
                if let Some(error) = &self.connection_error {
                    crate::theme::badge(ui, error, crate::theme::BadgeTone::Danger);
                }
            });

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(8.0);
            let mut credentials_changed = false;
            egui::CollapsingHeader::new("Advanced: custom domain / port")
                .id_salt("lovense-advanced")
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.small(
                        "Only needed if your toy connects through Lovense Remote on your phone instead of this PC. The app's Game Mode screen shows the domain and port to enter here.",
                    );
                    ui.add_space(8.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        credentials_changed |= text_input(
                            ui,
                            "Domain",
                            &mut self.provider_settings.lovense.domain,
                            false,
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("HTTP port");
                            credentials_changed |= ui
                                .add(egui::DragValue::new(
                                    &mut self.provider_settings.lovense.http_port,
                                ))
                                .changed();
                        });
                    });
                });
            if credentials_changed {
                self.reset_connection();
            }
        });
    }

    fn draw_toy_card(&mut self, ui: &mut Ui, busy: bool) {
        crate::theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::theme::colored_text_nudged_down(
                    ui,
                    egui_phosphor::regular::VIBRATE,
                    crate::theme::ACCENT,
                    4.0,
                );
                ui.label(crate::theme::heading_text("Toy", 19.0));
            });
            ui.add_space(4.0);
            ui.label("Pick which toy reacts to in-game events, once connected above.");
            ui.add_space(10.0);
            let selection_enabled = !self.devices.is_empty() && !busy;
            let selected_name = self
                .selected_device()
                .map(|device| device.name().to_owned())
                .unwrap_or_else(|| {
                    if self.devices.is_empty() {
                        "No toys found yet".to_owned()
                    } else {
                        "All connected toys".to_owned()
                    }
                });
            let mut selection_changed = false;
            let mut selected_device = self.selected_device.clone();
            ui.horizontal(|ui| {
                let can_refresh = self.client.is_some() && !busy;
                let refresh_width = 34.0;
                let spacing = ui.spacing().item_spacing.x;
                ui.add_enabled_ui(selection_enabled, |ui| {
                    egui::ComboBox::from_id_salt("device")
                        .selected_text(selected_name.as_str())
                        .width(ui.available_width() - refresh_width - spacing)
                        .show_ui(ui, |ui| {
                            for device in &self.devices {
                                selection_changed |= ui
                                    .selectable_value(
                                        &mut selected_device,
                                        Some(device.id().clone()),
                                        device.name(),
                                    )
                                    .changed();
                            }
                        });
                });
                if ui
                    .add_enabled(
                        can_refresh,
                        egui::Button::new(egui_phosphor::regular::ARROWS_CLOCKWISE)
                            .min_size(egui::vec2(refresh_width, 0.0)),
                    )
                    .on_hover_text("Refresh the toy list")
                    .clicked()
                {
                    self.start_device_refresh(ui.ctx().clone());
                }
            });
            if selection_changed && let Some(target) = selected_device {
                self.select_device(target);
            }
            ui.add_space(4.0);
            if self.devices.is_empty() {
                ui.small("Just connected your toy? Click the refresh button above. The list only updates when asked.");
            } else {
                ui.small("Leave it on \u{201c}All connected toys\u{201d} unless you specifically want just one to react.");
            }
            if self.devices.is_empty() {
                if let Some(error) = &self.connection_error {
                    ui.add_space(4.0);
                    crate::theme::badge(ui, error, crate::theme::BadgeTone::Danger);
                } else {
                    ui.add_space(4.0);
                    ui.small("Still nothing after refreshing? Double-check \u{201c}Game Mode\u{201d} is toggled on in the Lovense Remote app. A toy can show connected there without being visible to Game Mode.");
                }
            }
            ui.add_space(10.0);
            let can_test = self.client.is_some() && !busy;
            if ui
                .add_enabled(
                    can_test,
                    egui::Button::new(egui::RichText::new("Send test vibration").size(15.0))
                        .min_size(egui::vec2(ui.available_width(), 34.0)),
                )
                .clicked()
            {
                self.start_test_action(ui.ctx().clone());
            }
            if let Some(status) = &self.test_action_status {
                ui.add_space(8.0);
                crate::theme::badge(ui, status.label(), status.tone());
            }
        });
    }

    fn draw_effects(&mut self, ui: &mut Ui, busy: bool) {
        self.draw_profile_card(ui, busy);
        ui.add_space(10.0);
        // The profile card stays put; the trigger list and the editor each get
        // their own vertical scroll so a long list of triggers never pushes the
        // whole page.
        ui.columns(2, |columns| {
            egui::ScrollArea::vertical()
                .id_salt("effects-trigger-list-scroll")
                .auto_shrink([false, false])
                .show(&mut columns[0], |ui| {
                    self.draw_trigger_list(ui, busy);
                });
            egui::ScrollArea::vertical()
                .id_salt("effects-editor-scroll")
                .auto_shrink([false, false])
                .show(&mut columns[1], |ui| {
                    self.draw_effect_editor(ui, busy);
                });
        });
    }

    /// The profile switcher: one card with a chip per profile (click to
    /// switch, right-click to duplicate / rename / delete) and a New button.
    /// Everything below it in the Effects tab belongs to the selected profile;
    /// the connection and device are shared across all of them.
    fn draw_profile_card(&mut self, ui: &mut Ui, busy: bool) {
        #[derive(Clone, Copy)]
        enum ProfileAction {
            Switch(usize),
            Duplicate(usize),
            Rename(usize),
            Delete(usize),
            New,
        }
        let mut action: Option<ProfileAction> = None;
        let active = self.active_profile;
        let count = self.profiles.len();

        crate::theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::theme::colored_text_nudged_down(
                    ui,
                    egui_phosphor::regular::HEART,
                    crate::theme::ACCENT,
                    4.0,
                );
                ui.label(crate::theme::heading_text("Profiles", 19.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui
                            .button(format!("{}  New", egui_phosphor::regular::PLUS))
                            .on_hover_text("Add a fresh profile (death effect on, everything else off)")
                            .clicked()
                        {
                            action = Some(ProfileAction::New);
                        }
                    });
                });
            });

            ui.add_space(2.0);
            ui.small(
                "Each profile is its own set of triggers, priority order and resting level. Click a profile to switch; right-click to duplicate, rename or delete it. Your connection and device are shared.",
            );
            ui.add_space(10.0);

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for index in 0..count {
                    let selected = index == active;

                    if self.renaming_profile == Some(index) {
                        let response = ui.scope(|ui| {
                            let visuals = ui.visuals_mut();
                            visuals.override_text_color = Some(crate::theme::TEXT);
                            // The name is fully selected the moment the field
                            // opens, so the selection colour has to sit under
                            // TEXT-coloured glyphs. ACCENT is too light for
                            // that; ACCENT_DIM is the same dark pairing the
                            // selected-profile chip uses.
                            visuals.selection.bg_fill = crate::theme::ACCENT_DIM;
                            let name = &mut self.profiles[index].name;
                            let width = (name.len() as f32 * 8.5 + 16.0).clamp(64.0, 220.0);
                            ui.add(
                                egui::TextEdit::singleline(name)
                                    .desired_width(width)
                                    .margin(egui::Margin::symmetric(10, 5)),
                            )
                        });
                        let field = response.inner;
                        if self.renaming_needs_focus {
                            field.request_focus();
                            // Select the whole placeholder so the first
                            // keystroke replaces it instead of appending.
                            if let Some(mut state) =
                                egui::TextEdit::load_state(ui.ctx(), field.id)
                            {
                                let end = self.profiles[index].name.chars().count();
                                state.cursor.set_char_range(Some(
                                    egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(0),
                                        egui::text::CCursor::new(end),
                                    ),
                                ));
                                state.store(ui.ctx(), field.id);
                            }
                            self.renaming_needs_focus = false;
                        }
                        if field.lost_focus() {
                            self.commit_profile_rename(index);
                        }
                        continue;
                    }

                    let (fill, text_color, stroke) = if selected {
                        (
                            crate::theme::ACCENT_DIM,
                            crate::theme::TEXT,
                            egui::Stroke::new(1.0, crate::theme::ACCENT),
                        )
                    } else {
                        (
                            egui::Color32::TRANSPARENT,
                            crate::theme::TEXT_DIM,
                            egui::Stroke::new(1.0, crate::theme::STROKE),
                        )
                    };
                    let chip = egui::Button::new(
                        egui::RichText::new(self.profiles[index].name.clone()).color(text_color),
                    )
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(egui::CornerRadius::same(255))
                    .min_size(egui::vec2(0.0, 30.0));
                    let response = ui.add(chip);
                    if response.clicked() {
                        action = Some(ProfileAction::Switch(index));
                    }
                    response.context_menu(|ui| {
                        if ui
                            .button(format!("{}  Duplicate", egui_phosphor::regular::COPY))
                            .clicked()
                        {
                            action = Some(ProfileAction::Duplicate(index));
                            ui.close();
                        }
                        if ui
                            .button(format!(
                                "{}  Rename",
                                egui_phosphor::regular::PENCIL_SIMPLE
                            ))
                            .clicked()
                        {
                            action = Some(ProfileAction::Rename(index));
                            ui.close();
                        }
                        ui.add_enabled_ui(count > 1, |ui| {
                            if ui
                                .button(format!("{}  Delete", egui_phosphor::regular::TRASH))
                                .clicked()
                            {
                                action = Some(ProfileAction::Delete(index));
                                ui.close();
                            }
                        });
                    });
                }
            });
        });

        match action {
            Some(ProfileAction::Switch(index)) => self.switch_profile(index),
            Some(ProfileAction::Duplicate(index)) => self.duplicate_profile(index),
            Some(ProfileAction::Rename(index)) => self.begin_rename_profile(index),
            Some(ProfileAction::Delete(index)) => self.delete_profile(index),
            Some(ProfileAction::New) => self.add_blank_profile(),
            None => {}
        }
    }

    fn draw_trigger_list(&mut self, ui: &mut Ui, busy: bool) {
        ui.label(crate::theme::heading_text("Triggers", 26.0));
        crate::theme::flourish(ui);
        ui.small(format!(
            "Each trigger has its own {ACTION_KIND_LABEL} settings. Drag a card by its grip to set which effect wins when two fire close together. Higher cards keep the toy; lower ones are skipped until it finishes."
        ));
        ui.add_space(6.0);

        // The card list *is* the overlap-priority order: it renders in
        // `priority_order`, only the grip is a drag source, and a drop onto
        // any card rewrites the order. Clicking a card body selects it.
        let order = self.triggers.priority_order.clone();
        let mut drag_from = None;
        let mut drag_to = None;

        for (index, kind) in order.iter().copied().enumerate() {
            let summary = if kind.is_intensity_curve() {
                self.triggers.damage_taken_curve.summary()
            } else {
                self.triggers.get(kind).actions.summary()
            };
            let enabled = self.triggers.get(kind).enabled;
            let selected = self.selected_effect == kind;
            let frame = if selected {
                crate::theme::card_selected(ui)
            } else {
                crate::theme::card(ui)
            };

            let mut configure_clicked = false;
            let mut controls_rect = egui::Rect::NOTHING;
            let card = frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Only the grip starts a reorder, so a press anywhere else
                    // on the card is a plain click (select), not a drag.
                    ui.dnd_drag_source(
                        ui.id().with(("trigger_card_grip", index)),
                        PriorityDragIndex(index),
                        crate::theme::drag_handle,
                    )
                    .response
                    .on_hover_text("Drag to reorder priority");
                    crate::theme::icon_badge(ui, trigger_icon(kind), enabled);
                    // Add controls right-to-left first so the text vertical gets
                    // only the space between the icon and the controls, not the
                    // full remaining card width.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        configure_clicked |= ui.button("Configure").clicked();
                        ui.add_enabled_ui(!busy, |ui| {
                            toggle_button(ui, &mut self.triggers.get_mut(kind).enabled);
                        });
                        // Capture the controls-only rect before adding the text block.
                        controls_rect = ui.min_rect();
                        // Text fills the remaining space to the left of the controls.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                            ui.vertical(|ui| {
                                ui.label(crate::theme::heading_text(trigger_display_label(kind), 18.0));
                                if enabled {
                                    ui.colored_label(
                                        crate::theme::ACCENT_BRIGHT,
                                        format!("{} mode", self.triggers.get(kind).actions.mode.label()),
                                    );
                                }
                                ui.horizontal(|ui| {
                                    ui.colored_label(crate::theme::ACCENT, egui_phosphor::regular::HEART);
                                    ui.small(summary);
                                });
                            });
                        });
                    });
                });
            });
            let card_rect = card.response.rect;

            // A grip being dragged over this card: show where it would land
            // and, on release, record the move to apply after the loop.
            if let (Some(pointer), Some(_hovered)) = (
                ui.input(|i| i.pointer.interact_pos()),
                card.response.dnd_hover_payload::<PriorityDragIndex>(),
            ) {
                let above = pointer.y < card_rect.center().y;
                let stripe_y = if above { card_rect.top() } else { card_rect.bottom() };
                ui.painter().hline(
                    card_rect.x_range(),
                    stripe_y,
                    egui::Stroke::new(2.0, crate::theme::ACCENT),
                );
                if let Some(payload) = card.response.dnd_release_payload::<PriorityDragIndex>() {
                    drag_from = Some(payload.0);
                    drag_to = Some(if above { index } else { index + 1 });
                }
            }

            // The toggle/Configure controls sit inside the card, so the
            // whole-card click sense below is restricted to the area left of
            // them. Otherwise it sits on top in interaction order and
            // swallows clicks meant for the toggle button.
            let mut click_rect = card_rect;
            if controls_rect.is_positive() {
                click_rect.max.x = controls_rect.min.x;
            }
            let card_id = ui.id().with(("trigger_card", trigger_display_label(kind)));
            let card_response = ui.interact(click_rect, card_id, egui::Sense::click());
            if card_response.clicked() {
                configure_clicked = true;
            }
            if card_response.hovered() {
                ui.ctx()
                    .set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if configure_clicked {
                self.select_effect(kind);
            }
            ui.add_space(6.0);
        }

        if let (Some(from), Some(mut to)) = (drag_from, drag_to)
            && from != to
        {
            let mut order = self.triggers.priority_order.clone();
            let moved = order.remove(from);
            if from < to {
                to -= 1;
            }
            order.insert(to.min(order.len()), moved);
            self.triggers.priority_order = order;
        }

        ui.add_space(4.0);
        self.draw_resting_vibration(ui, busy);
        ui.add_space(4.0);
        crate::theme::flourish(ui);
    }

    /// A one-press way to silence the toy immediately, for a trigger effect
    /// that is stuck or has simply run longer than expected (e.g. a "hold
    /// until respawn" death effect). See [`AppState::force_stop_toy`].
    fn draw_emergency_stop(&mut self, ui: &mut Ui) {
        let can_stop = self.client.is_some();
        ui.add_enabled_ui(can_stop, |ui| {
            let button = egui::Button::new(
                egui::RichText::new(format!(
                    "{} Emergency stop",
                    egui_phosphor::regular::HAND_PALM
                ))
                .size(15.0)
                .strong(),
            )
            .fill(egui::Color32::from_rgb(230, 30, 140))
            .min_size(egui::vec2(ui.available_width(), 32.0));
            if ui
                .add(button)
                .on_hover_text("Silences the toy right now, regardless of what's running.")
                .clicked()
            {
                self.force_stop_toy("manual_emergency_stop");
            }
        });
        if !can_stop {
            ui.small("Connect a device to enable the emergency stop.");
        }
        ui.add_space(6.0);
    }

    /// A baseline level the companion holds on the toy between trigger effects,
    /// so a partner-driven constant vibration is not left silent after every
    /// trigger. See [`AppState::maintain_resting`].
    fn draw_resting_vibration(&mut self, ui: &mut Ui, busy: bool) {
        egui::CollapsingHeader::new("Resting vibration")
            .id_salt("resting-vibration")
            .show(ui, |ui| {
                ui.small(
                    "The toy holds this level between triggers. Set a partner's baseline here instead of the phone app; 0 turns it off.",
                );
                ui.add_space(4.0);
                ui.add_enabled_ui(!busy, |ui| {
                    let mut level = i32::from(self.resting_strength);
                    if ui
                        .add(egui::Slider::new(&mut level, 0..=20).text("strength"))
                        .changed()
                    {
                        self.resting_strength = level.clamp(0, 20) as u8;
                    }
                });
            });
    }

    /// Reserves the same vertical space as the trigger list's heading +
    /// flourish + caption (drawn transparently, so its height matches
    /// exactly rather than being guessed), so the effect editor's card
    /// starts level with the first trigger card next to it.
    fn draw_effect_editor_header_spacer(ui: &mut Ui) {
        ui.scope(|ui| {
            ui.visuals_mut().override_text_color = Some(egui::Color32::TRANSPARENT);
            ui.label(crate::theme::heading_text("Triggers", 26.0));
        });
        ui.add_space(14.0); // matches theme::flourish's fixed height
        ui.scope(|ui| {
            ui.visuals_mut().override_text_color = Some(egui::Color32::TRANSPARENT);
            ui.small("spacer");
        });
        ui.add_space(6.0);
    }

    fn draw_effect_editor(&mut self, ui: &mut Ui, busy: bool) {
        let destination = self.selected_effect;
        Self::draw_effect_editor_header_spacer(ui);
        crate::theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::theme::icon_badge_sized(ui, trigger_icon(destination), true, 56.0);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(crate::theme::heading_text(
                        format!("{} effect", trigger_display_label(destination)),
                        22.0,
                    ));
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.add_space(3.0);
                                ui.label(crate::theme::heading_text("Trigger", 16.0));
                            });
                            toggle_button(ui, &mut self.triggers.get_mut(destination).enabled);
                        });
                    });
                });
            });
            if matches!(
                destination,
                TriggerKind::AbilityUse | TriggerKind::AbilityCooldownReady
            ) {
                self.draw_ability_filter(ui, destination, busy);
                if destination == TriggerKind::AbilityCooldownReady {
                    ui.small("Cooldown ready includes a normal cooldown finishing and a charged ability restoring a charge.");
                }
            }
            if destination.is_amount_based() {
                self.draw_amount_threshold(ui, destination, busy);
            }
            if destination == TriggerKind::Death {
                ui.add_space(6.0);
                ui.add_enabled_ui(!busy, |ui| {
                    crate::theme::dot_checkbox(
                        ui,
                        &mut self.triggers.death_hold_until_respawn,
                        "Hold until I respawn",
                    )
                    .on_hover_text(format!(
                        "Ignore the duration above and keep the toy at this strength until you respawn (up to {RESPAWN_HOLD_SAFETY_CAP_SECS}s)."
                    ));
                    crate::theme::dot_checkbox(
                        ui,
                        &mut self.triggers.suppress_triggers_while_dead,
                        "Ignore ability, assist & ally-support triggers while dead",
                    )
                    .on_hover_text(
                        "Stops a spectated teammate's abilities, assists, heals and shields from cutting the death effect short.",
                    );
                });
            }
            if destination.is_intensity_curve() {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.add_enabled_ui(!busy, |ui| {
                    self.draw_intensity_curve_editor(ui);
                });
            } else {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    crate::theme::label_nudged_down(
                        ui,
                        &format!("Copy {ACTION_KIND_LABEL} settings from"),
                        8.0,
                    );
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::ComboBox::from_id_salt("copy-action-source")
                            .selected_text(trigger_display_label(self.copy_source))
                            .show_ui(ui, |ui| {
                                for source in PRIORITY_ORDER_DEFAULT {
                                    if source != destination && !source.is_intensity_curve() {
                                        ui.selectable_value(
                                            &mut self.copy_source,
                                            source,
                                            trigger_display_label(source),
                                        );
                                    }
                                }
                            });
                    });
                    ui.add_enabled_ui(!busy && self.copy_source != destination, |ui| {
                        let label = format!("{} Copy", egui_phosphor::regular::HEART);
                        if ui.button(label).clicked() {
                            self.copy_action_settings(self.copy_source, destination);
                        }
                    });
                });
                if let Some(feedback) = &self.copy_feedback {
                    status_line(ui, feedback, [0.30, 0.78, 0.42, 1.0]);
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.add_enabled_ui(!busy, |ui| {
                    let trigger = self.triggers.get_mut(destination);
                    crate::action_ui::draw_vibrate_settings_editor(ui, &mut trigger.actions);
                });
            }
        });

        ui.add_space(10.0);
        self.draw_emergency_stop(ui);
    }

    /// The damage-taken intensity trigger has no fixed strength: it drives the
    /// toy harder the more damage you are taking, along a curve you draw here.
    fn draw_intensity_curve_editor(&mut self, ui: &mut Ui) {
        ui.small(
            "The more damage you take inside the window, the stronger the pulse. Drag the dots to shape the curve, double-click the graph to add a point, right-click a point to remove it.",
        );
        ui.add_space(8.0);

        let curve = &mut self.triggers.damage_taken_curve;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label("Window");
            if ui
                .add(
                    egui::Slider::new(&mut curve.window_seconds, 0.5..=MAX_INTENSITY_WINDOW_SECS)
                        .suffix(" s")
                        .fixed_decimals(1),
                )
                .on_hover_text("How many seconds of damage taken are summed for the curve.")
                .changed()
            {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Pulse length");
            if ui
                .add(
                    egui::Slider::new(&mut curve.pulse_seconds, 0.25..=5.0)
                        .step_by(0.25)
                        .suffix(" s"),
                )
                .on_hover_text("How long each buzz lasts. Pulses repeat while damage keeps coming.")
                .changed()
            {
                curve.pulse_seconds = crate::action::nearest_duration_step(curve.pulse_seconds);
                changed = true;
            }
        });
        ui.add_space(8.0);

        changed |= draw_curve_graph(ui, curve);

        ui.add_space(4.0);
        ui.small(egui::RichText::new(curve.summary()).color(crate::theme::TEXT_DIM));

        if changed {
            curve.normalize();
        }
    }

    fn draw_ability_filter(&mut self, ui: &mut Ui, kind: TriggerKind, busy: bool) {
        let mut slots: BTreeSet<u32> = (1..=4).collect();
        slots.extend(self.ability_catalog.keys().copied());
        let slots: Vec<u32> = slots.into_iter().collect();
        let names = self.ability_catalog.clone();
        let filter = self
            .triggers
            .ability_filter_mut(kind)
            .expect("ability trigger has an ability filter");

        ui.add_space(6.0);
        ui.label("Abilities");
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("All").clicked() {
                    *filter = AbilityFilter::All;
                }
                if ui.button("None").clicked() {
                    *filter = AbilityFilter::Selected(BTreeSet::new());
                }
            });
            ui.horizontal_wrapped(|ui| {
                for slot in &slots {
                    let mut selected = filter.accepts(*slot);
                    let label = names
                        .get(slot)
                        .and_then(Option::as_deref)
                        .map(|name| format!("Slot {slot}: {name}"))
                        .unwrap_or_else(|| format!("Slot {slot}"));
                    if crate::theme::dot_checkbox(ui, &mut selected, &label).changed() {
                        if matches!(&*filter, AbilityFilter::All) {
                            let selected_slots: BTreeSet<u32> = slots
                                .iter()
                                .copied()
                                .filter(|candidate| *candidate != *slot)
                                .collect();
                            *filter = AbilityFilter::Selected(selected_slots);
                        } else if let AbilityFilter::Selected(selected_slots) = &mut *filter {
                            if selected {
                                selected_slots.insert(*slot);
                            } else {
                                selected_slots.remove(slot);
                            }
                        }
                    }
                }
            });
        });
        if matches!(&*filter, AbilityFilter::Selected(slots) if slots.is_empty()) {
            status_line(
                ui,
                "No abilities are selected; this trigger will not send an action.",
                [0.92, 0.68, 0.22, 1.0],
            );
        }
        if self.ability_catalog.is_empty() {
            ui.small("Using numbered slots until the game reports ability names.");
        }
    }

    /// The threshold + rolling-window controls for an amount-based trigger
    /// (damage taken, healing received): how much has to add up, and over
    /// how long a trailing window, before it fires.
    fn draw_amount_threshold(&mut self, ui: &mut Ui, kind: TriggerKind, busy: bool) {
        let Some(settings) = self.triggers.amount_settings_mut(kind) else {
            return;
        };
        ui.add_space(6.0);
        ui.label("Fires when this much accumulates within the window below");
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Threshold");
                ui.add(egui::DragValue::new(&mut settings.threshold).range(0.0..=100_000.0));
                ui.weak("health");
            });
            ui.horizontal(|ui| {
                ui.label("Window");
                ui.add(
                    egui::DragValue::new(&mut settings.window_seconds)
                        .range(0.1..=300.0)
                        .speed(0.1),
                );
                ui.weak("seconds");
            });
        });
    }

    fn draw_donate(ui: &mut Ui) {
        crate::theme::card(ui).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.colored_label(
                    crate::theme::ACCENT,
                    egui::RichText::new(egui_phosphor::regular::HEART).size(40.0),
                );
                ui.add_space(6.0);
                ui.label(crate::theme::heading_text("Enjoying Lovelock Companion?", 24.0));
                ui.small("If it's brought a little extra fun to your matches, consider supporting development on Ko-fi.");
                ui.add_space(12.0);
                let button = egui::Button::new(
                    egui::RichText::new(format!(
                        "{} Support on Ko-fi",
                        egui_phosphor::regular::HEART
                    ))
                    .strong(),
                )
                .fill(crate::theme::ACCENT);
                if ui.add(button).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(KOFI_URL));
                }
                ui.add_space(6.0);
                ui.hyperlink_to(KOFI_URL, KOFI_URL);
            });
        });

        ui.add_space(10.0);

        crate::theme::card(ui).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.colored_label(
                    crate::theme::ACCENT,
                    egui::RichText::new(egui_phosphor::regular::SPARKLE).size(28.0),
                );
                ui.add_space(6.0);
                ui.label(crate::theme::heading_text("Shoutout to volc", 20.0));
                ui.small("The original creator of DeadlockShock, which this companion is built on. Go check them out!");
                ui.add_space(10.0);
                let button = egui::Button::new(
                    egui::RichText::new(format!(
                        "{} Visit volc on GameBanana",
                        egui_phosphor::regular::SPARKLE
                    ))
                    .strong(),
                )
                .fill(crate::theme::ACCENT);
                if ui.add(button).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(VOLC_PROFILE_URL));
                }
                ui.add_space(6.0);
                ui.hyperlink_to(VOLC_PROFILE_URL, VOLC_PROFILE_URL);
            });
        });
    }

    fn draw_game_connection(&mut self, ui: &mut Ui) {
        crate::theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::ACCENT, egui_phosphor::regular::PLUGS_CONNECTED);
                ui.label(crate::theme::heading_text("Game connection", 19.0));
            });
            ui.small("Deadlock must be launched with -condebug so it writes console.log.");
            ui.add_space(4.0);
            text_input(ui, "Log path", &mut self.log_path, false);
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing.x;
                let button_size = egui::vec2(
                    (ui.available_width() - spacing) * 0.5,
                    ui.spacing().interact_size.y,
                );
                if ui
                    .add_sized(button_size, egui::Button::new("Auto-detect"))
                    .clicked()
                {
                    self.auto_detect_log_path();
                }
                if ui
                    .add_sized(button_size, egui::Button::new("Start/Restart listener"))
                    .clicked()
                {
                    self.start_listener_from_input();
                }
            });
            if let Some(status) = &self.log_detection_status {
                ui.add_space(4.0);
                crate::theme::badge(ui, status.label(), status.tone());
            }
            if let Some(error) = &self.listener_action_error {
                status_line(ui, error, [0.92, 0.32, 0.28, 1.0]);
            }
            ui.add_space(6.0);
            let listener_status = self.bridge_listener.status();
            draw_listener_status(ui, &listener_status, self.last_bridge_event.as_ref());
            ui.label(format!(
                "Current ability catalogue: {} slot(s).",
                self.ability_catalog.len()
            ));
            if let Some(status) = &self.action_status {
                let label = status.label();
                status_line(ui, &label, status.color());
            } else {
                ui.label("Last action delivery: none since startup.");
            }
        });
    }
}

/// How often `draw` rebuilds the full [`PersistedState`] to feed the autosave
/// debouncer. Sub-interval settling is invisible to the user (the save itself
/// is debounced by [`crate::persistence::SAVE_DEBOUNCE`]) and rebuilding it
/// every frame is pure waste, more so the more profiles exist.
const AUTOSAVE_CHECK_INTERVAL: Duration = Duration::from_millis(200);

pub struct CompanionApp {
    pub state: AppState,
    persistence: Persistence,
    /// When `draw` last handed a fresh snapshot to `persistence.observe`, and
    /// the repaint delay that call asked for (kept so idle frames between
    /// checks still reschedule the pending save).
    last_autosave_check: Instant,
    autosave_repaint_delay: Option<Duration>,
    reset_confirmation: bool,
    menu_error: Option<String>,
    version_check: VersionCheckOwner,
    version_warnings: WarningSelection,
    log_store: LogStore,
    logs_window_open: bool,
    logs_cached_revision: u64,
    logs_cached_text: String,
    logo_texture: Option<egui::TextureHandle>,
    credits_window_open: bool,
}

impl CompanionApp {
    pub fn load() -> Self {
        Self::load_with_store(LogStore::new())
    }

    fn load_with_store(log_store: LogStore) -> Self {
        match default_state_path() {
            Ok(path) => {
                Self::load_from_path_with_detector_and_store(path, deadlock_path::detect, log_store)
            }
            Err(error) => {
                log::warn!(
                    target: "companion::app",
                    "settings_load_unavailable error={:?}",
                    error
                );
                let (persistence, state) = Persistence::unavailable(error);
                Self::from_persisted_state(persistence, state, deadlock_path::detect, log_store)
            }
        }
    }

    pub fn load_with_context(context: egui::Context, log_store: LogStore) -> Self {
        let mut app = Self::load_with_store(log_store);
        app.version_check = VersionCheckOwner::new(&context);
        // The Lovense connection itself is never persisted (only the setup
        // that reaches it is), so without this the toy has to be manually
        // reconnected every time the app is opened, even though everything
        // needed to do so automatically is already saved.
        if app.state.credentials_present() {
            log::info!(target: "companion::app", "startup_connection_attempt provider={PROVIDER_LABEL}");
            app.state.start_connection_test(context);
        }
        app
    }

    #[cfg(test)]
    fn load_from_path_with_detector<F>(path: PathBuf, detector: F) -> Self
    where
        F: FnOnce() -> Result<Detection, DetectionError>,
    {
        Self::load_from_path_with_detector_and_store(path, detector, LogStore::new())
    }

    fn load_from_path_with_detector_and_store<F>(
        path: PathBuf,
        detector: F,
        log_store: LogStore,
    ) -> Self
    where
        F: FnOnce() -> Result<Detection, DetectionError>,
    {
        let (persistence, state) = Persistence::open(path);
        Self::from_persisted_state(persistence, state, detector, log_store)
    }

    fn from_persisted_state<F>(
        persistence: Persistence,
        persisted_state: PersistedState,
        detector: F,
        log_store: LogStore,
    ) -> Self
    where
        F: FnOnce() -> Result<Detection, DetectionError>,
    {
        let mut state = persisted_state.restore_app();
        state.initialize_log_listener(detector);
        Self {
            state,
            persistence,
            last_autosave_check: Instant::now(),
            autosave_repaint_delay: None,
            reset_confirmation: false,
            menu_error: None,
            version_check: VersionCheckOwner::with_client(LATEST_RELEASE_URL, None),
            version_warnings: WarningSelection::default(),
            log_store,
            logs_window_open: false,
            logs_cached_revision: 0,
            logs_cached_text: String::new(),
            logo_texture: None,
            credits_window_open: false,
        }
    }

    pub fn draw(&mut self, ui: &mut Ui) {
        self.version_check.poll();
        let listener_status = self.state.bridge_listener.status();
        let remote = match &self.version_check.state {
            VersionCheckState::Current { latest }
            | VersionCheckState::UpdateAvailable { latest } => Some(latest),
            _ => None,
        };
        self.version_warnings =
            select_warnings(&app_version(), &listener_status.mod_version, remote);

        draw_resize_borders(ui.ctx());
        self.draw_title_bar(ui);

        let listener_silent = listener_status
            .silent_since_attach()
            .is_some_and(|silent| silent >= LISTENER_SILENCE_HINT_AFTER);
        if self.persistence.load_warning().is_some()
            || self.persistence.save_error().is_some()
            || self.menu_error.is_some()
            || self.has_update_warning()
            || listener_silent
        {
            egui::Panel::top("notices")
                .frame(egui::Frame::NONE.fill(crate::theme::BASE).inner_margin(8.0))
                .show(ui, |ui| {
                    if let Some(warning) = self.persistence.load_warning() {
                        status_line(ui, warning, NOTICE_PINK);
                    }
                    if let Some(error) = self.persistence.save_error() {
                        status_line(ui, error, [0.92, 0.32, 0.28, 1.0]);
                    }
                    if let Some(error) = &self.menu_error {
                        status_line(ui, error, [0.92, 0.32, 0.28, 1.0]);
                    }
                    if listener_silent {
                        status_line(
                            ui,
                            "No data from Deadlock yet. Add -condebug to the game's Steam launch options, then restart it.",
                            NOTICE_PINK,
                        );
                    }
                    self.draw_update_panel(ui);
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(crate::theme::BASE)
                    .inner_margin(16.0),
            )
            .show(ui, |ui| {
                crate::theme::paint_dotted_background(ui);
                // No page-wide scroll: each section (and, on the Effects tab,
                // the trigger list and the editor separately) scrolls itself.
                self.state.draw(ui);
            });
        let ctx = ui.ctx().clone();
        self.draw_reset_confirmation(&ctx);
        self.draw_logs_window(&ctx);
        self.draw_credits_window(&ctx);

        let now = Instant::now();
        if now.saturating_duration_since(self.last_autosave_check) >= AUTOSAVE_CHECK_INTERVAL {
            self.last_autosave_check = now;
            self.autosave_repaint_delay = self
                .persistence
                .observe(PersistedState::from_app(&self.state), now);
        }
        if let Some(delay) = self.autosave_repaint_delay {
            // Cap the wait so a pending save still commits while the window is
            // otherwise idle, without pinning the frame rate the rest of the time.
            ctx.request_repaint_after(delay.min(AUTOSAVE_CHECK_INTERVAL));
        }
        if self.version_check.is_checking() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn has_update_warning(&self) -> bool {
        self.version_warnings.companion_outdated.is_some()
            || self.version_warnings.mod_outdated.is_some()
            || self.version_warnings.mod_legacy
            || self.version_warnings.mod_invalid
    }

    fn draw_title_bar(&mut self, ui: &mut Ui) {
        let bar_height = 44.0;
        egui::Panel::top("title_bar")
            .exact_size(bar_height)
            .frame(
                egui::Frame::NONE
                    .fill(crate::theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let bar_rect = ui.max_rect();
                ui.horizontal_centered(|ui| {
                    let drag_response = ui.interact(
                        bar_rect,
                        ui.id().with("title_bar_drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if drag_response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag_response.double_clicked() {
                        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }

                    if self.logo_texture.is_none() {
                        let image = image::load_from_memory_with_format(
                            include_bytes!("../assets/logo.png"),
                            image::ImageFormat::Png,
                        )
                        .expect("logo.png must be a valid PNG")
                        .into_rgba8();
                        let size = [image.width() as usize, image.height() as usize];
                        self.logo_texture = Some(ui.ctx().load_texture(
                            "lovelock-logo",
                            egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw()),
                            egui::TextureOptions::LINEAR,
                        ));
                    }

                    // The wordmark sits centered in the whole bar; the window
                    // controls float over its right end. A small guard keeps
                    // them from colliding on a very narrow window by falling
                    // back to a left-aligned wordmark.
                    let title = "Lovelock Companion";
                    let title_font = egui::FontId::new(20.0, crate::theme::heading_family());
                    let title_w = ui
                        .painter()
                        .layout_no_wrap(title.to_owned(), title_font, crate::theme::TEXT)
                        .size()
                        .x;
                    let logo_size = 28.0;
                    let gap = 8.0;
                    let group_w = logo_size + gap + title_w;
                    let centered = bar_rect.width() > group_w + 320.0;
                    let group_rect = if centered {
                        egui::Rect::from_center_size(
                            bar_rect.center(),
                            egui::vec2(group_w, bar_rect.height()),
                        )
                    } else {
                        egui::Rect::from_min_size(
                            bar_rect.left_center() - egui::vec2(0.0, bar_rect.height() / 2.0),
                            egui::vec2(group_w, bar_rect.height()),
                        )
                    };
                    let logo_texture = self.logo_texture.clone();
                    ui.scope_builder(
                        egui::UiBuilder::new().max_rect(group_rect).layout(
                            egui::Layout::left_to_right(egui::Align::Center),
                        ),
                        |ui| {
                            if let Some(logo) = &logo_texture {
                                ui.add(
                                    egui::Image::new(egui::load::SizedTexture::from_handle(logo))
                                        .fit_to_exact_size(egui::Vec2::splat(logo_size)),
                                );
                            }
                            ui.add_space(gap);
                            ui.label(crate::theme::heading_text(title, 20.0));
                        },
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(egui_phosphor::regular::X).frame(false))
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui
                            .add(egui::Button::new(egui_phosphor::regular::SQUARE).frame(false))
                            .clicked()
                        {
                            let maximized =
                                ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if ui
                            .add(egui::Button::new(egui_phosphor::regular::MINUS).frame(false))
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(10.0);
                        let gear_response = ui.add(
                            egui::Button::new(egui_phosphor::regular::GEAR)
                                .corner_radius(egui::CornerRadius::same(255))
                                .stroke(egui::Stroke::new(1.0, crate::theme::STROKE)),
                        );
                        egui::Popup::menu(&gear_response).show(|ui| {
                            ui.set_min_width(220.0);
                            self.draw_menu_contents(ui);
                        });
                        ui.add_space(10.0);
                        crate::theme::badge(
                            ui,
                            &format!("v{}", app_version()),
                            crate::theme::BadgeTone::Success,
                        );
                    });
                });
            });
    }
    fn draw_logs_window(&mut self, ctx: &egui::Context) {
        if !self.logs_window_open {
            return;
        }

        let revision = self.log_store.revision();
        if revision != self.logs_cached_revision {
            let snapshot: LogSnapshot = self.log_store.snapshot();
            self.logs_cached_revision = snapshot.revision;
            self.logs_cached_text = snapshot.text;
        }
        ctx.request_repaint_after(Duration::from_millis(250));

        let mut open = self.logs_window_open;
        egui::Window::new("Logs").open(&mut open).show(ctx, |ui| {
            if ui.button("Copy all").clicked() {
                ctx.copy_text(self.logs_cached_text.clone());
            }
            if self.logs_cached_text.is_empty() {
                ui.label("No log records have been captured yet.");
                return;
            }
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.logs_cached_text).monospace())
                            .selectable(true),
                    );
                });
        });
        self.logs_window_open = open;
    }

    fn draw_credits_window(&mut self, ctx: &egui::Context) {
        if !self.credits_window_open {
            return;
        }

        let mut open = true;
        egui::Window::new("Credits")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.colored_label(
                        crate::theme::ACCENT,
                        egui::RichText::new(egui_phosphor::regular::HEART).size(32.0),
                    );
                    ui.add_space(8.0);
                    ui.label(crate::theme::heading_text("volc", 18.0));
                    ui.label(
                        "A HUGE THANK YOU to volc for DeadlockShock, the mod this companion is built on.",
                    );
                    ui.add_space(12.0);
                    ui.label(crate::theme::heading_text("KaufkinNova", 18.0));
                    ui.label("A HUGE THANK YOU to KaufkinNova for the idea and for sponsoring this mod.");
                    ui.add_space(4.0);
                });
            });
        self.credits_window_open = open;
    }

    fn draw_update_panel(&self, ui: &mut Ui) {
        let has_warning = self.version_warnings.companion_outdated.is_some()
            || self.version_warnings.mod_outdated.is_some()
            || self.version_warnings.mod_legacy
            || self.version_warnings.mod_invalid;
        if !has_warning {
            return;
        }
        egui::Frame::group(ui.style())
            .fill(Color32::from_rgb(86, 64, 22))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.strong("Updates available");
                if let Some(target) = &self.version_warnings.companion_outdated {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Companion {} is older than {}.",
                            app_version(),
                            target
                        ));
                        ui.hyperlink_to("Download companion", COMPANION_RELEASE_URL);
                    });
                }
                if let Some((installed, target)) = &self.version_warnings.mod_outdated {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "DeadlockShock mod {} is older than {}.",
                            installed, target
                        ));
                        ui.hyperlink_to("Update mod", MOD_RELEASE_URL);
                    });
                } else if self.version_warnings.mod_legacy {
                    ui.horizontal(|ui| {
                        ui.label("The last observed DeadlockShock mod predates version reporting.");
                        ui.hyperlink_to("Update mod", MOD_RELEASE_URL);
                    });
                } else if self.version_warnings.mod_invalid {
                    ui.horizontal(|ui| {
                        ui.label("The last observed DeadlockShock mod reported an invalid version; reinstall the latest mod.");
                        ui.hyperlink_to("Update mod", MOD_RELEASE_URL);
                    });
                }
            });
    }
    pub fn flush_pending(&mut self) {
        log::info!(target: "companion::app", "settings_flush_boundary reason=application_exit");
        let result = self
            .persistence
            .flush(PersistedState::from_app(&self.state));
        if result.is_err() {
            log::warn!(target: "companion::app", "settings_flush_boundary outcome=failed");
        }
    }
    fn draw_menu_contents(&mut self, ui: &mut Ui) {
        let reset_available = !self.state.is_busy();
        ui.label(format!("Companion version: {}", app_version()));
        let mod_label = match &self.state.bridge_listener.status().mod_version {
            ModVersionObservation::Unknown => "unknown".to_owned(),
            ModVersionObservation::Legacy => "legacy (no version reporting)".to_owned(),
            ModVersionObservation::Invalid => "invalid".to_owned(),
            ModVersionObservation::Reported(version) => format!("last observed {version}"),
        };
        ui.label(format!("Mod version: {mod_label}"));
        match &self.version_check.state {
            VersionCheckState::Checking => ui.label("Latest stable: checking…"),
            VersionCheckState::Current { latest } => {
                ui.label(format!("Latest stable: {latest} (current)"))
            }
            VersionCheckState::UpdateAvailable { latest } => {
                ui.label(format!("Latest stable: {latest} (update available)"))
            }
            VersionCheckState::Unavailable { reason } => {
                ui.label(format!("Latest stable: unavailable ({reason})"))
            }
        };
        let checking = self.version_check.is_checking();
        if ui
            .add_enabled(!checking, egui::Button::new("Check for updates"))
            .clicked()
        {
            self.version_check.start(ui.ctx().clone());
        }
        ui.separator();
        if ui.button("Open config folder").clicked() {
            log::info!(target: "companion::app", "config_folder_open_requested");
            self.menu_error = self.persistence.open_config_directory().err();
            if let Some(error) = &self.menu_error {
                log::warn!(
                    target: "companion::app",
                    "config_folder_open_failed error={:?}",
                    error
                );
            }
        }
        if ui.button("Show logs").clicked() {
            self.logs_window_open = true;
            ui.close();
        }
        if ui.button("Credits").clicked() {
            self.credits_window_open = true;
            ui.close();
        }
        ui.separator();
        let response = ui.add_enabled(reset_available, egui::Button::new("Reset saved state…"));
        if response.clicked() {
            self.reset_confirmation = true;
        }
        if !reset_available {
            response.on_disabled_hover_text(
                "Wait for connection, test action, and action work to finish before resetting.",
            );
        }
    }

    fn draw_reset_confirmation(&mut self, ctx: &egui::Context) {
        if !self.reset_confirmation {
            return;
        }

        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        let reset_available = !self.state.is_busy();
        egui::Window::new("Reset saved state?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    "This clears saved provider setup, target preference, trigger action settings, and log path.",
                );
                ui.label("Any active log listener will be stopped.");
                if !reset_available {
                    status_line(
                        ui,
                        "Wait for connection, test action, and action work to finish.",
                        [0.92, 0.68, 0.22, 1.0],
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add_enabled(reset_available, egui::Button::new("Reset"))
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        self.reset_confirmation = open && !cancel;
        if confirm && self.reset_and_save() {
            self.reset_confirmation = false;
        }
    }

    fn reset_and_save(&mut self) -> bool {
        log::info!(target: "companion::app", "settings_reset_requested");
        if !self.state.reset_saved_state() {
            log::warn!(
                target: "companion::app",
                "settings_reset_outcome outcome=skipped"
            );
            return false;
        }
        let result = self
            .persistence
            .save_reset_now(PersistedState::from_app(&self.state));
        log::info!(
            target: "companion::app",
            "settings_reset_outcome outcome=applied saved={}",
            result.is_ok()
        );
        true
    }
}
fn trigger_display_label(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Death => "Death",
        TriggerKind::Kill => "Kill",
        TriggerKind::Assist => "Assist",
        TriggerKind::AbilityUse => "Ability use",
        TriggerKind::AbilityCooldownReady => "Cooldown ready",
        TriggerKind::DamageTaken => "Damage taken",
        TriggerKind::HealingReceived => "Healing received",
        TriggerKind::AllyHealed => "Healed an ally",
        TriggerKind::AllyShielded => "Shielded an ally",
        TriggerKind::DamageGiven => "Damage given",
        TriggerKind::SoulDeny => "Soul orb deny",
        TriggerKind::SoulSecure => "Soul orb secure",
        TriggerKind::ParrySuccess => "Parry success",
        TriggerKind::ParryFail => "Got parried",
        TriggerKind::ObjectiveGuardian => "Guardian destroyed",
        TriggerKind::ObjectiveWalker => "Walker destroyed",
        TriggerKind::ObjectiveBaseGuardian => "Base Guardian destroyed",
        TriggerKind::ObjectiveShrine => "Shrine destroyed",
        TriggerKind::ObjectivePatronWeakened => "Patron weakened",
        TriggerKind::GameWon => "Game won",
        TriggerKind::DamageTakenIntensity => "Damage intensity",
    }
}

fn trigger_icon(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Death => egui_phosphor::regular::SKULL,
        TriggerKind::Kill => egui_phosphor::regular::SWORD,
        TriggerKind::Assist => egui_phosphor::regular::HANDSHAKE,
        TriggerKind::AbilityUse => egui_phosphor::regular::MAGIC_WAND,
        TriggerKind::AbilityCooldownReady => egui_phosphor::regular::HOURGLASS_SIMPLE,
        TriggerKind::DamageTaken => egui_phosphor::regular::SHIELD_SLASH,
        TriggerKind::HealingReceived => egui_phosphor::regular::HEART_STRAIGHT,
        TriggerKind::AllyHealed => egui_phosphor::regular::HAND_HEART,
        TriggerKind::AllyShielded => egui_phosphor::regular::SHIELD_PLUS,
        TriggerKind::DamageGiven => egui_phosphor::regular::CROSSHAIR,
        TriggerKind::SoulDeny => egui_phosphor::regular::PROHIBIT,
        TriggerKind::SoulSecure => egui_phosphor::regular::COINS,
        TriggerKind::ParrySuccess => egui_phosphor::regular::SHIELD_CHECK,
        TriggerKind::ParryFail => egui_phosphor::regular::SHIELD_WARNING,
        TriggerKind::ObjectiveGuardian => egui_phosphor::regular::CASTLE_TURRET,
        TriggerKind::ObjectiveWalker => egui_phosphor::regular::ROBOT,
        TriggerKind::ObjectiveBaseGuardian => egui_phosphor::regular::CASTLE_TURRET,
        TriggerKind::ObjectiveShrine => egui_phosphor::regular::CASTLE_TURRET,
        TriggerKind::ObjectivePatronWeakened => egui_phosphor::regular::CROWN_SIMPLE,
        TriggerKind::GameWon => egui_phosphor::regular::TROPHY,
        TriggerKind::DamageTakenIntensity => egui_phosphor::regular::WARNING_OCTAGON,
    }
}

fn first_copy_source(destination: TriggerKind) -> TriggerKind {
    match destination {
        TriggerKind::Death => TriggerKind::AbilityUse,
        TriggerKind::Kill
        | TriggerKind::Assist
        | TriggerKind::AllyHealed
        | TriggerKind::AllyShielded
        | TriggerKind::DamageGiven
        | TriggerKind::SoulDeny
        | TriggerKind::SoulSecure
        | TriggerKind::ParrySuccess
        | TriggerKind::ParryFail
        | TriggerKind::ObjectiveGuardian
        | TriggerKind::ObjectiveWalker
        | TriggerKind::ObjectiveBaseGuardian
        | TriggerKind::ObjectiveShrine
        | TriggerKind::ObjectivePatronWeakened
        | TriggerKind::GameWon
        | TriggerKind::DamageTakenIntensity
        | TriggerKind::AbilityUse
        | TriggerKind::AbilityCooldownReady
        | TriggerKind::DamageTaken
        | TriggerKind::HealingReceived => TriggerKind::Death,
    }
}

/// The window has no OS decorations (custom title bar instead), so there is
/// no native edge-drag resize affordance. This paints invisible drag strips
/// along the screen edges/corners that issue `BeginResize` to the backend.
fn draw_resize_borders(ctx: &egui::Context) {
    let screen = ctx.input(|i| i.viewport_rect());
    if !screen.is_positive() {
        return;
    }

    let border = 6.0;
    let corner = 14.0;
    let zones: [(egui::Rect, egui::CursorIcon, egui::ResizeDirection); 8] = [
        (
            egui::Rect::from_min_size(screen.left_top(), egui::vec2(corner, corner)),
            egui::CursorIcon::ResizeNorthWest,
            egui::ResizeDirection::NorthWest,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - corner, screen.top()),
                egui::vec2(corner, corner),
            ),
            egui::CursorIcon::ResizeNorthEast,
            egui::ResizeDirection::NorthEast,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left(), screen.bottom() - corner),
                egui::vec2(corner, corner),
            ),
            egui::CursorIcon::ResizeSouthWest,
            egui::ResizeDirection::SouthWest,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - corner, screen.bottom() - corner),
                egui::vec2(corner, corner),
            ),
            egui::CursorIcon::ResizeSouthEast,
            egui::ResizeDirection::SouthEast,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left(), screen.top() + corner),
                egui::vec2(border, (screen.height() - 2.0 * corner).max(0.0)),
            ),
            egui::CursorIcon::ResizeWest,
            egui::ResizeDirection::West,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - border, screen.top() + corner),
                egui::vec2(border, (screen.height() - 2.0 * corner).max(0.0)),
            ),
            egui::CursorIcon::ResizeEast,
            egui::ResizeDirection::East,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left() + corner, screen.top()),
                egui::vec2((screen.width() - 2.0 * corner).max(0.0), border),
            ),
            egui::CursorIcon::ResizeNorth,
            egui::ResizeDirection::North,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left() + corner, screen.bottom() - border),
                egui::vec2((screen.width() - 2.0 * corner).max(0.0), border),
            ),
            egui::CursorIcon::ResizeSouth,
            egui::ResizeDirection::South,
        ),
    ];

    egui::Area::new(egui::Id::new("resize_borders"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            for (index, (rect, cursor, direction)) in zones.into_iter().enumerate() {
                if !rect.is_positive() {
                    continue;
                }
                let id = ui.id().with(("resize_zone", index));
                let response = ui.interact(rect, id, egui::Sense::drag());
                if response.hovered() || response.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if response.drag_started() {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                }
            }
        });
}

fn toggle_button(ui: &mut Ui, value: &mut bool) {
    let label = if *value { "On" } else { "Off" };
    let (fill, text_color) = if *value {
        (crate::theme::ACCENT, egui::Color32::WHITE)
    } else {
        (crate::theme::CARD_RAISED, crate::theme::TEXT_DIM)
    };
    let button = egui::Button::new(egui::RichText::new(label).strong().color(text_color))
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(255))
        .min_size(egui::vec2(52.0, 0.0));
    if ui.add(button).clicked() {
        *value = !*value;
    }
}

/// Once the listener has watched an unchanging console.log for this long with
/// nothing at all arriving, the game is almost certainly running without
/// `-condebug` rather than simply sitting on a quiet menu.
const LISTENER_SILENCE_HINT_AFTER: Duration = Duration::from_secs(25);

fn draw_listener_status(ui: &mut Ui, status: &ListenerStatus, last_event: Option<&BridgeEvent>) {
    let looks_silent = status
        .silent_since_attach()
        .is_some_and(|silent| silent >= LISTENER_SILENCE_HINT_AFTER);
    let (phase_label, phase_tone) = match status.phase {
        ListenerPhase::Stopped => ("Listener stopped.".to_owned(), crate::theme::BadgeTone::Neutral),
        ListenerPhase::WaitingForFile => (
            "Listener waiting for console.log to be created.".to_owned(),
            crate::theme::BadgeTone::Warning,
        ),
        ListenerPhase::Listening if looks_silent => (
            "Watching console.log, but nothing is arriving.".to_owned(),
            crate::theme::BadgeTone::Warning,
        ),
        ListenerPhase::Listening => (
            "Listener is monitoring console.log.".to_owned(),
            crate::theme::BadgeTone::Success,
        ),
        ListenerPhase::Failed => (
            format!(
                "Listener failed: {}",
                status.current_error.as_deref().unwrap_or("unknown error")
            ),
            crate::theme::BadgeTone::Danger,
        ),
    };
    if let Some(path) = &status.configured_path {
        ui.label(format!("Configured listener path: {}", path.display()));
    }
    crate::theme::badge(ui, &phase_label, phase_tone);
    if looks_silent {
        status_line(
            ui,
            "Deadlock is not writing to console.log. Open Steam, right click Deadlock, choose Properties, add -condebug to the launch options, then restart the game.",
            [0.92, 0.68, 0.22, 1.0],
        );
    }
    let activity = status
        .last_activity_at
        .map(|at| format!("Last log activity: {} ago.", format_duration(at.elapsed())))
        .unwrap_or_else(|| "Last log activity: none since listener start.".to_owned());
    ui.label(activity);
    let event = match (last_event, status.last_event_at) {
        (Some(event), Some(at)) => format!(
            "Last bridge event: {} ({} ago).",
            bridge_event_description(event),
            format_duration(at.elapsed())
        ),
        _ => "Last bridge event: none since listener start.".to_owned(),
    };
    ui.label(event);
}
fn bridge_event_description(event: &BridgeEvent) -> String {
    let ability_description = |name: &str, ability: &AbilityTrigger| {
        let ability_name = ability
            .ability_name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let charges = match (ability.charges_before, ability.charges_after) {
            (Some(before), Some(after)) => format!(", charges {before}→{after}"),
            _ => String::new(),
        };
        format!(
            "{name}, slot {}{ability_name}, detection {}{charges}",
            ability.ability_slot, ability.detection
        )
    };
    let count_description = |count: &CountTrigger| {
        let counts = match (count.count_before, count.count_after) {
            (Some(before), Some(after)) => format!(", count {before}→{after}"),
            _ => String::new(),
        };
        format!("detection {}{counts}", count.detection)
    };
    match event {
        BridgeEvent::HookReady(_)
        | BridgeEvent::LocalPlayerDeath(_)
        | BridgeEvent::LocalPlayerRespawn(_) => event.event_name().to_owned(),
        BridgeEvent::AbilityCatalog(catalog) => {
            format!("ability_catalog, {} slot(s)", catalog.abilities.len())
        }
        BridgeEvent::LocalPlayerKill(count) => {
            format!("local_player_kill, {}", count_description(count))
        }
        BridgeEvent::LocalPlayerAssist(count) => {
            format!("local_player_assist, {}", count_description(count))
        }
        BridgeEvent::AllyHealed(count) => {
            format!("ally_healed, {}", count_description(count))
        }
        BridgeEvent::AllyShielded(count) => {
            format!("ally_shielded, {}", count_description(count))
        }
        BridgeEvent::AbilityUsed(ability) => ability_description("ability_used", ability),
        BridgeEvent::AbilityCooldownReady(ability) => {
            ability_description("ability_cooldown_ready", ability)
        }
        BridgeEvent::DamageTaken(vitals) => {
            format!("damage_taken, {:.0} lost, detection {}", vitals.amount, vitals.detection)
        }
        BridgeEvent::HealingReceived(vitals) => {
            format!(
                "healing_received, {:.0} restored, detection {}",
                vitals.amount, vitals.detection
            )
        }
        BridgeEvent::DamageGiven(vitals) => {
            format!("damage_given, {:.0} dealt, detection {}", vitals.amount, vitals.detection)
        }
        BridgeEvent::SoulDeny(count) => format!("soul_deny, {}", count_description(count)),
        BridgeEvent::SoulSecure(count) => format!("soul_secure, {}", count_description(count)),
        BridgeEvent::ParrySuccess(count) => format!("parry_success, {}", count_description(count)),
        BridgeEvent::ParryFail(count) => format!("parry_fail, {}", count_description(count)),
        BridgeEvent::ObjectiveGuardian(count) => {
            format!("objective_guardian, {}", count_description(count))
        }
        BridgeEvent::ObjectiveWalker(count) => {
            format!("objective_walker, {}", count_description(count))
        }
        BridgeEvent::ObjectiveBaseGuardian(count) => {
            format!("objective_base_guardian, {}", count_description(count))
        }
        BridgeEvent::ObjectiveShrine(count) => {
            format!("objective_shrine, {}", count_description(count))
        }
        BridgeEvent::ObjectivePatronWeakened(count) => {
            format!("objective_patron_weakened, {}", count_description(count))
        }
        BridgeEvent::GameWon(count) => format!("game_won, {}", count_description(count)),
        BridgeEvent::DamageTakenIntensity(count) => {
            format!("damage_taken_intensity, {}", count_description(count))
        }
    }
}
fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!("{}m {}s", duration.as_secs() / 60, duration.as_secs() % 60)
    } else {
        format!("{:.1}s", duration.as_secs_f32())
    }
}
fn input_background() -> Color32 {
    Color32::from_rgb(38, 38, 42)
}
fn text_input(ui: &mut Ui, label: &str, value: &mut String, password: bool) -> bool {
    ui.label(label);
    ui.add(
        TextEdit::singleline(value)
            .password(password)
            .desired_width(f32::INFINITY)
            .background_color(input_background()),
    )
    .changed()
}
/// Interactive editor for an [`IntensityCurve`]: an X-axis of damage taken in
/// the window, a Y-axis of vibration level (0-20), and one draggable dot per
/// control point. Returns whether the curve changed this frame.
fn draw_curve_graph(ui: &mut Ui, curve: &mut IntensityCurve) -> bool {
    use egui::{Align2, FontId, Pos2, Rect, Sense, Stroke, Vec2, pos2};

    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, 220.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Plot area, leaving room for axis labels.
    let plot = Rect::from_min_max(
        pos2(rect.left() + 46.0, rect.top() + 8.0),
        pos2(rect.right() - 12.0, rect.bottom() - 26.0),
    );

    let last_damage = curve
        .points
        .last()
        .map(|p| p.damage)
        .unwrap_or(500.0)
        .max(100.0);
    let x_max = ((last_damage * 1.35 / 100.0).ceil() * 100.0)
        .clamp(200.0, MAX_INTENSITY_DAMAGE);
    let y_max = MAX_VIBRATE_STRENGTH;

    let to_screen = |damage: f32, level: f32| -> Pos2 {
        let tx = (damage / x_max).clamp(0.0, 1.0);
        let ty = (level / y_max).clamp(0.0, 1.0);
        pos2(
            plot.left() + tx * plot.width(),
            plot.bottom() - ty * plot.height(),
        )
    };
    let from_screen = |pos: Pos2| -> (f32, f32) {
        let tx = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
        let ty = ((plot.bottom() - pos.y) / plot.height()).clamp(0.0, 1.0);
        (tx * x_max, ty * y_max)
    };

    painter.rect_filled(rect, 8.0, crate::theme::CARD);
    painter.rect_stroke(
        plot,
        0.0,
        Stroke::new(1.0, crate::theme::STROKE),
        egui::StrokeKind::Inside,
    );

    let label_font = FontId::proportional(11.0);
    // Y grid + labels (level).
    for level in [0.0_f32, 5.0, 10.0, 15.0, 20.0] {
        let y = to_screen(0.0, level).y;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, crate::theme::STROKE.gamma_multiply(0.5)),
        );
        painter.text(
            pos2(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format!("{level:.0}"),
            label_font.clone(),
            crate::theme::TEXT_DIM,
        );
    }
    // X grid + labels (damage).
    for step in 0..=4 {
        let damage = x_max * step as f32 / 4.0;
        let x = to_screen(damage, 0.0).x;
        painter.line_segment(
            [pos2(x, plot.top()), pos2(x, plot.bottom())],
            Stroke::new(1.0, crate::theme::STROKE.gamma_multiply(0.5)),
        );
        painter.text(
            pos2(x, plot.bottom() + 6.0),
            Align2::CENTER_TOP,
            format!("{damage:.0}"),
            label_font.clone(),
            crate::theme::TEXT_DIM,
        );
    }
    painter.text(
        pos2(plot.center().x, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        "damage taken in window",
        label_font.clone(),
        crate::theme::TEXT_DIM,
    );

    // The eased curve, sampled across the plot width, with a soft fill under it.
    let samples = 96usize;
    let mut line: Vec<Pos2> = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let damage = x_max * i as f32 / samples as f32;
        line.push(to_screen(damage, curve.level_at(damage)));
    }
    if line.len() >= 2 {
        let mut fill = line.clone();
        fill.push(pos2(line[line.len() - 1].x, plot.bottom()));
        fill.push(pos2(line[0].x, plot.bottom()));
        painter.add(egui::Shape::convex_polygon(
            fill,
            crate::theme::ACCENT.gamma_multiply(0.12),
            Stroke::NONE,
        ));
        painter.add(egui::Shape::line(
            line.clone(),
            Stroke::new(2.0, crate::theme::ACCENT),
        ));
    }

    // Drag state: which point index (if any) this graph is currently moving.
    let drag_id = response.id.with("dragging_point");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());

    if response.drag_started() {
        dragging = pointer.and_then(|p| {
            curve
                .points
                .iter()
                .enumerate()
                .map(|(i, pt)| (i, to_screen(pt.damage, pt.level).distance(p)))
                .filter(|(_, dist)| *dist <= 14.0)
                .min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)))
                .map(|(i, _)| i)
        });
        ui.data_mut(|d| d.insert_temp(drag_id, dragging.unwrap_or(usize::MAX)));
    }
    if dragging == Some(usize::MAX) {
        dragging = None;
    }

    let mut changed = false;
    if let (Some(index), Some(pos)) = (dragging, pointer)
        && response.dragged()
        && index < curve.points.len()
    {
        let (mut damage, level) = from_screen(pos);
        let low = if index == 0 {
            0.0
        } else {
            curve.points[index - 1].damage + 10.0
        };
        let high = if index + 1 == curve.points.len() {
            MAX_INTENSITY_DAMAGE
        } else {
            curve.points[index + 1].damage - 10.0
        };
        damage = damage.clamp(low.min(high), high.max(low));
        curve.points[index].damage = damage.round();
        curve.points[index].level = level.round().clamp(0.0, y_max);
        changed = true;
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove::<usize>(drag_id));
    }

    // Double-click to add a point; right-click a point to remove it.
    if response.double_clicked()
        && let Some(pos) = pointer
        && plot.contains(pos)
        && curve.points.len() < IntensityCurve::MAX_POINTS
    {
        let (damage, level) = from_screen(pos);
        curve.points.push(IntensityPoint {
            damage: damage.round(),
            level: level.round().clamp(0.0, y_max),
        });
        changed = true;
    }
    if response.secondary_clicked()
        && let Some(pos) = pointer
        && curve.points.len() > 2
        && let Some((index, _)) = curve
            .points
            .iter()
            .enumerate()
            .map(|(i, pt)| (i, to_screen(pt.damage, pt.level).distance(pos)))
            .filter(|(_, dist)| *dist <= 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
    {
        curve.points.remove(index);
        changed = true;
    }

    // Dots on top.
    for (i, point) in curve.points.iter().enumerate() {
        let center = to_screen(point.damage, point.level);
        let hot = dragging == Some(i)
            || pointer.map(|p| center.distance(p) <= 14.0).unwrap_or(false);
        painter.circle_filled(
            center,
            if hot { 7.0 } else { 5.5 },
            if hot {
                crate::theme::ACCENT_BRIGHT
            } else {
                crate::theme::ACCENT
            },
        );
        painter.circle_stroke(center, 6.0, Stroke::new(1.5, crate::theme::CARD));
    }

    // Live readout: where the pointer sits maps to this damage -> level.
    if dragging.is_none()
        && let Some(pos) = pointer
        && plot.contains(pos)
    {
        let (damage, _) = from_screen(pos);
        let level = curve.level_for(damage);
        let marker = to_screen(damage, curve.level_at(damage));
        painter.line_segment(
            [pos2(marker.x, plot.top()), pos2(marker.x, plot.bottom())],
            Stroke::new(1.0, crate::theme::ACCENT.gamma_multiply(0.5)),
        );
        painter.circle_filled(marker, 3.5, crate::theme::ACCENT_BRIGHT);
        painter.text(
            pos2(marker.x + 8.0, marker.y - 8.0),
            Align2::LEFT_BOTTOM,
            format!("{damage:.0} dmg -> {level}"),
            FontId::proportional(12.0),
            crate::theme::TEXT,
        );
    }

    changed
}

fn status_line(ui: &mut Ui, value: &str, color: [f32; 4]) {
    ui.colored_label(to_color(color), value);
}
fn to_color(color: [f32; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        (color[3].clamp(0.0, 1.0) * 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::VibrateMode;
    use crate::logging::CapturingWriter;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::io::Write;

    fn trigger(kind: TriggerKind, session_id: &str, sequence: u64) -> TriggerIdentity {
        let is_ability = matches!(
            kind,
            TriggerKind::AbilityUse | TriggerKind::AbilityCooldownReady
        );
        TriggerIdentity {
            kind,
            session_id: session_id.to_owned(),
            sequence,
            client_time_ms: sequence,
            detection: "test".to_owned(),
            ability_slot: is_ability.then_some(2),
            ability_name: is_ability.then(|| "Test Ability".to_owned()),
            charges_before: None,
            charges_after: None,
            amount_total: None,
            override_action: None,
        }
    }

    fn resolved(strength: u8, duration_secs: f32) -> ResolvedVibrateAction {
        ResolvedVibrateAction {
            strength,
            duration_secs,
        }
    }

    #[test]
    fn action_resolution_is_an_immutable_snapshot() {
        let mut settings = VibrateActionSettings {
            mode: VibrateMode::Fixed,
            ..Default::default()
        };
        settings.fixed.strength = 14.0;
        settings.fixed.duration_seconds = 3.0;
        let mut rng = StdRng::seed_from_u64(4);
        let snapshot = settings.resolve_with(&mut rng).unwrap();
        settings.fixed.strength = 20.0;
        assert_eq!(settings.fixed.strength, 20.0);
        assert_eq!(snapshot.strength, 14);
        assert_eq!(snapshot.duration_secs, 3.0);
    }

    #[test]
    fn invalid_fixed_action_settings_are_skipped_without_fabricating_an_action() {
        let mut state = AppState::default();
        state.triggers.death.actions.mode = VibrateMode::Fixed;
        state.triggers.death.actions.fixed.strength = 21.0;
        state.queue_trigger_action(trigger(TriggerKind::Death, "session", 1));
        let Some(ActionStatus::Skipped { snapshot, reason }) = state.action_status else {
            panic!("invalid settings should be skipped");
        };
        assert!(snapshot.resolved.is_none());
        assert!(reason.contains("invalid action settings"));
    }

    #[test]
    fn copy_transfers_only_active_action_settings() {
        let mut state = AppState::default();
        state.triggers.death.enabled = false;
        state.triggers.death.actions.mode = VibrateMode::Fixed;
        state.triggers.death.actions.fixed.strength = 15.0;
        state.triggers.ability_use.trigger.enabled = true;
        state.triggers.ability_use.ability_filter = AbilityFilter::Selected(BTreeSet::from([2]));
        assert!(state.copy_action_settings(TriggerKind::Death, TriggerKind::AbilityUse));
        assert_eq!(
            state.triggers.ability_use.trigger.actions,
            state.triggers.death.actions
        );
        assert!(state.triggers.ability_use.trigger.enabled);
        assert_eq!(
            state.triggers.ability_use.ability_filter,
            AbilityFilter::Selected(BTreeSet::from([2]))
        );
    }

    #[test]
    fn action_queue_preserves_capacity_and_expiry() {
        let (sender, receiver) = mpsc::sync_channel(ACTION_QUEUE_CAPACITY);
        for value in 0..ACTION_QUEUE_CAPACITY {
            assert!(sender.try_send(value).is_ok());
        }
        assert!(matches!(
            sender.try_send(ACTION_QUEUE_CAPACITY),
            Err(TrySendError::Full(_))
        ));
        drop(receiver);
        assert!(matches!(
            sender.try_send(0),
            Err(TrySendError::Disconnected(_))
        ));
        let queued_at = Instant::now();
        assert!(action_job_expired_at(
            queued_at,
            queued_at + MAX_ACTION_QUEUE_AGE
        ));
    }

    #[test]
    fn queue_outcomes_use_generic_action_statuses() {
        let request = ActionRequest {
            target: None,
            resolved: resolved(15, 3.0),
            trigger: trigger(TriggerKind::Death, "session", 1),
            queued_at: Instant::now(),
        };
        let mut full = AppState::default();
        full.apply_action_enqueue_result(request.clone(), ActionEnqueueResult::Full);
        assert!(matches!(
            full.action_status,
            Some(ActionStatus::Skipped { reason, .. }) if reason == "action queue is full"
        ));
        let mut disconnected = AppState::default();
        disconnected
            .apply_action_enqueue_result(request.clone(), ActionEnqueueResult::Disconnected);
        assert!(matches!(
            disconnected.action_status,
            Some(ActionStatus::Failed { error, .. }) if error == "action worker is unavailable"
        ));
        let mut accepted = AppState::default();
        accepted.apply_action_enqueue_result(request, ActionEnqueueResult::Accepted);
        assert_eq!(accepted.action_in_flight, 1);
        assert!(matches!(
            accepted.action_status,
            Some(ActionStatus::Sending(_))
        ));
    }

    #[test]
    fn global_sequence_watermark_is_preserved_across_trigger_kinds() {
        let mut state = AppState::default();
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "first", 4));
        assert_eq!(state.last_sequence, Some(("first".to_owned(), 4)));
        state.queue_trigger_action(trigger(TriggerKind::Death, "first", 3));
        state.queue_trigger_action(trigger(TriggerKind::Death, "first", 5));
        assert_eq!(state.last_sequence, Some(("first".to_owned(), 5)));
    }

    #[test]
    fn kill_and_assist_triggers_queue_independently_of_death() {
        let mut state = AppState {
            devices: vec![ProviderTarget::new("toy-1", "hub")],
            selected_device: Some("toy-1".to_owned()),
            ..AppState::default()
        };
        state.triggers.kill.enabled = true;
        state.triggers.assist.enabled = true;
        state.queue_trigger_action(trigger(TriggerKind::Kill, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. }) if reason == "provider is not connected"
        ));
        state.queue_trigger_action(trigger(TriggerKind::Assist, "session", 2));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. }) if reason == "provider is not connected"
        ));
    }

    #[test]
    fn disabled_kill_trigger_is_ignored() {
        let mut state = AppState::default();
        assert!(!state.triggers.kill.enabled);
        state.queue_trigger_action(trigger(TriggerKind::Kill, "session", 1));
        assert!(state.action_status.is_none());
    }

    #[test]
    fn priority_order_defaults_and_normalization_keeps_every_kind_ranked() {
        let mut set = TriggerSettingsSet::default();
        assert_eq!(set.priority_order, PRIORITY_ORDER_DEFAULT.to_vec());
        set.priority_order = vec![TriggerKind::Kill, TriggerKind::Kill];
        set.normalize_priority_order();
        assert_eq!(set.priority_order[0], TriggerKind::Kill);
        assert_eq!(set.priority_order.len(), PRIORITY_ORDER_DEFAULT.len());
        for kind in PRIORITY_ORDER_DEFAULT {
            assert!(set.priority_order.contains(&kind));
        }
    }

    #[test]
    fn lower_priority_trigger_is_held_back_while_a_higher_one_plays() {
        let mut state = AppState::default();
        state.triggers.kill.enabled = true;
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::Death),
            ends_at: Instant::now() + Duration::from_secs(5),
        });
        state.queue_trigger_action(trigger(TriggerKind::Kill, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "a higher-priority effect is still playing"
        ));
    }

    #[test]
    fn higher_priority_trigger_preempts_a_running_lower_one() {
        let mut state = AppState::default();
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::AbilityUse),
            ends_at: Instant::now() + Duration::from_secs(5),
        });
        // Death outranks AbilityUse, so it clears the priority gate and then
        // trips the usual not-connected skip instead.
        state.queue_trigger_action(trigger(TriggerKind::Death, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "provider is not connected"
        ));
    }

    #[test]
    fn expired_active_action_stops_blocking_lower_priority_triggers() {
        let mut state = AppState::default();
        state.triggers.kill.enabled = true;
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::Death),
            ends_at: Instant::now() - Duration::from_secs(1),
        });
        state.queue_trigger_action(trigger(TriggerKind::Kill, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "provider is not connected"
        ));
    }

    #[test]
    fn hold_until_respawn_swaps_only_the_death_duration_for_the_safety_cap() {
        let base = resolved(14, 3.0);
        assert_eq!(
            resolved_for_trigger(TriggerKind::Death, true, base),
            resolved(14, RESPAWN_HOLD_SAFETY_CAP_SECS as f32)
        );
        assert_eq!(resolved_for_trigger(TriggerKind::Death, false, base), base);
        assert_eq!(resolved_for_trigger(TriggerKind::Kill, true, base), base);
    }

    #[test]
    fn resting_send_is_due_on_backoff_when_behind_and_on_heartbeat_when_matched() {
        assert!(!resting_send_due(None, 5, Duration::from_millis(500)));
        assert!(resting_send_due(None, 5, RESTING_RETRY_BACKOFF));
        assert!(!resting_send_due(Some(5), 5, RESTING_RETRY_BACKOFF));
        assert!(resting_send_due(Some(5), 5, RESTING_REASSERT_INTERVAL));
        assert!(resting_send_due(Some(3), 5, RESTING_RETRY_BACKOFF));
    }

    #[test]
    fn a_disabled_baseline_stays_hands_off_until_it_has_driven_the_toy() {
        // Never touched, or already idle: do not send a stray Stop that would
        // cut a partner's phone-driven vibration.
        assert!(!resting_should_act(0, None));
        assert!(!resting_should_act(0, Some(0)));
        assert!(resting_at_rest(0, None));
        assert!(resting_at_rest(0, Some(0)));
        // Winding down from a real level still needs one Stop.
        assert!(resting_should_act(0, Some(6)));
        assert!(!resting_at_rest(0, Some(6)));
        // A real baseline always wants asserting until it matches.
        assert!(resting_should_act(5, None));
        assert!(!resting_at_rest(5, Some(3)));
        assert!(resting_at_rest(5, Some(5)));
    }

    #[test]
    fn respawn_without_a_pending_hold_leaves_a_running_effect_alone() {
        let mut state = AppState::default();
        let ends_at = Instant::now() + Duration::from_secs(20);
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::Death),
            ends_at,
        });
        state.resting_applied = Some(4);
        state.handle_respawn(&LocalPlayerRespawn {
            schema: 1,
            session_id: "session".to_owned(),
            client_time_ms: 1,
        });
        assert!(matches!(state.active_action, Some(a) if a.ends_at == ends_at));
        assert_eq!(state.resting_applied, Some(4));
    }

    #[test]
    fn ability_and_assist_triggers_are_ignored_while_awaiting_respawn() {
        let mut state = AppState {
            awaiting_respawn: true,
            ..AppState::default()
        };
        state.triggers.assist.enabled = true;
        state.triggers.ability_use.trigger.enabled = true;

        state.queue_trigger_action(trigger(TriggerKind::Assist, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "ignored while you're dead"
        ));

        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "session", 2));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "ignored while you're dead"
        ));

        // Death itself must still get through the gate.
        state.queue_trigger_action(trigger(TriggerKind::Death, "session", 3));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "provider is not connected"
        ));
    }

    #[test]
    fn suppress_toggle_off_lets_ability_triggers_through_while_awaiting_respawn() {
        let mut state = AppState {
            awaiting_respawn: true,
            ..AppState::default()
        };
        state.triggers.suppress_triggers_while_dead = false;
        state.triggers.ability_use.trigger.enabled = true;
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { ref reason, .. })
                if reason == "provider is not connected"
        ));
    }

    #[test]
    fn respawn_event_clears_awaiting_respawn_and_the_active_effect() {
        let mut state = AppState {
            awaiting_respawn: true,
            resting_applied: Some(6),
            ..AppState::default()
        };
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::Death),
            ends_at: Instant::now() + Duration::from_secs(30),
        });
        state.handle_respawn(&LocalPlayerRespawn {
            schema: 1,
            session_id: "session".to_owned(),
            client_time_ms: 1,
        });
        assert!(!state.awaiting_respawn);
        assert!(state.active_action.is_none());
        // No provider is connected in this test, so there is nothing to send
        // a stop to; force_stop_toy leaves resting_applied untouched rather
        // than fabricating a result. maintain_resting's own no-provider guard
        // (exercised below) is what actually resets it once ticked.
        assert_eq!(state.resting_applied, Some(6));
    }

    #[test]
    fn manual_emergency_stop_clears_active_effect_tracking_even_without_a_provider() {
        let mut state = AppState::default();
        state.active_action = Some(ActiveAction {
            priority_rank: state.triggers.priority_rank(TriggerKind::Death),
            ends_at: Instant::now() + Duration::from_secs(RESPAWN_HOLD_SAFETY_CAP_SECS.into()),
        });
        state.awaiting_respawn = true;
        state.force_stop_toy("test");
        assert!(state.active_action.is_none());
        assert!(!state.awaiting_respawn);
    }

    #[test]
    fn maintain_resting_without_a_provider_forgets_the_applied_baseline() {
        let mut state = AppState {
            resting_strength: 8,
            resting_applied: Some(8),
            ..AppState::default()
        };
        state.maintain_resting();
        assert!(state.resting_applied.is_none());
    }

    #[test]
    fn action_status_contains_provider_target_and_trigger_details() {
        let status = ActionStatus::Sent(ActionRequest {
            target: Some(ProviderTarget::new("group".to_owned(), "group")),
            resolved: resolved(15, 3.0),
            trigger: {
                let mut trigger = trigger(TriggerKind::AbilityCooldownReady, "session", 9);
                trigger.detection = "charge_restored".to_owned();
                trigger.charges_before = Some(1);
                trigger.charges_after = Some(2);
                trigger
            },
            queued_at: Instant::now(),
        });
        let label = status.label();
        assert!(label.contains("Lovense"));
        assert!(label.contains("group"));
        assert!(label.contains("15/20 for 3 s"));
        assert!(label.contains("ability cooldown ready slot 2"));
        assert!(label.contains("charge_restored"));
        assert!(label.contains("charges 1→2"));
    }

    #[test]
    fn reset_clears_durable_banks_and_runtime_action_state() {
        let mut state = AppState::default();
        state.provider_settings.lovense.domain = "custom.lan".into();
        state.triggers.death.actions.mode = VibrateMode::Fixed;
        assert!(state.reset_saved_state());
        assert_eq!(state.provider_settings, ProviderSettings::default());
        assert!(state.runtime_trigger_and_action_state_is_clear());
    }
    #[test]
    fn preferred_target_is_reconciled_against_fresh_targets() {
        let preferred: TargetId = "toy-2".to_owned();
        let mut state = AppState {
            preferred_target: Some(preferred.clone()),
            ..AppState::default()
        };
        state.apply_devices(vec![
            ProviderTarget::new("toy-1", "Alpha"),
            ProviderTarget::new(preferred.clone(), "Beta"),
        ]);
        assert_eq!(state.selected_device, Some(preferred.clone()));
        assert_eq!(
            state.selected_device().map(ProviderTarget::name),
            Some("Beta")
        );
        state.reset_connection();
        assert!(state.selected_device.is_none());
        assert_eq!(state.preferred_target, Some(preferred.clone()));
        state.apply_connection_result(Err(ProviderError::NotConnected));
        assert_eq!(state.preferred_target, Some(preferred));
        state.apply_devices(vec![ProviderTarget::new("toy-3", "Gamma")]);
        assert_eq!(state.selected_device, Some("toy-3".to_owned()));
        assert!(!state.select_device("toy-99".to_owned()));
    }

    #[test]
    fn failed_connection_clears_stale_live_targets_but_preserves_preference() {
        let mut state = AppState {
            devices: vec![ProviderTarget::new("toy-1", "hub")],
            selected_device: Some("toy-1".to_owned()),
            preferred_target: Some("toy-1".to_owned()),
            ..AppState::default()
        };
        state.apply_connection_result(Err(ProviderError::NotConnected));
        assert!(state.devices.is_empty());
        assert!(state.selected_device.is_none());
        assert_eq!(state.preferred_target, Some("toy-1".to_owned()));
        assert_eq!(state.credential_state, CredentialState::Invalid);
    }

    #[test]
    fn test_action_status_reports_success_and_failure() {
        let mut state = AppState::default();
        state.apply_test_action_result(Ok(()));
        assert_eq!(state.test_action_status, Some(TestActionStatus::Sent));
        state.apply_test_action_result(Err(ProviderError::NotConnected));
        assert!(matches!(
            state.test_action_status,
            Some(TestActionStatus::Failed(_))
        ));
    }

    #[test]
    fn all_effect_editors_render_both_vibrate_modes() {
        for kind in PRIORITY_ORDER_DEFAULT {
            for mode in [VibrateMode::Interval, VibrateMode::Fixed] {
                let context = egui::Context::default();
        crate::theme::install_fonts(&context);
                let mut state = AppState {
                    selected_section: AppSection::Effects,
                    selected_effect: kind,
                    ..AppState::default()
                };
                state.triggers.get_mut(kind).actions.mode = mode;
                if matches!(
                    kind,
                    TriggerKind::AbilityUse | TriggerKind::AbilityCooldownReady
                ) {
                    state
                        .ability_catalog
                        .insert(1, Some("Power Slash".to_owned()));
                }
                let output = context.run_ui(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| state.draw(ui));
                });
                assert!(!output.shapes.is_empty());
            }
        }
    }

    #[test]
    fn detection_status_updates_path_and_guidance() {
        let path = PathBuf::from("/steam/Deadlock/game/citadel/console.log");
        let mut state = AppState::default();
        state.apply_log_detection(Ok(Detection::Ready { path: path.clone() }));
        assert_eq!(state.log_path, path.display().to_string());
        assert_eq!(state.log_detection_status, Some(LogDetectionStatus::Found));
        state.apply_log_detection(Ok(Detection::NotCreated { path }));
        assert!(
            state
                .log_detection_status
                .as_ref()
                .expect("status")
                .label()
                .contains("-condebug")
        );
        state.log_path = "/manual/console.log".into();
        state.apply_log_detection(Err(DetectionError::DeadlockNotInstalled));
        assert_eq!(state.log_path, "/manual/console.log");
    }

    #[test]
    fn manual_listener_restart_uses_configured_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("console.log");
        std::fs::write(&path, b"").unwrap();
        let mut state = AppState {
            log_path: path.display().to_string(),
            ..AppState::default()
        };

        state.start_listener_from_input();

        let status = state.bridge_listener.status();
        assert_eq!(status.configured_path, Some(path));
        assert_eq!(status.phase, ListenerPhase::Listening);
        assert!(state.bridge_events.is_some());
        assert!(state.listener_action_error.is_none());
    }

    #[test]
    fn startup_saved_path_starts_without_detection() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        let log_path = directory.path().join("console.log");
        std::fs::write(&log_path, b"").unwrap();
        {
            let mut first_launch =
                CompanionApp::load_from_path_with_detector(state_path.clone(), || {
                    Err(DetectionError::DeadlockNotInstalled)
                });
            first_launch.state.log_path = format!("  {}  ", log_path.display());
            first_launch.flush_pending();
            assert!(first_launch.persistence.save_error().is_none());
        }

        let app = CompanionApp::load_from_path_with_detector(
            state_path,
            || -> Result<Detection, DetectionError> {
                panic!("saved path startup must not detect");
            },
        );

        let status = app.state.bridge_listener.status();
        assert_eq!(app.state.log_path, log_path.display().to_string());
        assert!(app.state.bridge_events.is_some());
        assert_eq!(status.configured_path, Some(log_path));
        assert_eq!(status.phase, ListenerPhase::Listening);
        assert!(app.state.listener_action_error.is_none());
    }

    #[test]
    fn startup_saved_missing_path_waits_without_detection_or_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("console.log");
        let mut state = AppState {
            log_path: path.display().to_string(),
            ..AppState::default()
        };

        state.initialize_log_listener(|| -> Result<Detection, DetectionError> {
            panic!("saved path startup must not detect");
        });

        let status = state.bridge_listener.status();
        assert_eq!(status.configured_path, Some(path));
        assert_eq!(status.phase, ListenerPhase::WaitingForFile);
        assert!(state.listener_action_error.is_none());
    }

    #[test]
    fn startup_empty_state_ready_detection_starts_listener() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("console.log");
        std::fs::write(&path, b"").unwrap();
        let mut state = AppState::default();

        state.initialize_log_listener(|| Ok(Detection::Ready { path: path.clone() }));

        let status = state.bridge_listener.status();
        assert_eq!(state.log_path, path.display().to_string());
        assert_eq!(state.log_detection_status, Some(LogDetectionStatus::Found));
        assert_eq!(status.configured_path, Some(path));
        assert_eq!(status.phase, ListenerPhase::Listening);
    }

    #[test]
    fn startup_empty_state_not_created_detection_waits_with_guidance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("console.log");
        let mut state = AppState::default();

        state.initialize_log_listener(|| Ok(Detection::NotCreated { path: path.clone() }));

        let status = state.bridge_listener.status();
        assert_eq!(state.log_path, path.display().to_string());
        assert_eq!(
            state.log_detection_status,
            Some(LogDetectionStatus::NotCreated)
        );
        assert!(
            state
                .log_detection_status
                .as_ref()
                .expect("not-created status")
                .label()
                .contains("-condebug")
        );
        assert_eq!(status.configured_path, Some(path));
        assert_eq!(status.phase, ListenerPhase::WaitingForFile);
    }

    #[test]
    fn startup_detection_failure_is_non_fatal_and_stays_stopped() {
        let mut state = AppState::default();

        state.initialize_log_listener(|| Err(DetectionError::DeadlockNotInstalled));

        assert!(state.log_path.is_empty());
        assert_eq!(state.bridge_listener.status().phase, ListenerPhase::Stopped);
        assert!(state.bridge_events.is_none());
        assert!(state.listener_action_error.is_none());
        assert!(matches!(
            state.log_detection_status,
            Some(LogDetectionStatus::Failed(_))
        ));
    }

    #[test]
    fn startup_reset_stops_listener_without_second_detection() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        let log_path = directory.path().join("console.log");
        let detection_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let detector_calls = Arc::clone(&detection_calls);
        let mut app = CompanionApp::load_from_path_with_detector(state_path, move || {
            detector_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Detection::NotCreated {
                path: log_path.clone(),
            })
        });

        assert_eq!(detection_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            app.state.bridge_listener.status().phase,
            ListenerPhase::WaitingForFile
        );
        assert!(app.reset_and_save());
        assert_eq!(
            app.state.bridge_listener.status().phase,
            ListenerPhase::Stopped
        );
        assert_eq!(detection_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn expired_completion_is_skipped_and_decrements_in_flight() {
        let request = ActionRequest {
            target: None,
            resolved: resolved(15, 3.0),
            trigger: trigger(TriggerKind::Death, "session", 1),
            queued_at: Instant::now(),
        };
        let (sender, receiver) = mpsc::channel();
        let mut state = AppState {
            action_result: receiver,
            action_in_flight: 1,
            ..AppState::default()
        };
        sender
            .send(ActionCompletion {
                request,
                result: ActionCompletionResult::Skipped { reason: "expired" },
            })
            .unwrap();
        state.poll_action();
        assert_eq!(state.action_in_flight, 0);
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { reason, .. }) if reason == "expired"
        ));
    }

    #[test]
    fn actionable_events_share_global_watermark_and_disabled_filtering_advances_it() {
        let mut state = AppState::default();
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "first", 4));
        assert_eq!(state.last_sequence, Some(("first".to_owned(), 4)));
        state.queue_trigger_action(trigger(TriggerKind::Death, "first", 3));
        state.triggers.ability_use.trigger.enabled = true;
        state.triggers.ability_use.ability_filter = AbilityFilter::Selected(BTreeSet::new());
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "first", 5));
        assert_eq!(state.last_sequence, Some(("first".to_owned(), 5)));
        state.triggers.ability_use.ability_filter = AbilityFilter::All;
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "first", 5));
        assert!(state.action_status.is_none());
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "first", 6));
        assert_eq!(state.last_sequence, Some(("first".to_owned(), 6)));
    }

    #[test]
    fn parsed_enabled_ability_event_reaches_action_queue_path() {
        let event = crate::bridge_listener::parse_bridge_record(
            "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_cooldown_ready\",\"session_id\":\"session\",\"client_time_ms\":7,\"sequence\":3,\"ability_slot\":2,\"ability_name\":\"Bookwyrm\",\"detection\":\"charge_restored\",\"charges_before\":1,\"charges_after\":2}",
        )
        .expect("valid ability event");
        let (sender, receiver) = mpsc::channel();
        let mut state = AppState {
            bridge_events: Some(receiver),
            ..AppState::default()
        };
        state.triggers.ability_cooldown_ready.trigger.enabled = true;
        sender.send(event).unwrap();
        state.poll_bridge_events();
        assert_eq!(state.last_sequence, Some(("session".to_owned(), 3)));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { .. })
        ));
        assert_eq!(state.action_in_flight, 0);
    }

    #[test]
    fn hook_ready_and_catalog_events_do_not_advance_actionable_state() {
        let (sender, receiver) = mpsc::channel();
        let mut state = AppState {
            bridge_events: Some(receiver),
            ..AppState::default()
        };
        state
            .ability_catalog
            .insert(1, Some("Stale name".to_owned()));
        sender
            .send(BridgeEvent::HookReady(crate::bridge_listener::HookReady {
                schema: 1,
                session_id: "session".to_owned(),
                client_time_ms: 1,
                poll_interval_ms: 100,
            }))
            .unwrap();
        sender
            .send(BridgeEvent::AbilityCatalog(
                crate::bridge_listener::AbilityCatalog {
                    schema: 1,
                    session_id: "session".to_owned(),
                    client_time_ms: 2,
                    abilities: vec![crate::bridge_listener::AbilityCatalogEntry {
                        ability_slot: 2,
                        ability_name: Some("Replacement".to_owned()),
                    }],
                },
            ))
            .unwrap();
        state.poll_bridge_events();
        assert!(state.last_sequence.is_none());
        assert_eq!(
            state.ability_catalog.get(&2),
            Some(&Some("Replacement".to_owned()))
        );
    }

    #[test]
    fn trigger_routing_resolves_selected_profile_before_status() {
        let mut state = AppState::default();
        state.triggers.ability_use.trigger.enabled = true;
        state.triggers.ability_use.trigger.actions.mode = VibrateMode::Fixed;
        state.triggers.ability_use.trigger.actions.fixed.strength = 17.0;
        state
            .triggers
            .ability_use
            .trigger
            .actions
            .fixed
            .duration_seconds = 5.0;
        state.queue_trigger_action(trigger(TriggerKind::AbilityUse, "session", 1));
        let Some(status) = state.action_status.clone() else {
            panic!("enabled trigger should record missing-provider status");
        };
        assert_eq!(status.snapshot().resolved, Some(resolved(17, 5.0)));
        state.triggers.ability_use.trigger.actions.fixed.strength = 5.0;
        assert_eq!(status.snapshot().resolved, Some(resolved(17, 5.0)));
    }

    fn vitals(amount: f64, sequence: u64) -> VitalsTrigger {
        VitalsTrigger {
            schema: 1,
            session_id: "session".to_owned(),
            client_time_ms: sequence,
            sequence,
            detection: "test".to_owned(),
            amount,
            health_after: Some(300.0),
        }
    }

    #[test]
    fn amount_trigger_stays_quiet_below_threshold_and_fires_once_it_clears() {
        let mut state = AppState::default();
        state.triggers.damage_taken.trigger.enabled = true;
        state.triggers.damage_taken.threshold = 200.0;
        state.triggers.damage_taken.window_seconds = 3.0;

        assert!(
            state
                .accumulate_amount_trigger(TriggerKind::DamageTaken, vitals(120.0, 1))
                .is_none()
        );
        let fired = state
            .accumulate_amount_trigger(TriggerKind::DamageTaken, vitals(90.0, 2))
            .expect("threshold cleared over two samples");
        assert_eq!(fired.kind, TriggerKind::DamageTaken);
        assert_eq!(fired.amount_total, Some(210.0));

        // The window was drained on firing, so a small follow-up sample does
        // not immediately re-trigger.
        assert!(
            state
                .accumulate_amount_trigger(TriggerKind::DamageTaken, vitals(10.0, 3))
                .is_none()
        );
    }

    #[test]
    fn disabled_amount_trigger_never_accumulates() {
        let mut state = AppState::default();
        state.triggers.healing_received.trigger.enabled = false;
        state.triggers.healing_received.threshold = 50.0;
        assert!(
            state
                .accumulate_amount_trigger(TriggerKind::HealingReceived, vitals(999.0, 1))
                .is_none()
        );
    }

    #[test]
    fn amount_ledger_drops_samples_older_than_its_window() {
        let mut ledger = AmountLedger::default();
        let start = Instant::now();
        assert_eq!(ledger.record(100.0, 1.0, start), 100.0);
        let later = start + Duration::from_millis(1500);
        // The first sample has aged out of a 1s window by now, so only the
        // second one should count.
        assert_eq!(ledger.record(50.0, 1.0, later), 50.0);
    }

    #[test]
    fn ability_filters_cover_all_selected_empty_and_unknown_slots() {
        assert!(AbilityFilter::All.accepts(1));
        assert!(AbilityFilter::All.accepts(999));
        let selected = AbilityFilter::Selected(BTreeSet::from([2, 5]));
        assert!(!selected.accepts(1));
        assert!(selected.accepts(2));
        assert!(!selected.accepts(999));
        assert!(!AbilityFilter::Selected(BTreeSet::new()).accepts(2));
    }

    #[test]
    fn selecting_effect_updates_editor_and_copy_source() {
        let mut state = AppState {
            selected_effect: TriggerKind::Death,
            copy_source: TriggerKind::AbilityUse,
            copy_feedback: Some("old confirmation".to_owned()),
            ..AppState::default()
        };
        state.select_effect(TriggerKind::AbilityUse);
        assert_eq!(state.selected_effect, TriggerKind::AbilityUse);
        assert_eq!(state.copy_source, TriggerKind::Death);
        assert!(state.copy_feedback.is_none());
    }

    #[test]
    fn switching_profiles_swaps_the_live_effects_and_keeps_edits() {
        let mut state = AppState::default();
        state.triggers.kill.enabled = true;
        state.resting_strength = 6;

        state.add_blank_profile(); // fresh second profile, now active
        assert_eq!(state.active_profile, 1);
        assert!(!state.triggers.kill.enabled, "a fresh profile starts from defaults");
        assert!(state.triggers.death.enabled, "a fresh profile has the death effect on");
        assert!(
            state.triggers.death.actions.resolve().is_some_and(|a| a.strength > 0),
            "a fresh profile's death effect is actually felt, not a silent zero"
        );
        assert_eq!(state.resting_strength, 0);

        state.triggers.death.actions.fixed.strength = 15.0;

        state.switch_profile(0);
        assert_eq!(state.active_profile, 0);
        assert!(state.triggers.kill.enabled, "first profile's effects came back");
        assert_eq!(state.resting_strength, 6);

        state.switch_profile(1);
        assert_eq!(
            state.triggers.death.actions.fixed.strength, 15.0,
            "the edit made while profile 1 was active was retained"
        );
    }

    #[test]
    fn duplicate_copies_effects_and_delete_keeps_selection_and_last_profile() {
        let mut state = AppState::default();
        state.triggers.assist.enabled = true; // profile 0 "Default"

        state.duplicate_profile(0); // profile 1: a copy, now active
        assert!(state.triggers.assist.enabled, "the copy started from the source's effects");
        state.triggers.assist.enabled = false; // edit only the copy

        state.add_blank_profile(); // profile 2: fresh, now active
        assert_eq!(state.active_profile, 2);
        assert_eq!(state.profiles.len(), 3);

        // Deleting a profile below the active one shifts the active index down.
        state.delete_profile(0);
        assert_eq!(state.profiles.len(), 2);
        assert_eq!(state.active_profile, 1, "active followed its profile down");

        // Deleting the active profile itself clamps into range.
        state.delete_profile(1);
        assert_eq!(state.profiles.len(), 1);
        assert_eq!(state.active_profile, 0);
        assert!(
            !state.triggers.assist.enabled,
            "the surviving profile is the edited copy, and its effects are live"
        );

        // The last profile can't be deleted.
        state.delete_profile(0);
        assert_eq!(state.profiles.len(), 1);
    }

    #[test]
    fn a_new_profile_drops_straight_into_renaming_itself() {
        let mut state = AppState::default();
        state.add_blank_profile();
        assert_eq!(state.profiles.len(), 2);
        assert_eq!(state.active_profile, 1);
        assert_eq!(state.renaming_profile, Some(1), "the new chip is being renamed");
        assert!(state.renaming_needs_focus);

        // Committing a blank name falls back to a fresh unique placeholder.
        state.profiles[1].name = "   ".to_owned();
        state.commit_profile_rename(1);
        assert_eq!(state.renaming_profile, None);
        assert!(!state.profiles[1].name.trim().is_empty());
        assert_ne!(state.profiles[0].name, state.profiles[1].name);
    }

    #[test]
    fn begin_rename_marks_the_chip_and_ignores_an_out_of_range_index() {
        let mut state = AppState::default();
        state.add_blank_profile(); // now 2 profiles, active is 1
        state.begin_rename_profile(0);
        assert_eq!(state.renaming_profile, Some(0));
        assert!(state.renaming_needs_focus);

        // Switching to a different profile cancels an in-progress rename.
        state.switch_profile(0);
        assert_eq!(state.renaming_profile, None);

        state.begin_rename_profile(99);
        assert_eq!(state.renaming_profile, None);
    }

    #[test]
    fn reset_is_blocked_by_each_in_flight_work_kind() {
        let mut connection_busy = AppState {
            provider_settings: ProviderSettings {
                lovense: crate::provider::LovenseSetup {
                    domain: "keep.lan".to_owned(),
                    ..Default::default()
                },
            },
            ..AppState::default()
        };
        let (_sender, receiver) = mpsc::channel();
        connection_busy.connection_result = Some(receiver);
        assert!(!connection_busy.reset_saved_state());
        assert_eq!(connection_busy.provider_settings.lovense.domain, "keep.lan");
        let mut test_busy = AppState::default();
        let (_sender, receiver) = mpsc::channel();
        test_busy.test_action_result = Some(receiver);
        assert!(!test_busy.reset_saved_state());
        let mut action_busy = AppState {
            action_in_flight: 1,
            ..AppState::default()
        };
        assert!(!action_busy.reset_saved_state());
        let mut refresh_busy = AppState::default();
        let (_sender, receiver) = mpsc::channel();
        refresh_busy.device_refresh_result = Some(receiver);
        assert!(refresh_busy.is_busy());
        assert!(!refresh_busy.reset_saved_state());
    }

    #[test]
    fn device_refresh_replaces_stale_toy_list_without_reconnecting() {
        let mut state = AppState {
            devices: vec![ProviderTarget::new("stale-toy", "Stale")],
            selected_device: Some("stale-toy".to_owned()),
            ..AppState::default()
        };
        let (sender, receiver) = mpsc::channel();
        state.device_refresh_result = Some(receiver);
        sender
            .send(Ok(vec![ProviderTarget::new("fresh-toy", "Fresh")]))
            .unwrap();
        state.poll_device_refresh();
        assert!(state.device_refresh_result.is_none());
        assert_eq!(state.devices, vec![ProviderTarget::new("fresh-toy", "Fresh")]);
        assert_eq!(state.selected_device, Some("fresh-toy".to_owned()));
    }

    #[test]
    fn reset_clears_listener_runtime_and_writes_durable_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut app = CompanionApp::load_from_path_with_detector(path.clone(), || {
            Err(DetectionError::DeadlockNotInstalled)
        });
        app.state.provider_settings.lovense.domain = "custom.lan".to_owned();
        app.state.preferred_target = Some("group".to_owned());
        app.state.triggers.death.actions.mode = VibrateMode::Fixed;
        app.state.triggers.death.actions.fixed.strength = 18.0;
        app.state.log_path = directory.path().join("console.log").display().to_string();
        let _ = app
            .state
            .start_log_listener(PathBuf::from(&app.state.log_path));
        assert!(app.reset_and_save());
        assert_eq!(
            PersistedState::from_app(&app.state),
            PersistedState::default()
        );
        assert!(!app.state.listener_is_running());
        assert!(app.state.runtime_trigger_and_action_state_is_clear());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            serde_json::to_string_pretty(&PersistedState::default()).unwrap() + "\n"
        );
    }

    #[test]
    fn persistence_aware_app_renders_with_injected_state_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut app =
            CompanionApp::load_from_path_with_detector(directory.path().join("state.json"), || {
                Err(DetectionError::DeadlockNotInstalled)
            });
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let output = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw(ui));
        });
        assert!(!output.shapes.is_empty());
    }
    #[test]
    fn injected_logs_render_and_reopen_without_persisted_state_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LogStore::new();
        let mut writer = CapturingWriter::new(store.clone(), Vec::<u8>::new());
        writer.write_all(b"startup_record\nlive_record\n").unwrap();
        let mut app = CompanionApp::load_from_path_with_detector_and_store(
            directory.path().join("state.json"),
            || Err(DetectionError::DeadlockNotInstalled),
            store,
        );
        let persisted = PersistedState::from_app(&app.state);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        app.logs_window_open = true;
        let output = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw(ui));
        });
        assert!(!output.shapes.is_empty());
        assert!(app.logs_cached_text.contains("startup_record"));
        assert!(app.logs_cached_text.contains("live_record"));
        app.logs_window_open = false;
        let _ = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw(ui));
        });
        app.logs_window_open = true;
        let _ = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.draw(ui));
        });
        assert_eq!(PersistedState::from_app(&app.state), persisted);
    }
    #[test]
    fn completed_action_decrements_in_flight_and_records_sent_status() {
        let request = ActionRequest {
            target: None,
            resolved: resolved(15, 3.0),
            trigger: trigger(TriggerKind::Death, "session", 1),
            queued_at: Instant::now(),
        };
        let (sender, receiver) = mpsc::channel();
        let mut state = AppState {
            action_result: receiver,
            action_in_flight: 1,
            ..AppState::default()
        };
        sender
            .send(ActionCompletion {
                request,
                result: ActionCompletionResult::Completed(Ok(())),
            })
            .unwrap();
        state.poll_action();
        assert_eq!(state.action_in_flight, 0);
        assert!(matches!(state.action_status, Some(ActionStatus::Sent(_))));
    }

    #[test]
    fn no_connection_reports_skip_with_or_without_a_selected_toy() {
        let mut state = AppState::default();
        state.queue_trigger_action(trigger(TriggerKind::Death, "session", 1));
        assert!(matches!(
            state.action_status,
            Some(ActionStatus::Skipped { reason, .. }) if reason == "provider is not connected"
        ));

        let mut connected_shape = AppState {
            devices: vec![ProviderTarget::new("toy-1", "hub")],
            selected_device: Some("toy-1".to_owned()),
            ..AppState::default()
        };
        connected_shape.queue_trigger_action(trigger(TriggerKind::Death, "session", 1));
        assert!(matches!(
            connected_shape.action_status,
            Some(ActionStatus::Skipped { reason, .. }) if reason == "provider is not connected"
        ));
    }
}
