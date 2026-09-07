use rand::Rng;
use std::fmt;
use std::sync::LazyLock;

pub const MIN_VIBRATE_STRENGTH: f32 = 0.0;
pub const MAX_VIBRATE_STRENGTH: f32 = 20.0;
pub const MIN_VIBRATE_DURATION: f32 = 0.25;
pub const MAX_VIBRATE_DURATION: f32 = 60.0;
/// The sub-second portion of the duration grid: quarter-second steps below a
/// full second, where a whole-second count stops being meaningful. From 1
/// second up, [`duration_steps`] continues in whole seconds instead.
const SUB_SECOND_DURATION_STEPS: [f32; 3] = [0.25, 0.5, 0.75];

/// The grid is fixed for the life of the process, so it is built once instead
/// of reallocated on every UI slider draw and every resolve/normalize call.
static DURATION_STEPS: LazyLock<Vec<f32>> = LazyLock::new(|| {
    let mut steps = SUB_SECOND_DURATION_STEPS.to_vec();
    steps.extend((1..=MAX_VIBRATE_DURATION as u32).map(|whole| whole as f32));
    steps
});

/// The fixed set of durations selectable in the UI and accepted on resolve:
/// quarter-second steps up to a second, then whole seconds up to
/// [`MAX_VIBRATE_DURATION`]. Fine steps matter most for short buzzes; once
/// a whole second is on the table, finer-than-a-second precision is not
/// worth the extra picker length.
pub fn duration_steps() -> &'static [f32] {
    &DURATION_STEPS
}

/// The duration step closest to `value`, for snapping a stored or dragged
/// value onto the grid [`duration_steps`] defines.
pub fn nearest_duration_step(value: f32) -> f32 {
    duration_steps()
        .iter()
        .copied()
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
        .unwrap_or(MIN_VIBRATE_DURATION)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VibrateMode {
    Interval,
    /// The default: a single strength held for a set time. Predictable is the
    /// better starting point; a trigger only varies if the user opts into it.
    #[default]
    Fixed,
}
impl VibrateMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Interval => "Random",
            Self::Fixed => "Fixed",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct VibrateIntervalSettings {
    pub minimum_strength: f32,
    pub maximum_strength: f32,
    pub minimum_duration_seconds: f32,
    pub maximum_duration_seconds: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct VibrateFixedSettings {
    pub strength: f32,
    pub duration_seconds: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct VibrateActionSettings {
    pub mode: VibrateMode,
    pub interval: VibrateIntervalSettings,
    pub fixed: VibrateFixedSettings,
}
impl Default for VibrateActionSettings {
    fn default() -> Self {
        Self {
            mode: VibrateMode::default(),
            interval: VibrateIntervalSettings {
                minimum_strength: MIN_VIBRATE_STRENGTH,
                maximum_strength: MIN_VIBRATE_STRENGTH,
                minimum_duration_seconds: MIN_VIBRATE_DURATION,
                maximum_duration_seconds: MIN_VIBRATE_DURATION,
            },
            fixed: VibrateFixedSettings {
                strength: MIN_VIBRATE_STRENGTH,
                duration_seconds: MIN_VIBRATE_DURATION,
            },
        }
    }
}
impl VibrateActionSettings {
    /// A ready-to-feel starting point for a fresh profile's death effect: a
    /// fixed mid-strength buzz for a second, rather than the all-zero
    /// [`Default`] that stays silent until the user sets a strength.
    pub fn starter_death() -> Self {
        Self {
            mode: VibrateMode::Fixed,
            fixed: VibrateFixedSettings {
                strength: 10.0,
                duration_seconds: 1.0,
            },
            ..Self::default()
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedVibrateAction {
    pub strength: u8,
    /// Seconds, one of [`duration_steps`] - sub-second values are sent by
    /// holding the toy steady and timing the stop locally, since the
    /// Standard API's timed command only accepts whole seconds.
    pub duration_secs: f32,
}
impl ResolvedVibrateAction {
    pub fn summary(self) -> String {
        format!("{}/20 for {} s", self.strength, format_seconds_value(self.duration_secs))
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionValidationError {
    InvalidDuration,
    InvalidInterval,
    InvalidStrength,
}
impl fmt::Display for ActionValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDuration => {
                write!(f, "duration is out of the supported range for this action")
            }
            Self::InvalidInterval => write!(f, "interval minimum must not exceed maximum"),
            Self::InvalidStrength => {
                write!(f, "vibration strength must be an integer from 0 to 20")
            }
        }
    }
}
/// Renders a seconds value with two decimals if fractional, none otherwise -
/// the shared numeric half of both [`format_seconds`] (prose, "N seconds")
/// and [`ResolvedVibrateAction::summary`] (compact, "N s").
pub(crate) fn format_seconds_value(seconds: f32) -> String {
    if seconds.fract().abs() > f32::EPSILON {
        format!("{seconds:.2}")
    } else {
        format!("{seconds:.0}")
    }
}
fn format_seconds(seconds: f32) -> String {
    if (seconds - 1.0).abs() < f32::EPSILON {
        "1 second".to_owned()
    } else {
        format!("{} seconds", format_seconds_value(seconds))
    }
}
/// Renders a strength range as a single value when both ends match, since a
/// "3-3" range reads as a typo rather than a deliberate fixed value.
fn format_strength_range(minimum: f32, maximum: f32) -> String {
    if (minimum - maximum).abs() < f32::EPSILON {
        format!("{:.0}", minimum)
    } else {
        format!("{:.0} to {:.0}", minimum, maximum)
    }
}
/// Same as [`format_strength_range`], for the duration range.
fn format_duration_range(minimum: f32, maximum: f32) -> String {
    if (minimum - maximum).abs() < f32::EPSILON {
        format_seconds(minimum)
    } else {
        format!("{:.0}-{:.0} seconds", minimum, maximum)
    }
}
impl VibrateActionSettings {
    /// Plain-language description of the configured effect, shown to users in
    /// the trigger list who may not know what a compact "3-3/20 for 1-1s"
    /// shorthand means.
    pub fn summary(&self) -> String {
        match self.mode {
            VibrateMode::Fixed => format!(
                "Strength {:.0}, for {}",
                self.fixed.strength,
                format_seconds(self.fixed.duration_seconds)
            ),
            VibrateMode::Interval => format!(
                "Random strength {}, for {}",
                format_strength_range(
                    self.interval.minimum_strength,
                    self.interval.maximum_strength
                ),
                format_duration_range(
                    self.interval.minimum_duration_seconds,
                    self.interval.maximum_duration_seconds
                )
            ),
        }
    }
    pub fn resolve(&self) -> Option<ResolvedVibrateAction> {
        self.resolve_checked().ok()
    }
    pub fn resolve_checked(&self) -> Result<ResolvedVibrateAction, ActionValidationError> {
        let mut rng = rand::rng();
        self.resolve_with(&mut rng)
    }
    pub fn resolve_with<R: Rng + ?Sized>(
        &self,
        rng: &mut R,
    ) -> Result<ResolvedVibrateAction, ActionValidationError> {
        let strength = match self.mode {
            VibrateMode::Fixed => {
                portable_strength(self.fixed.strength).ok_or(ActionValidationError::InvalidStrength)?
            }
            VibrateMode::Interval => {
                let minimum = portable_strength(self.interval.minimum_strength)
                    .ok_or(ActionValidationError::InvalidStrength)?;
                let maximum = portable_strength(self.interval.maximum_strength)
                    .ok_or(ActionValidationError::InvalidStrength)?;
                if minimum > maximum {
                    return Err(ActionValidationError::InvalidInterval);
                }
                rng.random_range(minimum..=maximum)
            }
        };
        let duration_secs = match self.mode {
            VibrateMode::Fixed => portable_vibrate_duration(self.fixed.duration_seconds)
                .ok_or(ActionValidationError::InvalidDuration)?,
            VibrateMode::Interval => {
                let minimum = portable_vibrate_duration(self.interval.minimum_duration_seconds)
                    .ok_or(ActionValidationError::InvalidDuration)?;
                let maximum = portable_vibrate_duration(self.interval.maximum_duration_seconds)
                    .ok_or(ActionValidationError::InvalidDuration)?;
                if minimum > maximum {
                    return Err(ActionValidationError::InvalidInterval);
                }
                rng.random_range(minimum..=maximum)
            }
        };
        Ok(ResolvedVibrateAction {
            strength,
            duration_secs,
        })
    }
    pub fn copy_active_from(&mut self, source: &Self) {
        *self = source.clone();
    }
}
pub fn portable_strength(value: f32) -> Option<u8> {
    (value.is_finite()
        && value.fract() == 0.0
        && (MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH).contains(&value))
    .then_some(value as u8)
}
/// Accepts a value already on the [`duration_steps`] grid (allowing for
/// float round-trip slop from serialization or UI arithmetic), rather than
/// snapping arbitrary values - the picker only ever produces grid values, so
/// anything meaningfully off the grid signals a bad or hand-edited setting.
pub fn portable_vibrate_duration(value: f32) -> Option<f32> {
    if !value.is_finite() || !(MIN_VIBRATE_DURATION..=MAX_VIBRATE_DURATION).contains(&value) {
        return None;
    }
    let nearest = nearest_duration_step(value);
    ((nearest - value).abs() < 0.01).then_some(nearest)
}

/// The widest window the damage-intensity curve will sum damage over.
pub const MAX_INTENSITY_WINDOW_SECS: f32 = 20.0;
/// The largest windowed-damage figure the curve editor plots and clamps to.
pub const MAX_INTENSITY_DAMAGE: f32 = 3000.0;

/// One control point on the damage-intensity curve: `damage` taken within the
/// window maps to vibration `level`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntensityPoint {
    pub damage: f32,
    pub level: f32,
}

/// Maps "damage taken within a rolling window" to a vibration level, as a
/// piecewise-linear curve through user-set control points. Below the first
/// point the level is 0 (no effect); at or above the last point it holds at
/// that point's level. Used only by the damage-taken-intensity trigger.
#[derive(Clone, Debug, PartialEq)]
pub struct IntensityCurve {
    /// Seconds of damage taken that are summed into the value fed to the curve.
    pub window_seconds: f32,
    /// How long each intensity pulse runs on the toy.
    pub pulse_seconds: f32,
    /// Control points, kept sorted by `damage` ascending; always at least two.
    pub points: Vec<IntensityPoint>,
}

impl Default for IntensityCurve {
    fn default() -> Self {
        Self {
            window_seconds: 3.0,
            pulse_seconds: 1.0,
            points: vec![
                IntensityPoint {
                    damage: 100.0,
                    level: 1.0,
                },
                IntensityPoint {
                    damage: 500.0,
                    level: 10.0,
                },
                IntensityPoint {
                    damage: 1200.0,
                    level: 20.0,
                },
            ],
        }
    }
}

impl IntensityCurve {
    pub const MAX_POINTS: usize = 8;

    /// Snaps every field back into range: window and pulse clamped, points
    /// clamped, sorted by damage, de-duplicated, and padded back to at least
    /// two if a caller left fewer.
    pub fn normalize(&mut self) {
        self.window_seconds = self
            .window_seconds
            .clamp(0.5, MAX_INTENSITY_WINDOW_SECS);
        self.pulse_seconds = portable_vibrate_duration(self.pulse_seconds)
            .unwrap_or_else(|| nearest_duration_step(self.pulse_seconds.clamp(0.25, MAX_VIBRATE_DURATION)));
        for point in &mut self.points {
            point.damage = point.damage.clamp(0.0, MAX_INTENSITY_DAMAGE);
            point.level = point.level.clamp(MIN_VIBRATE_STRENGTH, MAX_VIBRATE_STRENGTH);
        }
        self.points.sort_by(|a, b| a.damage.total_cmp(&b.damage));
        self.points.dedup_by(|a, b| (a.damage - b.damage).abs() < 1.0);
        while self.points.len() < 2 {
            let last = self.points.last().copied().unwrap_or(IntensityPoint {
                damage: 100.0,
                level: 1.0,
            });
            self.points.push(IntensityPoint {
                damage: (last.damage + 400.0).min(MAX_INTENSITY_DAMAGE),
                level: (last.level + 4.0).min(MAX_VIBRATE_STRENGTH),
            });
        }
        self.points.truncate(Self::MAX_POINTS);
    }

    /// The (continuous, unrounded) level for `damage` summed over the window:
    /// 0 below the first point, an ease-in/ease-out (smoothstep) blend between
    /// adjacent points so the ramp has no hard corners, and held flat at the
    /// last point's level above it. Used to draw the curve.
    pub fn level_at(&self, damage: f32) -> f32 {
        let Some(first) = self.points.first().copied() else {
            return 0.0;
        };
        if damage < first.damage {
            return 0.0;
        }
        let last = self.points.last().copied().unwrap_or(first);
        if damage >= last.damage {
            return last.level.clamp(0.0, MAX_VIBRATE_STRENGTH);
        }
        for window in self.points.windows(2) {
            let (a, b) = (window[0], window[1]);
            if damage >= a.damage && damage <= b.damage {
                let span = b.damage - a.damage;
                let t = if span.abs() < f32::EPSILON {
                    0.0
                } else {
                    (damage - a.damage) / span
                };
                let eased = t * t * (3.0 - 2.0 * t);
                return (a.level + eased * (b.level - a.level)).clamp(0.0, MAX_VIBRATE_STRENGTH);
            }
        }
        0.0
    }

    /// [`Self::level_at`] rounded to the integer strength sent to the toy.
    pub fn level_for(&self, damage: f32) -> u8 {
        self.level_at(damage)
            .round()
            .clamp(0.0, MAX_VIBRATE_STRENGTH) as u8
    }

    /// Compact one-line description for the trigger list.
    pub fn summary(&self) -> String {
        let last = self.points.last().copied().unwrap_or(IntensityPoint {
            damage: 0.0,
            level: 0.0,
        });
        format!(
            "Up to level {:.0} at {:.0} damage in {}",
            last.level,
            last.damage,
            format_seconds(self.window_seconds)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn invalid_action_settings_return_typed_validation() {
        let mut settings = VibrateActionSettings {
            mode: VibrateMode::Fixed,
            ..Default::default()
        };
        settings.fixed.strength = 21.0;
        assert_eq!(
            settings.resolve_checked(),
            Err(ActionValidationError::InvalidStrength)
        );
    }

    #[test]
    fn resolution_is_a_snapshot() {
        let mut settings = VibrateActionSettings {
            mode: VibrateMode::Fixed,
            ..Default::default()
        };
        settings.fixed.strength = 12.0;
        settings.fixed.duration_seconds = 4.0;
        let mut rng = StdRng::seed_from_u64(4);
        let resolved = settings.resolve_with(&mut rng).unwrap();
        settings.fixed.strength = 20.0;
        assert_eq!(settings.fixed.strength, 20.0);
        assert_eq!(
            resolved,
            ResolvedVibrateAction {
                strength: 12,
                duration_secs: 4.0
            }
        );
    }

    #[test]
    fn interval_mode_rejects_inverted_bounds() {
        let mut settings = VibrateActionSettings {
            mode: VibrateMode::Interval,
            ..Default::default()
        };
        settings.interval.minimum_strength = 10.0;
        settings.interval.maximum_strength = 5.0;
        assert_eq!(
            settings.resolve_checked(),
            Err(ActionValidationError::InvalidInterval)
        );
    }

    #[test]
    fn duration_grid_covers_quarter_seconds_then_whole_seconds_to_the_maximum() {
        let steps = duration_steps();
        assert_eq!(&steps[..3], &[0.25, 0.5, 0.75]);
        assert_eq!(steps[3], 1.0);
        assert_eq!(*steps.last().unwrap(), MAX_VIBRATE_DURATION);
        assert_eq!(steps.len(), 3 + MAX_VIBRATE_DURATION as usize);
    }

    #[test]
    fn sub_second_durations_resolve_and_round_trip() {
        let mut settings = VibrateActionSettings {
            mode: VibrateMode::Fixed,
            ..Default::default()
        };
        settings.fixed.strength = 5.0;
        settings.fixed.duration_seconds = 0.25;
        let resolved = settings.resolve_checked().unwrap();
        assert_eq!(resolved.duration_secs, 0.25);
    }

    #[test]
    fn off_grid_durations_are_rejected() {
        assert_eq!(portable_vibrate_duration(0.4), None);
        assert_eq!(portable_vibrate_duration(1.5), None);
        assert_eq!(portable_vibrate_duration(0.0), None);
        assert_eq!(portable_vibrate_duration(61.0), None);
        assert_eq!(portable_vibrate_duration(0.25), Some(0.25));
        assert_eq!(portable_vibrate_duration(60.0), Some(60.0));
    }

    #[test]
    fn intensity_curve_eases_between_points_and_holds_at_the_ends() {
        let curve = IntensityCurve::default(); // (100->1), (500->10), (1200->20)
        assert_eq!(curve.level_for(0.0), 0);
        assert_eq!(curve.level_for(99.0), 0);
        assert_eq!(curve.level_for(100.0), 1);
        // Smoothstep is symmetric, so the midpoint still lands on the mean.
        assert_eq!(curve.level_for(300.0), 6); // 1 + 0.5*(10-1) = 5.5 -> 6
        assert_eq!(curve.level_for(500.0), 10);
        assert_eq!(curve.level_for(1200.0), 20);
        assert_eq!(curve.level_for(9000.0), 20); // held flat past the last point
        // Eased, not linear: a quarter of the way in sits below the linear 3.25.
        assert!(curve.level_at(200.0) < 3.25);
    }

    #[test]
    fn intensity_curve_normalize_sorts_clamps_and_keeps_two_points() {
        let mut curve = IntensityCurve {
            window_seconds: 99.0,
            pulse_seconds: 0.3,
            points: vec![
                IntensityPoint {
                    damage: 800.0,
                    level: 25.0,
                },
                IntensityPoint {
                    damage: -5.0,
                    level: 3.0,
                },
            ],
        };
        curve.normalize();
        assert!(curve.window_seconds <= MAX_INTENSITY_WINDOW_SECS);
        assert_eq!(curve.points.len(), 2);
        assert!(curve.points[0].damage <= curve.points[1].damage);
        assert_eq!(curve.points[0].damage, 0.0);
        assert_eq!(curve.points[1].level, MAX_VIBRATE_STRENGTH);
        assert!(duration_steps().contains(&curve.pulse_seconds));
    }
}
