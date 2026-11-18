use std::sync::Arc;
use std::time::Duration;

use deadlock_protocol::error::{ApiError, ErrorCode};
use deadlock_protocol::ws::{
    CLOSE_ACCOUNT_MISMATCH,
    CLOSE_POLICY,
    CLOSE_UNAUTHORIZED,
    Down,
};
use sqlx::PgPool;
use tokio::time::{Instant, MissedTickBehavior};
use topcoat::router::content::websocket::{
    CloseFrame,
    Message,
    WebSocket,
    close_code,
};
use topcoat::router::request::Bytes;
use tracing::warn;

use super::frames::{FrameRate, Verdict, judge};
use super::hub::{Mailbox, SocketInfo, WsHub, WsTiming};
use super::verify;
use crate::deadlock_app::auth::AppCaller;

const CLOSE_DRAIN: Duration = Duration::from_secs(1);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_FRAME: &str = "first frame must be hello";
const TEXT_ONLY: &str = "text frames only";
const TOO_MANY_FRAMES: &str = "too many frames";
const ACCOUNT_MISMATCH: &str = "account mismatch";

enum Step {
    Continue,
    Close(u16, &'static str),
    Stop,
}

pub async fn run(
    mut socket: WebSocket,
    hub: Arc<WsHub>,
    pool: PgPool,
    caller: AppCaller,
    account_id: u32,
) {
    let timing = hub.timing();
    let (registration, mut mailbox) =
        hub.register(caller.token_id, caller.discord_id, account_id);
    let info = SocketInfo {
        id: registration.id(),
        token_id: caller.token_id,
        discord_id: caller.discord_id,
        account_id,
    };

    match verify::check(&pool, &info).await {
        Ok(Some(code)) => {
            close(&mut socket, code, close_reason(code)).await;
            return;
        },
        Err(e) => {
            warn!(error = %e, "deadlock: websocket registration check failed");
            close(&mut socket, close_code::ERROR, "internal error").await;
            return;
        },
        Ok(None) => {},
    }

    let mut rate = FrameRate::new();
    if hello(&mut socket, &mut mailbox, &mut rate, timing, account_id).await {
        serve(&mut socket, mailbox, &mut rate, timing, account_id).await;
    }
}

async fn hello(
    socket: &mut WebSocket,
    mailbox: &mut Mailbox,
    rate: &mut FrameRate,
    timing: WsTiming,
    account_id: u32,
) -> bool {
    let hello_deadline = Instant::now() + timing.hello;
    loop {
        let message = tokio::select! {
            () = tokio::time::sleep_until(hello_deadline) => {
                close(socket, CLOSE_POLICY, "hello timeout").await;
                return false;
            },
            changed = mailbox.close.changed() => {
                match on_close(changed.is_ok(), mailbox) {
                    Step::Continue => continue,
                    Step::Close(code, reason) => {
                        close(socket, code, reason).await;
                        return false;
                    },
                    Step::Stop => return false,
                }
            },
            message = socket.recv() => message,
        };
        let message = match message {
            None => return false,
            Some(Err(_)) => {
                close(socket, CLOSE_POLICY, "invalid frame").await;
                return false;
            },
            Some(Ok(message)) => message,
        };
        if !rate.admit(Instant::now()) {
            close(socket, CLOSE_POLICY, TOO_MANY_FRAMES).await;
            return false;
        }
        match message {
            Message::Close(_) => return false,
            Message::Ping(_) | Message::Pong(_) => {},
            Message::Binary(_) => {
                close(socket, CLOSE_POLICY, TEXT_ONLY).await;
                return false;
            },
            Message::Text(text) => match judge(&text, account_id, false) {
                Verdict::Hello => {
                    let welcome = Down::Welcome { account_id: Some(account_id) };
                    return matches!(
                        send_down(socket, &welcome).await,
                        Step::Continue
                    );
                },
                Verdict::Policy(reason) => {
                    close(socket, CLOSE_POLICY, reason).await;
                    return false;
                },
                Verdict::Game
                | Verdict::Ignore
                | Verdict::Error(_)
                | Verdict::AccountMismatch => {
                    close(socket, CLOSE_POLICY, FIRST_FRAME).await;
                    return false;
                },
            },
        }
    }
}

async fn serve(
    socket: &mut WebSocket,
    mut mailbox: Mailbox,
    rate: &mut FrameRate,
    timing: WsTiming,
    account_id: u32,
) {
    let mut ping =
        tokio::time::interval_at(Instant::now() + timing.ping, timing.ping);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let idle = tokio::time::sleep(timing.idle);
    tokio::pin!(idle);

    loop {
        let step = tokio::select! {
            biased;
            changed = mailbox.close.changed() => on_close(changed.is_ok(), &mailbox),
            down = mailbox.outbound.recv() => match down {
                Some(down) => send_down(socket, &down).await,
                None => Step::Stop,
            },
            message = socket.recv() => {
                idle.as_mut().reset(Instant::now() + timing.idle);
                on_message(socket, rate, account_id, message).await
            },
            _ = ping.tick() => {
                if send(socket, Message::Ping(Bytes::new())).await {
                    Step::Continue
                } else {
                    Step::Stop
                }
            },
            () = &mut idle => Step::Close(CLOSE_POLICY, "idle timeout"),
        };
        match step {
            Step::Continue => {},
            Step::Close(code, reason) => {
                close(socket, code, reason).await;
                return;
            },
            Step::Stop => return,
        }
    }
}

async fn on_message(
    socket: &mut WebSocket,
    rate: &mut FrameRate,
    account_id: u32,
    message: Option<topcoat::Result<Message>>,
) -> Step {
    let message = match message {
        None => return Step::Stop,
        Some(Err(_)) => return Step::Close(CLOSE_POLICY, "invalid frame"),
        Some(Ok(message)) => message,
    };
    if !rate.admit(Instant::now()) {
        return Step::Close(CLOSE_POLICY, TOO_MANY_FRAMES);
    }
    match message {
        Message::Close(_) => Step::Stop,
        Message::Ping(_) | Message::Pong(_) => Step::Continue,
        Message::Binary(_) => Step::Close(CLOSE_POLICY, TEXT_ONLY),
        Message::Text(text) => match judge(&text, account_id, true) {
            Verdict::Hello | Verdict::Game | Verdict::Ignore => Step::Continue,
            Verdict::Error(message) => {
                let error = Down::Error {
                    error: ApiError {
                        code: ErrorCode::BadRequest,
                        message,
                        retry_after_secs: None,
                    },
                };
                send_down(socket, &error).await
            },
            Verdict::Policy(reason) => Step::Close(CLOSE_POLICY, reason),
            Verdict::AccountMismatch => {
                Step::Close(CLOSE_ACCOUNT_MISMATCH, ACCOUNT_MISMATCH)
            },
        },
    }
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    tokio::time::timeout(SEND_TIMEOUT, socket.send(message))
        .await
        .is_ok_and(|sent| sent.is_ok())
}

fn on_close(sender_alive: bool, mailbox: &Mailbox) -> Step {
    if sender_alive {
        (*mailbox.close.borrow())
            .map_or(Step::Continue, |code| Step::Close(code, close_reason(code)))
    } else {
        Step::Stop
    }
}

async fn send_down(socket: &mut WebSocket, down: &Down) -> Step {
    match serde_json::to_string(down) {
        Ok(text) => {
            if send(socket, Message::text(text)).await {
                Step::Continue
            } else {
                Step::Stop
            }
        },
        Err(e) => {
            warn!(error = %e, "deadlock: websocket frame failed to serialize");
            Step::Continue
        },
    }
}

const fn close_reason(code: u16) -> &'static str {
    match code {
        CLOSE_UNAUTHORIZED => "token revoked",
        CLOSE_ACCOUNT_MISMATCH => ACCOUNT_MISMATCH,
        _ => "closed by server",
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame { code, reason: reason.into() };
    if !send(socket, Message::Close(Some(frame))).await {
        return;
    }
    let drain = async {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(CLOSE_DRAIN, drain).await;
}
