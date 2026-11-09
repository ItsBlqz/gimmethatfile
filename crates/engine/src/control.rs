//! How many connections a download should use.
//!
//! More connections help when the server (or the path to it) limits each
//! connection. They hurt when one connection already fills the line: the
//! extra flows only add congestion. So instead of always opening the maximum,
//! the [`Controller`] *measures* what changes do to the total speed and keeps
//! a change only if it paid off (a noise-tolerant hill climb):
//!
//! 1. **Climb.** Start with `min(4, max)` connections and add one every
//!    300 ms (the slow start the engine always had). Every second, compare
//!    the last second with the one before: the connections added in between
//!    must each bring at least 40% of what an average connection brought
//!    before. If they don't, the line is full: go back to the count of the
//!    earlier second and stop climbing.
//! 2. **Descend.** After a climb stopped early, try halving the count while
//!    that costs less than 7% of the speed. Fewer connections at the same
//!    speed are kinder to the server and the network, and avoid congestion.
//!    A climb that reached the maximum is checked with a step down 2 s
//!    later: in the first seconds TCP itself is still ramping up, which
//!    makes every added connection look useful. A download whose maximum is
//!    4 or fewer has nothing to climb and gets the same check. Whenever a
//!    step down makes the download *faster*, the next step halves.
//! 3. **Re-probe.** Every 20 s, try a small step (a quarter of the count, at
//!    least one), alternating up and down, because conditions change. A step
//!    up is kept if it gains at least `max(10%, 40% of the proportional
//!    gain)`, a step down if it loses less than 7%; a kept step is followed
//!    by another one in the same direction. Each rejected probe doubles the
//!    wait before the next one in that direction (up to 160 s).
//!
//! A probe ("trial") waits until its connections are actually open, lets
//! them settle for 300 ms, then measures for a second (an obvious gain is
//! accepted after half a second).
//!
//! Two signals override this:
//! - **Server push-back** (`429`/`503`/...): the count drops to what the
//!   server tolerated, and there's no probing up for 15 s; after that, only
//!   one connection at a time.
//! - **Speed limit**: when connections spend a noticeable share of their time
//!   waiting for our own limiter, the limit is the bottleneck, not the
//!   network. Probing up would measure nothing, so the controller only probes
//!   *down* (first by halving, every 5 s, backing off the same way) and keeps
//!   the smallest count that still reaches the limit. When the limit stops
//!   binding (raised or removed), it climbs again.
//!
//! The controller is pure (time and measurements are passed in), so it can
//! be unit tested deterministically; `job::download_ranged` feeds it a
//! [`Sample`] every 150 ms.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

macro_rules! debug {
    ($($t:tt)*) => {
        if std::env::var_os("GTF_DEBUG").is_some() {
            eprintln!("[gtf] {}", format!($($t)*));
        }
    };
}

/// Connections opened at the start.
pub const START: u32 = 4;
/// While climbing, one more connection this often.
pub const RAMP_EVERY: Duration = Duration::from_millis(300);
/// Pause after a change before measuring (new connections need to ramp up).
pub const SETTLE: Duration = Duration::from_millis(300);
/// Length of one measurement.
pub const WINDOW: Duration = Duration::from_millis(1000);
/// Give up on a trial if its connections can't be opened in this time
/// (e.g. near the end, when there is no work left to split).
const OPEN_TIMEOUT: Duration = Duration::from_secs(3);
/// First probe down when starting at the maximum or right after climbing
/// to it.
pub const FIRST_PROBE: Duration = Duration::from_secs(2);
/// Re-probe interval in steady state...
pub const REPROBE: Duration = Duration::from_secs(20);
/// ...and while our own speed limit is the bottleneck.
pub const REPROBE_LIMITED: Duration = Duration::from_secs(5);
/// Each rejected probe doubles the interval, at most this many times.
const MAX_BACKOFF: u32 = 3;
/// Quiet time after server push-back before probing up again.
pub const RAISE_AFTER: Duration = Duration::from_secs(15);
/// A step up must gain at least this much...
const MIN_GAIN: f64 = 0.15;
/// ...and at least this share of the proportional gain (4 -> 8 = +100%).
const GAIN_SHARE: f64 = 0.4;
/// A step down that gains more than this means congestion: halve next.
const FASTER: f64 = 0.10;
/// A step down is kept if it loses at most this much.
const DOWN_TOLERANCE: f64 = 0.10;
/// Wait after opening extra connections before measuring a trial.
const TRIAL_SETTLE_UP: Duration = Duration::from_millis(500);
/// Wait after closing connections before measuring: the remaining ones need
/// time to grow into the freed bandwidth.
const TRIAL_SETTLE_DOWN: Duration = Duration::from_millis(1000);
/// Measurement window for trials (longer than `WINDOW` for more evidence).
const TRIAL_WINDOW: Duration = Duration::from_millis(1500);
/// Don't probe when the download will finish within about this long.
const ENDGAME: Duration = Duration::from_secs(10);
/// Share of connection time spent waiting for the limiter above which the
/// limiter counts as the bottleneck.
const LIMITED_SHARE: f64 = 0.2;

/// One observation of the download, taken by the supervisor every tick.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub at: Instant,
    /// Total bytes received so far.
    pub bytes: u64,
    /// Connections currently open.
    pub workers: u32,
    /// Total time connections have spent waiting for the speed limiter.
    pub limiter_wait: Duration,
    /// Bytes still to download.
    pub remaining: u64,
}

/// Speed over a span of samples.
#[derive(Clone, Copy, Debug)]
struct Measure {
    /// Bytes per second.
    rate: f64,
    /// Time-weighted average of open connections.
    workers: f64,
    /// The speed limiter was the bottleneck.
    limited: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    Up = 0,
    Down = 1,
}

impl Dir {
    fn flip(self) -> Self {
        match self {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    /// Slow start; `level` grows every `RAMP_EVERY`. `reference` is the
    /// previous one-second window, `window` the start of the current one.
    Climb {
        last_raise: Instant,
        window: Option<Instant>,
        reference: Option<Measure>,
    },
    /// Running `level` connections; `since` = when they were all open.
    Steady { since: Option<Instant> },
    /// Trying `to` connections, compared against `base_rate` at `level`.
    Trial {
        dir: Dir,
        to: u32,
        base_rate: f64,
        started: Instant,
        open_at: Option<Instant>,
    },
}

pub struct Controller {
    max: u32,
    /// The connection count we have settled on (or are climbing through).
    level: u32,
    phase: Phase,
    /// Halve on the way down (right after a climb, or when a limit binds).
    descending: bool,
    next_probe: Instant,
    next_dir: Dir,
    /// Rejected probes in a row, per direction.
    backoff: [u32; 2],
    last_throttle: Option<Instant>,
    was_limited: bool,
    samples: VecDeque<Sample>,
}

impl Controller {
    pub fn new(max: u32, now: Instant) -> Self {
        let max = max.max(1);
        let level = max.min(START);
        Self {
            max,
            level,
            phase: if level < max {
                Phase::Climb {
                    last_raise: now,
                    window: None,
                    reference: None,
                }
            } else {
                Phase::Steady { since: None }
            },
            descending: false,
            // Without a climb nothing is known yet, so check early whether
            // fewer connections would do as well.
            next_probe: now + if level < max { REPROBE } else { FIRST_PROBE },
            next_dir: Dir::Down,
            backoff: [0; 2],
            last_throttle: None,
            was_limited: false,
            samples: VecDeque::new(),
        }
    }

    /// How many connections should be open right now.
    pub fn target(&self) -> u32 {
        match self.phase {
            Phase::Trial { to, .. } => to,
            _ => self.level,
        }
    }

    /// The server pushed back; `tolerated` is how many connections it seemed
    /// to accept. Abandons any climb or trial and holds off probing up.
    pub fn on_throttle(&mut self, now: Instant, tolerated: u32) {
        self.level = tolerated.clamp(1, self.max).min(self.target());
        self.phase = Phase::Steady { since: None };
        self.descending = false;
        self.last_throttle = Some(now);
        self.next_probe = now + RAISE_AFTER;
        self.next_dir = Dir::Up;
        self.backoff = [0; 2];
        debug!("server pushed back: {} connections", self.level);
    }

    /// Speed from the first sample at or after `from` to the latest one.
    fn measure(&self, from: Instant) -> Option<Measure> {
        let last = self.samples.back()?;
        let start = self.samples.iter().position(|s| s.at >= from)?;
        let first = self.samples[start];
        let dt = last.at.duration_since(first.at).as_secs_f64();
        if dt < 0.2 {
            return None;
        }
        let rate = last.bytes.saturating_sub(first.bytes) as f64 / dt;
        // Each sample's connection count holds until the next sample.
        let conn_secs: f64 = self
            .samples
            .range(start..)
            .zip(self.samples.range(start + 1..))
            .map(|(a, b)| f64::from(b.workers) * b.at.duration_since(a.at).as_secs_f64())
            .sum();
        let workers = conn_secs / dt;
        let waited = last.limiter_wait.saturating_sub(first.limiter_wait);
        let limited = waited.as_secs_f64() / conn_secs.max(1e-3) > LIMITED_SHARE;
        Some(Measure {
            rate,
            workers,
            limited,
        })
    }

    fn up_allowed(&self, now: Instant) -> bool {
        self.level < self.max
            && self
                .last_throttle
                .is_none_or(|t| now.duration_since(t) >= RAISE_AFTER)
    }

    fn start_trial(&mut self, now: Instant, dir: Dir, base_rate: f64) {
        let quarter = (self.level / 4).max(1);
        let to = match dir {
            Dir::Up if self.last_throttle.is_some() => self.level + 1,
            Dir::Up => self.level + quarter,
            Dir::Down if self.descending => self.level / 2,
            Dir::Down => self.level.saturating_sub(quarter),
        }
        .clamp(1, self.max);
        if to == self.level {
            self.settle(now, false);
            return;
        }
        debug!(
            "probing {} -> {to} connections (at {:.2} MB/s)",
            self.level,
            base_rate / 1e6
        );
        self.phase = Phase::Trial {
            dir,
            to,
            base_rate,
            started: now,
            open_at: None,
        };
    }

    /// Back to steady state at `level`; schedule the next probe.
    fn settle(&mut self, now: Instant, limited: bool) {
        self.descending = false;
        self.phase = Phase::Steady { since: None };
        // The direction the next probe will (most likely) take.
        let dir = if limited || self.level >= self.max {
            Dir::Down
        } else {
            self.next_dir
        };
        let base = if limited { REPROBE_LIMITED } else { REPROBE };
        self.next_probe = now + base * (1 << self.backoff[dir as usize].min(MAX_BACKOFF));
    }

    fn climb(&mut self) {
        if self.level < self.max {
            debug!("climbing from {} connections", self.level);
            self.phase = Phase::Climb {
                last_raise: self.samples.back().map_or_else(Instant::now, |s| s.at),
                window: None,
                reference: None,
            };
        }
    }

    /// Feed one observation; returns the new target.
    pub fn tick(&mut self, s: Sample) -> u32 {
        let now = s.at;
        self.samples.push_back(s);
        while self
            .samples
            .front()
            .is_some_and(|f| now.duration_since(f.at) > Duration::from_secs(10))
        {
            self.samples.pop_front();
        }

        match self.phase {
            Phase::Climb {
                mut last_raise,
                window,
                reference,
            } => {
                if self.level < self.max && now.duration_since(last_raise) >= RAMP_EVERY {
                    self.level += 1;
                    last_raise = now;
                }
                let Some(window) = window else {
                    let window = (s.workers > 0).then_some(now);
                    self.phase = Phase::Climb {
                        last_raise,
                        window,
                        reference,
                    };
                    return self.target();
                };
                self.phase = Phase::Climb {
                    last_raise,
                    window: Some(window),
                    reference,
                };
                if now.duration_since(window) < WINDOW {
                    return self.target();
                }
                let Some(m) = self.measure(window) else {
                    return self.target();
                };
                if m.limited {
                    debug!("speed limit reached at {:.1} connections", m.workers);
                    self.level = (m.workers.round() as u32).clamp(1, self.max);
                    self.was_limited = true;
                    self.descending = true;
                    self.phase = Phase::Steady { since: None };
                    self.next_probe = now;
                    return self.target();
                }
                let next = Phase::Climb {
                    last_raise,
                    window: Some(now),
                    reference: Some(m),
                };
                let Some(r) = reference else {
                    self.phase = next;
                    return self.target();
                };
                let added = m.workers - r.workers;
                if added < 0.5 {
                    // Not more connections than a second ago: at the
                    // maximum, or no work left to hand out.
                    if self.level >= self.max && m.workers >= f64::from(self.max) - 0.5 {
                        debug!("climbed to {} connections", self.level);
                        self.settle(now, false);
                        // The climb reached the maximum so fast that TCP's own
                        // ramp-up may have hidden a full line: check soon
                        // whether fewer connections do as well.
                        self.next_probe = now + FIRST_PROBE;
                        self.next_dir = Dir::Down;
                    } else {
                        self.phase = next;
                    }
                    return self.target();
                }
                let marginal = (m.rate - r.rate) / added;
                let average = r.rate / r.workers.max(0.5);
                if marginal >= GAIN_SHARE * average {
                    self.phase = next;
                    return self.target();
                }
                // The last connections didn't pay for themselves.
                self.level = (r.workers.round() as u32).clamp(1, self.max);
                debug!(
                    "{:.1} -> {:.1} connections: {:.2} -> {:.2} MB/s; back to {}",
                    r.workers,
                    m.workers,
                    r.rate / 1e6,
                    m.rate / 1e6,
                    self.level
                );
                if self.level > 1 {
                    self.descending = true;
                    self.start_trial(now, Dir::Down, r.rate.max(m.rate));
                } else {
                    self.settle(now, false);
                }
            }
            Phase::Steady { since: None } => {
                if s.workers >= self.level {
                    self.phase = Phase::Steady { since: Some(now) };
                }
            }
            Phase::Steady { since: Some(since) } => {
                if now < since + SETTLE + WINDOW {
                    return self.target();
                }
                let from = now.checked_sub(WINDOW).unwrap_or(since);
                let Some(m) = self.measure(from) else {
                    return self.target();
                };
                if self.was_limited && !m.limited {
                    // The limit was raised or removed: look for speed again.
                    debug!("speed limit no longer binding");
                    self.was_limited = false;
                    self.backoff = [0; 2];
                    self.climb();
                    return self.target();
                }
                if m.limited && !self.was_limited {
                    self.was_limited = true;
                    self.descending = true;
                    self.backoff[Dir::Down as usize] = 0;
                    self.next_probe = self.next_probe.min(now);
                }
                if now < self.next_probe {
                    return self.target();
                }
                // Nearly done: a probe couldn't finish, and would only cost.
                if (s.remaining as f64) < m.rate * ENDGAME.as_secs_f64() {
                    return self.target();
                }
                let dir = if m.limited {
                    Dir::Down
                } else if self.next_dir == Dir::Up && self.up_allowed(now) {
                    Dir::Up
                } else {
                    Dir::Down
                };
                self.next_dir = dir.flip();
                if dir == Dir::Down && self.level == 1 {
                    self.settle(now, m.limited);
                } else {
                    self.start_trial(now, dir, m.rate);
                }
            }
            Phase::Trial {
                dir,
                to,
                base_rate,
                started,
                open_at,
            } => {
                let Some(open_at) = open_at else {
                    let open = match dir {
                        Dir::Up => s.workers >= to,
                        Dir::Down => s.workers <= to,
                    };
                    if open {
                        self.phase = Phase::Trial {
                            dir,
                            to,
                            base_rate,
                            started,
                            open_at: Some(now),
                        };
                    } else if now.duration_since(started) > OPEN_TIMEOUT {
                        debug!(
                            "could not reach {to} connections; staying at {}",
                            self.level
                        );
                        self.settle(now, false);
                    }
                    return self.target();
                };
                let from = open_at
                    + match dir {
                        Dir::Up => TRIAL_SETTLE_UP,
                        Dir::Down => TRIAL_SETTLE_DOWN,
                    };
                if now < from + TRIAL_WINDOW / 2 {
                    return self.target();
                }
                let Some(m) = self.measure(from) else {
                    return self.target();
                };
                let gain = m.rate / base_rate.max(1.0) - 1.0;
                let done = now >= from + TRIAL_WINDOW;
                let keep = match dir {
                    Dir::Up => {
                        let g = f64::from(to - self.level) / f64::from(self.level);
                        let need = MIN_GAIN.max(GAIN_SHARE * g);
                        // No early accept: an extra connection must prove
                        // itself over the whole window.
                        if m.limited {
                            Some(false)
                        } else {
                            done.then_some(gain >= need)
                        }
                    }
                    // Still held back by our own limit: fewer connections suffice.
                    Dir::Down if m.limited => done.then_some(true),
                    // An obvious loss: end the trial early.
                    Dir::Down if gain < -3.0 * DOWN_TOLERANCE => Some(false),
                    Dir::Down => done.then_some(gain >= -DOWN_TOLERANCE),
                };
                let Some(keep) = keep else {
                    return self.target();
                };
                debug!(
                    "{to} connections: {:+.0}% ({:.2} MB/s), {}",
                    gain * 100.0,
                    m.rate / 1e6,
                    if keep { "keeping" } else { "reverting" }
                );
                if keep {
                    self.level = to;
                    self.backoff[dir as usize] = 0;
                    if dir == Dir::Down && gain > FASTER {
                        // Fewer connections were faster: congestion. Halve.
                        self.descending = true;
                    }
                    // It paid off: keep going the same way.
                    let more = match dir {
                        // After server push-back, one step per re-probe only.
                        Dir::Up => {
                            self.up_allowed(now) && !m.limited && self.last_throttle.is_none()
                        }
                        Dir::Down => self.level > 1,
                    };
                    if more {
                        self.start_trial(now, dir, m.rate);
                    } else {
                        self.settle(now, m.limited);
                    }
                } else {
                    self.backoff[dir as usize] += 1;
                    self.settle(now, m.limited || self.was_limited);
                }
            }
        }
        self.target()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated download: throughput as a function of open connections.
    struct Sim {
        c: Controller,
        now: Instant,
        bytes: f64,
        wait: Duration,
        workers: u32,
        remaining: u64,
    }

    const TICK: Duration = Duration::from_millis(150);

    impl Sim {
        fn new(max: u32) -> Self {
            let now = Instant::now();
            Self {
                c: Controller::new(max, now),
                now,
                bytes: 0.0,
                wait: Duration::ZERO,
                workers: 0,
                remaining: u64::MAX,
            }
        }

        /// Run for `secs`. `speed(n)` gives bytes/s for `n` connections and
        /// `limit` caps the total like our speed limiter would. Returns the
        /// average speed.
        fn run(&mut self, secs: f64, speed: impl Fn(u32) -> f64, limit: Option<f64>) -> f64 {
            let start = (self.now, self.bytes);
            let end = self.now + Duration::from_secs_f64(secs);
            while self.now < end {
                self.now += TICK;
                // Like the supervisor: one new connection per tick at most;
                // surplus connections close within a tick.
                let target = self.c.target();
                if self.workers < target {
                    self.workers += 1;
                } else {
                    self.workers = target;
                }
                let raw = speed(self.workers);
                let got = limit.map_or(raw, |l| raw.min(l));
                if raw > got {
                    let share = 1.0 - got / raw;
                    self.wait += TICK.mul_f64(share * f64::from(self.workers));
                }
                self.bytes += got * TICK.as_secs_f64();
                self.c.tick(Sample {
                    at: self.now,
                    bytes: self.bytes as u64,
                    workers: self.workers,
                    limiter_wait: self.wait,
                    remaining: self.remaining,
                });
            }
            (self.bytes - start.1) / self.now.duration_since(start.0).as_secs_f64()
        }
    }

    const MB: f64 = 1e6;
    /// Each connection capped at 2 MB/s (most file hosts).
    fn per_conn(n: u32) -> f64 {
        2.0 * MB * f64::from(n)
    }
    /// One connection fills a 25 MB/s line; more add congestion.
    fn saturated(n: u32) -> f64 {
        25.0 * MB / (1.0 + 0.1 * f64::from(n.max(1) - 1))
    }

    #[test]
    fn no_probes_in_the_endgame() {
        let mut s = Sim::new(2);
        s.run(5.0, per_conn, None);
        assert_eq!(s.c.target(), 2);
        // 8 MB left at ~4 MB/s: well inside the last 10 s.
        s.remaining = 8_000_000;
        for _ in 0..100 {
            s.run(0.15, per_conn, None);
            assert_eq!(s.c.target(), 2);
        }
    }

    #[test]
    fn climbs_like_slow_start_when_each_connection_is_capped() {
        let mut s = Sim::new(16);
        assert_eq!(s.c.target(), 4);
        // 12 more connections at one per 300 ms.
        s.run(3.7, per_conn, None);
        assert_eq!(s.c.target(), 16);
        // And stays (nearly) there through re-probes.
        let avg = s.run(300.0, per_conn, None);
        assert!(s.c.target() >= 15, "{}", s.c.target());
        assert!(avg > 0.97 * per_conn(16), "{}", avg / MB);
    }

    #[test]
    fn falls_back_to_one_when_the_line_is_full() {
        let mut s = Sim::new(8);
        s.run(12.0, saturated, None);
        assert_eq!(s.c.target(), 1);
        // Re-probing up doesn't stick, and doesn't cost much.
        let avg = s.run(300.0, saturated, None);
        assert_eq!(s.c.target(), 1);
        assert!(avg > 0.97 * saturated(1), "{}", avg / MB);
    }

    #[test]
    fn notices_a_full_line_hidden_by_tcp_ramp_up() {
        // Like a real line: during the first 2.5 s everything is still
        // ramping up, so the climb sees gains and reaches the maximum. The
        // check right after the climb must still find out.
        let mut s = Sim::new(8);
        for i in 0..17 {
            let ramp = (f64::from(i) + 1.0) / 17.0;
            s.run(0.15, |n| saturated(n) * ramp, None);
        }
        s.run(20.0, saturated, None);
        assert!(s.c.target() <= 2, "{}", s.c.target());
    }

    #[test]
    fn finds_a_middle_ground() {
        // Each connection does 5 MB/s but the line tops out at 12 MB/s.
        let mut s = Sim::new(16);
        let f = |n: u32| (5.0 * MB * f64::from(n)).min(12.0 * MB);
        s.run(60.0, f, None);
        let t = s.c.target();
        assert!((3..=4).contains(&t), "{t}");
    }

    #[test]
    fn stays_put_at_the_maximum() {
        // max <= START: no climb; rare down probes only.
        let mut s = Sim::new(2);
        assert_eq!(s.c.target(), 2);
        let avg = s.run(300.0, per_conn, None);
        assert_eq!(s.c.target(), 2);
        assert!(avg > 0.98 * per_conn(2), "{}", avg / MB);
    }

    #[test]
    fn uses_few_connections_under_a_speed_limit() {
        // Server caps each connection at 2 MB/s, our limit is 3 MB/s:
        // 2 connections are enough.
        let mut s = Sim::new(16);
        s.run(30.0, per_conn, Some(3.0 * MB));
        assert_eq!(s.c.target(), 2);
        // And the limit is still reached on average.
        let avg = s.run(120.0, per_conn, Some(3.0 * MB));
        assert!(avg > 0.95 * 3.0 * MB, "{}", avg / MB);
        // Removing the limit lets it climb again (and the check after the
        // climb confirms that all 16 help).
        s.run(15.0, per_conn, None);
        assert_eq!(s.c.target(), 16);
    }

    #[test]
    fn throttling_caps_and_recovers_slowly() {
        let mut s = Sim::new(12);
        s.run(1.0, per_conn, None);
        s.c.on_throttle(s.now, 3);
        assert_eq!(s.c.target(), 3);
        // No probing up for RAISE_AFTER.
        s.run(14.0, per_conn, None);
        assert_eq!(s.c.target(), 3);
        // Then one step at a time (per_conn rewards each one).
        s.run(2.5, per_conn, None);
        assert_eq!(s.c.target(), 4);
    }

    #[test]
    fn single_connection_stays_single() {
        let mut s = Sim::new(1);
        s.run(60.0, per_conn, None);
        assert_eq!(s.c.target(), 1);
    }
}
