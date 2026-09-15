use std::f32::consts::PI;

use crate::types::Vec3;

const MIN_PROGRESS_BS: f32 = 1.5;
const TARGET_CHANGE_BS: f32 = 20.0;
const STALL_SECONDS: f32 = 1.5;
const RECOVERY_SECONDS: f32 = 1.0;
const MAX_RECOVERY_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug)]
pub struct RecoveryDirective {
    pub yaw: f32,
    pub jump: bool,
    pub recovering: bool,
    pub attempts: u8,
    pub give_up: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AntiStuck {
    anchor: Option<Vec3>,
    best_target_distance: Option<f32>,
    target: Option<Vec3>,
    stalled_for: f32,
    recovery_remaining: f32,
    recovery_yaw: Option<f32>,
    recovery_attempts: u8,
}

impl AntiStuck {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn stalled_for(&self) -> f32 {
        self.stalled_for
    }

    pub fn recovery_attempts(&self) -> u8 {
        self.recovery_attempts
    }

    pub fn adjust(
        &mut self,
        position: Vec3,
        target: Option<Vec3>,
        moving: bool,
        desired_yaw: f32,
        jump: bool,
        dt: f32,
    ) -> RecoveryDirective {
        if !moving || !position.is_finite() || !dt.is_finite() || dt <= 0.0 {
            self.reset();
            return direct(desired_yaw, jump);
        }

        if target_changed(self.target, target) {
            self.reset();
            self.target = target;
        }
        self.target = target;

        let target_distance = target.map(|target| horizontal_distance(position, target));
        let anchor = *self.anchor.get_or_insert(position);
        if self.best_target_distance.is_none() {
            self.best_target_distance = target_distance;
        }

        if self.recovery_remaining > 0.0 {
            let recovery_yaw = self.recovery_yaw.unwrap_or(desired_yaw);
            self.recovery_remaining = (self.recovery_remaining - dt).max(0.0);
            if self.recovery_remaining == 0.0 {
                // A recovery detour is allowed to move sideways or backward. Start
                // measuring goal-directed progress again from its endpoint instead
                // of treating the detour itself as success.
                self.anchor = Some(position);
                self.stalled_for = 0.0;
                self.recovery_yaw = None;
            }
            return RecoveryDirective {
                yaw: recovery_yaw,
                jump: false,
                recovering: true,
                attempts: self.recovery_attempts,
                give_up: false,
            };
        }

        let moved = horizontal_distance(anchor, position) >= MIN_PROGRESS_BS;
        let approached_target = match (self.best_target_distance, target_distance) {
            (Some(best_distance), Some(current_distance)) => {
                best_distance - current_distance >= MIN_PROGRESS_BS
            }
            // Relative movement has no target. It can only be judged by actual
            // displacement, preserving the existing `/move` behavior.
            (None, None) => true,
            _ => false,
        };
        if moved && approached_target {
            self.anchor = Some(position);
            self.best_target_distance = target_distance;
            self.stalled_for = 0.0;
            self.recovery_remaining = 0.0;
            self.recovery_yaw = None;
            self.recovery_attempts = 0;
            return direct(desired_yaw, jump);
        }

        self.stalled_for += dt;
        if self.stalled_for < STALL_SECONDS {
            return direct(desired_yaw, jump);
        }

        self.stalled_for = 0.0;
        self.anchor = Some(position);
        if self.recovery_attempts >= MAX_RECOVERY_ATTEMPTS {
            let attempts = self.recovery_attempts;
            self.reset();
            return RecoveryDirective {
                yaw: desired_yaw,
                jump: false,
                recovering: false,
                attempts,
                give_up: true,
            };
        }

        self.recovery_attempts += 1;
        self.recovery_remaining = RECOVERY_SECONDS;
        let recovery_yaw = desired_yaw + recovery_turn(self.recovery_attempts);
        self.recovery_yaw = Some(recovery_yaw);
        RecoveryDirective {
            yaw: recovery_yaw,
            jump: false,
            recovering: true,
            attempts: self.recovery_attempts,
            give_up: false,
        }
    }
}

fn direct(yaw: f32, jump: bool) -> RecoveryDirective {
    RecoveryDirective {
        yaw,
        jump,
        recovering: false,
        attempts: 0,
        give_up: false,
    }
}

fn recovery_turn(attempt: u8) -> f32 {
    match attempt {
        1 => PI * 0.5,
        2 => -PI * 0.5,
        _ => PI,
    }
}

fn target_changed(previous: Option<Vec3>, current: Option<Vec3>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => horizontal_distance(previous, current) > TARGET_CHANGE_BS,
        (None, None) => false,
        _ => true,
    }
}

fn horizontal_distance(a: Vec3, b: Vec3) -> f32 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f32, z: f32) -> Vec3 {
        Vec3 { x, y: 0.0, z }
    }

    #[test]
    fn stationary_movement_enters_side_recovery_without_jumping() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        for _ in 0..7 {
            let directive = anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, true, 0.2);
            assert!(!directive.recovering);
        }
        let directive = anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, true, 0.2);
        assert!(directive.recovering);
        assert!(!directive.jump);
        assert!((directive.yaw - PI * 0.5).abs() < 0.001);
    }

    #[test]
    fn real_progress_resets_recovery_attempts() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        for _ in 0..8 {
            anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, true, 0.2);
        }
        for _ in 0..6 {
            anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, true, 0.2);
        }
        let directive = anti_stuck.adjust(pos(2.0, 0.0), target, true, 0.0, true, 0.2);
        assert!(!directive.recovering);
        assert_eq!(directive.attempts, 0);
    }

    #[test]
    fn sideways_wall_sliding_is_not_goal_progress() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        let mut directive = direct(0.0, false);
        for tick in 0..8 {
            // Plenty of displacement, but all of it is perpendicular to the goal.
            directive = anti_stuck.adjust(
                pos(0.0, tick as f32 * 2.0),
                target,
                true,
                0.0,
                false,
                0.2,
            );
        }
        assert!(directive.recovering);
    }

    #[test]
    fn returning_from_a_detour_does_not_erase_recovery_history() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        for _ in 0..8 {
            anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, false, 0.2);
        }

        let mut attempts = 1;
        for tick in 0..30 {
            let position = if tick < 6 {
                pos(0.0, 10.0)
            } else {
                pos(0.0, 0.0)
            };
            let directive = anti_stuck.adjust(position, target, true, 0.0, false, 0.2);
            attempts = attempts.max(directive.attempts);
            if attempts >= 2 {
                break;
            }
        }
        assert_eq!(attempts, 2);
    }

    #[test]
    fn repeated_stalls_eventually_give_up() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        let mut gave_up = false;
        for _ in 0..80 {
            let directive = anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, true, 0.2);
            gave_up |= directive.give_up;
            if gave_up {
                break;
            }
        }
        assert!(gave_up);
    }

    #[test]
    fn dynamic_target_progress_uses_actual_displacement() {
        let mut anti_stuck = AntiStuck::default();
        for tick in 0..24 {
            let directive = anti_stuck.adjust(
                pos(tick as f32 * 2.0, 0.0),
                None,
                true,
                0.0,
                false,
                0.2,
            );
            assert!(!directive.recovering);
            assert!(!directive.give_up);
        }
    }

    #[test]
    fn recovery_heading_stays_fixed_when_desired_yaw_changes() {
        let mut anti_stuck = AntiStuck::default();
        let target = Some(pos(100.0, 0.0));
        for _ in 0..8 {
            anti_stuck.adjust(pos(0.0, 0.0), target, true, 0.0, false, 0.2);
        }
        let directive = anti_stuck.adjust(pos(0.0, 0.0), target, true, PI * 0.4, false, 0.2);
        assert!(directive.recovering);
        assert!((directive.yaw - PI * 0.5).abs() < 0.001);
    }
}
