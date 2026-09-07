# Detecting "successful parry" and "soul orb deny" from the HUD

> **Status: implemented.** The mod (`death_http_bridge.js`) now emits `soul_deny`
> from `#HudEventIndicatorsPanel .deny`, and `parry_success` / `parry_fail` from
> the enemy-stun / self-stun cross-reference described below. `TriggerKind::SoulDeny`
> is back in the companion UI. `soul_secure` stays retired.

Research notes from reading the shipped Deadlock content (not just `panorama/`).
Source: `game/citadel/pak01_dir.vpk` (130,731 entries), read-only via a small
VPK reader. Nothing in the game install was modified.

## TL;DR

| Signal | Detectable from the HUD? | How |
| --- | --- | --- |
| **Soul orb deny** | **Yes, cleanly** | A dedicated `.deny` combat indicator under `#HudEventIndicatorsPanel`. |
| **Soul orb secure** | No | Every soul gain (orb, trooper, ability, walk-over) shows one flat `.gold` indicator. Nothing marks "you shot the orb". Retirement was correct. |
| **Successful parry** | **Only by inference** | No HUD class means "parry landed". Best proxy: your `parry_on_cooldown` fired **and** an enemy `.damageImpactInstance` gained `.Stunned` within ~1 s, while your own crosshair did **not** go `.stunned`. |

The companion side is already wired for all of these
(`bridge_listener.rs` parses `soul_deny` / `soul_secure` / `parry_success` /
`parry_fail`; `app.rs` has `TriggerKind::SoulDeny` / `SoulSecure` /
`ParrySuccess` / `ParryFail` with labels and icons). The gap is the **mod**
(`death_http_bridge.js`) never emitting `soul_deny` / `soul_secure`, and
emitting `parry_*` from a guess rather than a real signal.

---

## 1. Soul orb deny

### Finding

`panorama/styles/hud_event_indicator.vcss_c` defines the floating combat-number
system (the same `HudIndicatorText` / `damage_type_*` classes the mod already
reads for `damage_given`). Its category classes on each indicator instance are
**mutually exclusive** and include a dedicated deny class:

```css
.gold  .HudIndicatorContainer { animation-name: gold_pop; }          /* soul gained  */
.gold  .HudIndicatorText      { color: #70F8C1; font-size: 40px; }   /* teal, soul icon */
.gold  .HudIndicatorIcon      { background-image: url(".../icon_soul.vsvg"); }

.deny  .HudIndicatorContainer { animation-name: gold_pop; }           /* soul DENIED   */
.deny  .HudIndicatorText      { color: rgb(255, 117, 85);            /* orange-red    */
                                font-weight: bold; font-style: italic;
                                animation-name: crit_pop; }
```

Corroboration elsewhere in the pack:

- `panorama/styles/citadel_gold_history.vcss_c` -- the post-game gold breakdown
  has its own `.barChunk.gold_orb_deny` category with a `ColorDeny` swatch,
  i.e. "orb deny" is a first-class economy event the game tracks separately.
- `panorama/styles/hud_reportcard.vcss_c` -- training report card has
  `.trainingTargetIcon.orbs_denied` (and `orbs_secured`) counters.
- `panorama/layout/hero_testing/teach_orbs_deny.vxml_c` -- a whole tutorial
  screen (`#guide_teach_orbs_deny`) dedicated to the mechanic.

### Where it lives

`panorama/styles/hud.vcss_c`:

```css
#HudEventIndicatorsPanel { width: 100%; height: 100%; }
```

`#HudEventIndicatorsPanel` is a sibling of `#crosshair`,
`#CitadelHudDamageReport`, `#hudPlayerStats` and (importantly) `#damageImpactInfo`
-- all directly reachable from the HUD root the mod already climbs to. The
indicator instances are created by the C++ `HudEventIndicator` element from the
`panorama/layout/hud_event_indicator.vxml` snippet (root `Panel` +
`HudIndicatorContainer` > `Value` / `Icon` / `Effectiveness` / `Desc`), and the
category class is stamped on the instance.

### Recommended implementation (mod side)

Mirror the existing `pollDamageImpactAssists` pattern exactly:

```js
var eventIndicators = findChildById(hudRoot, "HudEventIndicatorsPanel");
// every few polls, prune credited refs, then:
var denies = findChildrenWithClass(eventIndicators, "deny");
for (each new instance not already credited) {
    emitAction("soul_deny", { detection: "feedback_indicator_class:deny" });
}
```

- `detection: "feedback_indicator_class:deny"` is exactly the string
  `bridge_listener.rs`'s `SOUL_DENY_RECORD` test already expects.
- Track credited panel references (pruned when invalid), like
  `creditedAssistPanels` / `creditedHealPanels`, because instances are torn down
  after their fade-out animation.
- Throttle the scan (`findChildById` traverses the whole tree) the same way the
  assist/support polls are throttled (`DAMAGE_IMPACT_POLL_INTERVAL_POLLS`).
- Gate on `!spectating`, like the other combat polls.

### Soul orb secure -- still not worth it

There is no "secured" class. `.gold` fires for **any** soul gain: last-hitting an
orb, last-hitting a trooper (there is a separate `.trooper` sizing class but it
is not "secure"), ability souls, or just walking over a dropped soul. The mod
comment in `app.rs` ("the HUD gives one flat gold number for every soul gain")
is accurate. Leave `SoulSecure` retired.

---

## 2. Successful parry

### Finding: there is no "parry landed" class anywhere in the HUD

The only parry-related panorama in the entire pack:

- `panorama/styles/ability_hud_elements/element_gun.vcss_c`:
  ```css
  .parry_on_cooldown #parry_unavailable { visibility: visible; }
  ```
  `parry_on_cooldown` sits on the `.ability_element_gun` crosshair element and
  only means "your parry is on cooldown" (you threw one). There is **no**
  success / hit / whiff variant.
- `.stunned` / `.disarmed` on the same `.ability_element_gun` element -- these
  are **you** being stunned / disarmed (drive `#StunnedIcon`, `#clip_status`,
  the reticle). This is the "got parried" side and confirms the mod's fix to
  scan the `ability_element_gun` subtree for `stunned`.
- `.parryRebuttal` in `ability_icons.vcss_c` -- just the Rebuttal item icon.
- `particles/abilities/melee/melee_parry_*.vpcf` +
  `particles/abilities/fencer/fencer_riposte_*` -- the successful-parry feedback
  is a **world particle effect** (`melee_parry_light` / `_magic` / `_ring` /
  `_symbol` / `_smoke` / `_model`) plus a sound. Panorama cannot see particles
  or hear sound, so this is a dead end for the mod.

Grep of `hud.vcss`, `hud_kill_hype`, `hud_data_feed`, `citadel_hud_subtitles`,
etc. for `parr` -- nothing. A successful parry produces no panel, no class, no
label, no data-feed row.

### The one usable proxy: the enemy's stun

`panorama/styles/hud_damage_impact.vcss_c` -- the `#damageImpactInfo` centre
popup (one `.damageImpactInstance` per enemy you hit; the mod already reads this
for assists / heals / shields) carries the **target's** status effects:

```css
.damageImpactInstance.ShowStatusEffect .statusEffectDisplay { opacity: 1.0; }
.damageImpactInstance.Stunned  .statusEffectLabel { color: gold; }
.Stunned .statusEffectIcon { background-image: url(".../condition_stun.vsvg"); }
/* plus .Asleep .Silenced .Disarmed .Immobolized .Cursed */
```

When your parry connects, the parried enemy is stunned, so their
`.damageImpactInstance` gets `.Stunned` + `.ShowStatusEffect`.

### Recommended parry state machine (mod side)

Extend the existing `pollParryState`:

1. `parry_on_cooldown` turns on -> you threw a parry; open a ~900-1200 ms window.
2. During the window:
   - your `ability_element_gun` subtree gains `.stunned`  -> **`parry_fail`**
     (`detection: "stunned_after_parry"`) -- current behaviour, keep it.
   - else an enemy `#damageImpactInfo .damageImpactInstance` gains `.Stunned`
     (with `.ShowStatusEffect`, and not `.killed`) -> **`parry_success`**
     (`detection: "enemy_stunned_after_parry"`).
3. Window closes with neither -> **emit nothing** (a whiffed parry). This is the
   real behaviour change: today the code emits `parry_success` by default for
   every parry press.

Caveat: `.Stunned` on a damage-impact instance is any stun you apply, not only
parries. Gating it to the short window right after *your* `parry_on_cooldown`
fires makes a false positive unlikely (you would have to land an unrelated
ability stun in that same ~1 s). It is still far better than "every parry press
counts as a success", and it will not fire at all when you parry nothing.

Optional extra confidence: also require a `.damage_type_melee` indicator in
`#HudEventIndicatorsPanel` within the window (a parry that lands also deals
melee damage).

---

## Appendix: how this was read

Deadlock ships no `vpk.exe` in the retail build and the Source2Viewer install
here is GUI-only, so a ~90-line Node script parses the VPK v2 directory
(`game/citadel/pak01_dir.vpk` + `pak01_NNN.vpk` archives) and greps the
compiled `*_c` panorama files, which keep their CSS/selector text inline.
Scripts live in the session scratchpad; nothing was written into the game
folder.
