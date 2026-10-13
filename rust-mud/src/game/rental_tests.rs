//! Rental command -> descriptor teardown -> equipment reload regressions.
use super::*;
use crate::cformat::{CRentInfo, PersistenceFormat};
use crate::character::Character;
use crate::config::Config;
use crate::mock_database::MockDatabase;
use crate::object::{ExtraFlags, ObjectType, WearFlags};
use crate::room::{Room, RoomFlags};
use crate::world::ObjectProto;

pub(super) fn rental_proto(vnum: i32, kind: ObjectType) -> ObjectProto {
    ObjectProto {
        vnum,
        name: format!("gear{vnum}"),
        short_desc: format!("gear {vnum}"),
        description: "Equipment is here.".into(),
        obj_type: kind,
        wear_flags: WearFlags::TAKE,
        extra_flags: ExtraFlags::empty(),
        weight: 1,
        cost: 25,
        rent: 5,
        values: [0; 4],
        curr_slots: 0,
        total_slots: 0,
        obj_class: -1,
        min_level: 0,
        bitvector: 0,
        action_description: String::new(),
        affects: Vec::new(),
        ex_descriptions: Vec::new(),
    }
}

async fn rental_game(
    format: PersistenceFormat,
    mode: &str,
) -> (Game, CharId, ConnId, std::path::PathBuf, ObjId) {
    let path = std::env::temp_dir().join(format!(
        "deltamud-rental-lifecycle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    let mut config = Config::default();
    config.lib_path = path.to_string_lossy().into_owned();
    let mut state = GameState::new(config);
    let mut room = Room::new(150, 0, "Inn".into(), "A quiet inn.".into());
    if mode == "camp" {
        room.map_x = Some(5);
        room.map_y = Some(8);
    }
    if mode == "bed" {
        room.room_flags |= RoomFlags::HOUSE;
    }
    let room = state.add_room(room);
    state.add_room(Room::new(3, 0, "Idle rent".into(), "A rental room.".into()));
    let void = state.add_room(Room::new(1, 0, "Void".into(), "An idle void.".into()));
    let db = Arc::new(MockDatabase::new());
    let mut player = Character::new_player("Rental".into(), Class::Warrior, Race::Human);
    player.player.level = 20;
    player.points.gold = 100_000;
    player.idnum = db.create_player(&player, "secret").await.unwrap();
    let ch = state.create_char(player);
    state.char_to_room(ch, room);
    if mode == "idle" {
        state.get_char_mut(ch).unwrap().was_in_room = Some(room);
        state.char_from_room(ch);
        state.char_to_room(ch, void);
    }
    if mode == "bed" {
        let idnum = state.get_char(ch).unwrap().idnum;
        crate::house::set_test_houses(&mut state, vec![(150, idnum)]);
    }
    let conn = ConnId(41);
    let mut descriptor = Descriptor::new(conn, "127.0.0.1".into());
    descriptor.state = ConState::Playing;
    descriptor.character = Some(ch);
    state.descriptors.insert(conn, descriptor);
    state.get_char_mut(ch).unwrap().desc = Some(conn);
    for (vnum, kind) in [
        (700, ObjectType::Container),
        (701, ObjectType::Other),
        (702, ObjectType::Other),
        (703, ObjectType::Other),
    ] {
        let mut proto = rental_proto(vnum, kind);
        if vnum == 700 {
            proto.values[0] = 100;
        }
        if vnum == 702 {
            proto.wear_flags |= WearFlags::BODY;
        }
        state.obj_protos.insert(vnum, proto);
    }
    let bag = state.load_object(700).unwrap();
    let gem = state.load_object(701).unwrap();
    let armor = state.load_object(702).unwrap();
    let tent = state.load_object(703).unwrap();
    state.obj_to_char(bag, ch);
    state.obj_to_obj(gem, bag);
    state.equip_char(ch, armor, WEAR_BODY);
    state.obj_to_char(tent, ch);
    let file = crate::objsave::crash_filename(&state.config.lib_path, "Rental").unwrap();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let seed = if format == PersistenceFormat::C {
        crate::cformat::encode_rent_file(
            &CRentInfo {
                time: 0,
                rentcode: 1,
                net_cost_per_diem: 0,
                gold: 0,
                account: 0,
                nitems: 0,
            },
            &[],
        )
    } else {
        b"RENT 1 0 0 0 0\n".to_vec()
    };
    std::fs::write(&file, seed).unwrap();
    let mut game = Game::new(state, db);
    let (tx, _rx) = mpsc::channel(256);
    game.outputs.insert(conn, tx);
    (game, ch, conn, path, tent)
}

fn rental_command(game: &mut Game, ch: CharId, tent: ObjId, mode: &str) {
    match mode {
        "rent" | "cryo" => {
            let room = game.state.get_char(ch).unwrap().in_room.unwrap();
            let mut recep = Character::new_npc(101);
            recep.player.name = if mode == "cryo" {
                "cryogenicist"
            } else {
                "receptionist"
            }
            .into();
            let recep = game.state.create_char(recep);
            game.state.char_to_room(recep, room);
            crate::objsave::do_rent(&mut game.state, ch, "", 0);
        }
        "camp" => {
            assert!(crate::spec_procs::tent(
                &mut game.state,
                ch,
                tent,
                "camp",
                ""
            ));
        }
        "bed" => crate::house::do_bed(&mut game.state, ch, "", 0),
        "idle" => {
            game.state.get_char_mut(ch).unwrap().timer = 49;
            crate::limits::check_idling(&mut game.state, ch);
        }
        _ => panic!("unknown rental mode"),
    }
}

#[tokio::test]
async fn rental_departures_preserve_equipment_through_disconnect_and_reload() {
    for format in [PersistenceFormat::Rust, PersistenceFormat::C] {
        for mode in ["rent", "cryo", "camp", "bed", "idle"] {
            let (mut game, ch, conn, path, tent) = rental_game(format, mode).await;
            let file =
                crate::objsave::crash_filename(&game.state.config.lib_path, "Rental").unwrap();
            rental_command(&mut game, ch, tent, mode);
            assert_eq!(
                game.state.descriptors[&conn].state,
                ConState::Close,
                "{format:?} {mode}"
            );
            assert!(game.state.descriptors[&conn].rental_saved);
            assert_eq!(
                game.state.get_char(ch).unwrap().points.gold,
                if mode == "cryo" {
                    100_000 - 1_080
                } else {
                    100_000
                },
                "freeze fee must be charged exactly once"
            );
            let bytes = std::fs::read(&file).unwrap();
            if format == PersistenceFormat::C {
                let (header, objects) = crate::cformat::decode_rent_file(&bytes).unwrap();
                assert_eq!(
                    header.rentcode,
                    if mode == "cryo" {
                        3
                    } else if mode == "idle" {
                        5
                    } else {
                        2
                    }
                );
                assert_eq!(objects.len(), 4);
            } else {
                assert_eq!(
                    std::str::from_utf8(&bytes)
                        .unwrap()
                        .lines()
                        .filter(|line| line.starts_with("OBJ "))
                        .count(),
                    4
                );
            }
            game.flush_all().await;
            assert!(!game.state.char_exists(ch));
            assert!(!game.state.descriptors.contains_key(&conn));
            assert_eq!(
                std::fs::read(&file).unwrap(),
                bytes,
                "disconnect overwrote {format:?} {mode}"
            );
            assert_eq!(game.await_all_player_saves().await, 0);
            let player = game.load_player_latest("Rental").await.unwrap();
            let reloaded = game.state.create_char(player);
            game.state.char_to_room(reloaded, 0);
            crate::objsave::crash_load(&mut game.state, reloaded, path.to_str().unwrap());
            let restored: Vec<_> = game
                .state
                .objs
                .values()
                .map(|obj| obj.item_number)
                .collect();
            assert_eq!(restored.len(), 4, "{format:?} {mode}");
            for vnum in [700, 701, 702, 703] {
                assert!(restored.contains(&vnum));
            }
            let bag = game
                .state
                .objs
                .values()
                .find(|obj| obj.item_number == 700)
                .unwrap();
            assert_eq!(bag.contains.len(), 1);
            assert_eq!(
                bag.weight, 2,
                "container weight includes its child exactly once"
            );
            assert_eq!(game.state.get_obj(bag.contains[0]).unwrap().weight, 1);
            assert_eq!(game.state.get_char(reloaded).unwrap().carry_weight, 3);
            assert_eq!(
                game.state.get_obj(bag.contains[0]).unwrap().item_number,
                701
            );
            assert_eq!(
                game.state
                    .get_obj(game.state.get_char(reloaded).unwrap().equipment[WEAR_BODY].unwrap())
                    .unwrap()
                    .item_number,
                702
            );
            assert!(game.state.rooms.iter().all(|room| room.contents.is_empty()));
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}

#[tokio::test]
async fn rental_write_failure_retains_live_equipment_money_and_session() {
    for mode in ["rent", "cryo", "camp", "bed", "idle"] {
        let (mut game, ch, conn, path, tent) = rental_game(PersistenceFormat::Rust, mode).await;
        let file = crate::objsave::crash_filename(&game.state.config.lib_path, "Rental").unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap(); // Atomic rename over a directory fails, including as root.
        let before = game.state.get_char(ch).unwrap().clone();
        rental_command(&mut game, ch, tent, mode);
        let after = game.state.get_char(ch).unwrap();
        assert_eq!(after.points.gold, before.points.gold, "{mode}");
        assert_eq!(after.carrying, before.carrying);
        assert_eq!(after.equipment, before.equipment);
        assert_eq!(after.in_room, before.in_room);
        assert_eq!(after.act_flags, before.act_flags);
        assert_eq!(game.state.descriptors[&conn].state, ConState::Playing);
        assert!(!game.state.descriptors[&conn].rental_saved);
        assert!(
            game.state.descriptors[&conn]
                .outbuf
                .contains("could not be saved")
                || mode == "idle"
        );
        assert!(
            !game.state.descriptors[&conn]
                .outbuf
                .contains("stores your belongings")
        );
        assert!(game.state.player_save_requests.is_empty());
        assert!(game.state.player_extraction_requests.is_empty());
        assert_eq!(game.state.objs.len(), 4);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn linkless_idle_rent_retains_a_snapshot_before_extraction() {
    let (mut game, ch, conn, path, _tent) = rental_game(PersistenceFormat::Rust, "idle").await;
    game.state.descriptors.remove(&conn);
    game.state.get_char_mut(ch).unwrap().desc = None;
    game.state.get_char_mut(ch).unwrap().timer = 49;
    crate::limits::check_idling(&mut game.state, ch);
    assert!(game.state.char_exists(ch));
    assert!(game.state.player_extraction_requests.contains(&ch));
    game.drain_player_save_requests().await;
    assert!(!game.state.char_exists(ch));
    assert_eq!(game.await_all_player_saves().await, 0);
    assert_eq!(
        game.load_player_latest("Rental").await.unwrap().load_room,
        3
    );
    std::fs::remove_dir_all(path).unwrap();
}
