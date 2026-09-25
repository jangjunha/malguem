use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use malguem_server::state::Config;

struct TestServer {
    base: String,
    ws_base: String,
    _db: tempfile::NamedTempFile,
}

async fn spawn_server() -> TestServer {
    spawn_server_with(|_| {}).await
}

async fn spawn_server_with(tweak: impl FnOnce(&mut Config)) -> TestServer {
    let db = tempfile::NamedTempFile::new().unwrap();
    let mut cfg = Config {
        bind: String::new(),
        db_path: db.path().to_str().unwrap().to_string(),
        turn_secret: Some("test-secret".into()),
        turn_urls: vec!["turn:turn.example.com:3478?transport=udp".into()],
        turn_ttl_secs: 600,
        stun_urls: vec!["stun:stun.example.com:3478".into()],
        space_creator_sign_pubs: vec![],
        max_users: None,
    };
    tweak(&mut cfg);
    let state = malguem_server::build_state(cfg);
    let app = malguem_server::app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer {
        base: format!("http://{addr}/api"),
        ws_base: format!("ws://{addr}/api/ws"),
        _db: db,
    }
}

struct TestUser {
    id: String,
    token: String,
    #[allow(dead_code)]
    key: SigningKey,
}

async fn make_user(client: &reqwest::Client, base: &str, name: &str) -> TestUser {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    let key = SigningKey::from_bytes(&seed);
    let sign_pub = B64.encode(key.verifying_key().to_bytes());
    let kem_pub = B64.encode([7u8; 32]); // test value; the server treats it as opaque

    let r: Value = client
        .post(format!("{base}/register"))
        .json(&json!({ "name": name, "sign_pub": sign_pub, "kem_pub": kem_pub }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_id = r["user_id"].as_str().unwrap().to_string();

    let token = login(client, base, &user_id, &key).await;
    TestUser { id: user_id, token, key }
}

/// Run the challenge/login handshake and return the bearer token.
async fn login(client: &reqwest::Client, base: &str, user_id: &str, key: &SigningKey) -> String {
    let r: Value = client
        .get(format!("{base}/auth/challenge?user_id={user_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nonce = r["nonce"].as_str().unwrap();

    let sig = key.sign(format!("vc-login:{nonce}").as_bytes());
    let r: Value = client
        .post(format!("{base}/auth/login"))
        .json(&json!({ "user_id": user_id, "nonce": nonce, "sig": B64.encode(sig.to_bytes()) }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    r["token"].as_str().expect("login should return token").to_string()
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn ws_connect(server: &TestServer, user: &TestUser) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!(
        "{}?token={}",
        server.ws_base, user.token
    ))
    .await
    .unwrap();
    ws
}

/// Read events until one matches `ty`, failing after a timeout.
async fn expect_event(ws: &mut Ws, ty: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let msg = ws.next().await.expect("ws closed").unwrap();
            if let Message::Text(text) = msg {
                let v: Value = serde_json::from_str(&text).unwrap();
                if v["type"] == ty {
                    return v;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for event {ty}"))
}

#[tokio::test]
async fn full_flow() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let base = &server.base;

    let alice = make_user(&client, base, "alice").await;
    let bob = make_user(&client, base, "bob").await;

    // Alice creates a space; it comes with a default channel.
    let space: Value = client
        .post(format!("{base}/spaces"))
        .bearer_auth(&alice.token)
        .json(&json!({ "name": "game night" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let space_id = space["id"].as_str().unwrap().to_string();
    let channel_id = space["channels"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(space["current_epoch"], 1);

    // Alice uploads her own wrap of the epoch-1 space key.
    let r = client
        .post(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&alice.token)
        .json(&json!({ "epoch": 1, "wraps": [{ "user_id": alice.id, "wrapped": "sealed-for-alice" }] }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    // Bob can't see the space yet.
    let r = client
        .get(format!("{base}/spaces/{space_id}/members"))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Alice is online; she should receive a key_request when Bob joins.
    let mut alice_ws = ws_connect(&server, &alice).await;
    expect_event(&mut alice_ws, "hello").await;

    // Invite + accept.
    let invite: Value = client
        .post(format!("{base}/spaces/{space_id}/invites"))
        .bearer_auth(&alice.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let joined: Value = client
        .post(format!("{base}/invites/{}/accept", invite["token"].as_str().unwrap()))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(joined["id"].as_str().unwrap(), space_id);

    let req = expect_event(&mut alice_ws, "key_request").await;
    assert_eq!(req["user"]["user_id"].as_str().unwrap(), bob.id);

    // Alice wraps epoch 1 for Bob (join flow: non-owner-epoch upload allowed).
    let r = client
        .post(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&alice.token)
        .json(&json!({ "epoch": 1, "wraps": [{ "user_id": bob.id, "wrapped": "sealed-for-bob" }] }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    let keys: Value = client
        .get(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(keys["current_epoch"], 1);
    assert_eq!(keys["wraps"][0]["wrapped"], "sealed-for-bob");

    // Bob posts a (ciphertext) message; Alice gets it pushed and it persists.
    let mut bob_ws = ws_connect(&server, &bob).await;
    expect_event(&mut bob_ws, "hello").await;

    let posted: Value = client
        .post(format!("{base}/channels/{channel_id}/messages"))
        .bearer_auth(&bob.token)
        .json(&json!({ "epoch": 1, "nonce": "n0", "ct": "opaque-ciphertext", "sig": "s0" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pushed = expect_event(&mut alice_ws, "message_new").await;
    assert_eq!(pushed["message"]["id"], posted["id"]);
    assert_eq!(pushed["message"]["ct"], "opaque-ciphertext");

    let history: Value = client
        .get(format!("{base}/channels/{channel_id}/messages?limit=10"))
        .bearer_auth(&alice.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["messages"][0]["ct"], "opaque-ciphertext");
    assert_eq!(history["messages"][0]["sender_id"].as_str().unwrap(), bob.id);

    // Calls: alice joins, bob joins, signaling relays, leave notifies.
    alice_ws
        .send(Message::Text(
            json!({ "type": "call_join", "channel_id": channel_id }).to_string().into(),
        ))
        .await
        .unwrap();
    let roster = expect_event(&mut alice_ws, "call_roster").await;
    assert_eq!(roster["participants"].as_array().unwrap().len(), 1);

    // Bob isn't in the call but, as a space member, sees who is.
    let presence = expect_event(&mut bob_ws, "call_presence").await;
    assert_eq!(presence["channel_id"].as_str().unwrap(), channel_id);
    assert_eq!(presence["participants"][0].as_str().unwrap(), alice.id);
    // ...and a fresh connection gets a snapshot of running calls.
    let mut bob_ws2 = ws_connect(&server, &bob).await;
    let snap = expect_event(&mut bob_ws2, "call_presence").await;
    assert_eq!(snap["participants"][0].as_str().unwrap(), alice.id);
    drop(bob_ws2);

    // Streaming flag: only accepted from participants, fanned out as presence.
    alice_ws
        .send(Message::Text(
            json!({ "type": "call_streaming", "channel_id": channel_id, "streaming": true })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let live = expect_event(&mut bob_ws, "call_presence").await;
    assert_eq!(live["streaming"][0].as_str(), Some(alice.id.as_str()), "{live}");

    bob_ws
        .send(Message::Text(
            json!({ "type": "call_join", "channel_id": channel_id }).to_string().into(),
        ))
        .await
        .unwrap();
    let joined_evt = expect_event(&mut alice_ws, "call_peer_joined").await;
    assert_eq!(joined_evt["user_id"].as_str().unwrap(), bob.id);
    let roster = expect_event(&mut bob_ws, "call_roster").await;
    assert_eq!(roster["participants"].as_array().unwrap().len(), 2);

    bob_ws
        .send(Message::Text(
            json!({
                "type": "signal", "channel_id": channel_id, "to": alice.id,
                "payload": { "kind": "sdp", "sdp": "fake-offer" }, "sig": "fake-sig",
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let sig = expect_event(&mut alice_ws, "signal").await;
    assert_eq!(sig["from"].as_str().unwrap(), bob.id);
    assert_eq!(sig["payload"]["sdp"], "fake-offer");

    // Bob disconnects entirely → alice sees call_peer_left.
    drop(bob_ws);
    let left = expect_event(&mut alice_ws, "call_peer_left").await;
    assert_eq!(left["user_id"].as_str().unwrap(), bob.id);
    let presence = expect_event(&mut alice_ws, "call_presence").await;
    assert_eq!(presence["participants"].as_array().unwrap().len(), 1);
    assert_eq!(presence["streaming"][0].as_str().unwrap(), alice.id);

    // TURN credentials: HMAC ephemeral creds with both STUN and TURN urls.
    let turn: Value = client
        .get(format!("{base}/turn-credentials"))
        .bearer_auth(&alice.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turn["urls"].as_array().unwrap().len(), 2);
    assert!(turn["username"].as_str().unwrap().ends_with(&alice.id));
    assert!(!turn["credential"].as_str().unwrap().is_empty());

    // Unauthenticated access is rejected.
    let r = client.get(format!("{base}/spaces")).send().await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn space_creator_allowlist() {
    // alice's signing key is on the allowlist; bob's is not.
    let mut alice_seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut alice_seed);
    let alice_key = SigningKey::from_bytes(&alice_seed);
    let alice_sign_pub = B64.encode(alice_key.verifying_key().to_bytes());

    let server =
        spawn_server_with(|cfg| cfg.space_creator_sign_pubs = vec![alice_sign_pub.clone()]).await;
    let client = reqwest::Client::new();
    let base = &server.base;

    // Register alice using the allowlisted key directly so the pub matches.
    let alice_kem = B64.encode([7u8; 32]);
    let r: Value = client
        .post(format!("{base}/register"))
        .json(&json!({ "name": "alice", "sign_pub": alice_sign_pub, "kem_pub": alice_kem }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let alice_id = r["user_id"].as_str().unwrap().to_string();
    let alice_token = login(&client, base, &alice_id, &alice_key).await;

    let bob = make_user(&client, base, "bob").await;

    // bob is not on the allowlist → forbidden.
    let r = client
        .post(format!("{base}/spaces"))
        .bearer_auth(&bob.token)
        .json(&json!({ "name": "bob's space" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // alice is allowed.
    let r = client
        .post(format!("{base}/spaces"))
        .bearer_auth(&alice_token)
        .json(&json!({ "name": "alice's space" }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
}

#[tokio::test]
async fn max_users_limit() {
    let server = spawn_server_with(|cfg| cfg.max_users = Some(2)).await;
    let client = reqwest::Client::new();
    let base = &server.base;

    // The first two registrations fill the roster.
    make_user(&client, base, "one").await;
    make_user(&client, base, "two").await;

    // A third is rejected with 409 (uses a valid, distinct signing key so the
    // request reaches the cap check rather than failing key validation).
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    let key = SigningKey::from_bytes(&seed);
    let sign_pub = B64.encode(key.verifying_key().to_bytes());
    let kem_pub = B64.encode([2u8; 32]);
    let r = client
        .post(format!("{base}/register"))
        .json(&json!({ "name": "three", "sign_pub": sign_pub, "kem_pub": kem_pub }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
}

#[tokio::test]
async fn key_rotation_rules() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let base = &server.base;

    let owner = make_user(&client, base, "owner").await;
    let member = make_user(&client, base, "member").await;

    let space: Value = client
        .post(format!("{base}/spaces"))
        .bearer_auth(&owner.token)
        .json(&json!({ "name": "s" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let space_id = space["id"].as_str().unwrap();

    let invite: Value = client
        .post(format!("{base}/spaces/{space_id}/invites"))
        .bearer_auth(&owner.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    client
        .post(format!("{base}/invites/{}/accept", invite["token"].as_str().unwrap()))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap();

    // A non-owner cannot introduce a new epoch (rotation).
    let r = client
        .post(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&member.token)
        .json(&json!({ "epoch": 2, "wraps": [{ "user_id": member.id, "wrapped": "x" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // The owner can.
    let r: Value = client
        .post(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&owner.token)
        .json(&json!({ "epoch": 2, "wraps": [{ "user_id": owner.id, "wrapped": "x" }] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["current_epoch"], 2);

    // Epochs can't skip ahead.
    let r = client
        .post(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&owner.token)
        .json(&json!({ "epoch": 9, "wraps": [{ "user_id": owner.id, "wrapped": "x" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Removing the member deletes their wraps and bars further access.
    let r = client
        .delete(format!("{base}/spaces/{space_id}/members/{}", member.id))
        .bearer_auth(&owner.token)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let r = client
        .get(format!("{base}/spaces/{space_id}/keys"))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn stickers_owner_only() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let base = &server.base;

    let owner = make_user(&client, base, "owner").await;
    let member = make_user(&client, base, "member").await;

    let space: Value = client
        .post(format!("{base}/spaces"))
        .bearer_auth(&owner.token)
        .json(&json!({ "name": "s" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let space_id = space["id"].as_str().unwrap();

    let invite: Value = client
        .post(format!("{base}/spaces/{space_id}/invites"))
        .bearer_auth(&owner.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    client
        .post(format!("{base}/invites/{}/accept", invite["token"].as_str().unwrap()))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap();

    // The owner uploads a sticker (ciphertext blob, opaque to the server).
    let created: Value = client
        .post(format!("{base}/spaces/{space_id}/stickers"))
        .bearer_auth(&owner.token)
        .json(&json!({ "name": "wave", "epoch": 1, "nonce": "n", "ct": "opaque-webp" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sticker_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "wave");

    // A member can list and fetch the (still-encrypted) blob.
    let listed: Value = client
        .get(format!("{base}/spaces/{space_id}/stickers"))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["stickers"].as_array().unwrap().len(), 1);
    let full: Value = client
        .get(format!("{base}/spaces/{space_id}/stickers/{sticker_id}"))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(full["ct"], "opaque-webp");

    // A member cannot upload a sticker.
    let r = client
        .post(format!("{base}/spaces/{space_id}/stickers"))
        .bearer_auth(&member.token)
        .json(&json!({ "name": "nope", "epoch": 1, "nonce": "n", "ct": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Epoch must reference an existing space key.
    let r = client
        .post(format!("{base}/spaces/{space_id}/stickers"))
        .bearer_auth(&owner.token)
        .json(&json!({ "name": "future", "epoch": 9, "nonce": "n", "ct": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // A member cannot delete; the owner can.
    let r = client
        .delete(format!("{base}/spaces/{space_id}/stickers/{sticker_id}"))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = client
        .delete(format!("{base}/spaces/{space_id}/stickers/{sticker_id}"))
        .bearer_auth(&owner.token)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let listed: Value = client
        .get(format!("{base}/spaces/{space_id}/stickers"))
        .bearer_auth(&member.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["stickers"].as_array().unwrap().len(), 0);
}
