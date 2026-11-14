//! Compiz-style window and workspace animations.
//!
//! BubbleWM keeps the animation model deliberately small: every transition is
//! a timed interpolation of scale, opacity and offset. The renderer wraps the
//! affected surface elements in [`crate::render::AnimatedElement`], so the
//! animation is compositor-native and works with both the Winit and DRM
//! backends without any extra process or plugin.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::config::EffectsConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Fade,
    Zoom,
    Glide,
    Pop,
    Burn,
    Slide,
    Cube,
    Minimize,
    Restore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseOut,
    EaseInOut,
    Back,
    Elastic,
}

impl Easing {
    fn parse(value: &str) -> Self {
        match value {
            "linear" => Self::Linear,
            "ease-in-out" => Self::EaseInOut,
            "back" => Self::Back,
            "elastic" => Self::Elastic,
            _ => Self::EaseOut,
        }
    }

    fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseOut => 1.0 - (1.0 - t).powi(3),
            Self::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Self::Back => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
            }
            Self::Elastic => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else {
                    let c4 = std::f32::consts::TAU / 3.0;
                    (2.0_f32).powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishAction {
    None,
    Minimize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowTransform {
    pub scale: f32,
    pub alpha: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl WindowTransform {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        alpha: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
    };

    pub fn is_identity(self) -> bool {
        (self.scale - 1.0).abs() < f32::EPSILON
            && (self.alpha - 1.0).abs() < f32::EPSILON
            && self.offset_x.abs() < f32::EPSILON
            && self.offset_y.abs() < f32::EPSILON
    }
}

struct WindowAnimation {
    start: Instant,
    duration: Duration,
    easing: Easing,
    effect: Effect,
    reverse: bool,
    finish: FinishAction,
    finish_fired: bool,
}

impl WindowAnimation {
    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        ((now.saturating_duration_since(self.start)).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

struct WorkspaceAnimation {
    start: Instant,
    duration: Duration,
    easing: Easing,
    effect: Effect,
    direction: f32,
}

impl WorkspaceAnimation {
    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        ((now.saturating_duration_since(self.start)).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

/// Owns all in-flight window and workspace transitions.
pub struct AnimationEngine {
    enabled: bool,
    duration: Duration,
    easing: Easing,
    wobbly: bool,
    open_effect: Effect,
    close_effect: Effect,
    minimize_effect: Effect,
    workspace_effect: Effect,
    windows: HashMap<u64, WindowAnimation>,
    workspace: Option<WorkspaceAnimation>,
}

impl AnimationEngine {
    pub fn new(config: &EffectsConfig) -> Self {
        let mut engine = Self {
            enabled: false,
            duration: Duration::ZERO,
            easing: Easing::EaseOut,
            wobbly: false,
            open_effect: Effect::Zoom,
            close_effect: Effect::Zoom,
            minimize_effect: Effect::Glide,
            workspace_effect: Effect::Slide,
            windows: HashMap::new(),
            workspace: None,
        };
        engine.configure(config);
        engine
    }

    /// Applies the animation-related settings without dropping in-flight work.
    pub fn configure(&mut self, config: &EffectsConfig) {
        self.enabled = config.enabled && config.animations && config.animation_duration > 0;
        self.duration = Duration::from_millis(u64::from(config.animation_duration));
        self.easing = Easing::parse(&config.animation_easing);
        self.wobbly = config.wobbly;
        self.open_effect = parse_open_effect(&config.open_effect);
        self.close_effect = parse_close_effect(&config.close_effect);
        self.minimize_effect = parse_minimize_effect(&config.minimize_effect);
        self.workspace_effect =
            parse_workspace_effect(&config.workspace_effect, config.workspace_cube);
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin_open(&mut self, id: u64) -> bool {
        self.begin(id, self.open_effect, FinishAction::None, false)
    }

    pub fn begin_restore(&mut self, id: u64) -> bool {
        let effect = match self.open_effect {
            Effect::None => Effect::Restore,
            effect => effect,
        };
        self.begin(id, effect, FinishAction::None, false)
    }

    pub fn begin_minimize(&mut self, id: u64) -> bool {
        self.begin(id, self.minimize_effect, FinishAction::Minimize, false)
    }

    pub fn begin_close(&mut self, id: u64) -> bool {
        if self
            .windows
            .get(&id)
            .is_some_and(|animation| animation.finish == FinishAction::Close)
        {
            return true;
        }
        self.begin(id, self.close_effect, FinishAction::Close, true)
    }

    fn begin(&mut self, id: u64, effect: Effect, finish: FinishAction, reverse: bool) -> bool {
        if !self.enabled || effect == Effect::None {
            return false;
        }
        self.windows.insert(
            id,
            WindowAnimation {
                start: Instant::now(),
                duration: self.duration,
                easing: self.easing,
                effect,
                reverse,
                finish,
                finish_fired: false,
            },
        );
        true
    }

    pub fn begin_workspace(&mut self, direction: f32) {
        if !self.enabled || self.workspace_effect == Effect::None {
            return;
        }
        self.workspace = Some(WorkspaceAnimation {
            start: Instant::now(),
            duration: self.duration,
            easing: self.easing,
            effect: self.workspace_effect,
            direction: direction.signum(),
        });
    }

    /// Returns the animated transform for a window, if it is in transition.
    pub fn window_transform(&self, id: u64, now: Instant) -> WindowTransform {
        let Some(animation) = self.windows.get(&id) else {
            return WindowTransform::IDENTITY;
        };
        let t = animation.progress(now);
        let eased = animation.easing.apply(t);
        let mut transform = sample_effect(animation.effect, eased, t, animation.reverse);
        if self.wobbly && t < 1.0 {
            transform.offset_x += (t * std::f32::consts::PI * 5.0).sin() * (1.0 - t) * 7.0;
        }
        transform
    }

    /// Returns the current workspace transition offset.
    pub fn workspace_transform(&self, now: Instant) -> WindowTransform {
        let Some(animation) = self.workspace.as_ref() else {
            return WindowTransform::IDENTITY;
        };
        let t = animation.progress(now);
        let eased = animation.easing.apply(t);
        match animation.effect {
            Effect::Fade => WindowTransform {
                alpha: eased,
                ..WindowTransform::IDENTITY
            },
            Effect::Zoom => WindowTransform {
                scale: 0.92 + 0.08 * eased,
                alpha: eased,
                ..WindowTransform::IDENTITY
            },
            Effect::Cube => WindowTransform {
                scale: 0.94 + 0.06 * eased,
                alpha: 0.55 + 0.45 * eased,
                offset_x: (1.0 - eased) * 180.0 * animation.direction,
                offset_y: 0.0,
            },
            Effect::Slide => WindowTransform {
                scale: 1.0,
                alpha: 0.65 + 0.35 * eased,
                offset_x: (1.0 - eased) * 160.0 * animation.direction,
                offset_y: 0.0,
            },
            _ => WindowTransform::IDENTITY,
        }
    }

    /// Emits finish actions exactly once and retires completed workspace work.
    pub fn tick(&mut self, now: Instant) -> Vec<(u64, FinishAction)> {
        let mut finished = Vec::new();
        for (id, animation) in &mut self.windows {
            if !animation.finish_fired && animation.progress(now) >= 1.0 {
                animation.finish_fired = true;
                finished.push((*id, animation.finish));
            }
        }
        if self
            .workspace
            .as_ref()
            .is_some_and(|animation| animation.progress(now) >= 1.0)
        {
            self.workspace = None;
        }
        finished
    }

    /// True while a frame still changes on screen.
    pub fn active(&self, now: Instant) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|animation| animation.progress(now) < 1.0)
            || self
                .windows
                .values()
                .any(|animation| animation.progress(now) < 1.0)
    }

    pub fn remove(&mut self, id: u64) {
        self.windows.remove(&id);
    }

    pub fn clear(&mut self) {
        self.windows.clear();
        self.workspace = None;
    }
}

fn parse_open_effect(value: &str) -> Effect {
    match value {
        "none" => Effect::None,
        "fade" => Effect::Fade,
        "glide" => Effect::Glide,
        "pop" => Effect::Pop,
        "slide" => Effect::Slide,
        _ => Effect::Zoom,
    }
}

fn parse_close_effect(value: &str) -> Effect {
    match value {
        "none" => Effect::None,
        "fade" => Effect::Fade,
        "glide" => Effect::Glide,
        "burn" => Effect::Burn,
        "slide" => Effect::Slide,
        _ => Effect::Zoom,
    }
}

fn parse_minimize_effect(value: &str) -> Effect {
    match value {
        "none" => Effect::None,
        "fade" => Effect::Fade,
        "zoom" => Effect::Zoom,
        _ => Effect::Minimize,
    }
}

fn parse_workspace_effect(value: &str, cube: bool) -> Effect {
    match value {
        "none" => Effect::None,
        "fade" => Effect::Fade,
        "zoom" => Effect::Zoom,
        "slide" => {
            if cube {
                Effect::Cube
            } else {
                Effect::Slide
            }
        }
        "cube" => Effect::Cube,
        _ => Effect::Slide,
    }
}

fn sample_effect(effect: Effect, eased: f32, t: f32, reverse: bool) -> WindowTransform {
    match effect {
        Effect::None => WindowTransform::IDENTITY,
        Effect::Fade => WindowTransform {
            alpha: if reverse { 1.0 - eased } else { eased },
            ..WindowTransform::IDENTITY
        },
        Effect::Zoom => WindowTransform {
            scale: if reverse {
                1.0 - 0.14 * eased
            } else {
                0.86 + 0.14 * eased
            },
            alpha: if reverse { 1.0 - eased } else { eased },
            ..WindowTransform::IDENTITY
        },
        Effect::Glide => WindowTransform {
            alpha: if reverse { 1.0 - eased } else { eased },
            offset_y: if reverse {
                48.0 * eased
            } else {
                (1.0 - eased) * 48.0
            },
            ..WindowTransform::IDENTITY
        },
        Effect::Pop => WindowTransform {
            scale: if reverse {
                1.0 - 0.30 * eased
            } else {
                0.70 + 0.30 * Easing::Back.apply(t)
            },
            alpha: if reverse { 1.0 - t } else { (t * 1.6).min(1.0) },
            ..WindowTransform::IDENTITY
        },
        Effect::Burn => WindowTransform {
            scale: 1.0 - 0.18 * eased,
            alpha: (1.0 - t).powf(1.4),
            offset_y: -12.0 * eased,
            ..WindowTransform::IDENTITY
        },
        Effect::Slide => WindowTransform {
            alpha: if reverse { 1.0 - eased } else { eased },
            offset_x: if reverse {
                140.0 * eased
            } else {
                (1.0 - eased) * 140.0
            },
            ..WindowTransform::IDENTITY
        },
        Effect::Cube => WindowTransform {
            scale: if reverse {
                1.0 - 0.06 * eased
            } else {
                0.94 + 0.06 * eased
            },
            alpha: if reverse {
                1.0 - eased
            } else {
                0.55 + 0.45 * eased
            },
            offset_x: if reverse {
                140.0 * eased
            } else {
                (1.0 - eased) * 140.0
            },
            ..WindowTransform::IDENTITY
        },
        Effect::Minimize => WindowTransform {
            scale: 1.0 - 0.55 * eased,
            alpha: 1.0 - 0.78 * eased,
            offset_x: 0.0,
            offset_y: 64.0 * eased,
        },
        Effect::Restore => WindowTransform {
            scale: 0.58 + 0.42 * eased,
            alpha: 0.18 + 0.82 * eased,
            offset_x: 0.0,
            offset_y: 48.0 * (1.0 - eased),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints_are_stable() {
        for easing in [
            Easing::Linear,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::Back,
            Easing::Elastic,
        ] {
            assert!((easing.apply(0.0) - 0.0).abs() < 0.001);
            assert!((easing.apply(1.0) - 1.0).abs() < 0.001);
        }
    }

    #[test]
    fn disabled_engine_has_no_transitions() {
        let mut config = EffectsConfig::default();
        config.animations = false;
        let mut engine = AnimationEngine::new(&config);
        engine.begin_open(1);
        assert!(engine.window_transform(1, Instant::now()).is_identity());
    }

    #[test]
    fn close_animation_finishes_once() {
        let mut config = EffectsConfig::default();
        config.animation_duration = 1;
        let mut engine = AnimationEngine::new(&config);
        engine.begin_close(7);
        std::thread::sleep(Duration::from_millis(5));
        let now = Instant::now();
        let finished = engine.tick(now);
        assert_eq!(finished, vec![(7, FinishAction::Close)]);
        assert!(engine.tick(now).is_empty());
    }
}
