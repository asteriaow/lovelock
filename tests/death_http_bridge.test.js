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
    let eventIndicatorInstances = [];
    const eventIndicators = createPanel({ id: "HudEventIndicatorsPanel", children: [] });
    const healthRegenAndTotal = createPanel({ id: "HealthRegenAndTotal", children: [] });
    const feedbackDisplay = createPanel({
        id: "DamageFeedbackDisplay",
        paneltype: "CitadelDamageFeedbackDisplay",
        children: [],
    });
    let feedbackIndicatorInstances = [];
    let healthLabel = createPanel({ classes: ["currentHealthLabel"], paneltype: "Label", text: "600" });
    const healthBandWrapper = createPanel({ classes: [], children: [healthLabel] });
    const gunElement = createPanel({ classes: ["ability_element_gun"], children: [] });
    let bulletShieldValue = createPanel({ classes: ["progress_bar_current"], paneltype: "Label", text: "0" });
    let techShieldValue = createPanel({ classes: ["progress_bar_current"], paneltype: "Label", text: "0" });
    const bulletShieldContainer = createPanel({
        classes: ["BulletShieldNumbers"],
        children: [bulletShieldValue],
    });
    const techShieldContainer = createPanel({
        classes: ["TechShieldNumbers"],
        children: [techShieldValue],
    });
    let objectiveMapChildren = [];
    const objectivesMap = createPanel({ id: "ObjectivesMap", classes: [], children: [] });
    const objectiveHealth = createPanel({ classes: ["objective_health"], children: [] });
    let objectiveFeedRowPanels = [];
    const objectivesFeed = createPanel({ id: "ObjectivesFeed", classes: [], children: [] });
    const matchEnd = createPanel({ paneltype: "CitadelHudMatchEnd", classes: [], children: [] });
    const hudRoot = createPanel({
        id: "HudCore",
        children: [
            abilityRoot,
            damageImpactInfo,
            eventIndicators,
            healthRegenAndTotal,
            feedbackDisplay,
            healthBandWrapper,
            gunElement,
            bulletShieldContainer,
            techShieldContainer,
            objectivesMap,
            objectiveHealth,
            objectivesFeed,
            matchEnd,
        ],
    });

    const context = createPanel();
    context.GetParent = () => rootAvailable ? hudRoot : null;
    context.FindChildrenWithClassTraverse = (className) =>
        className === "LocalPlayer" && available ? [player] : [];
    context.IsValid = () => contextValid;

    const createdPanels = [];
    const panorama = {
        GetContextPanel: () => context,
        Msg: (message) => messages.push(message),
        Schedule: (delay, callback) => scheduled.push({ delay, callback }),
        CreatePanel: (paneltype, parent, id) => {
            const created = createPanel({ id, paneltype });
            if (parent && parent.setChildren) {
                parent.setChildren([...parent.Children(), created]);
            }
            createdPanels.push(created);
            return created;
        },
    };

    // Advance a frame's worth of ms per read so the mod's short wall-clock
    // timers (the parry resolve window, the damage-given flush) elapse over a
    // realistic number of polls instead of hundreds.
    Date.now = () => (now += 16);
    Math.random = () => 0.5;
    new Function("$", source)(panorama);

    function runNextPoll() {
        const next = scheduled.shift();
        expect(next).toBeDefined();
        expect(next.delay).toBe(0.1);
        next.callback();
    }

    function advancePolls(count) {
        for (let i = 0; i < count; i++) {
            runNextPoll();
        }
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
        advancePolls,
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
        setHealth: (value) => { healthLabel.setText(value === null ? null : String(value)); },
        setShield: (key, value) => {
            const panel = key === "bulletShield" ? bulletShieldValue : techShieldValue;
            panel.setText(String(value));
        },
        advanceVitalsFlush: () => {
            // pollVitals only flushes accumulated damage/healing every
            // VITALS_FLUSH_EVERY_POLLS polls in death_http_bridge.js.
            for (let i = 0; i < 3; i++) {
                runNextPoll();
            }
        },
        spawnDamageImpactInstance: ({
            name = "Enemy",
            assist = false,
            heal = false,
            shield = false,
            stunned = false,
            killed = false,
            barrierWidth = 40,
            healthWidth = 40,
        } = {}) => {
            const classes = ["team2", "damageImpactInstance", "fadeIn"];
            if (assist) {
                classes.push("assist");
            }
            if (stunned) {
                classes.push("Stunned", "ShowStatusEffect");
            }
            if (killed) {
                classes.push("killed");
            }
            if (heal) {
                // The game also stamps this ambiguous class on barrier-only
                // instances; the mod ignores it and reads the bars instead.
                classes.push("is_heal");
            }
            const children = [
                createPanel({
                    id: "KillAssistContainer",
                    children: [
                        createPanel({ classes: ["killLabel"], paneltype: "Label", text: "KILL" }),
                        createPanel({ classes: ["assistLabel"], paneltype: "Label", text: "KILL ASSIST" }),
                    ],
                }),
                // Both bars are always present in the instance; each rests at
                // width 0 and only grows when that effect actually landed.
                createPanel({
                    id: "healthGained",
                    properties: { actuallayoutwidth: heal ? healthWidth : 0 },
                }),
                createPanel({
                    id: "barrierGained",
                    properties: { actuallayoutwidth: shield ? barrierWidth : 0 },
                }),
            ];
            const instance = createPanel({ id: name, classes, children });
            damageImpactInstances = [...damageImpactInstances, instance];
            damageImpactInfo.setChildren(damageImpactInstances);
            return instance;
        },
        removeDamageImpactInstance: (instance) => {
            instance.setValid(false);
            damageImpactInstances = damageImpactInstances.filter((candidate) => candidate !== instance);
            damageImpactInfo.setChildren(damageImpactInstances);
        },
        // A floating combat-feedback indicator: `kind` is the category class
        // ("deny", "gold", "damage_type_gun", ...), text is the amount.
        spawnFeedbackIndicator: ({ kind = "damage_type_gun", amount = 100 } = {}) => {
            const label = createPanel({
                classes: [kind, "HudIndicatorText"],
                paneltype: "Label",
                text: String(amount),
            });
            feedbackDisplay.setChildren([...feedbackDisplay.Children(), label]);
            return label;
        },
        removeFeedbackIndicator: (label) => {
            label.setValid(false);
            feedbackDisplay.setChildren(
                feedbackDisplay.Children().filter((candidate) => candidate !== label),
            );
        },
        setFeedbackIndicatorText: (label, amount) => { label.setText(String(amount)); },
        // A floating event indicator: `kind` is the category class ("deny" for
        // a soul denied, "gold"/"gold_small" for a soul gained, ...). `under`
        // picks the host panel -- "events" (#HudEventIndicatorsPanel, default)
        // or "feedback" (#DamageFeedbackDisplay) -- since a deny can render in
        // either. `text` sets the HudIndicatorText amount.
        spawnEventIndicator: ({ kind = "deny", name = "SoulIndicator", under = "events", text = "" } = {}) => {
            const instance = createPanel({
                id: name,
                classes: [kind, "WindowRoot"],
                children: [
                    createPanel({ classes: ["HudIndicatorContainer"], children: [
                        createPanel({ classes: ["HudIndicatorText"], paneltype: "Label", text: String(text) }),
                    ] }),
                ],
            });
            if (under === "feedback") {
                feedbackIndicatorInstances = [...feedbackIndicatorInstances, instance];
                feedbackDisplay.setChildren(feedbackIndicatorInstances);
            } else {
                eventIndicatorInstances = [...eventIndicatorInstances, instance];
                eventIndicators.setChildren(eventIndicatorInstances);
            }
            return instance;
        },
        removeEventIndicator: (instance) => {
            instance.setValid(false);
            eventIndicatorInstances = eventIndicatorInstances.filter((candidate) => candidate !== instance);
            eventIndicators.setChildren(eventIndicatorInstances);
            feedbackIndicatorInstances = feedbackIndicatorInstances.filter((candidate) => candidate !== instance);
            feedbackDisplay.setChildren(feedbackIndicatorInstances);
        },
        advanceSoulDenyScan: () => advancePolls(3),
        setParryCooldown: (on) => { gunElement.setClasses(on ? ["ability_element_gun", "parry_on_cooldown"] : ["ability_element_gun"]); },
        setCrosshairStunned: (on) => {
            const base = gunElement.BHasClass("parry_on_cooldown")
                ? ["ability_element_gun", "parry_on_cooldown"]
                : ["ability_element_gun"];
            gunElement.setClasses(on ? [...base, "stunned"] : base);
        },
        setHealthBand: (band) => {
            healthBandWrapper.setClasses(
                band === 2 ? ["localPlayerLowHealth"] : band === 1 ? ["localPlayerMidHealth"] : [],
            );
        },
        advanceCombatScan: () => advancePolls(3),
        // pollObjectives only actually scans every OBJECTIVE_POLL_INTERVAL_POLLS
        // (5) polls; 6 guarantees at least one scan boundary from any phase.
        advanceObjectiveScan: () => advancePolls(6),
        setObjectiveEnemyTeam: (n) => {
            objectivesMap.setClasses(
                n === null
                    ? []
                    : ["Team" + n + "IsEnemy", "Team" + (n === 1 ? 2 : 1) + "IsFriend"],
            );
        },
        spawnObjective: (suffix, { team = 2, alive = true } = {}) => {
            const id = "Team" + team + suffix;
            let panel = objectiveMapChildren.find((c) => c.id === id);
            if (!panel) {
                panel = createPanel({
                    id,
                    classes: alive ? ["Alive", "Team" + team] : ["Team" + team],
                });
                objectiveMapChildren = [...objectiveMapChildren, panel];
                objectivesMap.setChildren(objectiveMapChildren);
            }
            return panel;
        },
        setObjectiveAlive: (suffix, alive, { team = 2 } = {}) => {
            const panel = objectiveMapChildren.find((c) => c.id === "Team" + team + suffix);
            if (panel) {
                panel.setClasses(alive ? ["Alive", "Team" + team] : ["Team" + team]);
            }
        },
        setObjectiveHealth: ({ type = null, dead = false, weakened = false, friend = false } = {}) => {
            const typeClass = {
                base_guardian: "is_barracks_boss",
                shrine: "is_shield_generator",
                titan: "is_titan",
                guardian: "is_tier1",
                walker: "is_tier2",
            }[type];
            const classes = ["objective_health"];
            if (typeClass) classes.push(typeClass);
            if (dead) classes.push("is_dead");
            if (weakened) classes.push("is_weakened");
            if (friend) classes.push("friend");
            objectiveHealth.setClasses(classes);
        },
        addBossKilledFeedRow: ({
            killer = "friend",
            midBoss = false,
            typeClass = null,
            victimImage = null,
            victimText = null,
        } = {}) => {
            const killerContainer = createPanel({
                classes: killer === "friend" ? ["killerContainer", "killerFriend"]
                    : killer === "enemy" ? ["killerContainer", "killerEnemy"]
                    : killer === "team1" ? ["killerContainer", "killerTeam1"]
                    : killer === "team2" ? ["killerContainer", "killerTeam2"]
                    : ["killerContainer"],
            });
            const victimKids = [];
            if (victimImage !== null) {
                victimKids.push(createPanel({ classes: ["entityImage"], properties: { src: victimImage } }));
            }
            if (victimText !== null) {
                victimKids.push(createPanel({ classes: ["personaName"], paneltype: "Label", text: victimText }));
            }
            if (typeClass) {
                victimKids.push(createPanel({ classes: [typeClass] }));
            }
            const victimContainer = createPanel({ classes: ["victimContainer"], children: victimKids });
            const row = createPanel({
                paneltype: "HudBossKilled",
                classes: midBoss ? ["midBoss"] : [],
                children: [killerContainer, victimContainer],
            });
            objectiveFeedRowPanels = [...objectiveFeedRowPanels, row];
            objectivesFeed.setChildren(objectiveFeedRowPanels);
            return row;
        },
        clearObjectiveFeed: () => {
            objectiveFeedRowPanels = [];
            objectivesFeed.setChildren([]);
        },
        setMatchEnd: ({ shown = false, localTeam = null, victoryTeam = null, abandoned = false } = {}) => {
            const classes = [];
            if (shown) classes.push("ShowMatchEnd");
            if (localTeam) classes.push("LocalPlayerTeam" + localTeam);
            if (victoryTeam) classes.push("Team" + victoryTeam + "Victory");
            if (abandoned) classes.push("MatchAbandoned");
            matchEnd.setClasses(classes);
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

    test("emits soul_deny when a deny indicator appears", () => {
        const harness = createHarness();
        harness.spawnEventIndicator({ kind: "deny" });
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toEqual([
            expect.objectContaining({ detection: "feedback_indicator_class:deny" }),
        ]);
    });

    test("does not emit soul_deny for a gold (soul gained) indicator", () => {
        const harness = createHarness();
        harness.spawnEventIndicator({ kind: "gold" });
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toHaveLength(0);
    });

    test("does not double-credit the same deny indicator across scans", () => {
        const harness = createHarness();
        harness.spawnEventIndicator({ kind: "deny" });
        harness.advanceSoulDenyScan();
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toHaveLength(1);
    });

    test("credits a second deny once the first indicator is torn down", () => {
        const harness = createHarness();
        const first = harness.spawnEventIndicator({ kind: "deny", name: "Deny1" });
        harness.advanceSoulDenyScan();
        harness.removeEventIndicator(first);
        harness.spawnEventIndicator({ kind: "deny", name: "Deny2" });
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toHaveLength(2);
    });

    test("ignores a deny indicator credited on the spectated hero's HUD", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHeroIdentity("hero_b");
        harness.advanceSoulDenyScan();
        harness.spawnEventIndicator({ kind: "deny" });
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toHaveLength(0);
    });

    test("emits soul_deny for a deny indicator hosted by #DamageFeedbackDisplay", () => {
        const harness = createHarness();
        harness.spawnEventIndicator({ kind: "deny", under: "feedback", text: "142" });
        harness.advanceSoulDenyScan();

        expect(harness.events("soul_deny")).toEqual([
            expect.objectContaining({ detection: "feedback_indicator_class:deny" }),
        ]);
    });

    test("every new soul indicator emits a sequence-less soul_indicator_diag with its shape", () => {
        const harness = createHarness();
        harness.spawnEventIndicator({ kind: "deny", text: "88" });
        harness.advanceSoulDenyScan();
        harness.spawnEventIndicator({ kind: "gold_small", name: "GS1", under: "feedback", text: "+25" });
        harness.advanceSoulDenyScan();

        const diag = harness.events("soul_indicator_diag");
        const denyDiag = diag.find((entry) => entry.match === "deny");
        expect(denyDiag).toBeDefined();
        expect(denyDiag.classes).toContain("deny");
        expect(denyDiag.text).toBe("88");
        expect(denyDiag.where).toBe("HudEventIndicatorsPanel");
        expect(denyDiag).not.toHaveProperty("sequence");

        const goldSmall = diag.find((entry) => entry.match === "gold_small");
        expect(goldSmall).toBeDefined();
        expect(goldSmall.text).toBe("+25");
        expect(goldSmall.where).toBe("DamageFeedbackDisplay");
    });

    test("the soul_indicator_diag shape dump is capped so a full match cannot flood the log", () => {
        const harness = createHarness();
        for (let i = 0; i < 70; i++) {
            harness.spawnEventIndicator({ kind: "gold", name: `Gold${i}`, text: String(i) });
            harness.advanceSoulDenyScan();
        }

        const diag = harness.events("soul_indicator_diag");
        expect(diag).toHaveLength(60); // soulDiagBudget
        expect(diag[0].match).toBe("gold");
        expect(diag[0].classes).toContain("gold");
        expect(harness.events("soul_deny")).toHaveLength(0);
    });

    test("emits ally_healed when an impact instance carries a heal marker", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze", heal: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_healed")).toEqual([
            expect.objectContaining({ detection: "impact_bar:healthGained" }),
        ]);
        expect(harness.events("ally_shielded")).toHaveLength(0);
    });

    test("a barrier-only instance is a shield, never a heal", () => {
        const harness = createHarness();
        // shield bar grows, health bar does not, but the .is_heal class is set.
        harness.spawnDamageImpactInstance({ name: "Haze", heal: true, shield: true, healthWidth: 0 });
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_shielded")).toHaveLength(1);
        expect(harness.events("ally_healed")).toHaveLength(0);
    });

    test("does not emit ally_healed for a plain (damage-only) impact instance", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze" });
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_healed")).toHaveLength(0);
    });

    test("does not double-credit the same ally_healed instance across scans", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze", heal: true });
        harness.advanceDamageImpactScan();
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_healed")).toHaveLength(1);
    });

    test("emits ally_shielded when an instance's barrier bar has grown", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze", shield: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_shielded")).toEqual([
            expect.objectContaining({ detection: "impact_bar:barrierGained" }),
        ]);
        expect(harness.events("ally_healed")).toHaveLength(0);
    });

    test("does not emit ally_shielded while the barrier bar is at its resting width", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze" }); // barrierGained width 0
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_shielded")).toHaveLength(0);
    });

    test("one instance that both heals and shields fires each once", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Haze", heal: true, shield: true });
        harness.advanceDamageImpactScan();
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_healed")).toHaveLength(1);
        expect(harness.events("ally_shielded")).toHaveLength(1);
    });

    test("ignores ally support credited on the spectated hero's HUD", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHeroIdentity("hero_b");
        harness.advanceDamageImpactScan();
        harness.spawnDamageImpactInstance({ name: "Haze", heal: true, shield: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("ally_healed")).toHaveLength(0);
        expect(harness.events("ally_shielded")).toHaveLength(0);
    });

    test("does not treat a heal or gold indicator as damage given", () => {
        const harness = createHarness();
        harness.spawnFeedbackIndicator({ kind: "heal", amount: 200 });
        harness.spawnFeedbackIndicator({ kind: "gold", amount: 90 });
        harness.advanceCombatScan();

        expect(harness.events("damage_given")).toHaveLength(0);
        expect(harness.events("soul_secure")).toHaveLength(0);
    });

    test("sums damage-number indicators into a damage_given amount", () => {
        const harness = createHarness();
        harness.spawnFeedbackIndicator({ kind: "damage_type_gun", amount: 100 });
        harness.spawnFeedbackIndicator({ kind: "damage_type_ability", amount: 175 });
        harness.advanceCombatScan();

        const given = harness.events("damage_given");
        expect(given).toHaveLength(1);
        expect(given[0].amount).toBe(275);
        expect(given[0].detection).toBe("feedback_damage_numbers");
    });

    test("only the rise of a batched damage number is added", () => {
        const harness = createHarness();
        const label = harness.spawnFeedbackIndicator({ kind: "damage_type_gun", amount: 50 });
        harness.advanceCombatScan();
        harness.setFeedbackIndicatorText(label, 130); // grew by 80
        harness.advancePolls(20); // clear the 250ms flush gate
        harness.advanceCombatScan();

        const given = harness.events("damage_given");
        expect(given.reduce((sum, e) => sum + e.amount, 0)).toBe(130);
    });

    test("a parry that hits nothing emits neither parry_success nor parry_fail", () => {
        const harness = createHarness();
        harness.setParryCooldown(true);
        harness.runNextPoll();
        harness.advancePolls(70); // > PARRY_RESOLVE_MS at 16ms/poll

        expect(harness.events("parry_success")).toHaveLength(0);
        expect(harness.events("parry_fail")).toHaveLength(0);
    });

    test("an enemy stunned within the parry window resolves as parry_success", () => {
        const harness = createHarness();
        harness.setParryCooldown(true);
        harness.runNextPoll();
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true });
        harness.advancePolls(70);

        expect(harness.events("parry_success")).toEqual([
            expect.objectContaining({ detection: "enemy_stunned_after_parry" }),
        ]);
        expect(harness.events("parry_fail")).toHaveLength(0);
    });

    test("a killed enemy's stun class does not count as a parry connect", () => {
        const harness = createHarness();
        harness.setParryCooldown(true);
        harness.runNextPoll();
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true, killed: true });
        harness.advancePolls(70);

        expect(harness.events("parry_success")).toHaveLength(0);
        expect(harness.events("parry_fail")).toHaveLength(0);
    });

    test("your own stun during the parry window resolves as parry_fail, outranking an enemy stun", () => {
        const harness = createHarness();
        harness.setParryCooldown(true);
        harness.runNextPoll();
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true });
        harness.setCrosshairStunned(true);
        harness.advancePolls(70);

        expect(harness.events("parry_fail")).toEqual([
            expect.objectContaining({ detection: "stunned_after_parry" }),
        ]);
        expect(harness.events("parry_success")).toHaveLength(0);
    });

    test("a stun that lands after the parry cooldown clears still resolves as parry_fail", () => {
        const harness = createHarness();
        harness.setParryCooldown(true);
        harness.runNextPoll();
        // Cooldown indicator drops, then the stun state shows a poll later.
        harness.setParryCooldown(false);
        harness.runNextPoll();
        harness.setCrosshairStunned(true);
        harness.advancePolls(70);

        expect(harness.events("parry_fail")).toHaveLength(1);
        expect(harness.events("parry_success")).toHaveLength(0);
    });

    test("an enemy already stunned outside any parry window is not a parry_success", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true });
        harness.advancePolls(20);

        expect(harness.events("parry_success")).toHaveLength(0);
    });

    test("an enemy stunned before the parry starts (still fading) is not a parry connect", () => {
        const harness = createHarness();
        // An ability stun landed a moment before the parry; its damage-impact
        // instance is still on screen when the parry is thrown.
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true });
        harness.advancePolls(2);
        harness.setParryCooldown(true);
        harness.runNextPoll(); // window opens with that instance already stunned
        harness.advancePolls(70);

        expect(harness.events("parry_success")).toHaveLength(0);
        expect(harness.events("parry_fail")).toHaveLength(0);
    });

    test("a fresh stun on a new instance during the window still resolves as parry_success", () => {
        const harness = createHarness();
        harness.spawnDamageImpactInstance({ name: "Abrams", stunned: true });
        harness.advancePolls(2);
        harness.setParryCooldown(true);
        harness.runNextPoll();
        // A second enemy is stunned by the parry after the window opens.
        harness.spawnDamageImpactInstance({ name: "Lash", stunned: true });
        harness.advancePolls(70);

        expect(harness.events("parry_success")).toEqual([
            expect.objectContaining({ detection: "enemy_stunned_after_parry" }),
        ]);
    });

    test("emits objective_guardian when an enemy Tier1 panel loses its Alive class", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.spawnObjective("Tier1_1", { alive: true });
        harness.advanceObjectiveScan(); // baseline
        harness.setObjectiveAlive("Tier1_1", false);
        harness.advanceObjectiveScan();

        expect(harness.events("objective_guardian")).toEqual([
            expect.objectContaining({ detection: "objectives_map:tier1_alive_cleared" }),
        ]);
        expect(harness.events("objective_walker")).toHaveLength(0);
    });

    test("emits objective_walker for an enemy Tier2 panel, once, not on every scan", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(1);
        harness.spawnObjective("Tier2_3", { team: 1, alive: true });
        harness.advanceObjectiveScan();
        harness.setObjectiveAlive("Tier2_3", false, { team: 1 });
        harness.advanceObjectiveScan();
        harness.advanceObjectiveScan();

        expect(harness.events("objective_walker")).toHaveLength(1);
    });

    test("does not credit an objective already dead when the baseline is taken", () => {
        const harness = createHarness();
        harness.spawnObjective("Tier1_1", { alive: true });
        harness.advanceObjectiveScan(); // enemy team unknown -> no baseline yet
        harness.setObjectiveEnemyTeam(2);
        harness.setObjectiveAlive("Tier1_1", false);
        harness.advanceObjectiveScan(); // first real baseline: records it dead
        harness.advanceObjectiveScan();

        expect(harness.events("objective_guardian")).toHaveLength(0);
    });

    test("ignores a friendly structure being destroyed", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.spawnObjective("Tier1_1", { team: 1, alive: true }); // friendly side
        harness.advanceObjectiveScan();
        harness.setObjectiveAlive("Tier1_1", false, { team: 1 });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_guardian")).toHaveLength(0);
    });

    test("emits game_won when the enemy Core panel loses Alive", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.spawnObjective("Core", { alive: true });
        harness.advanceObjectiveScan();
        harness.setObjectiveAlive("Core", false);
        harness.advanceObjectiveScan();
        harness.advanceObjectiveScan();

        expect(harness.events("game_won")).toHaveLength(1);
    });

    test("emits game_won from the match-end screen on a local-team victory", () => {
        const harness = createHarness();
        harness.setMatchEnd({ shown: true, localTeam: 2, victoryTeam: 2 });
        harness.advanceObjectiveScan();

        expect(harness.events("game_won")).toEqual([
            expect.objectContaining({ detection: "match_end:local_team_victory" }),
        ]);
    });

    test("does not emit game_won when the other team wins", () => {
        const harness = createHarness();
        harness.setMatchEnd({ shown: true, localTeam: 1, victoryTeam: 2 });
        harness.advanceObjectiveScan();

        expect(harness.events("game_won")).toHaveLength(0);
    });

    test("emits objective_shrine when the boss bar shows a shrine as dead", () => {
        const harness = createHarness();
        harness.setObjectiveHealth({ type: "shrine" });
        harness.advanceObjectiveScan();
        harness.setObjectiveHealth({ type: "shrine", dead: true });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_shrine")).toEqual([
            expect.objectContaining({ detection: "objective_health:is_shield_generator+is_dead" }),
        ]);
    });

    test("emits objective_base_guardian for a dead barracks boss, but not a friendly one", () => {
        const friendly = createHarness();
        friendly.setObjectiveHealth({ type: "base_guardian", dead: true, friend: true });
        friendly.advanceObjectiveScan();
        expect(friendly.events("objective_base_guardian")).toHaveLength(0);

        const enemy = createHarness();
        enemy.setObjectiveHealth({ type: "base_guardian" });
        enemy.advanceObjectiveScan();
        enemy.setObjectiveHealth({ type: "base_guardian", dead: true });
        enemy.advanceObjectiveScan();
        expect(enemy.events("objective_base_guardian")).toHaveLength(1);
    });

    test("emits objective_patron_weakened when the Patron bar gains is_weakened", () => {
        const harness = createHarness();
        harness.setObjectiveHealth({ type: "titan" });
        harness.advanceObjectiveScan();
        harness.setObjectiveHealth({ type: "titan", weakened: true });
        harness.advanceObjectiveScan();
        harness.advanceObjectiveScan();

        expect(harness.events("objective_patron_weakened")).toHaveLength(1);
    });

    test("emits objective_base_guardian from a friendly-credited boss-killed feed row", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan(); // baseline
        harness.addBossKilledFeedRow({ killer: "friend", victimText: "Base Guardian" });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_base_guardian")).toEqual([
            expect.objectContaining({ detection: "objectives_feed:boss_killed" }),
        ]);
    });

    test("classifies a shrine feed row from the victim image when it has no text", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan();
        harness.addBossKilledFeedRow({
            killer: "team1", // friendly side derived from the objectives map
            victimImage: "s2r://panorama/images/hud/shield_generator_psd.vtex",
        });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_shrine")).toHaveLength(1);
    });

    test("the feed ignores the mid boss and enemy-credited structure kills", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan();
        harness.addBossKilledFeedRow({ killer: "friend", midBoss: true, victimText: "Base Guardian" });
        harness.addBossKilledFeedRow({ killer: "enemy", victimText: "Base Guardian" });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_base_guardian")).toHaveLength(0);
    });

    test("the feed does not double-fire an objective the centre bar already reported", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan();
        harness.setObjectiveHealth({ type: "shrine" });
        harness.advanceObjectiveScan();
        harness.setObjectiveHealth({ type: "shrine", dead: true });
        harness.advanceObjectiveScan(); // centre bar emits objective_shrine
        harness.addBossKilledFeedRow({ killer: "friend", victimText: "Shrine" });
        harness.advanceObjectiveScan(); // feed would emit, but it is within the dedup window

        expect(harness.events("objective_shrine")).toHaveLength(1);
    });

    test("guardian and walker feed rows are left to the objectives-map diff", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan();
        harness.addBossKilledFeedRow({ killer: "friend", victimText: "Walker" });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_walker")).toHaveLength(0);
        expect(harness.events("objective_guardian")).toHaveLength(0);
    });

    test("an untypeable boss-killed row emits only a sequence-less diagnostic", () => {
        const harness = createHarness();
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan();
        harness.addBossKilledFeedRow({ killer: "friend", victimText: "mystery structure" });
        harness.advanceObjectiveScan();

        expect(harness.events("objective_base_guardian")).toHaveLength(0);
        expect(harness.events("objective_shrine")).toHaveLength(0);
        const diagnostics = harness.events("objective_feed_unclassified");
        expect(diagnostics).toHaveLength(1);
        expect(diagnostics[0].text_hint).toBe("mystery structure");
        expect(diagnostics[0]).not.toHaveProperty("sequence");
    });

    test("does not credit a feed row that was already on screen at baseline", () => {
        const harness = createHarness();
        harness.addBossKilledFeedRow({ killer: "friend", victimText: "Base Guardian" });
        harness.setObjectiveEnemyTeam(2);
        harness.advanceObjectiveScan(); // first baseline pass sees the row, does not credit it
        harness.advanceObjectiveScan();

        expect(harness.events("objective_base_guardian")).toHaveLength(0);
    });

    test("damage_taken_intensity fires when the health band worsens, not when it recovers", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHealthBand(1); // mid
        harness.advanceCombatScan();
        expect(harness.events("damage_taken_intensity")).toEqual([
            expect.objectContaining({ detection: "health_band:1" }),
        ]);

        harness.setHealthBand(2); // low
        harness.advanceCombatScan();
        expect(harness.events("damage_taken_intensity")).toHaveLength(2);

        harness.setHealthBand(0); // recovered
        harness.advanceCombatScan();
        expect(harness.events("damage_taken_intensity")).toHaveLength(2);
    });

    test("emits local_player_respawn without a sequence when the dead class clears", () => {
        const harness = createHarness();
        harness.settle();
        harness.setDead(true);
        harness.runNextPoll();
        harness.setDead(false);
        harness.runNextPoll();

        expect(harness.events("local_player_respawn")).toEqual([
            expect.objectContaining({
                event: "local_player_respawn",
                mod_version: "0.1.0",
                session_id: expect.any(String),
            }),
        ]);
        expect(harness.events("local_player_respawn")[0]).not.toHaveProperty("sequence");
    });

    test("does not emit respawn while the baseline is still settling", () => {
        const harness = createHarness({ initiallyDead: true });
        harness.runNextPoll();
        harness.setDead(false);
        harness.runNextPoll();

        expect(harness.events("local_player_respawn")).toHaveLength(0);
    });

    test("ignores spectated-hero ability signals while the hero identity holds steady", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHeroIdentity("hero_b");
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0"] });
        harness.runNextPoll();

        expect(harness.events("ability_used")).toHaveLength(0);
        expect(harness.events("ability_cooldown_ready")).toHaveLength(0);
    });

    test("ignores assists credited on the spectated hero's HUD", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHeroIdentity("hero_b");
        harness.advanceDamageImpactScan();
        harness.spawnDamageImpactInstance({ name: "Abrams", assist: true });
        harness.advanceDamageImpactScan();

        expect(harness.events("local_player_assist")).toHaveLength(0);
    });

    test("resumes ability triggers once back on the local player's hero", () => {
        const harness = createHarness();
        harness.settle();
        harness.setHeroIdentity("hero_b");
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setHeroIdentity("hero_a");
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();

        expect(harness.events("ability_used")).toEqual([
            expect.objectContaining({ detection: "cooldown_started" }),
        ]);
    });

    test("re-learns the local hero after a between-matches panel swap", () => {
        const harness = createHarness();
        harness.settle();
        // Spectating a teammate in match one: their signals are ignored.
        harness.setHeroIdentity("hero_b");
        harness.runNextPoll();
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();
        expect(harness.events("ability_used")).toHaveLength(0);

        // Match two: fresh player panel, a different hero now under our control.
        harness.replacePlayer();
        harness.setHeroIdentity("hero_c");
        harness.settle();
        harness.setAbilityState({ classes: ["trained", "Tier0"] });
        harness.runNextPoll();
        harness.setAbilityState({ classes: ["trained", "Tier0", "cooling_down"] });
        harness.runNextPoll();

        expect(harness.events("ability_used")).toEqual([
            expect.objectContaining({ detection: "cooldown_started" }),
        ]);
    });

    test("stops scheduling after its Panorama context is destroyed", () => {
        const harness = createHarness();
        harness.invalidateContext();
        harness.runNextPoll();
        expect(harness.scheduled).toHaveLength(0);
    });

    test("a health drop is flushed as damage_taken once the vitals baseline settles", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll(); // establish the vitals baseline at 600
        harness.setHealth(450);
        harness.advanceVitalsFlush();

        expect(harness.events("damage_taken")).toEqual([
            expect.objectContaining({ amount: 150, health: 450 }),
        ]);
    });

    test("a health rise is flushed as healing_received", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll();
        harness.setHealth(600);
        harness.setHealth(720);
        harness.advanceVitalsFlush();

        expect(harness.events("healing_received")).toEqual([
            expect.objectContaining({ amount: 120, health: 720 }),
        ]);
        expect(harness.events("damage_taken")).toHaveLength(0);
    });

    test("shield loss counts toward damage taken even when health itself is unchanged", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll();
        harness.setShield("bulletShield", 100);
        harness.runNextPoll();
        harness.setShield("bulletShield", 40);
        harness.advanceVitalsFlush();

        expect(harness.events("damage_taken")).toEqual([
            expect.objectContaining({ amount: 60 }),
        ]);
    });

    test("damage and healing within the same flush window are reported separately", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll();
        harness.setHealth(500);
        harness.runNextPoll();
        harness.setHealth(560);
        harness.advanceVitalsFlush();

        const damage = harness.events("damage_taken");
        const healing = harness.events("healing_received");
        expect(damage).toEqual([expect.objectContaining({ amount: 100 })]);
        expect(healing).toEqual([expect.objectContaining({ amount: 60 })]);
    });

    test("a respawn's full-health restore does not report as healing", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll();
        harness.setHealth(50);
        harness.setDead(true);
        harness.runNextPoll();
        // Respawning snaps health back to full; that is a rebaseline, not
        // healing the player earned.
        harness.setHealth(600);
        harness.setDead(false);
        harness.runNextPoll();
        harness.advanceVitalsFlush();

        expect(harness.events("healing_received")).toHaveLength(0);
        expect(harness.events("damage_taken")).toHaveLength(0);
    });

    test("does not flush a zero-amount direction alongside a real one", () => {
        const harness = createHarness();
        harness.settle();
        harness.runNextPoll();
        harness.setHealth(450);
        harness.advanceVitalsFlush();

        const events = harness.events().filter(
            (event) => event.event === "damage_taken" || event.event === "healing_received",
        );
        expect(events).toHaveLength(1);
    });
});
