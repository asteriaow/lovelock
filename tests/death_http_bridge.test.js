import { afterEach, describe, expect, test } from "bun:test";

const source = await Bun.file("mod/panorama/scripts/death_http_bridge.js").text();
const originalDateNow = Date.now;
const originalMathRandom = Math.random;

function createPanel({
    id = "",
    paneltype = "Panel",
    classes = [],
    text = null,
    children = [],
    attributes = {},
    properties = {},
} = {}) {
    let parent = null;
    let valid = true;
    let classNames = new Set(classes);
    let childPanels = children;
    const attributeValues = { ...attributes };
    const panel = {
        id,
        paneltype,
        visible: true,
        enabled: true,
        text,
        style: {},
        ...properties,
        IsValid: () => valid,
        GetParent: () => parent,
        Children: () => childPanels,
        BHasClass: (className) => classNames.has(className),
        GetAttributeString: (name, fallback) =>
            Object.hasOwn(attributeValues, name) ? attributeValues[name] : fallback,
        FindChildTraverse: (wantedId) => {
            for (const child of childPanels) {
                if (child.id === wantedId) {
                    return child;
                }
                const nested = child.FindChildTraverse(wantedId);
                if (nested) {
                    return nested;
                }
            }
            return null;
        },
        FindChildrenWithClassTraverse: (className) => {
            const matches = [];
            for (const child of childPanels) {
                if (child.BHasClass(className)) {
                    matches.push(child);
                }
                matches.push(...child.FindChildrenWithClassTraverse(className));
            }
            return matches;
        },
        setAttribute: (name, value) => {
            if (value === null) {
                delete attributeValues[name];
            } else {
                attributeValues[name] = value;
            }
        },
        setChildren: (nextChildren) => {
            childPanels = nextChildren;
            for (const child of childPanels) {
                child.setParent(panel);
            }
        },
        setClasses: (nextClasses) => { classNames = new Set(nextClasses); },
        setParent: (nextParent) => { parent = nextParent; },
        setText: (nextText) => { panel.text = nextText; },
        setValid: (nextValid) => { valid = nextValid; },
    };
    panel.setChildren(childPanels);
    return panel;
}

function createAbilityEntry({
    identity = "ability_test",
    name = "Test Ability",
    classes = ["trained", "Tier0"],
    charges = 3,
    maxCharges = 3,
} = {}) {
    const abilityName = createPanel({ classes: ["ability_name"], paneltype: "Label", text: name });
    const cooldownTimer = createPanel({ classes: ["cooldown_timer"], paneltype: "Label", text: "" });
    const chargeProgress = createPanel({
        paneltype: "ProgressBarWithMiddle",
        properties: { lowervalue: charges, uppervalue: charges, max: maxCharges },
    });
    const chargeContainer = createPanel({ classes: ["stack_charges"], children: [chargeProgress] });
    const entry = createPanel({
        classes: ["ability_container", ...classes],
        attributes: { ability_id: identity, ability_slot: "signature_1" },
        children: [abilityName, cooldownTimer, chargeContainer],
    });
    return { abilityName, chargeContainer, chargeProgress, cooldownTimer, entry };
}

// Sibling of the count Label inside KillStreakText in the live HUD: a panel
// holding the "Kill Streak!" / "First Blood!" banner labels. Present so the
// mod's "first direct Label child" selection is actually exercised.
function killStreakBanner() {
    return createPanel({
        children: [
            createPanel({ paneltype: "Label", text: "Kill\nStreak!" }),
            createPanel({ paneltype: "Label", text: "First\nBlood!" }),
        ],
    });
}

function createHarness({
    initiallyDead = false,
    playerAvailable = true,
    abilityAvailable = true,
    ability = {},
    kills = 0,
    assists = 0,
} = {}) {
    let dead = initiallyDead;
    let available = playerAvailable;
    let rootAvailable = abilityAvailable;
    let contextValid = true;
    let now = 1_700_000_000_000;
    const scheduled = [];
    const messages = [];

    let killsLabel = createPanel({ classes: ["PlayerStat", "kills"], paneltype: "Label", text: String(kills) });
    let assistsLabel = createPanel({ classes: ["PlayerStat", "assists"], paneltype: "Label", text: String(assists) });
    let killStreakLabel = createPanel({ paneltype: "Label", text: null });
    let killStreakText = createPanel({ id: "KillStreakText", children: [killStreakLabel, killStreakBanner()] });
    let player = createPanel({
        paneltype: "CitadelHudTopBarPlayer",
        children: [killsLabel, assistsLabel, killStreakText],
    });
    player.BHasClass = (className) => className === "Dead" && dead;

    let abilityParts = createAbilityEntry(ability);
    const abilityRoot = createPanel({
        id: "hud_signature",
        paneltype: "CitadelHudAbilities",
        attributes: { hero_id: "hero_a" },
        children: [abilityParts.entry],
    });
    let damageImpactInstances = [];
    const damageImpactInfo = createPanel({ id: "damageImpactInfo", children: [] });
    const hudRoot = createPanel({ id: "HudCore", children: [abilityRoot, damageImpactInfo] });

    const context = createPanel();
    context.GetParent = () => rootAvailable ? hudRoot : null;
    context.FindChildrenWithClassTraverse = (className) =>
        className === "LocalPlayer" && available ? [player] : [];
    context.IsValid = () => contextValid;

    const panorama = {
        GetContextPanel: () => context,
        Msg: (message) => messages.push(message),
        Schedule: (delay, callback) => scheduled.push({ delay, callback }),
    };

    Date.now = () => now++;
    Math.random = () => 0.5;
    new Function("$", source)(panorama);

    function runNextPoll() {
        const next = scheduled.shift();
        expect(next).toBeDefined();
        expect(next.delay).toBe(0.1);
        next.callback();
    }

    function events(name) {
        const parsed = messages
            .filter((message) => message.startsWith("[DEADLOCK_DEATH_HOOK]"))
            .map((message) => JSON.parse(message.slice("[DEADLOCK_DEATH_HOOK]".length)));
        return name ? parsed.filter((event) => event.event === name) : parsed;
    }

    function setAbilityState({
        classes,
        charges,
        maxCharges,
        uppervalue,
        identity,
        name,
        cooldownText,
    }) {
        if (classes !== undefined) {
            abilityParts.entry.setClasses(["ability_container", ...classes]);
        }
        if (charges !== undefined) {
            abilityParts.chargeProgress.lowervalue = charges;
        }
        if (maxCharges !== undefined) {
            abilityParts.chargeProgress.max = maxCharges;
        }
        if (uppervalue !== undefined) {
            abilityParts.chargeProgress.uppervalue = uppervalue;
        }
        if (identity !== undefined) {
            abilityParts.entry.setAttribute("ability_id", identity);
        }
        if (name !== undefined) {
            abilityParts.abilityName.setText(name);
        }
        if (cooldownText !== undefined) {
            abilityParts.cooldownTimer.setText(cooldownText);
        }
    }

    function settle() {
        // Exhausts the mod's post-baseline settle window (BASELINE_SETTLE_POLLS
        // in death_http_bridge.js) so death/kill/assist transitions in the test
        // body are evaluated for real instead of being absorbed as loading-state
        // settle noise.
        for (let i = 0; i < 250; i++) {
            runNextPoll();
        }
    }

    function advanceDamageImpactScan() {
        // pollDamageImpactAssists only actually scans every
        // DAMAGE_IMPACT_POLL_INTERVAL_POLLS polls in death_http_bridge.js.
        for (let i = 0; i < 3; i++) {
            runNextPoll();
        }
    }

    return {
        events,
        runNextPoll,
        settle,
        advanceDamageImpactScan,
        scheduled,
        setAbilityState,
        setAbilityComplete: (complete) => {
            abilityParts.chargeContainer.setChildren(complete ? [abilityParts.chargeProgress] : []);
        },
        setAbilityAvailable: (value) => { rootAvailable = value; },
        setAvailable: (value) => { available = value; },
        setDead: (value) => { dead = value; },
        setHeroIdentity: (value) => { abilityRoot.setAttribute("hero_id", value); },
        replaceAbilityPanel: (nextAbility = {}) => {
            abilityParts = createAbilityEntry(nextAbility);
            abilityRoot.setChildren([abilityParts.entry]);
        },
        replacePlayer: () => {
            player.setValid(false);
            killsLabel = createPanel({ classes: ["PlayerStat", "kills"], paneltype: "Label", text: killsLabel.text });
            assistsLabel = createPanel({ classes: ["PlayerStat", "assists"], paneltype: "Label", text: assistsLabel.text });
            killStreakLabel = createPanel({ paneltype: "Label", text: killStreakLabel.text });
            killStreakText = createPanel({ id: "KillStreakText", children: [killStreakLabel, killStreakBanner()] });
            player = createPanel({
                paneltype: "CitadelHudTopBarPlayer",
                children: [killsLabel, assistsLabel, killStreakText],
            });
            player.BHasClass = (className) => className === "Dead" && dead;
        },
        setKills: (value) => { killsLabel.setText(String(value)); },
        setAssists: (value) => { assistsLabel.setText(String(value)); },
        setKillStreak: (value) => { killStreakLabel.setText(value === null ? null : String(value)); },
        spawnDamageImpactInstance: ({ name = "Enemy", assist = false } = {}) => {
            const instance = createPanel({
                id: name,
                classes: assist
                    ? ["team2", "damageImpactInstance", "assist", "fadeIn"]
                    : ["team2", "damageImpactInstance", "fadeIn"],
                children: [
                    createPanel({
                        id: "KillAssistContainer",
                        children: [
                            createPanel({ classes: ["killLabel"], paneltype: "Label", text: "KILL" }),
                            createPanel({ classes: ["assistLabel"], paneltype: "Label", text: "KILL ASSIST" }),
                        ],
                    }),
                ],
            });
            damageImpactInstances = [...damageImpactInstances, instance];
            damageImpactInfo.setChildren(damageImpactInstances);
            return instance;
        },
        removeDamageImpactInstance: (instance) => {
            instance.setValid(false);
            damageImpactInstances = damageImpactInstances.filter((candidate) => candidate !== instance);
            damageImpactInfo.setChildren(damageImpactInstances);
        },
        invalidateContext: () => { contextValid = false; },
    };
}

function actionable(harness) {
    return harness.events().filter((event) =>
        [
            "local_player_death",
            "local_player_kill",
            "local_player_assist",
            "ability_used",
            "ability_cooldown_ready",
        ].includes(event.event)
    );
}

function chargedClasses(...extra) {
    return ["trained", "Tier0", "has_stack_charges", ...extra];
}

afterEach(() => {
    Date.now = originalDateNow;
    Math.random = originalMathRandom;
});

describe("death_http_bridge", () => {
    test("emitted mod version matches the companion Cargo package version", async () => {
        const cargo = Bun.TOML.parse(await Bun.file("companion/Cargo.toml").text());
        const harness = createHarness();

        expect(new Set(harness.events().map((event) => event.mod_version))).toEqual(
            new Set([cargo.package.version]),
        );
    });
    test("emits ready and the initial meaningful schema-1 ability catalogue", () => {
        const harness = createHarness();

        expect(harness.events()).toEqual([
            {
                schema: 1,
                event: "hook_ready",
                mod_version: "0.1.0",
                session_id: expect.any(String),
                client_time_ms: expect.any(Number),
                poll_interval_ms: 100,
            },
            {
                schema: 1,
                event: "ability_catalog",
                mod_version: "0.1.0",
                session_id: expect.any(String),
                client_time_ms: expect.any(Number),
                abilities: [{
                    ability_slot: 1,
                    ability_name: "Test Ability",
                }],
            },
        ]);
        expect(harness.events("ability_catalog")[0]).not.toHaveProperty("sequence");
    });

    test("ability catalogue keeps names optional", () => {
        const harness = createHarness({ ability: { name: null } });

        expect(harness.events("ability_catalog")[0].abilities).toEqual([
            { ability_slot: 1 },
        ]);
    });

    test("refreshes the catalogue after hero, panel, identity, and name replacement", () => {
        const harness = createHarness();
        harness.setHeroIdentity("hero_b");
        harness.runNextPoll();
        harness.setAbilityState({ identity: "ability_replaced" });
        harness.runNextPoll();
        harness.replaceAbilityPanel({ identity: "ability_other", name: "Other Ability" });
        harness.runNextPoll();
        harness.setAbilityState({ name: "Renamed Ability" });
        harness.runNextPoll();

        expect(harness.events("ability_catalog").map((event) => event.abilities)).toEqual([
            [{ ability_slot: 1, ability_name: "Test Ability" }],
            [{ ability_slot: 1, ability_name: "Test Ability" }],
            [{ ability_slot: 1, ability_name: "Test Ability" }],
            [{ ability_slot: 1, ability_name: "Other Ability" }],
            [{ ability_slot: 1, ability_name: "Renamed Ability" }],
        ]);
    });

    test("ordinary and repeated dead polling do not spam the ability catalogue", () => {
        const harness = createHarness();
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setDead(true);
        harness.runNextPoll();
        harness.runNextPoll();
        harness.runNextPoll();

        expect(harness.events("ability_catalog")).toHaveLength(1);
    });

    test("uses one global sequence for interleaved ability and death events", () => {
        const harness = createHarness();
        harness.settle();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();
        harness.setDead(true);
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0"] });
        harness.setDead(false);
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        harness.runNextPoll();

        expect(actionable(harness).map(({ event, sequence }) => ({ event, sequence }))).toEqual([
            { event: "ability_used", sequence: 1 },
            { event: "local_player_death", sequence: 2 },
            { event: "ability_used", sequence: 3 },
        ]);
    });

    test("four charged decrements emit four exact use payloads while upper progress is ignored", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses(), charges: 4, maxCharges: 4 },
        });

        for (let charges = 3; charges >= 0; charges--) {
            harness.setAbilityState({ charges, uppervalue: charges + 0.75 });
            harness.runNextPoll();
        }

        expect(harness.events("ability_used")).toEqual([3, 2, 1, 0].map((charges, index) => ({
            schema: 1,
            event: "ability_used",
            mod_version: "0.1.0",
            session_id: expect.any(String),
            client_time_ms: expect.any(Number),
            sequence: index + 1,
            ability_slot: 1,
            ability_name: "Test Ability",
            detection: "charge_decrement",
            charges_before: charges + 1,
            charges_after: charges,
        })));
    });

    test("charged cooldown completion and later charge restoration are separate readiness events", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses("cooling_down"), charges: 1, maxCharges: 3 },
        });
        harness.setAbilityState({ classes: chargedClasses(), charges: 1 });
        harness.runNextPoll();
        harness.setAbilityState({ charges: 2, uppervalue: 1.25 });
        harness.runNextPoll();

        expect(harness.events("ability_cooldown_ready")).toEqual([
            expect.objectContaining({
                mod_version: "0.1.0",
                sequence: 1,
                detection: "cooldown_finished",
                ability_slot: 1,
            }),
            expect.objectContaining({
                mod_version: "0.1.0",
                sequence: 2,
                detection: "charge_restored",
                charges_before: 1,
                charges_after: 2,
            }),
        ]);
    });

    test("coalesces coincident charged readiness causes into one event", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses("cooling_down"), charges: 1, maxCharges: 3 },
        });
        harness.setAbilityState({ classes: chargedClasses(), charges: 2 });
        harness.runNextPoll();

        expect(harness.events("ability_cooldown_ready")).toEqual([
            expect.objectContaining({
                sequence: 1,
                detection: "cooldown_finished_and_charge_restored",
                charges_before: 1,
                charges_after: 2,
            }),
        ]);
    });

    test("coalesces non-charged active and cooling signals and detects either independently", () => {
        const simultaneous = createHarness();
        simultaneous.setAbilityState({ classes: ["trained", "Tier0", "active", "cooling_down"] });
        simultaneous.runNextPoll();
        expect(simultaneous.events("ability_used")).toEqual([
            expect.objectContaining({ detection: "cooldown_started_and_activated", sequence: 1 }),
        ]);

        const active = createHarness();
        active.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        active.runNextPoll();
        expect(active.events("ability_used")).toEqual([
            expect.objectContaining({ detection: "activated", sequence: 1 }),
        ]);

        const cooldown = createHarness();
        cooldown.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        cooldown.runNextPoll();
        expect(cooldown.events("ability_used")).toEqual([
            expect.objectContaining({ detection: "cooldown_started", sequence: 1 }),
        ]);
    });

    test("detects non-charged cooldown readiness", () => {
        const harness = createHarness({ ability: { classes: ["trained", "Tier0", "cooling_down"] } });
        harness.setAbilityState({ classes: ["trained", "Tier0"] });
        harness.runNextPoll();

        expect(harness.events("ability_cooldown_ready")).toEqual([
            expect.objectContaining({ detection: "cooldown_finished", sequence: 1 }),
        ]);
    });

    test("charged active and cooling signals do not substitute for a charge decrement", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses(), charges: 2, maxCharges: 3 },
        });
        harness.setAbilityState({ classes: chargedClasses("active", "cooling_down"), charges: 2 });
        harness.runNextPoll();

        expect(harness.events("ability_used")).toHaveLength(0);
    });

    test("initial full charges and upgrade-driven tier, maximum, and charge increases rebaseline", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses(), charges: 3, maxCharges: 3 },
        });
        harness.setAbilityState({ uppervalue: 2.5 });
        harness.runNextPoll();
        harness.setAbilityState({
            classes: ["trained", "Tier1", "has_stack_charges"],
            charges: 4,
            maxCharges: 4,
        });
        harness.runNextPoll();
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("hero and non-localized ability identity changes rebaseline", () => {
        const harness = createHarness();
        harness.setHeroIdentity("hero_b");
        harness.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        harness.runNextPoll();
        harness.setAbilityState({ identity: "ability_other", classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("localized ability name changes rebaseline when no stable identity is exposed", () => {
        const harness = createHarness({ ability: { identity: null, name: "Hero A Ability" } });
        harness.setAbilityState({
            name: "Hero B Ability",
            classes: ["trained", "Tier0", "cooling_down"],
        });
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("root and ability panel reacquisition suppress their first snapshots", () => {
        const harness = createHarness();
        harness.setAbilityAvailable(false);
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        harness.setAbilityAvailable(true);
        harness.runNextPoll();
        harness.replaceAbilityPanel({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("incomplete charged snapshots are ignored and completion establishes a new baseline", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses(), charges: 2, maxCharges: 3 },
        });
        harness.setAbilityComplete(false);
        harness.runNextPoll();
        harness.setAbilityState({ charges: 1 });
        harness.setAbilityComplete(true);
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("missing numeric charge properties establish a fresh baseline when restored", () => {
        const harness = createHarness({
            ability: { classes: chargedClasses(), charges: 2, maxCharges: 3 },
        });
        harness.setAbilityState({ charges: null });
        harness.runNextPoll();
        harness.setAbilityState({ charges: 1 });
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("death and respawn rebaseline abilities while preserving death behavior", () => {
        const harness = createHarness();
        harness.settle();
        harness.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        harness.setDead(true);
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();
        harness.setDead(false);
        harness.setAbilityState({ classes: ["trained", "Tier0", "active"] });
        harness.runNextPoll();

        expect(actionable(harness)).toEqual([
            expect.objectContaining({
                event: "local_player_death",
                sequence: 1,
                detection: "top_bar_local_player_dead_class",
                mod_version: "0.1.0",
            }),
        ]);
    });

    test("rejected targeting and presentation-only changes do not emit", () => {
        const harness = createHarness();
        harness.setAbilityState({
            classes: ["trained", "Tier0", "targeting", "ability_not_ready", "channeling"],
            name: "Localized Other Name",
            cooldownText: "9.7",
        });
        harness.runNextPoll();
        harness.setAbilityState({ cooldownText: "8.2" });
        harness.runNextPoll();

        expect(actionable(harness)).toHaveLength(0);
    });

    test("does not emit death when loaded dead or when a dead player panel is reacquired", () => {
        const startup = createHarness({ initiallyDead: true });
        startup.runNextPoll();
        expect(startup.events("local_player_death")).toHaveLength(0);

        const reacquired = createHarness();
        reacquired.setAvailable(false);
        reacquired.runNextPoll();
        reacquired.setDead(true);
        reacquired.replacePlayer();
        reacquired.setAvailable(true);
        reacquired.runNextPoll();
        expect(reacquired.events("local_player_death")).toHaveLength(0);
    });

    test("does not emit death for a transient dead reading right after a panel is reacquired", () => {
        // Regression test: loading/hero-select -> match transitions recreate
        // the top bar player panel. If the HUD briefly reports a "not yet
        // spawned" state as Dead right as the new panel appears, that must
        // not be mistaken for a real death.
        const harness = createHarness();
        harness.replacePlayer();
        harness.runNextPoll();
        harness.setDead(true);
        harness.runNextPoll();
        harness.setDead(false);
        harness.runNextPoll();

        expect(harness.events("local_player_death")).toHaveLength(0);
    });

    test("emits once for every alive-to-dead transition", () => {
        const harness = createHarness();
        harness.settle();
        harness.setDead(true);
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setDead(false);
        harness.runNextPoll();
        harness.setDead(true);
        harness.runNextPoll();

        expect(harness.events("local_player_death")).toEqual([
            expect.objectContaining({ sequence: 1, detection: "top_bar_local_player_dead_class" }),
            expect.objectContaining({ sequence: 2, detection: "top_bar_local_player_dead_class" }),
        ]);
    });

    test("emits a kill when the streak popup appears and again each time it climbs", () => {
        const harness = createHarness();
        harness.settle();
        harness.setKillStreak(1);
        harness.runNextPoll();
        harness.setKillStreak(2);
        harness.runNextPoll();

        expect(harness.events("local_player_kill")).toEqual([
            expect.objectContaining({
                detection: "kill_streak_counter_increment",
                kills_before: 0,
                kills_after: 1,
            }),
            expect.objectContaining({
                detection: "kill_streak_counter_increment",
                kills_before: 1,
                kills_after: 2,
            }),
        ]);
    });

    test("does not credit a streak that is already on screen when the baseline establishes", () => {
        const harness = createHarness();
        harness.setKillStreak(3);
        harness.settle();

        expect(harness.events("local_player_kill")).toHaveLength(0);

        harness.setKillStreak(4);
        harness.runNextPoll();

        expect(harness.events("local_player_kill")).toEqual([
            expect.objectContaining({ kills_before: 3, kills_after: 4 }),
        ]);
    });

    test("a one-poll gap in the popup does not reset the streak baseline", () => {
        const harness = createHarness();
        harness.settle();
        harness.setKillStreak(2);
        harness.runNextPoll();
        harness.setKillStreak(null);
        harness.runNextPoll();
        harness.setKillStreak(2);
        harness.runNextPoll();

        expect(harness.events("local_player_kill")).toEqual([
            expect.objectContaining({ kills_before: 0, kills_after: 2 }),
        ]);
    });

    test("the popup reappearing after a sustained hide counts as a fresh kill", () => {
        const harness = createHarness();
        harness.settle();
        harness.setKillStreak(2);
        harness.runNextPoll();
        harness.setKillStreak(null);
        harness.runNextPoll();
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setKillStreak(1);
        harness.runNextPoll();

        expect(harness.events("local_player_kill")).toEqual([
            expect.objectContaining({ kills_before: 0, kills_after: 2 }),
            expect.objectContaining({ kills_before: 0, kills_after: 1 }),
        ]);
    });

    test("does not emit when the streak counter holds steady or ticks down", () => {
        const harness = createHarness();
        harness.settle();
        harness.setKillStreak(2);
        harness.runNextPoll();
        harness.setKillStreak(2);
        harness.runNextPoll();
        harness.setKillStreak(1);
        harness.runNextPoll();

        expect(harness.events("local_player_kill")).toEqual([
            expect.objectContaining({ kills_before: 0, kills_after: 2 }),
        ]);
    });

    test("emits an assist when a damage-impact instance carries the assist class", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", assist: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toEqual([
            expect.objectContaining({ detection: "damage_impact_assist_class" }),
        ]);
    });

    test("does not emit an assist for a damage-impact instance without the assist class", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", assist: false });
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toHaveLength(0);
    });

    test("does not double-credit the same assist instance across polls", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", assist: true });
        harness.advanceDamageImpactScan();
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toHaveLength(1);
    });

    test("credits a second assist once the first instance is torn down", () => {
        const harness = createHarness();
        const first = harness.spawnDamageImpactInstance({ name: "Abrams", assist: true });
        harness.advanceDamageImpactScan();
        harness.removeDamageImpactInstance(first);
        harness.spawnDamageImpactInstance({ name: "Vindicta", assist: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toHaveLength(2);
    });

    test("credits simultaneous assists on separate instances independently", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", assist: true });
        harness.spawnDamageImpactInstance({ name: "Vindicta", assist: false });
        harness.spawnDamageImpactInstance({ name: "Lash", assist: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toHaveLength(2);
    });

    test("stops scheduling after its Panorama context is destroyed", () => {
        const harness = createHarness();
        harness.invalidateContext();
        harness.runNextPoll();
        expect(harness.scheduled).toHaveLength(0);
    });
});
