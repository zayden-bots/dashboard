use std::collections::VecDeque;
use std::time::Duration;

use deadlock_protocol::MAX_TEXT_BYTES;
use deadlock_protocol::ws::{
    GameEvent,
    MAX_FRAME_BYTES,
    MAX_ROSTER_LEN,
    MAX_UP_FRAMES_PER_SEC,
    Up,
};
use tokio::time::Instant;

const FRAME_WINDOW: Duration = Duration::from_secs(1);
const FIRST_FRAME: &str = "first frame must be hello";
const TOO_LONG: &str = "string too long";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Hello,
    Game,
    Ignore,
    Error(String),
    Policy(&'static str),
    AccountMismatch,
}

#[must_use]
pub fn judge(text: &str, linked_account: u32, hello_done: bool) -> Verdict {
    if text.len() > MAX_FRAME_BYTES {
        return Verdict::Policy("frame too large");
    }
    let up = match serde_json::from_str::<Up>(text) {
        Ok(up) => up,
        Err(e) => {
            return if hello_done {
                Verdict::Error(truncated(format!("invalid frame: {e}")))
            } else {
                Verdict::Policy(FIRST_FRAME)
            };
        },
    };
    match up {
        Up::Hello { app_version, .. } => {
            if app_version.len() > MAX_TEXT_BYTES {
                Verdict::Policy(TOO_LONG)
            } else if hello_done {
                Verdict::Error("hello already received".to_owned())
            } else {
                Verdict::Hello
            }
        },
        Up::Game { .. } | Up::Unknown if !hello_done => Verdict::Policy(FIRST_FRAME),
        Up::Unknown => Verdict::Ignore,
        Up::Game { event, .. } => judge_event(&event, linked_account),
    }
}

fn truncated(mut message: String) -> String {
    let mut end = message.len().min(MAX_TEXT_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}

const fn judge_event(event: &GameEvent, linked_account: u32) -> Verdict {
    if let GameEvent::Map { name: Some(name) }
    | GameEvent::Hero { name }
    | GameEvent::Cue { name, .. } = event
        && name.len() > MAX_TEXT_BYTES
    {
        return Verdict::Policy(TOO_LONG);
    }
    if let GameEvent::Roster { enemies } = event
        && enemies.len() > MAX_ROSTER_LEN
    {
        return Verdict::Policy("roster too long");
    }
    if let GameEvent::Account { id } = event
        && *id != linked_account
    {
        return Verdict::AccountMismatch;
    }
    Verdict::Game
}

#[derive(Debug, Default)]
pub struct FrameRate {
    recent: VecDeque<Instant>,
}

impl FrameRate {
    #[must_use]
    pub const fn new() -> Self {
        Self { recent: VecDeque::new() }
    }

    pub fn admit(&mut self, now: Instant) -> bool {
        let limit = usize::try_from(MAX_UP_FRAMES_PER_SEC).unwrap_or(usize::MAX);
        while self
            .recent
            .front()
            .is_some_and(|front| now.duration_since(*front) >= FRAME_WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= limit {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}
