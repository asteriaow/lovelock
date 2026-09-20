<a href="https://gamebanana.com/mods/718568"><img src="https://gamebanana.com/mods/embeddables/718568?type=large" alt="Lovelock mod on GameBanana" /></a>

# Lovelock Companion

Lovelock Companion is a desktop app that syncs local-player events in Deadlock to a Lovense toy over the local Standard API. It reacts to deaths, kills, assists, ability use and cooldowns, damage dealt and taken, healing received, healing or shielding a teammate, soul-orb denies, parries, objective takedowns (Guardian, Walker, Shrine, ...), and winning the match. See [Triggers](#triggers) for the full list.

## !!! Required Deadlock mod !!!

Lovelock Companion does not work by itself. Install and enable the [Lovelock mod from GameBanana](https://gamebanana.com/mods/718568) in Deadlock before starting the companion. The mod detects gameplay events and writes them to the log that the companion listens to.

Huge shoutout to volc/bolc for creating the original [DeadlockShock mod](https://gamebanana.com/mods/700758) that Lovelock Companion is built on.

## Lovense promotion

There is a special promotion with Lovense for Lovelock, running now until October 11, 2026. Check it out and grab a toy through [my Lovense link](https://www.lovense.com/a/Asteriaow).

This is an affiliate link, so I may earn a commission if you buy through it, at no extra cost to you.

## Disclaimer

This is an unofficial, third-party mod and companion app, not affiliated with or endorsed by Valve. Using client mods is against most games' terms of service in some form, and Deadlock is no exception. Use it at your own risk. I am not liable for any bans, suspensions, or other consequences you receive from using this software.

## Getting Started (no coding required)

You don't need to build anything or know how to code to use this. Here's everything, start to finish:

**What you need:**
- [Deadlock](https://store.steampowered.com/app/1422450/Deadlock/) installed via Steam
- A Lovense toy, plus the [Lovense Connect/Remote app](https://www.lovense.com/download) on the same PC you play Deadlock on
- Windows

**Steps:**

1. **Install the Lovelock mod.** Download it from [GameBanana](https://gamebanana.com/mods/718568) and follow GameBanana's install instructions (or use a mod manager like [Deadlock Mod Manager](https://deadlockmods.app/)) to get it into Deadlock's `addons` folder, then make sure it's enabled in Deadlock's in-game mods menu. If you copied the file in by hand instead of using a mod manager, also read [Troubleshooting](#troubleshooting), because Deadlock needs one extra setting to load the `addons` folder.

2. **Set Deadlock's launch option.** In Steam, right-click Deadlock → **Properties** → **General** → **Launch Options**, and add:
   ```
   -condebug
   ```
   This makes Deadlock write the log file Lovelock Companion reads from. Without it, nothing will work.

3. **Download Lovelock Companion.** Grab the Windows zip (`lovelock-vX.Y.Z-windows.zip`) from this repo's [Releases page](https://github.com/asteriaow/lovelock/releases). It contains the companion exe, the mod file (`pak01_dir.vpk`) and this README. No installer needed: unzip it and run the exe.

4. **Open the Lovense Remote app** and turn on **Game Mode**. Leave it running in the background.

5. **Run `companion.exe`.** In the **Setup** tab, click **Test connection**. The default connection settings already work for the common case (Lovense Remote on the same PC), so you usually don't need to change anything. Once it's connected, optionally pick a specific toy (or leave it unselected to vibrate every connected toy).

6. **Turn on your triggers.** Go to the **Effects** tab and enable the ones you want. Death is on by default; every other trigger is opt-in. Adjust each trigger's vibration strength/duration to taste. The full list is under [Triggers](#triggers).

7. **Launch Deadlock and play.** Lovelock Companion auto-detects the game and starts listening on its own. Just leave the companion window open in the background.

If something's not connecting, check **Menu → Show logs** inside the companion for live diagnostics.

## Troubleshooting

**Triggers don't fire, but the Game connection tab says "Listening".** "Listening" only means the companion is watching the log file. It does not mean Deadlock is sending anything. Check these in order:

1. **`-condebug` is set and the game was restarted.** See step 2 above. If the file `Deadlock\game\citadel\console.log` doesn't exist or never changes, this is the problem.
2. **The log contains lines from the mod.** Launch Deadlock, get to the main menu, then run this in PowerShell (change the path if your Steam library is elsewhere):
   ```powershell
   Select-String -Path "C:\Program Files (x86)\Steam\steamapps\common\Deadlock\game\citadel\console.log" -Pattern "DEADLOCK_DEATH_HOOK" | Select-Object -First 5
   ```
   If lines come back, the mod is running. If nothing comes back, the mod is not loading, so keep going.
3. **Deadlock is set up to load the `addons` folder.** A stock Deadlock does not load mods from `game\citadel\addons` on its own. Mod managers such as [Deadlock Mod Manager](https://deadlockmods.app/) add this for you, but if you copied the mod file in by hand you need it yourself. Open `Deadlock\game\citadel\gameinfo.gi`, find the `SearchPaths` block, and make sure this line is there, above `Mod citadel`:
   ```
   Game                citadel/addons
   ```
   Back the file up first. Game updates can overwrite `gameinfo.gi`, so if the mod stops working after an update, check this again (a mod manager re-applies it for you).
4. **The mod file is in the right place.** It should sit directly in `game\citadel\addons` (not in a subfolder) and be named `pakNN_dir.vpk`, for example `pak01_dir.vpk`. The file is small, about 90 KB. If you already have other mods, use a free number rather than overwriting one.
5. **Another mod isn't overriding the same file.** The Lovelock mod replaces a HUD layout file, so another HUD mod can take priority. Move your other mods out of `addons` for a test.
6. **Restart Deadlock fully** after changing any of the above.

**The companion says it can't reach Lovense.** Make sure the Lovense Remote app is open on the same PC with Game Mode on, then click **Test connection** in Setup. If your toy is paired to a phone instead, enter the domain and port shown on that phone's Game Mode screen.

If you're still stuck, open an issue and paste the output of **Menu → Show logs**.

## Triggers

Every trigger is configured independently in the **Effects** tab, each with its
own vibration profile (fixed strength/duration or a random interval). **Death**
is enabled by default; everything else is opt-in. When several fire at once, an
overlap-priority order (reorderable in the UI) decides which one drives the toy.

| Trigger | Fires when | Notes |
| --- | --- | --- |
| **Death** | Your hero dies | Can hold the toy until you respawn instead of using a fixed duration. Ability/assist triggers can be suppressed while you're dead so a spectated teammate doesn't cut the effect short. |
| **Kill** | Your kill-streak counter ticks up | Reads the on-screen kill-streak popup; no scoreboard (Tab) needed. |
| **Assist** | The on-screen "KILL ASSIST" popup credits you | No scoreboard needed. |
| **Ability use** | You cast an ability | Per-slot filter (applies across heroes); ability names shown when the mod reports them, numbered slots otherwise. |
| **Cooldown ready** | An ability comes off cooldown | Also covers a charged ability restoring a charge. Same per-slot filter as Ability use. |
| **Damage taken** | You take damage (health or shields) | Strength follows an intensity curve: a heavier beating over a rolling window drives the toy harder. |
| **Healing received** | Your health/shields go up | Amount-based: a rolling-window sum gated by a threshold you set. |
| **Damage given** | The game shows floating damage numbers for your hits | Amount-based: rolling-window sum gated by a threshold. |
| **Got parried** | You get stunned right after starting a parry | Detected from your crosshair's stunned state. |
| **Guardian destroyed** | An enemy Guardian falls | Fires when it falls, whoever takes it. |
| **Walker destroyed** | An enemy Walker falls | Fires when it falls, whoever takes it. |
| **Base Guardian destroyed** | An enemy Base Guardian falls | Fires when it falls, whoever takes it. |
| **Shrine destroyed** | An enemy Shrine falls | Fires when it falls, whoever takes it. |
| **Patron weakened** | The enemy Patron enters its first (weakened) phase | Fires when the phase starts, whoever causes it. |
| **Game won** | Your team wins the match | The match-end screen shows your team's victory (also fires if the enemy Core falls first). |
| **Game lost** | Your team lost the match | The match-end screen shows your team's loss (also fires if the your Core falls first). |

Copy a vibration profile between triggers with the explicit **Copy** control
(it copies only the active profile, not enablement or filters). Setup, the
Lovense connection, all vibration profiles, and ability filters are saved to
your OS user config directory between runs.


**Objectives.** Guardian, Walker, Base Guardian, Shrine and Patron triggers fire whenever that objective falls or changes phase, no matter who lands the final blow. You do not have to be the one to take it, and you do not have to be nearby. Each trigger's `detection` field in the log names which HUD surface fired it.

## Contents

- `companion/` is Lovelock Companion, the Lovense-only desktop app.
- `lovense/` is the crate Lovelock Companion uses to talk to Lovense toys over
  the local Standard API ("Game Mode"), via the Lovense Connect/Remote app
  running on the same LAN. In **Setup**, enable Game Mode in the Lovense
  Remote app on the same PC, then Test connection. Connection settings, toy
  selection, and per-trigger vibration settings are all persisted.

The [DeadlockShock mod](https://gamebanana.com/mods/700758) that feeds Lovelock Companion its game events is built
and published separately [DeadlockShock Repo](https://github.com/VolcanoCookies/deadlockshock).

## Preview

[![Preview](./media/showcase.png)](./media/showcase.png)

## Building from source

You will need [Rust](https://rust-lang.org/). Install and enable the DeadlockShock mod (see above), then build and run Lovelock Companion from the repo root:

```sh
cargo run --manifest-path companion/Cargo.toml --release
```

On Windows, `build_and_run.bat` builds Lovelock Companion in debug mode and launches it in one step, which is handy while iterating.

In **Setup**, enter the Lovense Connect/Remote domain and HTTP port, test the connection, and optionally pick a specific toy (leave unselected to vibrate every connected toy).

In **Effects**, configure each trigger independently; see [Triggers](#triggers) for the full list and what each one reacts to. Every trigger has its own vibration profile (fixed strength/duration, or a random interval). Ability-use and cooldown-ready also have independent positional-slot filters that apply across heroes; ability names appear when the addon reports them, with numbered slots as the fallback. Use the explicit Copy control to copy only the active vibration profile between triggers without changing enablement or ability selection.

In **Game connection**, Lovelock Companion automatically resumes a saved `console.log` path or auto-detects Deadlock and starts the listener at launch. Use **Auto-detect** and **Start/Restart listener** for diagnostics, retry, or a manual path override. Deadlock must run with `-condebug` so the log is written.
On Windows releases Lovelock Companion uses the GUI application subsystem, so launching it from Explorer does not open a command window. On Windows and Linux, open **Menu → Show logs** for selectable startup and live diagnostics. Logs are retained only in memory for the current run and are not written to a persistent log file.

Lovelock Companion remembers your setup, including the Lovense connection, every trigger's vibration profile, the overlap-priority order, and ability filters, in your OS user config directory. Ability names are runtime diagnostics and are not saved.

## Provider/action architecture

`src/provider.rs` owns the Lovense connection snapshot, connected blocking client, toy targets, test action, execution, and disconnect. `src/action.rs` owns vibration settings, validation, immutable resolution, and safe summaries; `src/action_ui.rs` contains the explicit egui editor. `src/theme.rs` holds Lovelock Companion's visual identity: a pastel bubblegum-pink accent on a dusty-plum dark theme, paired with the Baloo 2 display font and Atkinson Hyperlegible body font. Event acceptance resolves an action before a bounded worker queue, so later UI edits cannot change queued work and provider calls never run on the egui thread.

Saved state is strict schema 7 JSON; anything else (including old multi-provider saves) resets to defaults and the old file is preserved alongside it as a backup rather than migrated, since Lovelock Companion is a from-scratch Lovense-only companion.

```sh
git tag v<version>
git push origin v<version>
```

Drone verifies `DRONE_TAG == v<companion Cargo version>` before building and publishing the companion artifacts. The [DeadlockShock mod](https://gamebanana.com/mods/700758) is built and published separately and is not part of this release pipeline.
