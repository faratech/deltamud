// lock_ok — poison-recovering lock helpers (game.rs auditor finding, W6 crash
// hardening).
//
// POLICY (post phase-1 statics retirement): world state lives on GameState and
// must NEVER be reached through these helpers — a new `OnceLock<Mutex<..>>`
// subsystem table is a design regression and the `static_freedom_gate` test in
// state.rs rejects it. The remaining legitimate callers are genuinely
// cross-task structures (mock database, ban snapshot handle, OLC publication
// lock) plus test guards, where poison recovery is still the right behavior: a
// panic while holding one of those guards leaves the data intact, and a bare
// `lock().unwrap()` would re-panic on every later touch — bricking the
// subsystem inside catch_unwind, or killing the Game task outside it.

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
