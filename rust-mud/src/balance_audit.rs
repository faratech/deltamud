// balance_audit.rs — the balance measuring stick for the Deltania Breathes
// program.
//
// One yardstick for world content and everything the program adds: per player
// level 1..=99 it counts
//
//   * mobs      — loaded mob prototypes at exactly that level,
//   * gear      — object prototypes gated to exactly that level (min_level),
//   * quests    — MOB_QUEST-flagged targets within ±5 levels (the window a
//                 player of that level plausibly gets assigned).
//
// A "dead band" is five consecutive levels with all three columns zero — the
// signature of the shipped world's bimodal curve (zero mobs at L21-25 and
// L27-29, gear ceiling L40, zones 30/31/32 with no loot). `audit()` reports
// them; the ignored real-lib test (`balance_audit_real_lib_gate`) fails on
// any dead band, which is exactly what scripts/balance-check.sh runs. The
// gate lands RED while the curve still has holes and goes green when the
// W4 authoring fills them.
//
// The synthetic-state unit tests keep `cargo test` green without the lib;
// the gate itself is opt-in because it needs the shipped world.

use crate::state::GameState;
use std::collections::HashSet;

/// MOB_QUEST (act flag 1<<19): the mob may be assigned as a quest target.
/// quest.rs keeps the working copy; this mirrors it for prototype auditing.
const MOB_QUEST: i64 = 1 << 19;

/// Quest-target proximity window: a quest mob within ±5 levels of the player.
const QUEST_LEVEL_WINDOW: i32 = 5;

/// How many consecutive levels without a single mob count as a mob dead band.
const MOB_BAND_WIDTH: usize = 5;

/// How many consecutive levels without a new gear unlock count as a gear dead
/// band. Wider than the mob width on purpose: the W4 gear ladder lands in
/// ~10-level rungs (a fresh min_level each rung), and "no new gear unlocked
/// for ten levels" is the actual play-feel hole, not "no gear at this exact
/// level".
const GEAR_BAND_WIDTH: usize = 10;

/// How many consecutive levels without a quest target in range count as a
/// quest dead band.
const QUEST_BAND_WIDTH: usize = 5;

pub const MIN_LEVEL: u8 = 1;
pub const MAX_LEVEL: u8 = 99;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditRow {
    pub level: u8,
    pub mobs: usize,
    pub gear: usize,
    pub quest_targets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditReport {
    pub rows: Vec<AuditRow>,
    /// Inclusive (first, last) level pairs of every dead band, per column.
    pub mob_bands: Vec<(u8, u8)>,
    pub gear_bands: Vec<(u8, u8)>,
    pub quest_bands: Vec<(u8, u8)>,
}

impl AuditReport {
    /// Union of every column's dead bands, for rendering.
    pub fn dead_bands(&self) -> Vec<(u8, u8)> {
        let mut all = self.mob_bands.clone();
        all.extend(self.gear_bands.iter());
        all.extend(self.quest_bands.iter());
        all.sort();
        all.dedup();
        all
    }

    pub fn is_balanced(&self) -> bool {
        self.mob_bands.is_empty() && self.gear_bands.is_empty() && self.quest_bands.is_empty()
    }
}

/// Audit the loaded world's balance curve. Prototypes only — the question is
/// what the WORLD offers at each level, not what is alive right now.
pub fn audit(g: &GameState) -> AuditReport {
    let mut mobs_at = [0usize; 256];
    let mut gear_at = [0usize; 256];
    let mut quest_levels: HashSet<i32> = HashSet::new();

    for proto in g.mob_protos.values() {
        mobs_at[proto.level as usize] += 1;
        if proto.act_flags & MOB_QUEST != 0 {
            quest_levels.insert(proto.level as i32);
        }
    }
    for proto in g.obj_protos.values() {
        if proto.min_level > 0 {
            let lvl = proto.min_level.clamp(0, 255) as usize;
            gear_at[lvl] += 1;
        }
    }

    let mut rows = Vec::with_capacity((MAX_LEVEL - MIN_LEVEL + 1) as usize);
    for level in MIN_LEVEL..=MAX_LEVEL {
        let li = level as i32;
        let quest_targets = quest_levels
            .iter()
            .filter(|&&ql| (ql - li).abs() <= QUEST_LEVEL_WINDOW)
            .count();
        rows.push(AuditRow {
            level,
            mobs: mobs_at[level as usize],
            gear: gear_at[level as usize],
            quest_targets,
        });
    }

    let mob_bands = find_dead_bands(&rows, |r| r.mobs, MOB_BAND_WIDTH);
    let gear_bands = find_dead_bands(&rows, |r| r.gear, GEAR_BAND_WIDTH);
    let quest_bands = find_dead_bands(&rows, |r| r.quest_targets, QUEST_BAND_WIDTH);
    AuditReport {
        rows,
        mob_bands,
        gear_bands,
        quest_bands,
    }
}

/// Contiguous runs of `width`+ zero rows in one column, coalesced into
/// (first, last) pairs.
fn find_dead_bands(
    rows: &[AuditRow],
    column: impl Fn(&AuditRow) -> usize,
    width: usize,
) -> Vec<(u8, u8)> {
    let mut bands = Vec::new();
    let mut run_start: Option<u8> = None;
    let mut run_len = 0usize;
    for row in rows {
        if column(row) == 0 {
            if run_start.is_none() {
                run_start = Some(row.level);
            }
            run_len += 1;
        } else if let Some(first) = run_start.take() {
            if run_len >= width {
                bands.push((first, row.level - 1));
            }
            run_len = 0;
        }
    }
    if let Some(first) = run_start {
        if run_len >= width {
            bands.push((first, MAX_LEVEL));
        }
    }
    bands
}

/// Compact human table (one row per level that has anything, plus the dead
/// bands) for `--nocapture` runs and the runbook.
pub fn render(report: &AuditReport) -> String {
    let mut out = String::from("lvl  mobs  gear  quest\n");
    for row in &report.rows {
        if row.mobs + row.gear + row.quest_targets > 0 {
            out.push_str(&format!(
                "{:>3} {:>5} {:>5} {:>5}\n",
                row.level, row.mobs, row.gear, row.quest_targets
            ));
        }
    }
    if report.dead_bands().is_empty() {
        out.push_str("no dead bands\n");
    } else {
        out.push_str("DEAD BANDS:\n");
        let label = |kind: &str, bands: &[(u8, u8)]| {
            let mut s = String::new();
            for (first, last) in bands {
                s.push_str(&format!("  {} L{}-L{}\n", kind, first, last));
            }
            s
        };
        out.push_str(&label("mobs ", &report.mob_bands));
        out.push_str(&label("gear ", &report.gear_bands));
        out.push_str(&label("quest", &report.quest_bands));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::world::MobileProto;

    fn mob(vnum: i32, level: u8, quest: bool) -> (i32, MobileProto) {
        let mut p = MobileProto {
            vnum,
            name: format!("mob {}", vnum),
            short_desc: String::new(),
            long_desc: String::new(),
            description: String::new(),
            level,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: crate::types::Position::Standing,
            default_pos: crate::types::Position::Standing,
            sex: crate::types::Gender::Neutral,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 0,
            damsizedice: 0,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        };
        if quest {
            p.act_flags |= MOB_QUEST;
        }
        (vnum, p)
    }

    fn obj(vnum: i32, min_level: i32) -> (i32, crate::world::ObjectProto) {
        let o = crate::world::ObjectProto {
            vnum,
            name: format!("obj {}", vnum),
            short_desc: String::new(),
            description: String::new(),
            obj_type: crate::object::ObjectType::Other,
            wear_flags: crate::object::WearFlags::empty(),
            extra_flags: crate::object::ExtraFlags::empty(),
            weight: 1,
            cost: 0,
            rent: 0,
            values: [0; 4],
            curr_slots: 0,
            total_slots: 0,
            obj_class: 0,
            min_level,
            bitvector: 0,
            action_description: String::new(),
            affects: Vec::new(),
            ex_descriptions: Vec::new(),
        };
        (vnum, o)
    }

    fn state_with(
        mobs: Vec<(i32, MobileProto)>,
        objs: Vec<(i32, crate::world::ObjectProto)>,
    ) -> GameState {
        let mut g = GameState::new(Config::default());
        for (vnum, m) in mobs {
            g.mob_protos.insert(vnum, m);
        }
        for (vnum, o) in objs {
            g.obj_protos.insert(vnum, o);
        }
        g
    }

    #[test]
    fn counts_mobs_gear_and_quest_window() {
        let g = state_with(
            vec![mob(100, 5, false), mob(101, 10, true), mob(102, 40, true)],
            vec![obj(500, 10), obj(501, 0)], // min_level 0 = unrestricted, not gear
        );
        let report = audit(&g);
        let row = |l: u8| report.rows.iter().find(|r| r.level == l).unwrap();
        assert_eq!(row(5).mobs, 1);
        assert_eq!(row(40).mobs, 1);
        assert_eq!(row(10).gear, 1);
        // min_level 0 = unrestricted: never counted as gated gear anywhere.
        assert_eq!(report.rows.iter().map(|r| r.gear).sum::<usize>(), 1);
        // L10 quest window sees the L10 quest mob; L40 sees the L40 one.
        assert_eq!(row(10).quest_targets, 1);
        assert_eq!(row(40).quest_targets, 1);
        // L15 is within ±5 of both the L10 quest mob only.
        assert_eq!(row(15).quest_targets, 1);
        assert_eq!(row(16).quest_targets, 0);
    }

    #[test]
    fn mob_dead_band_requires_five_consecutive_empty_levels() {
        let g = state_with(vec![mob(1, 1, false)], vec![]);
        let report = audit(&g);
        // L1 has a mob; everything from 2..=99 is mob-empty — one band.
        assert_eq!(report.mob_bands, vec![(2, 99)]);

        // Breaking the run at L50 splits it.
        let g2 = state_with(vec![mob(1, 1, false), mob(2, 50, false)], vec![]);
        let report2 = audit(&g2);
        assert!(
            report2
                .mob_bands
                .iter()
                .all(|(f, l)| !(*f..=*l).contains(&50))
        );
    }

    #[test]
    fn gear_dead_band_uses_the_ladder_width() {
        // Rungs every five levels: the 4-level gaps between rungs are below
        // the 10-wide gear gate and must NOT flag.
        let mut objs = Vec::new();
        for l in (1..=99).step_by(5) {
            objs.push(obj(500 + l, l));
        }
        let g = state_with(vec![], objs);
        let report = audit(&g);
        assert!(report.gear_bands.is_empty(), "{:?}", report.gear_bands);

        // Gear only at L1: no new unlock from L2 on — one band to the cap.
        let g2 = state_with(vec![], vec![obj(500, 1)]);
        let report2 = audit(&g2);
        assert_eq!(report2.gear_bands, vec![(2, 99)]);
    }

    #[test]
    fn fully_curved_world_has_no_dead_bands() {
        // The W4 end state: mobs at least every few levels, a gear rung at
        // least every ten, and a quest target always in window.
        let mut mobs = Vec::new();
        for l in (1..=99).step_by(4) {
            mobs.push(mob(1000 + l, l as u8, true));
        }
        let mut objs = Vec::new();
        for l in (1..=99).step_by(10) {
            objs.push(obj(2000 + l, l));
        }
        let g = state_with(mobs, objs);
        let report = audit(&g);
        assert!(report.is_balanced(), "{:?}", report.dead_bands());
    }

    // ------------------------------------------------------------------
    // The real-world gate. RED while the shipped curve has dead bands;
    // scripts/balance-check.sh runs this and closes green in W4.
    // ------------------------------------------------------------------
    #[tokio::test]
    #[ignore = "balance gate: needs the shipped lib; red until W4 fills the curve"]
    async fn balance_audit_real_lib_gate() {
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        assert!(
            std::path::Path::new(&format!("{}/world/worldmap", lib)).exists(),
            "balance gate needs the shipped lib at {}",
            lib
        );
        let mut g = GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();

        let report = audit(&g);
        print!("{}", render(&report));
        assert!(
            report.is_balanced(),
            "balance gate failed, dead bands: {:?}",
            report
                .dead_bands()
                .iter()
                .map(|(f, l)| format!("L{}-L{}", f, l))
                .collect::<Vec<_>>()
        );
    }
}
