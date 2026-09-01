// lock_ok — poison-recovering lock helpers for the module-static
// Mutex/RwLock tables (game.rs auditor finding, W6 crash hardening).
//
// Every per-subsystem static (OLC editors, pagers, socials, DG script memory,
// auction/ban/alias/questgiver tables, ...) is Game-task-owned state, so a
// panic while holding its guard poisons the lock but leaves the data intact.
// A `lock().unwrap()` on the poisoned lock then re-panics on EVERY later
// touch — inside catch_unwind that quietly bricks the subsystem (the
// heartbeat keeps counting while mob pulses / editor input / auctions all
// no-op forever), and outside it kills the Game task. Weather's clock already
// recovered via `into_inner()`; these helpers make that the uniform policy.

/// Lock a std Mutex, recovering the data from a poisoned guard.
pub fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read-lock a std RwLock, recovering from poison.
pub fn read<T>(m: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write-lock a std RwLock, recovering from poison.
pub fn write<T>(m: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}
