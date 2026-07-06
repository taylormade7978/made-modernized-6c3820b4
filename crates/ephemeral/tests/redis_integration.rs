//! Integration tests exercising the Redis ephemeral adapters against a *live*
//! Redis instance.
//!
//! These tests need a real server, which the default CI `build-and-test` job does
//! not provision, so they **self-skip** unless `MADE_REDIS_URL` (or `REDIS_URL`)
//! points at a reachable Redis. That keeps `cargo test --workspace` green
//! everywhere while still genuinely exercising TTL round-trips, the dual-axis
//! queue, and pub/sub delivery wherever a server is available:
//!
//! ```sh
//! docker run --rm -p 6379:6379 redis:7
//! MADE_REDIS_URL=redis://127.0.0.1:6379 cargo test -p ephemeral --test redis_integration
//! ```
//!
//! Each test uses a unique namespace so concurrent runs never collide on the
//! shared instance, and cleans up the keys it created.

use std::time::Duration;

use ephemeral::{connect, Candidate, MatchEvent, RedisConfig, RedisHandle};

/// The live Redis URL, or `None` when the suite should skip.
fn redis_url() -> Option<String> {
    std::env::var("MADE_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .ok()
}

/// A namespace unique to this run so tests never clobber one another (or another
/// tenant) on the shared instance.
fn unique_namespace(suffix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("made-it-{suffix}-{nanos}")
}

/// Connect with a unique namespace, or `None` to signal skip.
async fn handle(suffix: &str) -> Option<RedisHandle> {
    let url = redis_url()?;
    let cfg = RedisConfig::new(url)
        .with_namespace(unique_namespace(suffix))
        .with_connect_timeout(Duration::from_millis(2_000));
    Some(connect(&cfg).await.expect("live redis should connect"))
}

#[tokio::test]
async fn connect_fails_fast_on_unreachable_redis() {
    // Point at a port nothing listens on with a tight timeout: connect must
    // return an error rather than hang. This one needs no live server.
    let cfg =
        RedisConfig::new("redis://127.0.0.1:6390").with_connect_timeout(Duration::from_millis(300));
    let result = connect(&cfg).await;
    assert!(result.is_err(), "connect to a dead port must fail fast");
}

#[tokio::test]
async fn match_state_round_trips_with_ttl() {
    let Some(handle) = handle("state").await else {
        eprintln!("skipping match_state_round_trips_with_ttl: no MADE_REDIS_URL");
        return;
    };
    let store = handle.match_state();

    // Write within the match lifecycle with an explicit TTL, then read back.
    store
        .write_snapshot("m-1", b"snapshot-v1", Some(Duration::from_secs(120)))
        .await
        .unwrap();
    let got = store.read_snapshot("m-1").await.unwrap();
    assert_eq!(got.as_deref(), Some(&b"snapshot-v1"[..]));

    // The configured TTL is in force (a positive, bounded remaining time).
    let ttl = store.ttl_seconds("m-1").await.unwrap().expect("ttl set");
    assert!(ttl > 0 && ttl <= 120, "ttl {ttl} outside (0, 120]");

    // A missing match reads as None.
    assert_eq!(store.read_snapshot("missing").await.unwrap(), None);

    assert!(store.clear("m-1").await.unwrap());
    assert_eq!(store.read_snapshot("m-1").await.unwrap(), None);
}

#[tokio::test]
async fn matchmaking_queue_enqueue_dequeue_and_dual_axis_lookup() {
    let Some(handle) = handle("mmq").await else {
        eprintln!("skipping matchmaking_queue_...: no MADE_REDIS_URL");
        return;
    };
    let queue = handle.matchmaking();
    let q = "ranked";

    // MMR ~1500, level 12 target. Enqueue a spread of candidates.
    queue
        .enqueue(q, &Candidate::new("near", 1490.0, 11))
        .await
        .unwrap(); // both axes in band
    queue
        .enqueue(q, &Candidate::new("far-mmr", 2000.0, 12))
        .await
        .unwrap(); // MMR out of band
    queue
        .enqueue(q, &Candidate::new("far-level", 1495.0, 40))
        .await
        .unwrap(); // level out of band
    queue
        .enqueue(q, &Candidate::new("self", 1500.0, 12))
        .await
        .unwrap(); // the target itself
    assert_eq!(queue.len(q).await.unwrap(), 4);

    let target = Candidate::new("self", 1500.0, 12);
    let found = queue
        .find_candidates(q, &target, 150.0, 5, 10)
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|c| c.id.as_str()).collect();
    // Only "near" satisfies BOTH axes; the target itself is excluded.
    assert_eq!(ids, vec!["near"]);

    // Dequeue removes from both axes.
    assert!(queue.dequeue(q, "near").await.unwrap());
    assert_eq!(queue.len(q).await.unwrap(), 3);
    let found = queue
        .find_candidates(q, &target, 150.0, 5, 10)
        .await
        .unwrap();
    assert!(found.is_empty(), "near was dequeued");

    // Clean up the rest.
    for id in ["far-mmr", "far-level", "self"] {
        queue.dequeue(q, id).await.unwrap();
    }
}

#[tokio::test]
async fn pubsub_delivers_match_events_to_subscribers() {
    let Some(handle) = handle("pubsub").await else {
        eprintln!("skipping pubsub_delivers_...: no MADE_REDIS_URL");
        return;
    };
    let channel = handle.events();

    // Subscribe first so the publish that follows is delivered.
    let mut sub = channel.subscribe("m-1").await.unwrap();

    let event = MatchEvent::new(
        "m-1",
        "card.played",
        serde_json::json!({ "card": "Slugger" }),
    );
    let receivers = channel.publish(&event).await.unwrap();
    assert!(receivers >= 1, "the subscriber should have received it");

    // The subscriber receives the exact event, within a bounded wait.
    let received = tokio::time::timeout(Duration::from_secs(2), sub.next_event())
        .await
        .expect("event should arrive before the timeout")
        .expect("subscription should not error")
        .expect("a message, not a closed stream");
    assert_eq!(received, event);
}
