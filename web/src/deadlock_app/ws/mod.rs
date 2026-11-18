mod frames;
mod hub;
mod pusher;
mod session;
mod verify;

use deadlock_protocol::ws::MAX_FRAME_BYTES;
pub use frames::{FrameRate, Verdict, judge};
pub use hub::{
    MAX_SOCKETS_PER_TOKEN,
    Mailbox,
    Registration,
    SocketInfo,
    WsHub,
    WsTiming,
};
pub use pusher::{apply, revalidate_all, spawn_pusher};
pub use session::run;
pub use verify::check;

pub const UPGRADE_LIMIT_BYTES: usize = 2 * MAX_FRAME_BYTES;
