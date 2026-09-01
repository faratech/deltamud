// Object (item) entity — id-indexed. Location and containment are expressed
// as ids into the GameState arenas rather than locked pointers.

use crate::types::*;
use std::collections::HashSet;

/// Maximum number of object levels visited along a containment path. Roots are
/// serialized at depth 0 but count as level 1, so the 33rd object is skipped.
pub const MAX_OBJECT_GRAPH_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectGraphOrder {
    Preorder,
    Postorder,
}

/// Recursive-call order used by the C persistence functions before emitting
/// the current object. `House_save` visits contains then next_content;
/// `Crash_save` visits next_content then contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectListOrder {
    ContainsThenNext,
    NextThenContains,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectGraphVisit {
    pub id: ObjId,
    pub depth: usize,
    /// Index of the top-level root through which this identity was first seen.
    pub root_index: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ObjectGraphWalk {
    pub visits: Vec<ObjectGraphVisit>,
    pub cycle_detected: bool,
    pub duplicate_detected: bool,
    pub depth_overflow: bool,
    pub missing_detected: bool,
    pub problem_ids: Vec<ObjId>,
}

impl ObjectGraphWalk {
    pub fn malformed(&self) -> bool {
        self.cycle_detected
            || self.duplicate_detected
            || self.depth_overflow
            || self.missing_detected
    }
}

/// Iteratively walk an object containment graph in stable, left-to-right DFS
/// order. Each object identity is emitted at most once, even if malformed data
/// gives it multiple parents. Back-edges and nodes deeper than
/// [`MAX_OBJECT_GRAPH_DEPTH`] are skipped. A malformed traversal emits one
/// aggregate SYSERR after the walk rather than one message per bad edge.
pub fn walk_object_graph<I, F>(
    roots: I,
    order: ObjectGraphOrder,
    context: &str,
    mut children: F,
) -> ObjectGraphWalk
where
    I: IntoIterator<Item = ObjId>,
    F: FnMut(ObjId) -> Option<Vec<ObjId>>,
{
    walk_object_graph_with_depth(roots, order, context, |id| {
        children(id).map(|ids| ids.into_iter().map(|child| (child, 1)).collect())
    })
}

/// Variant for C linked-list persistence walks. Each edge supplies its logical
/// containment-depth increment: `1` for `contains`, `0` for `next_content`.
/// This preserves the depth limit without treating a long sibling list as
/// deeply nested.
pub fn walk_object_graph_with_depth<I, F>(
    roots: I,
    order: ObjectGraphOrder,
    context: &str,
    mut children: F,
) -> ObjectGraphWalk
where
    I: IntoIterator<Item = ObjId>,
    F: FnMut(ObjId) -> Option<Vec<(ObjId, usize)>>,
{
    #[derive(Clone, Copy)]
    enum Frame {
        Enter(ObjectGraphVisit),
        Exit(ObjectGraphVisit),
    }

    let roots: Vec<ObjId> = roots.into_iter().collect();
    let mut stack = Vec::with_capacity(roots.len());
    for (root_index, id) in roots.into_iter().enumerate().rev() {
        stack.push(Frame::Enter(ObjectGraphVisit {
            id,
            depth: 0,
            root_index,
        }));
    }

    let mut walk = ObjectGraphWalk::default();
    let mut visited = HashSet::new();
    let mut active = HashSet::new();

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter(visit) => {
                if active.contains(&visit.id) {
                    walk.cycle_detected = true;
                    walk.problem_ids.push(visit.id);
                    continue;
                }
                if visited.contains(&visit.id) {
                    walk.duplicate_detected = true;
                    walk.problem_ids.push(visit.id);
                    continue;
                }
                if visit.depth >= MAX_OBJECT_GRAPH_DEPTH {
                    walk.depth_overflow = true;
                    walk.problem_ids.push(visit.id);
                    continue;
                }
                let Some(contained) = children(visit.id) else {
                    walk.missing_detected = true;
                    walk.problem_ids.push(visit.id);
                    continue;
                };

                visited.insert(visit.id);
                active.insert(visit.id);
                if order == ObjectGraphOrder::Preorder {
                    walk.visits.push(visit);
                }
                stack.push(Frame::Exit(visit));
                for (id, depth_increment) in contained.into_iter().rev() {
                    stack.push(Frame::Enter(ObjectGraphVisit {
                        id,
                        depth: visit.depth.saturating_add(depth_increment),
                        root_index: visit.root_index,
                    }));
                }
            }
            Frame::Exit(visit) => {
                active.remove(&visit.id);
                if order == ObjectGraphOrder::Postorder {
                    walk.visits.push(visit);
                }
            }
        }
    }

    if walk.malformed() {
        log::warn!(
            "SYSERR: {} object containment traversal rejected edges (cycle={}, duplicate-parent={}, depth-overflow={}, missing={}) near ids {:?}; invalid nodes skipped",
            context,
            walk.cycle_detected,
            walk.duplicate_detected,
            walk.depth_overflow,
            walk.missing_detected,
            walk.problem_ids,
        );
    }

    walk
}

/// Iterative, cycle-safe emulation of the C functions that recurse over both
/// an object's `contains` pointer and its linked-list `next_content` pointer,
/// then emit the object. Each input vector is one C sibling list; separate
/// vectors let callers retain per-list metadata through `root_index`.
pub fn walk_object_lists_postorder<I, F>(
    root_lists: I,
    order: ObjectListOrder,
    context: &str,
    mut children: F,
) -> ObjectGraphWalk
where
    I: IntoIterator<Item = Vec<ObjId>>,
    F: FnMut(ObjId) -> Option<Vec<ObjId>>,
{
    enum Frame {
        List {
            ids: Vec<ObjId>,
            index: usize,
            depth: usize,
            root_index: usize,
        },
        Exit(ObjectGraphVisit),
    }

    let root_lists: Vec<Vec<ObjId>> = root_lists.into_iter().collect();
    let mut stack = Vec::with_capacity(root_lists.len());
    for (root_index, ids) in root_lists.into_iter().enumerate().rev() {
        stack.push(Frame::List {
            ids,
            index: 0,
            depth: 0,
            root_index,
        });
    }
    let mut walk = ObjectGraphWalk::default();
    let mut visited = HashSet::new();
    let mut active = HashSet::new();

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::List {
                ids,
                index,
                depth,
                root_index,
            } => {
                let Some(&id) = ids.get(index) else {
                    continue;
                };
                let rest = Frame::List {
                    ids,
                    index: index + 1,
                    depth,
                    root_index,
                };
                if active.contains(&id) {
                    walk.cycle_detected = true;
                    walk.problem_ids.push(id);
                    stack.push(rest);
                    continue;
                }
                if visited.contains(&id) {
                    walk.duplicate_detected = true;
                    walk.problem_ids.push(id);
                    stack.push(rest);
                    continue;
                }
                if depth >= MAX_OBJECT_GRAPH_DEPTH {
                    walk.depth_overflow = true;
                    walk.problem_ids.push(id);
                    stack.push(rest);
                    continue;
                }
                let Some(contained) = children(id) else {
                    walk.missing_detected = true;
                    walk.problem_ids.push(id);
                    stack.push(rest);
                    continue;
                };

                let visit = ObjectGraphVisit {
                    id,
                    depth,
                    root_index,
                };
                visited.insert(id);
                active.insert(id);
                stack.push(Frame::Exit(visit));
                let contents = Frame::List {
                    ids: contained,
                    index: 0,
                    depth: depth + 1,
                    root_index,
                };
                match order {
                    ObjectListOrder::ContainsThenNext => {
                        stack.push(rest);
                        stack.push(contents);
                    }
                    ObjectListOrder::NextThenContains => {
                        stack.push(contents);
                        stack.push(rest);
                    }
                }
            }
            Frame::Exit(visit) => {
                active.remove(&visit.id);
                walk.visits.push(visit);
            }
        }
    }

    if walk.malformed() {
        log::warn!(
            "SYSERR: {} object containment traversal rejected edges (cycle={}, duplicate-parent={}, depth-overflow={}, missing={}) near ids {:?}; invalid nodes skipped",
            context,
            walk.cycle_detected,
            walk.duplicate_detected,
            walk.depth_overflow,
            walk.missing_detected,
            walk.problem_ids,
        );
    }

    walk
}

// Object types (structs.h ITEM_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    // Discriminants MUST equal DeltaMUD structs.h ITEM_* numbers: they are the
    // value read from .obj files (file_loader) and the index into ITEM_TYPES
    // (constants.rs, C item_types[]). A previous, sequential numbering silently
    // mis-typed every non-weapon item (FOOD→OTHER, CONTAINER→FOOD, DRINKCON→
    // FOUNTAIN, ...) — masked for immortals, who bypass the item-type checks.
    Light = 1,
    Scroll = 2,
    Wand = 3,
    Staff = 4,
    Weapon = 5,
    FireWeapon = 6,
    Missile = 7,
    Treasure = 8,
    Armor = 9,
    Potion = 10,
    Worn = 11,
    Other = 12,
    Trash = 13,
    Trap = 14,
    Container = 15,
    Note = 16,
    LiqContainer = 17, // ITEM_DRINKCON
    Key = 18,
    Food = 19,
    Money = 20,
    Pen = 21,
    Boat = 22,
    Fountain = 23,
    Portal = 24,
    HpRegen = 25,
    MpRegen = 26,
    MvRegen = 27,
    Atm = 28,
}

impl ObjectType {
    pub fn from_i32(v: i32) -> ObjectType {
        use ObjectType::*;
        match v {
            1 => Light,
            2 => Scroll,
            3 => Wand,
            4 => Staff,
            5 => Weapon,
            6 => FireWeapon,
            7 => Missile,
            8 => Treasure,
            9 => Armor,
            10 => Potion,
            11 => Worn,
            13 => Trash,
            14 => Trap,
            15 => Container,
            16 => Note,
            17 => LiqContainer,
            18 => Key,
            19 => Food,
            20 => Money,
            21 => Pen,
            22 => Boat,
            23 => Fountain,
            24 => Portal,
            25 => HpRegen,
            26 => MpRegen,
            27 => MvRegen,
            28 => Atm,
            _ => Other, // 12 and any unknown
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WearFlags: u32 {
        const TAKE = 1 << 0;
        const FINGER = 1 << 1;
        const NECK = 1 << 2;
        const BODY = 1 << 3;
        const HEAD = 1 << 4;
        const LEGS = 1 << 5;
        const FEET = 1 << 6;
        const HANDS = 1 << 7;
        const ARMS = 1 << 8;
        const SHIELD = 1 << 9;
        const ABOUT = 1 << 10;
        const WAIST = 1 << 11;
        const WRIST = 1 << 12;
        const WIELD = 1 << 13;
        const HOLD = 1 << 14;
        const SHOULDERS = 1 << 15;
        const ANKLE = 1 << 16;
        const FACE = 1 << 17;
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExtraFlags: u64 {
        const GLOW = 1 << 0;
        const HUM = 1 << 1;
        const NO_RENT = 1 << 2;
        const NO_DONATE = 1 << 3;
        const NO_INVIS = 1 << 4;
        const INVISIBLE = 1 << 5;
        const MAGIC = 1 << 6;
        const NO_DROP = 1 << 7;
        const BLESS = 1 << 8;
        const ANTI_GOOD = 1 << 9;
        const ANTI_EVIL = 1 << 10;
        const ANTI_NEUTRAL = 1 << 11;
        const ANTI_MAGIC_USER = 1 << 12;
        const ANTI_CLERIC = 1 << 13;
        const ANTI_THIEF = 1 << 14;
        const ANTI_WARRIOR = 1 << 15;
        const NO_SELL = 1 << 16;
        const ANTI_ARTISAN = 1 << 17;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectAffect {
    pub location: i32,
    pub modifier: i32,
}

/// Where an object currently lives. Exactly one of these holds at a time,
/// mirroring CircleMUD's mutually-exclusive in_room/carried_by/worn_by/in_obj.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjLoc {
    Nowhere,
    Room(RoomRnum),
    Carried(CharId),
    Worn(CharId, usize),
    Contained(ObjId),
}

#[derive(Debug)]
pub struct Object {
    pub id: ObjId,
    pub item_number: ObjVnum, // prototype vnum, or NOTHING for synthetic

    /// Current location (room / inventory / worn / inside container).
    pub loc: ObjLoc,
    /// Ids of objects this container holds (CircleMUD obj->contains list).
    pub contains: Vec<ObjId>,

    // Descriptions
    pub name: String,              // keyword list
    pub description: String,       // shown on the ground
    pub short_description: String, // inventory / action line
    pub action_description: Option<String>,

    // Properties
    pub obj_type: ObjectType,
    pub wear_flags: WearFlags,
    pub extra_flags: ExtraFlags,
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub level: Level,
    pub timer: i32,
    pub values: [i32; 4],
    // DeltaMUD "slots" carried in value[4]/value[5] of the .obj values line.
    pub curr_slots: i32,
    pub total_slots: i32,
    // DeltaMUD per-object class restriction (`c` block, stored as class-1) and
    // minimum use level (`L` block).
    pub obj_class: i32,
    pub min_level: i32,
    // 32-bit affect bitvector (`BV` block).
    pub bitvector: i64,
    pub affects: Vec<ObjectAffect>,
    /// Extra descriptions (`E` blocks): (keyword, description).
    pub ex_descriptions: Vec<(String, String)>,
}

impl Object {
    pub fn new(vnum: ObjVnum, name: String, short_desc: String) -> Self {
        Object {
            id: ObjId(0),
            item_number: vnum,
            loc: ObjLoc::Nowhere,
            contains: Vec::new(),
            name,
            description: String::new(),
            short_description: short_desc,
            action_description: None,
            obj_type: ObjectType::Other,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost: 0,
            rent: 0,
            level: 0,
            timer: -1,
            values: [0; 4],
            curr_slots: 0,
            total_slots: 0,
            obj_class: -1,
            min_level: 0,
            bitvector: 0,
            affects: Vec::new(),
            ex_descriptions: Vec::new(),
        }
    }

    pub fn is_container(&self) -> bool {
        self.obj_type == ObjectType::Container
    }
    pub fn is_weapon(&self) -> bool {
        self.obj_type == ObjectType::Weapon
    }
    pub fn is_armor(&self) -> bool {
        self.obj_type == ObjectType::Armor
    }
    pub fn can_wear(&self, position: WearFlags) -> bool {
        self.wear_flags.contains(position)
    }

    /// Weapon damage dice (num, size) from values[1]/values[2].
    pub fn damage_dice(&self) -> Option<(i32, i32)> {
        if self.is_weapon() {
            Some((self.values[1], self.values[2]))
        } else {
            None
        }
    }

    pub fn armor_class(&self) -> i32 {
        if self.is_armor() { self.values[0] } else { 0 }
    }
}

#[cfg(test)]
mod object_graph_tests {
    use super::*;
    use std::collections::HashMap;

    fn walk(roots: &[u64], edges: &[(u64, &[u64])], order: ObjectGraphOrder) -> ObjectGraphWalk {
        let graph: HashMap<ObjId, Vec<ObjId>> = edges
            .iter()
            .map(|(id, children)| (ObjId(*id), children.iter().copied().map(ObjId).collect()))
            .collect();
        walk_object_graph(
            roots.iter().copied().map(ObjId),
            order,
            "object graph test",
            |id| graph.get(&id).cloned(),
        )
    }

    fn ids(walk: &ObjectGraphWalk) -> Vec<u64> {
        walk.visits.iter().map(|visit| visit.id.0).collect()
    }

    #[test]
    fn preorder_is_parent_before_left_to_right_children() {
        let walk = walk(
            &[1],
            &[(1, &[2, 3]), (2, &[4]), (3, &[]), (4, &[])],
            ObjectGraphOrder::Preorder,
        );

        assert_eq!(ids(&walk), vec![1, 2, 4, 3]);
    }

    #[test]
    fn postorder_is_children_before_parent() {
        let walk = walk(
            &[1],
            &[(1, &[2, 3]), (2, &[4]), (3, &[]), (4, &[])],
            ObjectGraphOrder::Postorder,
        );

        assert_eq!(ids(&walk), vec![4, 2, 3, 1]);
    }

    #[test]
    fn cycle_is_reported_and_each_identity_is_visited_once() {
        let walk = walk(&[1], &[(1, &[2]), (2, &[1])], ObjectGraphOrder::Preorder);

        assert_eq!(ids(&walk), vec![1, 2]);
        assert!(walk.cycle_detected);
        assert!(!walk.depth_overflow);
    }

    #[test]
    fn shared_child_is_emitted_only_for_its_first_parent() {
        let walk = walk(
            &[1, 2],
            &[(1, &[3]), (2, &[3]), (3, &[])],
            ObjectGraphOrder::Preorder,
        );

        assert_eq!(ids(&walk), vec![1, 3, 2]);
        assert_eq!(walk.visits[1].root_index, 0);
        assert!(!walk.cycle_detected);
        assert!(walk.duplicate_detected);
        assert!(!walk.depth_overflow);
    }

    #[test]
    fn missing_identity_is_reported_without_panicking() {
        let walk = walk(&[99], &[], ObjectGraphOrder::Preorder);

        assert!(walk.visits.is_empty());
        assert!(walk.missing_detected);
        assert_eq!(walk.problem_ids, vec![ObjId(99)]);
    }

    #[test]
    fn depth_33_is_skipped_and_reported() {
        let edges: Vec<(u64, Vec<u64>)> = (1..=33)
            .map(|id| {
                if id < 33 {
                    (id, vec![id + 1])
                } else {
                    (id, Vec::new())
                }
            })
            .collect();
        let graph: HashMap<ObjId, Vec<ObjId>> = edges
            .into_iter()
            .map(|(id, children)| (ObjId(id), children.into_iter().map(ObjId).collect()))
            .collect();

        let walk = walk_object_graph([ObjId(1)], ObjectGraphOrder::Preorder, "depth test", |id| {
            graph.get(&id).cloned()
        });

        assert_eq!(walk.visits.len(), MAX_OBJECT_GRAPH_DEPTH);
        assert_eq!(walk.visits.last().map(|visit| visit.id), Some(ObjId(32)));
        assert!(walk.depth_overflow);
        assert!(!walk.cycle_detected);
    }

    #[test]
    fn c_list_walks_match_house_and_crash_recursive_orders() {
        let graph: HashMap<ObjId, Vec<ObjId>> = [
            (ObjId(1), vec![ObjId(3), ObjId(4)]),
            (ObjId(2), vec![]),
            (ObjId(3), vec![]),
            (ObjId(4), vec![]),
        ]
        .into_iter()
        .collect();

        let house = walk_object_lists_postorder(
            [vec![ObjId(1), ObjId(2)]],
            ObjectListOrder::ContainsThenNext,
            "House_save order test",
            |id| graph.get(&id).cloned(),
        );
        assert_eq!(ids(&house), vec![4, 3, 2, 1]);
        assert_eq!(
            house
                .visits
                .iter()
                .map(|visit| visit.depth)
                .collect::<Vec<_>>(),
            vec![1, 1, 0, 0]
        );

        let crash = walk_object_lists_postorder(
            [vec![ObjId(1), ObjId(2)]],
            ObjectListOrder::NextThenContains,
            "Crash_save order test",
            |id| graph.get(&id).cloned(),
        );
        assert_eq!(ids(&crash), vec![2, 4, 3, 1]);
        assert_eq!(
            crash
                .visits
                .iter()
                .map(|visit| visit.depth)
                .collect::<Vec<_>>(),
            vec![0, 1, 1, 0]
        );
    }
}
