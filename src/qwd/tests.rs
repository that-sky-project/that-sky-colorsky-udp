//! QWD adapter integration tests.

use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, accept};

use crate::control::{
    ControlCommand, ControlHandle, ControlResponse, MoveOutcome, PlayerInfo, ServerEvent,
    ServerStatus, control_channel,
};
use crate::protocol::types::TgcUuid;

use super::util::{destination_uuid, endpoint_url, generate_room_id};
use super::{QwdClient, QwdConfig};
#[test]
fn endpoint_preserves_path_query_and_replaces_protocol_identity() {
    assert_eq!(
        endpoint_url("ws://127.0.0.1:1131", "node-1").unwrap(),
        "ws://127.0.0.1:1131/?v=1&node=node-1"
    );
    let endpoint = endpoint_url(
        "wss://manager.example/control/node?token=a%2Fb&v=9&node=old&v=2",
        "node /?&",
    )
    .unwrap();
    assert!(endpoint.contains("token=a%2Fb&"));
    let parsed = url::Url::parse(&endpoint).unwrap();
    assert_eq!(parsed.path(), "/control/node");
    let pairs: Vec<_> = parsed.query_pairs().collect();
    assert_eq!(pairs.len(), 3);
    assert_eq!(pairs[0], ("token".into(), "a/b".into()));
    assert_eq!(pairs[1], ("v".into(), "1".into()));
    assert_eq!(pairs[2], ("node".into(), "node /?&".into()));
    assert!(endpoint_url("ws://[::1]:1131/control", "n").is_ok());
    for invalid in [
        "manager.example:1131",
        "https://manager.example",
        "ftp://manager.example",
        "ws://",
        "ws://user:secret@manager.example",
        "ws://@manager.example",
        "ws://manager.example/#fragment",
        "ws://manager.example/#",
    ] {
        assert!(endpoint_url(invalid, "node-1").is_err(), "{invalid}");
    }
}

fn test_player(session: &str) -> PlayerInfo {
    PlayerInfo {
        player_id: 1,
        uuid: "player-1".into(),
        level_id: 7,
        lv_seq: 8,
        moving: false,
        session_id: Some(session.into()),
        position: Some([1.0, 2.0, 3.0]),
        velocity: None,
        state_at: 100,
        last_move_at: 0,
        affinity: Vec::new(),
    }
}

fn test_status(player: PlayerInfo) -> ServerStatus {
    ServerStatus {
        addr: "127.0.0.1:0".into(),
        peers: 1,
        players: 1,
        pending_moves: 0,
        draining: false,
        rooms: Vec::new(),
        player_list: vec![player],
    }
}

fn test_client() -> QwdClient {
    let (_, events) = mpsc::sync_channel(8);
    QwdClient {
        config: QwdConfig::new("ws://127.0.0.1:1", "127.0.0.1", 5413, 8),
        control: spawn_fake_control(None),
        events,
        reservations: Default::default(),
        node_id: "node-test".into(),
        room_id: "12345678".into(),
        telemetry_seq: 0,
        telemetry_roster: None,
        last_telemetry_sent_at: Default::default(),
        last_telemetry_seq_sent: 0,
        last_full_telemetry_seq: 0,
        last_full_telemetry_at: std::time::Instant::now(),
        last_manager_error: None,
        caps: Default::default(),
        force_full: true,
        draining: false,
        internal_to_qwd: Default::default(),
        qwd_moves: Default::default(),
        command_cache: super::command_cache::CommandCache::new(),
        pending_events: super::outbox::EventOutbox::new(),
    }
}

#[test]
fn telemetry_session_only_change_is_a_delta() {
    let mut client = test_client();
    client
        .publish_telemetry(&test_status(test_player("s1")), |_, _| Ok(()))
        .unwrap();
    let mut sent = Value::Null;
    client
        .publish_telemetry(&test_status(test_player("s2")), |_, value| {
            sent = value.clone();
            Ok(())
        })
        .unwrap();
    assert_eq!(sent["data"]["kind"], "delta");
    assert_eq!(sent["data"]["baseSeq"], 1);
    assert_eq!(sent["data"]["players"][0]["sessionId"], "s2");
    assert_eq!(client.telemetry_seq, 2);
}

#[test]
fn telemetry_full_failure_preserves_committed_baseline_and_retry() {
    let mut client = test_client();
    client
        .publish_telemetry(&test_status(test_player("s1")), |_, _| Ok(()))
        .unwrap();
    client.force_full = true;
    let roster = client.telemetry_roster.clone();
    let sent_at = client.last_telemetry_sent_at.clone();
    let full_at = client.last_full_telemetry_at;
    assert!(
        client
            .publish_telemetry(&test_status(test_player("s2")), |_, _| Err(
                "injected send failure".into()
            ))
            .is_err()
    );
    assert_eq!(client.telemetry_seq, 1);
    assert_eq!(client.last_telemetry_seq_sent, 1);
    assert_eq!(client.last_full_telemetry_seq, 1);
    assert_eq!(client.last_full_telemetry_at, full_at);
    assert_eq!(client.telemetry_roster, roster);
    assert_eq!(client.last_telemetry_sent_at, sent_at);
    assert!(client.force_full);
    client
        .publish_telemetry(&test_status(test_player("s2")), |_, value| {
            assert!(value["data"].get("kind").is_none());
            assert_eq!(value["data"]["seq"], 2);
            Ok(())
        })
        .unwrap();
    assert!(!client.force_full);
    assert_eq!(client.last_full_telemetry_seq, 2);
}

#[test]
fn telemetry_delta_failure_and_idle_tick_do_not_commit() {
    let mut client = test_client();
    let status = test_status(test_player("s1"));
    client.publish_telemetry(&status, |_, _| Ok(())).unwrap();
    let baseline = client.telemetry_roster.clone();
    let sent_at = client.last_telemetry_sent_at.clone();
    client
        .publish_telemetry(&status, |_, _| panic!("idle tick must not send"))
        .unwrap();
    assert_eq!(client.telemetry_seq, 1);
    assert!(
        client
            .publish_telemetry(&test_status(test_player("s2")), |_, value| {
                assert_eq!(value["data"]["kind"], "delta");
                Err("injected delta failure".into())
            })
            .is_err()
    );
    assert_eq!(client.telemetry_seq, 1);
    assert_eq!(client.telemetry_roster, baseline);
    assert_eq!(client.last_telemetry_sent_at, sent_at);
}

#[test]
fn moving_players_are_not_active_roster_but_remain_physical() {
    let mut client = test_client();
    let mut player = test_player("s1");
    player.moving = true;
    client
        .publish_telemetry(&test_status(player), |_, value| {
            assert_eq!(value["data"]["players"], json!([]));
            assert_eq!(value["data"]["physicalPlayers"], 1);
            assert_eq!(value["data"]["activePlayers"], 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn reconnect_forces_full_without_resetting_business_state_or_room_sequence() {
    let mut client = test_client();
    let now = std::time::Instant::now();
    let token = client
        .reservations
        .reserve_player("held", Duration::from_secs(60), now);
    client.internal_to_qwd.insert(42, "pending-move".into());
    client
        .publish_telemetry(&test_status(test_player("s1")), |_, _| Ok(()))
        .unwrap();
    client.reset_transport_baseline();
    assert!(client.force_full);
    assert_eq!(client.telemetry_seq, 1);
    assert_eq!(
        client.internal_to_qwd.get(&42).map(String::as_str),
        Some("pending-move")
    );
    assert_eq!(
        client.reservations.player_token_of("held", now),
        Some(token.as_str())
    );
    client
        .publish_telemetry(&test_status(test_player("s1")), |_, value| {
            assert!(value["data"].get("kind").is_none());
            assert_eq!(value["data"]["seq"], 2);
            Ok(())
        })
        .unwrap();
}

#[test]
fn close_frame_returns_from_connection_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut socket = accept(stream).unwrap();
        read_event(&mut socket, "room.telemetry");
        socket.close(None).unwrap();
        let _ = socket.read();
    });
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut client = test_client();
        client.config.url = format!("ws://{addr}");
        let result = client.connect_once();
        done_tx.send(result).unwrap();
    });
    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("Close must end the connection loop")
            .is_ok()
    );
    server.join().unwrap();
}

#[test]
fn socket_timeouts_apply_to_plain_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let mut socket = WebSocket::from_raw_socket(
        tungstenite::stream::MaybeTlsStream::Plain(stream),
        tungstenite::protocol::Role::Client,
        None,
    );
    super::util::set_socket_timeouts(
        &mut socket,
        Some(Duration::from_millis(250)),
        Some(Duration::from_secs(5)),
    )
    .unwrap();
    match socket.get_ref() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            assert_eq!(
                stream.read_timeout().unwrap(),
                Some(Duration::from_millis(250))
            );
            assert_eq!(
                stream.write_timeout().unwrap(),
                Some(Duration::from_secs(5))
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn endpoint_logs_omit_secrets_and_invalid_url_errors_are_redacted() {
    let endpoint =
        endpoint_url("wss://manager.example/secret-path?token=secret-token", "n").unwrap();
    let logged = super::util::redacted_endpoint(&endpoint);
    assert!(!logged.contains("secret"));
    assert!(logged.contains("manager.example"));
    let error = endpoint_url("ws://user:secret-password@manager.example", "n").unwrap_err();
    assert!(!error.contains("secret-password"));
}

#[test]
fn real_reconnect_retains_moves_leases_and_sends_full_with_increasing_sequence() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for expected_seq in [1, 2] {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut socket = accept(stream).unwrap();
            let frame = read_event(&mut socket, "room.telemetry");
            assert_eq!(frame["data"]["seq"], expected_seq);
            assert!(frame["data"].get("kind").is_none());
            socket.close(None).unwrap();
            let _ = socket.read();
        }
    });
    let mut client = test_client();
    client.config.url = format!("ws://{addr}");
    let now = std::time::Instant::now();
    let token = client
        .reservations
        .reserve_player("held", Duration::from_secs(60), now);
    client.internal_to_qwd.insert(42, "pending-move".into());
    client.qwd_moves.insert(
        "pending-move".into(),
        super::PendingQwdMove {
            pending: [42].into_iter().collect(),
            moved: Vec::new(),
            failed: Vec::new(),
            reason: None,
            token: Some("move-token".into()),
        },
    );
    for _ in 0..2 {
        client.connect_once().unwrap();
        assert_eq!(
            client.internal_to_qwd.get(&42).map(String::as_str),
            Some("pending-move")
        );
        assert!(client.qwd_moves.contains_key("pending-move"));
        assert_eq!(
            client
                .reservations
                .player_token_of("held", std::time::Instant::now()),
            Some(token.as_str())
        );
    }
    assert_eq!(
        client
            .reservations
            .outstanding_slots(now + Duration::from_secs(20)),
        0
    );
    server.join().unwrap();
}

#[test]
fn destination_uuid_matches_node_convention() {
    // Mirrors XYSKY's RoomServer.destinationUuid and the manager's
    // CrossCoordinator.destinationUuidText.
    let uuid = destination_uuid("12345678").expect("numeric id maps to a uuid");
    assert_eq!(format!("{uuid:?}"), "00000000-0000-4000-8000-000000bc614e");

    // UUID-shaped ids pass through so mixed-version clusters keep working.
    let text = "0d17e2c6-cdbd-40a5-983d-b9deb380dd00";
    assert_eq!(format!("{:?}", destination_uuid(text).unwrap()), text);

    for invalid in ["", "0x10", "1e3", "1234567890123456", "-1"] {
        assert!(destination_uuid(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn generated_room_id_is_decimal() {
    for _ in 0..16 {
        let room_id = generate_room_id();
        assert_eq!(room_id.len(), 8, "{room_id}");
        assert!(
            room_id.bytes().all(|byte| byte.is_ascii_digit()),
            "{room_id}"
        );
        let numeric: u32 = room_id.parse().unwrap();
        assert!((10_000_000..=99_999_999).contains(&numeric), "{room_id}");
    }
}

fn spawn_fake_control(player: Option<PlayerInfo>) -> ControlHandle {
    let (tx, rx) = control_channel();
    thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            let response = match request.command {
                ControlCommand::Status => ControlResponse::Status(ServerStatus {
                    addr: "127.0.0.1:0".to_string(),
                    peers: 0,
                    players: usize::from(player.is_some()),
                    pending_moves: 0,
                    draining: false,
                    rooms: Vec::new(),
                    player_list: player.iter().cloned().collect(),
                }),
                ControlCommand::MovePlayers { player_ids, .. } => {
                    ControlResponse::Move(MoveOutcome {
                        move_ids: vec![42],
                        moved: player_ids,
                        failed: Vec::new(),
                    })
                }
                ControlCommand::MovePlayersGuarded { player_ids, .. } => {
                    ControlResponse::Move(MoveOutcome {
                        move_ids: vec![42],
                        moved: player_ids,
                        failed: Vec::new(),
                    })
                }
                _ => ControlResponse::Ok,
            };
            if let Some(reply) = request.reply {
                let _ = reply.send(response);
            }
        }
    });
    ControlHandle::new(tx)
}

fn read_event(socket: &mut WebSocket<TcpStream>, name: &str) -> Value {
    loop {
        let value = read_json(socket);
        if value.get("event").and_then(Value::as_str) == Some(name) {
            return value;
        }
    }
}

fn read_json(socket: &mut WebSocket<TcpStream>) -> Value {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(text.as_str()).expect("valid JSON");
            }
            Ok(_) => continue,
            Err(error) => panic!("fake QWD read failed: {error}"),
        }
    }
}

fn read_ack(socket: &mut WebSocket<TcpStream>, id: i64) -> Value {
    loop {
        let value = read_json(socket);
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return value;
        }
    }
}

fn send_command(socket: &mut WebSocket<TcpStream>, value: Value) {
    socket
        .send(Message::text(serde_json::to_string(&value).unwrap()))
        .unwrap();
}

#[test]
fn falls_back_to_move_result_on_old_managers() {
    let player_uuid = TgcUuid::from(&[7u8; 16]).unwrap();
    let player_id = format!("{player_uuid:?}");
    // No players in the room yet: `move.prepare` must accept the group.
    let control = spawn_fake_control(None);
    let (event_tx, event_rx) = mpsc::sync_channel(32);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut config = QwdConfig::new(format!("ws://127.0.0.1:{port}"), "127.0.0.1", 5413, 8);
    config.reconnect_delay = Duration::from_secs(60);

    let _client = QwdClient::spawn(config, control, event_rx);

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut socket = accept(stream).unwrap();

        assert_eq!(read_json(&mut socket)["event"], "node.ready");
        let room_id = read_json(&mut socket)["data"]["roomId"]
            .as_str()
            .unwrap()
            .to_string();

        // Older manager build: `move.completed` is not part of its schema.
        send_command(
            &mut socket,
            json!({
                "code": 5999,
                "data": {
                    "message": r#"[{"received":"move.completed","code":"invalid_enum_value","path":["event"],"message":"Invalid enum value"}]"#
                }
            }),
        );

        send_command(
            &mut socket,
            json!({
                "v": 1,
                "id": 21,
                "cmd": "move.prepare",
                "data": {
                    "moveId": "move-old",
                    "roomId": room_id,
                    "playerIds": [player_id.clone()],
                    "ttlMs": 5000
                }
            }),
        );
        assert_eq!(read_ack(&mut socket, 21)["code"], 0);

        event_tx
            .send(ServerEvent::PlayerJoined {
                player_id: 1,
                uuid: player_uuid,
                level_id: 42,
                session_id: Some("s7".into()),
                lv_seq: 8,
            })
            .unwrap();

        let result = read_event(&mut socket, "move.result");
        assert_eq!(result["data"]["moveId"], "move-old");
        assert_eq!(result["data"]["accepted"], true);
        assert_eq!(result["data"]["movedPlayerIds"], json!([player_id]));
        assert!(result["data"].get("playerIds").is_none());

        socket.close(None).ok();
    });

    server.join().unwrap();
}

#[test]
fn registers_and_answers_qwd_commands() {
    let player_uuid = TgcUuid::from(&[9u8; 16]).unwrap();
    let player_id = format!("{player_uuid:?}");
    let control = spawn_fake_control(Some(PlayerInfo {
        player_id: 1,
        uuid: player_id.clone(),
        level_id: 0,
        lv_seq: 0,
        moving: false,
        session_id: Some("s9".into()),
        position: None,
        velocity: None,
        state_at: 0,
        last_move_at: 0,
        affinity: Vec::new(),
    }));
    let (event_tx, event_rx) = mpsc::sync_channel(32);
    let event_tx_for_server = event_tx.clone();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut config = QwdConfig::new(format!("ws://127.0.0.1:{port}"), "127.0.0.1", 5413, 8);
    config.reconnect_delay = Duration::from_secs(60);

    let _client = QwdClient::spawn(config, control, event_rx);

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut socket = accept(stream).unwrap();

        let ready = read_json(&mut socket);
        assert_eq!(ready["event"], "node.ready");
        let created = read_json(&mut socket);
        assert_eq!(created["event"], "room.created");
        let room_id = created["data"]["roomId"].as_str().unwrap().to_string();
        assert_eq!(room_id.len(), 8);
        assert!(room_id.bytes().all(|byte| byte.is_ascii_digit()));

        // Force a few read timeouts before sending the first command; the
        // adapter must keep the socket usable after `WouldBlock`.
        thread::sleep(Duration::from_millis(700));

        send_command(
            &mut socket,
            json!({
                "v": 1,
                "id": 7,
                "cmd": "room.reserve",
                "data": {
                    "roomId": room_id,
                    "playerId": "player-1",
                    "ttlMs": 5000
                }
            }),
        );
        assert_eq!(read_ack(&mut socket, 7)["code"], 0);

        send_command(
            &mut socket,
            json!({
                "v": 1,
                "id": 8,
                "cmd": "move.prepare",
                "data": {
                    "moveId": "move-1",
                    "roomId": room_id,
                    "playerIds": ["player-1"]
                }
            }),
        );
        assert_eq!(read_ack(&mut socket, 8)["code"], 0);

        // The manager must acknowledge its command before the node releases a
        // player allocation; sending it while a hold is still outstanding is a
        // separate scenario that expects the hold to remain reserved.
        event_tx_for_server
            .send(ServerEvent::PlayerJoined {
                player_id: 1,
                uuid: player_uuid,
                level_id: 0,
                session_id: Some("s9".into()),
                lv_seq: 0,
            })
            .unwrap();
        let joined = read_event(&mut socket, "player.join");
        assert_eq!(joined["data"]["player"]["sessionId"], "s9");

        send_command(
            &mut socket,
            json!({
                "v": 1,
                "id": 9,
                "cmd": "move.commit",
                "data": {
                    "moveId": "move-1",
                    "playerIds": [player_id.clone()],
                    "fromRoomId": room_id,
                    "toRoomId": "0d17e2c6-cdbd-40a5-983d-b9deb380dd00",
                    "udpHost": "127.0.0.1",
                    "udpPort": 5414,
                    "expectedParticipants": [{
                        "playerId": player_id,
                        "sessionId": "s9",
                        "levelId": 0,
                        "lvSeq": 0
                    }]
                }
            }),
        );
        assert_eq!(read_ack(&mut socket, 9)["code"], 0);

        event_tx_for_server
            .send(ServerEvent::MoveFinished {
                move_id: 42,
                player_id: 1,
                player_uuid,
                accepted: false,
                result: 1,
            })
            .unwrap();

        let result = read_event(&mut socket, "move.result");
        assert_eq!(result["data"]["moveId"], "move-1");
        assert_eq!(result["data"]["accepted"], false);
        assert_eq!(result["data"]["failedPlayerIds"][0], player_id);

        send_command(
            &mut socket,
            json!({ "v": 1, "id": 10, "cmd": "node.drain", "data": {} }),
        );
        assert_eq!(read_ack(&mut socket, 10)["code"], 0);

        socket.close(None).ok();
    });

    server.join().unwrap();
}
