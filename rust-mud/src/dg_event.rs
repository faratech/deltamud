// dg_event.rs — the DG-Scripts wait/event queue (port of dg_event.c).
//
// The C file is "a simplified event system to allow DG Script triggers to use
// the `wait` command, causing a delay in the middle of a script." It is NOT
// the full Circle event package — it is a single sorted list of events, each
// with a `time_remaining` countdown (in pulses) and a callback. Every pulse,
// `process_events()` decrements each event; an event that reaches 0 fires.
//
// In C the event payload is a `void *info` that the trigger system fills with
// a `struct wait_event_data { trig_data*, go, type }`. We cannot pass raw
// pointers, so the queue stores a `WaitEvent { go: GoRef, trig: TrigId }`
// value (the only kind of event DG ever schedules) and, on fire, hands it back
// to dg_scripts::trig_wait_event which resumes the paused trigger.
//
// Borrow discipline: the wait queue lives on GameState (`dg.events`) with a
// monotonic id source (`dg.next_event_id`). Firing happens in
// `process_events(g)` which pops the due
// events under the lock, releases the lock, then drives each one against
// `&mut GameState`. This avoids re-entrancy deadlocks when a resumed trigger
// schedules a *new* wait (it re-locks cleanly because we are not holding it).

use crate::dg_handler::TrigId;
use crate::dg_scripts::{self, GoRef};
use crate::state::GameState;

/// Opaque handle to a scheduled wait event. Stored on a trigger's runtime
/// state (TRIG_WAIT in C) so it can be cancelled when the trigger is extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventId(pub u64);

/// The payload of a DG wait event: which trigger to resume and on what game
/// object, plus its trigger-type (MOB/OBJ/WLD). Mirrors `wait_event_data`.
#[derive(Debug, Clone, Copy)]
pub struct WaitEvent {
    pub go: GoRef,
    pub trig: TrigId,
    pub trig_type: i32,
}

pub struct EventInfo {
    pub id: EventId,
    pub time_remaining: i64,
    pub payload: WaitEvent,
}

/// Clear the event queue (called from boot to drop any stale events across a
/// world reload / copyover, paralleling a fresh `event_list = NULL`).
pub fn boot_events(g: &mut GameState) {
    g.dg.events.clear();
}

/// add_event(time, func, info): schedule `payload` to fire in `time` pulses.
/// C inserts into the list in next-to-fire order; we keep the same invariant
/// (the list is sorted ascending by `time_remaining`) so process_events can
/// stop early — though for correctness we simply scan, exactly like C does on
/// each pulse. `time` <= 0 is clamped to 1 so the event still fires next pulse
/// (C's `--time_remaining == 0` would otherwise wrap negative and never fire).
pub fn add_event(g: &mut GameState, time: i64, payload: WaitEvent) -> EventId {
    let id = EventId(g.dg.next_event_id);
    g.dg.next_event_id += 1;
    let time_remaining = if time < 1 { 1 } else { time };
    let info = EventInfo {
        id,
        time_remaining,
        payload,
    };

    let list = &mut g.dg.events;
    // Sorted insert in next-to-fire order (C add_event).
    let pos = list
        .iter()
        .position(|e| e.time_remaining > time_remaining)
        .unwrap_or(list.len());
    list.insert(pos, info);
    id
}

/// remove_event(event): cancel a scheduled event by id. Safe if absent.
pub fn remove_event(g: &mut GameState, id: EventId) {
    g.dg.events.retain(|e| e.id != id);
}

/// process_events(): called once per pulse. Decrement every event's counter;
/// fire (and remove) those that reach 0. C fires inside the walk; we collect
/// the due payloads first (releasing the lock) so a resumed trigger can freely
/// schedule new waits without deadlocking on the same mutex.
pub fn process_events(g: &mut GameState) {
    let due: Vec<WaitEvent> = {
        let list = &mut g.dg.events;
        let mut fired = Vec::new();
        // Decrement counters; gather the ones that hit zero.
        for e in list.iter_mut() {
            e.time_remaining -= 1;
        }
        let mut i = 0;
        while i < list.len() {
            if list[i].time_remaining <= 0 {
                fired.push(list.remove(i).payload);
            } else {
                i += 1;
            }
        }
        fired
    };

    for payload in due {
        dg_scripts::trig_wait_event(g, payload);
    }
}

/// Cancel the wait event (if any) currently parked on a trigger's runtime
/// state. Used by extract_trigger so a purged trigger never resumes.
pub fn cancel_for_trigger(g: &mut GameState, maybe: Option<EventId>) {
    if let Some(id) = maybe {
        remove_event(g, id);
    }
}
