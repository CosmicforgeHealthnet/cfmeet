//! Channel analytics for the owner and admins (ADR 0072): counts of recorded events only,
//! per UTC day, never a person.
//!
//! * Follows / unfollows: `messaging_channel_follow_events` (recorded since `statsSince`;
//!   the follower count at the end of a day is derived backwards from today's count, and
//!   is `null` for days before recording began).
//! * Views: one row per person and post (`messaging_channel_post_views`), so a person
//!   viewing again is not counted again; `uniqueViewers` counts people.
//! * Reactions: the current reactions, on the day they were (last) given.
//! * Poll participation: voters per poll (a person once per poll).
//! * Forwards: shares into chats (messages sent).
//! * Engagement rate: (reactions + poll voters) ÷ views in the range; `null` without views.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{forbidden, load};
use crate::{
    db,
    error::{Error, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const DEFAULT_DAYS: i64 = 30;
pub const MAX_DAYS: i64 = 366;
pub const TOP_POSTS: i64 = 10;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyticsQuery {
    /// First UTC day (`YYYY-MM-DD`; default 29 days before `to`).
    #[serde(default)]
    pub from: Option<NaiveDate>,
    /// Last UTC day (default today).
    #[serde(default)]
    pub to: Option<NaiveDate>,
}

#[derive(FromQueryResult)]
struct DayCount {
    day: NaiveDate,
    n: i64,
}

#[derive(FromQueryResult)]
struct FollowDay {
    day: NaiveDate,
    follows: i64,
    unfollows: i64,
}

#[derive(FromQueryResult)]
struct TopPost {
    id: Uuid,
    kind: String,
    words: Option<String>,
    created_at: DateTime<Utc>,
    views: i64,
    reactions: i64,
    voters: i64,
    forwards: i64,
}

fn range(q: &AnalyticsQuery, today: NaiveDate) -> Result<(NaiveDate, NaiveDate)> {
    let to = q.to.unwrap_or(today).min(today);
    let from = q.from.unwrap_or(to - Duration::days(DEFAULT_DAYS - 1));
    if from > to {
        return Err(Error::validation("from must not be after to"));
    }
    if (to - from).num_days() + 1 > MAX_DAYS {
        return Err(Error::validation("The range is at most 366 days"));
    }
    Ok((from, to))
}

async fn by_day(
    state: &AppState,
    sql: &str,
    channel: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<HashMap<NaiveDate, i64>> {
    let rows: Vec<DayCount> = db::all(
        &state.db,
        sql,
        vec![channel.into(), start.into(), end.into()],
    )
    .await?;
    Ok(rows.into_iter().map(|r| (r.day, r.n)).collect())
}

pub async fn analytics(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    q: AnalyticsQuery,
) -> Result<Value> {
    let me = acting.principal_id;
    let row = load(&state.db, ctx, me, id).await?;
    if !row.staff(me) {
        return Err(forbidden(
            "NOT_CHANNEL_ADMIN",
            "Only the channel's owner and admins can see analytics",
        ));
    }
    let now = state.now();
    let (from, to) = range(&q, now.date_naive())?;
    let start = from.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
    let end = (to + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default()
        .and_utc();
    let day = "(x.at AT TIME ZONE 'UTC')::date";

    // Follows / unfollows per day, and every net change after the range's start (to walk
    // the follower count back from today).
    let follow_days: Vec<FollowDay> = db::all(
        &state.db,
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, \
           count(*) FILTER (WHERE kind = 'FOLLOW') AS follows, count(*) FILTER (WHERE kind = 'UNFOLLOW') AS unfollows \
           FROM messaging_channel_follow_events WHERE channel_id = $1 AND occurred_at >= $2 GROUP BY 1",
        vec![id.into(), start.into()],
    )
    .await?;
    let follows: HashMap<NaiveDate, (i64, i64)> = follow_days
        .iter()
        .map(|f| (f.day, (f.follows, f.unfollows)))
        .collect();
    let views = by_day(
        state,
        &format!(
            "SELECT {day} AS day, count(*) AS n FROM (SELECT v.viewed_at AS at FROM messaging_channel_post_views v \
               JOIN messaging_channel_posts p ON p.id = v.post_id WHERE p.channel_id = $1 AND v.viewed_at >= $2 AND v.viewed_at < $3) x GROUP BY 1"
        ),
        id,
        start,
        end,
    )
    .await?;
    let viewers = by_day(
        state,
        &format!(
            "SELECT {day} AS day, count(DISTINCT x.who) AS n FROM (SELECT v.viewed_at AS at, v.principal_id AS who \
               FROM messaging_channel_post_views v JOIN messaging_channel_posts p ON p.id = v.post_id \
              WHERE p.channel_id = $1 AND v.viewed_at >= $2 AND v.viewed_at < $3) x GROUP BY 1"
        ),
        id,
        start,
        end,
    )
    .await?;
    let reactions = by_day(
        state,
        &format!(
            "SELECT {day} AS day, count(*) AS n FROM (SELECT r.reacted_at AS at FROM messaging_channel_reactions r \
               JOIN messaging_channel_posts p ON p.id = r.post_id WHERE p.channel_id = $1 AND r.reacted_at >= $2 AND r.reacted_at < $3) x GROUP BY 1"
        ),
        id,
        start,
        end,
    )
    .await?;
    let votes = by_day(
        state,
        &format!(
            "SELECT {day} AS day, count(*) AS n FROM (SELECT min(v.voted_at) AS at FROM messaging_channel_poll_votes v \
               JOIN messaging_channel_posts p ON p.id = v.post_id WHERE p.channel_id = $1 \
              GROUP BY v.post_id, v.principal_id HAVING min(v.voted_at) >= $2 AND min(v.voted_at) < $3) x GROUP BY 1"
        ),
        id,
        start,
        end,
    )
    .await?;
    let forwards = by_day(
        state,
        &format!(
            "SELECT {day} AS day, coalesce(sum(x.n), 0)::bigint AS n FROM (SELECT f.occurred_at AS at, f.recipients AS n \
               FROM messaging_channel_forwards f WHERE f.channel_id = $1 AND f.occurred_at >= $2 AND f.occurred_at < $3) x GROUP BY 1"
        ),
        id,
        start,
        end,
    )
    .await?;
    let posts_published = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_channel_posts WHERE channel_id = $1 AND created_at >= $2 AND created_at < $3",
        vec![id.into(), start.into(), end.into()],
    )
    .await?;
    let unique_viewers = db::scalar_i64(
        &state.db,
        "SELECT count(DISTINCT v.principal_id) FROM messaging_channel_post_views v JOIN messaging_channel_posts p \
           ON p.id = v.post_id WHERE p.channel_id = $1 AND v.viewed_at >= $2 AND v.viewed_at < $3",
        vec![id.into(), start.into(), end.into()],
    )
    .await?;

    // Walk the follower count back from now: followers at the end of day D = current minus
    // the net change after D. Days before recording began are unknown.
    let stats_day = row.stats_since.date_naive();
    let mut series = Vec::new();
    let mut net_after: i64 = follows
        .iter()
        .filter(|(d, _)| **d > to)
        .map(|(_, (f, u))| f - u)
        .sum();
    let mut d = to;
    while d >= from {
        let (f, u) = follows.get(&d).copied().unwrap_or((0, 0));
        let followers_end = (d >= stats_day).then_some(row.followers - net_after);
        series.push(json!({
            "date": d.to_string(),
            "follows": f,
            "unfollows": u,
            "followers": followers_end,
            "views": views.get(&d).copied().unwrap_or(0),
            "uniqueViewers": viewers.get(&d).copied().unwrap_or(0),
            "reactions": reactions.get(&d).copied().unwrap_or(0),
            "pollVoters": votes.get(&d).copied().unwrap_or(0),
            "forwards": forwards.get(&d).copied().unwrap_or(0),
        }));
        net_after += f - u;
        d -= Duration::days(1);
    }
    series.reverse();
    let start_followers = (from > stats_day).then_some(row.followers - net_after);
    let sum = |m: &HashMap<NaiveDate, i64>| m.values().sum::<i64>();
    let total_follows: i64 = follows
        .iter()
        .filter(|(d, _)| **d >= from && **d <= to)
        .map(|(_, (f, _))| f)
        .sum();
    let total_unfollows: i64 = follows
        .iter()
        .filter(|(d, _)| **d >= from && **d <= to)
        .map(|(_, (_, u))| u)
        .sum();
    let (total_views, total_reactions, total_voters) = (sum(&views), sum(&reactions), sum(&votes));
    let engagement = (total_views > 0).then(|| {
        ((total_reactions + total_voters) as f64 / total_views as f64 * 10_000.0).round() / 10_000.0
    });
    let top: Vec<TopPost> = db::all(
        &state.db,
        &format!(
            "SELECT p.id, p.kind, p.words, p.created_at, \
               (SELECT count(*) FROM messaging_channel_post_views v WHERE v.post_id = p.id AND v.viewed_at >= $2 AND v.viewed_at < $3) AS views, \
               (SELECT count(*) FROM messaging_channel_reactions r WHERE r.post_id = p.id AND r.reacted_at >= $2 AND r.reacted_at < $3) AS reactions, \
               (SELECT count(DISTINCT v.principal_id) FROM messaging_channel_poll_votes v WHERE v.post_id = p.id AND v.voted_at >= $2 AND v.voted_at < $3) AS voters, \
               (SELECT coalesce(sum(f.recipients), 0)::bigint FROM messaging_channel_forwards f WHERE f.post_id = p.id AND f.occurred_at >= $2 AND f.occurred_at < $3) AS forwards \
             FROM messaging_channel_posts p WHERE p.channel_id = $1 AND p.created_at < $3 \
             ORDER BY views + reactions + voters + forwards DESC, p.created_at DESC LIMIT {TOP_POSTS}"
        ),
        vec![id.into(), start.into(), end.into()],
    )
    .await?;
    let top: Vec<Value> = top
        .iter()
        .filter(|p| p.views + p.reactions + p.voters + p.forwards > 0)
        .map(|p| {
            json!({
                "postId": p.id,
                "kind": p.kind,
                "excerpt": p.words.as_deref().map(|w| w.chars().take(80).collect::<String>()),
                "createdAt": crate::conversations::service::rfc3339(p.created_at),
                "views": p.views,
                "reactions": p.reactions,
                "pollVoters": p.voters,
                "forwards": p.forwards,
            })
        })
        .collect();
    Ok(json!({
        "channelId": id,
        "from": from.to_string(),
        "to": to.to_string(),
        "statsSince": crate::conversations::service::rfc3339(row.stats_since),
        "followers": {
            "current": row.followers,
            "atStart": start_followers,
            "follows": total_follows,
            "unfollows": total_unfollows,
            "net": total_follows - total_unfollows,
        },
        "posts": {
            "published": posts_published,
            "views": total_views,
            "uniqueViewers": unique_viewers,
            "reactions": total_reactions,
            "pollVoters": total_voters,
            "forwards": sum(&forwards),
        },
        "engagementRate": engagement,
        "series": series,
        "topPosts": top,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn ranges_default_to_thirty_days_and_are_bounded() {
        let today = d("2026-12-31");
        assert_eq!(
            range(&AnalyticsQuery::default(), today).unwrap(),
            (d("2026-12-02"), today)
        );
        let q = AnalyticsQuery {
            from: Some(d("2026-12-10")),
            to: Some(d("2027-02-01")),
        };
        assert_eq!(range(&q, today).unwrap(), (d("2026-12-10"), today));
        let bad = AnalyticsQuery {
            from: Some(d("2026-12-20")),
            to: Some(d("2026-12-10")),
        };
        assert!(range(&bad, today).is_err());
        let long = AnalyticsQuery {
            from: Some(d("2025-01-01")),
            to: Some(d("2026-12-31")),
        };
        assert!(range(&long, today).is_err());
    }
}
