//! Feedback loop for fix/workflow-resurrect-tombstone (2026-09-03).
//!
//! Bug: `buzz workflows update` after `workflows delete` re-upserts the
//! `workflows` row — the deleted workflow comes back to life (resurrection).
//! Diagnosis: NIP-09 delete removes the workflows-table row, but a later
//! kind:30620 def event for the same d-tag is treated as a fresh create.
//!
//! Fix under test: a deletion tombstone makes later 30620 def events for the
//! same workflow id REJECTED with a clear error instead of resurrecting it.
//!
//! Requires a running relay (isolated harness relay on :3030 recommended):
//!
//! ```text
//! RELAY_URL=ws://localhost:3030 RELAY_HTTP_URL=http://localhost:3030 \
//!   cargo test -p buzz-test-client --test e2e_workflow_resurrect -- --ignored --nocapture
//! ```

use std::time::Duration;

use buzz_test_client::BuzzTestClient;
use nostr::{Alphabet, EventBuilder, Filter, Keys, Kind, SingleLetterTag, Tag};

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3030".to_string())
}

fn relay_http_url() -> String {
    std::env::var("RELAY_HTTP_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn create_test_channel(keys: &Keys) -> String {
    let client = reqwest::Client::new();
    let pubkey_hex = keys.public_key().to_hex();
    let channel_uuid = uuid::Uuid::new_v4();
    let channel_name = format!("wf-res-{}", channel_uuid.simple());

    let event = EventBuilder::new(Kind::Custom(9007), "")
        .tags(vec![
            Tag::parse(["h", &channel_uuid.to_string()]).unwrap(),
            Tag::parse(["name", &channel_name]).unwrap(),
            Tag::parse(["channel_type", "stream"]).unwrap(),
            Tag::parse(["visibility", "open"]).unwrap(),
        ])
        .sign_with_keys(keys)
        .unwrap();

    let resp = client
        .post(format!("{}/events", relay_http_url()))
        .header("X-Pubkey", &pubkey_hex)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&event).unwrap())
        .send()
        .await
        .expect("submit create-channel event");
    assert!(
        resp.status().is_success(),
        "channel creation event failed: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("parse event response");
    assert!(
        body["accepted"].as_bool().unwrap_or(false),
        "channel creation not accepted: {}",
        body
    );

    channel_uuid.to_string()
}

async fn list_workflows(url: &str, keys: &Keys, channel: &str) -> Vec<nostr::Event> {
    let mut ws = BuzzTestClient::connect(url, keys).await.expect("connect");
    let sid = format!("wf-res-{}", uuid::Uuid::new_v4());
    let filter = Filter::new()
        .kind(Kind::Custom(30620))
        .custom_tags(SingleLetterTag::lowercase(Alphabet::H), [channel]);
    ws.subscribe(&sid, vec![filter]).await.expect("subscribe");
    let events = ws
        .collect_until_eose(&sid, Duration::from_secs(5))
        .await
        .expect("list EOSE");
    ws.disconnect().await.ok();
    events
}

/// Red while resurrection exists; green once the tombstone rejects the
/// post-delete update.
#[tokio::test]
#[ignore]
async fn workflow_update_after_delete_is_rejected() {
    let url = relay_url();
    let keys = Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();
    let channel = create_test_channel(&keys).await;

    // 1. Publish a workflow def (what `buzz workflows create` does).
    let workflow_id = uuid::Uuid::new_v4().to_string();
    let yaml = "name: resurrect-probe\ndescription: diag\ntrigger:\n  on: schedule\n  interval: 1h\nsteps:\n  - id: step1\n    name: noop\n    action: send_message\n    text: \"noop\"\n";
    let def = EventBuilder::new(Kind::Custom(30620), yaml)
        .tags([
            Tag::parse(["d", &workflow_id]).unwrap(),
            Tag::parse(["h", &channel]).unwrap(),
            Tag::parse(["name", "resurrect-probe"]).unwrap(),
        ])
        .sign_with_keys(&keys)
        .expect("sign workflow def");
    let mut ws = BuzzTestClient::connect(&url, &keys).await.expect("connect");
    let ok = ws.send_event(def).await.expect("send def");
    assert!(ok.accepted, "workflow def rejected: {}", ok.message);

    // 2. Delete it (kind-5 with a-tag 30620:<pubkey>:<uuid>).
    let a_coord = format!("30620:{pubkey_hex}:{workflow_id}");
    let del = EventBuilder::new(Kind::EventDeletion, "")
        .tags(vec![Tag::parse(["a", &a_coord]).unwrap()])
        .sign_with_keys(&keys)
        .expect("sign delete");
    let ok = ws.send_event(del).await.expect("send delete");
    assert!(ok.accepted, "delete rejected: {}", ok.message);
    ws.disconnect().await.expect("disconnect");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 3. THE RESURRECTION ATTEMPT: publish an updated def for the same d-tag
    //    (what `buzz workflows update` does). Must be REJECTED with the
    //    tombstone error — before the fix this succeeded and re-created the
    //    workflows row.
    let yaml_update = "name: resurrect-probe\nenabled: false\ntrigger:\n  on: schedule\n  interval: 8760h\nsteps:\n  - id: step1\n    name: noop\n    action: send_message\n    text: \"noop\"\n";
    let update = EventBuilder::new(Kind::Custom(30620), yaml_update)
        .tags([
            Tag::parse(["d", &workflow_id]).unwrap(),
            Tag::parse(["h", &channel]).unwrap(),
            Tag::parse(["name", "resurrect-probe"]).unwrap(),
        ])
        .sign_with_keys(&keys)
        .expect("sign update def");
    let mut ws = BuzzTestClient::connect(&url, &keys)
        .await
        .expect("reconnect");
    let ok = ws.send_event(update).await.expect("send update");
    assert!(
        !ok.accepted,
        "update-after-delete must be rejected, got accepted: {}",
        ok.message
    );
    assert!(
        ok.message.contains("deleted"),
        "rejection should say the workflow was deleted, got: {}",
        ok.message
    );

    // 4. A NEW workflow id in the same channel still works — deletion must
    //    not poison the channel or the owner.
    let workflow_id2 = uuid::Uuid::new_v4().to_string();
    let def2 = EventBuilder::new(Kind::Custom(30620), yaml)
        .tags([
            Tag::parse(["d", &workflow_id2]).unwrap(),
            Tag::parse(["h", &channel]).unwrap(),
            Tag::parse(["name", "resurrect-probe-2"]).unwrap(),
        ])
        .sign_with_keys(&keys)
        .expect("sign def2");
    let ok = ws.send_event(def2).await.expect("send def2");
    assert!(ok.accepted, "new workflow must be accepted: {}", ok.message);
    ws.disconnect().await.expect("disconnect");

    // 5. The rejected update must not have become the live def for the old
    //    coordinate: only def2 is served; the rejected update is absent.
    //    (Complements e2e_workflow_delete.rs, which asserts the deleted def
    //    leaves the 30620 view once events-row soft-delete lands.)
    tokio::time::sleep(Duration::from_secs(1)).await;
    let listed = list_workflows(&url, &keys, &channel).await;
    assert!(
        !listed.iter().any(|e| e.content.contains("interval: 8760h")),
        "rejected update def must not be served: {:?}",
        listed.iter().map(|e| e.content.clone()).collect::<Vec<_>>()
    );
}
