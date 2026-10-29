//! The route-event census: one door asks the capability's dispatch pair.
//!
//! `ConcentratedLiquidityOrchestration::route_event` and
//! `route_event_by_key` are the concentrated-liquidity family's two decision
//! points (one per key shape), and `BotState::route_event` is the ONE door that
//! asks them: it owns the family-vs-key agreement check and returns the
//! router's verdict whole.
//!
//! An apply door that re-wraps a capability call in its own `Ok(..)` is a
//! parallel dispatch: it skips that check while looking exactly like a door
//! that goes through it. This census pins the invariant at the source level —
//! each capability call appears exactly once in the orchestration file, and
//! that occurrence is inside `BotState::route_event`'s body.
//!
//! The count is taken over whitespace-condensed text because the line breaks
//! are rustfmt's call, not the invariant's: the door's V3 arm is one chain the
//! formatter splits after `.concentrated_liquidity`, which would hide the call
//! from a line-literal needle while the dispatch is still there.
//!
//! Scope is the orchestration source alone: `log_dispatcher.rs` used to be the
//! deliberate exception (the bench-gated hot path), and now that its arms go
//! through the door too, no caller outside this file asks the capability pair.
//!
//! The gate is a pure function over source text so its failure mode is
//! testable: a synthetic fixture with a second caller must go red (the
//! negative-probe idiom every census gate in this tree carries).

const ORCHESTRATION: &str = include_str!("../concentrated_liquidity_orchestration.rs");

/// The capability pair, each of which may be asked from exactly one place.
const CAPABILITY_CALLS: &[&str] = &[
    "concentrated_liquidity.route_event(",
    "concentrated_liquidity.route_event_by_key(",
];

/// The door: `BotState`'s single route-event entry (the capability's own
/// methods are `pub(crate)`, so this signature names the door and nothing else).
const DOOR: &str = "pub fn route_event(";

/// Source text with every whitespace run removed, so a needle matches the
/// tokens however the formatter laid the line out.
fn condensed(source: &str) -> String {
    source.split_whitespace().collect()
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The body of the function introduced by `signature`, braces included —
/// `None` when the function is absent or its braces never balance.
fn body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let start = source.find(signature)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, byte) in source[open..].char_indices() {
        match byte {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[start..=open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Violations across one source: a capability call asked from somewhere other
/// than the door, asked more than once inside it, or never asked at all — and
/// a missing door itself.
fn census(source: &str) -> Vec<String> {
    let Some(door) = body(source, DOOR) else {
        return vec![
            "no BotState::route_event door: the capability pair must be asked from one place"
                .to_string(),
        ];
    };
    let source = condensed(source);
    let door = condensed(door);
    let mut violations = Vec::new();
    for call in CAPABILITY_CALLS {
        let total = occurrences(&source, call);
        let inside = occurrences(&door, call);
        if total != inside {
            violations.push(format!(
                "{call} has {} asker(s) outside BotState::route_event (file: {total}, door: {inside})",
                total - inside
            ));
        }
        if inside != 1 {
            violations.push(format!(
                "{call} is asked {inside} time(s) inside BotState::route_event, expected 1"
            ));
        }
    }
    violations
}

/// live-invariant gate (ADR-087 D2): watches the live `route_event` capability callers in `concentrated_liquidity_orchestration.rs` against the one-door census; not tripwire precedent.
#[test]
fn the_route_event_census_is_clean() {
    assert_eq!(census(ORCHESTRATION), Vec::<String>::new());
}

/// The door over both key shapes, each capability call asked once — written in
/// the layout rustfmt produces (the V3 chain split after
/// `.concentrated_liquidity`), so the fixtures below also prove the census
/// reads through the formatter's line breaks.
fn door_fixture() -> String {
    [
        "    pub fn route_event(\n",
        "        &mut self,\n",
        "        key: PoolLookup,\n",
        "        event: BufferedPoolEvent,\n",
        "    ) -> Result<ApplyOutcome, PoolRefusal> {\n",
        "        match (key, event) {\n",
        "            (PoolLookup::Address(pool_address), BufferedPoolEvent::V3(event)) => Ok(self\n",
        "                .concentrated_liquidity\n",
        "                .route_event(&mut self.pools, phase, pool_address, event, swap_priors)),\n",
        "            (\n",
        "                PoolLookup::V4 {\n",
        "                    pool_manager,\n",
        "                    pool_id,\n",
        "                },\n",
        "                BufferedPoolEvent::V4(event),\n",
        "            ) => Ok(self.concentrated_liquidity.route_event_by_key(\n",
        "                &mut self.pools,\n",
        "                phase,\n",
        "                pool_manager,\n",
        "                pool_id,\n",
        "                event,\n",
        "                swap_priors,\n",
        "            )),\n",
        "            (key, event) => Err(PoolRefusal::FamilyMismatch {\n",
        "                key: key.family(),\n",
        "                value: event.family(),\n",
        "            }),\n",
        "        }\n",
        "    }\n",
    ]
    .concat()
}

#[test]
fn the_census_goes_red_on_a_second_caller() {
    let door = door_fixture();
    assert_eq!(
        census(&door),
        Vec::<String>::new(),
        "the door alone is clean"
    );

    // The retired shape, byte-for-byte what the swap doors carried: each one
    // re-wrapped its own capability call in a parallel dispatch.
    let second_callers = [
        "    pub fn apply_swap(&mut self) -> Result<ApplyOutcome, PoolRefusal> {\n",
        "        Ok(self.concentrated_liquidity.route_event(\n",
        "            &mut self.pools,\n",
        "            Phase::Live,\n",
        "        ))\n",
        "    }\n",
        "\n",
        "    pub fn apply_swap_update(&mut self) -> Result<ApplyOutcome, PoolRefusal> {\n",
        "        Ok(self.concentrated_liquidity.route_event_by_key(\n",
        "            &mut self.pools,\n",
        "            Phase::Live,\n",
        "        ))\n",
        "    }\n",
    ]
    .concat();
    let red = census(&[door.as_str(), second_callers.as_str()].concat());
    for call in CAPABILITY_CALLS {
        assert!(
            red.iter()
                .any(|v| v.contains(call) && v.contains("outside BotState::route_event")),
            "the census must catch the second caller for {call}: {red:?}"
        );
    }

    // A call the formatter broke immediately after the capability field is the
    // same dispatch, so it must not slip past the census.
    let split_caller = [
        "    pub fn apply_swap(&mut self) -> Result<ApplyOutcome, PoolRefusal> {\n",
        "        Ok(self\n",
        "            .concentrated_liquidity\n",
        "            .route_event(&mut self.pools, Phase::Live))\n",
        "    }\n",
    ]
    .concat();
    let split_red = census(&[door.as_str(), split_caller.as_str()].concat());
    assert!(
        split_red
            .iter()
            .any(|v| v.contains("concentrated_liquidity.route_event(")
                && v.contains("outside BotState::route_event")),
        "a formatter-split second caller is still a second caller: {split_red:?}"
    );

    // A source with no door at all: the pair has nowhere sanctioned to live.
    let doorless = census(&second_callers);
    assert!(
        doorless
            .iter()
            .any(|v| v.contains("no BotState::route_event door")),
        "the census must report the missing door: {doorless:?}"
    );

    // The door must actually ask both: a door that stopped dispatching leaves
    // the capability pair unreachable, which is just as red as a second caller.
    let silent_door = door.replace(".route_event(", ".route_elsewhere(");
    let silent = census(&silent_door);
    assert!(
        silent
            .iter()
            .any(|v| v.contains("is asked 0 time(s) inside BotState::route_event")),
        "the census must report a capability call the door stopped asking: {silent:?}"
    );
}
