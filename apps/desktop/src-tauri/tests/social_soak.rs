//! A4-T12 soak: launchers against the **real** API (in process, fresh PostgreSQL database) for a
//! long time, with the process RSS sampled along the way.
//!
//! - `sustained_chat_keeps_memory_flat`: two launchers exchange 2 messages per second
//!   (`VGAMES_SOAK_CHAT_SECS`, default 120; the A4-T12 run is 3600). Every message arrives
//!   exactly once and RSS stays flat after warm-up.
//! - `idle_sockets_stay_up`: two connected launchers do nothing (`VGAMES_SOAK_IDLE_SECS`,
//!   default 120; the A4-T12 run is 86400). No socket drops, and RSS stays flat.
//!
//! RSS covers the whole process: the API, PostgreSQL client pools and both launchers. The
//! launchers keep their databases on disk, as installed launchers do, so their message history
//! does not count as memory growth.
//!
//! `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/postgres \
//!  VGAMES_SOAK_CHAT_SECS=3600 cargo test -p vgames-desktop --test social_soak \
//!  -- --ignored --nocapture sustained_chat`

#![allow(
    dead_code, // tests/support/chat.rs is shared; each test uses part of it
    unused_imports,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

include!("support/chat.rs");

use std::time::Instant;

/// Resident set size of this process, in KiB (Linux).
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

fn secs(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
}

/// RSS samples over a run; flat means the end is within `limit` of the post-warm-up level.
struct Rss {
    samples: Vec<(Duration, u64)>,
    started: Instant,
}

impl Rss {
    fn new() -> Self {
        Self {
            samples: vec![(Duration::ZERO, rss_kib())],
            started: Instant::now(),
        }
    }

    fn sample(&mut self) {
        self.samples.push((self.started.elapsed(), rss_kib()));
    }

    /// Prints the samples and checks that the last quarter's median is at most `limit_kib`
    /// above the median of the second quarter (after warm-up).
    fn assert_flat(&self, what: &str, limit_kib: u64) {
        for (t, kib) in &self.samples {
            eprintln!("{what}: t={:>6}s rss={:>7} KiB", t.as_secs(), kib);
        }
        let n = self.samples.len();
        assert!(n >= 8, "{what}: too few samples ({n})");
        let median = |range: std::ops::Range<usize>| {
            let mut v: Vec<u64> = self.samples[range].iter().map(|(_, k)| *k).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        let warm = median(n / 4..n / 2);
        let end = median(n * 3 / 4..n);
        eprintln!("{what}: rss after warm-up {warm} KiB, at the end {end} KiB");
        assert!(
            end <= warm + limit_kib,
            "{what}: RSS grew from {warm} KiB to {end} KiB"
        );
    }
}

/// Counts arrivals by text without keeping the recorder's queue growing.
fn drain(l: &mut Launcher, got: &mut std::collections::HashSet<String>, dupes: &mut u32) {
    while let Ok(seen) = l.seen.try_recv() {
        if let Seen::Received(m) = seen
            && let MessageBody::Text { text } = m.body
            && !got.insert(text)
        {
            *dupes += 1;
        }
    }
}

async fn soak_pair(api: &Api, dir: &std::path::Path) -> (Launcher, Launcher, Uuid) {
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;
    let base = api.base.clone();
    let alice = launcher_with_db(
        api,
        &base,
        "alice",
        alice_id,
        Db::open(&dir.join("alice.db")).unwrap(),
    )
    .await;
    let bob = launcher_with_db(
        api,
        &base,
        "bob",
        bob_id,
        Db::open(&dir.join("bob.db")).unwrap(),
    )
    .await;
    alice.device().await;
    bob.device().await;
    let conv = alice
        .service
        .conversation_open_direct(bob_id)
        .await
        .unwrap();
    (alice, bob, conv.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak; needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn sustained_chat_keeps_memory_flat() {
    let duration = secs("VGAMES_SOAK_CHAT_SECS", 120);
    let api = start_api().await;
    let dir = std::env::temp_dir().join(format!("vgames-soak-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut alice, mut bob, conv) = soak_pair(&api, &dir).await;

    let mut rss = Rss::new();
    let sample_every = (duration / 40).max(Duration::from_secs(1));
    let mut next_sample = Instant::now() + sample_every;
    let (mut to_bob, mut to_alice) = (0u32, 0u32);
    let mut bob_got = std::collections::HashSet::new();
    let mut alice_got = std::collections::HashSet::new();
    let mut dupes = 0u32;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let started = Instant::now();
    let mut i = 0u32;
    while started.elapsed() < duration {
        tick.tick().await;
        // 2 messages per second in total, alternating directions.
        if i.is_multiple_of(2) {
            alice
                .service
                .message_send(conv, format!("a{i}"))
                .await
                .unwrap();
            to_bob += 1;
        } else {
            bob.service
                .message_send(conv, format!("b{i}"))
                .await
                .unwrap();
            to_alice += 1;
        }
        i += 1;
        drain(&mut bob, &mut bob_got, &mut dupes);
        drain(&mut alice, &mut alice_got, &mut dupes);
        if Instant::now() >= next_sample {
            rss.sample();
            next_sample += sample_every;
        }
    }
    // Everything sent arrives.
    let until = Instant::now() + Duration::from_secs(60);
    while (bob_got.len() as u32) < to_bob || (alice_got.len() as u32) < to_alice {
        assert!(
            Instant::now() < until,
            "undelivered: bob {}/{to_bob}, alice {}/{to_alice}",
            bob_got.len(),
            alice_got.len()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        drain(&mut bob, &mut bob_got, &mut dupes);
        drain(&mut alice, &mut alice_got, &mut dupes);
    }
    rss.sample();
    eprintln!(
        "chat soak: {}s, {} messages ({to_bob} to bob, {to_alice} to alice), {dupes} duplicates",
        duration.as_secs(),
        to_bob + to_alice
    );
    assert_eq!(dupes, 0);
    rss.assert_flat("chat soak", 16 * 1024);

    alice.shutdown.cancel();
    bob.shutdown.cancel();
    api.stop();
    api.drop_database().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "soak; needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn idle_sockets_stay_up() {
    let duration = secs("VGAMES_SOAK_IDLE_SECS", 120);
    let api = start_api().await;
    let dir = std::env::temp_dir().join(format!("vgames-soak-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut alice, mut bob, conv) = soak_pair(&api, &dir).await;

    let mut rss = Rss::new();
    let sample_every = (duration / 40).max(Duration::from_secs(1));
    let started = Instant::now();
    let mut drops = 0u32;
    while started.elapsed() < duration {
        tokio::time::sleep(sample_every).await;
        for l in [&mut alice, &mut bob] {
            while let Ok(seen) = l.seen.try_recv() {
                if matches!(&seen, Seen::Connection(c) if c.state != SocialConnectionState::Connected)
                {
                    drops += 1;
                }
            }
        }
        rss.sample();
    }
    eprintln!(
        "idle soak: {}s, {drops} connection drops",
        duration.as_secs()
    );
    assert_eq!(drops, 0, "idle sockets dropped");
    // Still usable at the end.
    alice.send(conv, "still there?").await;
    bob.received_text("still there?").await;
    rss.assert_flat("idle soak", 8 * 1024);

    alice.shutdown.cancel();
    bob.shutdown.cancel();
    api.stop();
    api.drop_database().await;
    std::fs::remove_dir_all(&dir).ok();
}
