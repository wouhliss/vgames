//! Bounded rumble effects. Deadlines exist only while an effect is scheduled or
//! playing; erasing/stopping one effect preserves the other active effects.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::Rumble;

struct Playing {
    effect: Rumble,
    start: Instant,
    end: Instant,
}
#[derive(Default)]
pub(super) struct Feedback {
    effects: BTreeMap<i16, Rumble>,
    playing: BTreeMap<i16, Playing>,
}
impl Feedback {
    pub(super) fn upload(&mut self, requested: i16, effect: Rumble) -> Option<i16> {
        let id = if (0..16).contains(&requested) {
            requested
        } else {
            (0..16).find(|id| !self.effects.contains_key(id))?
        };
        self.effects.insert(id, effect);
        Some(id)
    }
    pub(super) fn erase(&mut self, id: i16) {
        self.effects.remove(&id);
        self.playing.remove(&id);
    }
    pub(super) fn play(&mut self, id: i16, repetitions: i32, now: Instant) {
        self.playing.remove(&id);
        if repetitions <= 0 {
            return;
        }
        if let Some(effect) = self.effects.get(&id).copied() {
            let length = effect
                .duration_ms
                .saturating_mul(u32::try_from(repetitions).unwrap_or(0));
            if length == 0 {
                return;
            }
            let start = now + Duration::from_millis(u64::from(effect.delay_ms));
            self.playing.insert(
                id,
                Playing {
                    effect,
                    start,
                    end: start + Duration::from_millis(u64::from(length)),
                },
            );
        }
    }
    pub(super) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.playing
            .values()
            .map(|effect| {
                if effect.start > now {
                    effect.start
                } else {
                    effect.end
                }
            })
            .min()
    }
    pub(super) fn mix(&mut self, now: Instant) -> Rumble {
        self.playing.retain(|_, effect| effect.end > now);
        let mut result = Rumble::default();
        for effect in self.playing.values().filter(|effect| effect.start <= now) {
            result.large = result.large.max(effect.effect.large);
            result.small = result.small.max(effect.effect.small);
        }
        if (result.large != 0 || result.small != 0)
            && let Some(next) = self.next_deadline(now)
        {
            let remaining = next.saturating_duration_since(now);
            result.duration_ms =
                u32::try_from(remaining.as_millis().saturating_add(1)).unwrap_or(u32::MAX);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_effects_mix_and_erasing_one_preserves_the_other() {
        let now = Instant::now();
        let mut feedback = Feedback::default();
        let first = feedback
            .upload(
                -1,
                Rumble {
                    large: 1000,
                    small: 2000,
                    duration_ms: 100,
                    delay_ms: 0,
                },
            )
            .unwrap();
        let second = feedback
            .upload(
                -1,
                Rumble {
                    large: 3000,
                    small: 1000,
                    duration_ms: 200,
                    delay_ms: 50,
                },
            )
            .unwrap();
        feedback.play(first, 1, now);
        feedback.play(second, 1, now);
        let initial = feedback.mix(now);
        assert_eq!((initial.large, initial.small), (1000, 2000));
        assert_eq!(
            feedback.next_deadline(now),
            Some(now + Duration::from_millis(50))
        );
        let both = feedback.mix(now + Duration::from_millis(60));
        assert_eq!((both.large, both.small), (3000, 2000));
        feedback.erase(first);
        let remaining = feedback.mix(now + Duration::from_millis(60));
        assert_eq!((remaining.large, remaining.small), (3000, 1000));
        assert_eq!(
            feedback.mix(now + Duration::from_millis(250)),
            Rumble::default()
        );
        assert_eq!(
            feedback.next_deadline(now + Duration::from_millis(250)),
            None
        );
    }
    #[test]
    fn uploads_are_bounded_and_stopping_the_last_effect_removes_its_deadline() {
        let now = Instant::now();
        let mut feedback = Feedback::default();
        for _ in 0..16 {
            assert!(
                feedback
                    .upload(
                        -1,
                        Rumble {
                            large: 1,
                            duration_ms: 10,
                            ..Rumble::default()
                        }
                    )
                    .is_some()
            );
        }
        assert_eq!(feedback.upload(-1, Rumble::default()), None);
        feedback.play(0, 2, now);
        assert_eq!(
            feedback.next_deadline(now),
            Some(now + Duration::from_millis(20))
        );
        feedback.play(0, 0, now);
        assert_eq!(feedback.mix(now), Rumble::default());
        assert_eq!(feedback.next_deadline(now), None);
        feedback.erase(0);
        assert_eq!(feedback.upload(-1, Rumble::default()), Some(0));
    }
}
