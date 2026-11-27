//! Mission commands from the phone, shared by the Telegram and Discord bridges: what `/missions`,
//! `/mission <id>`, `/pause`, `/resume`, `/stop`, `/mission new` and `/status` do against the
//! daemon, with the rate limit and the stop confirmation.
//!
//! The bridge decides *whether* a message may run a command (the paired owner, in a private chat,
//! typed by them and not forwarded) and calls [`ControlHub`] only then. The hub never decides who
//! the owner is; it keeps what must be shared by every command: the rate limit, and the stops the
//! owner was asked to confirm. Both are kept per [`OwnerId`], the account the service
//! authenticated, so one account can never use up another's window or answer another's question.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use channel_api::control::{
    Action, ConfirmBook, Control, Fleet, MAX_GOAL_CHARS, MissionBrief, MissionTally, OwnerId, Pick,
    Rate, RateLimit, Say, pick_mission, say, status_extra,
};
use channel_api::notice::{Flavor, NoticeLang};
use parking_lot::Mutex;
use protocol::BotId;

use crate::daemon_client::DaemonClient;
use crate::error::TelegramError;

/// How long a "stop it for good?" question can be answered.
const CONFIRM_TTL: Duration = Duration::from_secs(5 * 60);

/// What to send back for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The text (empty: say nothing, the owner was already told they are too fast).
    pub text: String,
    /// The mission whose stop the owner is asked to confirm: attach the two buttons
    /// ([`channel_api::control::stop_confirm_callback`] and
    /// [`channel_api::control::stop_cancel_callback`]).
    pub confirm: Option<String>,
}

impl Answer {
    fn text(text: String) -> Self {
        Self {
            text,
            confirm: None,
        }
    }
}

struct State {
    /// A rate window for each owner (the bridge only calls the hub for the paired owner, so this
    /// holds one entry, or two after a re-pairing).
    limits: HashMap<OwnerId, RateLimit>,
    confirm: ConfirmBook,
}

impl State {
    /// Count a command of `owner`, or say it is too fast.
    fn check(&mut self, owner: OwnerId, ctl: &Control, now: Instant) -> Rate {
        self.limits.entry(owner).or_default().check(ctl, now)
    }

    /// Count a button press of `owner`.
    fn check_press(&mut self, owner: OwnerId, now: Instant) -> Rate {
        self.limits.entry(owner).or_default().check_press(now)
    }
}

/// Runs control commands for one bridge.
pub struct ControlHub {
    client: Arc<dyn DaemonClient>,
    state: Mutex<State>,
}

impl std::fmt::Debug for ControlHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlHub").finish_non_exhaustive()
    }
}

/// The answer for a daemon error. The daemon's own text is never repeated to the chat.
fn failure(lang: NoticeLang, flavor: Flavor, e: &TelegramError) -> Answer {
    let s = match e {
        TelegramError::DaemonHttp { status: 503, .. } => Say::Unavailable,
        TelegramError::DaemonHttp { status: 404, .. } => Say::NotFound,
        _ => Say::Failed,
    };
    Answer::text(say(lang, flavor, &s))
}

impl ControlHub {
    /// A hub that talks to the daemon through `client`.
    #[must_use]
    pub fn new(client: Arc<dyn DaemonClient>) -> Self {
        Self {
            client,
            state: Mutex::new(State {
                limits: HashMap::new(),
                confirm: ConfirmBook::new(CONFIRM_TTL),
            }),
        }
    }

    /// Run `ctl` for `owner` (the account the service authenticated). `bot` is the bot a new
    /// mission goes to: the chat's own, else the default; only read for `/mission new`.
    pub async fn handle(
        &self,
        owner: OwnerId,
        ctl: &Control,
        lang: NoticeLang,
        flavor: Flavor,
        bot: Option<BotId>,
    ) -> Answer {
        match self.state.lock().check(owner, ctl, Instant::now()) {
            Rate::Allowed => {}
            Rate::Refused => return Answer::text(say(lang, flavor, &Say::TooFast)),
            Rate::Silent => return Answer::text(String::new()),
        }
        let reply = |s: Say<'_>| Answer::text(say(lang, flavor, &s));
        match ctl {
            Control::Help => reply(Say::Help),
            Control::Missions => match self.client.list_missions().await {
                Ok(list) if list.is_empty() => reply(Say::NoMissions),
                Ok(list) => reply(Say::List(&list)),
                Err(e) => failure(lang, flavor, &e),
            },
            Control::Mission(arg) => {
                if arg.is_empty() {
                    return reply(Say::NeedMissionId);
                }
                let brief = match self.resolve(arg).await {
                    Ok(b) => b,
                    Err(a) => return a.into_answer(lang, flavor),
                };
                match self.client.mission(&brief.id).await {
                    Ok(view) => reply(Say::Detail(&view)),
                    Err(e) => failure(lang, flavor, &e),
                }
            }
            Control::Pause(arg) => self.change(owner, Action::Pause, arg, lang, flavor).await,
            Control::Resume(arg) => self.change(owner, Action::Resume, arg, lang, flavor).await,
            Control::Stop(arg) => self.change(owner, Action::Stop, arg, lang, flavor).await,
            Control::NewMission(text) => self.start(text, lang, flavor, bot).await,
        }
    }

    /// Find the mission `typed` names among the daemon's missions.
    async fn resolve(&self, typed: &str) -> Result<MissionBrief, Unresolved> {
        let list = self
            .client
            .list_missions()
            .await
            .map_err(Unresolved::Daemon)?;
        match pick_mission(&list, typed) {
            Pick::One(m) => Ok(m.clone()),
            Pick::None => Err(Unresolved::NotFound),
            Pick::Ambiguous(n) => Err(Unresolved::Ambiguous(n)),
            Pick::Invalid => Err(Unresolved::BadId),
        }
    }

    async fn change(
        &self,
        owner: OwnerId,
        action: Action,
        arg: &str,
        lang: NoticeLang,
        flavor: Flavor,
    ) -> Answer {
        if arg.is_empty() {
            return Answer::text(say(lang, flavor, &Say::NeedId(action)));
        }
        let brief = match self.resolve(arg).await {
            Ok(b) => b,
            Err(a) => return a.into_answer(lang, flavor),
        };
        if action == Action::Stop {
            if brief.is_ended() {
                return Answer::text(say(lang, flavor, &Say::CannotNow(action, &brief)));
            }
            // Stopping cannot be undone: the button decides, not the command.
            self.state
                .lock()
                .confirm
                .ask(&brief.id, owner, Instant::now());
            return Answer {
                text: say(lang, flavor, &Say::ConfirmStop(&brief)),
                confirm: Some(brief.id),
            };
        }
        self.apply(action, &brief, lang, flavor).await
    }

    async fn apply(
        &self,
        action: Action,
        brief: &MissionBrief,
        lang: NoticeLang,
        flavor: Flavor,
    ) -> Answer {
        match self.client.mission_action(&brief.id, action).await {
            Ok(after) => Answer::text(say(lang, flavor, &Say::Changed(action, &after))),
            Err(TelegramError::DaemonHttp {
                status: 409 | 400, ..
            }) => {
                // The state does not allow it (or the bot left the Full level): say what it is.
                let now = self
                    .client
                    .mission(&brief.id)
                    .await
                    .map_or_else(|_| brief.clone(), |v| v.mission);
                Answer::text(say(lang, flavor, &Say::CannotNow(action, &now)))
            }
            Err(e) => failure(lang, flavor, &e),
        }
    }

    async fn start(
        &self,
        text: &str,
        lang: NoticeLang,
        flavor: Flavor,
        bot: Option<BotId>,
    ) -> Answer {
        let reply = |s: Say<'_>| Answer::text(say(lang, flavor, &s));
        if text.is_empty() {
            return reply(Say::NewUsage);
        }
        if text.chars().count() > MAX_GOAL_CHARS {
            return reply(Say::GoalTooLong);
        }
        let Some(bot) = bot else {
            return reply(Say::NoBot);
        };
        match self.client.start_mission(bot, text).await {
            Ok(m) => reply(Say::Started(&m)),
            Err(TelegramError::DaemonHttp { status: 400, .. }) => reply(Say::NotStarted),
            Err(TelegramError::DaemonHttp { status: 404, .. }) => reply(Say::NoBot),
            Err(e) => failure(lang, flavor, &e),
        }
    }

    /// The owner pressed "Stop it" for mission `id`. Only a question this hub asked this owner,
    /// a moment ago, and only once. A press counts against the owner's rate window like a
    /// command; one that is too fast is ignored (empty answer).
    pub async fn confirm_stop(
        &self,
        owner: OwnerId,
        id: &str,
        lang: NoticeLang,
        flavor: Flavor,
    ) -> Answer {
        {
            let mut state = self.state.lock();
            let now = Instant::now();
            if state.check_press(owner, now) != Rate::Allowed {
                return Answer::text(String::new());
            }
            if !state.confirm.take(id, owner, now) {
                return Answer::text(say(lang, flavor, &Say::StopExpired));
            }
        }
        let brief = match self.client.mission(id).await {
            Ok(v) => v.mission,
            Err(e) => return failure(lang, flavor, &e),
        };
        if brief.is_ended() {
            return Answer::text(say(lang, flavor, &Say::CannotNow(Action::Stop, &brief)));
        }
        self.apply(Action::Stop, &brief, lang, flavor).await
    }

    /// The owner pressed "Keep going": the question is closed and nothing changes. Rate limited
    /// like [`Self::confirm_stop`].
    pub fn cancel_stop(
        &self,
        owner: OwnerId,
        id: &str,
        lang: NoticeLang,
        flavor: Flavor,
    ) -> Answer {
        let mut state = self.state.lock();
        let now = Instant::now();
        if state.check_press(owner, now) != Rate::Allowed {
            return Answer::text(String::new());
        }
        state.confirm.take(id, owner, now);
        Answer::text(say(lang, flavor, &Say::StopCancelled))
    }

    /// The lines `/status` adds to the daemon's health line: bots, runs, missions. A part the
    /// daemon cannot give is left out.
    pub async fn status_lines(&self, lang: NoticeLang, flavor: Flavor) -> String {
        let (bots, runs, missions) = tokio::join!(
            self.client.list_bots(),
            self.client.running_run_count(),
            self.client.list_missions()
        );
        let fleet = Fleet {
            bots: bots
                .map(|b| b.into_iter().map(|b| b.name).collect())
                .unwrap_or_default(),
            running_runs: runs.ok(),
            missions: missions.ok().map(|m| MissionTally::of(&m)),
        };
        status_extra(lang, flavor, &fleet)
    }
}

/// Why an id did not name a mission.
enum Unresolved {
    NotFound,
    Ambiguous(usize),
    BadId,
    Daemon(TelegramError),
}

impl Unresolved {
    fn into_answer(self, lang: NoticeLang, flavor: Flavor) -> Answer {
        let s = match self {
            Self::NotFound => Say::NotFound,
            Self::Ambiguous(n) => Say::Ambiguous(n),
            Self::BadId => Say::BadId,
            Self::Daemon(e) => return failure(lang, flavor, &e),
        };
        Answer::text(say(lang, flavor, &s))
    }
}
