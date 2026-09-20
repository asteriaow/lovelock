use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

pub const BRIDGE_RECORD_PREFIX: &str = "[DEADLOCK_DEATH_HOOK]";
pub const BRIDGE_SCHEMA: u32 = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_INCOMPLETE_LINE_BYTES: usize = 256 * 1024;
const READ_BUFFER_BYTES: usize = 16 * 1024;
const TAIL_CHECKPOINT_BYTES: usize = 64;
const MAX_METADATA_VERSION_BYTES: usize = 64;
const HISTORICAL_SCAN_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModVersionObservation {
    Unknown,
    Legacy,
    Invalid,
    Reported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeMetadata {
    pub schema: u32,
    pub event: String,
    pub mod_version: ModVersionObservation,
}

pub fn parse_bridge_metadata(record: &str) -> Option<BridgeMetadata> {
    let prefix_at = record.find(BRIDGE_RECORD_PREFIX)?;
    let payload = &record[prefix_at + BRIDGE_RECORD_PREFIX.len()..];
    let value: Value = serde_json::from_str(payload).ok()?;
    let schema = value.get("schema")?.as_u64()?.try_into().ok()?;
    let event = value.get("event")?.as_str()?.to_owned();
    let mod_version = match value.get("mod_version") {
        None => ModVersionObservation::Legacy,
        Some(Value::String(version))
            if !version.is_empty()
                && version.len() <= MAX_METADATA_VERSION_BYTES
                && Version::parse(version).is_ok() =>
        {
            ModVersionObservation::Reported(version.clone())
        }
        Some(_) => ModVersionObservation::Invalid,
    };
    Some(BridgeMetadata {
        schema,
        event,
        mod_version,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookReady {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub poll_interval_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbilityCatalogEntry {
    pub ability_slot: u32,
    pub ability_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbilityCatalog {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub abilities: Vec<AbilityCatalogEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalPlayerDeath {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub sequence: u64,
    pub detection: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalPlayerRespawn {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountTrigger {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub sequence: u64,
    pub detection: String,
    pub count_before: Option<u64>,
    pub count_after: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbilityTrigger {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub sequence: u64,
    pub ability_slot: u32,
    pub ability_name: Option<String>,
    pub detection: String,
    pub charges_before: Option<u64>,
    pub charges_after: Option<u64>,
}

/// A reported change in the local player's combined health-and-shield total:
/// how much of it was lost (damage) or restored (healing) since the last
/// report, plus the current health for context.
#[derive(Clone, Debug, PartialEq)]
pub struct VitalsTrigger {
    pub schema: u32,
    pub session_id: String,
    pub client_time_ms: u64,
    pub sequence: u64,
    pub detection: String,
    pub amount: f64,
    pub health_after: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BridgeEvent {
    HookReady(HookReady),
    AbilityCatalog(AbilityCatalog),
    LocalPlayerDeath(LocalPlayerDeath),
    LocalPlayerRespawn(LocalPlayerRespawn),
    LocalPlayerKill(CountTrigger),
    LocalPlayerAssist(CountTrigger),
    AllyHealed(CountTrigger),
    AllyShielded(CountTrigger),
    SoulDeny(CountTrigger),
    SoulSecure(CountTrigger),
    ParrySuccess(CountTrigger),
    ParryFail(CountTrigger),
    ObjectiveGuardian(CountTrigger),
    ObjectiveWalker(CountTrigger),
    ObjectiveBaseGuardian(CountTrigger),
    ObjectiveShrine(CountTrigger),
    ObjectivePatronWeakened(CountTrigger),
    GameWon(CountTrigger),
    DamageTakenIntensity(CountTrigger),
    AbilityUsed(AbilityTrigger),
    AbilityCooldownReady(AbilityTrigger),
    DamageTaken(VitalsTrigger),
    HealingReceived(VitalsTrigger),
    DamageGiven(VitalsTrigger),
}

impl BridgeEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::HookReady(_) => "hook_ready",
            Self::AbilityCatalog(_) => "ability_catalog",
            Self::LocalPlayerDeath(_) => "local_player_death",
            Self::LocalPlayerRespawn(_) => "local_player_respawn",
            Self::LocalPlayerKill(_) => "local_player_kill",
            Self::LocalPlayerAssist(_) => "local_player_assist",
            Self::AllyHealed(_) => "ally_healed",
            Self::AllyShielded(_) => "ally_shielded",
            Self::SoulDeny(_) => "soul_deny",
            Self::SoulSecure(_) => "soul_secure",
            Self::ParrySuccess(_) => "parry_success",
            Self::ParryFail(_) => "parry_fail",
            Self::ObjectiveGuardian(_) => "objective_guardian",
            Self::ObjectiveWalker(_) => "objective_walker",
            Self::ObjectiveBaseGuardian(_) => "objective_base_guardian",
            Self::ObjectiveShrine(_) => "objective_shrine",
            Self::ObjectivePatronWeakened(_) => "objective_patron_weakened",
            Self::GameWon(_) => "game_won",
            Self::DamageTakenIntensity(_) => "damage_taken_intensity",
            Self::AbilityUsed(_) => "ability_used",
            Self::AbilityCooldownReady(_) => "ability_cooldown_ready",
            Self::DamageTaken(_) => "damage_taken",
            Self::HealingReceived(_) => "healing_received",
            Self::DamageGiven(_) => "damage_given",
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "event")]
enum WireEvent {
    #[serde(rename = "hook_ready")]
    HookReady {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        poll_interval_ms: u64,
    },
    #[serde(rename = "ability_catalog")]
    AbilityCatalog {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        abilities: Vec<WireAbilityCatalogEntry>,
    },
    #[serde(rename = "local_player_death")]
    LocalPlayerDeath {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "local_player_respawn")]
    LocalPlayerRespawn {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
    },
    #[serde(rename = "local_player_kill")]
    LocalPlayerKill {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
        kills_before: Option<u64>,
        kills_after: Option<u64>,
    },
    #[serde(rename = "local_player_assist")]
    LocalPlayerAssist {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
        assists_before: Option<u64>,
        assists_after: Option<u64>,
    },
    #[serde(rename = "ally_healed")]
    AllyHealed {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "ally_shielded")]
    AllyShielded {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "soul_deny")]
    SoulDeny {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "soul_secure")]
    SoulSecure {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "parry_success")]
    ParrySuccess {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "parry_fail")]
    ParryFail {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "objective_guardian")]
    ObjectiveGuardian {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "objective_walker")]
    ObjectiveWalker {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "objective_base_guardian")]
    ObjectiveBaseGuardian {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "objective_shrine")]
    ObjectiveShrine {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "objective_patron_weakened")]
    ObjectivePatronWeakened {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "game_won")]
    GameWon {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "damage_taken_intensity")]
    DamageTakenIntensity {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
    },
    #[serde(rename = "damage_given")]
    DamageGiven {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
        amount: f64,
        health: Option<f64>,
    },
    #[serde(rename = "ability_used")]
    AbilityUsed {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        ability_slot: u32,
        ability_name: Option<String>,
        detection: String,
        charges_before: Option<u64>,
        charges_after: Option<u64>,
    },
    #[serde(rename = "ability_cooldown_ready")]
    AbilityCooldownReady {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        ability_slot: u32,
        ability_name: Option<String>,
        detection: String,
        charges_before: Option<u64>,
        charges_after: Option<u64>,
    },
    #[serde(rename = "damage_taken")]
    DamageTaken {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
        amount: f64,
        health: Option<f64>,
    },
    #[serde(rename = "healing_received")]
    HealingReceived {
        schema: u32,
        session_id: String,
        client_time_ms: u64,
        sequence: u64,
        detection: String,
        amount: f64,
        health: Option<f64>,
    },
}

#[derive(Deserialize)]
struct WireAbilityCatalogEntry {
    ability_slot: u32,
    ability_name: Option<String>,
}

pub fn parse_bridge_record(record: &str) -> Option<BridgeEvent> {
    let prefix_at = record.find(BRIDGE_RECORD_PREFIX)?;
    let payload = &record[prefix_at + BRIDGE_RECORD_PREFIX.len()..];
    let event: WireEvent = serde_json::from_str(payload).ok()?;
    match event {
        WireEvent::HookReady {
            schema,
            session_id,
            client_time_ms,
            poll_interval_ms,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::HookReady(HookReady {
            schema,
            session_id,
            client_time_ms,
            poll_interval_ms,
        })),
        WireEvent::AbilityCatalog {
            schema,
            session_id,
            client_time_ms,
            abilities,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::AbilityCatalog(AbilityCatalog {
            schema,
            session_id,
            client_time_ms,
            abilities: abilities
                .into_iter()
                .map(|ability| AbilityCatalogEntry {
                    ability_slot: ability.ability_slot,
                    ability_name: ability.ability_name,
                })
                .collect(),
        })),
        WireEvent::LocalPlayerDeath {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::LocalPlayerDeath(LocalPlayerDeath {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        })),
        WireEvent::LocalPlayerRespawn {
            schema,
            session_id,
            client_time_ms,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::LocalPlayerRespawn(LocalPlayerRespawn {
            schema,
            session_id,
            client_time_ms,
        })),
        WireEvent::LocalPlayerKill {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            kills_before,
            kills_after,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::LocalPlayerKill(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: kills_before,
            count_after: kills_after,
        })),
        WireEvent::LocalPlayerAssist {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            assists_before,
            assists_after,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::LocalPlayerAssist(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: assists_before,
            count_after: assists_after,
        })),
        WireEvent::AllyHealed {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::AllyHealed(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::AllyShielded {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::AllyShielded(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::SoulDeny {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::SoulDeny(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::SoulSecure {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::SoulSecure(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ParrySuccess {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ParrySuccess(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ParryFail {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ParryFail(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ObjectiveGuardian {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ObjectiveGuardian(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ObjectiveWalker {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ObjectiveWalker(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ObjectiveBaseGuardian {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ObjectiveBaseGuardian(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ObjectiveShrine {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ObjectiveShrine(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::ObjectivePatronWeakened {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::ObjectivePatronWeakened(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::GameWon {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::GameWon(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::DamageTakenIntensity {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::DamageTakenIntensity(CountTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            count_before: None,
            count_after: None,
        })),
        WireEvent::DamageGiven {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::DamageGiven(VitalsTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health_after: health,
        })),
        WireEvent::AbilityUsed {
            schema,
            session_id,
            client_time_ms,
            sequence,
            ability_slot,
            ability_name,
            detection,
            charges_before,
            charges_after,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::AbilityUsed(AbilityTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            ability_slot,
            ability_name,
            detection,
            charges_before,
            charges_after,
        })),
        WireEvent::AbilityCooldownReady {
            schema,
            session_id,
            client_time_ms,
            sequence,
            ability_slot,
            ability_name,
            detection,
            charges_before,
            charges_after,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::AbilityCooldownReady(AbilityTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            ability_slot,
            ability_name,
            detection,
            charges_before,
            charges_after,
        })),
        WireEvent::DamageTaken {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::DamageTaken(VitalsTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health_after: health,
        })),
        WireEvent::HealingReceived {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health,
        } if schema == BRIDGE_SCHEMA => Some(BridgeEvent::HealingReceived(VitalsTrigger {
            schema,
            session_id,
            client_time_ms,
            sequence,
            detection,
            amount,
            health_after: health,
        })),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListenerPhase {
    #[default]
    Stopped,
    WaitingForFile,
    Listening,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ListenerStatus {
    pub phase: ListenerPhase,
    pub configured_path: Option<PathBuf>,
    pub current_error: Option<String>,
    pub last_activity_at: Option<Instant>,
    pub last_event_at: Option<Instant>,
    pub mod_version: ModVersionObservation,
    /// When the listener most recently entered [`ListenerPhase::Listening`].
    /// Paired with `last_activity_at` this tells "watching a file that never
    /// grows" (Deadlock running without `-condebug`) apart from "watching a
    /// file that is quiet right now".
    pub listening_since: Option<Instant>,
}

impl Default for ListenerStatus {
    fn default() -> Self {
        Self {
            phase: ListenerPhase::Stopped,
            configured_path: None,
            current_error: None,
            last_activity_at: None,
            last_event_at: None,
            mod_version: ModVersionObservation::Unknown,
            listening_since: None,
        }
    }
}

impl ListenerStatus {
    pub fn is_listening(&self) -> bool {
        self.phase == ListenerPhase::Listening
    }

    pub fn idle_for(&self) -> Option<Duration> {
        self.last_activity_at.map(|activity| activity.elapsed())
    }

    pub fn is_live(&self, maximum_idle: Duration) -> bool {
        match self.idle_for() {
            Some(idle) => self.is_listening() && idle <= maximum_idle,
            None => false,
        }
    }

    /// How long the listener has been attached to `console.log` without a
    /// single byte arriving. `None` unless it is listening and has seen
    /// nothing at all since it attached. A non-trivial value here almost
    /// always means Deadlock is running without `-condebug`.
    pub fn silent_since_attach(&self) -> Option<Duration> {
        if self.phase != ListenerPhase::Listening || self.last_activity_at.is_some() {
            return None;
        }
        self.listening_since.map(|since| since.elapsed())
    }
}

struct Worker {
    stop: Sender<()>,
    join: JoinHandle<()>,
}

pub struct ConsoleLogListener {
    status: Arc<Mutex<ListenerStatus>>,
    subscribers: Arc<Mutex<Vec<Sender<BridgeEvent>>>>,
    worker: Option<Worker>,
}

impl Default for ConsoleLogListener {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleLogListener {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(ListenerStatus::default())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            worker: None,
        }
    }

    pub fn subscribe(&self) -> Receiver<BridgeEvent> {
        let (sender, receiver) = mpsc::channel();
        let count = {
            let mut subscribers = lock_unpoisoned(&self.subscribers);
            subscribers.push(sender);
            subscribers.len()
        };
        log::debug!(target: "companion::bridge_listener", "listener_subscriber_added count={count}");
        receiver
    }

    pub fn start(&mut self, path: PathBuf) -> io::Result<()> {
        if self.worker.is_some()
            && lock_unpoisoned(&self.status).configured_path.as_ref() == Some(&path)
        {
            log::debug!(
                target: "companion::bridge_listener",
                "listener_start_noop path={:?}",
                path
            );
            return Ok(());
        }

        let restarting = self.worker.is_some();
        if restarting {
            log::info!(
                target: "companion::bridge_listener",
                "listener_restart path={:?}",
                path
            );
        }
        self.stop();
        let (mut initial_log, skip_first_success) = match open_log_file(&path, true) {
            Ok(log) => (Some(log), false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, false),
            Err(error) => {
                log::debug!(
                    target: "companion::bridge_listener",
                    "listener_initial_open_deferred path={:?} error={:?}",
                    path,
                    error
                );
                (None, true)
            }
        };
        let initial_phase = if initial_log.is_some() {
            ListenerPhase::Listening
        } else {
            ListenerPhase::WaitingForFile
        };
        {
            let mut status = lock_unpoisoned(&self.status);
            status.phase = initial_phase;
            status.configured_path = Some(path.clone());
            status.current_error = None;
            status.last_activity_at = None;
            status.last_event_at = None;
            status.mod_version = ModVersionObservation::Unknown;
            status.listening_since = if initial_phase == ListenerPhase::Listening {
                Some(Instant::now())
            } else {
                None
            };
        }
        if let Some(log) = initial_log.as_mut()
            && let Err(error) = seed_metadata_from_tail(log, &self.status)
        {
            log::warn!(
                target: "companion::bridge_listener",
                "bridge_metadata_seed_failed error_kind=io error={error:?}"
            );
        }
        log::info!(
            target: "companion::bridge_listener",
            "listener_start path={:?} phase={:?}",
            path,
            initial_phase
        );

        let status = Arc::clone(&self.status);
        let subscribers = Arc::clone(&self.subscribers);
        let (stop_sender, stop_receiver) = mpsc::channel();
        match thread::Builder::new()
            .name("deadlock-console-listener".to_owned())
            .spawn(move || {
                worker_loop(
                    path,
                    status,
                    subscribers,
                    stop_receiver,
                    initial_log,
                    skip_first_success,
                )
            }) {
            Ok(join) => {
                self.worker = Some(Worker {
                    stop: stop_sender,
                    join,
                });
                Ok(())
            }
            Err(error) => {
                log::error!(
                    target: "companion::bridge_listener",
                    "listener_thread_spawn_failed path={:?} error={:?}",
                    lock_unpoisoned(&self.status).configured_path,
                    error
                );
                let mut status = lock_unpoisoned(&self.status);
                status.phase = ListenerPhase::Failed;
                status.current_error = Some(format!("could not start listener thread: {error}"));
                Err(error)
            }
        }
    }

    pub fn stop(&mut self) {
        let had_worker = self.worker.is_some();
        let previous_phase = lock_unpoisoned(&self.status).phase;
        if let Some(worker) = self.worker.take() {
            let _ = worker.stop.send(());
            let _ = worker.join.join();
        }
        let mut status = lock_unpoisoned(&self.status);
        status.phase = ListenerPhase::Stopped;
        status.current_error = None;
        status.mod_version = ModVersionObservation::Unknown;
        status.listening_since = None;
        if had_worker || previous_phase != ListenerPhase::Stopped {
            log::info!(
                target: "companion::bridge_listener",
                "listener_stop path={:?}",
                status.configured_path
            );
        }
    }

    pub fn status(&self) -> ListenerStatus {
        lock_unpoisoned(&self.status).clone()
    }

    pub fn configured_path(&self) -> Option<PathBuf> {
        self.status().configured_path
    }

    pub fn current_error(&self) -> Option<String> {
        self.status().current_error
    }

    pub fn is_listening(&self) -> bool {
        self.status().is_listening()
    }

    pub fn idle_for(&self) -> Option<Duration> {
        self.status().idle_for()
    }

    pub fn is_live(&self, maximum_idle: Duration) -> bool {
        self.status().is_live(maximum_idle)
    }
}

impl Drop for ConsoleLogListener {
    fn drop(&mut self) {
        self.stop();
    }
}

struct OpenLog {
    file: File,
    identity: FileIdentity,
    offset: u64,
    checkpoint: Vec<u8>,
    incomplete_line: Vec<u8>,
    discarding_oversized_line: bool,
}

fn worker_loop(
    path: PathBuf,
    status: Arc<Mutex<ListenerStatus>>,
    subscribers: Arc<Mutex<Vec<Sender<BridgeEvent>>>>,
    stop: Receiver<()>,
    mut open_log: Option<OpenLog>,
    mut skip_first_success: bool,
) {
    let mut read_buffer = [0_u8; READ_BUFFER_BYTES];
    if let Some(log) = open_log.as_mut() {
        let _ = seed_metadata_from_tail(log, &status);
    }

    loop {
        if stop.try_recv().is_ok() {
            break;
        }

        match fs::metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                skip_first_success = false;
                open_log = None;
                clear_mod_observation(&status);
                set_phase(&status, ListenerPhase::WaitingForFile, None);
            }
            Err(error) => {
                clear_mod_observation(&status);
                set_phase(
                    &status,
                    ListenerPhase::Failed,
                    Some(format!("could not inspect {}: {error}", path.display())),
                );
            }
            Ok(metadata) if !metadata.is_file() => {
                open_log = None;
                clear_mod_observation(&status);
                set_phase(
                    &status,
                    ListenerPhase::Failed,
                    Some(format!("{} is not a file", path.display())),
                );
            }
            Ok(path_metadata) => {
                let replaced = match (FileIdentity::from_path(&path), open_log.as_ref()) {
                    (Ok(path_identity), Some(log)) => log.identity != path_identity,
                    _ => false,
                };
                if replaced {
                    log::info!(
                        target: "companion::bridge_listener",
                        "listener_log_replaced path={:?}",
                        path
                    );
                    open_log = None;
                    clear_mod_observation(&status);
                }

                if open_log.is_none() {
                    let seed_metadata = skip_first_success;
                    match open_log_file(&path, skip_first_success) {
                        Ok(mut log) => {
                            if seed_metadata {
                                let _ = seed_metadata_from_tail(&mut log, &status);
                            }
                            open_log = Some(log);
                            skip_first_success = false;
                            set_phase(&status, ListenerPhase::Listening, None);
                        }
                        Err(error) => {
                            set_phase(
                                &status,
                                ListenerPhase::Failed,
                                Some(format!("could not open {}: {error}", path.display())),
                            );
                        }
                    }
                }

                let offset_before = open_log.as_ref().map(|log| log.offset);
                let truncation_error = open_log
                    .as_mut()
                    .and_then(|log| ensure_not_truncated(log, path_metadata.len()).err());
                if offset_before != open_log.as_ref().map(|log| log.offset) {
                    clear_mod_observation(&status);
                }
                if let Some(error) = truncation_error {
                    clear_mod_observation(&status);
                    set_phase(
                        &status,
                        ListenerPhase::Failed,
                        Some(format!(
                            "could not check {} for truncation: {error}",
                            path.display()
                        )),
                    );
                } else if let Some(log) = &mut open_log {
                    match read_available(
                        log,
                        path_metadata.len(),
                        &mut read_buffer,
                        &status,
                        &subscribers,
                    ) {
                        Ok(()) => set_phase(&status, ListenerPhase::Listening, None),
                        Err(error) => {
                            set_phase(
                                &status,
                                ListenerPhase::Failed,
                                Some(format!("could not read {}: {error}", path.display())),
                            );
                        }
                    }
                }
            }
        }

        match stop.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn open_log_file(path: &Path, skip_existing: bool) -> io::Result<OpenLog> {
    let mut file = File::open(path)?;
    let identity = FileIdentity::from_file(&file)?;
    let offset = file.seek(if skip_existing {
        SeekFrom::End(0)
    } else {
        SeekFrom::Start(0)
    })?;
    let checkpoint = read_checkpoint(&mut file, offset)?;
    Ok(OpenLog {
        file,
        identity,
        offset,
        checkpoint,
        incomplete_line: Vec::new(),
        discarding_oversized_line: false,
    })
}

fn ensure_not_truncated(log: &mut OpenLog, path_len: u64) -> io::Result<()> {
    if path_len < log.offset || !checkpoint_matches(log)? {
        log::info!(
            target: "companion::bridge_listener",
            "listener_log_reset reason=truncated_or_rewritten"
        );
        reset_after_truncation(log)?;
    }
    Ok(())
}

fn reset_after_truncation(log: &mut OpenLog) -> io::Result<()> {
    log.offset = log.file.seek(SeekFrom::Start(0))?;
    log.checkpoint.clear();
    log.incomplete_line.clear();
    log.discarding_oversized_line = false;
    Ok(())
}

fn read_checkpoint(file: &mut File, offset: u64) -> io::Result<Vec<u8>> {
    let checkpoint_len = offset.min(TAIL_CHECKPOINT_BYTES as u64) as usize;
    let mut checkpoint = vec![0; checkpoint_len];
    file.seek(SeekFrom::Start(offset - checkpoint_len as u64))?;
    file.read_exact(&mut checkpoint)?;
    file.seek(SeekFrom::Start(offset))?;
    Ok(checkpoint)
}

fn checkpoint_matches(log: &mut OpenLog) -> io::Result<bool> {
    if log.checkpoint.is_empty() {
        return Ok(true);
    }
    let checkpoint_start = log.offset - log.checkpoint.len() as u64;
    let mut current = [0_u8; TAIL_CHECKPOINT_BYTES];
    log.file.seek(SeekFrom::Start(checkpoint_start))?;
    log.file.read_exact(&mut current[..log.checkpoint.len()])?;
    log.file.seek(SeekFrom::Start(log.offset))?;
    Ok(current[..log.checkpoint.len()] == log.checkpoint)
}

fn update_checkpoint(checkpoint: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= TAIL_CHECKPOINT_BYTES {
        checkpoint.clear();
        checkpoint.extend_from_slice(&bytes[bytes.len() - TAIL_CHECKPOINT_BYTES..]);
        return;
    }
    let excess = checkpoint
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(TAIL_CHECKPOINT_BYTES);
    if excess > 0 {
        checkpoint.drain(..excess);
    }
    checkpoint.extend_from_slice(bytes);
}

fn read_available(
    log: &mut OpenLog,
    available_len: u64,
    read_buffer: &mut [u8],
    status: &Arc<Mutex<ListenerStatus>>,
    subscribers: &Arc<Mutex<Vec<Sender<BridgeEvent>>>>,
) -> io::Result<()> {
    loop {
        if log.offset >= available_len {
            return Ok(());
        }
        let remaining = available_len - log.offset;
        let read_len = remaining.min(read_buffer.len() as u64) as usize;
        let bytes_read = log.file.read(&mut read_buffer[..read_len])?;
        if bytes_read == 0 {
            return Ok(());
        }
        log.offset += bytes_read as u64;
        update_checkpoint(&mut log.checkpoint, &read_buffer[..bytes_read]);
        lock_unpoisoned(status).last_activity_at = Some(Instant::now());
        consume_bytes_with_metadata(
            &mut log.incomplete_line,
            &mut log.discarding_oversized_line,
            &read_buffer[..bytes_read],
            |metadata| {
                lock_unpoisoned(status).mod_version = metadata.mod_version;
            },
            |event| {
                let mut listener_status = lock_unpoisoned(status);
                if broadcast(subscribers, event) {
                    listener_status.last_event_at = Some(Instant::now());
                }
            },
        );
    }
}

#[cfg(test)]
fn consume_bytes(
    incomplete_line: &mut Vec<u8>,
    discarding_oversized_line: &mut bool,
    bytes: &[u8],
    deliver: impl FnMut(BridgeEvent),
) {
    consume_bytes_with_metadata(
        incomplete_line,
        discarding_oversized_line,
        bytes,
        |_| {},
        deliver,
    );
}

fn consume_bytes_with_metadata(
    incomplete_line: &mut Vec<u8>,
    discarding_oversized_line: &mut bool,
    bytes: &[u8],
    mut observe: impl FnMut(BridgeMetadata),
    mut deliver: impl FnMut(BridgeEvent),
) {
    for byte in bytes {
        if *discarding_oversized_line {
            if *byte == b'\n' {
                *discarding_oversized_line = false;
            }
            continue;
        }

        if *byte == b'\n' {
            let line = incomplete_line
                .strip_suffix(b"\r")
                .unwrap_or(incomplete_line.as_slice());
            match std::str::from_utf8(line) {
                Ok(line) => {
                    if line.contains(BRIDGE_RECORD_PREFIX) {
                        if let Some(metadata) = parse_bridge_metadata(line) {
                            observe(metadata);
                        }
                        match parse_bridge_record(line) {
                            Some(event) => deliver(event),
                            None => log_rejected_bridge_record(line),
                        }
                    }
                }
                Err(_) => {
                    if line
                        .windows(BRIDGE_RECORD_PREFIX.len())
                        .any(|window| window == BRIDGE_RECORD_PREFIX.as_bytes())
                    {
                        log::warn!(
                            target: "companion::bridge_listener",
                            "bridge_record_rejected reason=invalid_utf8"
                        );
                    }
                }
            }
            incomplete_line.clear();
        } else if incomplete_line.len() < MAX_INCOMPLETE_LINE_BYTES {
            incomplete_line.push(*byte);
        } else {
            incomplete_line.clear();
            *discarding_oversized_line = true;
            log::warn!(
                target: "companion::bridge_listener",
                "bridge_record_discarded reason=oversized_line limit_bytes={MAX_INCOMPLETE_LINE_BYTES}"
            );
        }
    }
}

fn log_rejected_bridge_record(line: &str) {
    let Some(prefix_at) = line.find(BRIDGE_RECORD_PREFIX) else {
        return;
    };
    let payload = &line[prefix_at + BRIDGE_RECORD_PREFIX.len()..];
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        log::warn!(
            target: "companion::bridge_listener",
            "bridge_record_rejected reason=malformed"
        );
        return;
    };
    let event = value.get("event").and_then(serde_json::Value::as_str);
    let schema = value.get("schema").and_then(serde_json::Value::as_u64);
    // Diagnostic events carry no trigger; the mod emits them so a live capture
    // shows raw HUD shape while a detector is being pinned down. Log the whole
    // payload at INFO instead of the terse "unsupported", so it lands in the
    // companion log the user shares.
    if event.is_some_and(|event| event.ends_with("_diag") || event == "objective_feed_unclassified")
    {
        log::info!(
            target: "companion::bridge_listener",
            "bridge_diagnostic event={} payload={payload}",
            event.unwrap_or("?")
        );
        return;
    }
    let known_event = matches!(
        event,
        Some(
            "hook_ready"
                | "local_player_death"
                | "local_player_respawn"
                | "damage_taken"
                | "healing_received"
                | "ally_healed"
                | "ally_shielded"
                | "soul_deny"
                | "soul_secure"
                | "parry_success"
                | "parry_fail"
                | "objective_guardian"
                | "objective_walker"
                | "objective_base_guardian"
                | "objective_shrine"
                | "objective_patron_weakened"
                | "game_won"
                | "damage_taken_intensity"
                | "damage_given"
        )
    );
    let schema_mismatch = schema.is_some_and(|schema| schema != u64::from(BRIDGE_SCHEMA));
    let reason = if schema_mismatch || !known_event {
        "unsupported"
    } else {
        "malformed"
    };
    log::warn!(
        target: "companion::bridge_listener",
        "bridge_record_rejected reason={reason}"
    );
}

fn clear_mod_observation(status: &Arc<Mutex<ListenerStatus>>) {
    lock_unpoisoned(status).mod_version = ModVersionObservation::Unknown;
}

fn seed_metadata_from_tail(
    log: &mut OpenLog,
    status: &Arc<Mutex<ListenerStatus>>,
) -> io::Result<()> {
    let end = log.offset;
    let start = end.saturating_sub(HISTORICAL_SCAN_BYTES as u64);
    log.file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((end - start) as usize);
    log.file
        .by_ref()
        .take(end - start)
        .read_to_end(&mut bytes)?;
    log.file.seek(SeekFrom::Start(end))?;
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let complete = &bytes[..complete_len];
    let mut lines = complete.split(|byte| *byte == b'\n');
    if start > 0 {
        let _ = lines.next();
    }
    let mut newest = lines.filter_map(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        std::str::from_utf8(line)
            .ok()
            .and_then(parse_bridge_metadata)
    });
    if let Some(metadata) = newest.next_back() {
        lock_unpoisoned(status).mod_version = metadata.mod_version;
    }
    Ok(())
}

fn broadcast(subscribers: &Arc<Mutex<Vec<Sender<BridgeEvent>>>>, event: BridgeEvent) -> bool {
    let mut delivered = false;
    let mut stale = 0_usize;
    lock_unpoisoned(subscribers).retain(|subscriber| {
        if subscriber.send(event.clone()).is_ok() {
            delivered = true;
            true
        } else {
            stale += 1;
            false
        }
    });
    if stale > 0 {
        log::debug!(
            target: "companion::bridge_listener",
            "listener_stale_subscribers_removed count={stale}"
        );
    }
    delivered
}

fn set_phase(
    status: &Arc<Mutex<ListenerStatus>>,
    phase: ListenerPhase,
    current_error: Option<String>,
) {
    let mut status = lock_unpoisoned(status);
    let phase_changed = status.phase != phase;
    let error_changed = status.current_error != current_error;
    if !phase_changed && !error_changed {
        return;
    }
    let previous_phase = status.phase;
    status.phase = phase;
    status.current_error = current_error.clone();
    if phase == ListenerPhase::Listening {
        if previous_phase != ListenerPhase::Listening {
            status.listening_since = Some(Instant::now());
        }
    } else {
        status.listening_since = None;
    }
    match phase {
        ListenerPhase::Failed => log::warn!(
            target: "companion::bridge_listener",
            "listener_phase phase=failed error={:?}",
            current_error
        ),
        ListenerPhase::Listening if previous_phase == ListenerPhase::Failed => log::info!(
            target: "companion::bridge_listener",
            "listener_recovered phase=listening"
        ),
        ListenerPhase::Listening => log::info!(
            target: "companion::bridge_listener",
            "listener_phase phase=listening"
        ),
        ListenerPhase::WaitingForFile => log::info!(
            target: "companion::bridge_listener",
            "listener_phase phase=waiting_for_file"
        ),
        ListenerPhase::Stopped => {}
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl FileIdentity {
    fn from_path(path: &Path) -> io::Result<Self> {
        Self::from_metadata(&fs::metadata(path)?)
    }

    fn from_file(file: &File) -> io::Result<Self> {
        Self::from_metadata(&file.metadata()?)
    }

    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    // NTFS file ID (volume + file index), not creation_time: NTFS "tunneling"
    // reuses a deleted file's creation timestamp for a same-named file
    // recreated within ~15s, which would otherwise hide a genuine log
    // rotation from truncation/replacement detection.
    volume_serial_number: u32,
    file_index: u64,
}

#[cfg(windows)]
impl FileIdentity {
    fn from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Self::from_file(&file)
    }

    fn from_file(file: &File) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let succeeded = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            volume_serial_number: info.dwVolumeSerialNumber,
            file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    created: Option<std::time::SystemTime>,
}

#[cfg(not(any(unix, windows)))]
impl FileIdentity {
    fn from_path(path: &Path) -> io::Result<Self> {
        Self::from_metadata(&fs::metadata(path)?)
    }

    fn from_file(file: &File) -> io::Result<Self> {
        Self::from_metadata(&file.metadata()?)
    }

    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            created: metadata.created().ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::mpsc::TryRecvError;

    const READY_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000000,\"poll_interval_ms\":100}";
    const VERSIONED_READY_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\",\"mod_version\":\"0.1.0\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000000,\"poll_interval_ms\":100}";
    const CATALOG_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_catalog\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000100,\"abilities\":[{\"ability_slot\":1,\"ability_name\":\"Kinetic Pulse\"},{\"ability_slot\":5}]}";
    const DEATH_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"local_player_death\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000123,\"sequence\":7,\"detection\":\"top_bar_local_player_dead_class\"}";
    const RESPAWN_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"local_player_respawn\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000140}";
    const KILL_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"local_player_kill\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000150,\"sequence\":8,\"detection\":\"top_bar_kills_stat_increment\",\"kills_before\":2,\"kills_after\":3}";
    const ASSIST_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"local_player_assist\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000175,\"sequence\":9,\"detection\":\"top_bar_assists_stat_increment\",\"assists_before\":0,\"assists_after\":1}";
    const ABILITY_USED_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_used\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000200,\"sequence\":8,\"ability_slot\":2,\"ability_name\":\"Kinetic Pulse\",\"detection\":\"charge_decrement\",\"charges_before\":3,\"charges_after\":2}";
    const ABILITY_READY_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_cooldown_ready\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000300,\"sequence\":9,\"ability_slot\":4,\"detection\":\"cooldown_finished\"}";
    const DAMAGE_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"damage_taken\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000400,\"sequence\":10,\"detection\":\"top_bar_health_and_shield_labels\",\"amount\":180.0,\"health\":420.0}";
    const HEALING_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"healing_received\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000500,\"sequence\":11,\"detection\":\"top_bar_health_and_shield_labels\",\"amount\":90.0,\"health\":510.0}";
    const ALLY_HEALED_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ally_healed\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000550,\"sequence\":12,\"detection\":\"impact_instance_class:is_heal\"}";
    const ALLY_SHIELDED_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ally_shielded\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000560,\"sequence\":13,\"detection\":\"impact_child_bar:barrierGained\"}";
    const SOUL_DENY_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"soul_deny\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000570,\"sequence\":14,\"detection\":\"feedback_indicator_class:deny\"}";
    const PARRY_FAIL_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"parry_fail\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000580,\"sequence\":15,\"detection\":\"stunned_after_parry\"}";
    const OBJECTIVE_WALKER_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"objective_walker\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000590,\"sequence\":16,\"detection\":\"objectives_map:tier2_alive_cleared\"}";
    const GAME_WON_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"game_won\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000600,\"sequence\":17,\"detection\":\"match_end:local_team_victory\"}";
    const DAMAGE_GIVEN_RECORD: &str = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"damage_given\",\"session_id\":\"abc-1\",\"client_time_ms\":1700000000590,\"sequence\":16,\"detection\":\"feedback_damage_numbers\",\"amount\":275.0}";

    #[test]
    fn metadata_parser_accepts_versions_without_accepting_future_events() {
        let record = "[DEADLOCK_DEATH_HOOK]{\"schema\":9,\"event\":\"future_event\",\"mod_version\":\"0.2.0\"}";
        assert_eq!(
            parse_bridge_metadata(record),
            Some(BridgeMetadata {
                schema: 9,
                event: "future_event".to_owned(),
                mod_version: ModVersionObservation::Reported("0.2.0".to_owned()),
            })
        );
        assert_eq!(parse_bridge_record(record), None);
    }

    #[test]
    fn metadata_parser_distinguishes_legacy_invalid_and_malformed() {
        let legacy =
            "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\",\"mod_version\":null}";
        assert_eq!(
            parse_bridge_metadata(legacy).map(|metadata| metadata.mod_version),
            Some(ModVersionObservation::Invalid)
        );
        let missing = "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\"}";
        assert_eq!(
            parse_bridge_metadata(missing).map(|metadata| metadata.mod_version),
            Some(ModVersionObservation::Legacy)
        );
        assert_eq!(parse_bridge_metadata("[DEADLOCK_DEATH_HOOK]{bad}"), None);
        let invalid_semver =
            "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\",\"mod_version\":\"new\"}";
        assert_eq!(
            parse_bridge_metadata(invalid_semver).map(|metadata| metadata.mod_version),
            Some(ModVersionObservation::Invalid)
        );
        let oversized = format!(
            "[DEADLOCK_DEATH_HOOK]{{\"schema\":1,\"event\":\"hook_ready\",\"mod_version\":\"{}\"}}",
            "1".repeat(MAX_METADATA_VERSION_BYTES + 1)
        );
        assert_eq!(
            parse_bridge_metadata(&oversized).map(|metadata| metadata.mod_version),
            Some(ModVersionObservation::Invalid)
        );
    }

    #[test]
    fn version_metadata_does_not_change_supported_gameplay_events() {
        for record in [
            READY_RECORD,
            CATALOG_RECORD,
            DEATH_RECORD,
            KILL_RECORD,
            ASSIST_RECORD,
            ABILITY_USED_RECORD,
            ABILITY_READY_RECORD,
            DAMAGE_RECORD,
            HEALING_RECORD,
        ] {
            let versioned = record.replacen(
                "{\"schema\":1,",
                "{\"schema\":1,\"mod_version\":\"0.1.0\",",
                1,
            );
            assert_eq!(parse_bridge_record(&versioned), parse_bridge_record(record));
        }
    }
    #[test]
    fn parser_accepts_exact_mod_payloads_and_console_decoration() {
        assert_eq!(
            parse_bridge_record(READY_RECORD),
            Some(BridgeEvent::HookReady(HookReady {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_000,
                poll_interval_ms: 100,
            }))
        );
        assert_eq!(
            parse_bridge_record(CATALOG_RECORD),
            Some(BridgeEvent::AbilityCatalog(AbilityCatalog {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_100,
                abilities: vec![
                    AbilityCatalogEntry {
                        ability_slot: 1,
                        ability_name: Some("Kinetic Pulse".to_owned()),
                    },
                    AbilityCatalogEntry {
                        ability_slot: 5,
                        ability_name: None,
                    },
                ],
            }))
        );
        assert_eq!(
            parse_bridge_record(&format!("[Console] {DEATH_RECORD}")),
            Some(BridgeEvent::LocalPlayerDeath(LocalPlayerDeath {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_123,
                sequence: 7,
                detection: "top_bar_local_player_dead_class".to_owned(),
            }))
        );
        assert_eq!(
            parse_bridge_record(RESPAWN_RECORD),
            Some(BridgeEvent::LocalPlayerRespawn(LocalPlayerRespawn {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_140,
            }))
        );
        assert_eq!(
            parse_bridge_record(KILL_RECORD),
            Some(BridgeEvent::LocalPlayerKill(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_150,
                sequence: 8,
                detection: "top_bar_kills_stat_increment".to_owned(),
                count_before: Some(2),
                count_after: Some(3),
            }))
        );
        assert_eq!(
            parse_bridge_record(ASSIST_RECORD),
            Some(BridgeEvent::LocalPlayerAssist(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_175,
                sequence: 9,
                detection: "top_bar_assists_stat_increment".to_owned(),
                count_before: Some(0),
                count_after: Some(1),
            }))
        );
        assert_eq!(
            parse_bridge_record(ALLY_HEALED_RECORD),
            Some(BridgeEvent::AllyHealed(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_550,
                sequence: 12,
                detection: "impact_instance_class:is_heal".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(ALLY_SHIELDED_RECORD),
            Some(BridgeEvent::AllyShielded(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_560,
                sequence: 13,
                detection: "impact_child_bar:barrierGained".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(SOUL_DENY_RECORD),
            Some(BridgeEvent::SoulDeny(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_570,
                sequence: 14,
                detection: "feedback_indicator_class:deny".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(PARRY_FAIL_RECORD),
            Some(BridgeEvent::ParryFail(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_580,
                sequence: 15,
                detection: "stunned_after_parry".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(OBJECTIVE_WALKER_RECORD),
            Some(BridgeEvent::ObjectiveWalker(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_590,
                sequence: 16,
                detection: "objectives_map:tier2_alive_cleared".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(GAME_WON_RECORD),
            Some(BridgeEvent::GameWon(CountTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_600,
                sequence: 17,
                detection: "match_end:local_team_victory".to_owned(),
                count_before: None,
                count_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(DAMAGE_GIVEN_RECORD),
            Some(BridgeEvent::DamageGiven(VitalsTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_590,
                sequence: 16,
                detection: "feedback_damage_numbers".to_owned(),
                amount: 275.0,
                health_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(ABILITY_USED_RECORD),
            Some(BridgeEvent::AbilityUsed(AbilityTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_200,
                sequence: 8,
                ability_slot: 2,
                ability_name: Some("Kinetic Pulse".to_owned()),
                detection: "charge_decrement".to_owned(),
                charges_before: Some(3),
                charges_after: Some(2),
            }))
        );
        assert_eq!(
            parse_bridge_record(ABILITY_READY_RECORD),
            Some(BridgeEvent::AbilityCooldownReady(AbilityTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_300,
                sequence: 9,
                ability_slot: 4,
                ability_name: None,
                detection: "cooldown_finished".to_owned(),
                charges_before: None,
                charges_after: None,
            }))
        );
        assert_eq!(
            parse_bridge_record(DAMAGE_RECORD),
            Some(BridgeEvent::DamageTaken(VitalsTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_400,
                sequence: 10,
                detection: "top_bar_health_and_shield_labels".to_owned(),
                amount: 180.0,
                health_after: Some(420.0),
            }))
        );
        assert_eq!(
            parse_bridge_record(HEALING_RECORD),
            Some(BridgeEvent::HealingReceived(VitalsTrigger {
                schema: 1,
                session_id: "abc-1".to_owned(),
                client_time_ms: 1_700_000_000_500,
                sequence: 11,
                detection: "top_bar_health_and_shield_labels".to_owned(),
                amount: 90.0,
                health_after: Some(510.0),
            }))
        );
    }

    #[test]
    fn parser_ignores_unrelated_unknown_and_malformed_records() {
        assert_eq!(parse_bridge_record("ordinary console output"), None);
        assert_eq!(
            parse_bridge_record("[DEADLOCK_DEATH_HOOK]{\"schema\":2,\"event\":\"hook_ready\"}"),
            None
        );
        assert_eq!(
            parse_bridge_record("[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"future_event\"}"),
            None
        );
        assert_eq!(parse_bridge_record("[DEADLOCK_DEATH_HOOK]{bad"), None);
        assert_eq!(
            parse_bridge_record(
                "[DEADLOCK_DEATH_HOOK]{\"schema\":2,\"event\":\"ability_used\",\"session_id\":\"abc-1\",\"client_time_ms\":1,\"sequence\":1,\"ability_slot\":1,\"detection\":\"activated\"}"
            ),
            None
        );
        assert_eq!(
            parse_bridge_record(
                "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"local_player_kill\",\"session_id\":\"abc-1\",\"client_time_ms\":1,\"sequence\":1}"
            ),
            None
        );
        assert_eq!(
            parse_bridge_record(
                "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_used\",\"session_id\":\"abc-1\",\"client_time_ms\":1,\"sequence\":1,\"detection\":\"activated\"}"
            ),
            None
        );
        assert_eq!(
            parse_bridge_record(
                "[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"ability_cooldown_ready\",\"session_id\":\"abc-1\",\"client_time_ms\":1,\"sequence\":1,\"ability_slot\":1,\"detection\":\"charge_restored\",\"charges_before\":\"one\"}"
            ),
            None
        );
    }

    #[test]
    fn existing_history_seeds_only_metadata_and_activity_requires_new_bytes() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        fs::write(&path, format!("{DEATH_RECORD}\n{VERSIONED_READY_RECORD}\n"))
            .expect("write history");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(listener.status().last_activity_at, None);
        assert_eq!(listener.status().last_event_at, None);
        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Reported("0.1.0".to_owned())
        );

        append(&path, "unrelated output\n");
        wait_until(|| listener.status().last_activity_at.is_some());
        assert_eq!(listener.status().last_event_at, None);

        append(&path, &format!("{ABILITY_USED_RECORD}\n"));
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("ability-used event"),
            parse_bridge_record(ABILITY_USED_RECORD).expect("parse expected ability use")
        );
        assert!(listener.status().last_event_at.is_some());
    }

    #[test]
    fn silent_since_attach_reports_until_first_bytes_then_clears() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        fs::write(&path, "old output that predates the listener\n").expect("seed history");
        let mut listener = ConsoleLogListener::new();
        let _events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        // Attached to a file that is not growing: the silence timer is running.
        let silent = listener
            .status()
            .silent_since_attach()
            .expect("silent while nothing has arrived");
        assert!(silent <= Duration::from_secs(5));

        append(&path, "fresh line\n");
        wait_until(|| listener.status().last_activity_at.is_some());
        assert_eq!(listener.status().silent_since_attach(), None);
    }

    #[test]
    fn historical_scan_uses_bounded_complete_newest_metadata() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        let mut history = vec![b'x'; HISTORICAL_SCAN_BYTES + 17];
        history.extend_from_slice(b"\n");
        history.extend_from_slice(
            b"[DEADLOCK_DEATH_HOOK]{\"schema\":1,\"event\":\"hook_ready\",\"mod_version\":\"0.1.0\"}\r\n",
        );
        history.extend_from_slice(b"[DEADLOCK_DEATH_HOOK]{bad}\r\n");
        history.extend_from_slice(
            b"[DEADLOCK_DEATH_HOOK]{\"schema\":9,\"event\":\"future\",\"mod_version\":\"0.2.0\"}\r\n",
        );
        history.extend_from_slice(
            b"[DEADLOCK_DEATH_HOOK]{\"schema\":9,\"event\":\"partial\",\"mod_version\":\"9.0.0\"}",
        );
        fs::write(&path, history).expect("write bounded history");

        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Reported("0.2.0".to_owned())
        );
        assert_eq!(listener.status().last_activity_at, None);
        assert_eq!(listener.status().last_event_at, None);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn historical_scan_stays_bounded_when_metadata_is_outside_window() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        let mut history = format!("{VERSIONED_READY_RECORD}\n").into_bytes();
        history.extend(std::iter::repeat_n(b'x', HISTORICAL_SCAN_BYTES + 1));
        history.push(b'\n');
        fs::write(&path, history).expect("write oversized history");

        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Unknown
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn live_future_and_invalid_metadata_update_status_without_delivery() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        File::create(&path).expect("create log");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        append(&path, "[DEADLOCK_DEATH_HOOK]{bad}\n");
        wait_until(|| listener.status().last_activity_at.is_some());
        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Unknown
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));

        append(
            &path,
            "[DEADLOCK_DEATH_HOOK]{\"schema\":9,\"event\":\"future\",\"mod_version\":\"0.2.0\"}\n",
        );
        wait_until(|| {
            listener.status().mod_version == ModVersionObservation::Reported("0.2.0".to_owned())
        });
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));

        append(
            &path,
            "[DEADLOCK_DEATH_HOOK]{\"schema\":9,\"event\":\"future\",\"mod_version\":\"invalid\"}\n",
        );
        wait_until(|| listener.status().mod_version == ModVersionObservation::Invalid);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(listener.status().last_event_at, None);
    }

    #[test]
    fn partial_lines_wait_for_newline_and_broadcast_to_every_subscriber() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        File::create(&path).expect("create log");
        let mut listener = ConsoleLogListener::new();
        let first = listener.subscribe();
        let second = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        append(&path, ABILITY_READY_RECORD);
        wait_until(|| listener.status().last_activity_at.is_some());
        assert_eq!(first.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(second.try_recv(), Err(TryRecvError::Empty));
        append(&path, "\n");

        let event =
            parse_bridge_record(ABILITY_READY_RECORD).expect("parse expected ability readiness");
        assert_eq!(
            first
                .recv_timeout(Duration::from_secs(2))
                .expect("first ability-readiness event"),
            event
        );
        assert_eq!(
            second
                .recv_timeout(Duration::from_secs(2))
                .expect("second ability-readiness event"),
            event
        );

        let late = listener.subscribe();
        assert_eq!(late.try_recv(), Err(TryRecvError::Empty));
        append(&path, &format!("{READY_RECORD}\n"));
        let ready = parse_bridge_record(READY_RECORD).expect("parse expected ready");
        assert_eq!(
            first
                .recv_timeout(Duration::from_secs(2))
                .expect("first ready"),
            ready
        );
        assert_eq!(
            second
                .recv_timeout(Duration::from_secs(2))
                .expect("second ready"),
            ready
        );
        assert_eq!(
            late.recv_timeout(Duration::from_secs(2))
                .expect("late ready"),
            ready
        );
    }

    #[test]
    fn oversized_records_are_discarded_without_losing_the_next_record() {
        let mut incomplete = Vec::new();
        let mut discarding = false;
        let mut bytes = vec![b'x'; MAX_INCOMPLETE_LINE_BYTES + 1];
        bytes.push(b'\n');
        bytes.extend_from_slice(READY_RECORD.as_bytes());
        bytes.push(b'\n');
        let mut events = Vec::new();

        consume_bytes(&mut incomplete, &mut discarding, &bytes, |event| {
            events.push(event)
        });

        assert!(incomplete.len() <= MAX_INCOMPLETE_LINE_BYTES);
        assert!(!discarding);
        assert_eq!(
            events,
            vec![parse_bridge_record(READY_RECORD).expect("parse expected ready")]
        );
    }

    #[test]
    fn missing_file_is_consumed_from_zero_when_created() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::WaitingForFile);

        fs::write(&path, format!("{READY_RECORD}\n")).expect("create log");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("ready event"),
            parse_bridge_record(READY_RECORD).expect("parse expected ready")
        );
    }

    #[test]
    fn truncation_and_replacement_are_consumed_from_zero() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        File::create(&path).expect("create log");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        append(&path, "padding that moves the listener offset\n");
        wait_until(|| listener.status().last_activity_at.is_some());
        fs::write(&path, format!("{ABILITY_READY_RECORD}\n")).expect("truncate and rewrite log");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("event after truncation"),
            parse_bridge_record(ABILITY_READY_RECORD).expect("parse expected ability readiness")
        );

        let replacement = temp.path().join("replacement.log");
        fs::write(&replacement, format!("{ABILITY_USED_RECORD}\n")).expect("write replacement");
        fs::remove_file(&path).expect("remove old log");
        fs::rename(&replacement, &path).expect("install replacement");
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("event after replacement"),
            parse_bridge_record(ABILITY_USED_RECORD).expect("parse expected ability use")
        );
    }

    #[test]
    fn changing_paths_joins_the_old_worker_and_tails_the_new_path() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let first_path = temp.path().join("first.log");
        let second_path = temp.path().join("second.log");
        File::create(&first_path).expect("create first log");
        fs::write(&second_path, format!("{READY_RECORD}\n")).expect("write second history");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener
            .start(first_path.clone())
            .expect("start first listener");
        wait_for_phase(&listener, ListenerPhase::Listening);

        listener
            .start(second_path.clone())
            .expect("restart on second path");
        wait_for_phase(&listener, ListenerPhase::Listening);
        assert_eq!(
            listener.configured_path().as_deref(),
            Some(second_path.as_path())
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        append(&first_path, &format!("{DEATH_RECORD}\n"));
        thread::sleep(POLL_INTERVAL * 2);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));

        append(&second_path, &format!("{ABILITY_USED_RECORD}\n"));
        assert_eq!(
            events
                .recv_timeout(Duration::from_secs(2))
                .expect("new-path event"),
            parse_bridge_record(ABILITY_USED_RECORD).expect("parse expected ability use")
        );
    }

    #[test]
    fn same_path_is_a_noop_and_stop_joins_the_worker() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("console.log");
        File::create(&path).expect("create log");
        let mut listener = ConsoleLogListener::new();
        let events = listener.subscribe();
        listener.start(path.clone()).expect("start listener");
        wait_for_phase(&listener, ListenerPhase::Listening);
        append(&path, &format!("{VERSIONED_READY_RECORD}\n"));
        events
            .recv_timeout(Duration::from_secs(2))
            .expect("live versioned ready event");
        wait_until(|| {
            listener.status().mod_version == ModVersionObservation::Reported("0.1.0".to_owned())
        });
        listener.start(path.clone()).expect("same-path no-op");
        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Reported("0.1.0".to_owned())
        );
        listener.stop();

        assert_eq!(listener.status().phase, ListenerPhase::Stopped);
        assert_eq!(
            listener.status().mod_version,
            ModVersionObservation::Unknown
        );
        append(&path, &format!("{READY_RECORD}\n"));
        thread::sleep(POLL_INTERVAL * 2);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        fs::remove_file(path).expect("joined worker released the log file");
    }

    fn append(path: &Path, contents: &str) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open log for append");
        file.write_all(contents.as_bytes()).expect("append log");
        file.flush().expect("flush log");
    }

    fn wait_for_phase(listener: &ConsoleLogListener, phase: ListenerPhase) {
        wait_until(|| listener.status().phase == phase);
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
