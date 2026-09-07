use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::action::{
    DEFAULT_VIBRATE_DURATION, DEFAULT_VIBRATE_STRENGTH, IntensityCurve, IntensityPoint,
    MAX_VIBRATE_DURATION, MAX_VIBRATE_STRENGTH, MIN_VIBRATE_DURATION, MIN_VIBRATE_STRENGTH,
    VibrateActionSettings, VibrateFixedSettings, VibrateIntervalSettings, VibrateMode,
    nearest_duration_step,
};
use crate::app::{
    AbilityFilter, AbilityTriggerSettings, AmountTriggerSettings, AppState, DEFAULT_PROFILE_NAME,
    EffectProfile, TriggerKind, TriggerSettings, TriggerSettingsSet,
};
use crate::provider::{LovenseSetup, ProviderSettings, TargetId};

pub const SCHEMA_VERSION: u32 = 7;
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedState {
    schema_version: u32,
    provider_settings: PersistedProviderSettings,
    preferred_target: Option<PersistedTarget>,
    // `triggers` / `resting_strength` are the active profile's effects, kept
    // at the top level so a state file from before profiles existed still
    // loads (it becomes the sole "Default" profile). `profiles` is the real
    // list once written; when it is empty the top-level pair is used instead.
    triggers: PersistedTriggers,
    log_path: String,
    // Added after schema 7; older state files fall back to 0 (disabled) rather
    // than forcing a schema bump that would discard the rest of the settings.
    #[serde(default)]
    resting_strength: u8,
    // Added after schema 7. Older files have no `profiles` key and fall back
    // to a single profile synthesized from the fields above.
    #[serde(default)]
    profiles: Vec<PersistedProfile>,
    #[serde(default)]
    active_profile: usize,
}

/// One named, switchable bundle of effect settings on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedProfile {
    name: String,
    triggers: PersistedTriggers,
    #[serde(default)]
    resting_strength: u8,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct SchemaVersion {
    schema_version: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedTriggers {
    local_player_death: PersistedTrigger,
    local_player_kill: PersistedTrigger,
    local_player_assist: PersistedTrigger,
    ability_used: PersistedAbilityTrigger,
    ability_cooldown_ready: PersistedAbilityTrigger,
    // Added after schema 7. State files written before it simply fall back to
    // the default order, so no schema bump (which would discard the rest of a
    // user's settings) is needed.
    #[serde(default = "default_priority_order")]
    priority_order: Vec<PersistedTriggerKind>,
    #[serde(default = "default_true")]
    death_hold_until_respawn: bool,
    #[serde(default = "default_true")]
    suppress_triggers_while_dead: bool,
    // Added after schema 7, same rationale as the fields above.
    #[serde(default = "default_damage_taken")]
    damage_taken: PersistedAmountTrigger,
    #[serde(default = "default_healing_received")]
    healing_received: PersistedAmountTrigger,
    // Fire-once event triggers for supporting a teammate. Added after schema 7.
    #[serde(default = "default_disabled_trigger")]
    ally_healed: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    ally_shielded: PersistedTrigger,
    // Combat-feedback triggers, added after schema 7.
    #[serde(default = "default_damage_given")]
    damage_given: PersistedAmountTrigger,
    #[serde(default = "default_disabled_trigger")]
    soul_deny: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    soul_secure: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    parry_success: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    parry_fail: PersistedTrigger,
    // Objective triggers, added after schema 7.
    #[serde(default = "default_disabled_trigger")]
    objective_guardian: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    objective_walker: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    objective_base_guardian: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    objective_shrine: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    objective_patron_weakened: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    game_won: PersistedTrigger,
    #[serde(default = "default_disabled_trigger")]
    damage_taken_intensity: PersistedTrigger,
    // `damage_taken_intensity_curve` is the name the first (separate-trigger)
    // version wrote; accepted as an alias so that file keeps its curve.
    #[serde(
        default = "default_intensity_curve",
        alias = "damage_taken_intensity_curve"
    )]
    damage_taken_curve: PersistedIntensityCurve,
    // The earlier amount-gated "healing given" trigger. Deserialized only so a
    // file that still has it loads; its enabled state migrates into
    // `ally_healed` in `to_app`, and it is never written back.
    #[serde(default, skip_serializing)]
    healing_given: Option<PersistedAmountTrigger>,
}

fn default_damage_taken() -> PersistedAmountTrigger {
    PersistedAmountTrigger::new(200.0, 3.0)
}
fn default_healing_received() -> PersistedAmountTrigger {
    PersistedAmountTrigger::new(150.0, 3.0)
}
fn default_damage_given() -> PersistedAmountTrigger {
    PersistedAmountTrigger::new(400.0, 3.0)
}
fn default_disabled_trigger() -> PersistedTrigger {
    disabled_trigger(PersistedVibrate::default())
}
fn default_intensity_curve() -> PersistedIntensityCurve {
    PersistedIntensityCurve::from_curve(&IntensityCurve::default())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedIntensityPoint {
    damage: f32,
    level: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedIntensityCurve {
    window_seconds: f32,
    pulse_seconds: f32,
    points: Vec<PersistedIntensityPoint>,
}
impl PersistedIntensityCurve {
    fn from_curve(curve: &IntensityCurve) -> Self {
        Self {
            window_seconds: curve.window_seconds,
            pulse_seconds: curve.pulse_seconds,
            points: curve
                .points
                .iter()
                .map(|point| PersistedIntensityPoint {
                    damage: point.damage,
                    level: point.level,
                })
                .collect(),
        }
    }
    fn to_curve(&self) -> IntensityCurve {
        let mut curve = IntensityCurve {
            window_seconds: self.window_seconds,
            pulse_seconds: self.pulse_seconds,
            points: self
                .points
                .iter()
                .map(|point| IntensityPoint {
                    damage: point.damage,
                    level: point.level,
                })
                .collect(),
        };
        curve.normalize();
        curve
    }
}

// `SoulSecure` is retired (see `PRIORITY_ORDER_DEFAULT` in app.rs); its
// `PersistedTriggerKind` variant and `soul_secure` field stay so an older
// state file still deserializes, and it is dropped from the live order on
// load. `DamageTakenIntensity` is likewise persisted-only.
const DEFAULT_PRIORITY_ORDER: [PersistedTriggerKind; 20] = [
    PersistedTriggerKind::LocalPlayerDeath,
    PersistedTriggerKind::LocalPlayerKill,
    PersistedTriggerKind::LocalPlayerAssist,
    PersistedTriggerKind::AbilityUsed,
    PersistedTriggerKind::AbilityCooldownReady,
    PersistedTriggerKind::DamageTaken,
    PersistedTriggerKind::HealingReceived,
    PersistedTriggerKind::AllyHealed,
    PersistedTriggerKind::AllyShielded,
    PersistedTriggerKind::DamageGiven,
    PersistedTriggerKind::SoulDeny,
    PersistedTriggerKind::ParrySuccess,
    PersistedTriggerKind::ParryFail,
    PersistedTriggerKind::ObjectiveGuardian,
    PersistedTriggerKind::ObjectiveWalker,
    PersistedTriggerKind::ObjectiveBaseGuardian,
    PersistedTriggerKind::ObjectiveShrine,
    PersistedTriggerKind::ObjectivePatronWeakened,
    PersistedTriggerKind::GameWon,
    PersistedTriggerKind::DamageTakenIntensity,
];

fn default_priority_order() -> Vec<PersistedTriggerKind> {
    DEFAULT_PRIORITY_ORDER.to_vec()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedTriggerKind {
    LocalPlayerDeath,
    LocalPlayerKill,
    LocalPlayerAssist,
    AbilityUsed,
    AbilityCooldownReady,
    DamageTaken,
    HealingReceived,
    // `healing_given` is the pre-split name; a priority order saved with it
    // reads back as this slot.
    #[serde(alias = "healing_given")]
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

impl From<TriggerKind> for PersistedTriggerKind {
    fn from(kind: TriggerKind) -> Self {
        match kind {
            TriggerKind::Death => Self::LocalPlayerDeath,
            TriggerKind::Kill => Self::LocalPlayerKill,
            TriggerKind::Assist => Self::LocalPlayerAssist,
            TriggerKind::AbilityUse => Self::AbilityUsed,
            TriggerKind::AbilityCooldownReady => Self::AbilityCooldownReady,
            TriggerKind::DamageTaken => Self::DamageTaken,
            TriggerKind::HealingReceived => Self::HealingReceived,
            TriggerKind::AllyHealed => Self::AllyHealed,
            TriggerKind::AllyShielded => Self::AllyShielded,
            TriggerKind::DamageGiven => Self::DamageGiven,
            TriggerKind::SoulDeny => Self::SoulDeny,
            TriggerKind::SoulSecure => Self::SoulSecure,
            TriggerKind::ParrySuccess => Self::ParrySuccess,
            TriggerKind::ParryFail => Self::ParryFail,
            TriggerKind::ObjectiveGuardian => Self::ObjectiveGuardian,
            TriggerKind::ObjectiveWalker => Self::ObjectiveWalker,
            TriggerKind::ObjectiveBaseGuardian => Self::ObjectiveBaseGuardian,
            TriggerKind::ObjectiveShrine => Self::ObjectiveShrine,
            TriggerKind::ObjectivePatronWeakened => Self::ObjectivePatronWeakened,
            TriggerKind::GameWon => Self::GameWon,
            TriggerKind::DamageTakenIntensity => Self::DamageTakenIntensity,
        }
    }
}

impl From<PersistedTriggerKind> for TriggerKind {
    fn from(kind: PersistedTriggerKind) -> Self {
        match kind {
            PersistedTriggerKind::LocalPlayerDeath => Self::Death,
            PersistedTriggerKind::LocalPlayerKill => Self::Kill,
            PersistedTriggerKind::LocalPlayerAssist => Self::Assist,
            PersistedTriggerKind::AbilityUsed => Self::AbilityUse,
            PersistedTriggerKind::AbilityCooldownReady => Self::AbilityCooldownReady,
            PersistedTriggerKind::DamageTaken => Self::DamageTaken,
            PersistedTriggerKind::HealingReceived => Self::HealingReceived,
            PersistedTriggerKind::AllyHealed => Self::AllyHealed,
            PersistedTriggerKind::AllyShielded => Self::AllyShielded,
            PersistedTriggerKind::DamageGiven => Self::DamageGiven,
            PersistedTriggerKind::SoulDeny => Self::SoulDeny,
            PersistedTriggerKind::SoulSecure => Self::SoulSecure,
            PersistedTriggerKind::ParrySuccess => Self::ParrySuccess,
            PersistedTriggerKind::ParryFail => Self::ParryFail,
            PersistedTriggerKind::ObjectiveGuardian => Self::ObjectiveGuardian,
            PersistedTriggerKind::ObjectiveWalker => Self::ObjectiveWalker,
            PersistedTriggerKind::ObjectiveBaseGuardian => Self::ObjectiveBaseGuardian,
            PersistedTriggerKind::ObjectiveShrine => Self::ObjectiveShrine,
            PersistedTriggerKind::ObjectivePatronWeakened => Self::ObjectivePatronWeakened,
            PersistedTriggerKind::GameWon => Self::GameWon,
            PersistedTriggerKind::DamageTakenIntensity => Self::DamageTakenIntensity,
        }
    }
}

/// Mirrors [`PersistedAbilityTrigger`]'s shape for an amount-gated trigger:
/// the underlying enable/action pair plus its own threshold and window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAmountTrigger {
    trigger: PersistedTrigger,
    threshold: f32,
    window_seconds: f32,
}
impl PersistedAmountTrigger {
    fn new(threshold: f32, window_seconds: f32) -> Self {
        Self {
            trigger: disabled_trigger(PersistedVibrate::default()),
            threshold,
            window_seconds,
        }
    }
    fn from_app(settings: &AmountTriggerSettings) -> Self {
        Self {
            trigger: PersistedTrigger::from_app(&settings.trigger),
            threshold: settings.threshold,
            window_seconds: settings.window_seconds,
        }
    }
    fn to_app(&self) -> AmountTriggerSettings {
        AmountTriggerSettings {
            trigger: self.trigger.to_app(),
            threshold: self.threshold,
            window_seconds: self.window_seconds,
        }
    }
    fn normalize(&mut self) {
        self.trigger.actions.vibrate.normalize();
        self.threshold = normalize_value(self.threshold, 0.0, 1_000_000.0, 0.0);
        self.window_seconds = normalize_value(self.window_seconds, 0.1, 300.0, 3.0);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedTrigger {
    enabled: bool,
    actions: PersistedActions,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedActions {
    vibrate: PersistedVibrate,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAbilityTrigger {
    trigger: PersistedTrigger,
    ability_filter: PersistedAbilityFilter,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAbilityFilter {
    mode: PersistedAbilityFilterMode,
    slots: Vec<u32>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PersistedAbilityFilterMode {
    All,
    Selected,
}
impl Default for PersistedAbilityFilter {
    fn default() -> Self {
        Self {
            mode: PersistedAbilityFilterMode::All,
            slots: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedProviderSettings {
    lovense: PersistedLovenseSetup,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedLovenseSetup {
    domain: String,
    http_port: u16,
}
impl Default for PersistedLovenseSetup {
    fn default() -> Self {
        let setup = LovenseSetup::default();
        Self {
            domain: setup.domain,
            http_port: setup.http_port,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedTarget {
    id: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedVibrate {
    mode: PersistedVibrateMode,
    interval: PersistedVibrateInterval,
    fixed: PersistedVibrateFixed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PersistedVibrateMode {
    Interval,
    Fixed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedVibrateInterval {
    minimum_strength: f32,
    maximum_strength: f32,
    minimum_duration_seconds: f32,
    maximum_duration_seconds: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedVibrateFixed {
    strength: f32,
    duration_seconds: f32,
}
impl Default for PersistedVibrate {
    fn default() -> Self {
        Self {
            mode: PersistedVibrateMode::Fixed,
            interval: PersistedVibrateInterval {
                minimum_strength: DEFAULT_VIBRATE_STRENGTH,
                maximum_strength: DEFAULT_VIBRATE_STRENGTH,
                minimum_duration_seconds: DEFAULT_VIBRATE_DURATION,
                maximum_duration_seconds: DEFAULT_VIBRATE_DURATION,
            },
            fixed: PersistedVibrateFixed {
                strength: DEFAULT_VIBRATE_STRENGTH,
                duration_seconds: DEFAULT_VIBRATE_DURATION,
            },
        }
    }
}
fn disabled_trigger(vibrate: PersistedVibrate) -> PersistedTrigger {
    PersistedTrigger {
        enabled: false,
        actions: PersistedActions { vibrate },
    }
}
impl Default for PersistedTriggers {
    fn default() -> Self {
        let vibrate = PersistedVibrate::default();
        Self {
            priority_order: default_priority_order(),
            death_hold_until_respawn: true,
            suppress_triggers_while_dead: true,
            local_player_death: PersistedTrigger {
                enabled: true,
                actions: PersistedActions {
                    vibrate: vibrate.clone(),
                },
            },
            local_player_kill: disabled_trigger(vibrate.clone()),
            local_player_assist: disabled_trigger(vibrate.clone()),
            ability_used: PersistedAbilityTrigger {
                trigger: PersistedTrigger {
                    enabled: false,
                    actions: PersistedActions {
                        vibrate: vibrate.clone(),
                    },
                },
                ability_filter: PersistedAbilityFilter::default(),
            },
            ability_cooldown_ready: PersistedAbilityTrigger {
                trigger: PersistedTrigger {
                    enabled: false,
                    actions: PersistedActions {
                        vibrate: vibrate.clone(),
                    },
                },
                ability_filter: PersistedAbilityFilter::default(),
            },
            damage_taken: default_damage_taken(),
            healing_received: default_healing_received(),
            ally_healed: disabled_trigger(vibrate.clone()),
            ally_shielded: disabled_trigger(vibrate.clone()),
            damage_given: default_damage_given(),
            soul_deny: disabled_trigger(vibrate.clone()),
            soul_secure: disabled_trigger(vibrate.clone()),
            parry_success: disabled_trigger(vibrate.clone()),
            parry_fail: disabled_trigger(vibrate.clone()),
            objective_guardian: disabled_trigger(vibrate.clone()),
            objective_walker: disabled_trigger(vibrate.clone()),
            objective_base_guardian: disabled_trigger(vibrate.clone()),
            objective_shrine: disabled_trigger(vibrate.clone()),
            objective_patron_weakened: disabled_trigger(vibrate.clone()),
            game_won: disabled_trigger(vibrate.clone()),
            damage_taken_intensity: disabled_trigger(vibrate),
            damage_taken_curve: default_intensity_curve(),
            healing_given: None,
        }
    }
}
impl Default for PersistedState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            provider_settings: PersistedProviderSettings::default(),
            preferred_target: None,
            triggers: PersistedTriggers::default(),
            log_path: String::new(),
            resting_strength: 0,
            // A single unmodified "Default" profile is written in the legacy
            // shape (empty `profiles`, top-level `triggers`) so a fresh file
            // still loads on an older build; see `from_app`.
            profiles: Vec::new(),
            active_profile: 0,
        }
    }
}

impl PersistedState {
    pub(crate) fn from_app(app: &AppState) -> Self {
        let setup = app.effective_provider_settings();
        // The active profile mirrors the live `triggers` / `resting_strength`
        // (which the UI edits directly, without touching `app.profiles`); the
        // rest are taken from the stored bundles as-is.
        let active_triggers = PersistedTriggers::from_app(&app.triggers);
        // A lone, unrenamed "Default" profile is fully represented by the
        // top-level `triggers` / `resting_strength`, so `profiles` stays empty
        // and the file reads on a build that predates profiles.
        let is_lone_default = app.profiles.len() == 1
            && app
                .profiles
                .first()
                .is_some_and(|profile| profile.name == DEFAULT_PROFILE_NAME);
        let profiles = if is_lone_default {
            Vec::new()
        } else {
            app.profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| PersistedProfile {
                    name: profile.name.clone(),
                    triggers: if index == app.active_profile {
                        active_triggers.clone()
                    } else {
                        PersistedTriggers::from_app(&profile.triggers)
                    },
                    resting_strength: if index == app.active_profile {
                        app.resting_strength
                    } else {
                        profile.resting_strength
                    },
                })
                .collect()
        };
        let active_profile = if is_lone_default { 0 } else { app.active_profile };
        let state = Self {
            schema_version: SCHEMA_VERSION,
            provider_settings: PersistedProviderSettings {
                lovense: PersistedLovenseSetup {
                    domain: setup.lovense.domain,
                    http_port: setup.lovense.http_port,
                },
            },
            preferred_target: app
                .preferred_target
                .as_ref()
                .map(PersistedTarget::from_target_id),
            triggers: active_triggers,
            log_path: app.log_path.clone(),
            resting_strength: app.resting_strength,
            profiles,
            active_profile,
        };
        state.normalized().unwrap_or_default()
    }
    pub(crate) fn restore_app(&self) -> AppState {
        let mut app = AppState::default();
        app.provider_settings = ProviderSettings {
            lovense: LovenseSetup {
                domain: self.provider_settings.lovense.domain.clone(),
                http_port: self.provider_settings.lovense.http_port,
            },
        };
        app.preferred_target = self
            .preferred_target
            .as_ref()
            .and_then(|target| target.to_target_id().ok());
        app.log_path = self.log_path.clone();

        // A state file from before profiles existed carries no `profiles`
        // list, so the top-level effects become its sole "Default" profile.
        let mut profiles: Vec<EffectProfile> = if self.profiles.is_empty() {
            vec![EffectProfile {
                name: DEFAULT_PROFILE_NAME.to_owned(),
                triggers: self.triggers.to_app(),
                resting_strength: self.resting_strength.min(20),
            }]
        } else {
            self.profiles
                .iter()
                .map(|profile| EffectProfile {
                    name: profile.name.clone(),
                    triggers: profile.triggers.to_app(),
                    resting_strength: profile.resting_strength.min(20),
                })
                .collect()
        };
        if profiles.is_empty() {
            profiles.push(EffectProfile::new(DEFAULT_PROFILE_NAME));
        }
        let active = self.active_profile.min(profiles.len() - 1);
        app.triggers = profiles[active].triggers.clone();
        app.resting_strength = profiles[active].resting_strength;
        app.profiles = profiles;
        app.active_profile = active;
        app
    }
    fn normalized(mut self) -> Result<Self, String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported schema version {}; expected {SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        self.triggers.normalize();
        self.resting_strength = self.resting_strength.min(20);
        for profile in &mut self.profiles {
            profile.triggers.normalize();
            profile.resting_strength = profile.resting_strength.min(20);
        }
        if !self.profiles.is_empty() {
            self.active_profile = self.active_profile.min(self.profiles.len() - 1);
        }
        if let Some(target) = &self.preferred_target {
            self.preferred_target = Some(PersistedTarget::from_target_id(&target.to_target_id()?));
        }
        Ok(self)
    }
}
impl PersistedTriggers {
    fn from_app(triggers: &TriggerSettingsSet) -> Self {
        Self {
            priority_order: triggers
                .priority_order
                .iter()
                .map(|kind| PersistedTriggerKind::from(*kind))
                .collect(),
            death_hold_until_respawn: triggers.death_hold_until_respawn,
            suppress_triggers_while_dead: triggers.suppress_triggers_while_dead,
            local_player_death: PersistedTrigger::from_app(&triggers.death),
            local_player_kill: PersistedTrigger::from_app(&triggers.kill),
            local_player_assist: PersistedTrigger::from_app(&triggers.assist),
            ability_used: PersistedAbilityTrigger::from_app(&triggers.ability_use),
            ability_cooldown_ready: PersistedAbilityTrigger::from_app(
                &triggers.ability_cooldown_ready,
            ),
            damage_taken: PersistedAmountTrigger::from_app(&triggers.damage_taken),
            healing_received: PersistedAmountTrigger::from_app(&triggers.healing_received),
            ally_healed: PersistedTrigger::from_app(&triggers.ally_healed),
            ally_shielded: PersistedTrigger::from_app(&triggers.ally_shielded),
            damage_given: PersistedAmountTrigger::from_app(&triggers.damage_given),
            soul_deny: PersistedTrigger::from_app(&triggers.soul_deny),
            soul_secure: PersistedTrigger::from_app(&triggers.soul_secure),
            parry_success: PersistedTrigger::from_app(&triggers.parry_success),
            parry_fail: PersistedTrigger::from_app(&triggers.parry_fail),
            objective_guardian: PersistedTrigger::from_app(&triggers.objective_guardian),
            objective_walker: PersistedTrigger::from_app(&triggers.objective_walker),
            objective_base_guardian: PersistedTrigger::from_app(&triggers.objective_base_guardian),
            objective_shrine: PersistedTrigger::from_app(&triggers.objective_shrine),
            objective_patron_weakened: PersistedTrigger::from_app(
                &triggers.objective_patron_weakened,
            ),
            game_won: PersistedTrigger::from_app(&triggers.game_won),
            damage_taken_intensity: PersistedTrigger::from_app(&triggers.damage_taken_intensity),
            damage_taken_curve: PersistedIntensityCurve::from_curve(
                &triggers.damage_taken_curve,
            ),
            healing_given: None,
        }
    }
    fn to_app(&self) -> TriggerSettingsSet {
        // A file that predates the split carries the old amount-gated
        // `healing_given`; if it was enabled, adopt its enable state and
        // effect for the new "healed an ally" trigger.
        let ally_healed = match &self.healing_given {
            Some(legacy) if legacy.trigger.enabled && !self.ally_healed.enabled => {
                legacy.trigger.to_app()
            }
            _ => self.ally_healed.to_app(),
        };
        let mut set = TriggerSettingsSet {
            death: self.local_player_death.to_app(),
            kill: self.local_player_kill.to_app(),
            assist: self.local_player_assist.to_app(),
            ability_use: self.ability_used.to_app(),
            ability_cooldown_ready: self.ability_cooldown_ready.to_app(),
            damage_taken: self.damage_taken.to_app(),
            healing_received: self.healing_received.to_app(),
            ally_healed,
            ally_shielded: self.ally_shielded.to_app(),
            damage_given: self.damage_given.to_app(),
            soul_deny: self.soul_deny.to_app(),
            soul_secure: self.soul_secure.to_app(),
            parry_success: self.parry_success.to_app(),
            parry_fail: self.parry_fail.to_app(),
            objective_guardian: self.objective_guardian.to_app(),
            objective_walker: self.objective_walker.to_app(),
            objective_base_guardian: self.objective_base_guardian.to_app(),
            objective_shrine: self.objective_shrine.to_app(),
            objective_patron_weakened: self.objective_patron_weakened.to_app(),
            game_won: self.game_won.to_app(),
            damage_taken_intensity: self.damage_taken_intensity.to_app(),
            damage_taken_curve: self.damage_taken_curve.to_curve(),
            priority_order: self
                .priority_order
                .iter()
                .map(|kind| TriggerKind::from(*kind))
                .collect(),
            death_hold_until_respawn: self.death_hold_until_respawn,
            suppress_triggers_while_dead: self.suppress_triggers_while_dead,
        };
        set.normalize_priority_order();
        set
    }
    /// Canonicalizes every trigger's action settings, ability filters and the
    /// overlap-priority order. Applied to the top-level triggers and to each
    /// stored profile.
    fn normalize(&mut self) {
        self.local_player_death.actions.vibrate.normalize();
        self.local_player_kill.actions.vibrate.normalize();
        self.local_player_assist.actions.vibrate.normalize();
        self.ability_used.trigger.actions.vibrate.normalize();
        self.ability_cooldown_ready.trigger.actions.vibrate.normalize();
        self.ability_used.ability_filter.normalize();
        self.ability_cooldown_ready.ability_filter.normalize();
        self.damage_taken.normalize();
        self.healing_received.normalize();
        self.ally_healed.actions.vibrate.normalize();
        self.ally_shielded.actions.vibrate.normalize();
        self.damage_given.normalize();
        self.soul_deny.actions.vibrate.normalize();
        self.soul_secure.actions.vibrate.normalize();
        self.parry_success.actions.vibrate.normalize();
        self.parry_fail.actions.vibrate.normalize();
        self.objective_guardian.actions.vibrate.normalize();
        self.objective_walker.actions.vibrate.normalize();
        self.objective_base_guardian.actions.vibrate.normalize();
        self.objective_shrine.actions.vibrate.normalize();
        self.objective_patron_weakened.actions.vibrate.normalize();
        self.game_won.actions.vibrate.normalize();
        self.damage_taken_intensity.actions.vibrate.normalize();
        self.damage_taken_curve =
            PersistedIntensityCurve::from_curve(&self.damage_taken_curve.to_curve());
        if let Some(legacy) = &mut self.healing_given {
            legacy.normalize();
        }
        self.normalize_priority_order();
    }
    fn normalize_priority_order(&mut self) {
        let mut ordered = Vec::with_capacity(DEFAULT_PRIORITY_ORDER.len());
        for kind in self.priority_order.drain(..) {
            if !ordered.contains(&kind) {
                ordered.push(kind);
            }
        }
        for kind in DEFAULT_PRIORITY_ORDER {
            if !ordered.contains(&kind) {
                ordered.push(kind);
            }
        }
        self.priority_order = ordered;
    }
}
impl PersistedTrigger {
    fn from_app(trigger: &TriggerSettings) -> Self {
        Self {
            enabled: trigger.enabled,
            actions: PersistedActions {
                vibrate: PersistedVibrate::from_app(&trigger.actions),
            },
        }
    }
    fn to_app(&self) -> TriggerSettings {
        TriggerSettings {
            enabled: self.enabled,
            actions: self.actions.vibrate.to_app(),
        }
    }
}
impl PersistedAbilityTrigger {
    fn from_app(trigger: &AbilityTriggerSettings) -> Self {
        Self {
            trigger: PersistedTrigger::from_app(&trigger.trigger),
            ability_filter: PersistedAbilityFilter::from_app(&trigger.ability_filter),
        }
    }
    fn to_app(&self) -> AbilityTriggerSettings {
        AbilityTriggerSettings {
            trigger: self.trigger.to_app(),
            ability_filter: self.ability_filter.to_app(),
        }
    }
}
impl PersistedAbilityFilter {
    fn from_app(filter: &AbilityFilter) -> Self {
        match filter {
            AbilityFilter::All => Self::default(),
            AbilityFilter::Selected(slots) => Self {
                mode: PersistedAbilityFilterMode::Selected,
                slots: slots.iter().copied().filter(|slot| *slot > 0).collect(),
            },
        }
    }
    fn to_app(&self) -> AbilityFilter {
        match self.mode {
            PersistedAbilityFilterMode::All => AbilityFilter::All,
            PersistedAbilityFilterMode::Selected => AbilityFilter::Selected(
                self.slots
                    .iter()
                    .copied()
                    .filter(|slot| *slot > 0)
                    .collect::<BTreeSet<_>>(),
            ),
        }
    }
    fn normalize(&mut self) {
        if self.mode == PersistedAbilityFilterMode::All {
            self.slots.clear();
        } else {
            self.slots.retain(|slot| *slot > 0);
            self.slots.sort_unstable();
            self.slots.dedup();
        }
    }
}
impl PersistedVibrate {
    fn from_app(vibrate: &VibrateActionSettings) -> Self {
        Self {
            mode: vibrate.mode.into(),
            interval: PersistedVibrateInterval {
                minimum_strength: vibrate.interval.minimum_strength,
                maximum_strength: vibrate.interval.maximum_strength,
                minimum_duration_seconds: vibrate.interval.minimum_duration_seconds,
                maximum_duration_seconds: vibrate.interval.maximum_duration_seconds,
            },
            fixed: PersistedVibrateFixed {
                strength: vibrate.fixed.strength,
                duration_seconds: vibrate.fixed.duration_seconds,
            },
        }
    }
    fn to_app(&self) -> VibrateActionSettings {
        VibrateActionSettings {
            mode: self.mode.into(),
            interval: VibrateIntervalSettings {
                minimum_strength: self.interval.minimum_strength,
                maximum_strength: self.interval.maximum_strength,
                minimum_duration_seconds: self.interval.minimum_duration_seconds,
                maximum_duration_seconds: self.interval.maximum_duration_seconds,
            },
            fixed: VibrateFixedSettings {
                strength: self.fixed.strength,
                duration_seconds: self.fixed.duration_seconds,
            },
        }
    }
    fn normalize(&mut self) {
        let normalize_strength =
            |value: f32| normalize_value(value, MIN_VIBRATE_STRENGTH, MAX_VIBRATE_STRENGTH, MIN_VIBRATE_STRENGTH);
        let normalize_duration = |value: f32| {
            nearest_duration_step(normalize_value(
                value,
                MIN_VIBRATE_DURATION,
                MAX_VIBRATE_DURATION,
                MIN_VIBRATE_DURATION,
            ))
        };

        self.interval.minimum_strength = normalize_strength(self.interval.minimum_strength);
        self.interval.maximum_strength =
            normalize_strength(self.interval.maximum_strength).max(self.interval.minimum_strength);
        self.fixed.strength = normalize_strength(self.fixed.strength);

        self.interval.minimum_duration_seconds =
            normalize_duration(self.interval.minimum_duration_seconds);
        self.interval.maximum_duration_seconds = normalize_duration(self.interval.maximum_duration_seconds)
            .max(self.interval.minimum_duration_seconds);
        self.fixed.duration_seconds = normalize_duration(self.fixed.duration_seconds);
    }
}
impl From<VibrateMode> for PersistedVibrateMode {
    fn from(mode: VibrateMode) -> Self {
        match mode {
            VibrateMode::Interval => Self::Interval,
            VibrateMode::Fixed => Self::Fixed,
        }
    }
}
impl From<PersistedVibrateMode> for VibrateMode {
    fn from(mode: PersistedVibrateMode) -> Self {
        match mode {
            PersistedVibrateMode::Interval => Self::Interval,
            PersistedVibrateMode::Fixed => Self::Fixed,
        }
    }
}
impl PersistedTarget {
    fn from_target_id(target: &TargetId) -> Self {
        Self { id: target.clone() }
    }
    fn to_target_id(&self) -> Result<TargetId, String> {
        if self.id.trim().is_empty() {
            Err("preferred target ID is empty".to_owned())
        } else {
            Ok(self.id.clone())
        }
    }
}

fn normalize_value(value: f32, minimum: f32, maximum: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        fallback
    }
}

pub(crate) struct LoadOutcome {
    pub state: PersistedState,
    pub warning: Option<String>,
    migrated: bool,
}

pub(crate) fn default_state_path() -> Result<PathBuf, String> {
    let result = dirs::config_dir()
        .map(|directory| directory.join("deadlockshock-companion").join("state.json"))
        .ok_or_else(|| {
            "The operating system did not provide a per-user config directory.".to_owned()
        });
    if let Ok(path) = &result {
        log::info!(
            target: "companion::persistence",
            "settings_path_resolved path={:?}",
            path
        );
    }
    result
}

pub(crate) fn load_from_path(path: &Path) -> LoadOutcome {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            log::info!(
                target: "companion::persistence",
                "settings_load_outcome path={:?} outcome=missing_defaults",
                path
            );
            return LoadOutcome {
                state: PersistedState::default(),
                warning: None,
                migrated: false,
            };
        }
        Err(error) => {
            log::warn!(
                target: "companion::persistence",
                "settings_load_failed path={:?} stage=read error={:?}",
                path,
                error
            );
            return LoadOutcome {
                state: PersistedState::default(),
                warning: Some(format!(
                    "Could not read saved state at {}: {error}. Defaults were restored.",
                    path.display()
                )),
                migrated: false,
            };
        }
    };

    let loaded = serde_json::from_str::<SchemaVersion>(&source)
        .map_err(|error| error.to_string())
        .and_then(|version| match version.schema_version {
            SCHEMA_VERSION => serde_json::from_str::<PersistedState>(&source)
                .map(|state| (state, false))
                .map_err(|error| error.to_string()),
            unsupported => Err(format!(
                "unsupported schema version {unsupported}; expected {SCHEMA_VERSION}"
            )),
        })
        .and_then(|(state, migrated)| state.normalized().map(|state| (state, migrated)));
    match loaded {
        Ok((state, migrated)) => {
            log::info!(
                target: "companion::persistence",
                "settings_load_outcome path={:?} outcome=loaded migrated={}",
                path,
                migrated
            );
            LoadOutcome {
                state,
                warning: None,
                migrated,
            }
        }
        Err(error) => {
            let preservation = match preserve_invalid_file(path) {
                Ok(backup) => {
                    log::warn!(
                        target: "companion::persistence",
                        "settings_load_failed path={:?} stage=parse backup={:?}",
                        path,
                        backup
                    );
                    format!("The invalid file was preserved at {}.", backup.display())
                }
                Err(backup_error) => {
                    log::warn!(
                        target: "companion::persistence",
                        "settings_load_failed path={:?} stage=parse backup_failed error={:?}",
                        path,
                        backup_error
                    );
                    format!(
                        "The invalid file could not be moved to a backup ({backup_error}); it remains at {}.",
                        path.display()
                    )
                }
            };
            LoadOutcome {
                state: PersistedState::default(),
                warning: Some(format!(
                    "Saved state was invalid ({error}). {preservation} Defaults were restored."
                )),
                migrated: false,
            }
        }
    }
}

fn preserve_invalid_file(path: &Path) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("state");
    let extension = path.extension().and_then(|extension| extension.to_str());
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    for collision in 0_u32.. {
        let suffix = if collision == 0 {
            String::new()
        } else {
            format!("-{collision}")
        };
        let mut file_name = format!(
            "{stem}.invalid-{}-{:09}{suffix}",
            timestamp.as_secs(),
            timestamp.subsec_nanos()
        );
        if let Some(extension) = extension {
            file_name.push('.');
            file_name.push_str(extension);
        }
        let backup = parent.join(file_name);
        if !backup.exists() {
            fs::rename(path, &backup)?;
            return Ok(backup);
        }
    }
    unreachable!("the invalid-state backup suffix space is inexhaustible")
}

fn write_state(path: &Path, state: &PersistedState) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Could not create the saved-state directory {}: {error}",
            parent.display()
        )
    })?;
    set_private_directory_permissions(parent)?;

    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "Could not create a temporary saved-state file in {}: {error}",
            parent.display()
        )
    })?;
    set_private_file_permissions(temporary.as_file())?;
    serde_json::to_writer_pretty(&mut temporary, state)
        .map_err(|error| format!("Could not serialize saved state: {error}"))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| format!("Could not finish writing saved state: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("Could not synchronize saved state: {error}"))?;
    temporary.persist(path).map_err(|error| {
        format!(
            "Could not atomically replace {}: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "Could not restrict saved-state directory permissions for {}: {error}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(file: &fs::File) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("Could not restrict saved-state file permissions: {error}"))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_file: &fs::File) -> Result<(), String> {
    Ok(())
}
#[cfg(target_os = "windows")]
fn open_directory(path: &Path) -> Result<(), String> {
    spawn_directory_opener("explorer", path)
}

#[cfg(any(target_os = "windows", test))]
fn spawn_directory_opener(program: &str, path: &Path) -> Result<(), String> {
    // Explorer commonly exits with code 1 after successfully handing the folder
    // off to the existing shell process, so only failure to launch is an error.
    Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            format!(
                "Could not start the operating system's folder opener for {}: {error}",
                path.display()
            )
        })
}

#[cfg(target_os = "macos")]
fn open_directory(path: &Path) -> Result<(), String> {
    run_directory_opener("open", path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_directory(path: &Path) -> Result<(), String> {
    run_directory_opener("xdg-open", path)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn open_directory(_path: &Path) -> Result<(), String> {
    Err("Opening the config folder is unsupported on this operating system.".to_owned())
}

#[cfg(unix)]
fn run_directory_opener(program: &str, path: &Path) -> Result<(), String> {
    let status = Command::new(program).arg(path).status().map_err(|error| {
        format!(
            "Could not start the operating system's folder opener for {}: {error}",
            path.display()
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "The operating system's folder opener failed for {} with status {status}.",
            path.display()
        ))
    }
}

#[derive(Clone, Copy)]
enum SaveReason {
    Autosave,
    Reset,
}

pub(crate) struct Persistence {
    path: Option<PathBuf>,
    saved: PersistedState,
    observed: PersistedState,
    pending: Option<PersistedState>,
    pending_reason: SaveReason,
    deadline: Option<Instant>,
    debounce: Duration,
    load_warning: Option<String>,
    save_error: Option<String>,
}

impl Persistence {
    pub(crate) fn open(path: PathBuf) -> (Self, PersistedState) {
        let LoadOutcome {
            state,
            warning,
            migrated,
        } = load_from_path(&path);
        log::info!(
            target: "companion::persistence",
            "settings_opened path={:?} load_warning={} migration_save_pending={}",
            path,
            warning.is_some(),
            migrated
        );
        let deadline = migrated.then(|| Instant::now() + SAVE_DEBOUNCE);
        let pending = migrated.then(|| state.clone());
        (
            Self {
                path: Some(path),
                saved: state.clone(),
                observed: state.clone(),
                pending,
                pending_reason: SaveReason::Autosave,
                deadline,
                debounce: SAVE_DEBOUNCE,
                load_warning: warning,
                save_error: None,
            },
            state,
        )
    }

    pub(crate) fn unavailable(message: String) -> (Self, PersistedState) {
        log::warn!(
            target: "companion::persistence",
            "settings_unavailable reason={:?}",
            message
        );
        let state = PersistedState::default();
        (
            Self {
                path: None,
                saved: state.clone(),
                observed: state.clone(),
                pending: None,
                pending_reason: SaveReason::Autosave,
                deadline: None,
                debounce: SAVE_DEBOUNCE,
                load_warning: Some(format!(
                    "Saved state is unavailable: {message} Settings will remain in memory for this session."
                )),
                save_error: None,
            },
            state,
        )
    }

    pub(crate) fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
    }

    pub(crate) fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }
    pub(crate) fn open_config_directory(&self) -> Result<(), String> {
        self.open_config_directory_with(open_directory)
    }

    fn open_config_directory_with(
        &self,
        opener: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let state_path = self
            .path
            .as_deref()
            .ok_or_else(|| "No per-user config folder is available.".to_owned())?;
        let directory = state_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| {
                format!(
                    "The saved-state path {} has no containing folder.",
                    state_path.display()
                )
            })?;
        fs::create_dir_all(directory).map_err(|error| {
            format!(
                "Could not create the config folder {}: {error}",
                directory.display()
            )
        })?;
        set_private_directory_permissions(directory)?;
        opener(directory)
    }

    pub(crate) fn observe(&mut self, state: PersistedState, now: Instant) -> Option<Duration> {
        if state != self.observed {
            self.observed = state.clone();
            if state == self.saved {
                self.pending = None;
                self.deadline = None;
                self.save_error = None;
                log::debug!(
                    target: "companion::persistence",
                    "settings_autosave_cancelled reason=reverted_to_saved"
                );
            } else {
                let coalesced = self.pending.is_some();
                self.pending = Some(state);
                self.pending_reason = SaveReason::Autosave;
                self.deadline = Some(now + self.debounce);
                log::debug!(
                    target: "companion::persistence",
                    "settings_autosave_scheduled coalesced={coalesced}"
                );
            }
        } else if self.pending.is_some() && self.deadline.is_none() {
            self.deadline = Some(now + self.debounce);
            log::debug!(
                target: "companion::persistence",
                "settings_autosave_rescheduled"
            );
        }

        if self.deadline.is_some_and(|deadline| deadline <= now) {
            let state = self
                .pending
                .clone()
                .expect("a save deadline requires pending state");
            let reason = self.pending_reason;
            if self.commit(state, reason).is_err() {
                self.deadline = Some(now + self.debounce);
            }
        }

        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    pub(crate) fn save_reset_now(&mut self, state: PersistedState) -> Result<(), ()> {
        log::info!(target: "companion::persistence", "settings_reset_save_started");
        self.observed = state.clone();
        self.commit(state, SaveReason::Reset)
    }

    pub(crate) fn flush(&mut self, state: PersistedState) -> Result<(), ()> {
        if state == self.saved && self.pending.is_none() {
            log::debug!(
                target: "companion::persistence",
                "settings_flush_noop reason=clean"
            );
            return Ok(());
        }
        log::debug!(target: "companion::persistence", "settings_flush_started");
        self.observed = state.clone();
        let reason = self
            .pending
            .as_ref()
            .map(|_| self.pending_reason)
            .unwrap_or(SaveReason::Autosave);
        self.commit(state, reason)
    }

    fn commit(&mut self, state: PersistedState, reason: SaveReason) -> Result<(), ()> {
        let result = self
            .path
            .as_deref()
            .ok_or_else(|| "No per-user saved-state path is available.".to_owned())
            .and_then(|path| write_state(path, &state));
        match result {
            Ok(()) => {
                let recovered = self.save_error.is_some();
                self.saved = state;
                self.pending = None;
                self.deadline = None;
                self.save_error = None;
                log::info!(
                    target: "companion::persistence",
                    "settings_save_committed reason={} recovered={}",
                    match reason {
                        SaveReason::Autosave => "autosave",
                        SaveReason::Reset => "reset",
                    },
                    recovered
                );
                Ok(())
            }
            Err(error) => {
                let first_failure = self.save_error.is_none();
                self.pending = Some(state);
                self.pending_reason = reason;
                self.deadline = None;
                self.save_error = Some(match reason {
                    SaveReason::Autosave => format!(
                        "Could not save settings: {error} Changes remain unsaved and will be retried."
                    ),
                    SaveReason::Reset => format!(
                        "Current settings were reset in memory, but saved state could not be replaced: {error} The previous disk state may return after restart."
                    ),
                });
                if first_failure {
                    log::warn!(
                        target: "companion::persistence",
                        "settings_save_failed reason={} error={:?}",
                        match reason {
                            SaveReason::Autosave => "autosave",
                            SaveReason::Reset => "reset",
                        },
                        error
                    );
                }
                Err(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::VibrateMode;
    use crate::app::AbilityFilter;
    use std::collections::BTreeSet;

    #[test]
    fn config_folder_action_creates_and_opens_the_state_directory() {
        let root = tempfile::tempdir().unwrap();
        let state_path = root.path().join("nested").join("state.json");
        let expected_directory = state_path.parent().unwrap().to_owned();
        let (persistence, _) = Persistence::open(state_path);
        let opened = std::cell::Cell::new(false);
        persistence
            .open_config_directory_with(|directory| {
                assert_eq!(directory, expected_directory);
                assert!(directory.is_dir());
                opened.set(true);
                Ok(())
            })
            .unwrap();
        assert!(opened.get());
    }

    #[test]
    fn round_trip_preserves_provider_target_filters_and_actions() {
        let mut original = AppState::default();
        original.provider_settings.lovense.domain = "192.168.1.2".to_owned();
        original.provider_settings.lovense.http_port = 30010;
        original.preferred_target = Some("toy-id".to_owned());
        original.triggers.death.enabled = false;
        original.triggers.death.actions.mode = VibrateMode::Fixed;
        original.triggers.death.actions.fixed.strength = 14.0;
        original.triggers.death.actions.fixed.duration_seconds = 4.0;
        original.triggers.kill.enabled = true;
        original.triggers.assist.enabled = true;
        original.triggers.ability_use.trigger.enabled = true;
        original
            .triggers
            .ability_use
            .trigger
            .actions
            .interval
            .minimum_strength = 3.0;
        original
            .triggers
            .ability_use
            .trigger
            .actions
            .interval
            .maximum_strength = 12.0;
        original.triggers.ability_use.ability_filter =
            AbilityFilter::Selected(BTreeSet::from([1, 4]));
        original.triggers.ability_cooldown_ready.trigger.enabled = true;
        original
            .triggers
            .ability_cooldown_ready
            .trigger
            .actions
            .fixed
            .strength = 9.0;
        original.triggers.ability_cooldown_ready.ability_filter = AbilityFilter::All;
        original.log_path = "/logs/console.log".to_owned();
        let persisted = PersistedState::from_app(&original);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&persisted).unwrap()).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["provider_settings"]["lovense"]["domain"], "192.168.1.2");
        assert_eq!(value["preferred_target"]["id"], "toy-id");
        assert_eq!(
            value["triggers"]["local_player_death"]["actions"]["vibrate"]["fixed"]["strength"],
            14.0
        );
        let restored = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap()
            .restore_app();
        assert_eq!(restored.provider_settings, original.provider_settings);
        assert_eq!(restored.preferred_target, original.preferred_target);
        assert_eq!(restored.triggers, original.triggers);
        assert_eq!(restored.log_path, original.log_path);
    }

    #[test]
    fn priority_order_round_trips_and_missing_field_falls_back_to_default() {
        let mut original = AppState::default();
        original.triggers.priority_order = vec![
            TriggerKind::AbilityUse,
            TriggerKind::Death,
            TriggerKind::Kill,
            TriggerKind::Assist,
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
        let persisted = PersistedState::from_app(&original);
        assert_eq!(
            persisted.restore_app().triggers.priority_order,
            original.triggers.priority_order
        );

        // A state file that still lists retired kinds loads without them.
        let mut with_retired = serde_json::to_value(&persisted).unwrap();
        with_retired["triggers"]["priority_order"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("soul_secure"));
        let cleaned = serde_json::from_value::<PersistedState>(with_retired)
            .unwrap()
            .restore_app();
        assert_eq!(cleaned.triggers.priority_order, original.triggers.priority_order);

        let mut value = serde_json::to_value(&persisted).unwrap();
        value["triggers"]
            .as_object_mut()
            .unwrap()
            .remove("priority_order");
        let without_field = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap();
        assert_eq!(
            without_field.restore_app().triggers.priority_order,
            crate::app::PRIORITY_ORDER_DEFAULT.to_vec()
        );
    }

    #[test]
    fn multiple_effect_profiles_round_trip_with_the_active_one() {
        let mut original = AppState::default();
        original.triggers.death.actions.fixed.strength = 4.0;
        original.resting_strength = 3;
        original.duplicate_profile(0); // a copy of Default, now active (index 1)
        original.triggers.death.actions.fixed.strength = 17.0;
        original.triggers.kill.enabled = true;
        original.resting_strength = 9;
        original.add_blank_profile(); // a fresh profile, now active (index 2)
        original.switch_profile(1); // leave the copy active

        let restored = PersistedState::from_app(&original).restore_app();

        assert_eq!(restored.profiles.len(), 3);
        assert_eq!(restored.active_profile, 1);
        assert_eq!(restored.profiles[0].name, DEFAULT_PROFILE_NAME);
        assert_eq!(restored.profiles[0].triggers.death.actions.fixed.strength, 4.0);
        assert_eq!(restored.profiles[0].resting_strength, 3);
        assert_eq!(restored.profiles[1].triggers.death.actions.fixed.strength, 17.0);
        assert!(restored.profiles[1].triggers.kill.enabled);
        assert_eq!(restored.profiles[1].resting_strength, 9);
        assert!(!restored.profiles[2].triggers.kill.enabled);
        // The active profile's effects are also live.
        assert_eq!(restored.triggers.death.actions.fixed.strength, 17.0);
        assert_eq!(restored.resting_strength, 9);
    }

    #[test]
    fn a_state_file_without_a_profiles_key_loads_as_one_default_profile() {
        let mut original = AppState::default();
        original.triggers.kill.enabled = true;
        original.resting_strength = 5;
        let mut value = serde_json::to_value(PersistedState::from_app(&original)).unwrap();
        // A lone Default profile is written in the legacy shape already, but be
        // explicit that an older file with neither key still loads.
        let object = value.as_object_mut().unwrap();
        object.remove("profiles");
        object.remove("active_profile");

        let restored = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap()
            .restore_app();

        assert_eq!(restored.profiles.len(), 1);
        assert_eq!(restored.profiles[0].name, DEFAULT_PROFILE_NAME);
        assert_eq!(restored.active_profile, 0);
        assert!(restored.triggers.kill.enabled);
        assert_eq!(restored.resting_strength, 5);
    }

    #[test]
    fn a_saved_active_profile_index_past_the_end_is_clamped() {
        let mut original = AppState::default();
        original.add_blank_profile();
        let mut value = serde_json::to_value(PersistedState::from_app(&original)).unwrap();
        value["active_profile"] = serde_json::json!(99);

        let restored = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap()
            .restore_app();

        assert_eq!(restored.active_profile, restored.profiles.len() - 1);
    }

    #[test]
    fn resting_and_death_hold_settings_round_trip_and_default_when_absent() {
        let mut original = AppState::default();
        original.resting_strength = 7;
        original.triggers.death_hold_until_respawn = false;
        original.triggers.suppress_triggers_while_dead = false;

        let persisted = PersistedState::from_app(&original);
        let restored = persisted.restore_app();
        assert_eq!(restored.resting_strength, 7);
        assert!(!restored.triggers.death_hold_until_respawn);
        assert!(!restored.triggers.suppress_triggers_while_dead);

        let mut value = serde_json::to_value(&persisted).unwrap();
        value.as_object_mut().unwrap().remove("resting_strength");
        let triggers = value["triggers"].as_object_mut().unwrap();
        triggers.remove("death_hold_until_respawn");
        triggers.remove("suppress_triggers_while_dead");
        let restored = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap()
            .restore_app();
        assert_eq!(restored.resting_strength, 0);
        assert!(restored.triggers.death_hold_until_respawn);
        assert!(restored.triggers.suppress_triggers_while_dead);
    }

    #[test]
    fn resting_strength_is_clamped_on_restore() {
        let mut value =
            serde_json::to_value(PersistedState::from_app(&AppState::default())).unwrap();
        value["resting_strength"] = serde_json::json!(250);
        let restored = serde_json::from_value::<PersistedState>(value)
            .unwrap()
            .normalized()
            .unwrap()
            .restore_app();
        assert_eq!(restored.resting_strength, 20);
    }

    #[test]
    fn normalization_is_independent_for_each_action_profile() {
        let mut state = PersistedState::default();
        state
            .triggers
            .local_player_death
            .actions
            .vibrate
            .interval
            .minimum_strength = 99.0;
        state
            .triggers
            .ability_used
            .trigger
            .actions
            .vibrate
            .fixed
            .duration_seconds = 9000.0;
        state
            .triggers
            .ability_cooldown_ready
            .trigger
            .actions
            .vibrate
            .interval
            .maximum_duration_seconds = 0.1;
        let normalized = state.normalized().unwrap();
        assert_eq!(
            normalized
                .triggers
                .local_player_death
                .actions
                .vibrate
                .interval
                .minimum_strength,
            MAX_VIBRATE_STRENGTH
        );
        assert_eq!(
            normalized
                .triggers
                .ability_used
                .trigger
                .actions
                .vibrate
                .fixed
                .duration_seconds,
            MAX_VIBRATE_DURATION
        );
        // Clamped up from 0.1, then floored at the (untouched, default)
        // minimum of this interval, which a max can never dip below.
        assert_eq!(
            normalized
                .triggers
                .ability_cooldown_ready
                .trigger
                .actions
                .vibrate
                .interval
                .maximum_duration_seconds,
            DEFAULT_VIBRATE_DURATION
        );
    }

    #[test]
    fn missing_file_loads_current_defaults_without_warning() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = load_from_path(&directory.path().join("state.json"));
        assert_eq!(outcome.state, PersistedState::default());
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn strict_unknown_fields_are_preserved_as_invalid_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut value = serde_json::to_value(PersistedState::default()).unwrap();
        value["triggers"]["ability_used"]["unexpected"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let outcome = load_from_path(&path);
        assert_eq!(outcome.state, PersistedState::default());
        assert!(outcome.warning.unwrap().contains("unknown field"));
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_file_is_preserved_before_defaults_are_returned() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, "{not json").unwrap();
        let outcome = load_from_path(&path);
        assert_eq!(outcome.state, PersistedState::default());
        assert!(outcome.warning.as_deref().unwrap().contains("preserved"));
        assert!(!path.exists());
        let backups = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert!(
            backups[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.invalid-")
        );
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), "{not json");
    }

    #[test]
    fn normalization_canonicalizes_each_profile_and_filter_independently() {
        let mut state = PersistedState::default();
        state
            .triggers
            .local_player_death
            .actions
            .vibrate
            .interval
            .minimum_strength = 99.0;
        state
            .triggers
            .local_player_death
            .actions
            .vibrate
            .interval
            .maximum_strength = -5.0;
        state
            .triggers
            .ability_used
            .trigger
            .actions
            .vibrate
            .fixed
            .duration_seconds = 9000.0;
        state.triggers.ability_used.ability_filter = PersistedAbilityFilter {
            mode: PersistedAbilityFilterMode::Selected,
            slots: vec![4, 0, 2, 4, 2],
        };
        state.triggers.ability_cooldown_ready.ability_filter = PersistedAbilityFilter {
            mode: PersistedAbilityFilterMode::All,
            slots: vec![1, 7],
        };
        let normalized = state.normalized().unwrap();
        assert_eq!(
            normalized
                .triggers
                .local_player_death
                .actions
                .vibrate
                .interval
                .minimum_strength,
            MAX_VIBRATE_STRENGTH
        );
        assert_eq!(
            normalized
                .triggers
                .local_player_death
                .actions
                .vibrate
                .interval
                .maximum_strength,
            MAX_VIBRATE_STRENGTH
        );
        assert_eq!(
            normalized
                .triggers
                .ability_used
                .trigger
                .actions
                .vibrate
                .fixed
                .duration_seconds,
            MAX_VIBRATE_DURATION
        );
        assert_eq!(
            normalized.triggers.ability_used.ability_filter.slots,
            vec![2, 4]
        );
        assert!(
            normalized
                .triggers
                .ability_cooldown_ready
                .ability_filter
                .slots
                .is_empty()
        );
    }

    #[test]
    fn debounce_writes_once_and_flush_writes_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let (mut persistence, initial) = Persistence::open(path.clone());
        let mut changed = initial.clone();
        changed.log_path = "/changed".to_owned();
        let start = Instant::now();
        assert_eq!(
            persistence.observe(changed.clone(), start),
            Some(SAVE_DEBOUNCE)
        );
        assert!(!path.exists());
        assert_eq!(
            persistence.observe(changed.clone(), start + SAVE_DEBOUNCE),
            None
        );
        assert_eq!(
            serde_json::from_str::<PersistedState>(&fs::read_to_string(&path).unwrap()).unwrap(),
            changed
        );
        changed.log_path = "/exit-flush".to_owned();
        persistence.observe(changed.clone(), start + SAVE_DEBOUNCE * 2);
        persistence.flush(changed.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<PersistedState>(&fs::read_to_string(&path).unwrap()).unwrap(),
            changed
        );
        changed.log_path = "/save-now".to_owned();
        persistence.save_reset_now(changed.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<PersistedState>(&fs::read_to_string(&path).unwrap()).unwrap(),
            changed
        );
    }

    #[test]
    fn unsupported_schema_is_backed_up_like_malformed_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let state = PersistedState {
            schema_version: SCHEMA_VERSION + 1,
            ..PersistedState::default()
        };
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let outcome = load_from_path(&path);
        assert_eq!(outcome.state, PersistedState::default());
        assert!(
            outcome
                .warning
                .unwrap()
                .contains("unsupported schema version")
        );
        assert!(!path.exists());
    }

    #[test]
    fn failed_reset_save_stays_dirty_and_reports_previous_disk_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::create_dir(&path).unwrap();
        let (mut persistence, mut state) = Persistence::open(path);
        state.log_path = "/reset-in-memory".to_owned();
        assert!(persistence.save_reset_now(state.clone()).is_err());
        assert_eq!(persistence.pending, Some(state));
        assert!(
            persistence
                .save_error()
                .unwrap()
                .contains("previous disk state may return")
        );
    }
}
