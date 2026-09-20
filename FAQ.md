# Lovelock FAQ and troubleshooting

Start with the section that matches your problem. If nothing here fixes it, open an issue and paste the output of **Menu → Show logs** in the companion.

- [The test vibration doesn't work](#the-test-vibration-doesnt-work)
- [The companion says "Listening" but triggers never fire](#the-companion-says-listening-but-triggers-never-fire)
- [One trigger doesn't work](#one-trigger-doesnt-work)
- [Vibration feels wrong or inconsistent](#vibration-feels-wrong-or-inconsistent)
- [General questions](#general-questions)

---

## The test vibration doesn't work

This is about the companion and your toy. The game and the mod aren't involved yet.

1. **Unzip the download.** Don't run the companion from inside the zip. Extract it and run the exe from somewhere that is **not** in your Deadlock game files, such as your Desktop.
2. **Put the toy in Game Mode.** In the Lovense Remote mobile app, open Game Mode and make sure the toy you want to use is connected and enabled there.
3. **Turn on LAN.** In the Game Mode settings, enable LAN. The app then shows a **local IP** and a **port**. Enter both in the matching boxes in the companion's Setup screen.
4. **Check the companion says connected.** Use **Test connection** in Setup. If it can't connect, the IP or port is wrong, or your phone and PC aren't on the same network.
5. **Same Wi-Fi.** Your phone and PC must be on the same network. A guest network or a VPN can block this.
6. **Bluetooth on.** Bluetooth has to be on for both the phone and the PC, and the toy must be paired to the phone.
7. **Toy on a PC instead of a phone?** If Lovense Remote runs on the same PC with Game Mode on, click **Test connection** with no IP. Otherwise use the phone's IP and port.

## The companion says "Listening" but triggers never fire

"Listening" only means the companion is watching Deadlock's log file. It doesn't mean the game is sending anything. Work through these in order:

1. **Launch Deadlock with `-condebug`.** In Steam, right-click Deadlock → Properties → Launch Options, add `-condebug`, then restart the game. Without it the game never writes `game\citadel\console.log`.
2. **Check the log file is changing.** If `Deadlock\game\citadel\console.log` doesn't exist, or its modified time is old, the game wasn't started with `-condebug`.
3. **Check the mod is loading.** Get to the main menu and run this in PowerShell (change the path if your Steam library is elsewhere):
   ```powershell
   Select-String -Path "C:\Program Files (x86)\Steam\steamapps\common\Deadlock\game\citadel\console.log" -Pattern "DEADLOCK_DEATH_HOOK" | Select-Object -First 5
   ```
   Lines coming back means the mod is running. No lines means it isn't loading, so keep going.
4. **Put the mod in the right folder.** `pak01_dir.vpk` goes directly in `Deadlock\game\citadel\addons`, not in a subfolder. If you already have other mods, use a free number (`pak02_dir.vpk`, and so on) rather than overwriting one.
5. **Let Deadlock load the `addons` folder.** A stock Deadlock doesn't load it on its own. Mod managers such as [Deadlock Mod Manager](https://deadlockmods.app/) add this for you. If you copied the file in by hand, open `Deadlock\game\citadel\gameinfo.gi`, find the `SearchPaths` block, and add this line above `Mod citadel`:
   ```
   Game                citadel/addons
   ```
   Back the file up first. Game updates can overwrite `gameinfo.gi`, so check it again if the mod stops working after an update.
6. **Check for another HUD mod.** Lovelock replaces a HUD layout file, so another HUD mod can take priority. Move your other mods out of `addons` for a test.
7. **Restart Deadlock fully** after changing any of the above.

## One trigger doesn't work

- **Is it switched on?** Each trigger has its own toggle in the companion. The log shows `trigger_disabled` when an event arrived but the trigger was off.
- **Ability triggers.** If the log shows `trigger_filtered reason=ability_not_selected`, the ability filter for that trigger excludes that ability.
- **Damage dealt.** It reads the game's floating damage numbers, so they must be visible in your HUD. If you don't see numbers when you shoot, Lovelock can't see them either.
- **Objectives** (Guardian, Walker, Base Guardian, Shrine, Patron). These fire whenever the objective falls or changes phase, no matter who lands the final blow. You don't have to take it yourself or be nearby.
- **Ignore … while dead.** With this option on for Death, other effects such as damage taken and healing are blocked while you're dead.

## Vibration feels wrong or inconsistent

- **Damage taken, Healing received and Damage dealt use a graph.** The strength comes from the total amount inside a rolling time window, not from a single hit. A hit right after another one lands at a higher level than the same hit alone.
- **Below the first point of the graph means no vibration.** If small hits or heals do nothing, move the first point to 0 or lower it.
- **Pulses closer than about 0.45 s apart are dropped**, so a busy fight ramps up rather than buzzing constantly.
- **Dying quickly cuts pulses short.** The death effect takes over from a damage pulse in progress.
- **Not sure why a pulse was skipped?** The log records `intensity_skipped reason=below_curve` or `reason=throttled` with the amounts.
- **Emergency stop.** Use the stop control in the companion to cancel whatever the toy is doing right now.

## General questions

**How does it work?** A small mod inside Deadlock reads what your HUD already shows and writes it to a local log file (`console.log`). The companion reads that file and sends vibrations to your toy. Nothing is injected into the game process. It ships as a normal addon `.vpk`.

**Where does the companion send data?** To your Lovense app over the local network (the Standard API), and it reads a local log file.

**Do I need the companion running before I start the game?** No, but it must be running for anything to happen. It picks up the log file whenever the game starts a new session.

**Where are my settings saved?** In your user config folder, under `deadlockshock-companion` (`state.json`). Delete that file to reset everything.

**A Deadlock update broke it.** Updates can rewrite `gameinfo.gi` (see the `addons` line above) or change HUD layouts. Check `gameinfo.gi` first, then look for a newer Lovelock release.

**How do I report a bug?** Open an issue and include **Menu → Show logs** from the companion, which Deadlock version you're on, and what you expected to happen.
