//! Gimbal control: the desired move per camera, the native camera lock and
//! yaw recenters. [`links::rc`](crate::links::rc) streams this state to the
//! aircraft at a fixed rate; nothing here touches I/O.

use std::sync::{Mutex, MutexGuard};

pub use dji_protocol::gimbal::{Camera, GimbalMove};

/// Desired gimbal state, one slot per camera.
#[derive(Default)]
pub struct State {
    pub cam1: GimbalMove,
    pub cam2: GimbalMove,
    /// When linked, the aircraft's native camera-lock is engaged: we drive only
    /// camera 1 (the master) and the drone mirrors camera 2 (see [`apply_move`]).
    ///
    /// [`apply_move`]: State::apply_move
    pub linked: bool,
    /// A pending lock change for the writer to emit once: `Some(true)` engages
    /// the lock, `Some(false)` releases it.
    pub lock_pending: Option<bool>,
    /// Yaw recenters for the writer to emit once each, in order, no repeats.
    pub reset_pending: Vec<Camera>,
}

impl State {
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

    /// Queue a yaw recenter (the hardware-verified native reset) for `target`
    /// (`None` = both cameras), releasing any movement on the targets first.
    ///
    /// While linked, the aircraft mirrors camera 2 from camera 1, so every
    /// reset goes to the master only; that resets the pair (verified on the
    /// M210). Independent, each camera is reset on its own.
    pub fn request_reset(&mut self, target: Option<Camera>) {
        let cams: &[Camera] = match (self.linked, target) {
            (true, _) => &[Camera::One],
            (false, Some(Camera::One)) => &[Camera::One],
            (false, Some(Camera::Two)) => &[Camera::Two],
            (false, None) => &[Camera::One, Camera::Two],
        };
        for &cam in cams {
            self.set(cam, GimbalMove::default());
            if !self.reset_pending.contains(&cam) {
                self.reset_pending.push(cam);
            }
        }
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

    /// Put a recenter back at the end of the queue (its frame was not written).
    pub fn requeue_reset(&mut self, cam: Camera) {
        if !self.reset_pending.contains(&cam) {
            self.reset_pending.push(cam);
        }
    }
}

/// The gimbal feature: [`State`] behind a lock, with the operations a caller
/// uses. The RC writer reads the state through [`Gimbal::lock`].
#[derive(Default)]
pub struct Gimbal(Mutex<State>);

impl Gimbal {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// Move one camera (pitch/yaw, active while held). See [`State::apply_move`].
    pub fn apply_move(&self, cam: Camera, mv: GimbalMove) {
        self.lock().apply_move(cam, mv);
    }

    /// Release every gimbal immediately.
    pub fn release_all(&self) {
        self.lock().release_all();
    }

    /// Recenter yaw on one camera, or both when `target` is `None`.
    pub fn request_reset(&self, target: Option<Camera>) {
        self.lock().request_reset(target);
    }

    /// Engage/release the aircraft's native camera lock (camera 2 follows 1).
    pub fn set_linked(&self, on: bool) {
        self.lock().set_linked(on);
    }

    pub fn linked(&self) -> bool {
        self.lock().linked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_drives_only_the_master_and_queues_the_lock() {
        let mut c = State::default();
        c.set_linked(true);
        assert!(c.linked);
        assert_eq!(c.lock_pending, Some(true)); // writer will engage the lock
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
        let mut c = State::default();
        c.set_linked(true);
        c.set_linked(false);
        assert!(!c.linked);
        assert_eq!(c.lock_pending, Some(false));
    }

    #[test]
    fn unlinked_move_drives_only_the_named_camera() {
        let g = Gimbal::default();
        g.apply_move(
            Camera::Two,
            GimbalMove {
                pitch: 10,
                yaw: 0,
                active: true,
            },
        );
        let c = g.lock();
        assert!(!c.cam1.active && c.cam2.active);
    }

    #[test]
    fn independent_resets_target_the_named_cameras_and_release_them() {
        let mut c = State {
            cam2: GimbalMove {
                pitch: 5,
                yaw: 5,
                active: true,
            },
            ..Default::default()
        };
        c.request_reset(Some(Camera::Two));
        assert_eq!(c.reset_pending, [Camera::Two]);
        assert!(!c.cam2.active); // released before the reset goes out
        c.request_reset(None);
        assert_eq!(c.reset_pending, [Camera::Two, Camera::One]); // no repeats
        c.requeue_reset(Camera::Two);
        assert_eq!(c.reset_pending, [Camera::Two, Camera::One]);
    }

    #[test]
    fn linked_resets_go_to_the_master_only() {
        let mut c = State::default();
        c.set_linked(true);
        for target in [Some(Camera::Two), None, Some(Camera::One)] {
            c.request_reset(target);
        }
        assert_eq!(c.reset_pending, [Camera::One]);
    }
}
