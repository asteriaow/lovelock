(function () {
    "use strict";

    var LOG_PREFIX = "[DEADLOCK_DEATH_HOOK]";
    var MOD_VERSION = "1.0.0";
    var POLL_INTERVAL_SECONDS = 0.1;
    var context = $.GetContextPanel();
    var localPlayerPanel = null;
    var deathBaselineEstablished = false;
    var wasDead = false;
    // Polls left in which vitals are re-baselined after a respawn: the health
    // bar refills and ramps up from 0, and that is not healing you received.
    var RESPAWN_VITALS_SETTLE_POLLS = 15;
    var respawnVitalsSettlePolls = 0;
    var sequence = 0;
    var abilityRoot = null;
    var abilityHeroIdentity = null;
    // The hero the local player actually controls, captured once the match has
    // settled. While dead you spectate a teammate, and the ability HUD then
    // shows their hero -- comparing against this lets us tell "my ability" from
    // "the hero I'm watching" so spectated play does not fire triggers.
    var localHeroIdentity = null;
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

    // ---------------------------------------------------------------
    // Panel helpers
    //
    // Small, defensive wrappers around the Panorama panel API used by every
    // watcher below. A stale or torn-down panel reference should read as
    // "not there" rather than throw, since panels come and go constantly as
    // the HUD rebuilds between loading screens, respawns and hero swaps.
    // ---------------------------------------------------------------

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

    // Id lookups for panels that live for the whole match -- the HUD-root
    // containers (#damageImpactInfo, #HudEventIndicatorsPanel) that several
    // polls below reach for every tick. FindChildTraverse walks the whole
    // subtree, so the resolved panel is cached and only re-searched once it
    // reports invalid (a HUD rebuild recreates it). Cleared on player-panel
    // reacquire. Keyed by id alone -- these ids are unique in the HUD.
    var childByIdCache = {};

    function findCachedChildById(root, id) {
        var cached = childByIdCache[id];
        if (isValidPanel(cached)) {
            return cached;
        }
        var found = findChildById(root, id);
        childByIdCache[id] = found;
        return found;
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

    function panelParent(panel) {
        try {
            return panel.GetParent ? panel.GetParent() : null;
        } catch (_error) {
            return null;
        }
    }

    // The HUD root is stable for the life of a match; only a panorama rebuild
    // (map load, full HUD reload) moves it. Climbing ~20-40 parents from the
    // context panel on every 100ms poll is wasted work, so the result is
    // cached. The cache is trusted only while the cheap invariants still hold:
    // the cached panel is valid and still the top of the tree, and context is
    // still attached below it. Any of those failing forces a fresh climb
    // (which, if context is now detached, correctly falls back to context).
    var cachedHudRoot = null;

    function highestContextAncestor() {
        if (isValidPanel(cachedHudRoot)
            && !isValidPanel(panelParent(cachedHudRoot))
            && isValidPanel(context)
            && isValidPanel(panelParent(context))) {
            return cachedHudRoot;
        }
        var panel = context;
        for (var i = 0; i < 64 && isValidPanel(panel); i++) {
            var parent = panelParent(panel);
            if (!isValidPanel(parent)) {
                break;
            }
            panel = parent;
        }
        cachedHudRoot = panel;
        return panel;
    }

    // ---------------------------------------------------------------
    // Abilities
    //
    // Each ability_container panel under hud_signature is snapshotted every
    // poll; a snapshot-to-snapshot diff (see detectAbilityTransitions) is
    // what actually reports ability_used / ability_cooldown_ready, not the
    // snapshot itself.
    // ---------------------------------------------------------------

    function findAbilityRoot(hudRoot) {
        return findChildById(
            isValidPanel(hudRoot) ? hudRoot : highestContextAncestor(),
            "hud_signature"
        );
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

    // True while the ability HUD belongs to the local player's own hero (or we
    // cannot tell yet). False means we are spectating someone else, so ability
    // and assist signals from that HUD are not ours to report.
    function controllingOwnHero(root) {
        if (localHeroIdentity === null) {
            return true;
        }
        var current = heroIdentity(root);
        return current === null || current === localHeroIdentity;
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

    function pollAbilities(forceBaseline, providedRoot) {
        var root = isValidPanel(providedRoot) ? providedRoot : findAbilityRoot();
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

    // ---------------------------------------------------------------
    // Kills (kill-streak popup)
    //
    // KillStreakText's running count pops on every hero kill, isolated ones
    // included - unlike the Tab-gated personal KDA text, it needs no
    // scoreboard open to read.
    // ---------------------------------------------------------------

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

    // ---------------------------------------------------------------
    // Assists (damage-impact panel)
    // ---------------------------------------------------------------

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

        var container = findCachedChildById(root, "damageImpactInfo");
        if (!isValidPanel(container)) {
            return;
        }
        creditedAssistPanels = prunedValidPanels(creditedAssistPanels);

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

    // ---------------------------------------------------------------
    // Soul orb deny
    //
    // The floating combat-number system stamps a category class on each
    // indicator instance: ".gold" / ".gold_small" for a soul gained, ".deny"
    // for a soul deny. Two panels host these instances with the same
    // vocabulary -- #HudEventIndicatorsPanel and #DamageFeedbackDisplay
    // (paneltype CitadelDamageFeedbackDisplay) -- and a deny can land in
    // either, so both are scanned, plus a HUD-wide fallback for ".deny"
    // (rare) in case an id has moved. Instances fade out and are torn down,
    // so credited refs are kept and pruned, like the assist popup above.
    //
    // Whether ".deny" renders on the *denier's* screen or the *denied*
    // player's is still unconfirmed, and a deny may actually be a flavoured
    // ".gold" rather than its own ".deny". Until a live capture settles it,
    // every ".deny" instance also emits a sequence-less soul_deny_diag with
    // its class list + text, and the first few ".gold" instances a session
    // emit soul_gold_diag, so console.log shows the real shape.
    // ---------------------------------------------------------------
    var EVENT_INDICATOR_IDS = ["HudEventIndicatorsPanel", "DamageFeedbackDisplay"];
    var SOUL_DENY_CLASS = "deny";
    // Captured live: when an enemy denies YOUR orb you get ".gold_small" (a
    // reduced gain), never ".deny". So ".deny" is still the candidate for the
    // opposite -- YOU denying THEIR orb -- but that has not been captured yet,
    // and it may instead show as a plain ".gold". The diagnostics below log
    // every new soul indicator (".deny", ".gold", ".gold_small"), tagged with
    // the panel it sat under, so a "you deny them" capture settles it.
    var SOUL_DIAG_CLASSES = ["deny", "gold", "gold_small"];
    // Keep the HUD-wide ".deny" fallback from matching an unrelated "deny".
    var INDICATOR_INSTANCE_HINT = "HudIndicatorContainer";
    var INDICATOR_CLASS_PROBES = [
        "deny", "gold", "gold_small", "crit", "batched", "buffed", "heal",
        "status_effect", "damage_type_gun", "damage_type_ability",
        "damage_type_melee", "damage_type_pure", "damage_type_poison",
        "WindowRoot", "HudIndicatorContainer"
    ];
    var creditedDenyPanels = [];
    var creditedSoulDiagPanels = [];
    var soulDiagBudget = 60; // generous shape dump for one targeted test session
    var soulDenyPollCounter = 0;
    var SOUL_DENY_POLL_INTERVAL_POLLS = 3; // ~0.3s at POLL_INTERVAL_SECONDS = 0.1

    function collectIndicatorInstances(root, className, wideFallback) {
        var out = [];
        function add(list) {
            for (var i = 0; i < list.length; i++) {
                if (isValidPanel(list[i]) && out.indexOf(list[i]) === -1) {
                    out.push(list[i]);
                }
            }
        }
        for (var c = 0; c < EVENT_INDICATOR_IDS.length; c++) {
            var container = findCachedChildById(root, EVENT_INDICATOR_IDS[c]);
            if (isValidPanel(container)) {
                add(findChildrenWithClass(container, className));
            }
        }
        if (wideFallback) {
            var wide = findChildrenWithClass(root, className);
            for (var w = 0; w < wide.length; w++) {
                if (isValidPanel(wide[w])
                    && (panelHasClass(wide[w], INDICATOR_INSTANCE_HINT)
                        || findChildrenWithClass(wide[w], INDICATOR_INSTANCE_HINT).length > 0)) {
                    add([wide[w]]);
                }
            }
        }
        return out;
    }

    // Panorama exposes no class enumeration, so probe the known indicator
    // vocabulary. Returns a space-joined list for the diagnostics.
    function indicatorClassList(panel) {
        var hit = [];
        for (var i = 0; i < INDICATOR_CLASS_PROBES.length; i++) {
            if (panelHasClass(panel, INDICATOR_CLASS_PROBES[i])) {
                hit.push(INDICATOR_CLASS_PROBES[i]);
            }
        }
        return hit.join(" ");
    }

    function indicatorText(panel) {
        var text = textFromClass(panel, "HudIndicatorText");
        return typeof text === "string" ? text : "";
    }

    function pollSoulDeny(root) {
        soulDenyPollCounter++;
        if (soulDenyPollCounter < SOUL_DENY_POLL_INTERVAL_POLLS) {
            return;
        }
        soulDenyPollCounter = 0;

        creditedDenyPanels = prunedValidPanels(creditedDenyPanels);
        creditedSoulDiagPanels = prunedValidPanels(creditedSoulDiagPanels);

        var denies = collectIndicatorInstances(root, SOUL_DENY_CLASS, true);
        for (var i = 0; i < denies.length; i++) {
            var panel = denies[i];
            if (creditedDenyPanels.indexOf(panel) !== -1) {
                continue;
            }
            creditedDenyPanels.push(panel);
            emitAction("soul_deny", { detection: "feedback_indicator_class:deny" });
        }

        // Shape dump: every new soul indicator this session, tagged with which
        // panel hosted it, so a "you deny their orb" capture shows the class
        // and where it lives. Capped so a full match cannot flood the log.
        if (soulDiagBudget <= 0) {
            return;
        }
        var scopes = [];
        for (var s = 0; s < EVENT_INDICATOR_IDS.length; s++) {
            var scopePanel = findCachedChildById(root, EVENT_INDICATOR_IDS[s]);
            if (isValidPanel(scopePanel)) {
                scopes.push({ where: EVENT_INDICATOR_IDS[s], panel: scopePanel });
            }
        }
        scopes.push({ where: "hud_wide", panel: root });
        for (var sc = 0; sc < scopes.length && soulDiagBudget > 0; sc++) {
            for (var cl = 0; cl < SOUL_DIAG_CLASSES.length && soulDiagBudget > 0; cl++) {
                var hits = findChildrenWithClass(scopes[sc].panel, SOUL_DIAG_CLASSES[cl]);
                for (var h = 0; h < hits.length && soulDiagBudget > 0; h++) {
                    var found = hits[h];
                    if (!isValidPanel(found) || creditedSoulDiagPanels.indexOf(found) !== -1) {
                        continue;
                    }
                    creditedSoulDiagPanels.push(found);
                    soulDiagBudget--;
                    emit("soul_indicator_diag", {
                        where: scopes[sc].where,
                        match: SOUL_DIAG_CLASSES[cl],
                        classes: indicatorClassList(found),
                        text: indicatorText(found)
                    });
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // Ally support: healing or shielding a teammate
    //
    // The centre-screen impact popup (#damageImpactInfo) spawns a
    // .damageImpactInstance for each teammate you affected. Inside every
    // instance are two bars that rest at width 0 and only grow when that
    // thing actually happened (confirmed in hud_damage_impact.vcss):
    //   #healthGained  (forestgreen) -> you restored their health
    //   #barrierGained (#a2a37a)     -> you gave them a barrier / shield,
    //                                   e.g. Divine Barrier, Rescue Beam
    // They are independent, so a tick that both heals and shields fires
    // both, and a shield is never mistaken for a heal: the instance also
    // carries an ambiguous .is_heal class for any friendly interaction, so
    // that class is deliberately ignored here. Instances are torn down after
    // their fade, so credited panel refs are kept and pruned.
    // ---------------------------------------------------------------
    var SUPPORT_BAR_MIN_WIDTH = 2; // the bars rest collapsed at 0

    var creditedHealPanels = [];
    var creditedShieldPanels = [];
    var supportPollCounter = 0;
    var SUPPORT_POLL_INTERVAL_POLLS = 3; // ~0.3s at POLL_INTERVAL_SECONDS = 0.1

    // True when the instance's `#barId` bar is drawn and has grown past its
    // collapsed resting width, i.e. that effect actually landed.
    function instanceBarGrew(instance, barId, minWidth) {
        var bar = findChildById(instance, barId);
        if (!isValidPanel(bar) || panelProperty(bar, "visible") === false) {
            return false;
        }
        var width = panelProperty(bar, "actuallayoutwidth");
        return typeof width === "number" && width > minWidth;
    }

    function prunedValidPanels(list) {
        var kept = [];
        for (var i = 0; i < list.length; i++) {
            if (isValidPanel(list[i])) {
                kept.push(list[i]);
            }
        }
        return kept;
    }

    function resetAllySupportState() {
        creditedHealPanels = [];
        creditedShieldPanels = [];
        supportPollCounter = 0;
        creditedDenyPanels = [];
        creditedSoulDiagPanels = [];
        soulDiagBudget = 60;
        soulDenyPollCounter = 0;
    }

    function pollAllySupport(root) {
        supportPollCounter++;
        if (supportPollCounter < SUPPORT_POLL_INTERVAL_POLLS) {
            return;
        }
        supportPollCounter = 0;

        var container = findChildById(root, "damageImpactInfo");
        if (!isValidPanel(container)) {
            return;
        }
        creditedHealPanels = prunedValidPanels(creditedHealPanels);
        creditedShieldPanels = prunedValidPanels(creditedShieldPanels);

        var children = panelChildren(container);
        for (var i = 0; i < children.length; i++) {
            var child = children[i];
            if (!isValidPanel(child)) {
                continue;
            }

            if (instanceBarGrew(child, "barrierGained", SUPPORT_BAR_MIN_WIDTH)
                && creditedShieldPanels.indexOf(child) === -1) {
                creditedShieldPanels.push(child);
                emitAction("ally_shielded", { detection: "impact_bar:barrierGained" });
            }

            if (instanceBarGrew(child, "healthGained", SUPPORT_BAR_MIN_WIDTH)
                && creditedHealPanels.indexOf(child) === -1) {
                creditedHealPanels.push(child);
                emitAction("ally_healed", { detection: "impact_bar:healthGained" });
            }
        }
    }

    // ---------------------------------------------------------------
    // Combat feedback: damage you deal, parries, and the health band the
    // low-health screen effect keys off.
    //
    // Every probe here is a single native FindChildrenWithClassTraverse
    // (it runs C++ side) off the HUD root; there is deliberately no JS tree
    // walking, since that is what makes a per-poll watcher cost frames. The
    // class names below were read straight out of the shipped Deadlock
    // Panorama styles, so each is one exact string rather than a guess list:
    //   hud_event_indicator.vcss:                 HudIndicatorText, heal,
    //     damage_type_gun|ability|melee|pure|poison, gold, deny
    //   ability_hud_elements/element_gun.vcss:    parry_on_cooldown, stunned,
    //     ability_element_gun
    //   hud_damage_impact.vcss:                   damageImpactInstance, Stunned
    //   hud_health_container.vcss:                localPlayerLowHealth,
    //     localPlayerMidHealth, healthLow
    // The category class sits on the indicator instance, a couple of levels
    // above its HudIndicatorText label.
    //
    // (Soul orb *secure* stays unreported: the HUD gives one flat ".gold"
    // number for every soul gain, so a shot orb is indistinguishable from
    // one walked over. Soul orb *deny* is reported by pollSoulDeny via the
    // dedicated ".deny" indicator class.)
    //
    // Parry has no "landed" class, so it is inferred inside a short window
    // after the crosshair's parry goes on cooldown (you threw one): your own
    // "stunned" means you got parried; an enemy's "Stunned" on their
    // #damageImpactInfo instance means your parry connected; neither means it
    // hit nothing, and nothing is emitted.
    // ---------------------------------------------------------------
    var FEEDBACK_LABEL_CLASS = "HudIndicatorText";
    var FEEDBACK_DAMAGE_CLASSES = [
        "damage_type_gun", "damage_type_ability", "damage_type_melee",
        "damage_type_pure", "damage_type_poison",
        "bullet_damage_new", "ability_damage_new", "melee_damage_new",
        "pure_damage_new"
    ];
    // label -> HudIndicatorContainer -> WindowRoot(instance) carries the
    // category class. Walk well past that in case of extra wrappers.
    var FEEDBACK_CATEGORY_DEPTH = 6;
    var PARRY_COOLDOWN_CLASS = "parry_on_cooldown";
    var PARRY_STUNNED_CLASS = "stunned";
    // The status class an enemy's #damageImpactInfo instance carries while
    // stunned (hud_damage_impact.vcss: `.damageImpactInstance.Stunned`). Seen
    // inside the post-parry window, it means the parry connected.
    var PARRY_TARGET_STUN_CLASS = "Stunned";
    // The crosshair/gun HUD element. `parry_on_cooldown` and `stunned` both
    // live on it or on its children (per element_gun.vcss), but not
    // necessarily on the same panel, so the stun probe is scoped to this
    // element's whole subtree rather than walked up from the cooldown panel.
    var PARRY_GUN_ELEMENT_CLASS = "ability_element_gun";
    var HEALTH_LOW_CLASSES = ["localPlayerLowHealth", "healthLow"];
    var HEALTH_MID_CLASS = "localPlayerMidHealth";

    var COMBAT_POLL_INTERVAL_POLLS = 3; // ~0.3s
    var DAMAGE_GIVEN_FLUSH_MS = 250;
    var PARRY_RESOLVE_MS = 900;
    // A crowded team fight can stack a lot of floating numbers at once; cap
    // the per-scan work so one busy frame cannot spike.
    var FEEDBACK_LABEL_SCAN_CAP = 40;

    var combatPollCounter = 0;
    var damageGivenSeen = [];       // [{panel, value}] running per-number tallies
    var damageGivenPending = 0;
    var labelKindCache = [];        // [{panel, category}] classified feedback labels
    var damageGivenFlushAt = 0;
    var damageDiagBudget = 30;
    var unclassDiagBudget = 40;
    var unclassDiagPanels = [];
    var damageDiagPanels = [];

    var parryCdWas = false;
    var parryResolveAt = 0;
    var parryStunSeen = false;      // you were stunned -> got parried
    var parryEnemyStunSeen = false; // an enemy was freshly stunned -> parry connected
    var parryPreStunnedPanels = []; // enemy instances already stunned when the window opened

    var healthBand = 0;             // 0 healthy, 1 mid, 2 low
    var healthBandBaselined = false;

    // Walk up at most maxDepth parents looking for `className`. Only ever
    // called on the handful of panels a native traverse already returned, so
    // the climb is short and bounded.
    function anyAncestorHasClass(panel, className, maxDepth) {
        var node = panel;
        for (var depth = 0; depth <= maxDepth && isValidPanel(node); depth++) {
            if (panelHasClass(node, className)) {
                return true;
            }
            try {
                node = node.GetParent ? node.GetParent() : null;
            } catch (_error) {
                node = null;
            }
        }
        return false;
    }

    // One upward walk that classifies a feedback label. Returns
    // { kind, instance }: kind is "heal", "damage" or null; instance is the
    // ancestor panel that carried the class (one feedback popup holds several
    // HudIndicatorText labels -- value, effectiveness, description).
    function feedbackCategory(label) {
        var node = label;
        for (var depth = 0; depth <= FEEDBACK_CATEGORY_DEPTH && isValidPanel(node); depth++) {
            if (panelHasClass(node, "heal")) { return { kind: "heal", instance: node }; }
            for (var i = 0; i < FEEDBACK_DAMAGE_CLASSES.length; i++) {
                if (panelHasClass(node, FEEDBACK_DAMAGE_CLASSES[i])) {
                    return { kind: "damage", instance: node };
                }
            }
            try {
                node = node.GetParent ? node.GetParent() : null;
            } catch (_error) {
                node = null;
            }
        }
        return { kind: null, instance: null };
    }

    // Reads a floating damage number's text into a positive number. Tolerates
    // a leading "+", surrounding whitespace and a "k"/"m" magnitude suffix
    // (Deadlock abbreviates large hits). Returns 0 for anything unparseable
    // or non-positive.
    function parseFeedbackNumber(text) {
        if (typeof text !== "string" && typeof text !== "number") {
            return 0;
        }
        var raw = ("" + text).trim().toLowerCase();
        // A feedback popup also carries an effectiveness label like "(50)%";
        // that is not a damage amount.
        if (raw.indexOf("%") !== -1 || /[0-9] *(ms|s|sec)$/.test(raw)) {
            return 0;
        }
        var scale = 1;
        if (raw.charAt(raw.length - 1) === "k") {
            scale = 1000;
            raw = raw.slice(0, -1);
        } else if (raw.charAt(raw.length - 1) === "m") {
            scale = 1000000;
            raw = raw.slice(0, -1);
        }
        raw = raw.replace(/[^0-9.\-]/g, "");
        var value = Number(raw) * scale;
        return isFinite(value) && value > 0 ? value : 0;
    }

    // True if a single native traverse off root turns up any panel carrying
    // one of classNames. Short-circuits on the first class that hits.
    function hudHasClass(root, classNames) {
        for (var i = 0; i < classNames.length; i++) {
            if (findChildrenWithClass(root, classNames[i]).length > 0) {
                return true;
            }
        }
        return false;
    }

    function resetCombatState() {
        combatPollCounter = 0;
        damageGivenSeen = [];
        labelKindCache = [];
        damageGivenPending = 0;
        damageGivenFlushAt = 0;
        parryCdWas = false;
        parryResolveAt = 0;
        parryStunSeen = false;
        parryEnemyStunSeen = false;
        parryPreStunnedPanels = [];
        healthBand = 0;
        healthBandBaselined = false;
    }

    // True while the local player's crosshair/gun element is showing the
    // stunned state. Checks the gun element itself and its whole subtree,
    // since `stunned` and `parry_on_cooldown` are not guaranteed to sit on
    // the same panel. Deliberately not a HUD-wide search: other panels
    // (status-effect icons, other players' bars) also use a "stunned" class
    // and would turn a clean parry into a false parry_fail. Falls back to an
    // ancestor walk from the cooldown panels if the gun element class cannot
    // be located in this HUD build.
    function parryStunVisible(root, cdPanels) {
        var gunEls = findChildrenWithClass(root, PARRY_GUN_ELEMENT_CLASS);
        for (var g = 0; g < gunEls.length; g++) {
            if (panelHasClass(gunEls[g], PARRY_STUNNED_CLASS)
                || findChildrenWithClass(gunEls[g], PARRY_STUNNED_CLASS).length > 0) {
                return true;
            }
        }
        if (gunEls.length === 0) {
            for (var p = 0; p < cdPanels.length; p++) {
                if (anyAncestorHasClass(cdPanels[p], PARRY_STUNNED_CLASS, 4)) {
                    return true;
                }
            }
        }
        return false;
    }

    // Enemy #damageImpactInfo instances currently showing the stunned state
    // (a hit of yours stunned them). Killed instances are excluded -- their
    // status label is suppressed and a kill is not a parry.
    function stunnedEnemyInstances(root) {
        var out = [];
        var container = findCachedChildById(root, "damageImpactInfo");
        if (!isValidPanel(container)) {
            return out;
        }
        var stunned = findChildrenWithClass(container, PARRY_TARGET_STUN_CLASS);
        for (var i = 0; i < stunned.length; i++) {
            if (panelHasClass(stunned[i], "damageImpactInstance")
                && !panelHasClass(stunned[i], "killed")) {
                out.push(stunned[i]);
            }
        }
        return out;
    }

    // True if `instances` holds one that was not already stunned when the
    // parry window opened -- a fresh stun this parry is responsible for,
    // rather than an ability stun from before it that is still fading.
    function hasFreshStun(instances) {
        for (var i = 0; i < instances.length; i++) {
            if (parryPreStunnedPanels.indexOf(instances[i]) === -1) {
                return true;
            }
        }
        return false;
    }

    // Runs every poll: the parry state machine wants finer timing than the
    // 3-poll combat cadence. Idle cost is a single native traverse.
    function pollParryState(root, now) {
        var cdPanels = findChildrenWithClass(root, PARRY_COOLDOWN_CLASS);
        var cdNow = cdPanels.length > 0;

        if (cdNow && !parryCdWas) {
            parryResolveAt = now + PARRY_RESOLVE_MS;
            parryStunSeen = false;
            parryEnemyStunSeen = false;
            // Enemies already stunned as the parry starts are not this parry's
            // doing; only a stun that appears afterwards counts as a connect.
            parryPreStunnedPanels = stunnedEnemyInstances(root);
        }
        parryCdWas = cdNow;

        if (parryResolveAt > 0) {
            if (!parryStunSeen && parryStunVisible(root, cdPanels)) {
                parryStunSeen = true;
            }
            if (!parryStunSeen && !parryEnemyStunSeen
                && hasFreshStun(stunnedEnemyInstances(root))) {
                parryEnemyStunSeen = true;
            }
            if (now >= parryResolveAt) {
                if (parryStunSeen) {
                    emitAction("parry_fail", { detection: "stunned_after_parry" });
                } else if (parryEnemyStunSeen) {
                    emitAction("parry_success", { detection: "enemy_stunned_after_parry" });
                }
                // Neither: the parry hit nothing. Emit nothing.
                parryResolveAt = 0;
                parryPreStunnedPanels = [];
            }
        }
    }

    function pollHealthBand(root, rebaseline) {
        var band = 0;
        if (hudHasClass(root, HEALTH_LOW_CLASSES)) {
            band = 2;
        } else if (findChildrenWithClass(root, HEALTH_MID_CLASS).length > 0) {
            band = 1;
        }
        if (rebaseline || !healthBandBaselined) {
            healthBand = band;
            healthBandBaselined = true;
            return;
        }
        if (band > healthBand) {
            emitAction("damage_taken_intensity", {
                detection: "health_band:" + band
            });
        }
        healthBand = band;
    }

    function pollCombatFeedback(root, now) {
        var keptDamage = [];
        for (var d = 0; d < damageGivenSeen.length; d++) {
            if (isValidPanel(damageGivenSeen[d].panel)) {
                keptDamage.push(damageGivenSeen[d]);
            }
        }
        damageGivenSeen = keptDamage;
        var keptKinds = [];
        for (var k = 0; k < labelKindCache.length; k++) {
            if (isValidPanel(labelKindCache[k].panel)) {
                keptKinds.push(labelKindCache[k]);
            }
        }
        labelKindCache = keptKinds;

        var labels = findChildrenWithClass(root, FEEDBACK_LABEL_CLASS);
        var scanned = Math.min(labels.length, FEEDBACK_LABEL_SCAN_CAP);
        for (var i = 0; i < scanned; i++) {
            var label = labels[i];
            if (!isValidPanel(label)) {
                continue;
            }
            // A label's category is fixed for its lifetime, and classifying it
            // climbs several parents with a class probe at each, so it is done
            // once per label rather than on every poll of a busy team fight.
            var category = null;
            for (var c = 0; c < labelKindCache.length; c++) {
                if (labelKindCache[c].panel === label) {
                    category = labelKindCache[c].category;
                    break;
                }
            }
            if (category === null) {
                category = feedbackCategory(label);
                labelKindCache.push({ panel: label, category: category });
            }
            // Empty, uncategorised labels exist from match start and used to
            // burn the whole diag budget before any real hit; skip them.
            if (category.kind !== null && damageDiagBudget > 0 && damageDiagPanels.indexOf(label) === -1) {
                damageDiagBudget--;
                damageDiagPanels.push(label);
                emit("damage_feedback_diag", {
                    kind: category.kind,
                    text: panelProperty(label, "text"),
                    parent_classes: indicatorClassList(panelParent(label))
                });
            }

            // Uncategorised labels with text: log their ancestry so a wrong class
            // name for the floating damage numbers shows up in console.log.
            if (category.kind === null && unclassDiagBudget > 0 && unclassDiagPanels.indexOf(label) === -1) {
                var unclassText = panelProperty(label, "text");
                if (typeof unclassText === "string" && unclassText !== "" && /[0-9]/.test(unclassText)) {
                    unclassDiagBudget--;
                    unclassDiagPanels.push(label);
                    var chain = [];
                    var up = label;
                    for (var u = 0; u < 5 && isValidPanel(up); u++) {
                        chain.push(indicatorClassList(up));
                        up = panelParent(up);
                    }
                    emit("damage_unclassified_diag", { text: unclassText, chain: chain.join(" < ") });
                }
            }
            // A heal number is not damage you dealt; skip it. Anything without
            // a damage_type class (xp, mana burn, status text) is skipped too.
            // The game's own damage numbers carry no category class (verified in
            // console.log: label < HudIndicatorContainer < bare or "crit buffed"
            // WindowRoot), unlike heal/gold/deny, so a plain unsigned number in an
            // indicator container that is none of those is a damage number.
            var plainDamage = false;
            if (category.kind === null) {
                var plainText = panelProperty(label, "text");
                plainDamage = typeof plainText === "string" && /^[0-9][0-9.,]*k?$/i.test(plainText.trim())
                    && anyAncestorHasClass(label, "HudIndicatorContainer", 3)
                    && !anyAncestorHasClass(label, "gold", 6)
                    && !anyAncestorHasClass(label, "deny", 6);
            }
            if (category.kind !== "damage" && !plainDamage) {
                continue;
            }

            var value = parseFeedbackNumber(panelProperty(label, "text"));
            if (value <= 0) {
                continue;
            }
            var entry = null;
            for (var e = 0; e < damageGivenSeen.length; e++) {
                if (damageGivenSeen[e].panel === label) {
                    entry = damageGivenSeen[e];
                    break;
                }
            }
            if (!entry) {
                entry = { panel: label, value: 0 };
                damageGivenSeen.push(entry);
            }
            // A "batched" number grows in place, so credit only the rise.
            var delta = value - entry.value;
            entry.value = value;
            if (delta > 0) {
                damageGivenPending += delta;
            }
        }

        if (damageGivenPending > 0 && now >= damageGivenFlushAt) {
            damageGivenFlushAt = now + DAMAGE_GIVEN_FLUSH_MS;
            emitAction("damage_given", {
                detection: "feedback_damage_numbers",
                amount: damageGivenPending
            });
            damageGivenPending = 0;
        }
    }

    function pollCombat(root, rebaseline) {
        if (!isValidPanel(root)) {
            return;
        }
        var now = Date.now();
        // A single bad panel read must never take down the whole poll loop
        // (which also carries death and kill detection).
        try {
            pollParryState(root, now);

            combatPollCounter++;
            if (combatPollCounter < COMBAT_POLL_INTERVAL_POLLS) {
                return;
            }
            combatPollCounter = 0;
            pollHealthBand(root, rebaseline);
            pollCombatFeedback(root, now);
        } catch (_error) {
        }
    }

    // ---------------------------------------------------------------
    // Objectives: enemy structures destroyed, and winning the match.
    //
    // Three surfaces, all read straight from shipped Deadlock styles:
    //   objectives_map.vcss  -- #ObjectivesMap (already in this mod's top-bar
    //     layout) holds one stable-id panel per structure:
    //       #Team{N}Tier1_1..4  Guardians
    //       #Team{N}Tier2_1..4  Walkers
    //       #Team{N}Core        Patron / core
    //     each carries `.Alive` while it stands; a wrapper carries
    //     `.Team{N}IsEnemy` / `.Team{N}IsFriend`. A per-poll `.Alive` diff on
    //     the enemy side is map-wide and needs no proximity.
    //   hud_objective_health.vcss -- the single centre-screen boss bar
    //     (class "objective_health"), whose class set is the full taxonomy:
    //       .is_tier1 .is_tier2 .is_barracks_boss .is_shield_generator
    //       .is_titan .is_weakened .is_dead .friend
    //     Used for Base Guardian / Shrine / weakened Patron, which have no
    //     objectives-map id. Best-effort: the bar only shows the objective you
    //     are near/contesting, so an off-screen kill can be missed.
    //   hud_match_end.vcss -- CitadelHudMatchEnd gains .ShowMatchEnd plus
    //     .LocalPlayerTeam{N} and .Team{N}Victory; matching N is a win.
    // ---------------------------------------------------------------
    var OBJECTIVE_POLL_INTERVAL_POLLS = 5; // ~0.5s; objectives fall rarely
    var OBJECTIVE_MAP_ID = "ObjectivesMap";
    var OBJECTIVE_GUARDIAN_SUFFIXES = ["Tier1_1", "Tier1_2", "Tier1_3", "Tier1_4"];
    var OBJECTIVE_WALKER_SUFFIXES = ["Tier2_1", "Tier2_2", "Tier2_3", "Tier2_4"];
    var OBJECTIVE_HEALTH_CLASS = "objective_health";
    // hud_data_feed.vcss: #ObjectivesFeed is the on-screen kill feed for
    // structures/bosses. Unlike the centre-screen objective_health bar it does
    // not depend on standing near the objective, so it is a second, position-
    // independent source for Base Guardian / Shrine kills (the two the bar
    // catches only when you are there). See pollObjectiveFeed.
    var OBJECTIVE_FEED_ID = "ObjectivesFeed";
    // The feed row and the bar's `is_dead` for the same structure land within
    // a second or two of each other, so an emit of a given objective event
    // from either surface suppresses the same event name from the other for
    // this long. Kept short so two structures of the same kind falling in
    // quick succession are not both swallowed.
    var OBJECTIVE_DEDUP_MS = 2500;
    // citadel_hud_data_feed_boss_killed.vcss carries no structure-type class,
    // so a row is typed from an explicit class if one is ever present, then
    // the victim image src, then the victim label text. These are the class
    // names the objective_health bar uses, checked first in case a future
    // build puts them on the feed row too.
    var OBJECTIVE_FEED_TYPE_CLASSES = [
        ["is_barracks_boss", "base_guardian"],
        ["is_shield_generator", "shrine"],
        ["is_tier2", "walker"],
        ["is_tier1", "guardian"]
    ];
    var OBJECTIVE_FEED_EVENT = {
        base_guardian: "objective_base_guardian",
        shrine: "objective_shrine",
        walker: "objective_walker",
        guardian: "objective_guardian"
    };

    var objectivePollCounter = 0;
    var objectiveAliveState = {};   // full id -> last-seen `.Alive` boolean
    var objectivesBaselined = false;
    var gameWonEmitted = false;
    var gameLostEmitted = false;
    var objHealthTracked = [];      // [{panel, sig, dead, weakened}] per objective_health panel
    var creditedObjectiveFeedPanels = []; // feed rows already handled (rows fade + tear down)
    var recentObjectiveEmits = {};        // objective event name -> last emit Date.now()
    var feedDiagBudget = 20;

    function resetObjectiveState() {
        objectivePollCounter = 0;
        objectiveAliveState = {};
        objectivesBaselined = false;
        gameWonEmitted = false;
        gameLostEmitted = false;
        objHealthTracked = [];
        creditedObjectiveFeedPanels = [];
        recentObjectiveEmits = {};
    }

    // Emit an objective action unless the other objective surface already
    // emitted the same event within OBJECTIVE_DEDUP_MS. Records the time on
    // every successful emit so the next call from either surface sees it.
    function emitObjectiveDeduped(eventName, fields) {
        var now = Date.now();
        if (now - (recentObjectiveEmits[eventName] || 0) < OBJECTIVE_DEDUP_MS) {
            return false;
        }
        recentObjectiveEmits[eventName] = now;
        emitAction(eventName, fields);
        return true;
    }

    function panelHasAnyClass(panel, classNames) {
        for (var i = 0; i < classNames.length; i++) {
            if (panelHasClass(panel, classNames[i])) {
                return true;
            }
        }
        return false;
    }

    // "1" or "2" for the enemy team, or null while it cannot be told (pre-game,
    // spectator, class not set yet). The IsEnemy class sits on #ObjectivesMap
    // or a wrapper a couple of levels above it.
    function objectiveEnemyTeam(objMap) {
        for (var d = 0, node = objMap; d <= 3 && isValidPanel(node); d++) {
            if (panelHasClass(node, "Team1IsEnemy")) { return "1"; }
            if (panelHasClass(node, "Team2IsEnemy")) { return "2"; }
            node = panelParent(node);
        }
        return null;
    }

    // Diff one objectives-map panel's `.Alive` against last poll; emit `event`
    // on the alive->destroyed edge once the baseline pass has run.
    function diffObjectiveAlive(objMap, fullId, event, detection) {
        var panel = findChildById(objMap, fullId);
        if (!isValidPanel(panel)) {
            return; // absent for this lane count, or not built yet
        }
        var alive = panelHasClass(panel, "Alive");
        var prev = objectiveAliveState[fullId];
        objectiveAliveState[fullId] = alive;
        if (objectivesBaselined && prev === true && alive === false) {
            emitAction(event, { detection: detection });
        }
    }

    var OBJECTIVE_HEALTH_PROBES = [
        "is_active", "is_dead", "is_weakened", "is_titan", "is_barracks_boss",
        "is_shield_generator", "is_tier1", "is_tier2", "is_mid", "friend",
        "team1", "team2"
    ];
    var objHealthDiagBudget = 40;
    var objHealthNullDiagBudget = 15;

    // The team layout builds several objective_health panels (per team and per
    // structure group), most of them idle, so every one is checked and tracked
    // on its own. A state class may sit on the panel itself or on a child, so
    // the panel is tried first and its subtree second.
    function objHasClass(w, className) {
        return panelHasClass(w, className)
            || findChildrenWithClass(w, className).length > 0;
    }

    function objectiveHealthType(w) {
        var types = [
            ["is_barracks_boss", "base_guardian"],
            ["is_shield_generator", "shrine"],
            ["is_titan", "titan"],
            ["is_tier1", "guardian"],
            ["is_tier2", "walker"]
        ];
        var i;
        for (i = 0; i < types.length; i++) {
            if (panelHasClass(w, types[i][0])) { return types[i][1]; }
        }
        for (i = 0; i < types.length; i++) {
            if (findChildrenWithClass(w, types[i][0]).length > 0) { return types[i][1]; }
        }
        return null; // mid boss / neutral / nothing shown
    }

    function objectiveHealthSignature(w) {
        var hit = [];
        for (var i = 0; i < OBJECTIVE_HEALTH_PROBES.length; i++) {
            if (objHasClass(w, OBJECTIVE_HEALTH_PROBES[i])) {
                hit.push(OBJECTIVE_HEALTH_PROBES[i]);
            }
        }
        return hit.join(" ");
    }

    // Native panel reads are the expensive part of Panorama script work, so
    // each panel's type and team are resolved once and cached (they never
    // change for a given panel), an untyped panel is only re-examined every
    // OBJECTIVE_TYPE_RECHECK_POLLS objective polls, and the shape dump is
    // rate-limited per panel.
    var OBJECTIVE_TYPE_RECHECK_POLLS = 10; // ~5s at the objective cadence
    var OBJECTIVE_DIAG_MIN_MS = 3000;

    // The bars are named objective_<entity index> at runtime, so they are also
    // found by id in case the objective_health class is not on the panel.
    var objIdPanels = [];
    var objIdScanPolls = 0;
    var objIdDiagBudget = 25;

    function collectObjectiveIdPanels(node, depth, out) {
        if (depth > 7 || out.length >= 40) {
            return;
        }
        var kids = panelChildren(node);
        for (var i = 0; i < kids.length && out.length < 40; i++) {
            var kid = kids[i];
            if (!isValidPanel(kid)) {
                continue;
            }
            var id = panelProperty(kid, "id");
            if (typeof id === "string" && id.indexOf("objective_") === 0) {
                out.push(kid);
            }
            collectObjectiveIdPanels(kid, depth + 1, out);
        }
    }

    function pollObjectiveHealth(root) {
        var matches = findChildrenWithClass(root, OBJECTIVE_HEALTH_CLASS);
        objIdPanels = prunedValidPanels(objIdPanels);
        if (objIdScanPolls++ % 5 === 0) {
            var found = [];
            collectObjectiveIdPanels(root, 0, found);
            for (var f = 0; f < found.length; f++) {
                if (objIdPanels.indexOf(found[f]) === -1) {
                    objIdPanels.push(found[f]);
                    if (objIdDiagBudget > 0) {
                        objIdDiagBudget--;
                        emit("objective_health_diag", { by: "id", id: panelProperty(found[f], "id"), classes: objectiveHealthSignature(found[f]) });
                    }
                }
            }
        }
        for (var g = 0; g < objIdPanels.length; g++) {
            if (matches.indexOf(objIdPanels[g]) === -1) { matches.push(objIdPanels[g]); }
        }
        objHealthTracked = prunedValidPanelEntries(objHealthTracked);
        var now = Date.now();
        for (var m = 0; m < matches.length; m++) {
            var w = matches[m];
            if (!isValidPanel(w)) {
                continue;
            }
            var entry = null;
            for (var t = 0; t < objHealthTracked.length; t++) {
                if (objHealthTracked[t].panel === w) {
                    entry = objHealthTracked[t];
                    break;
                }
            }
            if (!entry) {
                entry = {
                    panel: w, type: null, friendly: false, polls: 0,
                    sig: "", sigAt: 0, dead: false, weakened: false
                };
                objHealthTracked.push(entry);
            }

            if (entry.type === null) {
                entry.polls++;
                if (entry.polls % OBJECTIVE_TYPE_RECHECK_POLLS !== 1) {
                    continue;
                }
                entry.type = objectiveHealthType(w);
                if (entry.type === null) {
                    // Log panels that matched objective_health but carry none of
                    // the type classes, so a wrong class name shows in console.log.
                    if (objHealthNullDiagBudget > 0 && !entry.nullLogged) {
                        entry.nullLogged = true;
                        objHealthNullDiagBudget--;
                        emit("objective_health_diag", {
                            type: null,
                            id: panelProperty(w, "id"),
                            paneltype: panelProperty(w, "paneltype"),
                            classes: objectiveHealthSignature(w)
                        });
                    }
                    continue; // mid boss / neutral / nothing shown yet
                }
                entry.friendly = anyAncestorHasClass(w, "friend", 6);
            }

            // Shape dump: log a typed panel's class set when it changes, so a
            // missed objective can be tuned from console.log.
            if (objHealthDiagBudget > 0 && now - entry.sigAt >= OBJECTIVE_DIAG_MIN_MS) {
                entry.sigAt = now;
                var sig = objectiveHealthSignature(w);
                if (sig !== entry.sig) {
                    entry.sig = sig;
                    objHealthDiagBudget--;
                    emit("objective_health_diag", {
                        type: entry.type,
                        friendly: entry.friendly,
                        classes: sig
                    });
                }
            }
            if (entry.friendly) {
                continue; // a friendly objective
            }
            // Phase 1 ends when the Patron starts transforming, ~20s before
            // is_weakened would appear, so is_transforming is the only trigger.
            if (!entry.weakened && entry.type === "titan" && objHasClass(w, "is_transforming")) {
                entry.weakened = true;
                emitObjectiveDeduped("objective_patron_weakened", {
                    detection: "objective_health:is_titan+is_transforming"
                });
            }
            if (!entry.dead && objHasClass(w, "is_dead")) {
                if (entry.type === "base_guardian") {
                    entry.dead = true;
                    emitObjectiveDeduped("objective_base_guardian", {
                        detection: "objective_health:is_barracks_boss+is_dead"
                    });
                } else if (entry.type === "shrine") {
                    entry.dead = true;
                    emitObjectiveDeduped("objective_shrine", {
                        detection: "objective_health:is_shield_generator+is_dead"
                    });
                }
            }
        }
    }

    function prunedValidPanelEntries(entries) {
        var kept = [];
        for (var i = 0; i < entries.length; i++) {
            if (isValidPanel(entries[i].panel)) {
                kept.push(entries[i]);
            }
        }
        return kept;
    }

    // The direct children of #ObjectivesFeed that are boss/structure kill rows
    // (citadel_hud_data_feed_boss_killed -> paneltype "HudBossKilled", with a
    // .killerContainer and a .victimContainer). #ObjectivesFeed can also hold
    // plain team-message rows, which have neither and are skipped.
    function objectiveFeedRows(feed) {
        var rows = [];
        var children = panelChildren(feed);
        for (var i = 0; i < children.length; i++) {
            var row = children[i];
            if (!isValidPanel(row)) {
                continue;
            }
            if (panelProperty(row, "paneltype") === "HudBossKilled"
                || (findChildrenWithClass(row, "killerContainer").length > 0
                    && findChildrenWithClass(row, "victimContainer").length > 0)) {
                rows.push(row);
            }
        }
        return rows;
    }

    function rowHasClassDeep(row, className) {
        return panelHasClass(row, className)
            || findChildrenWithClass(row, className).length > 0;
    }

    // "friend" when the row credits the local player's team with the kill (so
    // an enemy structure fell), "enemy" when the enemy cleared one of ours, or
    // null when it cannot be told -- in which case the caller skips the row
    // rather than guess. `friendlyTeam` is "1"/"2"/null from the objectives
    // map; the .killerFriend/.killerEnemy classes are preferred when present.
    function objectiveFeedKillerSide(row, friendlyTeam) {
        if (rowHasClassDeep(row, "killerFriend")) { return "friend"; }
        if (rowHasClassDeep(row, "killerEnemy")) { return "enemy"; }
        if (friendlyTeam !== null) {
            var enemyTeam = friendlyTeam === "1" ? "2" : "1";
            if (rowHasClassDeep(row, "killerTeam" + friendlyTeam)) { return "friend"; }
            if (rowHasClassDeep(row, "killerTeam" + enemyTeam)) { return "enemy"; }
        }
        return null;
    }

    function objectiveRowImageHint(row) {
        var images = findChildrenWithClass(row, "entityImage");
        for (var i = 0; i < images.length; i++) {
            var src = panelProperty(images[i], "src")
                || panelAttribute(images[i], "src")
                || panelProperty(images[i], "image");
            if (typeof src === "string" && src !== "") {
                return src.toLowerCase();
            }
        }
        return "";
    }

    function objectiveRowTextHint(row) {
        var text = "";
        var labels = findChildrenWithClass(row, "personaName");
        if (labels.length === 0) {
            labels = findChildrenWithClass(row, "victimInfo");
        }
        for (var i = 0; i < labels.length; i++) {
            var value = panelProperty(labels[i], "text");
            if (typeof value === "string") {
                text += " " + value;
            }
        }
        return text.toLowerCase();
    }

    // "base_guardian" | "shrine" | "walker" | "guardian" | null. Explicit type
    // class first (future-proof), then the victim image src, then the victim
    // label text. "base guardian" is matched before the bare "guardian" so it
    // is not misfiled.
    function classifyObjectiveFeedRow(row) {
        for (var i = 0; i < OBJECTIVE_FEED_TYPE_CLASSES.length; i++) {
            if (rowHasClassDeep(row, OBJECTIVE_FEED_TYPE_CLASSES[i][0])) {
                return OBJECTIVE_FEED_TYPE_CLASSES[i][1];
            }
        }
        var hint = objectiveRowImageHint(row) + " " + objectiveRowTextHint(row);
        if (hint.indexOf("barrack") !== -1
            || hint.indexOf("base guardian") !== -1
            || hint.indexOf("base_guardian") !== -1) {
            return "base_guardian";
        }
        if (hint.indexOf("shield_gen") !== -1 || hint.indexOf("shield gen") !== -1
            || hint.indexOf("shrine") !== -1 || hint.indexOf("generator") !== -1) {
            return "shrine";
        }
        if (hint.indexOf("walker") !== -1 || hint.indexOf("tier2") !== -1
            || hint.indexOf("tier_2") !== -1) {
            return "walker";
        }
        if (hint.indexOf("guardian") !== -1 || hint.indexOf("tier1") !== -1
            || hint.indexOf("tier_1") !== -1) {
            return "guardian";
        }
        return null;
    }

    // Position-independent Base Guardian / Shrine detection off #ObjectivesFeed.
    // Guardians and Walkers are left to the objectives-map diff, which already
    // sees them cleanly and map-wide; the feed only fills the bar's blind spot.
    function pollObjectiveFeed(root, friendlyTeam) {
        var feed = findCachedChildById(root, OBJECTIVE_FEED_ID);
        if (!isValidPanel(feed)) {
            return;
        }
        creditedObjectiveFeedPanels = prunedValidPanels(creditedObjectiveFeedPanels);

        var rows = objectiveFeedRows(feed);
        for (var i = 0; i < rows.length; i++) {
            var row = rows[i];
            if (creditedObjectiveFeedPanels.indexOf(row) !== -1) {
                continue;
            }
            creditedObjectiveFeedPanels.push(row);

            // A row seen before the objectives map has baselined is almost
            // certainly a stale one from before the mod (re)loaded.
            if (!objectivesBaselined) {
                continue;
            }
            if (rowHasClassDeep(row, "midBoss")) {
                continue; // the mid boss is not one of the objective triggers
            }
            if (objectiveFeedKillerSide(row, friendlyTeam) !== "friend") {
                if (feedDiagBudget > 0) {
                    feedDiagBudget--;
                    emit("objective_feed_diag", {
                        friendly_team: friendlyTeam,
                        image_hint: objectiveRowImageHint(row),
                        text_hint: objectiveRowTextHint(row).replace(/^s+/, ""),
                        killer_side: objectiveFeedKillerSide(row, friendlyTeam)
                    });
                }
                continue; // only your team clearing an enemy structure counts
            }

            var kind = classifyObjectiveFeedRow(row);
            if (kind === null) {
                // Best-effort: a boss row we could not type. A non-trigger
                // diagnostic (no sequence) so the raw hints reach console.log
                // for tuning the matchers above.
                emit("objective_feed_unclassified", {
                    detection: "objectives_feed:boss_killed",
                    image_hint: objectiveRowImageHint(row),
                    text_hint: objectiveRowTextHint(row).replace(/^\s+/, "")
                });
                continue;
            }
            if (kind !== "base_guardian" && kind !== "shrine") {
                continue; // guardian/walker already covered by the map diff
            }
            emitObjectiveDeduped(OBJECTIVE_FEED_EVENT[kind], {
                detection: "objectives_feed:boss_killed"
            });
        }
    }

    function pollMatchEnd(root) {
        if (gameWonEmitted || gameLostEmitted) {
            return;
        }
        var ends = findChildrenWithClass(root, "ShowMatchEnd");
        for (var i = 0; i < ends.length; i++) {
            var p = ends[i];
            if (!isValidPanel(p) || panelHasClass(p, "MatchAbandoned")) {
                continue;
            }
            var won =
                (panelHasAnyClass(p, ["LocalPlayerTeam1", "localPlayerTeam1"])
                    && panelHasClass(p, "Team1Victory"))
                || (panelHasAnyClass(p, ["LocalPlayerTeam2", "localPlayerTeam2"])
                    && panelHasClass(p, "Team2Victory"));
            if (won) {
                gameWonEmitted = true;
                emitAction("game_won", { detection: "match_end:local_team_victory" });
                return;
            }
            var lost =
                (panelHasAnyClass(p, ["LocalPlayerTeam1", "localPlayerTeam1"])
                    && panelHasClass(p, "Team2Victory"))
                || (panelHasAnyClass(p, ["LocalPlayerTeam2", "localPlayerTeam2"])
                    && panelHasClass(p, "Team1Victory"));
            if (lost) {
                gameLostEmitted = true;
                emitAction("game_lost", { detection: "match_end:enemy_team_victory" });
                return;
            }
        }
    }

    // Shape dump of #ObjectivesMap: every panel that has an id, with the
    // classes that matter, logged whenever the set changes. Base Guardians,
    // Shrines and the Patron have no styled panel in the shipped map, so this
    // shows what (if anything) it exposes for them during a real match.
    var MAP_DIAG_PROBES = [
        "Alive", "Dead", "Titan", "Core", "Icon", "Team1", "Team2",
        "Team1IsEnemy", "Team2IsEnemy", "Team1IsFriend", "Team2IsFriend",
        "Weakened", "Invulnerable", "Underattack"
    ];
    var mapDiagBudget = 60;
    var mapDiagSig = "";
    var mapDiagLastAt = 0;
    var MAP_DIAG_MIN_MS = 2000;

    function collectMapPanels(node, depth, out) {
        if (depth > 4 || out.length >= 80) {
            return;
        }
        var kids = panelChildren(node);
        for (var i = 0; i < kids.length && out.length < 80; i++) {
            var kid = kids[i];
            if (!isValidPanel(kid)) {
                continue;
            }
            var id = panelProperty(kid, "id");
            if (typeof id === "string" && id !== "") {
                var hit = [];
                for (var p = 0; p < MAP_DIAG_PROBES.length; p++) {
                    if (panelHasClass(kid, MAP_DIAG_PROBES[p])) {
                        hit.push(MAP_DIAG_PROBES[p]);
                    }
                }
                out.push(id + "[" + hit.join(",") + "]");
            }
            collectMapPanels(kid, depth + 1, out);
        }
    }

    function pollObjectivesMapDiag(objMap) {
        if (mapDiagBudget <= 0) {
            return;
        }
        var now = Date.now();
        if (now - mapDiagLastAt < MAP_DIAG_MIN_MS) {
            return;
        }
        mapDiagLastAt = now;
        var out = [];
        collectMapPanels(objMap, 0, out);
        var sig = out.join(" ");
        if (sig === mapDiagSig) {
            return;
        }
        mapDiagSig = sig;
        mapDiagBudget--;
        emit("objectives_map_diag", { count: out.length, panels: sig });
    }

    // Base Guardians have no objective_ bar, but the map has a map_button with
    // class boss_barracks_icon for each. A live enemy one carries "active" and
    // "enemy"; when the guardian dies "active" drops off the same panel.
    // The Patron phase 1 icon (boss_icon_t3) is handled the same way, and
    // boss_buildingzip is only logged until its meaning is confirmed.
    var MAP_ICON_KINDS = [
        { cls: "boss_barracks_icon", event: "objective_base_guardian", detection: "map:boss_barracks_icon_active_cleared" },
        { cls: "boss_icon_t3", event: "objective_patron_weakened", detection: "map:boss_icon_t3_active_cleared" },
        { cls: "boss_buildingzip", event: null, detection: "" }
    ];
    var barracksPanels = [];   // [{panel, kind, wasActive, dead}]
    var barracksScanPolls = 0;
    var barracksDiagBudget = 20;

    function pollBarracksMap(root) {
        barracksPanels = prunedValidPanelEntries(barracksPanels);
        if (barracksScanPolls++ % 5 === 0) {
            for (var q = 0; q < MAP_ICON_KINDS.length; q++) {
                var found = findChildrenWithClass(root, MAP_ICON_KINDS[q].cls);
                for (var f = 0; f < found.length; f++) {
                    var known = false;
                    for (var k = 0; k < barracksPanels.length; k++) {
                        if (barracksPanels[k].panel === found[f]) { known = true; break; }
                    }
                    if (!known) {
                        barracksPanels.push({ panel: found[f], kind: MAP_ICON_KINDS[q], wasActive: false, dead: false });
                    }
                }
            }
        }
        for (var i = 0; i < barracksPanels.length; i++) {
            var e = barracksPanels[i];
            var p = e.panel;
            var active = panelHasClass(p, "active");
            var enemyish = panelHasClass(p, "enemy");
            if (active && !e.wasActive && barracksDiagBudget > 0) {
                barracksDiagBudget--;
                emit("barracks_map_diag", { kind: e.kind.cls, enemy: enemyish, classes: indicatorClassList(p) });
            }
            if (active) {
                e.wasActive = true;
                continue;
            }
            if (e.wasActive && !e.dead && enemyish && e.kind.event !== null) {
                e.dead = true;
                // Each panel fires once (e.dead), so several Base Guardians dying
                // close together each count. Only the single Patron icon dedupes,
                // against its HUD-bar path.
                if (e.kind.cls === "boss_barracks_icon") {
                    emitAction(e.kind.event, { detection: e.kind.detection });
                } else {
                    emitObjectiveDeduped(e.kind.event, { detection: e.kind.detection });
                }
            }
        }
    }

    function pollObjectives(root) {
        if (!isValidPanel(root)) {
            return;
        }
        objectivePollCounter++;
        if (objectivePollCounter < OBJECTIVE_POLL_INTERVAL_POLLS) {
            return;
        }
        objectivePollCounter = 0;

        // One bad panel read here must not take down the poll loop.
        try {
            pollMatchEnd(root);
            pollObjectiveHealth(root);
            pollBarracksMap(root);

            var objMap = findCachedChildById(root, OBJECTIVE_MAP_ID);
            var enemy = isValidPanel(objMap) ? objectiveEnemyTeam(objMap) : null;
            pollObjectiveFeed(root, enemy === null ? null : (enemy === "1" ? "2" : "1"));
            if (isValidPanel(objMap)) {
                pollObjectivesMapDiag(objMap);
            }

            if (!isValidPanel(objMap)) {
                return;
            }
            if (enemy === null) {
                return; // can't attribute yet; don't baseline a half-built map
            }
            var i;
            for (i = 0; i < OBJECTIVE_GUARDIAN_SUFFIXES.length; i++) {
                diffObjectiveAlive(
                    objMap, "Team" + enemy + OBJECTIVE_GUARDIAN_SUFFIXES[i],
                    "objective_guardian", "objectives_map:tier1_alive_cleared"
                );
            }
            for (i = 0; i < OBJECTIVE_WALKER_SUFFIXES.length; i++) {
                diffObjectiveAlive(
                    objMap, "Team" + enemy + OBJECTIVE_WALKER_SUFFIXES[i],
                    "objective_walker", "objectives_map:tier2_alive_cleared"
                );
            }
            if (!gameWonEmitted) {
                var coreId = "Team" + enemy + "Core";
                var core = findChildById(objMap, coreId);
                if (isValidPanel(core)) {
                    var coreAlive = panelHasClass(core, "Alive");
                    var corePrev = objectiveAliveState[coreId];
                    objectiveAliveState[coreId] = coreAlive;
                    if (objectivesBaselined && corePrev === true && coreAlive === false) {
                        gameWonEmitted = true;
                        emitAction("game_won", {
                            detection: "objectives_map:enemy_core_alive_cleared"
                        });
                    }
                }
            }
            objectivesBaselined = true;
        } catch (_error) {
        }
    }

    // ---------------------------------------------------------------
    // Vitals: health & shields
    //
    // The top bar labels current health and, separately, each shield bar's
    // current value as plain text - the same mechanism the kill-streak
    // counter reads from, just different classes. Health plus both shields
    // is tracked as one combined total; a poll-over-poll drop is damage
    // taken, a rise is healing received (which also covers a shield
    // regenerating or a teammate topping one up). Resolved label panels are
    // cached, since re-searching the whole HUD tree on every 100ms poll
    // would be far too expensive - a cache entry is only dropped, and the
    // search retried, after it has gone stale for a run of consecutive
    // misses, mirroring the poll-count throttling used elsewhere in this
    // file rather than a wall-clock timer.
    // ---------------------------------------------------------------

    var VITALS_LABEL_RETRY_MISSES = 20; // re-search after this many stale polls
    // Reading three labels every single 100ms poll (each a native panel
    // property access, the expensive part of Panorama script work) is more
    // than this needs: deltas are only ever flushed every VITALS_FLUSH_EVERY_
    // POLLS anyway, so the reads themselves are throttled to the same
    // cadence instead of running 3x more often than their result is used.
    var VITALS_POLL_INTERVAL_POLLS = 3; // ~0.3s at POLL_INTERVAL_SECONDS = 0.1
    var vitalsPollCounter = 0;

    var HEALTH_LABEL_CLASSES = ["currentHealthLabel", "current_health"];
    var SHIELD_CONTAINER_CLASSES = {
        bulletShield: ["BulletShieldNumbers"],
        techShield: ["TechShieldNumbers"]
    };

    var vitalsLabelCache = {};

    function resolveVitalsPanel(root, key, finder) {
        var entry = vitalsLabelCache[key];
        if (entry && isValidPanel(entry.panel)) {
            return entry.panel;
        }
        if (entry && entry.misses < VITALS_LABEL_RETRY_MISSES) {
            entry.misses++;
            return null;
        }
        var panel = finder(root);
        vitalsLabelCache[key] = { panel: panel, misses: 0 };
        return panel;
    }

    function readNumericLabel(panel) {
        if (!isValidPanel(panel)) {
            return null;
        }
        var text = panelProperty(panel, "text");
        if (text === null) {
            return null;
        }
        var value = Number(text);
        return isFinite(value) ? value : null;
    }

    function findFirstWithAnyClass(root, classNames) {
        for (var i = 0; i < classNames.length; i++) {
            var matches = findChildrenWithClass(root, classNames[i]);
            if (matches.length > 0) {
                return matches[0];
            }
        }
        return null;
    }

    function findShieldValuePanel(containerClassNames) {
        return function (root) {
            var container = findFirstWithAnyClass(root, containerClassNames);
            if (!isValidPanel(container)) {
                return null;
            }
            var bar = findChildrenWithClass(container, "progress_bar_current");
            return bar.length > 0 ? bar[0] : null;
        };
    }

    function readHealth(root) {
        var panel = resolveVitalsPanel(root, "health", function (r) {
            return findFirstWithAnyClass(r, HEALTH_LABEL_CLASSES);
        });
        return readNumericLabel(panel);
    }

    function readShield(root, key) {
        var panel = resolveVitalsPanel(root, key, findShieldValuePanel(SHIELD_CONTAINER_CLASSES[key]));
        return readNumericLabel(panel);
    }

    var vitalsState = {
        total: null,
        lost: 0,
        gained: 0
    };

    function resetVitalsState() {
        vitalsState.total = null;
        vitalsState.lost = 0;
        vitalsState.gained = 0;
        vitalsLabelCache = {};
        vitalsPollCounter = 0;
    }

    function flushVitalsDeltas(health) {
        if (vitalsState.lost > 0) {
            emitAction("damage_taken", {
                amount: vitalsState.lost,
                health: health,
                detection: "top_bar_health_and_shield_labels"
            });
            vitalsState.lost = 0;
        }
        if (vitalsState.gained > 0) {
            emitAction("healing_received", {
                amount: vitalsState.gained,
                health: health,
                detection: "top_bar_health_and_shield_labels"
            });
            vitalsState.gained = 0;
        }
    }

    function pollVitals(root, rebaseline) {
        if (!rebaseline) {
            vitalsPollCounter++;
            if (vitalsPollCounter < VITALS_POLL_INTERVAL_POLLS) {
                return;
            }
            vitalsPollCounter = 0;
        }

        var health = readHealth(root);
        if (health === null) {
            vitalsState.total = null;
            return;
        }
        var bulletShield = readShield(root, "bulletShield") || 0;
        var techShield = readShield(root, "techShield") || 0;
        var total = health + bulletShield + techShield;

        if (rebaseline || vitalsState.total === null) {
            vitalsState.total = total;
            vitalsState.lost = 0;
            vitalsState.gained = 0;
            return;
        }

        var delta = total - vitalsState.total;
        vitalsState.total = total;
        if (delta < 0) {
            vitalsState.lost += -delta;
        } else if (delta > 0) {
            vitalsState.gained += delta;
        }

        if (vitalsState.lost > 0 || vitalsState.gained > 0) {
            flushVitalsDeltas(health);
        }
    }

    // ---------------------------------------------------------------
    // Death & respawn
    // ---------------------------------------------------------------

    function findLocalPlayerPanel() {
        if (isValidPanel(localPlayerPanel)) {
            return localPlayerPanel;
        }

        localPlayerPanel = null;
        deathBaselineEstablished = false;
        lastKillStreakCount = null;
        childByIdCache = {};
        resetVitalsState();
        resetAllySupportState();
        resetCombatState();
        resetObjectiveState();
        // The top-bar player panel is recreated between matches, so a stale
        // identity from a previous game (possibly a different hero) must not
        // linger and suppress every trigger in the next one.
        localHeroIdentity = null;

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
        // Resolved once per poll and threaded through every consumer: walking to
        // the HUD root climbs up to 64 parents, and this ran it 3-4 times.
        var hudRoot = highestContextAncestor();
        var currentAbilityRoot = findAbilityRoot(hudRoot);

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
            } else {
                // Plain emit (no sequence): the companion treats respawn as a
                // state signal, not a trigger, so it must not consume a slot in
                // the monotonic trigger sequence.
                emit("local_player_respawn", {});
                respawnVitalsSettlePolls = RESPAWN_VITALS_SETTLE_POLLS;
            }
            wasDead = isDead;
        }

        // Learn the local player's own hero once the match has settled and they
        // are alive (and therefore in control of their own HUD).
        if (!isDead && baselineSettlePollsRemaining === 0 && localHeroIdentity === null) {
            var ownIdentity = heroIdentity(currentAbilityRoot);
            if (ownIdentity !== null) {
                localHeroIdentity = ownIdentity;
            }
        }
        var spectating = !controllingOwnHero(currentAbilityRoot);

        if (respawnVitalsSettlePolls > 0 && !isDead) {
            respawnVitalsSettlePolls--;
        }
        var rebaselineVitals = forceAbilityBaseline || baselineSettlePollsRemaining > 0
            || isDead || respawnVitalsSettlePolls > 0;
        // A bad panel read in any one watcher must never kill the poll loop
        // (which also carries death detection). pollCombat keeps its own inner
        // catch so a combat throw doesn't skip the polls listed after it.
        try {
            pollAbilities(forceAbilityBaseline || isDead || spectating, currentAbilityRoot);
            pollOwnKillStreak(player, forceAbilityBaseline || baselineSettlePollsRemaining > 0);
            pollVitals(hudRoot, rebaselineVitals);
            // Objectives and match end are team/match-level state, valid whether
            // or not you are on your own hero, so they run outside the gate.
            pollObjectives(hudRoot);
            if (!spectating) {
                pollDamageImpactAssists(hudRoot);
                pollAllySupport(hudRoot);
                pollSoulDeny(hudRoot);
                pollCombat(hudRoot, rebaselineVitals);
            }
        } catch (_error) {
        }
        $.Schedule(POLL_INTERVAL_SECONDS, pollState);
    }

    emit("hook_ready", {
        poll_interval_ms: POLL_INTERVAL_SECONDS * 1000
    });
    pollState();
})();
