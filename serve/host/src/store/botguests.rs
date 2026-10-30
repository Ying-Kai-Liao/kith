//! Bot guests (docs/kith-bot-guests.md 4.3): cloud agents with their own key that belong to one person. The bots, the
//! invites that let a key become one, and the conversations that map a provider's own conversation id to a Kith
//! thread. Who may use which agent stays kith-auth's truth; `agents` here mirrors what this host set there.

use super::{Store, logged, now_ms};
use crate::LockExt;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

/// Where a bot is in its life. `Denied` and `Revoked` are final.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BotState {
    /// The invite exists; no key has joined.
    Invited,
    /// A key joined at the cloud and is in the ACL, but has not shown the invite token.
    Joining,
    /// Proven, waiting for the Mac owner's allow.
    Pending,
    Active,
    Denied,
    Revoked,
}

impl BotState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invited => "invited",
            Self::Joining => "joining",
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Denied => "denied",
            Self::Revoked => "revoked",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "invited" => Self::Invited,
            "joining" => Self::Joining,
            "pending" => Self::Pending,
            "active" => Self::Active,
            "denied" => Self::Denied,
            // An unreadable state grants nothing.
            _ => Self::Revoked,
        }
    }

    /// Counts against the owner's ten bots and may still hold a device.
    pub fn live(self) -> bool {
        matches!(self, Self::Invited | Self::Joining | Self::Pending | Self::Active)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BotGuest {
    pub bot_id: String,
    /// The kith-auth person this bot runs as (`bot-` + 8 hex).
    pub person_id: String,
    /// None is the Mac owner's own bot.
    pub owner_person: Option<String>,
    pub name: String,
    pub provider: String,
    /// What the owner picked at invite time (kith-auth is the truth).
    pub agents: Vec<String>,
    pub state: BotState,
    pub device_key: Option<String>,
    pub cloud_device: Option<String>,
    pub rate_per_hour: i64,
    pub concurrent: i64,
    /// How long a room waits for this bot's post on its turn before it counts as a pass (seconds).
    pub turn_timeout_s: i64,
    /// The kith-auth profile the host wakes this bot through at its room turn; None: no wake.
    pub wake_profile: Option<String>,
    pub created_at: i64,
    pub joined_at: Option<i64>,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub revoked_why: Option<String>,
}

#[allow(clippy::struct_field_names, reason = "wire names")]
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BotInvite {
    pub invite_id: String,
    pub bot_id: String,
    #[serde(skip)]
    pub token_hash: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl BotInvite {
    pub fn open(&self, now: i64) -> bool {
        self.used_at.is_none() && self.revoked_at.is_none() && self.expires_at > now
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BotConversation {
    pub conversation_id: String,
    pub bot_id: String,
    pub agent: String,
    /// The `bots` row holding the transcript.
    pub chat_bot_id: String,
    /// The root entry in that chat: the thread.
    pub thread_root: String,
    pub external_ref: Option<String>,
    pub room_id: Option<String>,
    pub speaker: Option<String>,
    pub origin_label: String,
    pub created_at: i64,
    pub last_at: i64,
}

const GUEST_COLS: &str = "bot_id, person_id, owner_person, name, provider, agents, state, device_key, cloud_device, \
     rate_per_hour, concurrent, created_at, joined_at, decided_at, decided_by, last_seen_at, revoked_at, revoked_why, \
     turn_timeout_s, wake_profile";

fn row_guest(r: &rusqlite::Row) -> rusqlite::Result<BotGuest> {
    let (agents, state): (String, String) = (r.get(5)?, r.get(6)?);
    Ok(BotGuest {
        bot_id: r.get(0)?,
        person_id: r.get(1)?,
        owner_person: r.get(2)?,
        name: r.get(3)?,
        provider: r.get(4)?,
        agents: serde_json::from_str(&agents).unwrap_or_default(),
        state: BotState::parse(&state),
        device_key: r.get(7)?,
        cloud_device: r.get(8)?,
        rate_per_hour: r.get(9)?,
        concurrent: r.get(10)?,
        created_at: r.get(11)?,
        joined_at: r.get(12)?,
        decided_at: r.get(13)?,
        decided_by: r.get(14)?,
        last_seen_at: r.get(15)?,
        revoked_at: r.get(16)?,
        revoked_why: r.get(17)?,
        turn_timeout_s: r.get(18)?,
        wake_profile: r.get(19)?,
    })
}

const INVITE_COLS: &str = "invite_id, bot_id, token_hash, created_at, expires_at, used_at, revoked_at";

fn row_invite(r: &rusqlite::Row) -> rusqlite::Result<BotInvite> {
    Ok(BotInvite {
        invite_id: r.get(0)?,
        bot_id: r.get(1)?,
        token_hash: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        used_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

const CONV_COLS: &str = "conversation_id, bot_id, agent, chat_bot_id, thread_root, external_ref, room_id, speaker, \
     origin_label, created_at, last_at";

fn row_conv(r: &rusqlite::Row) -> rusqlite::Result<BotConversation> {
    Ok(BotConversation {
        conversation_id: r.get(0)?,
        bot_id: r.get(1)?,
        agent: r.get(2)?,
        chat_bot_id: r.get(3)?,
        thread_root: r.get(4)?,
        external_ref: r.get(5)?,
        room_id: r.get(6)?,
        speaker: r.get(7)?,
        origin_label: r.get(8)?,
        created_at: r.get(9)?,
        last_at: r.get(10)?,
    })
}

impl Store {
    pub fn put_bot_guest(&self, g: &BotGuest) -> Result<()> {
        self.db.locked().execute(
            &format!("INSERT INTO bot_guests({GUEST_COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"),
            params![
                g.bot_id,
                g.person_id,
                g.owner_person,
                g.name,
                g.provider,
                serde_json::to_string(&g.agents)?,
                g.state.as_str(),
                g.device_key,
                g.cloud_device,
                g.rate_per_hour,
                g.concurrent,
                g.created_at,
                g.joined_at,
                g.decided_at,
                g.decided_by,
                g.last_seen_at,
                g.revoked_at,
                g.revoked_why,
                g.turn_timeout_s,
                g.wake_profile
            ],
        )?;
        Ok(())
    }

    pub fn bot_guest(&self, id: &str) -> Option<BotGuest> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {GUEST_COLS} FROM bot_guests WHERE bot_id = ?1"), [id], row_guest);
        logged("bot guest", row.optional()).flatten()
    }

    pub fn bot_guest_of_person(&self, person: &str) -> Option<BotGuest> {
        let c = self.db.locked();
        let row =
            c.query_row(&format!("SELECT {GUEST_COLS} FROM bot_guests WHERE person_id = ?1"), [person], row_guest);
        logged("bot guest by person", row.optional()).flatten()
    }

    /// The bot a device key belongs to, while that key is its current one.
    pub fn bot_guest_of_device(&self, key: &str) -> Option<BotGuest> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!("SELECT {GUEST_COLS} FROM bot_guests WHERE device_key = ?1 AND revoked_at IS NULL"),
            [key],
            row_guest,
        );
        logged("bot guest by device", row.optional()).flatten()
    }

    /// Oldest first.
    pub fn bot_guests(&self) -> Result<Vec<BotGuest>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!("SELECT {GUEST_COLS} FROM bot_guests ORDER BY created_at, bot_id"))?;
        Ok(st.query_map([], row_guest)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether any bot could still hold a device, so the cloud state keeps being pulled for unclaimed computers.
    pub fn has_live_bot_guests(&self) -> bool {
        let c = self.db.locked();
        let n = c.query_row(
            "SELECT COUNT(*) FROM bot_guests WHERE state IN ('invited','joining','pending','active')",
            [],
            |r| r.get::<_, i64>(0),
        );
        logged("live bots", n).unwrap_or(0) > 0
    }

    /// A key joined: the bot is `joining` with that device (a re-key replaces the old key).
    pub fn bind_bot_device(&self, id: &str, key: &str, cloud_device: &str, state: BotState) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE bot_guests SET device_key = ?2, cloud_device = ?3, state = ?4, last_seen_at = ?5
             WHERE bot_id = ?1 AND revoked_at IS NULL AND state IN ('invited','joining','pending','active')",
            params![id, key, cloud_device, state.as_str(), now_ms()],
        )?;
        Ok(n > 0)
    }

    pub fn set_bot_proven(&self, id: &str, state: BotState, by: &str, now: i64) -> Result<()> {
        self.db.locked().execute(
            "UPDATE bot_guests SET state = ?2, joined_at = COALESCE(joined_at, ?4), decided_by = ?3,
               decided_at = CASE WHEN ?2 = 'active' THEN ?4 ELSE decided_at END
             WHERE bot_id = ?1 AND revoked_at IS NULL",
            params![id, state.as_str(), by, now],
        )?;
        Ok(())
    }

    /// Pending -> active or denied by a person's answer. False when the bot was not pending.
    pub fn decide_bot_guest(&self, id: &str, state: BotState, by: &str, now: i64) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE bot_guests SET state = ?2, decided_by = ?3, decided_at = ?4 WHERE bot_id = ?1 AND state = 'pending'",
            params![id, state.as_str(), by, now],
        )?;
        Ok(n > 0)
    }

    /// Ends a bot for good (`denied` or `revoked`) and forgets its key. False when it had already ended, so the
    /// cascade that follows runs once however many triggers race.
    pub fn end_bot_guest(&self, id: &str, state: BotState, why: &str, now: i64) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE bot_guests SET state = ?2, revoked_at = ?4, revoked_why = ?3
             WHERE bot_id = ?1 AND state IN ('invited','joining','pending','active')",
            params![id, state.as_str(), why, now],
        )?;
        Ok(n > 0)
    }

    pub fn set_bot_limits(&self, id: &str, rate_per_hour: i64, concurrent: i64) -> Result<()> {
        self.db.locked().execute(
            "UPDATE bot_guests SET rate_per_hour = ?2, concurrent = ?3 WHERE bot_id = ?1",
            params![id, rate_per_hour, concurrent],
        )?;
        Ok(())
    }

    /// A bot's room settings: how long a room waits for its post, and the profile it is woken through.
    pub fn set_bot_room_settings(&self, id: &str, turn_timeout_s: i64, wake_profile: Option<&str>) -> Result<()> {
        self.db.locked().execute(
            "UPDATE bot_guests SET turn_timeout_s = ?2, wake_profile = ?3 WHERE bot_id = ?1",
            params![id, turn_timeout_s, wake_profile],
        )?;
        Ok(())
    }

    pub fn touch_bot_guest(&self, id: &str) {
        let c = self.db.locked();
        if let Err(error) =
            c.execute("UPDATE bot_guests SET last_seen_at = ?2 WHERE bot_id = ?1", params![id, now_ms()])
        {
            tracing::warn!(%error, "couldn't note a bot's last call");
        }
    }

    /// Deletes a bot that never left this computer (the cloud refused its invite).
    pub fn delete_bot_guest(&self, id: &str) -> Result<()> {
        let mut c = self.db.locked();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM bot_invites WHERE bot_id = ?1", [id])?;
        tx.execute("DELETE FROM bot_guests WHERE bot_id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Live bots of one owner (None: the Mac owner), for the ten-bot limit.
    pub fn live_bot_count(&self, owner: Option<&str>) -> usize {
        self.bot_guests()
            .unwrap_or_default()
            .iter()
            .filter(|g| g.state.live() && g.owner_person.as_deref() == owner)
            .count()
    }

    // MARK: invites

    pub fn put_bot_invite(&self, i: &BotInvite) -> Result<()> {
        self.db.locked().execute(
            &format!("INSERT INTO bot_invites({INVITE_COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7)"),
            params![i.invite_id, i.bot_id, i.token_hash, i.created_at, i.expires_at, i.used_at, i.revoked_at],
        )?;
        Ok(())
    }

    pub fn bot_invite(&self, id: &str) -> Option<BotInvite> {
        let c = self.db.locked();
        let row = c.query_row(&format!("SELECT {INVITE_COLS} FROM bot_invites WHERE invite_id = ?1"), [id], row_invite);
        logged("bot invite", row.optional()).flatten()
    }

    pub fn bot_invites_of(&self, bot: &str) -> Result<Vec<BotInvite>> {
        let c = self.db.locked();
        let mut st =
            c.prepare(&format!("SELECT {INVITE_COLS} FROM bot_invites WHERE bot_id = ?1 ORDER BY created_at DESC"))?;
        Ok(st.query_map([bot], row_invite)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_bot_invite(&self, id: &str) -> Result<()> {
        self.db.locked().execute("DELETE FROM bot_invites WHERE invite_id = ?1", [id])?;
        Ok(())
    }

    /// Takes an invite that is still open for good; false when it was used, revoked or expired meanwhile.
    pub fn use_bot_invite(&self, id: &str, now: i64) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE bot_invites SET used_at = ?2 WHERE invite_id = ?1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?2",
            params![id, now],
        )?;
        Ok(n > 0)
    }

    /// Revokes an invite that was not used; false when there was nothing to revoke (a repeat is fine).
    pub fn revoke_bot_invite(&self, id: &str) -> Result<bool> {
        let n = self.db.locked().execute(
            "UPDATE bot_invites SET revoked_at = ?2 WHERE invite_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
            params![id, now_ms()],
        )?;
        Ok(n > 0)
    }

    // MARK: conversations

    pub fn put_bot_conversation(&self, c: &BotConversation) -> Result<()> {
        self.db.locked().execute(
            &format!("INSERT INTO bot_conversations({CONV_COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"),
            params![
                c.conversation_id,
                c.bot_id,
                c.agent,
                c.chat_bot_id,
                c.thread_root,
                c.external_ref,
                c.room_id,
                c.speaker,
                c.origin_label,
                c.created_at,
                c.last_at
            ],
        )?;
        Ok(())
    }

    pub fn bot_conversation_by_id(&self, id: &str) -> Option<BotConversation> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!("SELECT {CONV_COLS} FROM bot_conversations WHERE conversation_id = ?1"),
            [id],
            row_conv,
        );
        logged("bot conversation", row.optional()).flatten()
    }

    pub fn bot_conversation_by_ref(&self, bot: &str, external_ref: &str) -> Option<BotConversation> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!("SELECT {CONV_COLS} FROM bot_conversations WHERE bot_id = ?1 AND external_ref = ?2"),
            params![bot, external_ref],
            row_conv,
        );
        logged("bot conversation by ref", row.optional()).flatten()
    }

    /// The conversation whose thread is `root` of chat `chat`.
    pub fn bot_conversation_of_thread(&self, chat: &str, root: &str) -> Option<BotConversation> {
        let c = self.db.locked();
        let row = c.query_row(
            &format!("SELECT {CONV_COLS} FROM bot_conversations WHERE chat_bot_id = ?1 AND thread_root = ?2"),
            params![chat, root],
            row_conv,
        );
        logged("bot conversation by thread", row.optional()).flatten()
    }

    /// Newest first.
    pub fn bot_conversations_of(&self, bot: &str, limit: i64) -> Result<Vec<BotConversation>> {
        let c = self.db.locked();
        let mut st = c.prepare(&format!(
            "SELECT {CONV_COLS} FROM bot_conversations WHERE bot_id = ?1 ORDER BY last_at DESC, conversation_id LIMIT ?2"
        ))?;
        Ok(st.query_map(params![bot, limit], row_conv)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn touch_bot_conversation(&self, id: &str, speaker: Option<&str>) -> Result<()> {
        self.db.locked().execute(
            "UPDATE bot_conversations SET last_at = ?2, speaker = COALESCE(?3, speaker) WHERE conversation_id = ?1",
            params![id, now_ms(), speaker],
        )?;
        Ok(())
    }

    // MARK: people and audit

    /// Kai's "allow this person's bots without asking".
    pub fn set_auto_bots(&self, person: &str, on: bool) -> Result<bool> {
        let n =
            self.db.locked().execute("UPDATE people SET auto_bots = ?2 WHERE person_id = ?1", params![person, on])?;
        Ok(n > 0)
    }

    /// Calls `method` by `person` that were allowed since `since`, for the hourly ask limit.
    pub fn audit_count_since(&self, person: &str, method: &str, since: i64) -> i64 {
        let c = self.db.locked();
        let n = c.query_row(
            "SELECT COUNT(*) FROM audit WHERE person_id = ?1 AND method = ?2 AND allowed = 1 AND at >= ?3",
            params![person, method, since],
            |r| r.get::<_, i64>(0),
        );
        logged("audit count", n).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::store::tests::temp_store;

    pub(crate) fn guest(id: &str, owner: Option<&str>) -> BotGuest {
        BotGuest {
            bot_id: id.into(),
            person_id: format!("bot-{id}"),
            owner_person: owner.map(str::to_owned),
            name: "Rex".into(),
            provider: "grok".into(),
            agents: vec!["house".into()],
            state: BotState::Invited,
            device_key: None,
            cloud_device: None,
            rate_per_hour: 60,
            concurrent: 2,
            turn_timeout_s: 300,
            wake_profile: None,
            created_at: 1,
            joined_at: None,
            decided_at: None,
            decided_by: None,
            last_seen_at: None,
            revoked_at: None,
            revoked_why: None,
        }
    }

    #[test]
    fn a_bot_ends_once_and_keeps_its_key_unique_only_while_live() {
        let s = temp_store();
        s.put_bot_guest(&guest("a", None)).unwrap();
        assert!(s.bind_bot_device("a", "dk1", "bd_1", BotState::Joining).unwrap());
        assert_eq!(s.bot_guest_of_device("dk1").unwrap().bot_id, "a");
        assert!(s.end_bot_guest("a", BotState::Revoked, "owner", 5).unwrap());
        assert!(!s.end_bot_guest("a", BotState::Revoked, "kai", 6).unwrap(), "ending twice is a no-op");
        assert_eq!(s.bot_guest("a").unwrap().revoked_why.as_deref(), Some("owner"));
        assert!(s.bot_guest_of_device("dk1").is_none(), "a revoked bot's key is nobody's");
        assert!(!s.bind_bot_device("a", "dk2", "bd_2", BotState::Joining).unwrap(), "a revoked bot can't join");
        // The same key may serve a new bot after the old one ended.
        s.put_bot_guest(&guest("b", Some("sam"))).unwrap();
        assert!(s.bind_bot_device("b", "dk1", "bd_3", BotState::Joining).unwrap());
        assert_eq!(s.live_bot_count(Some("sam")), 1);
        assert_eq!(s.live_bot_count(None), 0);
    }

    #[test]
    fn conversations_are_found_by_id_ref_and_thread() {
        let s = temp_store();
        s.put_bot_guest(&guest("a", None)).unwrap();
        let c = BotConversation {
            conversation_id: "cv_1".into(),
            bot_id: "a".into(),
            agent: "house".into(),
            chat_bot_id: "chat".into(),
            thread_root: "root".into(),
            external_ref: Some("grok:1".into()),
            room_id: None,
            speaker: None,
            origin_label: "from Grok".into(),
            created_at: 1,
            last_at: 1,
        };
        s.put_bot_conversation(&c).unwrap();
        assert_eq!(s.bot_conversation_by_id("cv_1"), Some(c.clone()));
        assert_eq!(s.bot_conversation_by_ref("a", "grok:1"), Some(c.clone()));
        assert_eq!(s.bot_conversation_of_thread("chat", "root"), Some(c.clone()));
        assert!(s.bot_conversation_by_ref("b", "grok:1").is_none());
        let again = BotConversation { conversation_id: "cv_2".into(), thread_root: "r2".into(), ..c };
        assert!(s.put_bot_conversation(&again).is_err(), "one conversation per (bot, ref)");
    }

    #[test]
    fn a_bot_invite_is_used_once() {
        let s = temp_store();
        s.put_bot_guest(&guest("a", None)).unwrap();
        let i = BotInvite {
            invite_id: "bi_1".into(),
            bot_id: "a".into(),
            token_hash: "h".into(),
            created_at: 1,
            expires_at: now_ms() + 10_000,
            used_at: None,
            revoked_at: None,
        };
        s.put_bot_invite(&i).unwrap();
        assert!(s.bot_invite("bi_1").unwrap().open(now_ms()));
        assert!(s.use_bot_invite("bi_1", now_ms()).unwrap());
        assert!(!s.use_bot_invite("bi_1", now_ms()).unwrap());
        assert!(!s.revoke_bot_invite("bi_1").unwrap());
    }
}
