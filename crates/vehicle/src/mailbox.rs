//! ACKs from the link readers, handed to whoever is waiting for them.
//!
//! Each entry is keyed by `(key, seq)`: OpenProtocol ACKs use their session
//! (≤ 31) as the key, DUML ACKs `0x100 + cmd_id`. A request waits for its own
//! sequence number; late ACKs age out of the bounded queue.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const CAPACITY: usize = 32;

#[derive(Default)]
pub struct Mailbox {
    queue: Mutex<VecDeque<(u16, u16, Vec<u8>)>>,
    ready: Condvar,
}

impl Mailbox {
    pub fn push(&self, key: u16, seq: u16, data: &[u8]) {
        let mut q = self.queue.lock().unwrap();
        q.push_back((key, seq, data.to_vec()));
        while q.len() > CAPACITY {
            q.pop_front();
        }
        self.ready.notify_all();
    }

    /// The data of the ACK `(key, seq)`, waiting up to `timeout` for it.
    pub fn wait(&self, key: u16, seq: u16, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(i) = q.iter().position(|(k, n, _)| (*k, *n) == (key, seq)) {
                return q.remove(i).map(|(_, _, d)| d);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            q = self.ready.wait_timeout(q, left).unwrap().0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_by_key_and_sequence_and_ages_out() {
        let m = Mailbox::default();
        m.push(2, 7, &[1]);
        m.push(2, 8, &[2]);
        assert_eq!(m.wait(2, 9, Duration::ZERO), None);
        assert_eq!(m.wait(2, 8, Duration::ZERO), Some(vec![2]));
        assert_eq!(m.wait(2, 8, Duration::ZERO), None); // taken once
        for i in 0..CAPACITY as u16 {
            m.push(3, i, &[]);
        }
        assert_eq!(m.wait(2, 7, Duration::ZERO), None); // pushed out
    }
}
