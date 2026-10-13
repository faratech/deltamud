//! Persistence and canceled-session regressions (#436-#439).

use super::*;
use crate::character::{Affect, Character};
use crate::mock_database::MockDatabase;
use crate::object::Object;
use crate::room::Room;
use std::sync::atomic::{AtomicU64, Ordering};

struct ScratchLib(std::path::PathBuf);
impl Drop for ScratchLib {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(db: Arc<MockDatabase>) -> (Game, ScratchLib) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "deltamud-persistence-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(path.join("lib/etc")).unwrap();
    let mut config = crate::config::Config::default();
    config.lib_path = path.join("lib").to_string_lossy().into_owned();
    let mut game = Game::new(GameState::new(config), db);
    game.state
        .add_room(Room::new(100, 1, "Safe room".into(), String::new()));
    (game, ScratchLib(path))
}

async fn seed(db: &MockDatabase, name: &str) -> Character {
    let character = Character::new_player(name.into(), Class::Warrior, Race::Human);
    db.create_player(&character, "test-password").await.unwrap();
    db.load_player(name).await.unwrap()
}

fn login_descriptor(game: &mut Game, conn: ConnId, name: &str, state: ConState) {
    let mut descriptor = Descriptor::new(conn, "127.0.0.1".into());
    descriptor.temp_name = Some(name.into());
    descriptor.state = state;
    game.state.descriptors.insert(conn, descriptor);
}

/// Release a paused database response only after the Game has consumed the
/// disconnect and dropped that descriptor's output sender. This reproduces the
/// continuation race without relying on sleeps or scheduler ordering.
fn disconnect_while_paused(
    game: &mut Game,
    conn: ConnId,
    pause: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>),
) -> tokio::task::JoinHandle<()> {
    let (game_tx, game_rx) = mpsc::channel(8);
    game.game_rx = Some(game_rx);
    let (output_tx, mut output_rx) = mpsc::channel(32);
    game.outputs.insert(conn, output_tx);
    tokio::spawn(async move {
        pause.0.notified().await;
        game_tx
            .send(GameMessage::Disconnect { conn_id: conn })
            .await
            .unwrap();
        while output_rx.recv().await.is_some() {}
        pause.1.notify_one();
    })
}

#[tokio::test]
async fn failed_disconnect_generation_survives_reaping_and_blocks_exit_until_retry() {
    let db = Arc::new(MockDatabase::new());
    let mut current = seed(&db, "Retained").await;
    crate::gold::set(&mut current, crate::gold::Account::Carried, 777);
    let (mut game, _lib) = fixture(db.clone());
    db.fail_next_save();
    game.queue_player_save(current.clone(), "latest.example.test".into());
    while !game.pending_player_saves[&current.idnum].task.is_finished() {
        tokio::task::yield_now().await;
    }
    game.reap_completed_player_saves().await;
    assert!(game.pending_player_saves.is_empty());
    assert_eq!(
        game.load_player_latest("Retained")
            .await
            .unwrap()
            .points
            .gold,
        777
    );
    assert_eq!(db.load_player("Retained").await.unwrap().points.gold, 0);
    assert_eq!(game.persist_copyover_players().await, 1);
    assert!(!game.shutdown_save().await.unwrap().persistence_succeeded());

    game.failed_player_saves
        .get_mut(&current.idnum)
        .unwrap()
        .retry_at = tokio::time::Instant::now();
    game.reap_completed_player_saves().await;
    assert_eq!(game.await_all_player_saves().await, 0);
    assert_eq!(db.load_player("Retained").await.unwrap().points.gold, 777);
    assert!(game.failed_player_saves.is_empty());
    assert_eq!(
        game.player_save_failures, 1,
        "historical failure remains diagnostic only"
    );
    assert_eq!(game.persist_copyover_players().await, 0);
    assert!(game.shutdown_save().await.unwrap().persistence_succeeded());
}

#[tokio::test]
async fn newest_committed_snapshot_supersedes_failed_prior_generation() {
    let db = Arc::new(MockDatabase::new());
    let mut older = seed(&db, "Supersede").await;
    let (mut game, _lib) = fixture(db.clone());
    let pause = db.pause_next_operation("save_player");
    db.fail_next_save();
    crate::gold::set(&mut older, crate::gold::Account::Carried, 10);
    game.queue_player_save(older.clone(), String::new());
    pause.0.notified().await;
    crate::gold::set(&mut older, crate::gold::Account::Carried, 20);
    game.queue_player_save(older, String::new());
    pause.1.notify_one();
    assert_eq!(game.await_all_player_saves().await, 0);
    assert!(game.failed_player_saves.is_empty());
    assert_eq!(
        game.player_save_failures, 1,
        "superseded failures remain diagnostic"
    );
    assert_eq!(db.load_player("Supersede").await.unwrap().points.gold, 20);
}

#[tokio::test]
async fn repeated_player_save_failures_use_bounded_backoff_and_newest_snapshot() {
    let db = Arc::new(MockDatabase::new());
    let current = seed(&db, "Backoff").await;
    let (mut game, _lib) = fixture(db.clone());
    db.fail_next_save();
    game.queue_player_save(current.clone(), "retry.example.test".into());
    for next_delay in [2, 4, 8, 16, 30, 30] {
        assert_eq!(game.await_all_player_saves().await, 1);
        let failed = &mut game.failed_player_saves.get_mut(&current.idnum).unwrap();
        assert_eq!(failed.retry_delay_secs, next_delay);
        assert!(failed.retry_at > tokio::time::Instant::now());
        assert_eq!(failed.host, "retry.example.test");
        failed.retry_at = tokio::time::Instant::now();
        db.fail_next_save();
        game.reap_completed_player_saves().await;
    }
    assert_eq!(game.await_all_player_saves().await, 1);
    let mut newest = current.clone();
    crate::gold::set(&mut newest, crate::gold::Account::Carried, 1234);
    game.queue_player_save(newest, String::new());
    assert_eq!(game.await_all_player_saves().await, 0);
    assert_eq!(db.load_player("Backoff").await.unwrap().points.gold, 1234);
}

#[tokio::test]
async fn login_counter_does_not_publish_pending_gameplay_or_get_overwritten_by_save() {
    let db = Arc::new(MockDatabase::new());
    let mut newest = seed(&db, "Counterrace").await;
    let (mut game, _lib) = fixture(db.clone());
    let conn = ConnId(20_101);
    login_descriptor(&mut game, conn, "Counterrace", ConState::GetOldPassword);
    crate::gold::set(&mut newest, crate::gold::Account::Carried, 999);
    newest.skills.insert(1, 87);
    newest.affected.push(Affect {
        spell_type: 7,
        duration: 6,
        modifier: 5,
        location: 1,
        bitvector: 1,
        caster: None,
    });
    let pause = db.pause_next_operation("save_player");
    game.queue_player_save(newest, String::new());
    pause.0.notified().await;
    game.nanny(conn, "incorrect".into()).await;
    let bookkeeping_only = db.load_player("Counterrace").await.unwrap();
    assert_eq!(bookkeeping_only.bad_pws, 1);
    assert_eq!(bookkeeping_only.points.gold, 0);
    assert!(bookkeeping_only.skills.is_empty());
    assert!(bookkeeping_only.affected.is_empty());
    pause.1.notify_one();
    assert_eq!(game.await_all_player_saves().await, 0);
    let stored = db.load_player("Counterrace").await.unwrap();
    assert_eq!(stored.points.gold, 999);
    assert_eq!(stored.skills[&1], 87);
    assert_eq!(stored.affected.len(), 1);
    assert_eq!(stored.bad_pws, 1);
    assert_eq!(
        db.update_login_failure_count(stored.idnum, "Counterrace", true)
            .await
            .unwrap(),
        Some(1)
    );
    let mut stale = stored.clone();
    stale.bad_pws = 99;
    db.save_player(&stale).await.unwrap();
    assert_eq!(db.load_player("Counterrace").await.unwrap().bad_pws, 0);
    assert_eq!(
        db.update_login_failure_count(stored.idnum + 1, "Counterrace", false)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn login_counter_uses_durable_name_spelling_for_imported_accounts() {
    let db = Arc::new(MockDatabase::new());
    seed(&db, "lowercounter").await;
    let (mut game, _lib) = fixture(db.clone());
    let conn = ConnId(20_111);
    login_descriptor(&mut game, conn, "Lowercounter", ConState::GetOldPassword);
    game.nanny(conn, "incorrect".into()).await;
    assert_eq!(db.load_player("lowercounter").await.unwrap().bad_pws, 1);
    game.nanny(conn, "test-password".into()).await;
    assert_eq!(game.state.descriptors[&conn].state, ConState::ReadMotd);
    assert_eq!(db.load_player("lowercounter").await.unwrap().bad_pws, 0);
}

#[tokio::test]
async fn disconnected_login_cannot_take_over_live_owner_or_repopulate_pending_load() {
    for operation in ["get_password_hash", "load_player"] {
        let db = Arc::new(MockDatabase::new());
        let (mut game, _lib) = fixture(db.clone());
        let owner_conn = ConnId(20_201);
        let owner =
            super::tests::persistent_connected_player(&mut game, &db, owner_conn, "Loginabort", 1)
                .await;
        let conn = ConnId(20_202);
        login_descriptor(&mut game, conn, "Loginabort", ConState::GetOldPassword);
        let close = disconnect_while_paused(&mut game, conn, db.pause_next_operation(operation));
        tokio::time::timeout(
            Duration::from_secs(5),
            game.nanny(conn, "test-password".into()),
        )
        .await
        .unwrap();
        close.await.unwrap();
        assert!(!game.state.descriptors.contains_key(&conn));
        assert!(!game.pending_load.contains_key(&conn));
        assert_eq!(game.state.descriptors[&owner_conn].state, ConState::Playing);
        assert_eq!(game.state.descriptors[&owner_conn].character, Some(owner));
        assert_eq!(game.state.get_char(owner).unwrap().desc, Some(owner_conn));
        assert!(!game.state.players_by_name.contains_key(""));
        assert!(
            !game
                .perform_dupe_check(conn, game.state.get_char(owner).unwrap().idnum)
                .await
        );
    }
}

#[tokio::test]
async fn disconnected_creation_reconciles_committed_identity_without_session_caches() {
    let db = Arc::new(MockDatabase::new());
    let (mut game, _lib) = fixture(db.clone());
    let conn = ConnId(20_301);
    login_descriptor(&mut game, conn, "Createabort", ConState::RollStats);
    game.state.descriptors.get_mut(&conn).unwrap().password_hash =
        Some(crate::password::hash_password("test-password"));
    game.pending.insert(conn, PendingChoices::default());
    let close = disconnect_while_paused(&mut game, conn, db.pause_next_operation("created_player"));
    tokio::time::timeout(Duration::from_secs(5), game.create_and_enter(conn))
        .await
        .unwrap();
    close.await.unwrap();
    assert!(db.player_exists("Createabort").await.unwrap());
    assert!(
        game.state
            .player_table
            .iter()
            .any(|p| p.name == "Createabort")
    );
    assert!(!game.pending_load.contains_key(&conn));
    assert!(!game.pending.contains_key(&conn));
    assert!(!game.just_created.contains(&conn));
    assert!(!game.state.descriptors.contains_key(&conn));
}

#[tokio::test]
async fn disconnected_copyover_recovery_does_not_materialize_orphan_player() {
    let db = Arc::new(MockDatabase::new());
    seed(&db, "Recoverabort").await;
    let (mut game, _lib) = fixture(db.clone());
    let conn = ConnId(20_401);
    let (tx, rx) = mpsc::channel(8);
    game.game_rx = Some(rx);
    let (output, mut output_rx) = mpsc::channel(32);
    let pause = db.pause_next_operation("load_player");
    let close = tokio::spawn(async move {
        pause.0.notified().await;
        tx.send(GameMessage::Disconnect { conn_id: conn })
            .await
            .unwrap();
        while output_rx.recv().await.is_some() {}
        pause.1.notify_one();
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        game.recover_player(
            conn,
            "127.0.0.1".into(),
            "127.0.0.1".into(),
            None,
            -1,
            "Recoverabort".into(),
            output,
        ),
    )
    .await
    .unwrap();
    close.await.unwrap();
    assert!(!game.state.descriptors.contains_key(&conn));
    assert!(game.state.find_player_by_name("Recoverabort").is_none());
    assert!(game.state.chars.is_empty());
    assert!(game.pending_load.is_empty());
}

#[tokio::test]
async fn switched_disconnect_persists_principal_and_preserves_borrowed_npc_inventory() {
    let db = Arc::new(MockDatabase::new());
    let (mut game, _lib) = fixture(db.clone());
    let conn = ConnId(20_501);
    let principal =
        super::tests::persistent_connected_player(&mut game, &db, conn, "Switchsave", LVL_IMPL)
            .await;
    game.state.char_to_room(principal, 0);
    crate::gold::set(
        game.state.get_char_mut(principal).unwrap(),
        crate::gold::Account::Carried,
        808,
    );
    let mut npc = Character::new_player("Borrowed".into(), Class::Warrior, Race::Human);
    npc.is_npc = true;
    let npc = game.state.create_char(npc);
    game.state.char_to_room(npc, 0);
    let object = game
        .state
        .create_obj(Object::new(9001, "sword".into(), "a sword".into()));
    game.state.obj_to_char(object, npc);
    crate::cmd_wizard::do_switch(&mut game.state, principal, "Borrowed", 0);
    assert_eq!(game.state.descriptors[&conn].original, Some(principal));
    game.disconnect(conn).await;
    assert_eq!(game.await_all_player_saves().await, 0);
    assert_eq!(db.load_player("Switchsave").await.unwrap().points.gold, 808);
    assert!(!game.state.char_exists(principal));
    assert!(game.state.char_exists(npc));
    assert_eq!(game.state.get_char(npc).unwrap().desc, None);
    assert_eq!(game.state.get_char(npc).unwrap().carrying, vec![object]);
    assert!(game.state.get_obj(object).is_some());
}

#[tokio::test]
async fn failed_disconnect_object_save_retains_inventory_then_retries_or_reconnects() {
    for reconnect in [false, true] {
        let db = Arc::new(MockDatabase::new());
        let (mut game, lib) = fixture(db.clone());
        let valid_lib = game.state.config.lib_path.clone();
        let conn = ConnId(20_601);
        let player =
            super::tests::persistent_connected_player(&mut game, &db, conn, "Objectsafe", 1).await;
        game.state.char_to_room(player, 0);
        let object =
            game.state
                .create_obj(Object::new(9002, "keepsake".into(), "a keepsake".into()));
        game.state.obj_to_char(object, player);
        let blocked = lib.0.join("blocked-lib");
        std::fs::write(&blocked, "not a directory").unwrap();
        game.state.config.lib_path = blocked.to_string_lossy().into_owned();
        game.disconnect(conn).await;
        assert_eq!(game.await_all_player_saves().await, 0);
        assert!(game.state.char_exists(player));
        assert!(game.state.get_obj(object).is_some());
        assert_eq!(game.state.get_char(player).unwrap().desc, None);
        assert!(game.linkless_object_saves.contains_key(&player));
        crate::gold::set(
            game.state.get_char_mut(player).unwrap(),
            crate::gold::Account::Carried,
            4545,
        );
        game.state.get_char_mut(player).unwrap().conditions[0] = 7;
        game.state.config.lib_path = valid_lib;
        if reconnect {
            let next = ConnId(20_602);
            login_descriptor(&mut game, next, "Objectsafe", ConState::GetOldPassword);
            assert!(
                game.perform_dupe_check(next, game.state.get_char(player).unwrap().idnum)
                    .await
            );
            game.retry_linkless_object_saves();
            assert!(game.linkless_object_saves.is_empty());
            assert!(game.state.char_exists(player));
            assert_eq!(game.state.get_char(player).unwrap().carrying, vec![object]);
        } else {
            game.linkless_object_saves
                .get_mut(&player)
                .unwrap()
                .retry_at = tokio::time::Instant::now();
            game.retry_linkless_object_saves();
            assert_eq!(game.await_all_player_saves().await, 0);
            let newest = db.load_player("Objectsafe").await.unwrap();
            assert_eq!(newest.points.gold, 4545);
            assert_eq!(newest.conditions[0], 7);
            assert!(game.linkless_object_saves.is_empty());
            assert!(!game.state.char_exists(player));
            assert!(
                std::path::Path::new(
                    &crate::objsave::crash_filename(&game.state.config.lib_path, "Objectsafe")
                        .unwrap()
                )
                .exists()
            );
        }
    }
}

#[tokio::test]
async fn failed_alias_departures_retain_aliases_and_preserve_completed_rent_file() {
    for mode in ["disconnect", "rental", "idle"] {
        let db = Arc::new(MockDatabase::new());
        let (mut game, _lib) = fixture(db.clone());
        let conn = ConnId(20_701);
        let player =
            super::tests::persistent_connected_player(&mut game, &db, conn, "Aliassafe", 1).await;
        game.state.char_to_room(player, 0);
        let idnum = game.state.get_char(player).unwrap().idnum;
        let aliases = vec![crate::alias::AliasEntry {
            alias: "nn".into(),
            replacement: "north".into(),
            atype: 0, // C ALIAS_SIMPLE
        }];
        crate::alias::set_aliases(&mut game.state, idnum, aliases.clone());
        game.state.obj_protos.insert(
            9003,
            super::rental_tests::rental_proto(9003, crate::object::ObjectType::Other),
        );
        let object = game.state.load_object(9003).unwrap();
        game.state.obj_to_char(object, player);
        game.state
            .get_char_mut(player)
            .unwrap()
            .affected
            .push(Affect {
                spell_type: 7,
                duration: 3,
                modifier: 1,
                location: 0,
                bitvector: 0,
                caster: None,
            });
        let rent_path =
            crate::objsave::crash_filename(&game.state.config.lib_path, "Aliassafe").unwrap();
        let rented_bytes = if mode != "disconnect" {
            assert!(crate::objsave::crash_rentsave(&mut game.state, player, 0));
            Some(std::fs::read(&rent_path).unwrap())
        } else {
            None
        };
        let alias_root = std::path::Path::new(&game.state.config.lib_path).join("plralias");
        std::fs::write(&alias_root, "blocked alias directory").unwrap();
        if mode == "idle" {
            game.state.get_char_mut(player).unwrap().desc = None;
            game.state.descriptors.remove(&conn);
            game.state.request_player_save_and_extract(player);
            game.drain_player_save_requests().await;
        } else {
            game.disconnect(conn).await;
        }
        assert_eq!(game.await_all_player_saves().await, 0);
        assert!(game.state.char_exists(player), "{mode}");
        assert_eq!(crate::alias::get_aliases(&game.state, idnum), aliases);
        assert!(game.linkless_object_saves.contains_key(&player));
        if let Some(bytes) = &rented_bytes {
            assert!(game.state.rental_departures.contains(&player));
            assert_eq!(game.state.get_char(player).unwrap().in_room, None);
            let reconnect = ConnId(20_702);
            login_descriptor(&mut game, reconnect, "Aliassafe", ConState::GetOldPassword);
            assert!(game.perform_dupe_check(reconnect, idnum).await);
            assert_eq!(game.state.descriptors[&reconnect].state, ConState::Close);
            assert!(
                game.state.descriptors[&reconnect]
                    .outbuf
                    .contains("departure is still finishing")
            );
            assert_eq!(game.state.get_char(player).unwrap().desc, None);
            game.disconnect(reconnect).await;
            game.state.get_char_mut(player).unwrap().timer = 49;
            game.state.get_char_mut(player).unwrap().was_in_room = Some(0);
            for _ in 0..3 {
                crate::limits::check_idling(&mut game.state, player);
                crate::limits::point_update(&mut game.state);
                crate::magic::affect_update(&mut game.state);
            }
            assert_eq!(game.state.get_char(player).unwrap().timer, 49);
            assert_eq!(game.state.get_char(player).unwrap().affected[0].duration, 3);
            assert_eq!(std::fs::read(&rent_path).unwrap(), *bytes);
            assert!(game.state.player_extraction_requests.is_empty());
        }
        // A retry while the sidecar is still unavailable keeps the only copy.
        game.linkless_object_saves
            .get_mut(&player)
            .unwrap()
            .retry_at = tokio::time::Instant::now();
        game.retry_linkless_object_saves();
        assert!(game.state.char_exists(player));
        assert_eq!(crate::alias::get_aliases(&game.state, idnum), aliases);
        std::fs::remove_file(&alias_root).unwrap();
        game.linkless_object_saves
            .get_mut(&player)
            .unwrap()
            .retry_at = tokio::time::Instant::now();
        game.retry_linkless_object_saves();
        assert_eq!(game.await_all_player_saves().await, 0);
        assert!(!game.state.char_exists(player));
        assert!(game.linkless_object_saves.is_empty());
        assert!(!game.state.rental_departures.contains(&player));
        let lib_path = game.state.config.lib_path.clone();
        crate::alias::read_aliases(&mut game.state, &lib_path, "Aliassafe", idnum).unwrap();
        assert_eq!(crate::alias::get_aliases(&game.state, idnum), aliases);
        if let Some(bytes) = rented_bytes {
            assert_eq!(
                std::fs::read(&rent_path).unwrap(),
                bytes,
                "{mode}: alias retry must preserve rented inventory"
            );
            let reconnect = ConnId(20_703);
            login_descriptor(&mut game, reconnect, "Aliassafe", ConState::Menu);
            game.enter_game(reconnect, false).await;
            let reloaded = game.state.descriptors[&reconnect].character.unwrap();
            let carried = &game.state.get_char(reloaded).unwrap().carrying;
            assert_eq!(
                carried.len(),
                1,
                "{mode}: fresh login must load saved rental gear"
            );
            assert_eq!(game.state.get_obj(carried[0]).unwrap().item_number, 9003);
        }
    }
}

#[tokio::test]
async fn descriptorless_exit_hands_off_snapshot_before_extraction() {
    let db = Arc::new(MockDatabase::new());
    let mut character = seed(&db, "Idlesnapshot").await;
    crate::gold::set(&mut character, crate::gold::Account::Carried, 654);
    let (mut game, _lib) = fixture(db.clone());
    let player = game.state.create_char(character);
    game.state.char_to_room(player, 0);
    game.state.request_player_save_and_extract(player);
    db.fail_next_save();
    game.drain_player_save_requests().await;
    assert!(!game.state.char_exists(player));
    assert_eq!(game.await_all_player_saves().await, 1);
    assert_eq!(
        game.load_player_latest("Idlesnapshot")
            .await
            .unwrap()
            .points
            .gold,
        654
    );
}
