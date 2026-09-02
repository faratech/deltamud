// GameState: the single owner of the entire world, mirroring CircleMUD's
// single-threaded heartbeat. Every entity lives in an id-indexed arena here;
// commands and handlers operate on `&mut GameState`. Async I/O lives outside
// (game.rs / connection.rs) and communicates only through Descriptor::outbuf.

use crate::character::Character;
use crate::config::Config;
use crate::connection::{ConState, Descriptor};
use crate::object::{ObjLoc, Object, ObjectGraphOrder, walk_object_graph};
use crate::rng::Rng;
use crate::room::Room;
use crate::types::*;
use crate::world::{MobileProto, ObjectProto, Zone};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};

/// Process-global copy of the listening socket fd (C `mother_desc`). main.rs
/// publishes it here right after TcpListener::bind so do_copyover (running in
/// the Game task, which never sees main's local listener) can clear FD_CLOEXEC
/// on it and hand it to the re-exec'd binary. -1 until the listener is bound.
static LISTENER_FD: AtomicI32 = AtomicI32::new(-1);

/// Publish the bound listener fd (main.rs, at boot). Mirrors C setting
/// `mother_desc` before init_game so copyover can inherit it.
pub fn set_listener_fd(fd: std::os::unix::io::RawFd) {
    LISTENER_FD.store(fd, Ordering::SeqCst);
}

#[cfg(test)]
mod object_extraction_tests {
    use super::*;

    #[test]
    fn extract_obj_refuses_an_attached_cyclic_graph_without_partial_mutation() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Extraction test".to_string(),
            "A test room.".to_string(),
        ));
        let a = g.create_obj(Object::new(100, "a".to_string(), "object a".to_string()));
        let b = g.create_obj(Object::new(101, "b".to_string(), "object b".to_string()));
        g.obj_to_room(a, room);
        g.get_obj_mut(a).unwrap().contains.push(b);
        g.get_obj_mut(b).unwrap().contains.push(a);

        assert!(!g.extract_obj(a));

        assert_eq!(g.room(room).contents, vec![a]);
        assert_eq!(g.get_obj(a).unwrap().loc, ObjLoc::Room(room));
        assert_eq!(g.get_obj(a).unwrap().contains, vec![b]);
        assert_eq!(g.get_obj(b).unwrap().contains, vec![a]);
    }

    #[test]
    fn extract_obj_detaches_an_attached_valid_graph_only_after_preflight() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            101,
            0,
            "Extraction test".to_string(),
            "A test room.".to_string(),
        ));
        let root = g.create_obj(Object::new(
            200,
            "root".to_string(),
            "root object".to_string(),
        ));
        let child = g.create_obj(Object::new(
            201,
            "child".to_string(),
            "child object".to_string(),
        ));
        g.obj_to_room(root, room);
        g.obj_to_obj(child, root);

        assert!(g.extract_obj(root));

        assert!(g.room(room).contents.is_empty());
        assert!(g.get_obj(root).is_none());
        assert!(g.get_obj(child).is_none());
    }

    #[test]
    fn extract_obj_refuses_an_attached_overdeep_graph_without_detaching_it() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            102,
            0,
            "Extraction test".to_string(),
            "A test room.".to_string(),
        ));
        let objects: Vec<ObjId> = (0..=crate::object::MAX_OBJECT_GRAPH_DEPTH)
            .map(|index| {
                g.create_obj(Object::new(
                    300 + index as i32,
                    format!("object-{index}"),
                    format!("object {index}"),
                ))
            })
            .collect();
        g.obj_to_room(objects[0], room);
        for pair in objects.windows(2) {
            g.obj_to_obj(pair[1], pair[0]);
        }

        assert!(!g.extract_obj(objects[0]));

        assert_eq!(g.room(room).contents, vec![objects[0]]);
        assert_eq!(g.get_obj(objects[0]).unwrap().loc, ObjLoc::Room(room));
        assert!(objects.iter().all(|id| g.get_obj(*id).is_some()));
    }

    #[test]
    fn extract_objs_preflights_the_entire_batch_before_detaching_any_root() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            103,
            0,
            "Extraction test".to_string(),
            "A test room.".to_string(),
        ));
        let valid = g.create_obj(Object::new(
            400,
            "valid".to_string(),
            "valid object".to_string(),
        ));
        let corrupt = g.create_obj(Object::new(
            401,
            "corrupt".to_string(),
            "corrupt object".to_string(),
        ));
        let child = g.create_obj(Object::new(
            402,
            "child".to_string(),
            "child object".to_string(),
        ));
        g.obj_to_room(valid, room);
        g.obj_to_room(corrupt, room);
        g.get_obj_mut(corrupt).unwrap().contains.push(child);
        g.get_obj_mut(child).unwrap().contains.push(corrupt);

        assert!(!g.extract_objs([valid, corrupt]));

        assert!(g.room(room).contents.contains(&valid));
        assert!(g.room(room).contents.contains(&corrupt));
        assert_eq!(g.get_obj(valid).unwrap().loc, ObjLoc::Room(room));
        assert_eq!(g.get_obj(corrupt).unwrap().loc, ObjLoc::Room(room));
        assert!(g.get_obj(child).is_some());
    }

    #[test]
    fn extract_obj_refuses_non_reciprocal_containment_without_detaching_root() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            104,
            0,
            "Extraction test".to_string(),
            "A test room.".to_string(),
        ));
        let root = g.create_obj(Object::new(
            500,
            "root".to_string(),
            "root object".to_string(),
        ));
        let child = g.create_obj(Object::new(
            501,
            "child".to_string(),
            "child object".to_string(),
        ));
        g.obj_to_room(root, room);
        g.get_obj_mut(root).unwrap().contains.push(child);

        assert!(!g.extract_obj(root));

        assert_eq!(g.room(room).contents, vec![root]);
        assert_eq!(g.get_obj(root).unwrap().loc, ObjLoc::Room(room));
        assert_eq!(g.get_obj(root).unwrap().contains, vec![child]);
        assert_eq!(g.get_obj(child).unwrap().loc, ObjLoc::Nowhere);
    }
}

#[cfg(test)]
mod principal_authority_tests {
    use super::*;
    use crate::character::Character;

    fn connected_player(
        g: &mut GameState,
        conn: ConnId,
        name: &str,
        level: Level,
        trust: i32,
    ) -> CharId {
        let mut character = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.player.level = level;
        character.trust = trust;
        character.desc = Some(conn);
        let character = g.create_char(character);
        let mut descriptor = Descriptor::new(conn, "authority.test".to_string());
        descriptor.character = Some(character);
        g.descriptors.insert(conn, descriptor);
        character
    }

    #[test]
    fn connected_pc_authority_is_exact_persisted_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let high_level = connected_player(&mut g, ConnId(801), "Display", LVL_IMPL, 1);
        let high_trust = connected_player(&mut g, ConnId(802), "Trusted", 1, i32::from(LVL_IMPL));

        assert_eq!(g.principal_authority(high_level).unwrap().authority, 1);
        let trusted = g.principal_authority(high_trust).unwrap();
        assert_eq!(trusted.authority, i32::from(LVL_IMPL));
        assert!(trusted.is_authenticated_player());
    }

    #[test]
    fn switched_body_resolves_original_pc_and_duplicate_alias_fails_closed() {
        let mut g = GameState::new(Config::default());
        let principal = connected_player(&mut g, ConnId(803), "Principal", 1, i32::from(LVL_GRGOD));
        let mut body = Character::new_npc(7001);
        body.player.level = LVL_IMPL;
        body.desc = Some(ConnId(803));
        let body = g.create_char(body);
        g.get_char_mut(principal).unwrap().desc = None;
        {
            let descriptor = g.descriptors.get_mut(&ConnId(803)).unwrap();
            descriptor.character = Some(body);
            descriptor.original = Some(principal);
        }

        for target in [principal, body] {
            let authority = g.principal_authority(target).unwrap();
            assert_eq!(authority.principal, principal);
            assert_eq!(authority.authority, i32::from(LVL_GRGOD));
            assert!(authority.switched_session);
            assert!(authority.is_authenticated_player());
        }

        let mut alias_body = Character::new_npc(7002);
        alias_body.desc = Some(ConnId(804));
        let alias_body = g.create_char(alias_body);
        let mut duplicate = Descriptor::new(ConnId(804), "authority.test".to_string());
        duplicate.character = Some(alias_body);
        duplicate.original = Some(principal);
        g.descriptors.insert(ConnId(804), duplicate);

        assert_eq!(g.principal_authority(principal), None);
        assert_eq!(g.principal_authority(body), None);
    }

    #[test]
    fn invalid_pc_trust_fails_closed_but_descriptorless_npc_keeps_level_mechanics() {
        let mut g = GameState::new(Config::default());
        let corrupt = connected_player(&mut g, ConnId(805), "Corrupt", LVL_IMPL, 106);
        assert_eq!(g.principal_authority(corrupt), None);

        let mut npc = Character::new_npc(7003);
        npc.player.level = LVL_GRGOD;
        let npc = g.create_char(npc);
        let authority = g.principal_authority(npc).unwrap();
        assert_eq!(authority.authority, i32::from(LVL_GRGOD));
        assert!(!authority.principal_is_player);
        assert!(!authority.is_authenticated_player());
    }

    #[test]
    fn inspection_and_player_index_use_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let trusted = connected_player(&mut g, ConnId(806), "Trusted", 1, 104);
        let spoofed = connected_player(&mut g, ConnId(807), "Spoofed", LVL_IMPL, 1);

        assert!(g.can_inspect_player_authority(trusted, 104));
        assert!(!g.can_inspect_player_authority(spoofed, 2));
        assert!(!g.can_inspect_player_authority(trusted, 106));

        let mut indexed = Character::new_player(
            "Indexed".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        indexed.idnum = 9_999;
        indexed.player.level = LVL_IMPL;
        indexed.trust = 3;
        g.update_player_index_from_character(&indexed, 123, "index.test");
        let row = g.player_index("Indexed").unwrap();
        assert_eq!(row.level, LVL_IMPL);
        assert_eq!(row.trust, 3);
    }
}

/// Read the published listener fd (do_copyover). -1 if not yet bound.
pub fn listener_fd() -> std::os::unix::io::RawFd {
    LISTENER_FD.load(Ordering::SeqCst)
}

/// One row of the in-memory player index (C `struct player_index_element`,
/// db.h). C carries only {name, id, level}; the Rust port additionally caches
/// selected `player_main` fields so offline reports that C answers directly
/// from SQL (last/roster/autowiz) do not silently omit logged-off players.
/// `name` keeps the player's stored capitalisation (C lowercases its copy;
/// get_name_by_id then returns the lowercased form — we keep the canonical name
/// so callers that display it, e.g. the ignore listing, match the C
/// `last`/listing output).
#[derive(Debug, Clone)]
pub struct PlayerIndex {
    pub idnum: i64,
    pub name: String,
    pub level: u8,
    /// Persisted command authority. Display level remains available for
    /// gameplay/reporting, but offline authorization must use this field.
    pub trust: i32,
    pub class: crate::types::Class,
    pub last_logon: i64,
    pub host: String,
    pub act_flags: i64,
    pub clan: i32,
    pub clan_rank: i32,
}

/// A deferred immortal command against an OFFLINE player (the async bridge for
/// C's do_set/do_stat/do_show, which load a logged-off player's full record via
/// retrieve_player_entry, edit it, and save). The synchronous command handler
/// can't await the async DB, so when it finds the target offline-but-indexed it
/// queues one of these instead of degrading to "no such player". The async Game
/// loop drains the queue (game.rs), loads the player into the world, REPLAYS
/// `command` through command_interpreter so the existing online handler logic
/// applies, then persists + extracts. `requester` is the immortal who typed it;
/// `target` is the offline player's name; `command` is the immortal's ORIGINAL
/// command verbatim (e.g. "set Mortvictim gold 5000") so the replay re-parses
/// the identical field/value.
#[derive(Debug, Clone)]
pub struct OfflineOp {
    pub requester: CharId,
    pub target: String,
    pub command: String,
    pub authority: OfflineOpAuthority,
}

/// A live-player rename which must cross the synchronous command/async SQL
/// boundary before any success is published.  The async Game shell rechecks
/// every identity and authority field, moves the name-keyed sidecars, performs
/// one conditional database rename, and only then updates the live indexes.
#[derive(Debug, Clone)]
pub struct PlayerRenameRequest {
    pub authorization: AuthenticatedCommandRequest,
    pub victim: CharId,
    pub idnum: i64,
    pub old_name: String,
    pub new_name: String,
}

/// A password-only write queued by the synchronous authenticated `set passwd`
/// command. Carrying resolved identity fields keeps the async bridge typed and
/// prevents replay or string re-parsing from selecting a different account.
pub struct PasswordUpdateRequest {
    pub authorization: AuthenticatedCommandRequest,
    pub victim: CharId,
    pub idnum: i64,
    pub name: String,
    /// Held only until the async bridge can enter the bounded off-thread KDF.
    /// Deliberately lacks Debug/Clone so the credential cannot be copied into
    /// routine diagnostics or queue snapshots.
    pub plaintext_password: String,
}

/// An AFK-terminal unlock verification queued by the synchronous command
/// dispatcher. The async Game shell runs the KDF on the bounded password
/// worker pool, then rechecks this exact live session before clearing the lock.
pub struct LockoutUnlockRequest {
    pub character: CharId,
    pub principal: CharId,
    pub descriptor: ConnId,
    pub idnum: i64,
    pub name: String,
    pub expected_hash: String,
    /// Deliberately not Debug/Clone: typed password material must live only
    /// until the bounded off-thread verifier consumes it.
    pub plaintext_password: String,
}

/// A rank/capability transition queued by a synchronous immortal command.
/// The async Game shell compares this complete tuple against durable storage
/// before changing live authority or publishing success.
#[derive(Debug, Clone)]
pub struct AuthorityUpdateRequest {
    pub authorization: AuthenticatedCommandRequest,
    pub victim: CharId,
    pub idnum: i64,
    pub name: String,
    pub expected: crate::PlayerAuthorityState,
    pub replacement: crate::PlayerAuthorityState,
}

/// Why the graceful game loop is exiting. The process wrapper maps Restart to
/// a dedicated non-zero status for systemd, while operator/service stops remain
/// successful exits that `Restart=on-failure` must not revive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessDisposition {
    Stop,
    Restart,
}

/// Exact authenticated session identity captured by a synchronous command
/// before an asynchronous/destructive action is queued. The async consumer
/// must re-resolve this tuple and its required grant after all earlier durable
/// authority transitions have drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedCommandRequest {
    pub requester_body: CharId,
    pub requester_principal: CharId,
    pub descriptor: ConnId,
    pub idnum: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownMode {
    Shutdown,
    Reboot,
    Now,
    Die,
    Pause,
}

impl ShutdownMode {
    pub fn disposition(self) -> ProcessDisposition {
        match self {
            Self::Shutdown | Self::Reboot | Self::Now => ProcessDisposition::Restart,
            Self::Die | Self::Pause => ProcessDisposition::Stop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownRequest {
    /// Internal scheduler/signal-equivalent path; no player capability is
    /// being carried across an async boundary.
    System(ProcessDisposition),
    Command {
        authorization: AuthenticatedCommandRequest,
        mode: ShutdownMode,
    },
}

/// The authority principal resolved for a live character identity.
///
/// A switched descriptor controls one body (`Descriptor::character`) on
/// behalf of its authenticated player (`Descriptor::original`).  Privileged
/// callers must use `authority`, which is the player's persisted `trust`, not
/// the display level of whichever body happens to be active.  Descriptorless
/// NPCs have no authenticated player principal; their level is exposed only so
/// ordinary NPC mechanics can retain the historical level hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrincipalAuthority {
    pub principal: CharId,
    pub authority: i32,
    pub descriptor: Option<ConnId>,
    pub descriptor_controls_target: bool,
    pub switched_session: bool,
    pub principal_is_player: bool,
}

impl PrincipalAuthority {
    /// True only when this authority belongs to the player authenticated on a
    /// live descriptor.  Destructive administrative exceptions should require
    /// this in addition to their trust threshold.
    pub fn is_authenticated_player(self) -> bool {
        self.principal_is_player && self.descriptor.is_some()
    }
}

/// Authorization contract carried across the synchronous-command/async-DB
/// bridge. Most deferred commands rely on their replayed handler's own gates;
/// player inspection additionally has to be checked against the indexed level
/// before queueing and the freshly loaded/live level before replay (#409).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineOpAuthority {
    ReplayHandler,
    InspectPlayer,
}

/// C's `stat file` denial, shared by every online/offline inspection route.
pub const PLAYER_INSPECTION_DENIED: &str = "Sorry, you can't do that.\r\n";

/// Deferred async DB work queued from synchronous command handlers.
#[derive(Debug, Clone)]
pub enum DeferredDbOp {
    /// clan.c:242-255: shift clans past the destroyed one.
    ClanDestroyFixup(i32),
    /// clan.c:388-405: lower every member of the clan to rank 1.
    ClanLowerRanks(i32),
}
/// The OLC core registry (phase 1: was olc.rs module statics).
#[derive(Default)]
pub struct OlcState {
    /// conn -> active editor kind.
    pub active: HashMap<ConnId, crate::olc::EditorKind>,
    /// (zone, component) pairs edited but not yet written to disk.
    pub save_list: Vec<(i32, i32)>,
    /// save-list entries whose durable publication is unresolved.
    pub unresolved: Vec<crate::olc::UnresolvedSave>,
    /// modify.rs: in-flight string-editor states keyed by connection.
    pub edits: HashMap<ConnId, crate::modify::EditState>,
    /// modify.rs: active pagers keyed by connection.
    pub pagers: HashMap<ConnId, crate::modify::Pager>,
    /// Per-editor working copies keyed by connection (the old per-editor
    /// `STATES` statics).
    pub redit_states: HashMap<ConnId, crate::redit::ReditState>,
    pub oedit_states: HashMap<ConnId, crate::oedit::OeditState>,
    pub medit_states: HashMap<ConnId, crate::medit::MeditState>,
    pub zedit_states: HashMap<ConnId, crate::zedit::ZeditState>,
    pub sedit_states: HashMap<ConnId, crate::sedit::SeditState>,
    pub aedit_states: HashMap<ConnId, crate::aedit::AeditState>,
    pub hedit_states: HashMap<ConnId, crate::hedit::HeditState>,
    pub trigedit_states: HashMap<ConnId, crate::trigedit::TrigEditState>,
    /// redit/oedit multi-line text sub-editor buffers.
    pub redit_text_bufs: HashMap<ConnId, String>,
    pub oedit_text_bufs: HashMap<ConnId, String>,
    /// aedit.rs: durable editor social-action table + loaded flag.
    pub aedit_soc_list: Vec<crate::aedit::SocialAction>,
    pub aedit_soc_loaded: bool,
}

/// World-side boot tables and runtime state that used to live in module
/// statics (phase 1 migration). Grows as families migrate off globals.
/// The DG script VM's boot tables (phase 1 migration; the live script/trigger
/// arenas join here in the DG family step).
#[derive(Default)]
pub struct DgState {
    /// dg_db_scripts.rs trig_index: rnum-ordered trigger prototypes.
    pub proto_trigs: Vec<crate::dg_db_scripts::TrigProto>,
    /// dg_db_scripts.rs: trigger vnum -> proto rnum.
    pub trig_rnum_map: HashMap<i32, usize>,
    /// dg_db_scripts.rs proto_script: (kind, entity vnum) -> bound trigger
    /// vnums in load order.
    pub proto_scripts: HashMap<(i32, i32), Vec<i32>>,
    /// dg_event.rs: the pulse-ticked wait-event queue (sorted by fire time).
    pub events: Vec<crate::dg_event::EventInfo>,
    /// dg_event.rs: monotonically increasing wait-event id source.
    pub next_event_id: u64,
    /// mobact.rs: mob remembered-attacker lists (C mob_specials.memory).
    pub mob_memory: HashMap<CharId, Vec<i64>>,
    /// dg_mobcmd.rs: mob script memory (mremember/mforget, MEMORY triggers).
    pub script_memory: HashMap<CharId, Vec<crate::dg_mobcmd::ScriptMemory>>,
    /// dg_handler.rs: live script containers keyed by (owner kind, id).
    pub scripts: HashMap<crate::dg_handler::ScriptKey, crate::dg_handler::ScriptData>,
    /// dg_handler.rs: the live trigger arena.
    pub trigs: HashMap<crate::dg_handler::TrigId, crate::dg_handler::TrigData>,
    /// dg_handler.rs: per-mob greet/entry memory lists.
    pub dg_memory: HashMap<CharId, Vec<crate::dg_handler::ScriptMemory>>,
    /// dg_handler.rs: monotonically increasing trigger id source.
    pub next_trig_id: u64,
    /// dg_scripts.rs: current script recursion depth (was a thread_local).
    pub script_depth: i32,
    /// dg_scripts.rs: dg_owner_purged latch (was a thread_local).
    pub owner_purged: bool,
    /// dg_scripts.rs: the script-side RNG stream (was a thread_local; the
    /// substitution path is non-reentrant and panic-isolated).
    pub script_rng: std::cell::RefCell<crate::rng::Rng>,
}

/// Social/economy-adjacent player-facing stores that used to live in module
/// statics (phase 1 migration). Grows as families migrate off globals.
#[derive(Default)]
pub struct SocialState {
    /// cmd_social.rs: the live social table.
    pub socials: crate::cmd_social::SocialTable,
    /// hedit.rs: the help table (C help_table[] + top_of_helpt).
    pub help_table: Vec<crate::hedit::HelpEntry>,
    /// hedit.rs: true once the help table has been loaded this run.
    pub help_loaded: bool,
    /// alias.rs: per-player alias lists keyed by persistent idnum
    /// (C GET_ALIASES side lists).
    pub aliases: HashMap<i64, Vec<crate::alias::AliasEntry>>,
    /// boards.rs: the board runtime (messages, formats, quarantine, compose
    /// state).
    pub boards: crate::boards::BoardRuntime,
    /// mail.rs: the mail system (plrmail block store + index).
    pub mail: crate::mail::MailSystem,
    /// mail.rs: connections currently composing a mail body.
    pub mail_pending: HashMap<ConnId, crate::mail::PendingMail>,
    /// ban.rs: the authoritative ban/invalid-name tables.
    pub ban: crate::ban::BanData,
    /// ban.rs: shared snapshot handle for the pre-Game accept path.
    pub ban_handle: crate::ban::BanHandle,
}

#[derive(Default)]
pub struct WorldState {
    /// fight_messages.rs: combat hit-message sets from `<lib>/misc/messages`.
    pub fight_messages: Vec<crate::fight_messages::MessageList>,
    /// spec_assign.rs: the vnum -> special-procedure tables (assign_mobiles/
    /// objects/rooms), built once at boot.
    pub specs: crate::spec_assign::SpecTables,
    /// spec_assign.rs: ROOM_DEATH vnums captured before assign_specs builds
    /// the room table (dts_are_dumps dump registration).
    pub death_trap_rooms: Vec<RoomVnum>,
    /// spec_procs.rs: the mayor's patrol state (castle.c SPECIAL(mayor)).
    pub mayor: crate::spec_procs::MayorState,
    /// town_life.rs: computed caravan routes keyed by mob vnum.
    pub routes: HashMap<MobVnum, Vec<RoomRnum>>,
    /// maputils.rs: parsed worldmap grids keyed by their source file name.
    pub maps: HashMap<String, crate::maputils::MapData>,
    /// castle.rs: King Welmar's patrol state keyed by the mob's CharId.
    pub king_walks: HashMap<CharId, crate::castle::KingWalk>,
}

/// Economy-side stores that used to live in module statics (phase 1).
#[derive(Default)]
pub struct EconomyState {
    /// quest.rs: questgiver side-table (C ch->questgiver pointer).
    pub quest_givers: HashMap<CharId, CharId>,
    /// arena.rs: live arena side-state (per-char arena status, arenamaster,
    /// observer links).
    pub arena: crate::arena::ArenaWorld,
    /// clan.rs: the live clan table (lib/etc/clans.dat mirror).
    pub clans: crate::clan::ClanTable,
    /// house.rs: the house-control table (C house_control[]).
    pub houses: Vec<crate::house::HouseControlRec>,
    /// house.rs: detected persistence format of lib/etc/hcontrol.
    pub house_control_format: crate::cformat::PersistenceFormat,
    /// house.rs: per-house object file persistence format.
    pub house_object_formats: HashMap<RoomVnum, crate::cformat::PersistenceFormat>,
    /// shop.rs: loaded shop table (C shop_index) — added with the shop family.
    pub shops: Vec<crate::shop::ShopData>,
    /// shop.rs: keeper vnum -> captured secondary spec procs.
    pub shop_funcs: Option<HashMap<MobVnum, crate::shop::ShopFn>>,
    /// auction.rs: the live auction house state.
    pub auction: crate::auction::AuctionData,
}

pub struct GameState {
    // Static world (loaded at boot; mutated by resets / OLC).
    pub rooms: Vec<Room>,
    pub room_index: HashMap<RoomVnum, RoomRnum>,
    /// Append-only rnum index for ordinary world rooms. Surface-map cells are
    /// deliberately absent: their `map_x`/`map_y` coordinates identify them
    /// when `add_room` splices them in. Keeping this index alongside `rooms`
    /// lets hot paths visit the roughly hundreds of real rooms without walking
    /// every generated map cell, while OLC-created rooms appended after the map
    /// are included automatically.
    non_map_room_rnums: Vec<RoomRnum>,
    pub zones: Vec<Zone>,
    pub mob_protos: HashMap<MobVnum, MobileProto>,
    pub obj_protos: HashMap<ObjVnum, ObjectProto>,

    // Live instances. `IndexMap` is an ordered map: O(1) insert/get/swap_remove
    // *and* ordered iteration, so it replaces the old `HashMap` + separate
    // `*_list: Vec` (which carried C's character_list/object_list order at the
    // cost of O(n) prepend + O(n) Vec removal on every spawn/extract). Insertion
    // order is preserved (oldest-first); extraction uses swap_remove (O(1),
    // reorders the tail). Iteration order is internal-only — no observable
    // behavior depends on it. The id<->struct lookup is the map itself.
    pub chars: IndexMap<CharId, Character>,
    pub objs: IndexMap<ObjId, Object>,

    // Connections (the Descriptor lives here; the async output channel lives
    // in the Game wrapper keyed by the same ConnId).
    pub descriptors: HashMap<ConnId, Descriptor>,
    /// Deltania Breathes (W5): connections whose GMCP snapshot is stale and
    /// needs re-pushing at the next drain (prompt time / heartbeat end).
    /// Marked by state mutations (movement, combat damage) rather than pushed
    /// blindly after every command.
    pub gmcp_dirty: std::collections::HashSet<ConnId>,
    pub players_by_name: HashMap<String, CharId>,

    // In-memory player name<->idnum index (C `player_table`, built by
    // build_player_index() at boot from a `SELECT idnum,name,level FROM
    // player_main`). Lets name<->id lookups, `last`, and ignore resolve
    // OFFLINE players without an async DB hit. We additionally cache
    // last_logon + host so `do_last` can render an offline player's record
    // straight from the index. Kept fresh by update_player_index() on
    // create/enter/save (the C MUD rebuilds the whole table after a
    // create_entry; we upsert the single row).
    pub player_table: Vec<PlayerIndex>,

    // Deferred immortal commands against OFFLINE players (set/stat/show on a
    // logged-off player's full record). The sync command path can't await the
    // async DB, so it queues an OfflineOp here; the async Game loop (game.rs)
    // drains this each heartbeat — loads the player into the world, replays the
    // command, then saves + extracts. Empty in steady state.
    pub offline_ops: Vec<OfflineOp>,

    /// Deferred durable live-player renames.  `do_rename` cannot await SQL, so
    /// it queues the fully resolved identities here without changing names or
    /// files; `Game::drain_player_rename_requests` owns the commit protocol.
    pub player_rename_requests: Vec<PlayerRenameRequest>,

    /// Password-only writes queued by authenticated Implementors. Success is
    /// not published until the async Game shell confirms the targeted update.
    pub password_update_requests: Vec<PasswordUpdateRequest>,

    /// Password checks for terminal unlock. The command path never performs a
    /// KDF synchronously on the single-owner world thread.
    pub lockout_unlock_requests: Vec<LockoutUnlockRequest>,

    /// Rank/capability CAS operations. These are drained before ordinary
    /// player saves so an older broad snapshot cannot resurrect revoked trust.
    pub authority_update_requests: Vec<AuthorityUpdateRequest>,

    /// Player identities whose durable authority could not be observed after
    /// an ambiguous write. Privileged dispatch and process exit stay blocked
    /// until an exact readback or retry reconciles the tuple.
    pub authority_quarantine: std::collections::HashSet<i64>,

    /// Deferred async DB operations queued from sync command paths - e.g.
    /// clan destroy/rank-lower SQL that must also cover OFFLINE players'
    /// rows (C runs the UPDATEs synchronously; #165).
    pub deferred_db_ops: Vec<DeferredDbOp>,
    pub pfileclean_requested: Option<AuthenticatedCommandRequest>,
    pub player_save_requests: Vec<CharId>,

    /// do_who's `boot_high` (act.informative.c): the highest simultaneous
    /// visible-player count seen this boot, reported as "There is a boot time
    /// high of N players." Updated by do_who as C does; reset on boot.
    pub boot_high: usize,

    next_char_id: u64,
    next_obj_id: u64,

    pub rng: Rng,
    pub credits: String,
    pub news: String,
    pub info: String,
    pub handbook: String,
    pub policies: String,
    pub motd: String,
    pub imotd: String,
    /// C `startup` (text/startup): the pre-login banner sent after the colour
    /// question (db.c STARTUP_FILE) (#198).
    pub startup: String,
    /// C `background` (text/background): main-menu option 3's page (#198).
    pub background: String,
    pub circlemud: String,
    pub config: Config,
    pub pulse: u64,
    /// C comm.c dg_act_check (DG_NO_TRIG): false while a DG script is
    /// executing, so VM-originated act() lines do not re-fire act triggers
    /// recursively (#138).
    pub dg_act_check: bool,
    /// Set by `do_shutdown` or the scheduled reboot clock. The Game run loop
    /// exits through the graceful save path and returns this explicit process
    /// disposition to the systemd-facing wrapper.
    pub shutdown_requested: Option<ShutdownRequest>,
    /// Immortal copyover command request. The async Game shell consumes this
    /// so it can durably await database saves before the synchronous exec.
    pub copyover_requested: Option<AuthenticatedCommandRequest>,
    /// C `pk_allowed` (config.c:53, `int pk_allowed = NO`). The live PvP gate
    /// read by do_hit/do_kill/murder, the killer-flagging path in fight.c and
    /// the PvP spell guards; toggled by `set Legal_PKS ON|OFF`
    /// (act.wizard.c:3914-3921).
    pub pk_allowed: bool,
    /// `nameserver_is_slow` (config.c:254, initialised to YES). Toggled by the
    /// immortal `slowns` command (act.other.c do_gen_toggle SCMD_SLOWNS); the
    /// resolver itself is not modelled, only the reported state.
    pub nameserver_is_slow: bool,

    // --- Owned subsystem state (phase 1 architecture migration) ------------
    // Each sub-struct below replaces a module-static global; the world thread
    // is the single owner. See docs/MODERNIZATION.md and the phase-1 roadmap.
    /// The spell/skill info table (spell_parser.c spell_info[]), immutable
    /// after boot.
    pub spells: crate::spell_parser::SpellTables,
    /// Boot-populated world tables and runtime world-side state that used to
    /// live in module statics (spec assignments, fight messages, surface map,
    /// town routes, special-proc scratch state).
    pub world: WorldState,
    /// Player-facing social stores (socials table; boards/mail/aliases/ban as
    /// families migrate).
    pub social: SocialState,
    /// DG script VM state (prototype tables; live arenas as families migrate).
    pub dg: DgState,
    /// interpreter.rs: whether the in-flight command arrived from the live
    /// Playing descriptor of the acting principal (Indirect for force/queue/DM).
    pub command_source: crate::interpreter::CommandSource,
    /// olc.rs: the OLC core registry (active editors, save journal, unresolved
    /// publications).
    pub olc: OlcState,
    /// The mud calendar + sun state (weather.rs TimeWeather).
    pub clock: crate::weather::MudClock,
    /// Economy stores (quest givers; shops/clans/houses/auction as families
    /// migrate).
    pub econ: EconomyState,

    // Surface ("outside") world-map splice (maputils.c read_map). The 99x99
    // grid of map cells is appended to `rooms` *after* the real-room block, so
    // real-room rnums (and real_room(vnum)) are untouched. `map_start_rnum` is
    // the rnum of the first map cell (1-based grid (1,1)); cell (x,y) lives at
    // `map_start_rnum + (y-1)*max_map_x + (x-1)` (C find_room_by_coords). None
    // until integrate_map_rooms() runs (or the worldmap file is missing).
    pub map_start_rnum: Option<RoomRnum>,
    pub max_map_x: i32,
    pub max_map_y: i32,
}

impl GameState {
    pub fn new(config: Config) -> Self {
        GameState {
            rooms: Vec::new(),
            room_index: HashMap::new(),
            non_map_room_rnums: Vec::new(),
            zones: Vec::new(),
            mob_protos: HashMap::new(),
            obj_protos: HashMap::new(),
            chars: IndexMap::new(),
            objs: IndexMap::new(),
            descriptors: HashMap::new(),
            gmcp_dirty: std::collections::HashSet::new(),
            players_by_name: HashMap::new(),
            player_table: Vec::new(),
            offline_ops: Vec::new(),
            player_rename_requests: Vec::new(),
            password_update_requests: Vec::new(),
            lockout_unlock_requests: Vec::new(),
            authority_update_requests: Vec::new(),
            authority_quarantine: std::collections::HashSet::new(),
            deferred_db_ops: Vec::new(),
            pfileclean_requested: None,
            player_save_requests: Vec::new(),
            boot_high: 0,
            next_char_id: 1,
            next_obj_id: 1,
            rng: Rng::default(),
            shutdown_requested: None,
            copyover_requested: None,
            pk_allowed: false,
            // config.c:254 `int nameserver_is_slow = YES;`
            nameserver_is_slow: true,
            spells: crate::spell_parser::SpellTables::default(),
            world: WorldState::default(),
            social: SocialState::default(),
            dg: DgState::default(),
            command_source: crate::interpreter::CommandSource::Indirect,
            olc: OlcState::default(),
            clock: crate::weather::MudClock::default(),
            econ: EconomyState::default(),
            credits: String::new(),
            news: String::new(),
            info: String::new(),
            handbook: String::new(),
            policies: String::new(),
            motd: String::new(),
            imotd: String::new(),
            startup: String::new(),
            background: String::new(),
            circlemud: String::new(),
            config,
            pulse: 0,
            dg_act_check: true,
            map_start_rnum: None,
            max_map_x: 0,
            max_map_y: 0,
        }
    }

    /// find_room_by_coords (maputils.c): the rnum of the 1-based map cell (x,y),
    /// with the world wrapping (it is "ROUND!"). None when the surface map has
    /// not been spliced in (map_start_rnum is None / dimensions are 0).
    pub fn map_coords_to_rnum(&self, x: i32, y: i32) -> Option<RoomRnum> {
        let start = self.map_start_rnum?;
        if self.max_map_x <= 0 || self.max_map_y <= 0 {
            return None;
        }
        // WRAPX / WRAPY (maputils.c): fold the coordinate into 1..=max.
        let mut nx = x;
        let mut ny = y;
        while nx > self.max_map_x {
            nx -= self.max_map_x;
        }
        while nx < 1 {
            nx += self.max_map_x;
        }
        while ny > self.max_map_y {
            ny -= self.max_map_y;
        }
        while ny < 1 {
            ny += self.max_map_y;
        }
        Some(start + ((ny - 1) * self.max_map_x + (nx - 1)) as usize)
    }

    // ---- Rooms ----------------------------------------------------------
    pub fn real_room(&self, vnum: RoomVnum) -> Option<RoomRnum> {
        self.room_index.get(&vnum).copied()
    }
    pub fn room(&self, rnum: RoomRnum) -> &Room {
        &self.rooms[rnum]
    }
    pub fn room_mut(&mut self, rnum: RoomRnum) -> &mut Room {
        &mut self.rooms[rnum]
    }
    /// C utils.h:223-228 IS_DARK(room): a room is dark when unlit AND
    /// (flagged DARK, OR outdoors-but-not-city at sunset/night). The Rust
    /// Room::is_dark only knew the flag, so nights never darkened outdoor
    /// rooms (#99).
    pub fn is_dark(&self, rnum: RoomRnum) -> bool {
        let Some(room) = self.room_opt(rnum) else {
            return false;
        };
        if room.light != 0 {
            return false;
        }
        if room.room_flags.contains(crate::room::RoomFlags::DARK) {
            return true;
        }
        let sun = crate::weather::sunlight(self);
        let outdoors = !matches!(
            room.sector_type,
            crate::room::SectorType::Inside | crate::room::SectorType::City
        );
        outdoors && (sun == crate::weather::SUN_SET || sun == crate::weather::SUN_DARK)
    }

    pub fn room_opt(&self, rnum: RoomRnum) -> Option<&Room> {
        self.rooms.get(rnum)
    }

    /// Rnums of ordinary world rooms, in insertion order. The room arena is
    /// append-only, so entries remain stable for the lifetime of the process.
    pub(crate) fn non_map_room_rnums(&self) -> &[RoomRnum] {
        &self.non_map_room_rnums
    }

    pub fn add_room(&mut self, room: Room) -> RoomRnum {
        // integrate_map_rooms assigns both coordinates before calling us. All
        // file-loaded and OLC-created rooms leave them unset, including rooms
        // appended after the contiguous surface-map block.
        let is_surface_map_cell = room.map_x.is_some() && room.map_y.is_some();
        let vnum = room.number;
        let rnum = self.rooms.len();
        self.rooms.push(room);
        if !is_surface_map_cell {
            self.non_map_room_rnums.push(rnum);
        }
        // C db.c:2729 real_room scans forward and keeps the FIRST match;
        // insert() overwrote, so duplicate vnums resolved to the LAST room
        // (#241). C also stops loading a file at vnum >= MAX_ROOM_VNUM
        // (500000, structs.h:583) - enforced in the file loader.
        self.room_index.entry(vnum).or_insert(rnum);
        rnum
    }

    // ---- Characters -----------------------------------------------------
    pub fn get_char(&self, id: CharId) -> Option<&Character> {
        self.chars.get(&id)
    }
    pub fn get_char_mut(&mut self, id: CharId) -> Option<&mut Character> {
        self.chars.get_mut(&id)
    }
    pub fn char_exists(&self, id: CharId) -> bool {
        self.chars.contains_key(&id)
    }

    /// Resolve the player principal behind `target`, including either half of
    /// a switched descriptor relationship.
    ///
    /// The relationship is accepted only when every forward/reverse alias is
    /// unique and symmetric: the descriptor map key must equal its embedded
    /// id, its active body must point back to that descriptor, a switched
    /// original must be a detached PC, and neither identity may be referenced
    /// by another descriptor.  Broken aliases and persisted PC trust outside
    /// 0..=LVL_IMPL fail closed.  A genuinely descriptorless NPC is the sole
    /// case where display level is returned as authority, for non-admin game
    /// mechanics that historically compare NPC levels.
    pub fn principal_authority(&self, target: CharId) -> Option<PrincipalAuthority> {
        let target_character = self.get_char(target)?;
        let mut matching = self.descriptors.iter().filter(|(_, descriptor)| {
            descriptor.character == Some(target) || descriptor.original == Some(target)
        });
        let descriptor = matching.next();
        if matching.next().is_some() {
            return None;
        }

        let Some((&descriptor_key, descriptor)) = descriptor else {
            // A stale Character::desc is a broken session link, not an
            // ordinary descriptorless character.
            if target_character.desc.is_some() {
                return None;
            }
            if target_character.is_npc {
                return Some(PrincipalAuthority {
                    principal: target,
                    authority: i32::from(target_character.player.level),
                    descriptor: None,
                    descriptor_controls_target: false,
                    switched_session: false,
                    principal_is_player: false,
                });
            }
            let trust = target_character.trust;
            if !(0..=i32::from(LVL_IMPL)).contains(&trust) {
                return None;
            }
            return Some(PrincipalAuthority {
                principal: target,
                authority: trust,
                descriptor: None,
                descriptor_controls_target: false,
                switched_session: false,
                principal_is_player: true,
            });
        };

        if descriptor.id != descriptor_key {
            return None;
        }
        let body = descriptor.character?;
        let original = descriptor.original;
        if original == Some(body) {
            return None;
        }
        let body_character = self.get_char(body)?;
        if body_character.desc != Some(descriptor_key) {
            return None;
        }

        let principal = original.unwrap_or(body);
        let principal_character = self.get_char(principal)?;
        if principal_character.is_npc {
            return None;
        }
        if original.is_some() && principal_character.desc.is_some() {
            return None;
        }
        let trust = principal_character.trust;
        if !(0..=i32::from(LVL_IMPL)).contains(&trust) {
            return None;
        }
        // A malformed switched-to PC row is not allowed to hide invalid
        // persisted authority merely because that PC is not the principal.
        if !body_character.is_npc && !(0..=i32::from(LVL_IMPL)).contains(&body_character.trust) {
            return None;
        }

        // The matching descriptor must be the only descriptor that aliases
        // either side of this session.  This catches duplicate forward links,
        // duplicate originals, and cross-linked switched sessions.
        if self.descriptors.iter().any(|(&other_key, other)| {
            other_key != descriptor_key
                && [other.character, other.original]
                    .into_iter()
                    .flatten()
                    .any(|id| id == body || id == principal)
        }) {
            return None;
        }

        Some(PrincipalAuthority {
            principal,
            authority: trust,
            descriptor: Some(descriptor_key),
            descriptor_controls_target: descriptor.character == Some(target),
            switched_session: original.is_some(),
            principal_is_player: true,
        })
    }

    /// Revalidate the live account, hierarchy, and granular grant behind a
    /// snoop. Snoop is a continuing disclosure, so authorization must remain
    /// true for every relayed write rather than only when the link is created.
    pub(crate) fn can_start_snoop(&self, snooper: CharId, target: CharId) -> bool {
        if snooper == target {
            return false;
        }
        let Some(snooper_authority) = self
            .principal_authority(snooper)
            .filter(|authority| authority.is_authenticated_player())
        else {
            return false;
        };
        let Some(descriptor) = snooper_authority.descriptor else {
            return false;
        };
        let Some(session) = self.descriptors.get(&descriptor) else {
            return false;
        };
        if !snooper_authority.descriptor_controls_target
            || session.state != ConState::Playing
            || session.character != Some(snooper)
        {
            return false;
        }
        let Some(principal) = self.get_char(snooper_authority.principal) else {
            return false;
        };
        if self.authority_quarantine.contains(&principal.idnum)
            || snooper_authority.authority < i32::from(LVL_IMMORT)
            || principal.godcmds1 & crate::gcmd::GCMD_SNOOP == 0
        {
            return false;
        }

        let Some(target_authority) = self.principal_authority(target) else {
            return false;
        };
        if target_authority.principal_is_player {
            let Some(target_principal) = self.get_char(target_authority.principal) else {
                return false;
            };
            if self.authority_quarantine.contains(&target_principal.idnum) {
                return false;
            }
        }
        snooper_authority.authority > target_authority.authority
    }

    pub(crate) fn snoop_link_is_authorized(&self, snooper: CharId, target: CharId) -> bool {
        self.get_char(snooper)
            .is_some_and(|character| character.snooping == Some(target))
            && self
                .get_char(target)
                .is_some_and(|character| character.snoop_by == Some(snooper))
            && self.can_start_snoop(snooper, target)
    }

    fn sever_snoop_pair(&mut self, snooper: CharId, target: CharId) {
        if let Some(character) = self.get_char_mut(snooper)
            && character.snooping == Some(target)
        {
            character.snooping = None;
        }
        if let Some(character) = self.get_char_mut(target)
            && character.snoop_by == Some(snooper)
        {
            character.snoop_by = None;
        }
    }

    /// Tear down stale or newly unauthorized disclosure links immediately
    /// after authority/grant changes. send_to_char repeats the same check at
    /// use time so no other mutation path can leave a usable stale link.
    pub(crate) fn revalidate_snoop_links(&mut self) {
        let links: Vec<(CharId, CharId)> = self
            .chars
            .iter()
            .filter_map(|(&snooper, character)| character.snooping.map(|target| (snooper, target)))
            .collect();
        for (snooper, target) in links {
            if !self.snoop_link_is_authorized(snooper, target) {
                self.sever_snoop_pair(snooper, target);
            }
        }

        // Also clear one-sided reverse links which had no outgoing entry.
        let stale_reverse: Vec<(CharId, CharId)> = self
            .chars
            .iter()
            .filter_map(|(&target, character)| {
                character.snoop_by.and_then(|snooper| {
                    (!self
                        .get_char(snooper)
                        .is_some_and(|character| character.snooping == Some(target)))
                    .then_some((snooper, target))
                })
            })
            .collect();
        for (snooper, target) in stale_reverse {
            self.sever_snoop_pair(snooper, target);
        }
    }

    /// Insert a character into the world (assigns id, appends to the ordered
    /// arena — CircleMUD prepends to character_list, but iteration order is
    /// internal-only here, so O(1) append is used). Does NOT place it in a room.
    pub fn create_char(&mut self, mut ch: Character) -> CharId {
        let id = CharId(self.next_char_id);
        self.next_char_id += 1;
        ch.id = id;
        self.chars.insert(id, ch);
        id
    }

    /// Snapshot of all live character ids (insertion order). Replaces the old
    /// `char_list.clone()` the hot loops took before iterating + mutating.
    pub fn char_ids(&self) -> Vec<CharId> {
        self.chars.keys().copied().collect()
    }

    /// Snapshot of all live object ids (insertion order).
    pub fn obj_ids(&self) -> Vec<ObjId> {
        self.objs.keys().copied().collect()
    }

    pub fn find_player_by_name(&self, name: &str) -> Option<CharId> {
        self.players_by_name.get(&name.to_lowercase()).copied()
    }

    // ---- Player index (C player_table / get_id_by_name / get_name_by_id) ----

    /// get_id_by_name() (db.c): the persistent idnum for `name`, or None if no
    /// player by that name exists. Case-insensitive (C lowercases both sides),
    /// resolves OFFLINE players from the boot-loaded index. Like C, only the
    /// first whitespace token of `name` is considered.
    pub fn get_id_by_name(&self, name: &str) -> Option<i64> {
        let arg = name.split_whitespace().next().unwrap_or("");
        if arg.is_empty() {
            return None;
        }
        self.player_table
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(arg))
            .map(|p| p.idnum)
    }

    /// get_name_by_id() (db.c): the stored (canonical-cased) name for an idnum,
    /// or None. Resolves offline players from the index.
    pub fn get_name_by_id(&self, id: i64) -> Option<String> {
        self.player_table
            .iter()
            .find(|p| p.idnum == id)
            .map(|p| p.name.clone())
    }

    /// The full index row for `name` (level / last_logon / host), or None.
    /// Case-insensitive; used by `do_last` to render an offline player.
    pub fn player_index(&self, name: &str) -> Option<&PlayerIndex> {
        let arg = name.split_whitespace().next().unwrap_or("");
        if arg.is_empty() {
            return None;
        }
        self.player_table
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(arg))
    }

    /// UPSERT a player_table row (keyed on idnum), keeping the index fresh as
    /// players are created/saved/enter. C rebuilds the whole table after a
    /// create_entry(); we update the single row in place (or append it). A
    /// negative idnum or empty name is ignored (mobs / not-yet-allocated).
    pub fn update_player_index(
        &mut self,
        idnum: i64,
        name: &str,
        level: u8,
        last_logon: i64,
        host: &str,
    ) {
        if idnum < 0 || name.is_empty() {
            return;
        }
        if let Some(p) = self.player_table.iter_mut().find(|p| p.idnum == idnum) {
            p.name = name.to_string();
            p.level = level;
            // This compatibility helper is primarily used by fixtures and
            // callers without a Character. Treat its supplied rank as both
            // display and authority; production Character updates overwrite
            // trust with the exact persisted value below.
            p.trust = i32::from(level);
            p.last_logon = last_logon;
            // Preserve a known host if the caller has none (a save with no live
            // descriptor shouldn't blank the host the last login recorded).
            if !host.is_empty() {
                p.host = host.to_string();
            }
        } else {
            self.player_table.push(PlayerIndex {
                idnum,
                name: name.to_string(),
                level,
                trust: i32::from(level),
                class: crate::types::Class::Warrior,
                last_logon,
                host: host.to_string(),
                act_flags: 0,
                clan: -1,
                clan_rank: -1,
            });
        }
    }

    pub fn update_player_index_from_character(
        &mut self,
        ch: &crate::character::Character,
        last_logon: i64,
        host: &str,
    ) {
        self.update_player_index(ch.idnum, ch.get_name(), ch.player.level, last_logon, host);
        if let Some(p) = self.player_table.iter_mut().find(|p| p.idnum == ch.idnum) {
            p.trust = ch.trust;
            p.class = ch.player.class;
            p.act_flags = ch.act_flags;
            p.clan = ch.clan;
            p.clan_rank = ch.clan_rank;
        }
    }

    /// Queue a deferred immortal command against an OFFLINE player (the async
    /// bridge for set/stat/show on a logged-off record). Called from cmd_wizard
    /// when the target is offline-but-indexed; drained next heartbeat by game.rs.
    /// `command` must be the immortal's ORIGINAL command verbatim so the replay
    /// re-parses the identical field/value.
    pub fn queue_offline_op(
        &mut self,
        requester: CharId,
        target: &str,
        command: &str,
        authority: OfflineOpAuthority,
    ) {
        self.offline_ops.push(OfflineOp {
            requester,
            target: target.to_string(),
            command: command.to_string(),
            authority,
        });
    }

    pub fn queue_player_rename(
        &mut self,
        authorization: AuthenticatedCommandRequest,
        victim: CharId,
        idnum: i64,
        old_name: &str,
        new_name: &str,
    ) {
        self.player_rename_requests.push(PlayerRenameRequest {
            authorization,
            victim,
            idnum,
            old_name: old_name.to_string(),
            new_name: new_name.to_string(),
        });
    }

    pub fn take_player_rename_requests(&mut self) -> Vec<PlayerRenameRequest> {
        std::mem::take(&mut self.player_rename_requests)
    }

    pub fn queue_password_update(
        &mut self,
        authorization: AuthenticatedCommandRequest,
        victim: CharId,
        idnum: i64,
        name: &str,
        plaintext_password: String,
    ) {
        self.password_update_requests.push(PasswordUpdateRequest {
            authorization,
            victim,
            idnum,
            name: name.to_string(),
            plaintext_password,
        });
    }

    pub fn take_password_update_requests(&mut self) -> Vec<PasswordUpdateRequest> {
        std::mem::take(&mut self.password_update_requests)
    }

    pub fn queue_lockout_unlock(&mut self, request: LockoutUnlockRequest) {
        self.lockout_unlock_requests.push(request);
    }

    pub fn take_lockout_unlock_requests(&mut self) -> Vec<LockoutUnlockRequest> {
        std::mem::take(&mut self.lockout_unlock_requests)
    }

    pub fn queue_authority_update(&mut self, request: AuthorityUpdateRequest) {
        self.authority_update_requests.push(request);
    }

    pub fn take_authority_update_requests(&mut self) -> Vec<AuthorityUpdateRequest> {
        std::mem::take(&mut self.authority_update_requests)
    }

    /// One target-authority predicate for `stat player`, `stat file`, and
    /// `show player`, whether the target is online, indexed, freshly loaded,
    /// or raced online while a deferred operation was waiting. DeltaMUD's C
    /// `stat file` rule denies only a target *above* the requester, so equal
    /// authority ranks remain inspectable.
    pub fn can_inspect_player_authority(&self, requester: CharId, target_trust: i32) -> bool {
        (0..=i32::from(crate::types::LVL_IMPL)).contains(&target_trust)
            && self
                .principal_authority(requester)
                .filter(|authority| authority.is_authenticated_player())
                .is_some_and(|authority| authority.authority >= target_trust)
    }

    /// Revalidate an authenticated request immediately before its queued
    /// destructive effect. This deliberately repeats the dispatcher gate:
    /// descriptor ownership, principal identity, persisted trust, quarantine,
    /// and the granular command bit may all change while earlier async work is
    /// draining.
    pub(crate) fn authenticated_command_request_is_current(
        &self,
        request: AuthenticatedCommandRequest,
        minimum_authority: i32,
        godcmd_set: usize,
        godcmd: i64,
    ) -> bool {
        if !self.authenticated_session_request_is_current(request, minimum_authority) {
            return false;
        }
        let Some(principal) = self.get_char(request.requester_principal) else {
            return false;
        };
        let grants = [
            principal.godcmds1,
            principal.godcmds2,
            principal.godcmds3,
            principal.godcmds4,
        ];
        let Some(bits) = godcmd_set
            .checked_sub(1)
            .and_then(|index| grants.get(index))
        else {
            return false;
        };
        godcmd != 0 && bits & godcmd != 0
    }

    /// Revalidate an exact authenticated session without imposing a granular
    /// administrator grant. Long-lived ordinary editors (notably boards) use
    /// this to recheck descriptor ownership, persisted trust and quarantine at
    /// their eventual publication boundary.
    pub(crate) fn authenticated_session_request_is_current(
        &self,
        request: AuthenticatedCommandRequest,
        minimum_authority: i32,
    ) -> bool {
        let Some(authority) = self
            .principal_authority(request.requester_body)
            .filter(|authority| authority.is_authenticated_player())
        else {
            return false;
        };
        if authority.principal != request.requester_principal
            || authority.descriptor != Some(request.descriptor)
            || !authority.descriptor_controls_target
            || authority.authority < minimum_authority
        {
            return false;
        }
        let Some(descriptor) = self.descriptors.get(&request.descriptor) else {
            return false;
        };
        if descriptor.state != ConState::Playing
            || descriptor.character != Some(request.requester_body)
        {
            return false;
        }
        let Some(principal) = self.get_char(request.requester_principal) else {
            return false;
        };
        if principal.is_npc
            || principal.idnum != request.idnum
            || self.authority_quarantine.contains(&request.idnum)
        {
            return false;
        }
        true
    }

    /// Queue `pfileclean`'s async DB cleanup. The command path is synchronous,
    /// so game.rs drains this between awaits and rebuilds player_table after
    /// deleting PLR_DELETED rows from persistent storage.
    pub fn queue_pfileclean(&mut self, request: AuthenticatedCommandRequest) -> bool {
        if self.pfileclean_requested.is_some() {
            return false;
        }
        self.pfileclean_requested = Some(request);
        true
    }

    pub fn take_pfileclean_request(&mut self) -> Option<AuthenticatedCommandRequest> {
        self.pfileclean_requested.take()
    }

    /// Queue a live PC row save for the async game loop. This is the sync
    /// command equivalent of C `save_char(ch, NOWHERE)` for handlers that cannot
    /// await the database directly.
    pub fn request_player_save(&mut self, ch: CharId) {
        if !self.player_save_requests.contains(&ch) {
            self.player_save_requests.push(ch);
        }
    }

    pub fn take_player_save_requests(&mut self) -> Vec<CharId> {
        std::mem::take(&mut self.player_save_requests)
    }

    // ---- Objects --------------------------------------------------------
    pub fn get_obj(&self, id: ObjId) -> Option<&Object> {
        self.objs.get(&id)
    }
    pub fn get_obj_mut(&mut self, id: ObjId) -> Option<&mut Object> {
        self.objs.get_mut(&id)
    }
    pub fn create_obj(&mut self, mut obj: Object) -> ObjId {
        let id = ObjId(self.next_obj_id);
        self.next_obj_id += 1;
        obj.id = id;
        self.objs.insert(id, obj);
        id
    }

    /// Remove an object from the world entirely, including its contents.
    ///
    /// The complete containment graph is validated before the root is
    /// detached.  This ordering is load-bearing: a corrupt cycle, duplicate
    /// parent, missing identity, or excessive depth must leave both the arena
    /// and the root's room/character/container attachment unchanged.
    /// Returns `true` only when the whole extraction completed.
    pub fn extract_obj(&mut self, id: ObjId) -> bool {
        self.extract_objs([id])
    }

    /// Atomically extract multiple independent roots. All graphs and worn
    /// attachments pass preflight before any root is detached, so batch
    /// cleanup cannot remove an early root and then fail on a later one.
    pub fn extract_objs<I>(&mut self, roots: I) -> bool
    where
        I: IntoIterator<Item = ObjId>,
    {
        let roots: Vec<ObjId> = roots.into_iter().collect();
        let walk = walk_object_graph(
            roots.iter().copied(),
            ObjectGraphOrder::Postorder,
            "extract_objs",
            |oid| self.objs.get(&oid).map(|o| o.contains.clone()),
        );
        if walk.malformed() {
            log::warn!(
                "SYSERR: extract_objs({:?}) left the object graph unchanged because containment was malformed",
                roots
            );
            return false;
        }

        // A valid forward walk is not enough if a child's reciprocal location
        // points somewhere else. Refuse that inconsistency before deleting
        // either identity and leaving the other side dangling.
        for visit in &walk.visits {
            let Some(parent) = self.objs.get(&visit.id) else {
                unreachable!("object extraction walk emitted a missing identity");
            };
            for &child in &parent.contains {
                let child_location = self.objs.get(&child).map(|object| object.loc);
                if child_location != Some(ObjLoc::Contained(visit.id)) {
                    log::warn!(
                        "SYSERR: extract_objs rejected non-reciprocal containment: parent {:?} vnum {} lists child {:?} vnum {:?}, whose location is {:?}; graph unchanged",
                        visit.id,
                        parent.item_number,
                        child,
                        self.objs.get(&child).map(|object| object.item_number),
                        child_location,
                    );
                    return false;
                }
            }
        }

        // Validate every declared root attachment before detaching the first.
        // Contained roots also validate the bounded upward ancestry used for
        // weight propagation. This remains proportional to the target graph
        // and its declared attachment lists, never to the synthetic world.
        for &root in &roots {
            let Some(location) = self.objs.get(&root).map(|object| object.loc) else {
                unreachable!("object extraction preflight accepted a missing root");
            };
            let attachment_valid = match location {
                ObjLoc::Room(room) => self.rooms.get(room).is_some_and(|room| {
                    room.contents.iter().filter(|&&id| id == root).count() == 1
                }),
                ObjLoc::Carried(character) => self
                    .chars
                    .get(&character)
                    .is_some_and(|ch| ch.carrying.iter().filter(|&&id| id == root).count() == 1),
                ObjLoc::Worn(character, position) => {
                    self.chars
                        .get(&character)
                        .and_then(|ch| ch.equipment.get(position))
                        .copied()
                        .flatten()
                        == Some(root)
                }
                ObjLoc::Contained(parent) => {
                    let direct_parent_valid = self.objs.get(&parent).is_some_and(|object| {
                        object.contains.iter().filter(|&&id| id == root).count() == 1
                    });
                    let mut current = parent;
                    let mut ancestors = std::collections::HashSet::new();
                    let mut ancestry_valid = direct_parent_valid;
                    while ancestry_valid {
                        if current == root
                            || !ancestors.insert(current)
                            || ancestors.len() >= crate::object::MAX_OBJECT_GRAPH_DEPTH
                        {
                            ancestry_valid = false;
                            break;
                        }
                        let Some(object) = self.objs.get(&current) else {
                            ancestry_valid = false;
                            break;
                        };
                        match object.loc {
                            ObjLoc::Contained(next) => {
                                ancestry_valid = self.objs.get(&next).is_some_and(|parent| {
                                    parent.contains.iter().filter(|&&id| id == current).count() == 1
                                });
                                current = next;
                            }
                            _ => break,
                        }
                    }
                    ancestry_valid
                }
                ObjLoc::Nowhere => true,
            };
            if !attachment_valid {
                let vnum = self.objs.get(&root).map(|object| object.item_number);
                log::warn!(
                    "SYSERR: extract_objs rejected inconsistent root attachment: object {:?} vnum {:?}, location {:?}; graph unchanged",
                    root,
                    vnum,
                    location,
                );
                return false;
            }
        }

        // Match C extract_obj(): unlink each root from wherever it lives, but
        // only after every recursive operation is guaranteed to succeed. The
        // descendants need no individual detach because their sole parent is
        // part of the same validated postorder removal.
        for root in roots {
            match self.objs.get(&root).map(|object| object.loc) {
                Some(ObjLoc::Worn(character, position)) => {
                    let _ = self.unequip_char(character, position);
                }
                Some(_) => self.obj_from_anywhere(root),
                None => unreachable!("object extraction preflight accepted a missing root"),
            }
        }
        for visit in walk.visits {
            // shift_remove (NOT swap_remove): swap_remove moves the
            // last-inserted entry into the vacated slot while its .id keeps
            // the OLD value, so a stale id held across an extraction would
            // resolve to a DIFFERENT object instead of None. Extraction is
            // rare; O(n) is fine.
            self.objs.shift_remove(&visit.id);
        }
        true
    }

    /// WAIT_STATE(ch, cycles) (utils.h): impose `cycles` pulses of command lag.
    /// PCs store this on their descriptor; NPCs use char_specials.wait_state
    /// (`mob_wait`) so perform_violence can count down bash/trip recovery.
    pub fn set_wait_state(&mut self, id: CharId, cycles: i32) {
        if let Some(c) = self.chars.get_mut(&id) {
            if c.is_npc {
                c.mob_wait = cycles;
                return;
            }
        }
        if let Some(conn) = self.chars.get(&id).and_then(|c| c.desc) {
            if let Some(d) = self.descriptors.get_mut(&conn) {
                d.wait = cycles;
            }
        }
    }

    // ---- Output ---------------------------------------------------------
    /// Append raw text to a character's connection buffer (C send_to_char).
    pub fn send_to_char(&mut self, id: CharId, msg: &str) {
        if msg.is_empty() {
            return;
        }
        let snooper = self.chars.get(&id).and_then(|c| c.snoop_by);
        let authorized_snooper =
            snooper.filter(|&snooper| self.snoop_link_is_authorized(snooper, id));
        if let Some(snooper) = snooper
            && authorized_snooper.is_none()
        {
            self.sever_snoop_pair(snooper, id);
        }

        let conn = match self.chars.get(&id).and_then(|c| c.desc) {
            Some(c) => c,
            None => return,
        };
        if let Some(d) = self.descriptors.get_mut(&conn) {
            d.write(msg);
        }
        // Snoop relay (comm.c process_output): if this character is being
        // snooped, tee its output to the snooper, prefixed "% " / suffixed "%%".
        if let Some(snooper) = authorized_snooper {
            if let Some(sconn) = self.chars.get(&snooper).and_then(|c| c.desc) {
                if let Some(sd) = self.descriptors.get_mut(&sconn) {
                    sd.write("% ");
                    sd.write(msg);
                    sd.write("%%");
                }
            }
        }
    }

    /// Convenience: append a line (adds CRLF), matching most C send_to_char
    /// callers that include "\r\n".
    pub fn send_line(&mut self, id: CharId, msg: &str) {
        let conn = match self.chars.get(&id).and_then(|c| c.desc) {
            Some(c) => c,
            None => return,
        };
        if let Some(d) = self.descriptors.get_mut(&conn) {
            d.write(msg);
            d.write("\r\n");
        }
    }

    /// Send to everyone in a room except optionally one character.
    pub fn send_to_room(&mut self, rnum: RoomRnum, msg: &str, exclude: Option<CharId>) {
        let people = match self.rooms.get(rnum) {
            Some(r) => r.people.clone(),
            None => return,
        };
        for id in people {
            if Some(id) == exclude {
                continue;
            }
            self.send_to_char(id, msg);
        }
    }

    /// Send to every playing descriptor (for shouts / wiznet later).
    pub fn send_to_all_players(&mut self, msg: &str) {
        let ids: Vec<CharId> = self.players_by_name.values().copied().collect();
        for id in ids {
            self.send_to_char(id, msg);
        }
    }

    // ---- Misc -----------------------------------------------------------
    /// Equivalent of CircleMUD's GET_ROOM_VNUM(IN_ROOM(ch)).
    pub fn char_room_vnum(&self, id: CharId) -> Option<RoomVnum> {
        let rnum = self.chars.get(&id)?.in_room?;
        Some(self.rooms[rnum].number)
    }

    /// Set PLR_CRASH on a non-NPC (C handler.c SET_BIT(PLR_FLAGS, PLR_CRASH)
    /// in obj_to_char / obj_from_char / (un)equip_char). Flags the player for the
    /// next crash_save_all so carried/worn objects survive a crash/restart
    /// (BUG 14). No-op for NPCs / missing chars.
    pub fn mark_crash(&mut self, cid: CharId) {
        if let Some(c) = self.chars.get_mut(&cid) {
            if !c.is_npc {
                c.act_flags |= crate::objsave::PLR_CRASH;
            }
        }
    }

    /// Detach an object from wherever it currently sits (room/char/container).
    /// Leaves the object in the arena with loc = Nowhere.
    pub fn obj_from_anywhere(&mut self, oid: ObjId) {
        let loc = match self.objs.get(&oid) {
            Some(o) => o.loc,
            None => return,
        };
        match loc {
            ObjLoc::Room(rnum) => {
                if let Some(r) = self.rooms.get_mut(rnum) {
                    r.contents.retain(|&o| o != oid);
                }
            }
            ObjLoc::Carried(cid) => {
                // Mirror C obj_from_char (handler.c:551): drop from the carry
                // list AND decrement the carrier's encumbrance (BUG 7 — the
                // weight/count were leaking, so get->drop netted a positive
                // weight every round). Symmetric with obj_to_char, which adds
                // both. Then flag PLR_CRASH (BUG 14).
                let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
                if let Some(c) = self.chars.get_mut(&cid) {
                    c.carrying.retain(|&o| o != oid);
                    c.carry_weight -= weight;
                    c.carry_items = c.carry_items.saturating_sub(1);
                }
                self.mark_crash(cid);
            }
            ObjLoc::Worn(cid, pos) => {
                // Worn items are NOT counted in carry_weight/carry_items (C
                // equip_char never adds them), so removing one only clears the
                // slot — no encumbrance adjustment. Flag PLR_CRASH (BUG 14).
                if let Some(c) = self.chars.get_mut(&cid) {
                    if pos < NUM_WEARS && c.equipment[pos] == Some(oid) {
                        c.equipment[pos] = None;
                    }
                }
                self.mark_crash(cid);
            }
            ObjLoc::Contained(container) => {
                let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
                if let Some(c) = self.objs.get_mut(&container) {
                    c.contains.retain(|&o| o != oid);
                }
                self.adjust_container_chain_weight(container, -weight);
            }
            ObjLoc::Nowhere => {}
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Nowhere;
        }
    }
}

/// Structural regression gate for the phase-1 statics retirement: the crate
/// must not grow new `OnceLock`/`thread_local!` process globals outside the
/// documented allowlist. Each allowlisted entry is either (a) genuinely
/// cross-task infrastructure the single-owner design requires, or (b) a
/// recursion/re-entrancy latch that carries no game state.
/// Inventory/documentation test: every GameState-owned sub-struct is touched
/// here, so removing or renaming one breaks this test and forces the copyover
/// snapshot discussion to be revisited (the statics-era copyover could not see
/// any of this state at all).
#[cfg(test)]
mod gamestate_inventory {
    use super::*;
    #[test]
    fn every_owned_sub_struct_is_present_and_defaulted() {
        let g = GameState::new(crate::config::Config::default());

        // spells: spell_info table built at GameState construction.
        assert!(!g.spells.info.is_empty());
        // world tables default empty/absent.
        assert!(g.world.fight_messages.is_empty());
        assert!(!g.world.specs.built);
        assert!(g.world.death_trap_rooms.is_empty());
        let _ = g.world.mayor;
        assert!(g.world.routes.is_empty());
        assert!(g.world.maps.is_empty());
        assert!(g.world.king_walks.is_empty());
        // econ stores default empty.
        assert!(g.econ.quest_givers.is_empty());
        assert!(g.econ.arena.chars.is_empty());
        assert!(g.econ.clans.clans.is_empty());
        assert!(g.econ.houses.is_empty());
        assert!(g.econ.house_object_formats.is_empty());
        assert!(g.econ.shops.is_empty());
        assert!(g.econ.shop_funcs.is_none());
        // social stores default empty.
        assert!(g.social.socials.list.is_empty());
        assert!(g.social.help_table.is_empty());
        assert!(!g.social.help_loaded);
        assert!(g.social.aliases.is_empty());
        assert_eq!(g.social.boards.boards.len(), 13); // NUM_OF_BOARDS, each empty
        assert!(g.social.mail_pending.is_empty());
        assert!(g.social.ban.ban_list.is_empty());
        // dg prototype tables + runtime arenas default empty; script rng seeded.
        assert!(g.dg.proto_trigs.is_empty());
        assert!(g.dg.proto_scripts.is_empty());
        assert!(g.dg.events.is_empty());
        // next_event_id starts at 0 (first add_event consumes id 0).
        assert!(g.dg.mob_memory.is_empty());
        assert!(g.dg.script_memory.is_empty());
        assert!(g.dg.scripts.is_empty());
        assert!(g.dg.trigs.is_empty());
        assert!(g.dg.dg_memory.is_empty());
        // next_trig_id starts at 0 (first install_trig consumes id 0).
        assert_eq!(g.dg.script_depth, 0);
        assert!(!g.dg.owner_purged);
        // clock defaults to the C epoch fallback.
        assert_eq!(g.clock.tw.year, 1000);
        // command dispatch starts indirect.
        assert_eq!(
            g.command_source,
            crate::interpreter::CommandSource::Indirect
        );
        // olc registry defaults empty.
        assert!(g.olc.active.is_empty());
        assert!(g.olc.save_list.is_empty());
        assert!(g.olc.unresolved.is_empty());
        assert!(g.olc.edits.is_empty());
        assert!(g.olc.pagers.is_empty());
        assert!(g.olc.redit_states.is_empty());
        assert!(g.olc.oedit_states.is_empty());
        assert!(g.olc.medit_states.is_empty());
        assert!(g.olc.zedit_states.is_empty());
        assert!(g.olc.sedit_states.is_empty());
        assert!(g.olc.aedit_states.is_empty());
        assert!(g.olc.hedit_states.is_empty());
        assert!(g.olc.trigedit_states.is_empty());
        assert!(g.olc.redit_text_bufs.is_empty());
        assert!(g.olc.oedit_text_bufs.is_empty());
        assert!(g.olc.aedit_soc_list.is_empty());
        assert!(!g.olc.aedit_soc_loaded);
    }
}

#[cfg(test)]
mod static_freedom_gate {
    #[test]
    fn module_statics_stay_within_the_documented_allowlist() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let src_dir = std::path::Path::new(manifest).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let allow: &[&str] = &[
            // Cross-task infrastructure (async edges), not world state:
            "src/password.rs", // Argon2id semaphore shared with spawn_blocking
            "src/olc.rs",      // atomic-publication lock + temp-name sequence
            "src/state.rs",    // LISTENER_FD published by main before Game owns state
            // Recursion/re-entrancy latches (no game data, cleared per call):
            "src/spec_assign.rs",     // SPEC_DEPTH
            "src/cmd_informative.rs", // LOC_DEPTH
            // Test-only guards/fault injection:
            "src/shop.rs",
            "src/spells.rs",
            "src/mail.rs",
            "src/game.rs",
            "src/clan.rs",
            "src/arena.rs",
            "src/dg_handler.rs",
            "src/cmd_social.rs",
            "src/hedit.rs",
            "src/town_life.rs",
            "src/cmd_other.rs",
        ];
        let mut entries: Vec<std::path::PathBuf> = Vec::new();
        collect_rs(&src_dir, &mut entries);
        for file in &entries {
            let rel = file.strip_prefix(&src_dir).unwrap();
            let rel_display = format!("src/{}", rel.display());
            let content = std::fs::read_to_string(file).unwrap_or_default();
            for (line_no, line) in content.lines().enumerate() {
                let in_comment = line.trim_start().starts_with("//");
                let in_test_cfg = content.contains("#[cfg(test)]");
                let _ = in_test_cfg;
                if in_comment {
                    continue;
                }
                if (line.contains("OnceLock<") || line.contains("thread_local!"))
                    && !allow.contains(&rel_display.as_str())
                {
                    offenders.push(format!("{}:{}: {}", rel_display, line_no + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "new process-global statics appeared outside the allowlist:\n{}",
            offenders.join("\n")
        );
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs(&path, out);
                } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    out.push(path);
                }
            }
        }
    }
}
