# Objective triggers: what the HUD exposes and how to wire them

> **Status: implemented.** Six opt-in `TriggerKind`s (`ObjectiveGuardian`,
> `ObjectiveWalker`, `ObjectiveBaseGuardian`, `ObjectiveShrine`,
> `ObjectivePatronWeakened`, `GameWon`), each with its own vibration profile.
> The mod's `pollObjectives` reads `#ObjectivesMap`, the `objective_health`
> bar, and `CitadelHudMatchEnd` as described below.

Research from reading the shipped Deadlock content (`game/citadel/pak01_dir.vpk`,
read-only). Goal: a per-objective trigger family (Guardian, Walker, Base
Guardian, Shrine, weakened Patron, game won) where each is independently
toggled and has its own vibration level.

## TL;DR

| Objective | Detectable? | Surface |
| --- | --- | --- |
| **Guardian destroyed** | **Yes, clean** | `#ObjectivesMap` → `#Team{N}Tier1_*` loses `.Alive` |
| **Walker destroyed** | **Yes, clean** | `#ObjectivesMap` → `#Team{N}Tier2_*` loses `.Alive` |
| **Patron destroyed / game won** | **Yes, clean** | `CitadelHudMatchEnd` `.ShowMatchEnd` + `.LocalPlayerTeam{N}.Team{N}Victory` |
| **Weakened Patron (1st phase)** | Best-effort | `objective_health` widget `.is_titan.is_weakened` |
| **Base Guardian destroyed** | Best-effort | `objective_health` widget `.is_barracks_boss.is_dead` |
| **Shrine destroyed** | Best-effort | `objective_health` widget `.is_shield_generator.is_dead` |

"Clean" = a persistent panel with a stable id/class the mod can diff every
poll, map-wide, no "were you standing near it" dependency. "Best-effort" = the
class exists but only on the single centre-screen objective-health bar, which
shows whichever objective you are currently near/contesting, so a kill happening
while you look elsewhere can be missed.

The companion already gives every `TriggerKind` its own `TriggerSettings`
(enable flag + fixed or random vibration profile), so "toggle each, different
level for each" is automatic once each objective is its own kind.

---

## 1. The clean surface: `#ObjectivesMap`

`panorama/styles/objectives_map.vcss_c` + the mod already ships
`<CitadelObjectivesMap id="ObjectivesMap" liveGame="true" />` in
`citadel_hud_top_bar.xml`, so `#ObjectivesMap` is guaranteed present in the
mod's HUD and live for the whole match (hidden only in `.GameStatePreGame`).

Inside it, one positioned panel per structure, each with a **stable id**:

```
#Team1Core   #Team1Titan
#Team1Tier1_1  #Team1Tier1_2  #Team1Tier1_3  #Team1Tier1_4   -- Guardians
#Team1Tier2_1  #Team1Tier2_2  #Team1Tier2_3  #Team1Tier2_4   -- Walkers
   (and the Team2* mirror)
```

- `Tier1_*` = **Guardians**, `Tier2_*` = **Walkers**, `Core` / `Titan` = the
  Patron area (Titan is the Patron entity, Core the throne behind it).
- Each panel carries `.Alive` while the structure stands; the class is removed
  when it is destroyed (`.Alive.Team1{...}` / `.Team1IsEnemy .Alive.Team1{...}`
  in the vcss — `.Alive` is a per-panel state class).
- The container carries `.Team1IsEnemy` / `.Team1IsFriend` /
  `.Team2IsEnemy` / `.Team2IsFriend` (used as ancestor selectors), so the mod
  reads which side is the enemy off `#ObjectivesMap` (or its parent) once.
- `.ThreeLane` / `.FourLane` gate which `Tier*_2` / `Tier*_4` panels exist;
  just iterate the ids that resolve and have ever been `.Alive`.

**Detection:** each poll, for every enemy-side `Tier1_*` / `Tier2_*` / `Core`
panel, read `.Alive`. Baseline on first sight; on an `Alive: true -> false`
edge, emit the matching objective event. No text, no localisation, works
regardless of where the player is.

## 2. The clean surface: `CitadelHudMatchEnd` (game won)

`panorama/styles/hud_match_end.vcss_c`. The `CitadelHudMatchEnd` panel gains:

- `.ShowMatchEnd` when the end screen is up,
- `.LocalPlayerTeam1` / `.LocalPlayerTeam2` — the local player's team,
- `.Team1Victory` / `.Team2Victory` — the winning team,
- also `.MatchComplete`, `.MatchFinished`, `.MatchAbandoned`.

Winning = `.ShowMatchEnd` **and** (`.LocalPlayerTeam1` + `.Team1Victory`) or
(`.LocalPlayerTeam2` + `.Team2Victory`). The vcss spells out the exact
combinations (`.LocalPlayerTeam1.Team2Victory` = you lost). Emit once per
`.ShowMatchEnd` rising edge; ignore `.MatchAbandoned`.

Find it via `findChildrenWithClass(hudRoot, "ShowMatchEnd")` or by walking for
the `CitadelHudMatchEnd` paneltype.

## 3. Best-effort: `objective_health` widget (Base Guardian, Shrine, weakened Patron)

`panorama/layout/hud_objective_health.vxml` mounts a single panel
`class="objective_health"` — the centre-screen boss health bar. Its class set
(`hud_objective_health.vcss_c`) is the full objective taxonomy:

```
.is_tier1  .is_tier2  .is_barracks_boss  .is_shield_generator
.is_titan  .is_weakened  .is_mid  .is_neutral
.is_active  .is_dead  .is_transforming  .friend
#team_friendly_container  #team_enemy_container
.localPlayerTeam1  .localPlayerTeam2  .TeamSpectator
```

So **Base Guardian** = `.is_barracks_boss`, **Shrine** = `.is_shield_generator`,
**weakened Patron** = `.is_titan.is_weakened`, and `.is_dead` marks the shown
objective as destroyed.

Caveat: this widget shows **one** objective — whichever you are near or
contesting. It is reliable for the Patron fight (everyone is there, the bar is
up the whole time, so `.is_titan` gaining `.is_weakened` and later `.is_dead`
is dependable). It is only partial for Shrines / Base Guardians: if you are
across the map when your team takes one, the widget may never show it.

`hud_team_objective_health` (`#base_boss_container`, `#shield_gen_container`,
`#team_neutral_container`, `.is_active` / `.any_active`) is a persistent
per-team strip, but it has **no `.is_dead`** — it just drops an objective from
its container when it stops being active, which is ambiguous with "no longer
contested". Usable only as a weak secondary signal (child disappeared).

Other fallbacks, both worse: `#ObjectivesFeed` under `#DataFeed` (C++-fed
text/icon rows) and `CitadelHudGameAnnouncements` toasts (`.AnnouncementTitle`
/ `.AnnouncementDescription`, `{s:title}` bound text -- language dependent).

---

## Proposed triggers

Six new opt-in `TriggerKind`s, each with its own `TriggerSettings` (so each
gets its own enable checkbox and its own vibration profile in the Effects
tab). Suggested UI group heading: **Objectives**.

| `TriggerKind` | label | event name | detection |
| --- | --- | --- | --- |
| `ObjectiveGuardian` | Guardian destroyed | `objective_guardian` | `objectives_map:tier1_alive_cleared` |
| `ObjectiveWalker` | Walker destroyed | `objective_walker` | `objectives_map:tier2_alive_cleared` |
| `ObjectiveBaseGuardian` | Base Guardian destroyed | `objective_base_guardian` | `objective_health:is_barracks_boss+is_dead` |
| `ObjectiveShrine` | Shrine destroyed | `objective_shrine` | `objective_health:is_shield_generator+is_dead` |
| `ObjectivePatronWeakened` | Patron weakened | `objective_patron_weakened` | `objective_health:is_titan+is_weakened` |
| `GameWon` | Game won | `game_won` | `match_end:local_team_victory` |

All fire only for **enemy** objectives (destroying your own is a griefing case
and the class data distinguishes friend/enemy anyway). All are count triggers
with a `sequence`, like `soul_deny` / `parry_success`.

This mirrors the existing one-event-name-per-trigger convention
(`parry_success`, `soul_deny`, ...), so on the companion side each is a new
`WireEvent` + `BridgeEvent` + `TriggerKind` arm with nothing clever in the
routing. (An alternative is a single `objective` event with an
`objective: "guardian" | ...` field routed like `ability_used` routes by slot
-- more future-proof if Valve adds structure types, but more code to add now.)

### Mod side (`death_http_bridge.js`)

One `pollObjectives(hudRoot)`, throttled (~5 polls; objectives change rarely),
gated on `!spectating`, baselined on player-panel reacquire so a mid-match load
never fires for already-dead structures:

1. `objMap = findCachedChildById(hudRoot, "ObjectivesMap")`; read enemy side
   from `.Team1IsEnemy` / `.Team2IsEnemy` on `objMap` or its parent; bail if
   neither (spectator / pre-game).
2. For each enemy `Team{N}Tier1_*`, `Team{N}Tier2_*`, `Team{N}Core`: diff
   `.Alive` against the last poll; on a cleared edge emit
   `objective_guardian` / `objective_walker` / (Core -> covered by `game_won`).
3. `oh = findCachedChildById(hudRoot, ...)` for the `objective_health` widget;
   if it is an enemy objective (`#team_enemy_container` present / not `.friend`)
   and newly `.is_dead`, emit by its type class; if `.is_titan` newly
   `.is_weakened`, emit `objective_patron_weakened`. Remember the last
   (type, dead, weakened) tuple to de-dupe across polls.
4. Match end: if a panel with `.ShowMatchEnd` is present and the local-team /
   victory classes line up, emit `game_won` once per rising edge.

### Companion side

Per new kind: `TriggerKind` variant, `TriggerSettings` field on
`TriggerSettingsSet` (default `enabled: false`), `get`/`get_mut` arm,
`label()` / `trigger_display_label()` / `trigger_icon()` arm, `PersistedTrigger`
field + `PersistedTriggerKind` variant + `From` arms, an entry in
`PRIORITY_ORDER_DEFAULT` (app) and `DEFAULT_PRIORITY_ORDER` (persistence), and
a `WireEvent` + `BridgeEvent` + `parse_bridge_record` arm in `bridge_listener`.
Same shape as the `SoulDeny` / `ParrySuccess` plumbing added earlier.

### Open items to confirm live (panorama debugger)

- Exact panel/class that carries `.Team{N}IsEnemy` (on `#ObjectivesMap` vs a
  parent).
- Whether `Core` or `Titan` is the one that loses `.Alive` on the final kill,
  and whether either reflects the weakened state (if `Titan` flips on weaken,
  the weakened-Patron trigger can move to the clean surface too).
- Whether `objective_health` is reachable by a stable id or only by
  `class="objective_health"` (use `findChildrenWithClass` if so).
- Whether Shrines / Base Guardians ever appear in `objective_health` in
  practice, or if that trigger needs the announcements fallback.
