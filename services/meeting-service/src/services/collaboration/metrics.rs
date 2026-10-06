//! Process-wide collaboration counters (Part F §72). Exposed with the RTC counters on
//! `GET /internal/rtc/metrics`. Labels never include email, display name, message
//! content or medical data — these are plain counters.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        #[derive(Default)]
        pub struct CollabMetrics { $(pub $name: AtomicU64,)* }

        impl CollabMetrics {
            pub const fn new() -> Self { Self { $($name: AtomicU64::new(0),)* } }
            pub fn snapshot(&self) -> Value {
                let mut map = Map::new();
                $(map.insert(stringify!($name).to_string(), Value::from(self.$name.load(Ordering::Relaxed)));)*
                Value::Object(map)
            }
        }
    };
}

counters!(
    media_mutes,
    audio_locks,
    video_locks,
    screen_share_locks,
    screen_share_starts,
    screen_share_stops,
    provider_media_failures,
    spotlight_changes,
    chat_sends,
    chat_rejects,
    chat_moderations,
    rate_limited,
    reactions_sent,
    reactions_dropped,
    hand_raises,
    breakouts_created,
    breakouts_opened,
    breakout_moves,
    breakouts_closed,
    breakout_routing_failures,
    breakout_drift,
    polls_opened,
    poll_responses,
    poll_validation_failures,
    qna_submissions,
    qna_moderations,
    anonymous_projection_guards,
);

pub static COLLAB_METRICS: CollabMetrics = CollabMetrics::new();

pub fn inc(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}
