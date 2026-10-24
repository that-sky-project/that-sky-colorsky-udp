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
fn endpoint_forces_root_and_protocol_version() {
    assert_eq!(
        endpoint_url("ws://127.0.0.1:1131", "node-1").unwrap(),
        "ws://127.0.0.1:1131/?v=1&node=node-1"
    );
    assert_eq!(
        endpoint_url("wss://manager.example/ignored/path", "node-1").unwrap(),
        "wss://manager.example/?v=1&node=node-1"
    );
    assert!(endpoint_url("manager.example:1131", "node-1").is_err());
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
                    "udpPort": 5414
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
