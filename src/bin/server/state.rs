//! Shared desired gimbal state, and the UI command messages that change it.

use std::sync::{Arc, Mutex};

use serde::Deserialize;

use dji_orin_interface::gimbal::{Camera, GimbalMove};
use dji_orin_interface::logln;

/// Desired gimbal state, one slot per camera, updated by the WS clients and
/// streamed to ep2 by the injector thread.
#[derive(Default)]
pub struct Commands {
    pub cam1: GimbalMove,
    pub cam2: GimbalMove,
    /// When linked, the aircraft's native camera-lock is engaged: we drive only
    /// camera 1 (the master) and the drone mirrors camera 2 (see [`apply_move`]).
    ///
    /// [`apply_move`]: Commands::apply_move
    pub linked: bool,
    /// A pending lock change for the injector to emit once: `Some(true)` engages
    /// the lock, `Some(false)` releases it.
    pub lock_pending: Option<bool>,
}

pub type Shared = Arc<Mutex<Commands>>;

impl Commands {
    fn set(&mut self, cam: Camera, mv: GimbalMove) {
        match cam {
            Camera::One => self.cam1 = mv,
            Camera::Two => self.cam2 = mv,
        }
    }

    /// Apply a move from the UI. When linked, everything drives camera 1 (the
    /// master); the aircraft moves camera 2 to follow, so we never send camera 2
    /// directly. Otherwise only the named camera moves.
    pub fn apply_move(&mut self, cam: Camera, mv: GimbalMove) {
        if self.linked {
            self.cam1 = mv;
        } else {
            self.set(cam, mv);
        }
    }

    pub fn release_all(&mut self) {
        self.cam1.active = false;
        self.cam2.active = false;
    }

    /// Enter/leave linked mode: queue the native lock command and, when linking,
    /// stop driving camera 2 directly (the drone takes it over).
    pub fn set_linked(&mut self, on: bool) {
        self.linked = on;
        self.lock_pending = Some(on);
        if on {
            self.cam2.active = false;
        }
    }
}

/// A command message from the UI (mirrors `Command` in web/src/api.ts).
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum WsCommand {
    /// Set one camera's gimbal move (pitch/yaw, active while held).
    Gimbal {
        camera: u8,
        #[serde(default)]
        pitch: i16,
        #[serde(default)]
        yaw: i16,
        #[serde(default)]
        active: bool,
    },
    /// Engage/release the aircraft's native camera lock (camera 2 follows 1).
    Link { on: bool },
    /// Release every gimbal immediately.
    Stop,
}

/// Parse one UI message and apply it to the shared state.
pub fn handle(text: &str, cmds: &Shared) {
    match serde_json::from_str::<WsCommand>(text) {
        Ok(WsCommand::Gimbal {
            camera,
            pitch,
            yaw,
            active,
        }) => {
            if let Some(cam) = Camera::from_number(camera) {
                cmds.lock()
                    .unwrap()
                    .apply_move(cam, GimbalMove { pitch, yaw, active });
            }
        }
        Ok(WsCommand::Link { on }) => {
            cmds.lock().unwrap().set_linked(on);
            logln!("link: {}", if on { "on" } else { "off" });
        }
        Ok(WsCommand::Stop) => cmds.lock().unwrap().release_all(),
        Err(e) => logln!("ws: bad command: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_drives_only_the_master_and_queues_the_lock() {
        let mut c = Commands::default();
        c.set_linked(true);
        assert!(c.linked);
        assert_eq!(c.lock_pending, Some(true)); // injector will engage the lock
        // A move while linked drives camera 1 only; the drone follows camera 2.
        c.apply_move(
            Camera::Two,
            GimbalMove {
                pitch: 30,
                yaw: -5,
                active: true,
            },
        );
        assert!(c.cam1.active);
        assert!(!c.cam2.active);
        assert_eq!((c.cam1.pitch, c.cam1.yaw), (30, -5));
    }

    #[test]
    fn unlinking_queues_the_release() {
        let mut c = Commands::default();
        c.set_linked(true);
        c.set_linked(false);
        assert!(!c.linked);
        assert_eq!(c.lock_pending, Some(false));
    }

    #[test]
    fn unlinked_move_drives_only_the_named_camera() {
        let mut c = Commands::default();
        c.apply_move(
            Camera::Two,
            GimbalMove {
                pitch: 10,
                yaw: 0,
                active: true,
            },
        );
        assert!(!c.cam1.active && c.cam2.active);
    }

    #[test]
    fn handle_parses_ui_messages() {
        let cmds: Shared = Arc::default();
        handle(
            r#"{"cmd":"gimbal","camera":2,"pitch":5,"yaw":-3,"active":true}"#,
            &cmds,
        );
        assert_eq!(
            cmds.lock().unwrap().cam2,
            GimbalMove {
                pitch: 5,
                yaw: -3,
                active: true
            }
        );
        handle(r#"{"cmd":"stop"}"#, &cmds);
        assert!(!cmds.lock().unwrap().cam2.active);
        handle(r#"{"cmd":"link","on":true}"#, &cmds);
        assert!(cmds.lock().unwrap().linked);
        handle(r#"{"cmd":"gimbal","camera":9,"active":true}"#, &cmds); // ignored
        handle("not json", &cmds); // logged, ignored
    }
}
