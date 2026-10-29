//! The XT2's thermal core: the settings the caller wants sent (spot meter,
//! display mode, isotherm band, measurement), queued in order for the RC
//! writer, and the [`auto`] range that keeps the band fitted to the scene.
//! Every setting is a command to the aircraft.

pub mod auto;

use std::sync::{Mutex, MutexGuard};

pub use dji_protocol::thermal::{DisplayMode, Isotherm, Measure, Report};

/// One thermal camera setting (see [`dji_protocol::thermal`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThermalCmd {
    /// Move the spot meter to normalised `(x, y)` on the thermal image.
    Spot(f32, f32),
    /// Switch what the thermal camera's video shows.
    Mode(DisplayMode),
    /// Set one isotherm bound, °C.
    Isotherm(Isotherm, i16),
    /// Choose what the camera measures.
    Measure(Measure),
    /// Area-measurement rectangle: left, top, right, bottom (0..1).
    Area([f32; 4]),
}

/// Isotherm bounds in °C: lower, middle, upper.
pub type Band = (i16, i16, i16);
const MAX_PENDING: usize = 64;

/// The isotherm bounds a caller may set (Pilot's fire preset went to 751 °C;
/// the XT2's low-gain range ends around there).
pub const ISOTHERM_RANGE: std::ops::RangeInclusive<i16> = -40..=800;

/// Widen over every accepted target before changing the middle, then narrow.
/// Always send all steps: reports can lag previously sent or in-flight commands.
/// Include the reported extremes as well, in case Pilot set a wider band.
pub fn isotherm_sequence(cur: Band, target: Band) -> Vec<ThermalCmd> {
    vec![
        ThermalCmd::Isotherm(Isotherm::Lower, cur.0.min(*ISOTHERM_RANGE.start())),
        ThermalCmd::Isotherm(Isotherm::Upper, cur.2.max(*ISOTHERM_RANGE.end())),
        ThermalCmd::Isotherm(Isotherm::Middle, target.1),
        ThermalCmd::Isotherm(Isotherm::Lower, target.0),
        ThermalCmd::Isotherm(Isotherm::Upper, target.2),
    ]
}

/// The camera's current isotherm bounds from its report.
pub fn band_of(r: &Report) -> Band {
    (r.isotherm_lower, r.isotherm_middle, r.isotherm_upper)
}

/// Why a setting was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// A coordinate is not finite.
    BadPosition,
    /// The band is out of order or outside [`ISOTHERM_RANGE`].
    BadBand,
    /// The band depends on the camera's current one, and it has not reported.
    NoReport,
    /// The queue is full (the RC link is down or far behind).
    QueueFull,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Rejected::BadPosition => "bad position",
            Rejected::BadBand => "band out of order or range",
            Rejected::NoReport => "no thermal report yet",
            Rejected::QueueFull => "thermal queue full",
        })
    }
}

/// Settings waiting to be sent, in order, plus the auto-range switch.
#[derive(Default)]
pub struct Queue {
    pub pending: Vec<ThermalCmd>,
    /// Reserve queue capacity while the writer sends without holding the lock.
    in_flight: usize,
    /// Keep the isotherm band fitted to the scene ([`auto`]).
    pub auto: bool,
}

impl Queue {
    pub fn capacity(&self, additional: usize) -> bool {
        self.pending.len() + self.in_flight + additional <= MAX_PENDING
    }

    /// Everything pending, for the writer to send; the slots stay reserved
    /// until [`Queue::sent`] or [`Queue::retry`].
    pub fn take(&mut self) -> Vec<ThermalCmd> {
        let pending = std::mem::take(&mut self.pending);
        self.in_flight = pending.len();
        pending
    }

    pub fn sent(&mut self) {
        self.in_flight -= 1;
    }

    /// Put the unsent tail of a taken batch back, ahead of anything newer.
    pub fn retry(&mut self, unsent: &[ThermalCmd]) {
        let newer = std::mem::take(&mut self.pending);
        self.pending = unsent.iter().copied().chain(newer).collect();
        self.in_flight = 0;
    }
}

/// The thermal feature: the [`Queue`] behind a lock, with the operations a
/// caller uses. The RC writer and [`auto`] use [`Thermal::lock`].
#[derive(Default)]
pub struct Thermal(Mutex<Queue>);

impl Thermal {
    pub fn lock(&self) -> MutexGuard<'_, Queue> {
        self.0.lock().unwrap()
    }

    pub fn auto(&self) -> bool {
        self.lock().auto
    }

    /// Fit the isotherm band to the scene automatically, or stop doing so.
    pub fn set_auto(&self, on: bool) {
        self.lock().auto = on;
    }

    /// Move the spot meter (normalised 0..1 on the thermal image). `report` is
    /// the camera's latest report, if any. A spot ends auto range, whatever
    /// the camera measures right now: otherwise auto would switch it back to
    /// area measurement within a second and the spot would be lost.
    pub fn spot(&self, x: f32, y: f32, report: Option<&Report>) -> Result<(), Rejected> {
        if !(x.is_finite() && y.is_finite()) {
            return Err(Rejected::BadPosition);
        }
        let mut q = self.lock();
        if !q.capacity(2) {
            return Err(Rejected::QueueFull);
        }
        // Only the latest unsent spot (and its switch to spot measurement)
        // matters.
        q.pending
            .retain(|t| !matches!(t, ThermalCmd::Spot(..) | ThermalCmd::Measure(Measure::Spot)));
        q.auto = false;
        // The camera rejects a spot unless it is in spot measurement.
        if report.is_none_or(|r| r.measure != Measure::Spot as u8) {
            q.pending.push(ThermalCmd::Measure(Measure::Spot));
        }
        q.pending.push(ThermalCmd::Spot(x, y));
        Ok(())
    }

    /// Set the display mode; a mode still waiting is replaced.
    pub fn set_mode(&self, mode: DisplayMode) -> Result<(), Rejected> {
        let mut q = self.lock();
        if let Some(m) = q
            .pending
            .iter_mut()
            .find(|t| matches!(t, ThermalCmd::Mode(_)))
        {
            *m = ThermalCmd::Mode(mode);
        } else if q.capacity(1) {
            q.pending.push(ThermalCmd::Mode(mode));
        } else {
            return Err(Rejected::QueueFull);
        }
        Ok(())
    }

    /// Set the isotherm band (lower ≤ middle ≤ upper, °C), ordered so the
    /// camera never sees an inverted band on the way. Ends auto range.
    pub fn set_isotherm(&self, target: Band, report: Option<&Report>) -> Result<(), Rejected> {
        let (lower, middle, upper) = target;
        if !(lower <= middle && middle <= upper)
            || ![lower, upper].iter().all(|v| ISOTHERM_RANGE.contains(v))
        {
            return Err(Rejected::BadBand);
        }
        let r = report.ok_or(Rejected::NoReport)?;
        let mut q = self.lock();
        if !q.capacity(5) {
            return Err(Rejected::QueueFull);
        }
        q.auto = false;
        q.pending.retain(|t| !matches!(t, ThermalCmd::Isotherm(..)));
        q.pending.extend(isotherm_sequence(band_of(r), target));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(t: &Thermal) -> Vec<ThermalCmd> {
        t.lock().pending.clone()
    }

    fn spot_report(band: Band) -> Report {
        Report {
            measure: Measure::Spot as u8,
            isotherm_lower: band.0,
            isotherm_middle: band.1,
            isotherm_upper: band.2,
            ..Report::default()
        }
    }

    #[test]
    fn isotherm_sequence_keeps_the_bounds_ordered_at_every_step() {
        let cases = [
            ((27, 32, 38), (100, 150, 200)),  // all up
            ((100, 150, 200), (-10, 0, 5)),   // all down
            ((27, 32, 751), (119, 177, 695)), // Pilot's custom step
            ((0, 10, 20), (5, 10, 15)),       // narrow
            ((5, 10, 15), (5, 10, 15)),       // no change
        ];
        for (cur, target) in cases {
            let mut band = cur;
            for cmd in isotherm_sequence(cur, target) {
                let ThermalCmd::Isotherm(bound, v) = cmd else {
                    panic!("not an isotherm write: {cmd:?}")
                };
                match bound {
                    Isotherm::Lower => band.0 = v,
                    Isotherm::Middle => band.1 = v,
                    Isotherm::Upper => band.2 = v,
                }
                assert!(
                    band.0 <= band.1 && band.1 <= band.2,
                    "{cur:?}->{target:?}: {band:?}"
                );
            }
            assert_eq!(band, target);
        }
        assert_eq!(isotherm_sequence((5, 10, 15), (5, 10, 15)).len(), 5);
    }

    #[test]
    fn stale_report_and_partial_previous_band_do_not_break_ordering() {
        let report = (0, 10, 20);
        let first = isotherm_sequence(report, (100, 150, 200));
        for sent in 0..=first.len() {
            let mut actual = report;
            for cmd in first[..sent]
                .iter()
                .copied()
                .chain(isotherm_sequence(report, (5, 10, 15)))
            {
                match cmd {
                    ThermalCmd::Isotherm(Isotherm::Lower, v) => actual.0 = v,
                    ThermalCmd::Isotherm(Isotherm::Middle, v) => actual.1 = v,
                    ThermalCmd::Isotherm(Isotherm::Upper, v) => actual.2 = v,
                    _ => unreachable!(),
                }
                assert!(actual.0 <= actual.1 && actual.1 <= actual.2, "{actual:?}");
            }
            assert_eq!(actual, (5, 10, 15));
        }
    }

    #[test]
    fn queue_reserves_in_flight_capacity_and_retries_only_unsent() {
        let mut q = Queue::default();
        let mode = ThermalCmd::Mode(DisplayMode::Thermal);
        q.pending = vec![mode; MAX_PENDING];
        let batch = q.take();
        assert!(!q.capacity(1));
        q.sent();
        assert!(q.capacity(1));
        q.pending.push(ThermalCmd::Spot(0.5, 0.5));
        q.retry(&batch[1..]);
        assert_eq!(q.pending.len(), MAX_PENDING);
        assert_eq!(q.pending.last(), Some(&ThermalCmd::Spot(0.5, 0.5)));
        assert_eq!(q.in_flight, 0);
        assert!(!q.capacity(1));
    }

    #[test]
    fn queued_modes_coalesce_and_full_queue_rejects_without_mutation() {
        let t = Thermal::default();
        for _ in 0..100 {
            t.set_mode(DisplayMode::Thermal).unwrap();
            t.set_mode(DisplayMode::Visual).unwrap();
        }
        assert_eq!(pending(&t), [ThermalCmd::Mode(DisplayMode::Visual)]);
        t.lock().pending = vec![ThermalCmd::Measure(Measure::Area); MAX_PENDING];
        assert_eq!(t.set_mode(DisplayMode::Thermal), Err(Rejected::QueueFull));
        assert_eq!(t.spot(0.5, 0.5, None), Err(Rejected::QueueFull));
        assert_eq!(pending(&t).len(), MAX_PENDING);
    }

    #[test]
    fn isotherm_and_spot_commands_respect_the_camera_state() {
        let t = Thermal::default();
        t.set_auto(true);
        let r = spot_report((27, 32, 38));
        assert_eq!(
            t.set_isotherm((30, 20, 40), Some(&r)),
            Err(Rejected::BadBand)
        );
        assert_eq!(t.set_isotherm((20, 30, 40), None), Err(Rejected::NoReport));
        assert!(pending(&t).is_empty());
        assert!(t.auto());
        t.set_isotherm((20, 30, 40), Some(&r)).unwrap();
        assert_eq!(
            pending(&t),
            [
                ThermalCmd::Isotherm(Isotherm::Lower, -40),
                ThermalCmd::Isotherm(Isotherm::Upper, 800),
                ThermalCmd::Isotherm(Isotherm::Middle, 30),
                ThermalCmd::Isotherm(Isotherm::Lower, 20),
                ThermalCmd::Isotherm(Isotherm::Upper, 40),
            ]
        );
        assert!(!t.auto()); // a manual band ends auto

        // A spot while measuring an area switches to spot measurement first.
        let t = Thermal::default();
        let area = Report {
            measure: Measure::Area as u8,
            ..r
        };
        t.spot(0.5, 0.5, Some(&area)).unwrap();
        assert_eq!(
            pending(&t),
            [
                ThermalCmd::Measure(Measure::Spot),
                ThermalCmd::Spot(0.5, 0.5)
            ]
        );
        let t = Thermal::default();
        t.set_auto(true);
        t.spot(0.5, 0.5, Some(&r)).unwrap();
        assert_eq!(pending(&t), [ThermalCmd::Spot(0.5, 0.5)]);
        // Already in spot mode: the spot still ends auto range.
        assert!(!t.auto());
    }

    #[test]
    fn commands_queue_in_order_with_only_the_latest_spot() {
        let t = Thermal::default();
        t.spot(0.1, 0.2, None).unwrap();
        t.set_mode(DisplayMode::Thermal).unwrap();
        t.spot(0.5, 0.6, None).unwrap(); // replaces 0.1/0.2
        assert_eq!(t.spot(f32::NAN, 0.0, None), Err(Rejected::BadPosition));
        assert_eq!(
            pending(&t),
            [
                ThermalCmd::Mode(DisplayMode::Thermal),
                // No report yet, so the spot is preceded by spot measurement.
                ThermalCmd::Measure(Measure::Spot),
                ThermalCmd::Spot(0.5, 0.6)
            ]
        );
    }
}
