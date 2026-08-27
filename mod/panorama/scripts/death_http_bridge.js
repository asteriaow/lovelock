(function () {
    "use strict";

    var LOG_PREFIX = "[DEADLOCK_DEATH_HOOK]";
    var MOD_VERSION = "0.1.0";
    var POLL_INTERVAL_SECONDS = 0.1;
    var context = $.GetContextPanel();
    var localPlayerPanel = null;
    var deathBaselineEstablished = false;
    var wasDead = false;
    var sequence = 0;
    var abilityRoot = null;
    var abilityHeroIdentity = null;
    var abilityPanels = [];
    var abilityStates = [];
    var lastAbilityCatalogSignature = null;
    var sessionId = Date.now().toString(36) + "-" + Math.floor(Math.random() * 0x1000000).toString(36);
    // Loading/hero-select -> match transitions recreate the top bar player
    // panel, which resets the death baseline. The "not yet spawned" state
    // during hero-select/loading reads as Dead for the duration of that
    // phase (observed ~14s in practice), not just a single frame, so the
    // settle window has to cover the whole loading phase, not just a
    // UI-refresh blip. A real death this early into a fresh baseline is
    // very unlikely, since baselines (re)establish at match start.
    var BASELINE_SETTLE_POLLS = 250; // ~25s at POLL_INTERVAL_SECONDS = 0.1
    var baselineSettlePollsRemaining = 0;
    // The kill-streak popup (PlayerDetailsContainer > KillStreakContainer >
    // KillStreakText, inside the local player's own CitadelHudTopBarPlayer
    // panel) shows a running kill count and pops on every hero kill, isolated
    // ones included -- unlike the Tab-gated personal KDA text. Its first child
    // Label holds the number; sibling panels hold the "Kill Streak!" /
    // "First Blood!" banners. The popup animates out after the streak's
    // timeout, so a null reading means "not currently shown". Require a few
    // consecutive nulls before dropping the baseline so a one-frame render
    // gap mid-animation doesn't make the next count look like a new streak.
    var lastKillStreakCount = null;
    var killStreakNullPolls = 0;
    var KILL_STREAK_RESET_POLLS = 3; // ~0.3s at POLL_INTERVAL_SECONDS = 0.1

    function emit(eventName, fields) {
        var payload = {
            schema: 1,
            event: eventName,
            mod_version: MOD_VERSION,
            session_id: sessionId,
            client_time_ms: Date.now()
        };

        if (fields) {
            for (var key in fields) {
                if (Object.prototype.hasOwnProperty.call(fields, key)) {
                    payload[key] = fields[key];
                }
            }
        }

        $.Msg(LOG_PREFIX + JSON.stringify(payload));
    }

    function emitAction(eventName, fields) {
        sequence++;
        fields.sequence = sequence;
        emit(eventName, fields);
    }

    function isValidPanel(panel) {
        try {
            return !!(panel && panel.IsValid && panel.IsValid());
        } catch (_error) {
            return false;
        }
    }

    function panelHasClass(panel, className) {
        try {
            return !!(isValidPanel(panel) && panel.BHasClass && panel.BHasClass(className));
        } catch (_error) {
            return false;
        }
    }

    function panelProperty(panel, propertyName) {
        try {
            var value = panel[propertyName];
            if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
                return value;
            }
        } catch (_error) {
        }
        return null;
    }

    function panelAttribute(panel, attributeName) {
        if (!isValidPanel(panel) || !panel.GetAttributeString) {
            return null;
        }
        var missing = "__deadlockshock_missing__";
        try {
            var value = panel.GetAttributeString(attributeName, missing);
            return value !== missing && value !== "" ? value : null;
        } catch (_error) {
            return null;
        }
    }

    function panelChildren(panel) {
        try {
            if (panel.Children) {
                return panel.Children();
            }
            if (panel.GetChildCount && panel.GetChild) {
                var children = [];
                for (var i = 0; i < panel.GetChildCount(); i++) {
                    children.push(panel.GetChild(i));
                }
                return children;
            }
        } catch (_error) {
        }
        return [];
    }

    function findChildById(panel, id) {
        if (!isValidPanel(panel)) {
            return null;
        }
        try {
            if (panel.id === id) {
                return panel;
            }
            if (panel.FindChildTraverse) {
                return panel.FindChildTraverse(id);
            }
        } catch (_error) {
        }
        return null;
    }

    function findChildrenWithClass(panel, className) {
        if (!isValidPanel(panel)) {
            return [];
        }
        try {
            if (panel.FindChildrenWithClassTraverse) {
                return panel.FindChildrenWithClassTraverse(className);
            }
        } catch (_error) {
        }
        return [];
    }

    function firstPanelWithType(panel, panelType) {
        if (!isValidPanel(panel)) {
            return null;
        }
        if (panelProperty(panel, "paneltype") === panelType) {
            return panel;
        }
        var children = panelChildren(panel);
        for (var i = 0; i < children.length; i++) {
            var match = firstPanelWithType(children[i], panelType);
            if (match) {
                return match;
            }
        }
        return null;
    }

    function textFromClass(panel, className) {
        var matches = findChildrenWithClass(panel, className);
        return matches.length > 0 ? panelProperty(matches[0], "text") : null;
    }

    function highestContextAncestor() {
        var panel = context;
        for (var i = 0; i < 64 && isValidPanel(panel); i++) {
            var parent = null;
            try {
                parent = panel.GetParent ? panel.GetParent() : null;
            } catch (_error) {
                parent = null;
            }
            if (!isValidPanel(parent)) {
                break;
            }
            panel = parent;
        }
        return panel;
    }

    function findAbilityRoot() {
        return findChildById(highestContextAncestor(), "hud_signature");
    }

    function abilityEntries(root) {
        return findChildrenWithClass(root, "ability_container");
    }

    function stableIdentity(panel, attributeNames) {
        for (var i = 0; i < attributeNames.length; i++) {
            var value = panelAttribute(panel, attributeNames[i]);
            if (value !== null) {
                return attributeNames[i] + ":" + value;
            }
        }
        return null;
    }

    function heroIdentity(root) {
        return stableIdentity(root, ["hero_id", "hero_name", "heroname", "unit_name", "entity_index"]);
    }

    function abilityIdentity(panel) {
        return stableIdentity(panel, ["ability_id", "ability", "ability_name", "entity_index"]);
    }

    function abilityTier(panel) {
        for (var tier = 0; tier <= 3; tier++) {
            if (panelHasClass(panel, "Tier" + tier)) {
                return tier;
            }
        }
        return null;
    }

    function integerPanelProperty(panel, propertyName) {
        var rawValue = panelProperty(panel, propertyName);
        if (rawValue === null) {
            return null;
        }
        var value = Number(rawValue);
        if (!isFinite(value) || Math.floor(value) !== value || value < 0) {
            return null;
        }
        return value;
    }

    function abilitySnapshot(panel, slotIndex) {
        if (!isValidPanel(panel)) {
            return null;
        }

        var charged = panelHasClass(panel, "has_stack_charges");
        var charges = null;
        var maxCharges = null;
        if (charged) {
            var chargeContainers = findChildrenWithClass(panel, "stack_charges");
            if (chargeContainers.length === 0) {
                return null;
            }
            var progress = null;
            for (var chargeIndex = 0; chargeIndex < chargeContainers.length && !progress; chargeIndex++) {
                progress = firstPanelWithType(chargeContainers[chargeIndex], "ProgressBarWithMiddle");
            }
            if (!progress) {
                return null;
            }
            charges = integerPanelProperty(progress, "lowervalue");
            maxCharges = integerPanelProperty(progress, "max");
            if (charges === null || maxCharges === null || charges > maxCharges) {
                return null;
            }
        }

        return {
            slot: slotIndex + 1,
            identity: abilityIdentity(panel),
            name: textFromClass(panel, "ability_name"),
            tier: abilityTier(panel),
            charged: charged,
            charges: charges,
            max_charges: maxCharges,
            cooling_down: panelHasClass(panel, "cooling_down"),
            active: panelHasClass(panel, "active")
        };
    }

    function resetAbilityState() {
        abilityRoot = null;
        abilityHeroIdentity = null;
        abilityPanels = [];
        abilityStates = [];
    }

    function baselineAbilityPanels(root, panels) {
        abilityRoot = root;
        abilityHeroIdentity = heroIdentity(root);
        abilityPanels = panels.slice(0);
        abilityStates = [];
        for (var i = 0; i < panels.length; i++) {
            abilityStates.push(abilitySnapshot(panels[i], i));
        }
    }

    function emitAbilityCatalog(forceRefresh) {
        var abilities = [];
        for (var i = 0; i < abilityStates.length; i++) {
            var snapshot = abilityStates[i];
            if (!snapshot) {
                continue;
            }
            var ability = {
                ability_slot: snapshot.slot
            };
            if (typeof snapshot.name === "string" && snapshot.name !== "") {
                ability.ability_name = snapshot.name;
            }
            abilities.push(ability);
        }
        if (abilities.length === 0) {
            return;
        }
        var signature = JSON.stringify(abilities);
        if (!forceRefresh && signature === lastAbilityCatalogSignature) {
            return;
        }
        lastAbilityCatalogSignature = signature;
        emit("ability_catalog", {
            abilities: abilities
        });
    }

    function samePanelSet(panels) {
        if (panels.length !== abilityPanels.length) {
            return false;
        }
        for (var i = 0; i < panels.length; i++) {
            if (panels[i] !== abilityPanels[i] || !isValidPanel(panels[i])) {
                return false;
            }
        }
        return true;
    }

    function eventFields(snapshot, detection, chargesBefore, chargesAfter) {
        var fields = {
            ability_slot: snapshot.slot,
            detection: detection
        };
        if (typeof snapshot.name === "string" && snapshot.name !== "") {
            fields.ability_name = snapshot.name;
        }
        if (chargesBefore !== null && chargesBefore !== undefined) {
            fields.charges_before = chargesBefore;
        }
        if (chargesAfter !== null && chargesAfter !== undefined) {
            fields.charges_after = chargesAfter;
        }
        return fields;
    }

    function detectAbilityTransitions(previous, current) {
        var chargeDecreased = current.charged && current.charges < previous.charges;
        var chargeIncreased = current.charged && current.charges > previous.charges;
        var cooldownStarted = !previous.cooling_down && current.cooling_down;
        var activated = !previous.active && current.active;
        var cooldownFinished = previous.cooling_down && !current.cooling_down;

        if (current.charged) {
            if (chargeDecreased) {
                emitAction("ability_used", eventFields(
                    current,
                    "charge_decrement",
                    previous.charges,
                    current.charges
                ));
            }
        } else if (cooldownStarted || activated) {
            var useDetection = cooldownStarted && activated
                ? "cooldown_started_and_activated"
                : (cooldownStarted ? "cooldown_started" : "activated");
            emitAction("ability_used", eventFields(current, useDetection, null, null));
        }

        if (cooldownFinished || chargeIncreased) {
            var readyDetection = cooldownFinished && chargeIncreased
                ? "cooldown_finished_and_charge_restored"
                : (cooldownFinished ? "cooldown_finished" : "charge_restored");
            emitAction("ability_cooldown_ready", eventFields(
                current,
                readyDetection,
                chargeIncreased ? previous.charges : null,
                chargeIncreased ? current.charges : null
            ));
        }
    }

    function pollAbilities(forceBaseline) {
        var root = findAbilityRoot();
        if (!isValidPanel(root)) {
            resetAbilityState();
            return;
        }

        var panels = abilityEntries(root);
        var currentHeroIdentity = heroIdentity(root);
        var rootReplaced = abilityRoot !== root;
        var panelsReplaced = !samePanelSet(panels);
        var heroReplaced = currentHeroIdentity !== abilityHeroIdentity;
        if (forceBaseline || rootReplaced || panelsReplaced || heroReplaced) {
            baselineAbilityPanels(root, panels);
            emitAbilityCatalog(rootReplaced || panelsReplaced || heroReplaced);
            return;
        }

        var catalogueChanged = false;
        var catalogueReplaced = false;
        for (var slotIndex = 0; slotIndex < panels.length; slotIndex++) {
            var current = abilitySnapshot(panels[slotIndex], slotIndex);
            var previous = abilityStates[slotIndex];
            if (!current || !previous) {
                catalogueChanged = catalogueChanged || (!!current !== !!previous);
                abilityStates[slotIndex] = current;
                continue;
            }

            if (current.identity !== previous.identity
                || (current.identity === null && current.name !== previous.name)
                || current.tier !== previous.tier
                || current.charged !== previous.charged
                || current.max_charges !== previous.max_charges) {
                catalogueReplaced = catalogueReplaced || current.identity !== previous.identity;
                catalogueChanged = catalogueChanged || current.name !== previous.name;
                abilityStates[slotIndex] = current;
                continue;
            }

            catalogueChanged = catalogueChanged || current.name !== previous.name;
            detectAbilityTransitions(previous, current);
            abilityStates[slotIndex] = current;
        }
        if (catalogueChanged || catalogueReplaced) {
            emitAbilityCatalog(catalogueReplaced);
        }
    }

    function killStreakCount(player) {
        var textNode = findChildById(player, "KillStreakText");
        if (!isValidPanel(textNode)) {
            return null;
        }
        // The count is the first direct Label child; the sibling panel below
        // it holds the "Kill Streak!" / "First Blood!" banner labels.
        var children = panelChildren(textNode);
        for (var i = 0; i < children.length; i++) {
            if (panelProperty(children[i], "paneltype") !== "Label") {
                continue;
            }
            var text = panelProperty(children[i], "text");
            if (text === null) {
                return null;
            }
            var value = Number(text);
            return isFinite(value) && value >= 0 ? Math.floor(value) : null;
        }
        return null;
    }

    function pollOwnKillStreak(player, forceBaseline) {
        var count = killStreakCount(player);

        if (count === null) {
            // Popup not currently shown. Only drop the baseline once it has
            // been gone for a few polls, so a single-frame render gap during
            // the pop-in/out animation doesn't reset mid-streak.
            killStreakNullPolls++;
            if (killStreakNullPolls >= KILL_STREAK_RESET_POLLS) {
                lastKillStreakCount = null;
            }
            return;
        }
        killStreakNullPolls = 0;

        if (forceBaseline) {
            // Baseline (re)establishing -- e.g. mod just loaded mid-streak, or
            // just respawned. Adopt whatever is on screen without crediting it.
            lastKillStreakCount = count;
            return;
        }

        if (lastKillStreakCount === null) {
            // Popup just appeared: every hero kill pops it, so a fresh count
            // of >=1 is a kill we haven't credited yet.
            if (count >= 1) {
                emitAction("local_player_kill", {
                    detection: "kill_streak_counter_increment",
                    kills_before: 0,
                    kills_after: count
                });
            }
        } else if (count > lastKillStreakCount) {
            emitAction("local_player_kill", {
                detection: "kill_streak_counter_increment",
                kills_before: lastKillStreakCount,
                kills_after: count
            });
        }
        lastKillStreakCount = count;
    }

    // The centre-screen damage-impact popup (root id "damageImpactInfo")
    // gets one child panel per enemy you damaged/killed/assisted on, named
    // after that enemy's hero and (re)created for each new instance. When
    // the local player is credited with an assist, that instance panel
    // carries an "assist" class (alongside "KillAssistContainer" showing a
    // "KILL ASSIST" label) -- a class flag rather than bound text, so unlike
    // the Tab-gated personal KDA counters it's live regardless of the
    // scoreboard. Instance panels are destroyed after their fade-out
    // animation, so track already-credited panel references (pruned once
    // invalid) rather than a simple counter to avoid double-crediting the
    // same instance across polls.
    var creditedAssistPanels = [];
    // findChildById searches the whole HUD tree from its highest reachable
    // root, which is expensive; running it every single 100ms poll (forever,
    // for the whole match) risks slow enough script execution to disrupt the
    // poll loop itself -- including death/kill detection, which share it.
    // The assist popup stays on screen for a few seconds, so a few hundred
    // ms of extra latency here is unnoticeable.
    var damageImpactPollCounter = 0;
    var DAMAGE_IMPACT_POLL_INTERVAL_POLLS = 3; // ~0.3s at POLL_INTERVAL_SECONDS = 0.1

    function pollDamageImpactAssists(root) {
        damageImpactPollCounter++;
        if (damageImpactPollCounter < DAMAGE_IMPACT_POLL_INTERVAL_POLLS) {
            return;
        }
        damageImpactPollCounter = 0;

        var container = findChildById(root, "damageImpactInfo");
        if (!isValidPanel(container)) {
            return;
        }
        var stillValid = [];
        for (var p = 0; p < creditedAssistPanels.length; p++) {
            if (isValidPanel(creditedAssistPanels[p])) {
                stillValid.push(creditedAssistPanels[p]);
            }
        }
        creditedAssistPanels = stillValid;

        var children = panelChildren(container);
        for (var i = 0; i < children.length; i++) {
            var child = children[i];
            if (!isValidPanel(child) || !panelHasClass(child, "assist")) {
                continue;
            }
            if (creditedAssistPanels.indexOf(child) !== -1) {
                continue;
            }
            creditedAssistPanels.push(child);
            emitAction("local_player_assist", {
                detection: "damage_impact_assist_class"
            });
        }
    }

    function findLocalPlayerPanel() {
        if (isValidPanel(localPlayerPanel)) {
            return localPlayerPanel;
        }

        localPlayerPanel = null;
        deathBaselineEstablished = false;
        lastKillStreakCount = null;

        var panels = context.FindChildrenWithClassTraverse("LocalPlayer");
        for (var i = 0; i < panels.length; i++) {
            if (panels[i].paneltype === "CitadelHudTopBarPlayer") {
                localPlayerPanel = panels[i];
                return localPlayerPanel;
            }
        }

        return null;
    }

    function pollState() {
        if (!isValidPanel(context)) {
            return;
        }

        var player = findLocalPlayerPanel();
        if (!player) {
            resetAbilityState();
            $.Schedule(POLL_INTERVAL_SECONDS, pollState);
            return;
        }

        var isDead = panelHasClass(player, "Dead");

        var forceAbilityBaseline = false;
        if (!deathBaselineEstablished) {
            wasDead = isDead;
            deathBaselineEstablished = true;
            baselineSettlePollsRemaining = BASELINE_SETTLE_POLLS;
            forceAbilityBaseline = true;
        } else if (baselineSettlePollsRemaining > 0) {
            baselineSettlePollsRemaining--;
            wasDead = isDead;
        } else if (isDead !== wasDead) {
            forceAbilityBaseline = true;
            if (isDead) {
                emitAction("local_player_death", {
                    detection: "top_bar_local_player_dead_class"
                });
            }
            wasDead = isDead;
        }

        pollAbilities(forceAbilityBaseline || isDead);
        pollOwnKillStreak(player, forceAbilityBaseline || baselineSettlePollsRemaining > 0);
        pollDamageImpactAssists(highestContextAncestor());
        $.Schedule(POLL_INTERVAL_SECONDS, pollState);
    }

    emit("hook_ready", {
        poll_interval_ms: POLL_INTERVAL_SECONDS * 1000
    });
    pollState();
})();
