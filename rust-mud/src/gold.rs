//! Checked carried/bank gold mutations.
//!
//! Gameplay code must not perform arithmetic directly on persisted i32 gold
//! fields. All calculations here use i64 and enforce one shared non-negative
//! one-billion-coin ceiling.

use crate::character::Character;
use crate::state::GameState;
use crate::types::{CharId, Gold};

pub const GOLD_CAP: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Account {
    Carried,
    Bank,
}

fn slot(ch: &Character, account: Account) -> Gold {
    match account {
        Account::Carried => ch.points.gold,
        Account::Bank => ch.points.bank_gold,
    }
}

fn slot_mut(ch: &mut Character, account: Account) -> &mut Gold {
    match account {
        Account::Carried => &mut ch.points.gold,
        Account::Bank => &mut ch.points.bank_gold,
    }
}

pub fn balance(ch: &Character, account: Account) -> i64 {
    i64::from(slot(ch, account)).clamp(0, GOLD_CAP)
}

pub fn normalize(value: i64) -> Gold {
    value.clamp(0, GOLD_CAP) as Gold
}

pub fn set(ch: &mut Character, account: Account, value: i64) -> Gold {
    let value = normalize(value);
    *slot_mut(ch, account) = value;
    value
}

/// Credit as much of a non-negative amount as fits. Returns the amount applied.
pub fn credit(ch: &mut Character, account: Account, amount: i64) -> i64 {
    let current = balance(ch, account);
    if amount <= 0 {
        set(ch, account, current);
        return 0;
    }
    let next = current.saturating_add(amount).min(GOLD_CAP);
    set(ch, account, next);
    next - current
}

/// Debit the full non-negative amount or leave the balance unchanged.
pub fn debit(ch: &mut Character, account: Account, amount: i64) -> bool {
    let current = balance(ch, account);
    if amount < 0 || amount > current {
        set(ch, account, current);
        return false;
    }
    set(ch, account, current - amount);
    true
}

/// Debit up to the requested amount. Returns the amount actually removed.
pub fn debit_up_to(ch: &mut Character, account: Account, amount: i64) -> i64 {
    let current = balance(ch, account);
    let removed = amount.max(0).min(current);
    set(ch, account, current - removed);
    removed
}

/// Atomically move a balance between two accounts on one character.
pub fn transfer(ch: &mut Character, from: Account, to: Account, amount: i64) -> bool {
    if amount < 0 {
        return false;
    }
    if from == to {
        let current = balance(ch, from);
        set(ch, from, current);
        return current >= amount;
    }
    let source = balance(ch, from);
    let destination = balance(ch, to);
    if amount > source || destination.saturating_add(amount) > GOLD_CAP {
        return false;
    }
    set(ch, from, source - amount);
    set(ch, to, destination + amount);
    true
}

/// Atomically transfer between characters/accounts after validating both ends.
pub fn transfer_between(
    g: &mut GameState,
    from: CharId,
    from_account: Account,
    to: CharId,
    to_account: Account,
    amount: i64,
) -> bool {
    if from == to {
        return g
            .get_char_mut(from)
            .map(|ch| transfer(ch, from_account, to_account, amount))
            .unwrap_or(false);
    }
    if amount < 0 {
        return false;
    }
    let source = match g.get_char(from) {
        Some(ch) => balance(ch, from_account),
        None => return false,
    };
    let destination = match g.get_char(to) {
        Some(ch) => balance(ch, to_account),
        None => return false,
    };
    if amount > source || destination.saturating_add(amount) > GOLD_CAP {
        return false;
    }
    if let Some(ch) = g.get_char_mut(from) {
        set(ch, from_account, source - amount);
    } else {
        return false;
    }
    if let Some(ch) = g.get_char_mut(to) {
        set(ch, to_account, destination + amount);
        true
    } else {
        if let Some(ch) = g.get_char_mut(from) {
            set(ch, from_account, source);
        }
        false
    }
}

/// Saturating payment that consumes carried coins first, then bank coins.
pub fn debit_carried_then_bank(ch: &mut Character, amount: i64) -> i64 {
    let requested = amount.max(0);
    let carried = debit_up_to(ch, Account::Carried, requested);
    carried + debit_up_to(ch, Account::Bank, requested - carried)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::types::{Class, Race};

    fn player(name: &str) -> Character {
        Character::new_player(name.to_string(), Class::Warrior, Race::Human)
    }

    #[test]
    fn credit_and_debit_enforce_nonnegative_cap_without_i32_overflow() {
        let mut ch = player("Money");
        set(&mut ch, Account::Carried, GOLD_CAP - 5);
        assert_eq!(credit(&mut ch, Account::Carried, i64::MAX), 5);
        assert_eq!(balance(&ch, Account::Carried), GOLD_CAP);
        assert!(!debit(&mut ch, Account::Carried, GOLD_CAP + 1));
        assert_eq!(balance(&ch, Account::Carried), GOLD_CAP);
        assert!(!debit(&mut ch, Account::Carried, -1));
        assert!(debit(&mut ch, Account::Carried, GOLD_CAP));
        assert_eq!(balance(&ch, Account::Carried), 0);
    }

    #[test]
    fn transfer_is_atomic_when_destination_would_exceed_cap() {
        let mut g = GameState::new(Config::default());
        let from = g.create_char(player("From"));
        let to = g.create_char(player("To"));
        set(g.get_char_mut(from).unwrap(), Account::Carried, 50);
        set(g.get_char_mut(to).unwrap(), Account::Bank, GOLD_CAP - 10);

        assert!(!transfer_between(
            &mut g,
            from,
            Account::Carried,
            to,
            Account::Bank,
            20,
        ));
        assert_eq!(balance(g.get_char(from).unwrap(), Account::Carried), 50);
        assert_eq!(
            balance(g.get_char(to).unwrap(), Account::Bank),
            GOLD_CAP - 10
        );
    }
}
